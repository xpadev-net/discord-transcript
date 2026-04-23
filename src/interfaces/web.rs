use crate::domain::speaker::SpeakerProfile;
use crate::domain::transcript::TranscriptSource;
use crate::infrastructure::storage_fs::sanitize_path_component;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio_postgres::Client as PgClient;
use tower_http::services::{ServeDir, ServeFile};
use tracing::warn;

type HmacSha256 = Hmac<Sha256>;
const SESSION_COOKIE_NAME: &str = "dt_session";
const SESSION_TTL_SECS: u64 = 7 * 24 * 3600; // 7 days
const VIEW_CHANNEL: u64 = 1 << 10;
const ADMINISTRATOR: u64 = 1 << 3;

// ---------- State ----------

const PERMISSION_CACHE_TTL_SECS: u64 = 300;
const GUILD_CACHE_TTL_SECS: u64 = 300;

type PermissionCache = Arc<tokio::sync::RwLock<HashMap<(String, String), (bool, Instant)>>>;
type GuildCache = Arc<tokio::sync::RwLock<Option<(DiscordGuildFull, Instant)>>>;

#[derive(Clone)]
pub struct WebState {
    pub db: Arc<PgClient>,
    pub chunk_storage_dir: String,
    pub auth: Option<Arc<AuthConfig>>,
    pub http_client: reqwest::Client,
    /// Cache: (user_id, channel_id) → (allowed, expires_at)
    pub permission_cache: PermissionCache,
    /// Cache: guild info (shared across all requests)
    guild_cache: GuildCache,
    pub static_files_dir: String,
}

impl WebState {
    pub fn new(
        db: Arc<PgClient>,
        chunk_storage_dir: String,
        auth: Option<Arc<AuthConfig>>,
        http_client: reqwest::Client,
        static_files_dir: String,
    ) -> Self {
        Self {
            db,
            chunk_storage_dir,
            auth,
            http_client,
            permission_cache: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            guild_cache: Arc::new(tokio::sync::RwLock::new(None)),
            static_files_dir,
        }
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
        .route("/auth/logout", get(auth_logout));

    let protected = Router::new()
        .route("/api/meetings/{meeting_id}", get(api_meeting))
        .route("/api/meetings/{meeting_id}/transcript", get(api_transcript))
        .route("/api/meetings/{meeting_id}/summary", get(api_summary))
        .route("/api/meetings/{meeting_id}/audio", get(api_audio))
        .route("/api/meetings/{meeting_id}/speakers", get(api_speakers))
        .route(
            "/api/meetings/{meeting_id}/speakers/{speaker_id}/audio",
            get(api_speaker_audio),
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

    if let Some(cookie_val) = get_cookie(&headers, SESSION_COOKIE_NAME)
        && let Some(session) = verify_session(&cookie_val, &auth.session_secret)
        && session.gid == auth.guild_id
    {
        request.extensions_mut().insert(AuthUserId(session.uid));
        return next.run(request).await;
    }

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
    let state_param = sign_oauth_state(&redirect, &auth.session_secret);

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

    Redirect::temporary(&url).into_response()
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
    Query(params): Query<CallbackParams>,
) -> Response {
    let Some(ref auth) = state.auth else {
        return (StatusCode::SERVICE_UNAVAILABLE, "OAuth not configured").into_response();
    };

    let Some(code) = params.code else {
        return (StatusCode::BAD_REQUEST, "missing code").into_response();
    };
    let Some(ref state_param) = params.state else {
        return (StatusCode::BAD_REQUEST, "missing state").into_response();
    };

    let Some(redirect) = verify_oauth_state(state_param, &auth.session_secret) else {
        return (StatusCode::BAD_REQUEST, "invalid state").into_response();
    };

    // Exchange code for access token
    let token: TokenResponse = match state
        .http_client
        .post("https://discord.com/api/oauth2/token")
        .form(&[
            ("client_id", auth.client_id.as_str()),
            ("client_secret", auth.client_secret.as_str()),
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", auth.redirect_uri.as_str()),
        ])
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.json().await {
            Ok(t) => t,
            Err(_) => return (StatusCode::BAD_GATEWAY, "invalid token response").into_response(),
        },
        _ => return (StatusCode::BAD_GATEWAY, "token exchange failed").into_response(),
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
            Err(_) => return (StatusCode::BAD_GATEWAY, "invalid user response").into_response(),
        },
        _ => return (StatusCode::BAD_GATEWAY, "failed to fetch user").into_response(),
    };

    let guilds: Vec<DiscordGuild> = match guilds_res {
        Ok(resp) if resp.status().is_success() => match resp.json().await {
            Ok(g) => g,
            Err(_) => return (StatusCode::BAD_GATEWAY, "invalid guilds response").into_response(),
        },
        _ => return (StatusCode::BAD_GATEWAY, "failed to fetch guilds").into_response(),
    };

    if !guilds.iter().any(|g| g.id == auth.guild_id) {
        return (StatusCode::FORBIDDEN, "not a member of this server").into_response();
    }

    // Create session cookie with user ID
    let redirect = sanitize_redirect(&redirect);
    let session_value = sign_session(&user.id, &auth.guild_id, &auth.session_secret);
    let secure_flag = if auth.secure_cookie { "; Secure" } else { "" };
    let cookie = format!(
        "{SESSION_COOKIE_NAME}={session_value}; HttpOnly; SameSite=Lax; Path=/; Max-Age={SESSION_TTL_SECS}{secure_flag}",
    );

    Response::builder()
        .status(StatusCode::TEMPORARY_REDIRECT)
        .header(header::LOCATION, &redirect)
        .header(header::SET_COOKIE, cookie)
        .body(axum::body::Body::empty())
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn auth_logout(State(state): State<WebState>) -> Response {
    let secure_flag = if state.auth.as_ref().is_some_and(|a| a.secure_cookie) {
        "; Secure"
    } else {
        ""
    };
    let cookie =
        format!("{SESSION_COOKIE_NAME}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0{secure_flag}",);
    Response::builder()
        .status(StatusCode::TEMPORARY_REDIRECT)
        .header(header::LOCATION, "/")
        .header(header::SET_COOKIE, cookie)
        .body(axum::body::Body::empty())
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

// ========== Auth: session helpers ==========

#[derive(Serialize, Deserialize)]
struct SessionPayload {
    uid: String,
    gid: String,
    exp: u64,
}

fn sign_session(user_id: &str, guild_id: &str, secret: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let payload = SessionPayload {
        uid: user_id.to_owned(),
        gid: guild_id.to_owned(),
        exp: now + SESSION_TTL_SECS,
    };
    let json = serde_json::to_string(&payload).unwrap_or_default();
    let payload_hex = to_hex(json.as_bytes());
    let sig_hex = hmac_hex(secret, &payload_hex);
    format!("{payload_hex}.{sig_hex}")
}

fn verify_session(cookie: &str, secret: &str) -> Option<SessionPayload> {
    let (payload_hex, sig_hex) = cookie.rsplit_once('.')?;
    let expected = hmac_hex(secret, payload_hex);
    if !constant_time_eq(sig_hex.as_bytes(), expected.as_bytes()) {
        return None;
    }
    let payload_bytes = from_hex(payload_hex)?;
    let payload: SessionPayload = serde_json::from_slice(&payload_bytes).ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if now > payload.exp {
        return None;
    }
    Some(payload)
}

const OAUTH_STATE_TTL_SECS: u64 = 600; // 10 minutes

fn sign_oauth_state(redirect: &str, secret: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let json = serde_json::json!({"r": redirect, "t": now}).to_string();
    let payload_hex = to_hex(json.as_bytes());
    let sig_hex = hmac_hex(secret, &payload_hex);
    format!("{payload_hex}.{sig_hex}")
}

fn verify_oauth_state(state: &str, secret: &str) -> Option<String> {
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
    if now.saturating_sub(created) > OAUTH_STATE_TTL_SECS {
        return None;
    }
    value.get("r")?.as_str().map(|s| s.to_owned())
}

// ========== Channel permission check ==========

/// Verify that the authenticated user has VIEW_CHANNEL permission on the
/// voice channel where the meeting was recorded.
/// Results are cached per (user_id, channel_id) for 5 minutes to avoid
/// Discord API rate-limit exhaustion on page loads (which trigger ~4 requests).
async fn verify_meeting_access(
    state: &WebState,
    meeting_id: &str,
    user_id: &str,
) -> Result<(), StatusCode> {
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;

    // Look up the meeting's voice channel
    let row = state
        .db
        .query_opt(
            "SELECT voice_channel_id FROM meetings WHERE id=$1",
            &[&meeting_id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let channel_id: String = row.get("voice_channel_id");

    // Check permission cache
    let cache_key = (user_id.to_owned(), channel_id.clone());
    {
        let cache = state.permission_cache.read().await;
        if let Some(&(allowed, expires_at)) = cache.get(&cache_key)
            && Instant::now() < expires_at
        {
            return if allowed {
                Ok(())
            } else {
                Err(StatusCode::FORBIDDEN)
            };
        }
    }

    // Cache miss — query Discord API
    let allowed = check_channel_permission(state, auth, &channel_id, user_id).await?;

    // Store result in cache (also evict expired entries periodically)
    {
        let mut cache = state.permission_cache.write().await;
        let expires_at = Instant::now() + std::time::Duration::from_secs(PERMISSION_CACHE_TTL_SECS);
        cache.insert(cache_key, (allowed, expires_at));

        // Evict expired entries if cache grows large
        if cache.len() > 5000 {
            let now = Instant::now();
            cache.retain(|_, (_, exp)| *exp > now);
        }
    }

    if allowed {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
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

    let guild_body = guild_resp.text().await.map_err(|err| {
        warn!(error = %err, "discord guild API response read failed");
        StatusCode::BAD_GATEWAY
    })?;
    let guild: DiscordGuildFull = serde_json::from_str(&guild_body).map_err(|err| {
        warn!(
            error = %err,
            body_preview = %&guild_body[..guild_body.len().min(500)],
            "discord guild API response parse failed"
        );
        StatusCode::BAD_GATEWAY
    })?;

    let expires_at = Instant::now() + std::time::Duration::from_secs(GUILD_CACHE_TTL_SECS);
    *cache = Some((guild.clone(), expires_at));

    Ok(guild)
}

/// Query Discord API for channel permission. Returns Ok(true) if allowed,
/// Ok(false) if denied, Err on API failure.
async fn check_channel_permission(
    state: &WebState,
    auth: &AuthConfig,
    channel_id: &str,
    user_id: &str,
) -> Result<bool, StatusCode> {
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
        return Ok(false);
    }
    if channel_status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        warn!(
            status = %channel_status,
            retry_after = retry_after_header.as_deref(),
            body_preview = %&channel_body[..channel_body.len().min(500)],
            "discord channel API rate limited"
        );
        return Err(StatusCode::BAD_GATEWAY);
    }
    if !channel_status.is_success() {
        warn!(
            status = %channel_status,
            body_preview = %&channel_body[..channel_body.len().min(500)],
            "discord channel API non-success"
        );
        return Err(StatusCode::BAD_GATEWAY);
    }

    let channel: DiscordChannelFull = serde_json::from_str(&channel_body).map_err(|err| {
        warn!(
            error = %err,
            body_preview = %&channel_body[..channel_body.len().min(500)],
            "discord channel API response parse failed"
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
        return Ok(false);
    }
    if !member_resp.status().is_success() {
        warn!(status = %member_resp.status(), "discord member API non-success");
        return Err(StatusCode::BAD_GATEWAY);
    }
    let member: DiscordMemberFull = member_resp.json().await.map_err(|err| {
        warn!(error = %err, "discord member API response parse failed");
        StatusCode::BAD_GATEWAY
    })?;

    let perms = compute_channel_permissions(
        user_id,
        &guild.owner_id,
        &auth.guild_id,
        &member.roles,
        &guild.roles,
        &channel.permission_overwrites,
    );

    Ok(perms & VIEW_CHANNEL != 0 || perms & ADMINISTRATOR != 0)
}

// Discord API response types for permission checking

fn zero_perm_string() -> String {
    "0".to_string()
}

/// Discord API returns permission values as either strings or integers
/// depending on the API version and context. Accept both.
fn deserialize_string_or_number<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct StringOrNumber;
    impl<'de> de::Visitor<'de> for StringOrNumber {
        type Value = String;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a string or number")
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<String, E> {
            Ok(v.to_owned())
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<String, E> {
            Ok(v.to_string())
        }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<String, E> {
            Ok(v.to_string())
        }
    }
    deserializer.deserialize_any(StringOrNumber)
}

#[derive(Deserialize, Clone)]
struct DiscordGuildFull {
    owner_id: String,
    roles: Vec<DiscordRoleFull>,
}

#[derive(Deserialize, Clone)]
struct DiscordRoleFull {
    id: String,
    #[serde(deserialize_with = "deserialize_string_or_number")]
    permissions: String,
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
        default = "zero_perm_string",
        deserialize_with = "deserialize_string_or_number"
    )]
    allow: String,
    #[serde(
        default = "zero_perm_string",
        deserialize_with = "deserialize_string_or_number"
    )]
    deny: String,
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
        .and_then(|r| r.permissions.parse().ok())
        .unwrap_or(0);

    // Add permissions from member's roles
    for role in guild_roles {
        if member_roles.contains(&role.id) {
            permissions |= role.permissions.parse::<u64>().unwrap_or(0);
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
        let allow = ow.allow.parse::<u64>().unwrap_or(0);
        let deny = ow.deny.parse::<u64>().unwrap_or(0);
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
        role_allow |= ow.allow.parse::<u64>().unwrap_or(0);
        role_deny |= ow.deny.parse::<u64>().unwrap_or(0);
    }
    permissions &= !role_deny;
    permissions |= role_allow;

    // Apply member-specific overwrite
    if let Some(ow) = overwrites
        .iter()
        .find(|o| matches!(o.type_, DiscordOverwriteType::Member) && o.id == user_id)
    {
        let allow = ow.allow.parse::<u64>().unwrap_or(0);
        let deny = ow.deny.parse::<u64>().unwrap_or(0);
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

// ---------- Handlers ----------

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

    let segments: Vec<TranscriptSegmentResponse> = rows
        .iter()
        .map(|row| {
            let speaker_id: String = row.get("speaker_id");
            let profile = SpeakerProfile {
                speaker_id: speaker_id.clone(),
                username: row.get::<_, Option<String>>("username"),
                nickname: row.get::<_, Option<String>>("nickname"),
                display_name: row.get::<_, Option<String>>("display_name"),
            };

            TranscriptSegmentResponse {
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
                source: row
                    .get::<_, Option<String>>("source")
                    .and_then(|s| TranscriptSource::parse_str(&s).map(|v| v.as_str().to_owned()))
                    .unwrap_or_else(|| TranscriptSource::Voice.as_str().to_owned()),
            }
        })
        .collect();

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

    let markdown = row.map(|r| r.get::<_, String>("markdown"));
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
                let length = end - start + 1;
                let content_range = format!("bytes {start}-{end}/{file_size}");

                let body = stream_file_range(&path, start, length).await?;

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

    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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
            "SELECT speaker_id, username, nickname, display_name \
             FROM meeting_speakers \
             WHERE meeting_id=$1 \
             ORDER BY speaker_id",
            &[&meeting_id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if rows.is_empty() {
        return Ok(Json(vec![]));
    }

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
    let primary_speakers_dir = workspace.speakers_dir();
    let legacy_speakers_dir = layout.legacy_meeting_dir(&meeting_id).join("speakers");

    let mut speakers = Vec::with_capacity(rows.len());
    for row in &rows {
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

        let has_audio = tokio::fs::try_exists(&primary_path).await.unwrap_or(false)
            || tokio::fs::try_exists(&legacy_path).await.unwrap_or(false);

        speakers.push(SpeakerAudioResponse {
            speaker_id: speaker_id.clone(),
            username: username.clone(),
            nickname: nickname.clone(),
            display_name: display_name.clone(),
            display_label: profile.display_label(),
            has_audio,
        });
    }

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
    let safe_speaker = sanitize_path_component(&speaker_id);
    let filename = format!("{safe_speaker}_speaker.wav");
    let primary = workspace.speakers_dir().join(&filename);
    let legacy = layout.legacy_meeting_dir(&meeting_id).join("speakers").join(&filename);
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
                let length = end - start + 1;
                let content_range = format!("bytes {start}-{end}/{file_size}");

                let body = stream_file_range(&path, start, length).await?;

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
                return Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{file_size}"))
                    .body(axum::body::Body::empty())
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR);
            }
        }
    }

    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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

// ---------- Helpers ----------

fn parse_range(range_str: &str, file_size: u64) -> Option<(u64, u64)> {
    if file_size == 0 {
        return None;
    }
    let range_str = range_str.strip_prefix("bytes=")?;
    let mut parts = range_str.splitn(2, '-');
    let start_str = parts.next()?.trim();
    let end_str = parts.next()?.trim();

    if start_str.is_empty() {
        // Suffix range: bytes=-N
        let suffix_len: u64 = end_str.parse().ok()?;
        if suffix_len == 0 {
            return None;
        }
        let start = file_size.saturating_sub(suffix_len);
        Some((start, file_size - 1))
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
        Some((start, end))
    }
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

    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let limited = file.take(length);
    let stream = tokio_util::io::ReaderStream::new(limited);
    Ok(axum::body::Body::from_stream(stream))
}

#[cfg(test)]
mod discord_channel_full_tests {
    use super::{
        DiscordChannelFull, DiscordOverwrite, DiscordOverwriteType, DiscordRoleFull, VIEW_CHANNEL,
        compute_channel_permissions,
    };

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
        assert_eq!(ch.permission_overwrites[0].allow, "1024");
        assert_eq!(ch.permission_overwrites[0].deny, "0");
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
        assert_eq!(ch.permission_overwrites[0].allow, "1024");
        assert_eq!(ch.permission_overwrites[1].allow, "1");
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
        assert_eq!(ch.permission_overwrites[0].allow, "1024");
        assert_eq!(ch.permission_overwrites[0].deny, "0");
        assert_eq!(ch.permission_overwrites[1].allow, "1");
        assert_eq!(ch.permission_overwrites[1].deny, "0");
        assert_eq!(ch.permission_overwrites[2].allow, "0");
        assert_eq!(ch.permission_overwrites[2].deny, "2");
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
    fn compute_channel_permissions_applies_role_and_member_overwrites() {
        let guild_id = "guild";
        let user_id = "user";
        let member_roles = vec!["role-a".to_owned()];
        let guild_roles = vec![
            DiscordRoleFull {
                id: guild_id.to_owned(),
                permissions: "0".to_owned(),
            },
            DiscordRoleFull {
                id: "role-a".to_owned(),
                permissions: "0".to_owned(),
            },
        ];
        let overwrites = vec![
            DiscordOverwrite {
                id: guild_id.to_owned(),
                type_: DiscordOverwriteType::Role,
                allow: "0".to_owned(),
                deny: "0".to_owned(),
            },
            DiscordOverwrite {
                id: "role-a".to_owned(),
                type_: DiscordOverwriteType::Role,
                allow: VIEW_CHANNEL.to_string(),
                deny: "0".to_owned(),
            },
            DiscordOverwrite {
                id: user_id.to_owned(),
                type_: DiscordOverwriteType::Member,
                allow: "0".to_owned(),
                deny: VIEW_CHANNEL.to_string(),
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
}
