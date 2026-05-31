use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use tokio_postgres::Client as PgClient;
use tower_http::services::{ServeDir, ServeFile};
use tracing::warn;
use uuid::Uuid;

use crate::bootstrap::config::is_iso639_1_format;
use crate::domain::speaker::SpeakerProfile;
use crate::domain::transcript::TranscriptSource;
use crate::infrastructure::sql::{
    COUNT_GUILD_MEETINGS_SQL, GET_GUILD_SETTINGS_SQL, UPSERT_GUILD_SETTINGS_SQL,
};
use crate::infrastructure::storage_fs::sanitize_path_component;

type HmacSha256 = Hmac<Sha256>;
const SESSION_COOKIE_NAME: &str = "dt_session";
const SESSION_TTL_SECS: u64 = 7 * 24 * 3600; // 7 days
const SESSION_MEMBERSHIP_VERIFY_INTERVAL_SECS: u64 = 15 * 60; // 15 minutes
const MEMBERSHIP_REVERIFY_INFLIGHT_SECS: u64 = 30;
const OAUTH_NONCE_COOKIE_NAME: &str = "dt_oauth_nonce";
const OAUTH_NONCE_COOKIE_PATH: &str = "/auth/callback";
const OAUTH_STATE_TTL_SECS: u64 = 600; // 10 minutes
const VIEW_CHANNEL: u64 = 1 << 10;
const ADMINISTRATOR: u64 = 1 << 3;

// ---------- State ----------

const PERMISSION_CACHE_TTL_SECS: u64 = 300;
const GUILD_CACHE_TTL_SECS: u64 = 300;
const MIN_AUDIO_RANGE_BYTES: u64 = 64 * 1024;
const AUDIO_RANGE_BUCKET_CAPACITY: f64 = 30.0;
const AUDIO_RANGE_REFILL_PER_SEC: f64 = 10.0;

type PermissionCache =
    Arc<tokio::sync::RwLock<HashMap<(String, String), (CachedChannelPermission, Instant)>>>;
type GuildCache = Arc<tokio::sync::RwLock<Option<(DiscordGuildFull, Instant)>>>;
type MembershipReverifyInflight = Arc<tokio::sync::Mutex<HashMap<String, Instant>>>;

#[derive(Debug, Default)]
struct AudioRangeRateLimiter {
    buckets: HashMap<String, AudioRangeBucket>,
}

#[derive(Debug)]
struct AudioRangeBucket {
    tokens: f64,
    last_refill: Instant,
}

impl AudioRangeRateLimiter {
    fn allow(&mut self, key: &str) -> bool {
        let now = Instant::now();
        let idle_secs = AUDIO_RANGE_BUCKET_CAPACITY / AUDIO_RANGE_REFILL_PER_SEC;
        self.buckets
            .retain(|_, bucket| now.duration_since(bucket.last_refill).as_secs_f64() < idle_secs);
        let bucket = self
            .buckets
            .entry(key.to_owned())
            .or_insert(AudioRangeBucket {
                tokens: AUDIO_RANGE_BUCKET_CAPACITY,
                last_refill: now,
            });
        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens =
            (bucket.tokens + elapsed * AUDIO_RANGE_REFILL_PER_SEC).min(AUDIO_RANGE_BUCKET_CAPACITY);
        bucket.last_refill = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachedChannelPermission {
    pub can_view: bool,
    pub is_admin: bool,
}

#[derive(Debug, Clone)]
pub struct GuildSettingsDefaults {
    pub whisper_language: Option<String>,
    pub whisper_vad: bool,
    pub auto_stop_grace_seconds: i64,
    pub retention_raw_audio_ttl_days: i32,
    pub retention_transcript_ttl_days: i32,
}

#[derive(Clone)]
pub struct WebState {
    pub db: Arc<PgClient>,
    pub chunk_storage_dir: String,
    pub auth: Option<Arc<AuthConfig>>,
    pub http_client: reqwest::Client,
    /// Cache: (user_id, channel_id) -> (computed channel access, expires_at)
    pub permission_cache: PermissionCache,
    /// Cache: guild info (shared across all requests)
    guild_cache: GuildCache,
    /// In-flight guild membership re-verification per user id
    membership_reverify_inflight: MembershipReverifyInflight,
    audio_range_limiter: Arc<Mutex<AudioRangeRateLimiter>>,
    pub static_files_dir: String,
    /// Default guild settings used when a guild has no custom settings
    pub guild_settings_defaults: Arc<GuildSettingsDefaults>,
}

impl WebState {
    pub fn new(
        db: Arc<PgClient>,
        chunk_storage_dir: String,
        auth: Option<Arc<AuthConfig>>,
        http_client: reqwest::Client,
        static_files_dir: String,
        guild_settings_defaults: GuildSettingsDefaults,
    ) -> Self {
        Self {
            db,
            chunk_storage_dir,
            auth,
            http_client,
            permission_cache: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            guild_cache: Arc::new(tokio::sync::RwLock::new(None)),
            membership_reverify_inflight: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            audio_range_limiter: Arc::new(Mutex::new(AudioRangeRateLimiter::default())),
            static_files_dir,
            guild_settings_defaults: Arc::new(guild_settings_defaults),
        }
    }
}

fn audio_range_rate_limited_response() -> Response {
    Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header(header::RETRY_AFTER, "1")
        .body(axum::body::Body::empty())
        .unwrap_or_else(|_| Response::new(axum::body::Body::empty()))
}

async fn check_audio_range_rate_limit(state: &WebState, user_id: &str) -> Result<(), Response> {
    let mut limiter = state.audio_range_limiter.lock().await;
    if limiter.allow(user_id) {
        Ok(())
    } else {
        Err(audio_range_rate_limited_response())
    }
}

pub struct AuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub session_secret: String,
    pub redirect_uri: String,
    pub guild_id: String,
    pub bot_token: String,
    pub secure_cookie: bool,
}

/// Authenticated user's Discord ID, injected by `require_auth` middleware.
#[derive(Clone)]
struct AuthUserId(String);

// ---------- Router ----------

pub fn create_router(state: WebState) -> Router {
    let auth_routes = Router::new()
        .route("/auth/login", get(auth_login))
        .route("/auth/callback", get(auth_callback))
        .route(
            "/auth/logout",
            post(auth_logout).get(auth_logout_get_rejected),
        );

    let protected = Router::new()
        .route("/api/me", get(api_me))
        .route("/api/guild/meetings", get(api_guild_meetings))
        .route(
            "/api/guild/settings",
            get(api_guild_settings).put(api_update_guild_settings),
        )
        .route("/api/meetings/{meeting_id}", get(api_meeting))
        .route("/api/meetings/{meeting_id}/transcript", get(api_transcript))
        .route("/api/meetings/{meeting_id}/summary", get(api_summary))
        .route("/api/meetings/{meeting_id}/audio", get(api_audio))
        .route("/api/meetings/{meeting_id}/speakers", get(api_speakers))
        .route(
            "/api/meetings/{meeting_id}/speakers/{speaker_id}/audio",
            get(api_speaker_audio),
        )
        .route(
            "/api/meetings/{meeting_id}/debug/manifest",
            get(api_debug_manifest),
        )
        .route(
            "/api/meetings/{meeting_id}/debug/files/{artifact_id}",
            get(api_debug_file),
        )
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_auth,
        ));

    let index_html = format!("{}/index.html", state.static_files_dir);
    let spa = ServeDir::new(&state.static_files_dir).not_found_service(ServeFile::new(index_html));

    Router::new()
        .merge(auth_routes)
        .merge(protected)
        .fallback_service(spa)
        .with_state(state)
}

// ========== Auth: middleware ==========

async fn require_auth(
    State(state): State<WebState>,
    headers: HeaderMap,
    mut request: axum::extract::Request,
    next: Next,
) -> Response {
    let Some(ref auth) = state.auth else {
        return (StatusCode::SERVICE_UNAVAILABLE, "OAuth not configured").into_response();
    };

    let Some(cookie_val) = get_cookie(&headers, SESSION_COOKIE_NAME) else {
        return auth_required_redirect_or_unauthorized(&request);
    };
    let Some(session) = verify_session(&cookie_val, &auth.session_secret) else {
        return auth_required_redirect_or_unauthorized(&request);
    };
    if session.gid != auth.guild_id {
        return auth_required_redirect_or_unauthorized(&request);
    }
    match session_is_revoked(&state.db, &session.uid, session.issued_at).await {
        Ok(true) => {
            return auth_required_redirect_or_unauthorized(&request)
                .with_cleared_session_cookie(auth.secure_cookie);
        }
        Ok(false) => {}
        Err(err) => {
            warn!(
                error = %err,
                user_id = %session.uid,
                "failed to check session revocation"
            );
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    }

    let mut refreshed_session_cookie = None;
    if session_needs_membership_reverify(&session)
        && begin_membership_reverify(&state.membership_reverify_inflight, &session.uid).await
    {
        let membership = is_guild_member(&state, auth, &session.uid).await;
        end_membership_reverify(&state.membership_reverify_inflight, &session.uid).await;
        match membership {
            Ok(true) => {
                refreshed_session_cookie = Some(session_cookie_with_membership(
                    &session.uid,
                    &auth.guild_id,
                    auth,
                    session.exp,
                    session.issued_at,
                    unix_now_secs(),
                    0,
                ));
            }
            Ok(false) => {
                invalidate_permission_cache_for_user(&state.permission_cache, &session.uid).await;
                warn!(
                    user_id = %session.uid,
                    guild_id = %auth.guild_id,
                    "denying session after failed guild membership re-verification"
                );
                return auth_required_redirect_or_unauthorized(&request)
                    .with_cleared_session_cookie(auth.secure_cookie);
            }
            Err(status) => {
                warn!(
                    status = %status,
                    user_id = %session.uid,
                    "guild membership re-verify unavailable; allowing stale session"
                );
                refreshed_session_cookie = Some(session_cookie_with_membership(
                    &session.uid,
                    &auth.guild_id,
                    auth,
                    session.exp,
                    session.issued_at,
                    session.verified_at,
                    unix_now_secs(),
                ));
            }
        }
    }

    request
        .extensions_mut()
        .insert(AuthUserId(session.uid.clone()));
    let mut response = next.run(request).await;
    if let Some(cookie) = refreshed_session_cookie
        && let Ok(value) = header::HeaderValue::from_str(&cookie)
    {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

fn cleared_session_cookie(secure_cookie: bool) -> String {
    let secure_flag = if secure_cookie { "; Secure" } else { "" };
    format!("{SESSION_COOKIE_NAME}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0{secure_flag}")
}

trait AuthFailureResponse {
    fn with_cleared_session_cookie(self, secure_cookie: bool) -> Response;
}

impl AuthFailureResponse for Response {
    fn with_cleared_session_cookie(mut self, secure_cookie: bool) -> Response {
        if let Ok(value) = header::HeaderValue::from_str(&cleared_session_cookie(secure_cookie)) {
            self.headers_mut().append(header::SET_COOKIE, value);
        }
        self
    }
}

fn auth_required_redirect_or_unauthorized(request: &axum::extract::Request) -> Response {
    let path = request
        .uri()
        .path_and_query()
        .map_or_else(|| "/".to_owned(), |pq| pq.as_str().to_owned());

    if path.starts_with("/api/") {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let login_url = format!("/auth/login?redirect={}", percent_encode(&path));
    Redirect::temporary(&login_url).into_response()
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

async fn begin_membership_reverify(inflight: &MembershipReverifyInflight, user_id: &str) -> bool {
    let mut map = inflight.lock().await;
    let now = Instant::now();
    if let Some(started) = map.get(user_id)
        && now.duration_since(*started).as_secs() < MEMBERSHIP_REVERIFY_INFLIGHT_SECS
    {
        return false;
    }
    map.insert(user_id.to_owned(), now);
    if map.len() >= 5000 {
        map.retain(|_, started| {
            now.duration_since(*started).as_secs() < MEMBERSHIP_REVERIFY_INFLIGHT_SECS
        });
    }
    true
}

async fn end_membership_reverify(inflight: &MembershipReverifyInflight, user_id: &str) {
    inflight.lock().await.remove(user_id);
}

fn session_needs_membership_reverify(session: &SessionPayload) -> bool {
    let now = unix_now_secs();
    if now.saturating_sub(session.verified_at) < SESSION_MEMBERSHIP_VERIFY_INTERVAL_SECS {
        return false;
    }
    session.reverify_attempt_at == 0
        || now.saturating_sub(session.reverify_attempt_at)
            >= SESSION_MEMBERSHIP_VERIFY_INTERVAL_SECS
}

fn guild_member_status_indicates_membership(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::OK
}

async fn is_guild_member(
    state: &WebState,
    auth: &AuthConfig,
    user_id: &str,
) -> Result<bool, StatusCode> {
    let bot_auth = format!("Bot {}", auth.bot_token);
    let response = state
        .http_client
        .get(format!(
            "https://discord.com/api/guilds/{}/members/{user_id}",
            auth.guild_id
        ))
        .header("Authorization", &bot_auth)
        .send()
        .await
        .map_err(|err| {
            warn!(error = %err, user_id = %user_id, "discord guild member re-verify request failed");
            StatusCode::BAD_GATEWAY
        })?;

    let status = response.status();
    if guild_member_status_indicates_membership(status) {
        return Ok(true);
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(false);
    }
    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::UNAUTHORIZED {
        warn!(
            status = %status,
            user_id = %user_id,
            "discord guild member re-verify forbidden; check bot token and GUILD_MEMBERS intent"
        );
        return Err(StatusCode::BAD_GATEWAY);
    }

    warn!(
        status = %status,
        user_id = %user_id,
        "discord guild member re-verify returned unexpected status"
    );
    Err(StatusCode::BAD_GATEWAY)
}

async fn invalidate_permission_cache_for_user(cache: &PermissionCache, user_id: &str) {
    let mut cache = cache.write().await;
    cache.retain(|(uid, _), _| uid != user_id);
}

// ========== Auth: handlers ==========

#[derive(Deserialize)]
struct LoginParams {
    redirect: Option<String>,
}

async fn auth_login(State(state): State<WebState>, Query(params): Query<LoginParams>) -> Response {
    let Some(ref auth) = state.auth else {
        return (StatusCode::SERVICE_UNAVAILABLE, "OAuth not configured").into_response();
    };

    let redirect = sanitize_redirect(params.redirect.as_deref().unwrap_or("/"));
    let (_state_param, oauth_nonce_cookie, url) = prepare_oauth_login(&redirect, auth);

    Response::builder()
        .status(StatusCode::TEMPORARY_REDIRECT)
        .header(header::LOCATION, url)
        .header(header::SET_COOKIE, oauth_nonce_cookie)
        .body(axum::body::Body::empty())
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn prepare_oauth_login(redirect: &str, auth: &AuthConfig) -> (String, String, String) {
    let nonce = Uuid::new_v4().to_string();
    let state_param = sign_oauth_state(redirect, &nonce, &auth.session_secret);
    let oauth_nonce_cookie =
        format_oauth_nonce_cookie(&nonce, auth.secure_cookie, OAUTH_STATE_TTL_SECS);
    let url = format!(
        "https://discord.com/api/oauth2/authorize\
         ?client_id={}\
         &redirect_uri={}\
         &response_type=code\
         &scope=identify%20guilds\
         &state={}",
        percent_encode(&auth.client_id),
        percent_encode(&auth.redirect_uri),
        percent_encode(&state_param),
    );
    (state_param, oauth_nonce_cookie, url)
}

#[derive(Deserialize)]
struct CallbackParams {
    code: Option<String>,
    state: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
struct DiscordUserInfo {
    id: String,
}

#[derive(Deserialize)]
struct DiscordGuild {
    id: String,
}

async fn auth_callback(
    State(state): State<WebState>,
    headers: HeaderMap,
    Query(params): Query<CallbackParams>,
) -> Response {
    let Some(ref auth) = state.auth else {
        return (StatusCode::SERVICE_UNAVAILABLE, "OAuth not configured").into_response();
    };

    let redirect = match verify_oauth_callback_preexchange(&params, &headers, &auth.session_secret)
    {
        Ok(redirect) => redirect,
        Err((status, message)) => {
            return oauth_callback_failure_response(status, message, auth.secure_cookie);
        }
    };

    // Exchange code for access token
    let token: TokenResponse = match state
        .http_client
        .post("https://discord.com/api/oauth2/token")
        .form(&[
            ("client_id", auth.client_id.as_str()),
            ("client_secret", auth.client_secret.as_str()),
            ("grant_type", "authorization_code"),
            (
                "code",
                params.code.as_deref().expect("preexchange ensures code"),
            ),
            ("redirect_uri", auth.redirect_uri.as_str()),
        ])
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.json().await {
            Ok(t) => t,
            Err(_) => {
                return oauth_callback_failure_response(
                    StatusCode::BAD_GATEWAY,
                    "invalid token response",
                    auth.secure_cookie,
                );
            }
        },
        _ => {
            return oauth_callback_failure_response(
                StatusCode::BAD_GATEWAY,
                "token exchange failed",
                auth.secure_cookie,
            );
        }
    };

    let bearer = format!("Bearer {}", token.access_token);

    // Fetch user info and guilds in parallel
    let (user_res, guilds_res) = tokio::join!(
        state
            .http_client
            .get("https://discord.com/api/users/@me")
            .header("Authorization", &bearer)
            .send(),
        state
            .http_client
            .get("https://discord.com/api/users/@me/guilds")
            .header("Authorization", &bearer)
            .send(),
    );

    let user: DiscordUserInfo = match user_res {
        Ok(resp) if resp.status().is_success() => match resp.json().await {
            Ok(u) => u,
            Err(_) => {
                return oauth_callback_failure_response(
                    StatusCode::BAD_GATEWAY,
                    "invalid user response",
                    auth.secure_cookie,
                );
            }
        },
        _ => {
            return oauth_callback_failure_response(
                StatusCode::BAD_GATEWAY,
                "failed to fetch user",
                auth.secure_cookie,
            );
        }
    };

    let guilds: Vec<DiscordGuild> = match guilds_res {
        Ok(resp) if resp.status().is_success() => match resp.json().await {
            Ok(g) => g,
            Err(_) => {
                return oauth_callback_failure_response(
                    StatusCode::BAD_GATEWAY,
                    "invalid guilds response",
                    auth.secure_cookie,
                );
            }
        },
        _ => {
            return oauth_callback_failure_response(
                StatusCode::BAD_GATEWAY,
                "failed to fetch guilds",
                auth.secure_cookie,
            );
        }
    };

    if !guilds.iter().any(|g| g.id == auth.guild_id) {
        return oauth_callback_failure_response(
            StatusCode::FORBIDDEN,
            "not a member of this server",
            auth.secure_cookie,
        );
    }

    // Create session cookie with user ID
    let redirect = sanitize_redirect(&redirect);
    let session_cookie = session_cookie_value(&user.id, &auth.guild_id, auth);
    let clear_oauth_nonce_cookie = format_oauth_nonce_cookie("", auth.secure_cookie, 0);

    Response::builder()
        .status(StatusCode::TEMPORARY_REDIRECT)
        .header(header::LOCATION, &redirect)
        .header(header::SET_COOKIE, session_cookie)
        .header(header::SET_COOKIE, clear_oauth_nonce_cookie)
        .body(axum::body::Body::empty())
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn auth_logout_get_rejected() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST")]).into_response()
}

async fn auth_logout(State(state): State<WebState>, headers: HeaderMap) -> Response {
    let secure_flag = if state.auth.as_ref().is_some_and(|a| a.secure_cookie) {
        "; Secure"
    } else {
        ""
    };
    if let Some(ref auth) = state.auth
        && let Some(cookie_val) = get_cookie(&headers, SESSION_COOKIE_NAME)
        && let Some(session) = verify_session(&cookie_val, &auth.session_secret)
        && let Err(err) = revoke_session(&state.db, &session.uid, session.issued_at).await
    {
        warn!(
            error = %err,
            user_id = %session.uid,
            "failed to persist session revocation"
        );
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let cookie =
        format!("{SESSION_COOKIE_NAME}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0{secure_flag}",);
    Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, "/")
        .header(header::SET_COOKIE, cookie)
        .body(axum::body::Body::empty())
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn revoke_session(
    db: &PgClient,
    user_id: &str,
    issued_at: u64,
) -> Result<(), tokio_postgres::Error> {
    db.execute(
        crate::infrastructure::sql::REVOKE_SESSION_SQL,
        &[&user_id, &(issued_at as i64)],
    )
    .await?;
    Ok(())
}

async fn session_is_revoked(
    db: &PgClient,
    user_id: &str,
    issued_at: u64,
) -> Result<bool, tokio_postgres::Error> {
    let row = db
        .query_opt(
            crate::infrastructure::sql::SESSION_IS_REVOKED_SQL,
            &[&user_id, &(issued_at as i64)],
        )
        .await?;
    Ok(row.is_some())
}

// ========== Auth: session helpers ==========

#[derive(Serialize, Deserialize)]
struct SessionPayload {
    uid: String,
    gid: String,
    exp: u64,
    #[serde(default)]
    verified_at: u64,
    #[serde(default)]
    reverify_attempt_at: u64,
    #[serde(default)]
    issued_at: u64,
}

fn session_cookie_value(user_id: &str, guild_id: &str, auth: &AuthConfig) -> String {
    let now = unix_now_secs();
    session_cookie_with_membership(user_id, guild_id, auth, now + SESSION_TTL_SECS, now, now, 0)
}

fn session_cookie_with_membership(
    user_id: &str,
    guild_id: &str,
    auth: &AuthConfig,
    exp: u64,
    issued_at: u64,
    verified_at: u64,
    reverify_attempt_at: u64,
) -> String {
    let session_value = sign_session_with_exp(
        user_id,
        guild_id,
        &auth.session_secret,
        exp,
        issued_at,
        verified_at,
        reverify_attempt_at,
    );
    let secure_flag = if auth.secure_cookie { "; Secure" } else { "" };
    let max_age = exp.saturating_sub(unix_now_secs()).max(1);
    format!(
        "{SESSION_COOKIE_NAME}={session_value}; HttpOnly; SameSite=Lax; Path=/; Max-Age={max_age}{secure_flag}",
    )
}

fn sign_session_with_exp(
    user_id: &str,
    guild_id: &str,
    secret: &str,
    exp: u64,
    issued_at: u64,
    verified_at: u64,
    reverify_attempt_at: u64,
) -> String {
    let payload = SessionPayload {
        uid: user_id.to_owned(),
        gid: guild_id.to_owned(),
        exp,
        issued_at,
        verified_at,
        reverify_attempt_at,
    };
    let json = serde_json::to_string(&payload).unwrap_or_default();
    let payload_hex = to_hex(json.as_bytes());
    let sig_hex = hmac_hex(secret, &payload_hex);
    format!("{payload_hex}.{sig_hex}")
}

#[cfg(test)]
fn sign_session(user_id: &str, guild_id: &str, secret: &str, now: u64, verified_at: u64) -> String {
    sign_session_with_exp(
        user_id,
        guild_id,
        secret,
        now + SESSION_TTL_SECS,
        now,
        verified_at,
        0,
    )
}

fn verify_session(cookie: &str, secret: &str) -> Option<SessionPayload> {
    let (payload_hex, sig_hex) = cookie.rsplit_once('.')?;
    let expected = hmac_hex(secret, payload_hex);
    if !constant_time_eq(sig_hex.as_bytes(), expected.as_bytes()) {
        return None;
    }
    let payload_bytes = from_hex(payload_hex)?;
    let mut payload: SessionPayload = serde_json::from_slice(&payload_bytes).ok()?;
    let now = unix_now_secs();
    if now >= payload.exp {
        return None;
    }
    if payload.issued_at == 0 {
        payload.issued_at = payload.exp.saturating_sub(SESSION_TTL_SECS);
    }
    if payload.verified_at == 0 {
        payload.verified_at = payload.issued_at;
    }
    Some(payload)
}

fn format_oauth_nonce_cookie(nonce: &str, secure_cookie: bool, max_age: u64) -> String {
    let secure_flag = if secure_cookie { "; Secure" } else { "" };
    if max_age == 0 {
        format!(
            "{OAUTH_NONCE_COOKIE_NAME}={nonce}; HttpOnly; SameSite=Lax; Path={OAUTH_NONCE_COOKIE_PATH}; Max-Age=0{secure_flag}",
        )
    } else {
        format!(
            "{OAUTH_NONCE_COOKIE_NAME}={nonce}; HttpOnly; SameSite=Lax; Path={OAUTH_NONCE_COOKIE_PATH}; Max-Age={max_age}{secure_flag}",
        )
    }
}

fn oauth_callback_failure_response(
    status: StatusCode,
    message: &'static str,
    secure_cookie: bool,
) -> Response {
    Response::builder()
        .status(status)
        .header(
            header::SET_COOKIE,
            format_oauth_nonce_cookie("", secure_cookie, 0),
        )
        .body(axum::body::Body::from(message))
        .unwrap_or_else(|_| status.into_response())
}

fn verify_oauth_callback_preexchange(
    params: &CallbackParams,
    headers: &HeaderMap,
    secret: &str,
) -> Result<String, (StatusCode, &'static str)> {
    let Some(_) = params.code.as_ref() else {
        return Err((StatusCode::BAD_REQUEST, "missing code"));
    };
    let Some(state_param) = params.state.as_ref() else {
        return Err((StatusCode::BAD_REQUEST, "missing state"));
    };
    let Some(cookie_nonce) = get_cookie(headers, OAUTH_NONCE_COOKIE_NAME) else {
        return Err((StatusCode::BAD_REQUEST, "missing oauth nonce"));
    };
    if cookie_nonce.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "missing oauth nonce"));
    }
    let Some(redirect) = verify_oauth_state(state_param, &cookie_nonce, secret) else {
        return Err((StatusCode::BAD_REQUEST, "invalid state"));
    };
    Ok(redirect)
}

fn oauth_nonce_digest(secret: &str, nonce: &str) -> String {
    hmac_hex(secret, &format!("oauth-nonce:{nonce}"))
}

fn sign_oauth_state(redirect: &str, nonce: &str, secret: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let digest = oauth_nonce_digest(secret, nonce);
    let json = serde_json::json!({"r": redirect, "t": now, "n": digest}).to_string();
    let payload_hex = to_hex(json.as_bytes());
    let sig_hex = hmac_hex(secret, &payload_hex);
    format!("{payload_hex}.{sig_hex}")
}

fn verify_oauth_state(state: &str, cookie_nonce: &str, secret: &str) -> Option<String> {
    let (payload_hex, sig_hex) = state.rsplit_once('.')?;
    let expected = hmac_hex(secret, payload_hex);
    if !constant_time_eq(sig_hex.as_bytes(), expected.as_bytes()) {
        return None;
    }
    let bytes = from_hex(payload_hex)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let created = value.get("t")?.as_u64()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if created > now || now.saturating_sub(created) > OAUTH_STATE_TTL_SECS {
        return None;
    }
    let state_digest = value.get("n")?.as_str()?;
    let expected_digest = oauth_nonce_digest(secret, cookie_nonce);
    if !constant_time_eq(state_digest.as_bytes(), expected_digest.as_bytes()) {
        return None;
    }
    value.get("r")?.as_str().map(|s| s.to_owned())
}

// ========== Channel permission check ==========

/// Meeting workspace identifiers returned alongside an access check so callers
/// don't need to re-query the meetings table to find the workspace.
#[derive(Debug, Clone)]
struct MeetingAccess {
    guild_id: String,
    voice_channel_id: String,
}

fn meeting_access_from_row(
    guild_id: String,
    voice_channel_id: String,
    authenticated_guild_id: &str,
) -> Result<MeetingAccess, StatusCode> {
    if guild_id != authenticated_guild_id {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(MeetingAccess {
        guild_id,
        voice_channel_id,
    })
}

async fn verify_meeting_access_after_row<Fut>(
    guild_id: String,
    channel_id: String,
    authenticated_guild_id: &str,
    user_id: &str,
    permission_cache: &PermissionCache,
    permission_check: Fut,
) -> Result<MeetingAccess, StatusCode>
where
    Fut: Future<Output = Result<CachedChannelPermission, StatusCode>>,
{
    let access = meeting_access_from_row(guild_id, channel_id.clone(), authenticated_guild_id)?;

    // Check permission cache
    let cache_key = (user_id.to_owned(), channel_id.clone());
    {
        let cache = permission_cache.read().await;
        if let Some(&(permission, expires_at)) = cache.get(&cache_key)
            && Instant::now() < expires_at
        {
            return if permission.can_view {
                Ok(access)
            } else {
                Err(StatusCode::FORBIDDEN)
            };
        }
    }

    // Cache miss — query Discord API
    let permission = permission_check.await?;

    // Store result in cache (also evict expired entries periodically)
    {
        let mut cache = permission_cache.write().await;
        let expires_at = Instant::now() + std::time::Duration::from_secs(PERMISSION_CACHE_TTL_SECS);
        cache.insert(cache_key, (permission, expires_at));

        // Evict expired entries if cache grows large
        if cache.len() > 5000 {
            let now = Instant::now();
            cache.retain(|_, (_, exp)| *exp > now);
        }
    }

    if permission.can_view {
        Ok(access)
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

/// Verify that the authenticated user has VIEW_CHANNEL permission on the
/// voice channel where the meeting was recorded. Returns the meeting's
/// guild/voice-channel IDs so callers can build paths without an extra
/// DB round-trip.
/// Results are cached per (user_id, channel_id) for 5 minutes to avoid
/// Discord API rate-limit exhaustion on page loads (which trigger ~4 requests).
async fn verify_meeting_access(
    state: &WebState,
    meeting_id: &str,
    user_id: &str,
) -> Result<MeetingAccess, StatusCode> {
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;

    // Look up the meeting's voice channel and guild ID in a single round-trip.
    let row = state
        .db
        .query_opt(
            "SELECT guild_id, voice_channel_id FROM meetings WHERE id=$1",
            &[&meeting_id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let guild_id: String = row.get("guild_id");
    let channel_id: String = row.get("voice_channel_id");
    verify_meeting_access_after_row(
        guild_id,
        channel_id.clone(),
        &auth.guild_id,
        user_id,
        &state.permission_cache,
        resolve_channel_permission_flags(state, auth, &channel_id, user_id),
    )
    .await
}

/// Fetch guild info with caching. Guild data is shared across all requests
/// since the server operates on a single guild.
async fn get_guild_info(
    state: &WebState,
    auth: &AuthConfig,
) -> Result<DiscordGuildFull, StatusCode> {
    // Fast path: read lock
    {
        let cache = state.guild_cache.read().await;
        if let Some((ref guild, expires_at)) = *cache
            && Instant::now() < expires_at
        {
            return Ok(guild.clone());
        }
    }

    // Slow path: hold write lock for the entire fetch to serialize concurrent misses
    let mut cache = state.guild_cache.write().await;
    if let Some((ref guild, expires_at)) = *cache
        && Instant::now() < expires_at
    {
        return Ok(guild.clone());
    }

    let bot_auth = format!("Bot {}", auth.bot_token);
    let guild_resp = state
        .http_client
        .get(format!("https://discord.com/api/guilds/{}", auth.guild_id))
        .header("Authorization", &bot_auth)
        .send()
        .await
        .map_err(|err| {
            warn!(error = %err, "discord guild API request failed");
            StatusCode::BAD_GATEWAY
        })?;

    let guild_status = guild_resp.status();
    let guild_body = guild_resp.text().await.map_err(|err| {
        warn!(error = %err, "discord guild API response read failed");
        StatusCode::BAD_GATEWAY
    })?;
    let guild: DiscordGuildFull = serde_json::from_str(&guild_body).map_err(|err| {
        warn!(
            error = %err,
            status = %guild_status,
            body_len = guild_body.len(),
            "discord guild API response parse failed"
        );
        tracing::debug!(
            body_len = guild_body.len(),
            body_prefix = %utf8_safe_byte_prefix(&guild_body, 500),
            "discord guild API response parse debug"
        );
        StatusCode::BAD_GATEWAY
    })?;

    let expires_at = Instant::now() + std::time::Duration::from_secs(GUILD_CACHE_TTL_SECS);
    *cache = Some((guild.clone(), expires_at));

    Ok(guild)
}

/// Query Discord API for resolved channel permissions. Returns Ok(None) when
/// the user or channel is inaccessible, Err on upstream API failure.
async fn resolve_channel_permissions(
    state: &WebState,
    auth: &AuthConfig,
    channel_id: &str,
    user_id: &str,
) -> Result<Option<u64>, StatusCode> {
    let bot_auth = format!("Bot {}", auth.bot_token);

    // Fetch guild from cache, channel and member from API in parallel
    let (guild_result, channel_res, member_res) = tokio::join!(
        get_guild_info(state, auth),
        state
            .http_client
            .get(format!("https://discord.com/api/channels/{channel_id}"))
            .header("Authorization", &bot_auth)
            .send(),
        state
            .http_client
            .get(format!(
                "https://discord.com/api/guilds/{}/members/{user_id}",
                auth.guild_id
            ))
            .header("Authorization", &bot_auth)
            .send(),
    );

    let guild = guild_result?;

    let channel_resp = channel_res.map_err(|err| {
        warn!(error = %err, "discord channel API request failed");
        StatusCode::BAD_GATEWAY
    })?;
    let channel_status = channel_resp.status();
    let retry_after_header = channel_resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_owned());

    let channel_body = channel_resp.text().await.map_err(|err| {
        warn!(error = %err, "discord channel API response read failed");
        StatusCode::BAD_GATEWAY
    })?;

    if channel_status == reqwest::StatusCode::NOT_FOUND
        || channel_status == reqwest::StatusCode::FORBIDDEN
        || channel_status == reqwest::StatusCode::UNAUTHORIZED
    {
        return Ok(None);
    }
    if channel_status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        warn!(
            status = %channel_status,
            retry_after = retry_after_header.as_deref(),
            body_len = channel_body.len(),
            "discord channel API rate limited"
        );
        return Err(StatusCode::BAD_GATEWAY);
    }
    if !channel_status.is_success() {
        warn!(
            status = %channel_status,
            body_len = channel_body.len(),
            "discord channel API non-success"
        );
        return Err(StatusCode::BAD_GATEWAY);
    }

    let channel: DiscordChannelFull = serde_json::from_str(&channel_body).map_err(|err| {
        warn!(
            error = %err,
            status = %channel_status,
            body_len = channel_body.len(),
            "discord channel API response parse failed"
        );
        tracing::debug!(
            body_len = channel_body.len(),
            body_prefix = %utf8_safe_byte_prefix(&channel_body, 500),
            "discord channel API response parse debug"
        );
        StatusCode::BAD_GATEWAY
    })?;

    let member_resp = member_res.map_err(|err| {
        warn!(error = %err, "discord member API request failed");
        StatusCode::BAD_GATEWAY
    })?;
    if member_resp.status() == reqwest::StatusCode::NOT_FOUND
        || member_resp.status() == reqwest::StatusCode::FORBIDDEN
    {
        return Ok(None);
    }
    if !member_resp.status().is_success() {
        warn!(status = %member_resp.status(), "discord member API non-success");
        return Err(StatusCode::BAD_GATEWAY);
    }
    let member: DiscordMemberFull = member_resp.json().await.map_err(|err| {
        warn!(error = %err, "discord member API response parse failed");
        StatusCode::BAD_GATEWAY
    })?;

    Ok(Some(compute_channel_permissions(
        user_id,
        &guild.owner_id,
        &auth.guild_id,
        &member.roles,
        &guild.roles,
        &channel.permission_overwrites,
    )))
}

/// Query Discord API for channel permission flags.
async fn resolve_channel_permission_flags(
    state: &WebState,
    auth: &AuthConfig,
    channel_id: &str,
    user_id: &str,
) -> Result<CachedChannelPermission, StatusCode> {
    let Some(perms) = resolve_channel_permissions(state, auth, channel_id, user_id).await? else {
        return Ok(CachedChannelPermission {
            can_view: false,
            is_admin: false,
        });
    };
    let is_admin = perms & ADMINISTRATOR != 0;
    Ok(CachedChannelPermission {
        can_view: perms & VIEW_CHANNEL != 0 || is_admin,
        is_admin,
    })
}

async fn check_guild_admin_permission(
    state: &WebState,
    auth: &AuthConfig,
    user_id: &str,
) -> Result<bool, StatusCode> {
    let cache_key = (user_id.to_owned(), "__guild__".to_owned());

    // Check cache first
    {
        let cache = state.permission_cache.read().await;
        if let Some(&(permission, expires_at)) = cache.get(&cache_key)
            && Instant::now() < expires_at
        {
            return Ok(permission.is_admin);
        }
    }

    // Fast path: check if user is guild owner
    let guild = get_guild_info(state, auth).await?;
    if user_id == guild.owner_id {
        cache_guild_admin_permission(state, user_id, true).await;
        return Ok(true);
    }

    // Slow path: fetch member roles and check ADMINISTRATOR bit
    let bot_auth = format!("Bot {}", auth.bot_token);
    let member_resp = state
        .http_client
        .get(format!(
            "https://discord.com/api/guilds/{}/members/{user_id}",
            auth.guild_id
        ))
        .header("Authorization", &bot_auth)
        .send()
        .await;

    // Handle request errors as retryable upstream failures.
    if let Err(err) = member_resp {
        warn!(error = %err, "discord member API request failed");
        return Err(StatusCode::BAD_GATEWAY);
    }

    let resp_status = member_resp.as_ref().unwrap().status();

    // Handle rate limiting as error (don't cache), treat 404/403 as not admin
    if resp_status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        warn!(status = %resp_status, "discord member API rate limited");
        return Err(StatusCode::BAD_GATEWAY);
    }

    if resp_status == reqwest::StatusCode::NOT_FOUND
        || resp_status == reqwest::StatusCode::FORBIDDEN
        || resp_status == reqwest::StatusCode::UNAUTHORIZED
    {
        cache_guild_admin_permission(state, user_id, false).await;
        return Ok(false);
    }

    if !resp_status.is_success() {
        warn!(status = %resp_status, "discord member API non-success");
        return Err(StatusCode::BAD_GATEWAY);
    }

    let member: DiscordMemberFull = match member_resp.unwrap().json().await {
        Ok(m) => m,
        Err(err) => {
            warn!(error = %err, "discord member API response parse failed");
            return Err(StatusCode::BAD_GATEWAY);
        }
    };

    let permissions = compute_channel_permissions(
        user_id,
        &guild.owner_id,
        &auth.guild_id,
        &member.roles,
        &guild.roles,
        &[],
    );
    let is_admin = permissions & ADMINISTRATOR != 0;

    cache_guild_admin_permission(state, user_id, is_admin).await;
    Ok(is_admin)
}

async fn cache_guild_admin_permission(state: &WebState, user_id: &str, is_admin: bool) {
    let mut cache = state.permission_cache.write().await;
    let expires_at = Instant::now() + std::time::Duration::from_secs(PERMISSION_CACHE_TTL_SECS);
    cache.insert(
        (user_id.to_owned(), "__guild__".to_owned()),
        (
            CachedChannelPermission {
                can_view: is_admin,
                is_admin,
            },
            expires_at,
        ),
    );

    // Evict old entries if cache is too large (same pattern as check_channel_admin_permission)
    if cache.len() > 5000 {
        let now = Instant::now();
        cache.retain(|_, (_, exp)| *exp > now);
    }
}

async fn check_channel_admin_permission(
    state: &WebState,
    auth: &AuthConfig,
    channel_id: &str,
    user_id: &str,
) -> Result<bool, StatusCode> {
    let cache_key = (user_id.to_owned(), channel_id.to_owned());
    {
        let cache = state.permission_cache.read().await;
        if let Some(&(permission, expires_at)) = cache.get(&cache_key)
            && Instant::now() < expires_at
        {
            return Ok(permission.is_admin);
        }
    }

    let permission = resolve_channel_permission_flags(state, auth, channel_id, user_id).await?;
    {
        let mut cache = state.permission_cache.write().await;
        let expires_at = Instant::now() + std::time::Duration::from_secs(PERMISSION_CACHE_TTL_SECS);
        cache.insert(cache_key, (permission, expires_at));
        if cache.len() > 5000 {
            let now = Instant::now();
            cache.retain(|_, (_, exp)| *exp > now);
        }
    }
    Ok(permission.is_admin)
}

// Discord API response types for permission checking

fn zero_permission_bits() -> u64 {
    0
}

/// Discord API returns permission values as either strings or integers
/// depending on the API version and context. Accept both.
fn deserialize_permission_bits<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct PermissionBitsVisitor;
    impl<'de> de::Visitor<'de> for PermissionBitsVisitor {
        type Value = u64;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a non-negative u64 permission bitset as a string or number")
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<u64, E> {
            v.parse::<u64>()
                .map_err(|_| E::custom(format!("invalid permission bitset: {v}")))
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<u64, E> {
            Ok(v)
        }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<u64, E> {
            u64::try_from(v).map_err(|_| E::custom(format!("invalid permission bitset: {v}")))
        }
    }
    deserializer.deserialize_any(PermissionBitsVisitor)
}

#[derive(Deserialize, Clone)]
struct DiscordGuildFull {
    owner_id: String,
    roles: Vec<DiscordRoleFull>,
}

#[derive(Deserialize, Clone)]
struct DiscordRoleFull {
    id: String,
    #[serde(deserialize_with = "deserialize_permission_bits")]
    permissions: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiscordOverwriteType {
    Role,
    Member,
}

impl<'de> Deserialize<'de> for DiscordOverwriteType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de;

        struct DiscordOverwriteTypeVisitor;

        impl<'de> de::Visitor<'de> for DiscordOverwriteTypeVisitor {
            type Value = DiscordOverwriteType;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("0, 1, \"role\", or \"member\"")
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                match value {
                    0 => Ok(DiscordOverwriteType::Role),
                    1 => Ok(DiscordOverwriteType::Member),
                    other => Err(E::custom(format!("invalid overwrite type: {other}"))),
                }
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
                match value {
                    0 => Ok(DiscordOverwriteType::Role),
                    1 => Ok(DiscordOverwriteType::Member),
                    other => Err(E::custom(format!("invalid overwrite type: {other}"))),
                }
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                match value {
                    "0" | "role" => Ok(DiscordOverwriteType::Role),
                    "1" | "member" => Ok(DiscordOverwriteType::Member),
                    other => Err(E::custom(format!("invalid overwrite type: {other}"))),
                }
            }
        }

        deserializer.deserialize_any(DiscordOverwriteTypeVisitor)
    }
}

#[derive(Deserialize)]
struct DiscordOverwrite {
    id: String,
    #[serde(rename = "type")]
    type_: DiscordOverwriteType,
    #[serde(
        default = "zero_permission_bits",
        deserialize_with = "deserialize_permission_bits"
    )]
    allow: u64,
    #[serde(
        default = "zero_permission_bits",
        deserialize_with = "deserialize_permission_bits"
    )]
    deny: u64,
}

fn deserialize_permission_overwrites<'de, D>(
    deserializer: D,
) -> Result<Vec<DiscordOverwrite>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt = Option::<Vec<DiscordOverwrite>>::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}

#[derive(Deserialize)]
struct DiscordChannelFull {
    #[serde(default, deserialize_with = "deserialize_permission_overwrites")]
    permission_overwrites: Vec<DiscordOverwrite>,
}

#[derive(Deserialize)]
struct DiscordMemberFull {
    roles: Vec<String>,
}

/// Compute a user's effective permissions for a channel following Discord's
/// permission resolution algorithm.
fn compute_channel_permissions(
    user_id: &str,
    owner_id: &str,
    guild_id: &str,
    member_roles: &[String],
    guild_roles: &[DiscordRoleFull],
    overwrites: &[DiscordOverwrite],
) -> u64 {
    // Guild owner has all permissions
    if user_id == owner_id {
        return u64::MAX;
    }

    // Base permissions from @everyone role (id == guild_id)
    let mut permissions: u64 = guild_roles
        .iter()
        .find(|r| r.id == guild_id)
        .map(|r| r.permissions)
        .unwrap_or(0);

    // Add permissions from member's roles
    for role in guild_roles {
        if member_roles.contains(&role.id) {
            permissions |= role.permissions;
        }
    }

    // Administrator bypasses all channel overwrites
    if permissions & ADMINISTRATOR != 0 {
        return u64::MAX;
    }

    // Apply @everyone overwrite
    if let Some(ow) = overwrites
        .iter()
        .find(|o| matches!(o.type_, DiscordOverwriteType::Role) && o.id == guild_id)
    {
        let allow = ow.allow;
        let deny = ow.deny;
        permissions &= !deny;
        permissions |= allow;
    }

    // Apply role overwrites (union of allow/deny across all matching roles)
    let mut role_allow: u64 = 0;
    let mut role_deny: u64 = 0;
    for ow in overwrites.iter().filter(|o| {
        matches!(o.type_, DiscordOverwriteType::Role)
            && o.id != guild_id
            && member_roles.contains(&o.id)
    }) {
        role_allow |= ow.allow;
        role_deny |= ow.deny;
    }
    permissions &= !role_deny;
    permissions |= role_allow;

    // Apply member-specific overwrite
    if let Some(ow) = overwrites
        .iter()
        .find(|o| matches!(o.type_, DiscordOverwriteType::Member) && o.id == user_id)
    {
        let allow = ow.allow;
        let deny = ow.deny;
        permissions &= !deny;
        permissions |= allow;
    }

    permissions
}

// ========== Auth: crypto helpers ==========

fn hmac_hex(secret: &str, data: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(data.as_bytes());
    to_hex(&mac.finalize().into_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Ensure redirect is a safe relative path (prevents open redirect).
fn sanitize_redirect(input: &str) -> String {
    if input.starts_with('/') && !input.starts_with("//") && input.len() <= 2048 {
        input.to_owned()
    } else {
        "/".to_owned()
    }
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

fn get_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let header = headers.get(header::COOKIE)?.to_str().ok()?;
    for pair in header.split(';') {
        let pair = pair.trim();
        if let Some((key, value)) = pair.split_once('=')
            && key.trim() == name
        {
            return Some(value.trim().to_owned());
        }
    }
    None
}

// ---------- Response types ----------

#[derive(Serialize)]
struct MeetingResponse {
    id: String,
    title: Option<String>,
    status: String,
    started_at: Option<String>,
    stopped_at: Option<String>,
    duration_seconds: Option<i32>,
}

#[derive(Serialize)]
struct CurrentUserResponse {
    user_id: String,
    guild_id: String,
    is_admin: bool,
}

#[derive(Deserialize)]
struct GuildMeetingsQuery {
    page: Option<u32>,
    limit: Option<u32>,
}

#[derive(Serialize)]
struct GuildMeetingEntryResponse {
    id: String,
    title: Option<String>,
    status: String,
    started_at: Option<String>,
    stopped_at: Option<String>,
    duration_seconds: Option<i32>,
    stop_reason: Option<String>,
}

#[derive(Serialize)]
struct GuildMeetingsResponse {
    meetings: Vec<GuildMeetingEntryResponse>,
    page: u32,
    limit: u32,
    total: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct GuildSettingsResponse {
    whisper_language: Option<String>,
    whisper_language_explicit: bool,
    whisper_vad: bool,
    auto_stop_grace_seconds: i64,
    retention_raw_audio_ttl_days: i32,
    retention_transcript_ttl_days: i32,
    summary_enabled: bool,
    is_admin: bool,
}

#[derive(Debug, Deserialize)]
struct GuildSettingsUpdateRequest {
    whisper_language: Option<String>,
    whisper_vad: bool,
    auto_stop_grace_seconds: i64,
    retention_raw_audio_ttl_days: i32,
    retention_transcript_ttl_days: i32,
    summary_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredGuildSettings {
    whisper_language: Option<String>,
    whisper_language_explicit: bool,
    whisper_vad: Option<bool>,
    auto_stop_grace_seconds: Option<i64>,
    retention_raw_audio_ttl_days: Option<i32>,
    retention_transcript_ttl_days: Option<i32>,
    summary_enabled: Option<bool>,
}

#[derive(Serialize)]
struct SpeakerResponse {
    id: String,
    username: Option<String>,
    nickname: Option<String>,
    display_name: Option<String>,
    display_label: String,
}

#[derive(Serialize)]
struct TranscriptSegmentResponse {
    speaker_id: String,
    speaker: SpeakerResponse,
    start_ms: i32,
    end_ms: i32,
    text: String,
    confidence: Option<f64>,
    is_noisy: bool,
    source: String,
}

#[derive(Serialize)]
struct SummaryResponse {
    markdown: Option<String>,
}

#[derive(Serialize)]
struct SpeakerAudioResponse {
    speaker_id: String,
    username: Option<String>,
    nickname: Option<String>,
    display_name: Option<String>,
    display_label: String,
    has_audio: bool,
}

#[derive(Serialize)]
struct DebugArtifactEntry {
    id: String,
    label: String,
    category: &'static str,
    available: bool,
    download_url: String,
    filename: String,
    content_type: &'static str,
}

// ---------- Handlers ----------

fn normalize_guild_meetings_pagination(query: GuildMeetingsQuery) -> (u32, u32) {
    let page = query.page.unwrap_or(1).max(1);
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    (page, limit)
}

fn validate_guild_settings_update(request: &GuildSettingsUpdateRequest) -> Result<(), StatusCode> {
    if let Some(language) = request.whisper_language.as_deref()
        && !is_iso639_1_format(language)
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    if !(10..=3600).contains(&request.auto_stop_grace_seconds) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if !(1..=365).contains(&request.retention_raw_audio_ttl_days)
        || !(1..=365).contains(&request.retention_transcript_ttl_days)
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

fn guild_settings_response(
    defaults: &GuildSettingsDefaults,
    stored: Option<StoredGuildSettings>,
    is_admin: bool,
) -> GuildSettingsResponse {
    let stored = stored.unwrap_or(StoredGuildSettings {
        whisper_language: None,
        whisper_language_explicit: false,
        whisper_vad: None,
        auto_stop_grace_seconds: None,
        retention_raw_audio_ttl_days: None,
        retention_transcript_ttl_days: None,
        summary_enabled: None,
    });
    let whisper_language = if stored.whisper_language_explicit {
        stored.whisper_language
    } else {
        defaults.whisper_language.clone()
    };
    GuildSettingsResponse {
        whisper_language,
        whisper_language_explicit: stored.whisper_language_explicit,
        whisper_vad: stored.whisper_vad.unwrap_or(defaults.whisper_vad),
        auto_stop_grace_seconds: stored
            .auto_stop_grace_seconds
            .unwrap_or(defaults.auto_stop_grace_seconds),
        retention_raw_audio_ttl_days: stored
            .retention_raw_audio_ttl_days
            .unwrap_or(defaults.retention_raw_audio_ttl_days),
        retention_transcript_ttl_days: stored
            .retention_transcript_ttl_days
            .unwrap_or(defaults.retention_transcript_ttl_days),
        summary_enabled: stored.summary_enabled.unwrap_or(true),
        is_admin,
    }
}

async fn current_user_is_guild_admin(state: &WebState, user_id: &str) -> Result<bool, StatusCode> {
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    check_guild_admin_permission(state, auth, user_id).await
}

async fn api_me(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
) -> Result<Json<CurrentUserResponse>, StatusCode> {
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let is_admin = check_guild_admin_permission(&state, auth, &user_id).await?;
    Ok(Json(CurrentUserResponse {
        user_id,
        guild_id: auth.guild_id.clone(),
        is_admin,
    }))
}

async fn api_guild_meetings(
    State(state): State<WebState>,
    Query(query): Query<GuildMeetingsQuery>,
) -> Result<Json<GuildMeetingsResponse>, StatusCode> {
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let (page, limit) = normalize_guild_meetings_pagination(query);
    let offset = i64::from(page.saturating_sub(1)) * i64::from(limit);
    let limit_i64 = i64::from(limit);

    let count_row = state
        .db
        .query_one(COUNT_GUILD_MEETINGS_SQL, &[&auth.guild_id])
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let total: i64 = count_row.get(0);

    let rows = state
        .db
        .query(
            "SELECT id, status, \
                    to_char(started_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') as started_at, \
                    to_char(stopped_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') as stopped_at, \
                    meeting_duration_seconds, title, stop_reason \
             FROM meetings \
             WHERE guild_id = $1 \
             ORDER BY started_at DESC \
             LIMIT $2 OFFSET $3",
            &[&auth.guild_id, &limit_i64, &offset],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let meetings = rows
        .iter()
        .map(|row| GuildMeetingEntryResponse {
            id: row.get("id"),
            title: row.get("title"),
            status: row.get("status"),
            started_at: row.get("started_at"),
            stopped_at: row.get("stopped_at"),
            duration_seconds: row.get("meeting_duration_seconds"),
            stop_reason: row.get("stop_reason"),
        })
        .collect();

    Ok(Json(GuildMeetingsResponse {
        meetings,
        page,
        limit,
        total,
    }))
}

async fn load_guild_settings(
    state: &WebState,
    guild_id: &str,
) -> Result<Option<StoredGuildSettings>, StatusCode> {
    let row = state
        .db
        .query_opt(GET_GUILD_SETTINGS_SQL, &[&guild_id])
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(row.map(|row| StoredGuildSettings {
        whisper_language: row.get("whisper_language"),
        whisper_language_explicit: row.get("whisper_language_explicit"),
        whisper_vad: row.get("whisper_vad"),
        auto_stop_grace_seconds: row.get("auto_stop_grace_seconds"),
        retention_raw_audio_ttl_days: row.get("retention_raw_audio_ttl_days"),
        retention_transcript_ttl_days: row.get("retention_transcript_ttl_days"),
        summary_enabled: row.get("summary_enabled"),
    }))
}

async fn api_guild_settings(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
) -> Result<Json<GuildSettingsResponse>, StatusCode> {
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let is_admin = current_user_is_guild_admin(&state, &user_id).await?;
    let stored = load_guild_settings(&state, &auth.guild_id).await?;

    Ok(Json(guild_settings_response(
        &state.guild_settings_defaults,
        stored,
        is_admin,
    )))
}

async fn api_update_guild_settings(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Json(request): Json<GuildSettingsUpdateRequest>,
) -> Result<Json<GuildSettingsResponse>, StatusCode> {
    validate_guild_settings_update(&request)?;
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    if !check_guild_admin_permission(&state, auth, &user_id).await? {
        return Err(StatusCode::FORBIDDEN);
    }

    let whisper_language_explicit = request.whisper_language.is_some();
    state
        .db
        .execute(
            UPSERT_GUILD_SETTINGS_SQL,
            &[
                &auth.guild_id,
                &request.whisper_language,
                &whisper_language_explicit,
                &request.whisper_vad,
                &request.auto_stop_grace_seconds,
                &request.retention_raw_audio_ttl_days,
                &request.retention_transcript_ttl_days,
                &request.summary_enabled,
            ],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(guild_settings_response(
        &state.guild_settings_defaults,
        Some(StoredGuildSettings {
            whisper_language: request.whisper_language,
            whisper_language_explicit,
            whisper_vad: Some(request.whisper_vad),
            auto_stop_grace_seconds: Some(request.auto_stop_grace_seconds),
            retention_raw_audio_ttl_days: Some(request.retention_raw_audio_ttl_days),
            retention_transcript_ttl_days: Some(request.retention_transcript_ttl_days),
            summary_enabled: Some(request.summary_enabled),
        }),
        true,
    )))
}

async fn api_meeting(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(meeting_id): Path<String>,
) -> Result<Json<MeetingResponse>, StatusCode> {
    verify_meeting_access(&state, &meeting_id, &user_id).await?;

    let row = state
        .db
        .query_opt(
            "SELECT id, title, status, \
             to_char(started_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') as started_at, \
             to_char(stopped_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') as stopped_at, \
             meeting_duration_seconds \
             FROM meetings WHERE id=$1",
            &[&meeting_id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(MeetingResponse {
        id: row.get("id"),
        title: row.get("title"),
        status: row.get("status"),
        started_at: row.get("started_at"),
        stopped_at: row.get("stopped_at"),
        duration_seconds: row.get("meeting_duration_seconds"),
    }))
}

fn transcript_source_for_api(raw: Option<String>) -> Result<String, ()> {
    let Some(value) = raw else {
        warn!("transcript row has NULL source");
        return Err(());
    };
    let Some(parsed) = TranscriptSource::parse_str(&value) else {
        warn!(source = %value, "unknown transcript source");
        return Err(());
    };
    Ok(parsed.as_str().to_owned())
}

async fn api_transcript(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(meeting_id): Path<String>,
) -> Result<Json<Vec<TranscriptSegmentResponse>>, StatusCode> {
    verify_meeting_access(&state, &meeting_id, &user_id).await?;

    let rows = state
        .db
        .query(
            "SELECT t.speaker_id, t.start_ms, t.end_ms, t.text, t.confidence, t.is_noisy, t.source, \
                    ms.username, ms.nickname, ms.display_name \
             FROM transcripts t \
             LEFT JOIN meeting_speakers ms \
               ON ms.meeting_id = t.meeting_id AND ms.speaker_id = t.speaker_id \
             WHERE t.meeting_id=$1 AND NOT t.is_deleted \
             ORDER BY t.start_ms, t.end_ms, t.speaker_id, t.id",
            &[&meeting_id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut segments = Vec::with_capacity(rows.len());
    for row in &rows {
        let speaker_id: String = row.get("speaker_id");
        let profile = SpeakerProfile {
            speaker_id: speaker_id.clone(),
            username: row.get::<_, Option<String>>("username"),
            nickname: row.get::<_, Option<String>>("nickname"),
            display_name: row.get::<_, Option<String>>("display_name"),
        };
        let source = transcript_source_for_api(row.get("source"))
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        segments.push(TranscriptSegmentResponse {
            speaker_id,
            speaker: SpeakerResponse {
                id: profile.speaker_id.clone(),
                username: profile.username.clone(),
                nickname: profile.nickname.clone(),
                display_name: profile.display_name.clone(),
                display_label: profile.display_label(),
            },
            start_ms: row.get("start_ms"),
            end_ms: row.get("end_ms"),
            text: row.get("text"),
            confidence: row.get("confidence"),
            is_noisy: row.get("is_noisy"),
            source,
        });
    }

    Ok(Json(segments))
}

async fn api_summary(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(meeting_id): Path<String>,
) -> Result<Json<SummaryResponse>, StatusCode> {
    verify_meeting_access(&state, &meeting_id, &user_id).await?;

    let row = state
        .db
        .query_opt(
            "SELECT markdown FROM summaries WHERE meeting_id=$1 ORDER BY version DESC LIMIT 1",
            &[&meeting_id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let markdown = row.and_then(|r| r.get::<_, Option<String>>("markdown"));
    Ok(Json(SummaryResponse { markdown }))
}

async fn api_audio(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(meeting_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    verify_meeting_access(&state, &meeting_id, &user_id).await?;

    let row = state
        .db
        .query_opt(
            "SELECT guild_id, voice_channel_id FROM meetings WHERE id=$1 LIMIT 1",
            &[&meeting_id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let guild_id: String = row.get("guild_id");
    let voice_channel_id: String = row.get("voice_channel_id");

    let layout =
        crate::infrastructure::workspace::MeetingWorkspaceLayout::new(&state.chunk_storage_dir);
    let workspace = layout.for_meeting(&guild_id, &voice_channel_id, &meeting_id);
    let primary = workspace.mixdown_path();
    let legacy = layout.legacy_meeting_dir(&meeting_id).join("mixdown.wav");
    let path = if tokio::fs::try_exists(&primary).await.unwrap_or(false) {
        primary
    } else {
        legacy
    };

    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let file_size = metadata.len();

    if let Some(range_header) = headers.get(header::RANGE) {
        let range_str = range_header.to_str().map_err(|_| StatusCode::BAD_REQUEST)?;
        match parse_range(range_str, file_size) {
            Some((start, end)) => {
                if let Err(resp) = check_audio_range_rate_limit(&state, &user_id).await {
                    return Ok(resp);
                }
                let length = end - start + 1;
                let content_range = format!("bytes {start}-{end}/{file_size}");

                let body = stream_file_range(&path, start, length).await.map_err(|e| {
                    if e == StatusCode::NOT_FOUND {
                        StatusCode::NOT_FOUND
                    } else {
                        StatusCode::INTERNAL_SERVER_ERROR
                    }
                })?;

                return Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header(header::CONTENT_TYPE, "audio/wav")
                    .header(header::ACCEPT_RANGES, "bytes")
                    .header(header::CONTENT_LENGTH, length.to_string())
                    .header(header::CONTENT_RANGE, content_range)
                    .body(body)
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR);
            }
            None => {
                // RFC 7233: 416 Range Not Satisfiable
                return Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{file_size}"))
                    .body(axum::body::Body::empty())
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR);
            }
        }
    }

    let file = tokio::fs::File::open(&path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    })?;
    let stream = tokio_util::io::ReaderStream::new(file);
    let body = axum::body::Body::from_stream(stream);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "audio/wav")
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, file_size.to_string())
        .body(body)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn api_speakers(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(meeting_id): Path<String>,
) -> Result<Json<Vec<SpeakerAudioResponse>>, StatusCode> {
    verify_meeting_access(&state, &meeting_id, &user_id).await?;

    let rows = state
        .db
        .query(
            "SELECT ms.speaker_id, ms.username, ms.nickname, ms.display_name, \
                    m.guild_id, m.voice_channel_id \
             FROM meeting_speakers ms \
             JOIN meetings m ON m.id = ms.meeting_id \
             WHERE ms.meeting_id=$1 \
             ORDER BY ms.speaker_id",
            &[&meeting_id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if rows.is_empty() {
        return Ok(Json(vec![]));
    }

    let guild_id: String = rows[0].get("guild_id");
    let voice_channel_id: String = rows[0].get("voice_channel_id");

    let layout =
        crate::infrastructure::workspace::MeetingWorkspaceLayout::new(&state.chunk_storage_dir);
    let workspace = layout.for_meeting(&guild_id, &voice_channel_id, &meeting_id);
    let primary_speakers_dir = workspace.speakers_dir();
    let legacy_speakers_dir = layout.legacy_meeting_dir(&meeting_id).join("speakers");

    let speaker_tasks: Vec<_> = rows
        .iter()
        .map(|row| {
            let speaker_id: String = row.get("speaker_id");
            let username: Option<String> = row.get("username");
            let nickname: Option<String> = row.get("nickname");
            let display_name: Option<String> = row.get("display_name");
            let profile = SpeakerProfile {
                speaker_id: speaker_id.clone(),
                username: username.clone(),
                nickname: nickname.clone(),
                display_name: display_name.clone(),
            };
            let safe_speaker = sanitize_path_component(&speaker_id);
            let filename = format!("{safe_speaker}_speaker.wav");
            let primary_path = primary_speakers_dir.join(&filename);
            let legacy_path = legacy_speakers_dir.join(&filename);
            async move {
                let (primary_exists, legacy_exists) = tokio::join!(
                    tokio::fs::try_exists(&primary_path),
                    tokio::fs::try_exists(&legacy_path),
                );
                let has_audio = primary_exists.unwrap_or(false) || legacy_exists.unwrap_or(false);
                SpeakerAudioResponse {
                    speaker_id,
                    username,
                    nickname,
                    display_name,
                    display_label: profile.display_label(),
                    has_audio,
                }
            }
        })
        .collect();
    let speakers = futures_util::future::join_all(speaker_tasks).await;

    Ok(Json(speakers))
}

async fn api_speaker_audio(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path((meeting_id, speaker_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    verify_meeting_access(&state, &meeting_id, &user_id).await?;

    let row = state
        .db
        .query_opt(
            "SELECT ms.username, ms.nickname, ms.display_name, \
                    m.guild_id, m.voice_channel_id \
             FROM meeting_speakers ms \
             JOIN meetings m ON m.id = ms.meeting_id \
             WHERE ms.meeting_id=$1 AND ms.speaker_id=$2 \
             LIMIT 1",
            &[&meeting_id, &speaker_id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let profile = SpeakerProfile {
        speaker_id: speaker_id.clone(),
        username: row.get("username"),
        nickname: row.get("nickname"),
        display_name: row.get("display_name"),
    };
    let display_label = profile.display_label();

    let guild_id: String = row.get("guild_id");
    let voice_channel_id: String = row.get("voice_channel_id");

    let layout =
        crate::infrastructure::workspace::MeetingWorkspaceLayout::new(&state.chunk_storage_dir);
    let workspace = layout.for_meeting(&guild_id, &voice_channel_id, &meeting_id);
    let safe_speaker = sanitize_path_component(&speaker_id);
    let filename = format!("{safe_speaker}_speaker.wav");
    let primary = workspace.speakers_dir().join(&filename);
    let legacy = layout
        .legacy_meeting_dir(&meeting_id)
        .join("speakers")
        .join(&filename);
    let path = if tokio::fs::try_exists(&primary).await.unwrap_or(false) {
        primary
    } else {
        legacy
    };

    let metadata = tokio::fs::metadata(&path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    })?;
    let file_size = metadata.len();
    let content_disposition = build_content_disposition(&display_label);

    if let Some(range_header) = headers.get(header::RANGE) {
        let range_str = range_header.to_str().map_err(|_| StatusCode::BAD_REQUEST)?;
        match parse_range(range_str, file_size) {
            Some((start, end)) => {
                if let Err(resp) = check_audio_range_rate_limit(&state, &user_id).await {
                    return Ok(resp);
                }
                let length = end - start + 1;
                let content_range = format!("bytes {start}-{end}/{file_size}");

                let body = stream_file_range(&path, start, length).await.map_err(|e| {
                    if e == StatusCode::NOT_FOUND {
                        StatusCode::NOT_FOUND
                    } else {
                        StatusCode::INTERNAL_SERVER_ERROR
                    }
                })?;

                return Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header(header::CONTENT_TYPE, "audio/wav")
                    .header(header::ACCEPT_RANGES, "bytes")
                    .header(header::CONTENT_LENGTH, length.to_string())
                    .header(header::CONTENT_RANGE, content_range)
                    .header(header::CONTENT_DISPOSITION, &content_disposition)
                    .body(body)
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR);
            }
            None => {
                return Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{file_size}"))
                    .header(header::CONTENT_DISPOSITION, &content_disposition)
                    .body(axum::body::Body::empty())
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR);
            }
        }
    }

    let file = tokio::fs::File::open(&path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    })?;
    let stream = tokio_util::io::ReaderStream::new(file);
    let body = axum::body::Body::from_stream(stream);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "audio/wav")
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, file_size.to_string())
        .header(header::CONTENT_DISPOSITION, &content_disposition)
        .body(body)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

// ========== Debug artifacts ==========

/// Static (non-parameterized) debug artifact identifier. Parameterized
/// identifiers (`whisper~<id>`, `speaker_audio~<id>`) are handled separately
/// in [`resolve_debug_artifact`].
#[derive(Debug, Clone, Copy)]
enum StaticDebugArtifactKind {
    MixdownAudio,
    WhisperMixdown,
    TranscriptPreCorrection,
    TranscriptPostCorrection,
    TranscriptManifest,
    CorrectionPrompt,
    SummaryPrompt,
    SummaryOutput,
}

impl StaticDebugArtifactKind {
    fn as_id(self) -> &'static str {
        match self {
            Self::MixdownAudio => "mixdown_audio",
            Self::WhisperMixdown => "whisper_mixdown",
            Self::TranscriptPreCorrection => "transcript_pre_correction",
            Self::TranscriptPostCorrection => "transcript_post_correction",
            Self::TranscriptManifest => "transcript_manifest",
            Self::CorrectionPrompt => "correction_prompt",
            Self::SummaryPrompt => "summary_prompt",
            Self::SummaryOutput => "summary_output",
        }
    }

    fn parse(input: &str) -> Option<Self> {
        Some(match input {
            "mixdown_audio" => Self::MixdownAudio,
            "whisper_mixdown" => Self::WhisperMixdown,
            "transcript_pre_correction" => Self::TranscriptPreCorrection,
            "transcript_post_correction" => Self::TranscriptPostCorrection,
            "transcript_manifest" => Self::TranscriptManifest,
            "correction_prompt" => Self::CorrectionPrompt,
            "summary_prompt" => Self::SummaryPrompt,
            "summary_output" => Self::SummaryOutput,
            _ => return None,
        })
    }
}

/// Resolved physical source for a debug artifact request.
enum DebugArtifactSource {
    /// File on disk relative to the meeting workspace.
    File {
        path: std::path::PathBuf,
        filename: String,
        content_type: &'static str,
    },
    /// Inline body served from the database (or another non-file source).
    Inline {
        bytes: Vec<u8>,
        filename: String,
        content_type: &'static str,
    },
}

async fn api_debug_manifest(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(meeting_id): Path<String>,
) -> Result<Json<Vec<DebugArtifactEntry>>, StatusCode> {
    let access = verify_meeting_access(&state, &meeting_id, &user_id).await?;
    let raw_debug_allowed = verify_raw_debug_artifact_access(&state, &access, &user_id)
        .await
        .unwrap_or(false);

    let layout =
        crate::infrastructure::workspace::MeetingWorkspaceLayout::new(&state.chunk_storage_dir);
    let workspace = layout.for_meeting(&access.guild_id, &access.voice_channel_id, &meeting_id);
    let legacy_dir = layout.legacy_meeting_dir(&meeting_id);

    let speaker_rows = state
        .db
        .query(
            "SELECT speaker_id, username, nickname, display_name \
             FROM meeting_speakers WHERE meeting_id=$1 ORDER BY speaker_id",
            &[&meeting_id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let base_url = format!("/api/meetings/{}/debug/files", percent_encode(&meeting_id));

    // Pre-compute all static paths so the existence checks can run concurrently.
    let mixdown_primary = workspace.mixdown_path();
    let mixdown_legacy = legacy_dir.join("mixdown.wav");
    let mixdown_whisper_path = workspace.mixdown_whisper_response_path();
    let pre_correction_path = workspace.pre_correction_transcript_path();
    let masked_path = workspace.masked_transcript_path();
    let manifest_path = workspace.transcript_manifest_path();
    let correction_prompt_path = workspace.correction_prompt_path();
    let summary_prompt_path = workspace.summary_prompt_path();

    let summary_query_params: [&(dyn tokio_postgres::types::ToSql + Sync); 1] = [&meeting_id];
    let summary_query = state.db.query_opt(
        "SELECT markdown FROM summaries WHERE meeting_id=$1 ORDER BY version DESC LIMIT 1",
        &summary_query_params,
    );

    let (
        mixdown_primary_exists,
        mixdown_legacy_exists,
        mixdown_whisper_exists,
        pre_correction_exists,
        masked_exists,
        manifest_exists,
        correction_prompt_exists,
        summary_prompt_exists,
        summary_row,
    ) = tokio::join!(
        tokio::fs::try_exists(&mixdown_primary),
        tokio::fs::try_exists(&mixdown_legacy),
        tokio::fs::try_exists(&mixdown_whisper_path),
        tokio::fs::try_exists(&pre_correction_path),
        tokio::fs::try_exists(&masked_path),
        tokio::fs::try_exists(&manifest_path),
        tokio::fs::try_exists(&correction_prompt_path),
        tokio::fs::try_exists(&summary_prompt_path),
        summary_query,
    );

    let mixdown_available =
        mixdown_primary_exists.unwrap_or(false) || mixdown_legacy_exists.unwrap_or(false);

    let primary_speakers_dir = workspace.speakers_dir();
    let legacy_speakers_dir = legacy_dir.join("speakers");
    let speaker_tasks: Vec<_> = speaker_rows
        .iter()
        .map(|row| {
            let speaker_id: String = row.get("speaker_id");
            let username: Option<String> = row.get("username");
            let nickname: Option<String> = row.get("nickname");
            let display_name: Option<String> = row.get("display_name");
            let profile = SpeakerProfile {
                speaker_id: speaker_id.clone(),
                username,
                nickname,
                display_name,
            };
            let label_base = profile.display_label();
            let safe_speaker = sanitize_path_component(&speaker_id);

            let speaker_filename = format!("{safe_speaker}_speaker.wav");
            let primary_speaker_path = primary_speakers_dir.join(&speaker_filename);
            let legacy_speaker_path = legacy_speakers_dir.join(&speaker_filename);
            let whisper_path = workspace.whisper_response_path_for_sanitized(&safe_speaker);
            async move {
                let (primary_exists, legacy_exists, whisper_exists) = tokio::join!(
                    tokio::fs::try_exists(&primary_speaker_path),
                    tokio::fs::try_exists(&legacy_speaker_path),
                    tokio::fs::try_exists(&whisper_path),
                );
                (
                    safe_speaker,
                    label_base,
                    speaker_filename,
                    primary_exists.unwrap_or(false) || legacy_exists.unwrap_or(false),
                    whisper_exists.unwrap_or(false),
                )
            }
        })
        .collect();
    let speaker_results = futures_util::future::join_all(speaker_tasks).await;

    let summary_row = summary_row.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let summary_available = summary_row
        .as_ref()
        .and_then(|r| r.get::<_, Option<String>>("markdown"))
        .is_some();

    let mut entries: Vec<DebugArtifactEntry> = Vec::new();

    entries.push(DebugArtifactEntry {
        id: StaticDebugArtifactKind::MixdownAudio.as_id().to_owned(),
        label: "Mixdown 音声".to_owned(),
        category: "audio",
        available: mixdown_available,
        download_url: format!(
            "{base_url}/{}",
            StaticDebugArtifactKind::MixdownAudio.as_id()
        ),
        filename: "mixdown.wav".to_owned(),
        content_type: "audio/wav",
    });

    for (safe_speaker, label_base, speaker_filename, speaker_available, whisper_available) in
        speaker_results
    {
        entries.push(DebugArtifactEntry {
            id: format!("speaker_audio~{safe_speaker}"),
            label: format!("Whisper送信音声 ({label_base})"),
            category: "audio",
            available: speaker_available,
            download_url: format!("{base_url}/speaker_audio~{safe_speaker}"),
            filename: speaker_filename,
            content_type: "audio/wav",
        });

        if raw_debug_allowed {
            entries.push(DebugArtifactEntry {
                id: format!("whisper~{safe_speaker}"),
                label: format!("Whisperレスポンス ({label_base})"),
                category: "whisper",
                available: whisper_available,
                download_url: format!("{base_url}/whisper~{safe_speaker}"),
                filename: format!("whisper_{safe_speaker}.json"),
                content_type: "application/json",
            });
        }
    }

    if raw_debug_allowed {
        entries.push(DebugArtifactEntry {
            id: StaticDebugArtifactKind::WhisperMixdown.as_id().to_owned(),
            label: "Whisperレスポンス (mixdown)".to_owned(),
            category: "whisper",
            available: mixdown_whisper_exists.unwrap_or(false),
            download_url: format!(
                "{base_url}/{}",
                StaticDebugArtifactKind::WhisperMixdown.as_id()
            ),
            filename: "whisper_mixdown.json".to_owned(),
            content_type: "application/json",
        });
    }

    entries.push(DebugArtifactEntry {
        id: StaticDebugArtifactKind::TranscriptPreCorrection
            .as_id()
            .to_owned(),
        label: "Transcript (補正前)".to_owned(),
        category: "transcript",
        available: pre_correction_exists.unwrap_or(false),
        download_url: format!(
            "{base_url}/{}",
            StaticDebugArtifactKind::TranscriptPreCorrection.as_id()
        ),
        filename: "transcript_pre_correction.md".to_owned(),
        content_type: "text/markdown",
    });

    entries.push(DebugArtifactEntry {
        id: StaticDebugArtifactKind::TranscriptPostCorrection
            .as_id()
            .to_owned(),
        label: "Transcript (補正後)".to_owned(),
        category: "transcript",
        available: masked_exists.unwrap_or(false),
        download_url: format!(
            "{base_url}/{}",
            StaticDebugArtifactKind::TranscriptPostCorrection.as_id()
        ),
        filename: "transcript_masked.md".to_owned(),
        content_type: "text/markdown",
    });

    entries.push(DebugArtifactEntry {
        id: StaticDebugArtifactKind::TranscriptManifest
            .as_id()
            .to_owned(),
        label: "Transcript manifest".to_owned(),
        category: "transcript",
        available: manifest_exists.unwrap_or(false),
        download_url: format!(
            "{base_url}/{}",
            StaticDebugArtifactKind::TranscriptManifest.as_id()
        ),
        filename: "manifest.json".to_owned(),
        content_type: "application/json",
    });

    entries.push(DebugArtifactEntry {
        id: StaticDebugArtifactKind::CorrectionPrompt.as_id().to_owned(),
        label: "Transcript補正プロンプト".to_owned(),
        category: "prompt",
        available: correction_prompt_exists.unwrap_or(false),
        download_url: format!(
            "{base_url}/{}",
            StaticDebugArtifactKind::CorrectionPrompt.as_id()
        ),
        filename: "correction_prompt.txt".to_owned(),
        content_type: "text/plain",
    });

    entries.push(DebugArtifactEntry {
        id: StaticDebugArtifactKind::SummaryPrompt.as_id().to_owned(),
        label: "要約プロンプト".to_owned(),
        category: "prompt",
        available: summary_prompt_exists.unwrap_or(false),
        download_url: format!(
            "{base_url}/{}",
            StaticDebugArtifactKind::SummaryPrompt.as_id()
        ),
        filename: "summary_prompt.txt".to_owned(),
        content_type: "text/plain",
    });

    entries.push(DebugArtifactEntry {
        id: StaticDebugArtifactKind::SummaryOutput.as_id().to_owned(),
        label: "要約モデル生出力".to_owned(),
        category: "summary",
        available: summary_available,
        download_url: format!(
            "{base_url}/{}",
            StaticDebugArtifactKind::SummaryOutput.as_id()
        ),
        filename: "summary.md".to_owned(),
        content_type: "text/markdown",
    });

    Ok(Json(entries))
}

async fn api_debug_file(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path((meeting_id, artifact_id)): Path<(String, String)>,
) -> Result<Response, StatusCode> {
    let access = verify_meeting_access(&state, &meeting_id, &user_id).await?;
    if debug_artifact_requires_admin(&artifact_id) {
        let allowed = verify_raw_debug_artifact_access(&state, &access, &user_id).await?;
        authorize_debug_artifact_download(allowed)?;
    }

    let source = resolve_debug_artifact(&state, &meeting_id, &access, &artifact_id).await?;
    match source {
        DebugArtifactSource::File {
            path,
            filename,
            content_type,
        } => stream_debug_file(&path, &filename, content_type).await,
        DebugArtifactSource::Inline {
            bytes,
            filename,
            content_type,
        } => Ok(build_inline_debug_response(bytes, &filename, content_type)),
    }
}

async fn verify_raw_debug_artifact_access(
    state: &WebState,
    access: &MeetingAccess,
    user_id: &str,
) -> Result<bool, StatusCode> {
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    if check_guild_admin_permission(state, auth, user_id).await? {
        return Ok(true);
    }
    check_channel_admin_permission(state, auth, &access.voice_channel_id, user_id).await
}

fn authorize_debug_artifact_download(raw_debug_allowed: bool) -> Result<(), StatusCode> {
    if !raw_debug_allowed {
        Err(StatusCode::FORBIDDEN)
    } else {
        Ok(())
    }
}

fn debug_artifact_requires_admin(artifact_id: &str) -> bool {
    if let Some((kind, _)) = artifact_id.split_once('~') {
        return kind == "whisper";
    }
    matches!(
        StaticDebugArtifactKind::parse(artifact_id),
        Some(StaticDebugArtifactKind::WhisperMixdown)
    )
}

async fn resolve_debug_artifact(
    state: &WebState,
    meeting_id: &str,
    access: &MeetingAccess,
    artifact_id: &str,
) -> Result<DebugArtifactSource, StatusCode> {
    let layout =
        crate::infrastructure::workspace::MeetingWorkspaceLayout::new(&state.chunk_storage_dir);
    let workspace = layout.for_meeting(&access.guild_id, &access.voice_channel_id, meeting_id);
    let legacy_dir = layout.legacy_meeting_dir(meeting_id);

    if let Some((kind, raw_value)) = artifact_id.split_once('~') {
        // Pass the raw value through the same sanitizer used at write time so
        // we cannot escape the workspace via crafted artifact_ids.
        let safe = sanitize_path_component(raw_value);
        match kind {
            "speaker_audio" => {
                let filename = format!("{safe}_speaker.wav");
                let primary = workspace.speakers_dir().join(&filename);
                let legacy = legacy_dir.join("speakers").join(&filename);
                let path = if tokio::fs::try_exists(&primary).await.unwrap_or(false) {
                    primary
                } else if tokio::fs::try_exists(&legacy).await.unwrap_or(false) {
                    legacy
                } else {
                    return Err(StatusCode::NOT_FOUND);
                };
                return Ok(DebugArtifactSource::File {
                    path,
                    filename,
                    content_type: "audio/wav",
                });
            }
            "whisper" => {
                // Unlike speaker_audio there is no legacy fallback, so we
                // skip the explicit existence check here and let
                // stream_debug_file's metadata() call surface 404s.
                let path = workspace.whisper_response_path_for_sanitized(&safe);
                return Ok(DebugArtifactSource::File {
                    path,
                    filename: format!("whisper_{safe}.json"),
                    content_type: "application/json",
                });
            }
            _ => return Err(StatusCode::NOT_FOUND),
        }
    }

    let kind = StaticDebugArtifactKind::parse(artifact_id).ok_or(StatusCode::NOT_FOUND)?;
    Ok(match kind {
        StaticDebugArtifactKind::MixdownAudio => {
            let primary = workspace.mixdown_path();
            let legacy = legacy_dir.join("mixdown.wav");
            let path = if tokio::fs::try_exists(&primary).await.unwrap_or(false) {
                primary
            } else if tokio::fs::try_exists(&legacy).await.unwrap_or(false) {
                legacy
            } else {
                return Err(StatusCode::NOT_FOUND);
            };
            DebugArtifactSource::File {
                path,
                filename: "mixdown.wav".to_owned(),
                content_type: "audio/wav",
            }
        }
        StaticDebugArtifactKind::WhisperMixdown => DebugArtifactSource::File {
            path: workspace.mixdown_whisper_response_path(),
            filename: "whisper_mixdown.json".to_owned(),
            content_type: "application/json",
        },
        StaticDebugArtifactKind::TranscriptPreCorrection => DebugArtifactSource::File {
            path: workspace.pre_correction_transcript_path(),
            filename: "transcript_pre_correction.md".to_owned(),
            content_type: "text/markdown",
        },
        StaticDebugArtifactKind::TranscriptPostCorrection => DebugArtifactSource::File {
            path: workspace.masked_transcript_path(),
            filename: "transcript_masked.md".to_owned(),
            content_type: "text/markdown",
        },
        StaticDebugArtifactKind::TranscriptManifest => DebugArtifactSource::File {
            path: workspace.transcript_manifest_path(),
            filename: "manifest.json".to_owned(),
            content_type: "application/json",
        },
        StaticDebugArtifactKind::CorrectionPrompt => DebugArtifactSource::File {
            path: workspace.correction_prompt_path(),
            filename: "correction_prompt.txt".to_owned(),
            content_type: "text/plain",
        },
        StaticDebugArtifactKind::SummaryPrompt => DebugArtifactSource::File {
            path: workspace.summary_prompt_path(),
            filename: "summary_prompt.txt".to_owned(),
            content_type: "text/plain",
        },
        StaticDebugArtifactKind::SummaryOutput => {
            let summary_row = state
                .db
                .query_opt(
                    "SELECT markdown FROM summaries WHERE meeting_id=$1 ORDER BY version DESC LIMIT 1",
                    &[&meeting_id],
                )
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            let markdown: String = summary_row
                .and_then(|r| r.get::<_, Option<String>>("markdown"))
                .ok_or(StatusCode::NOT_FOUND)?;
            DebugArtifactSource::Inline {
                bytes: markdown.into_bytes(),
                filename: "summary.md".to_owned(),
                content_type: "text/markdown",
            }
        }
    })
}

async fn stream_debug_file(
    path: &std::path::Path,
    filename: &str,
    content_type: &'static str,
) -> Result<Response, StatusCode> {
    // Open the file first, then read metadata from the same file handle so
    // Content-Length is tied to the same on-disk inode as the byte stream
    // (avoids TOCTOU between stat and open if the file is replaced or
    // truncated between syscalls).
    let file = tokio::fs::File::open(path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    })?;
    let metadata = file
        .metadata()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let stream = tokio_util::io::ReaderStream::new(file);
    let body = axum::body::Body::from_stream(stream);
    let content_disposition = build_debug_content_disposition(filename);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, metadata.len().to_string())
        .header(header::CONTENT_DISPOSITION, content_disposition)
        .body(body)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

fn build_inline_debug_response(
    bytes: Vec<u8>,
    filename: &str,
    content_type: &'static str,
) -> Response {
    let len = bytes.len();
    let content_disposition = build_debug_content_disposition(filename);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, len.to_string())
        .header(header::CONTENT_DISPOSITION, content_disposition)
        .body(axum::body::Body::from(bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Build a Content-Disposition header for debug artifacts.
///
/// Mirrors the dual `filename` / `filename*` (RFC 5987) pattern used by
/// [`build_content_disposition`] so the header stays RFC 6266-compliant
/// even if a future caller passes a non-ASCII filename. Today's call sites
/// produce only ASCII (server-built names sanitized via
/// [`sanitize_path_component`] or static literals), but the dual encoding
/// is forward-defensive.
fn build_debug_content_disposition(filename: &str) -> String {
    let safe: String = filename
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            _ => c,
        })
        .collect();
    let safe = if safe.trim().is_empty() {
        "debug_artifact".to_owned()
    } else {
        safe
    };
    let ascii_fallback: String = safe
        .chars()
        .map(|c| {
            if c.is_ascii() && !c.is_control() {
                c
            } else {
                '_'
            }
        })
        .collect();
    let ascii_fallback = ascii_fallback.trim().trim_matches('_');
    let fallback_name = if ascii_fallback.is_empty() {
        "debug_artifact"
    } else {
        ascii_fallback
    };
    let encoded: String = safe
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{:02X}", b),
        })
        .collect();
    format!(r#"attachment; filename="{fallback_name}"; filename*=UTF-8''{encoded}"#)
}

// ---------- Helpers ----------

fn build_content_disposition(display_label: &str) -> String {
    let safe_label: String = display_label
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            _ => c,
        })
        .collect();
    let ascii_fallback: String = safe_label
        .chars()
        .map(|c| {
            if c.is_ascii() && !c.is_control() {
                c
            } else {
                '_'
            }
        })
        .collect();
    let ascii_fallback = ascii_fallback.trim().trim_matches('_');
    let fallback_name = if ascii_fallback.is_empty() {
        "speaker"
    } else {
        ascii_fallback
    };
    let input_to_encode: &str = if safe_label.trim().trim_matches('_').is_empty() {
        fallback_name
    } else {
        &safe_label
    };
    let encoded_label: String = input_to_encode
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{:02X}", b),
        })
        .collect();
    format!(
        r#"attachment; filename="{fallback_name}_speaker.wav"; filename*=UTF-8''{encoded_label}_speaker.wav"#
    )
}

fn utf8_safe_byte_prefix(body: &str, max_bytes: usize) -> &str {
    if body.len() <= max_bytes {
        return body;
    }
    let mut end = max_bytes;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    &body[..end]
}

fn parse_range(range_str: &str, file_size: u64) -> Option<(u64, u64)> {
    if file_size == 0 {
        return None;
    }
    let trimmed = range_str.trim();
    let range_spec = trimmed.strip_prefix("bytes=")?;
    if range_spec.contains(',') {
        return None;
    }
    let mut parts = range_spec.splitn(2, '-');
    let start_str = parts.next()?.trim();
    let end_str = parts.next()?.trim();
    let is_suffix_probe = start_str.is_empty();
    let is_open_ended = !start_str.is_empty() && end_str.is_empty();

    let (start, end) = if is_suffix_probe {
        let suffix_len: u64 = end_str.parse().ok()?;
        if suffix_len == 0 {
            return None;
        }
        let start = file_size.saturating_sub(suffix_len);
        (start, file_size - 1)
    } else {
        let start: u64 = start_str.parse().ok()?;
        if start >= file_size {
            return None;
        }
        let end = if end_str.is_empty() {
            file_size - 1
        } else {
            end_str.parse::<u64>().ok()?.min(file_size - 1)
        };
        if start > end {
            return None;
        }
        (start, end)
    };

    let length = end.saturating_sub(start).saturating_add(1);
    if !is_suffix_probe
        && !is_open_ended
        && file_size > MIN_AUDIO_RANGE_BYTES
        && length < MIN_AUDIO_RANGE_BYTES
    {
        return None;
    }
    Some((start, end))
}

/// Stream a byte range from a file. Seeks to `start` and limits the reader
/// to `length` bytes, then wraps it in a `ReaderStream` so the response is
/// streamed without buffering the entire range in memory.
async fn stream_file_range(
    path: &std::path::Path,
    start: u64,
    length: u64,
) -> Result<axum::body::Body, StatusCode> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let mut file = tokio::fs::File::open(path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    })?;
    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let limited = file.take(length);
    let stream = tokio_util::io::ReaderStream::new(limited);
    Ok(axum::body::Body::from_stream(stream))
}

#[cfg(test)]
mod guild_api_tests {
    use super::{
        GuildMeetingsQuery, GuildSettingsDefaults, GuildSettingsUpdateRequest, StoredGuildSettings,
        guild_settings_response, normalize_guild_meetings_pagination,
        validate_guild_settings_update,
    };
    use axum::http::StatusCode;

    fn valid_settings_request() -> GuildSettingsUpdateRequest {
        GuildSettingsUpdateRequest {
            whisper_language: Some("en".to_owned()),
            whisper_vad: true,
            auto_stop_grace_seconds: 60,
            retention_raw_audio_ttl_days: 7,
            retention_transcript_ttl_days: 30,
            summary_enabled: true,
        }
    }

    fn default_settings() -> GuildSettingsDefaults {
        GuildSettingsDefaults {
            whisper_language: Some("ja".to_owned()),
            whisper_vad: false,
            auto_stop_grace_seconds: 120,
            retention_raw_audio_ttl_days: 14,
            retention_transcript_ttl_days: 60,
        }
    }

    #[test]
    fn guild_settings_validation_accepts_boundary_values() {
        let mut request = valid_settings_request();
        request.auto_stop_grace_seconds = 10;
        request.retention_raw_audio_ttl_days = 1;
        request.retention_transcript_ttl_days = 365;

        assert_eq!(validate_guild_settings_update(&request), Ok(()));

        request.auto_stop_grace_seconds = 3600;
        request.retention_raw_audio_ttl_days = 365;
        request.retention_transcript_ttl_days = 1;

        assert_eq!(validate_guild_settings_update(&request), Ok(()));
    }

    #[test]
    fn guild_settings_validation_rejects_invalid_language() {
        for language in ["EN", "eng", "e1", ""] {
            let mut request = valid_settings_request();
            request.whisper_language = Some(language.to_owned());

            assert_eq!(
                validate_guild_settings_update(&request),
                Err(StatusCode::BAD_REQUEST)
            );
        }
    }

    #[test]
    fn guild_settings_validation_rejects_out_of_range_values() {
        let mut request = valid_settings_request();
        request.auto_stop_grace_seconds = 9;
        assert_eq!(
            validate_guild_settings_update(&request),
            Err(StatusCode::BAD_REQUEST)
        );

        request = valid_settings_request();
        request.auto_stop_grace_seconds = 3601;
        assert_eq!(
            validate_guild_settings_update(&request),
            Err(StatusCode::BAD_REQUEST)
        );

        request = valid_settings_request();
        request.retention_raw_audio_ttl_days = 0;
        assert_eq!(
            validate_guild_settings_update(&request),
            Err(StatusCode::BAD_REQUEST)
        );

        request = valid_settings_request();
        request.retention_transcript_ttl_days = 366;
        assert_eq!(
            validate_guild_settings_update(&request),
            Err(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn guild_settings_response_falls_back_to_defaults() {
        let response = guild_settings_response(&default_settings(), None, false);

        assert_eq!(response.whisper_language.as_deref(), Some("ja"));
        assert!(!response.whisper_language_explicit);
        assert!(!response.whisper_vad);
        assert_eq!(response.auto_stop_grace_seconds, 120);
        assert_eq!(response.retention_raw_audio_ttl_days, 14);
        assert_eq!(response.retention_transcript_ttl_days, 60);
        assert!(response.summary_enabled);
        assert!(!response.is_admin);
    }

    #[test]
    fn guild_settings_response_honors_stored_values() {
        let response = guild_settings_response(
            &default_settings(),
            Some(StoredGuildSettings {
                whisper_language: Some("fr".to_owned()),
                whisper_language_explicit: true,
                whisper_vad: Some(true),
                auto_stop_grace_seconds: Some(300),
                retention_raw_audio_ttl_days: Some(21),
                retention_transcript_ttl_days: Some(90),
                summary_enabled: Some(false),
            }),
            true,
        );

        assert_eq!(response.whisper_language.as_deref(), Some("fr"));
        assert!(response.whisper_language_explicit);
        assert!(response.whisper_vad);
        assert_eq!(response.auto_stop_grace_seconds, 300);
        assert_eq!(response.retention_raw_audio_ttl_days, 21);
        assert_eq!(response.retention_transcript_ttl_days, 90);
        assert!(!response.summary_enabled);
        assert!(response.is_admin);
    }

    #[test]
    fn guild_meetings_pagination_is_bounded() {
        assert_eq!(
            normalize_guild_meetings_pagination(GuildMeetingsQuery {
                page: None,
                limit: None
            }),
            (1, 20)
        );
        assert_eq!(
            normalize_guild_meetings_pagination(GuildMeetingsQuery {
                page: Some(0),
                limit: Some(0)
            }),
            (1, 1)
        );
        assert_eq!(
            normalize_guild_meetings_pagination(GuildMeetingsQuery {
                page: Some(2),
                limit: Some(250)
            }),
            (2, 100)
        );
    }
}

#[cfg(test)]
mod discord_channel_full_tests {
    use super::{
        CachedChannelPermission, DiscordChannelFull, DiscordOverwrite, DiscordOverwriteType,
        DiscordRoleFull, PERMISSION_CACHE_TTL_SECS, PermissionCache, VIEW_CHANNEL,
        authorize_debug_artifact_download, build_content_disposition, compute_channel_permissions,
        debug_artifact_requires_admin, meeting_access_from_row, verify_meeting_access_after_row,
    };
    use axum::http::StatusCode;
    use std::collections::HashMap;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::Instant;

    #[test]
    fn channel_full_permission_overwrites_omitted() {
        let ch: DiscordChannelFull = serde_json::from_str("{}").unwrap();
        assert!(ch.permission_overwrites.is_empty());
    }

    #[test]
    fn channel_full_permission_overwrites_null() {
        let ch: DiscordChannelFull =
            serde_json::from_str(r#"{"permission_overwrites":null}"#).unwrap();
        assert!(ch.permission_overwrites.is_empty());
    }

    #[test]
    fn channel_full_permission_overwrites_populated() {
        let ch: DiscordChannelFull = serde_json::from_str(
            r#"{"permission_overwrites":[{"id":"1","type":0,"allow":"1024","deny":"0"}]}"#,
        )
        .unwrap();
        assert_eq!(ch.permission_overwrites.len(), 1);
        assert_eq!(ch.permission_overwrites[0].id, "1");
        assert_eq!(
            ch.permission_overwrites[0].type_,
            DiscordOverwriteType::Role
        );
        assert_eq!(ch.permission_overwrites[0].allow, 1024);
        assert_eq!(ch.permission_overwrites[0].deny, 0);
    }

    #[test]
    fn channel_full_permission_overwrites_string_types() {
        let ch: DiscordChannelFull = serde_json::from_str(
            r#"{"permission_overwrites":[
                {"id":"10","type":"role","allow":"1024","deny":"0"},
                {"id":"20","type":"member","allow":1,"deny":0}
            ]}"#,
        )
        .unwrap();
        assert_eq!(ch.permission_overwrites.len(), 2);
        assert_eq!(
            ch.permission_overwrites[0].type_,
            DiscordOverwriteType::Role
        );
        assert_eq!(
            ch.permission_overwrites[1].type_,
            DiscordOverwriteType::Member
        );
        assert_eq!(ch.permission_overwrites[0].allow, 1024);
        assert_eq!(ch.permission_overwrites[1].allow, 1);
    }

    #[test]
    fn overwrite_allow_deny_numeric_and_partial() {
        let ch: DiscordChannelFull = serde_json::from_str(
            r#"{"permission_overwrites":[
                {"id":"10","type":1,"allow":1024,"deny":0},
                {"id":"20","type":0,"allow":"1"},
                {"id":"30","type":0,"deny":"2"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(ch.permission_overwrites.len(), 3);
        assert_eq!(ch.permission_overwrites[0].allow, 1024);
        assert_eq!(ch.permission_overwrites[0].deny, 0);
        assert_eq!(ch.permission_overwrites[1].allow, 1);
        assert_eq!(ch.permission_overwrites[1].deny, 0);
        assert_eq!(ch.permission_overwrites[2].allow, 0);
        assert_eq!(ch.permission_overwrites[2].deny, 2);
    }

    #[test]
    fn overwrite_invalid_type_rejected() {
        for type_value in [r#""unknown""#, "2", "-1"] {
            let json = format!(
                r#"{{"permission_overwrites":[{{"id":"1","type":{},"allow":"0","deny":"0"}}]}}"#,
                type_value
            );
            let result = serde_json::from_str::<DiscordChannelFull>(&json);
            assert!(result.is_err(), "type {type_value} unexpectedly parsed");
            let err = result.err().unwrap();
            assert!(err.to_string().contains("invalid overwrite type"));
        }
    }

    #[test]
    fn overwrite_invalid_permission_bitsets_rejected() {
        for field in ["allow", "deny"] {
            for value in [r#""not-a-number""#, r#""-1""#, "-1"] {
                let json = format!(
                    r#"{{"permission_overwrites":[{{"id":"1","type":0,"{field}":{value}}}]}}"#
                );
                let result = serde_json::from_str::<DiscordChannelFull>(&json);
                assert!(
                    result.is_err(),
                    "{field}={value} unexpectedly parsed as permission bits"
                );
                let err = result.err().unwrap();
                assert!(err.to_string().contains("invalid permission bitset"));
            }
        }
    }

    #[test]
    fn role_invalid_permission_bitsets_rejected() {
        for value in [r#""not-a-number""#, r#""-1""#, "-1"] {
            let json = format!(
                r#"{{"owner_id":"owner","roles":[{{"id":"guild","permissions":{value}}}]}}"#
            );
            let result = serde_json::from_str::<super::DiscordGuildFull>(&json);
            assert!(
                result.is_err(),
                "role permissions={value} unexpectedly parsed"
            );
            let err = result.err().unwrap();
            assert!(err.to_string().contains("invalid permission bitset"));
        }
    }

    #[test]
    fn role_permission_bitsets_accept_strings_and_numbers() {
        for value in [r#""1024""#, "1024"] {
            let json = format!(
                r#"{{"owner_id":"owner","roles":[{{"id":"guild","permissions":{value}}}]}}"#
            );
            let guild: super::DiscordGuildFull =
                serde_json::from_str(&json).expect("valid permission bitset should parse");

            assert_eq!(guild.roles[0].permissions, 1024);
        }
    }

    #[test]
    fn compute_channel_permissions_applies_role_and_member_overwrites() {
        let guild_id = "guild";
        let user_id = "user";
        let member_roles = vec!["role-a".to_owned()];
        let guild_roles = vec![
            DiscordRoleFull {
                id: guild_id.to_owned(),
                permissions: 0,
            },
            DiscordRoleFull {
                id: "role-a".to_owned(),
                permissions: 0,
            },
        ];
        let overwrites = vec![
            DiscordOverwrite {
                id: guild_id.to_owned(),
                type_: DiscordOverwriteType::Role,
                allow: 0,
                deny: 0,
            },
            DiscordOverwrite {
                id: "role-a".to_owned(),
                type_: DiscordOverwriteType::Role,
                allow: VIEW_CHANNEL,
                deny: 0,
            },
            DiscordOverwrite {
                id: user_id.to_owned(),
                type_: DiscordOverwriteType::Member,
                allow: 0,
                deny: VIEW_CHANNEL,
            },
        ];

        let permissions = compute_channel_permissions(
            user_id,
            "other-owner",
            guild_id,
            &member_roles,
            &guild_roles,
            &overwrites,
        );

        assert_eq!(permissions & VIEW_CHANNEL, 0);
    }

    #[test]
    fn meeting_access_rejects_mismatched_guild_before_permission_checks() {
        let result =
            meeting_access_from_row("other-guild".to_owned(), "voice".to_owned(), "auth-guild");

        assert!(matches!(result, Err(StatusCode::NOT_FOUND)));
    }

    #[test]
    fn meeting_access_accepts_authenticated_guild_row() {
        let access =
            meeting_access_from_row("auth-guild".to_owned(), "voice".to_owned(), "auth-guild")
                .expect("matching guild should be allowed");

        assert_eq!(access.guild_id, "auth-guild");
        assert_eq!(access.voice_channel_id, "voice");
    }

    #[tokio::test]
    async fn meeting_access_rejects_mismatched_guild_before_allowed_cache() {
        let cache: PermissionCache = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        cache.write().await.insert(
            ("user".to_owned(), "voice".to_owned()),
            (
                CachedChannelPermission {
                    can_view: true,
                    is_admin: false,
                },
                Instant::now() + std::time::Duration::from_secs(PERMISSION_CACHE_TTL_SECS),
            ),
        );
        let permission_check_called = Arc::new(AtomicBool::new(false));
        let permission_check_called_in_future = Arc::clone(&permission_check_called);

        let result = verify_meeting_access_after_row(
            "other-guild".to_owned(),
            "voice".to_owned(),
            "auth-guild",
            "user",
            &cache,
            async move {
                permission_check_called_in_future.store(true, Ordering::SeqCst);
                Ok(CachedChannelPermission {
                    can_view: true,
                    is_admin: false,
                })
            },
        )
        .await;

        assert!(matches!(result, Err(StatusCode::NOT_FOUND)));
        assert!(!permission_check_called.load(Ordering::SeqCst));
    }

    #[test]
    fn raw_whisper_debug_artifacts_require_admin_access() {
        assert!(debug_artifact_requires_admin("whisper_mixdown"));
        assert!(debug_artifact_requires_admin("whisper~speaker-1"));
        assert!(!debug_artifact_requires_admin("mixdown_audio"));
        assert!(!debug_artifact_requires_admin("speaker_audio~speaker-1"));
        assert!(!debug_artifact_requires_admin("transcript_post_correction"));
    }

    #[test]
    fn normal_viewer_cannot_download_raw_whisper_debug_artifacts() {
        assert_eq!(
            authorize_debug_artifact_download(false),
            Err(StatusCode::FORBIDDEN)
        );
        assert_eq!(authorize_debug_artifact_download(true), Ok(()));
    }

    #[test]
    fn content_disposition_strips_control_chars_and_empty_fallback() {
        let label = "John\r\nDoe:/test";
        let cd = build_content_disposition(label);
        assert!(
            cd.contains(r#"filename="John__Doe__test_speaker.wav""#),
            "unexpected ascii fallback in: {cd}"
        );
        assert!(
            cd.contains("filename*=UTF-8''"),
            "missing RFC5987 encoded filename in: {cd}"
        );
        assert!(
            !cd.contains('\r') && !cd.contains('\n'),
            "control chars leaked into header"
        );
    }

    #[test]
    fn content_disposition_empty_fallback() {
        let cd = build_content_disposition("\t\n\r");
        assert!(
            cd.contains(r#"filename="speaker_speaker.wav""#),
            "fallback name missing in: {cd}"
        );
        assert!(
            !cd.contains('\r') && !cd.contains('\n'),
            "control chars leaked into header"
        );
    }
}

#[cfg(test)]
mod oauth_state_tests {
    use super::{
        AuthConfig, CallbackParams, OAUTH_NONCE_COOKIE_PATH, OAUTH_STATE_TTL_SECS,
        format_oauth_nonce_cookie, get_cookie, prepare_oauth_login, sign_oauth_state,
        verify_oauth_callback_preexchange, verify_oauth_state,
    };
    use axum::http::{HeaderMap, HeaderValue, StatusCode};

    const SECRET: &str = "test-session-secret";

    fn test_auth_config() -> AuthConfig {
        AuthConfig {
            client_id: "client".to_owned(),
            client_secret: "secret".to_owned(),
            session_secret: SECRET.to_owned(),
            redirect_uri: "http://localhost/auth/callback".to_owned(),
            guild_id: "guild".to_owned(),
            bot_token: "bot".to_owned(),
            secure_cookie: true,
        }
    }

    #[test]
    fn oauth_state_round_trip_with_matching_nonce() {
        let nonce = "browser-a-nonce";
        let state = sign_oauth_state("/meetings/1", nonce, SECRET);
        let redirect = verify_oauth_state(&state, nonce, SECRET).expect("valid state");
        assert_eq!(redirect, "/meetings/1");
    }

    #[test]
    fn oauth_state_rejects_missing_cookie_nonce() {
        let state = sign_oauth_state("/", "nonce-a", SECRET);
        assert!(verify_oauth_state(&state, "", SECRET).is_none());
    }

    #[test]
    fn oauth_state_rejects_mismatched_browser_nonce() {
        let state = sign_oauth_state("/", "nonce-a", SECRET);
        assert!(verify_oauth_state(&state, "nonce-b", SECRET).is_none());
    }

    #[test]
    fn oauth_state_rejects_replayed_state_from_other_browser() {
        let attacker_state = sign_oauth_state("/", "attacker-nonce", SECRET);
        assert!(
            verify_oauth_state(&attacker_state, "victim-nonce", SECRET).is_none(),
            "victim cookie must not unlock attacker state"
        );
    }

    #[test]
    fn oauth_state_rejects_legacy_state_without_nonce_binding() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let json = serde_json::json!({"r": "/", "t": now}).to_string();
        let payload_hex = super::to_hex(json.as_bytes());
        let sig_hex = super::hmac_hex(SECRET, &payload_hex);
        let legacy_state = format!("{payload_hex}.{sig_hex}");
        assert!(
            verify_oauth_state(&legacy_state, "any-nonce", SECRET).is_none(),
            "pre-binding states must be rejected"
        );
    }

    #[test]
    fn oauth_nonce_cookie_parser_reads_callback_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            HeaderValue::from_static("dt_oauth_nonce=abc123; dt_session=ignored"),
        );
        assert_eq!(
            get_cookie(&headers, "dt_oauth_nonce").as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn prepare_oauth_login_sets_callback_scoped_nonce_cookie() {
        let auth = test_auth_config();
        let (_state, cookie, url) = prepare_oauth_login("/meetings/1", &auth);
        assert!(url.contains("discord.com/api/oauth2/authorize"));
        assert!(cookie.contains("dt_oauth_nonce="));
        assert!(cookie.contains(&format!("Path={OAUTH_NONCE_COOKIE_PATH}")));
        assert!(cookie.contains(&format!("Max-Age={OAUTH_STATE_TTL_SECS}")));
        assert!(cookie.contains("; Secure"));
    }

    #[test]
    fn verify_oauth_callback_preexchange_happy_path() {
        let nonce = "browser-nonce";
        let state = sign_oauth_state("/", nonce, SECRET);
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            HeaderValue::from_str(&format!("dt_oauth_nonce={nonce}")).expect("cookie value"),
        );
        let params = CallbackParams {
            code: Some("discord-code".to_owned()),
            state: Some(state),
        };
        assert_eq!(
            verify_oauth_callback_preexchange(&params, &headers, SECRET).unwrap(),
            "/"
        );
    }

    #[test]
    fn verify_oauth_callback_preexchange_rejects_missing_cookie() {
        let state = sign_oauth_state("/", "nonce", SECRET);
        let params = CallbackParams {
            code: Some("code".to_owned()),
            state: Some(state),
        };
        let err =
            verify_oauth_callback_preexchange(&params, &HeaderMap::new(), SECRET).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert_eq!(err.1, "missing oauth nonce");
    }

    #[test]
    fn verify_oauth_callback_preexchange_rejects_cross_browser_replay() {
        let state = sign_oauth_state("/", "attacker", SECRET);
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            HeaderValue::from_static("dt_oauth_nonce=victim"),
        );
        let params = CallbackParams {
            code: Some("code".to_owned()),
            state: Some(state),
        };
        let err = verify_oauth_callback_preexchange(&params, &headers, SECRET).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert_eq!(err.1, "invalid state");
    }

    #[test]
    fn oauth_callback_failure_clears_nonce_cookie() {
        let cleared = format_oauth_nonce_cookie("", true, 0);
        assert!(cleared.contains("dt_oauth_nonce=;"));
        assert!(cleared.contains(&format!("Path={OAUTH_NONCE_COOKIE_PATH}")));
        assert!(cleared.contains("Max-Age=0"));
    }
}

#[cfg(test)]
mod session_reverify_tests {
    use super::{
        SESSION_MEMBERSHIP_VERIFY_INTERVAL_SECS, SESSION_TTL_SECS, SessionPayload,
        guild_member_status_indicates_membership, session_needs_membership_reverify, sign_session,
        verify_session,
    };
    use reqwest::StatusCode as HttpStatus;

    const SECRET: &str = "test-session-secret";

    #[test]
    fn session_needs_reverify_after_interval() {
        let now = super::unix_now_secs();
        let session = SessionPayload {
            uid: "u".to_owned(),
            gid: "g".to_owned(),
            exp: now + super::SESSION_TTL_SECS,
            verified_at: now.saturating_sub(SESSION_MEMBERSHIP_VERIFY_INTERVAL_SECS + 60),
            reverify_attempt_at: 0,
            issued_at: now,
        };
        assert!(session_needs_membership_reverify(&session));
    }

    #[test]
    fn session_skips_reverify_within_interval() {
        let now = super::unix_now_secs();
        let session = SessionPayload {
            uid: "u".to_owned(),
            gid: "g".to_owned(),
            exp: now + super::SESSION_TTL_SECS,
            verified_at: now.saturating_sub(SESSION_MEMBERSHIP_VERIFY_INTERVAL_SECS / 2),
            reverify_attempt_at: 0,
            issued_at: now,
        };
        assert!(!session_needs_membership_reverify(&session));
    }

    #[test]
    fn session_skips_reverify_until_attempt_backoff_elapses() {
        let now = super::unix_now_secs();
        let session = SessionPayload {
            uid: "u".to_owned(),
            gid: "g".to_owned(),
            exp: now + super::SESSION_TTL_SECS,
            verified_at: now.saturating_sub(SESSION_MEMBERSHIP_VERIFY_INTERVAL_SECS + 60),
            reverify_attempt_at: now.saturating_sub(60),
            issued_at: now,
        };
        assert!(!session_needs_membership_reverify(&session));
    }

    #[test]
    fn legacy_session_without_verified_at_uses_issue_time_estimate() {
        let now = super::unix_now_secs();
        let cookie = sign_session("user-1", "guild-1", SECRET, now, 0);
        let session = verify_session(&cookie, SECRET).expect("session should verify");
        assert_eq!(
            session.verified_at,
            session.exp.saturating_sub(SESSION_TTL_SECS)
        );
        assert!(!session_needs_membership_reverify(&session));
    }

    #[test]
    fn non_success_member_status_is_not_positive_membership() {
        assert!(!guild_member_status_indicates_membership(
            HttpStatus::NOT_FOUND
        ));
        assert!(!guild_member_status_indicates_membership(
            HttpStatus::FORBIDDEN
        ));
    }

    #[test]
    fn active_member_status_is_membership() {
        assert!(guild_member_status_indicates_membership(HttpStatus::OK));
    }

    #[test]
    fn refreshed_session_carries_new_verified_at() {
        let now = super::unix_now_secs();
        let cookie = sign_session("user-1", "guild-1", SECRET, now, now);
        let session = verify_session(&cookie, SECRET).expect("session should verify");
        assert_eq!(session.uid, "user-1");
        assert_eq!(session.verified_at, now);
    }

    #[test]
    fn legacy_session_backfills_issued_at_from_exp() {
        let now = super::unix_now_secs();
        let cookie = super::sign_session_with_exp(
            "user-1",
            "guild-1",
            SECRET,
            now + SESSION_TTL_SECS,
            0,
            now,
            0,
        );
        let session = verify_session(&cookie, SECRET).expect("session should verify");
        assert_eq!(
            session.issued_at,
            session.exp.saturating_sub(SESSION_TTL_SECS)
        );
    }

    #[test]
    fn stale_verified_at_session_still_parses_for_reverify_gate() {
        let now = super::unix_now_secs();
        let stale = now.saturating_sub(SESSION_MEMBERSHIP_VERIFY_INTERVAL_SECS + 120);
        let cookie = sign_session("user-1", "guild-1", SECRET, stale, stale);
        // issued_at preserved separately from verified_at in production cookies
        let session = verify_session(&cookie, SECRET).expect("session should verify");
        assert!(session_needs_membership_reverify(&session));
    }
}

#[cfg(test)]
mod transcript_source_api_tests {
    use super::transcript_source_for_api;
    use crate::domain::transcript::TranscriptSource;

    #[test]
    fn api_transcript_source_accepts_known_values() {
        assert_eq!(
            transcript_source_for_api(Some("voice".to_owned())).unwrap(),
            TranscriptSource::Voice.as_str()
        );
        assert_eq!(
            transcript_source_for_api(Some("vc_text".to_owned())).unwrap(),
            TranscriptSource::VcText.as_str()
        );
    }

    #[test]
    fn api_transcript_source_rejects_unknown_and_null() {
        assert!(transcript_source_for_api(Some("unknown".to_owned())).is_err());
        assert!(transcript_source_for_api(None).is_err());
    }
}

#[cfg(test)]
mod parse_range_tests {
    use super::parse_range;

    const LARGE_FILE: u64 = 256 * 1024;

    #[test]
    fn parse_range_rejects_multipart_specs() {
        assert!(parse_range("bytes=0-99,200-299", LARGE_FILE).is_none());
    }

    #[test]
    fn parse_range_rejects_short_ranges_on_large_files() {
        assert!(parse_range("bytes=0-1", LARGE_FILE).is_none());
    }

    #[test]
    fn parse_range_allows_suffix_tail_probe() {
        assert_eq!(
            parse_range("bytes=-4096", LARGE_FILE),
            Some((258_048, 262_143))
        );
    }

    #[test]
    fn parse_range_rejects_garbage_after_spec() {
        assert!(parse_range("bytes=0-10 garbage", LARGE_FILE).is_none());
    }

    #[test]
    fn parse_range_accepts_open_ended_range() {
        let end = LARGE_FILE - 1;
        assert_eq!(parse_range("bytes=0-", LARGE_FILE), Some((0, end)));
    }

    #[test]
    fn parse_range_accepts_open_ended_tail_seek_under_min_chunk() {
        let start = LARGE_FILE - (32 * 1024);
        assert_eq!(
            parse_range(&format!("bytes={start}-"), LARGE_FILE),
            Some((start, LARGE_FILE - 1))
        );
    }
}

#[cfg(test)]
mod discord_log_safety_tests {
    use super::utf8_safe_byte_prefix;

    #[test]
    fn utf8_safe_byte_prefix_avoids_mid_codepoint_panic() {
        let body = format!("{}語", "a".repeat(498));
        assert_eq!(body.len(), 501);
        let prefix = utf8_safe_byte_prefix(&body, 500);
        assert_eq!(prefix.len(), 498);
        assert!(prefix.is_char_boundary(prefix.len()));
        assert!(std::str::from_utf8(prefix.as_bytes()).is_ok());
        assert!(prefix.ends_with('a'));
    }
}
