use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio_postgres::Client as PgClient;
use tracing::warn;

type HmacSha256 = Hmac<Sha256>;

const MEETING_HTML: &str = include_str!("../assets/meeting.html");
const SESSION_COOKIE_NAME: &str = "dt_session";
const SESSION_TTL_SECS: u64 = 7 * 24 * 3600; // 7 days
const VIEW_CHANNEL: u64 = 1 << 10;
const ADMINISTRATOR: u64 = 1 << 3;

// ---------- State ----------

const PERMISSION_CACHE_TTL_SECS: u64 = 60;

type PermissionCache = Arc<Mutex<HashMap<(String, String), (bool, Instant)>>>;

#[derive(Clone)]
pub struct WebState {
    pub db: Arc<PgClient>,
    pub chunk_storage_dir: String,
    pub auth: Option<Arc<AuthConfig>>,
    pub http_client: reqwest::Client,
    /// Cache: (user_id, channel_id) → (allowed, expires_at)
    pub permission_cache: PermissionCache,
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
        .route("/meetings/{meeting_id}", get(meeting_page))
        .route("/api/meetings/{meeting_id}", get(api_meeting))
        .route("/api/meetings/{meeting_id}/transcript", get(api_transcript))
        .route("/api/meetings/{meeting_id}/summary", get(api_summary))
        .route("/api/meetings/{meeting_id}/audio", get(api_audio))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_auth,
        ));

    Router::new()
        .route("/", get(index_page))
        .merge(auth_routes)
        .merge(protected)
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
/// Results are cached per (user_id, channel_id) for 60 seconds to avoid
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
        let cache = state
            .permission_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
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
        let mut cache = state
            .permission_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let expires_at = Instant::now() + std::time::Duration::from_secs(PERMISSION_CACHE_TTL_SECS);
        cache.insert(cache_key, (allowed, expires_at));

        // Evict expired entries if cache grows large
        if cache.len() > 1000 {
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

/// Query Discord API for channel permission. Returns Ok(true) if allowed,
/// Ok(false) if denied, Err on API failure.
async fn check_channel_permission(
    state: &WebState,
    auth: &AuthConfig,
    channel_id: &str,
    user_id: &str,
) -> Result<bool, StatusCode> {
    let bot_auth = format!("Bot {}", auth.bot_token);
    let (guild_res, channel_res, member_res) = tokio::join!(
        state
            .http_client
            .get(format!("https://discord.com/api/guilds/{}", auth.guild_id))
            .header("Authorization", &bot_auth)
            .send(),
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

    let guild_resp = guild_res.map_err(|err| {
        warn!(error = %err, "discord guild API request failed");
        StatusCode::BAD_GATEWAY
    })?;
    let guild: DiscordGuildFull = guild_resp.json().await.map_err(|err| {
        warn!(error = %err, "discord guild API response parse failed");
        StatusCode::BAD_GATEWAY
    })?;

    let channel_resp = channel_res.map_err(|err| {
        warn!(error = %err, "discord channel API request failed");
        StatusCode::BAD_GATEWAY
    })?;
    let channel: DiscordChannelFull = channel_resp.json().await.map_err(|err| {
        warn!(error = %err, "discord channel API response parse failed");
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

#[derive(Deserialize)]
struct DiscordGuildFull {
    owner_id: String,
    roles: Vec<DiscordRoleFull>,
}

#[derive(Deserialize)]
struct DiscordRoleFull {
    id: String,
    permissions: String,
}

#[derive(Deserialize)]
struct DiscordChannelFull {
    #[serde(default)]
    permission_overwrites: Vec<DiscordOverwrite>,
}

#[derive(Deserialize)]
struct DiscordOverwrite {
    id: String,
    #[serde(rename = "type")]
    type_: u8, // 0 = role, 1 = member
    allow: String,
    deny: String,
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
    if let Some(ow) = overwrites.iter().find(|o| o.type_ == 0 && o.id == guild_id) {
        let allow = ow.allow.parse::<u64>().unwrap_or(0);
        let deny = ow.deny.parse::<u64>().unwrap_or(0);
        permissions &= !deny;
        permissions |= allow;
    }

    // Apply role overwrites (union of allow/deny across all matching roles)
    let mut role_allow: u64 = 0;
    let mut role_deny: u64 = 0;
    for ow in overwrites
        .iter()
        .filter(|o| o.type_ == 0 && o.id != guild_id && member_roles.contains(&o.id))
    {
        role_allow |= ow.allow.parse::<u64>().unwrap_or(0);
        role_deny |= ow.deny.parse::<u64>().unwrap_or(0);
    }
    permissions &= !role_deny;
    permissions |= role_allow;

    // Apply member-specific overwrite
    if let Some(ow) = overwrites.iter().find(|o| o.type_ == 1 && o.id == user_id) {
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

// ---------- Index ----------

async fn index_page() -> Html<&'static str> {
    Html("<html><body><p>discord-transcript is running.</p></body></html>")
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
struct TranscriptSegmentResponse {
    speaker_id: String,
    start_ms: i32,
    end_ms: i32,
    text: String,
    confidence: Option<f64>,
    is_noisy: bool,
}

#[derive(Serialize)]
struct SummaryResponse {
    markdown: Option<String>,
}

// ---------- Handlers ----------

async fn meeting_page(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(meeting_id): Path<String>,
) -> Result<Html<String>, StatusCode> {
    verify_meeting_access(&state, &meeting_id, &user_id).await?;

    // serde_json produces a quoted, JS-safe string (escapes ", \, control chars).
    // Escape </ to <\/ to prevent </script> injection inside the script tag.
    let escaped = serde_json::to_string(&meeting_id)
        .unwrap_or_default()
        .replace("</", "<\\/");
    let html = MEETING_HTML.replace("{{MEETING_ID}}", &escaped);
    Ok(Html(html))
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

async fn api_transcript(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(meeting_id): Path<String>,
) -> Result<Json<Vec<TranscriptSegmentResponse>>, StatusCode> {
    verify_meeting_access(&state, &meeting_id, &user_id).await?;

    let rows = state
        .db
        .query(
            "SELECT speaker_id, start_ms, end_ms, text, confidence, is_noisy \
             FROM transcripts \
             WHERE meeting_id=$1 AND NOT is_deleted \
             ORDER BY start_ms",
            &[&meeting_id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let segments: Vec<TranscriptSegmentResponse> = rows
        .iter()
        .map(|row| TranscriptSegmentResponse {
            speaker_id: row.get("speaker_id"),
            start_ms: row.get("start_ms"),
            end_ms: row.get("end_ms"),
            text: row.get("text"),
            confidence: row.get("confidence"),
            is_noisy: row.get("is_noisy"),
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

    let safe_id = crate::storage_fs::sanitize_path_component(&meeting_id);
    let path = std::path::PathBuf::from(&state.chunk_storage_dir)
        .join(&safe_id)
        .join("mixdown.wav");

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
