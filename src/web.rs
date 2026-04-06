use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Json, Router};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::sync::Arc;
use tokio_postgres::Client as PgClient;

type HmacSha256 = Hmac<Sha256>;

const MEETING_HTML: &str = include_str!("../assets/meeting.html");
const SESSION_COOKIE_NAME: &str = "dt_session";
const SESSION_TTL_SECS: u64 = 7 * 24 * 3600; // 7 days
const MAX_RANGE_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB cap for range reads

// ---------- State ----------

#[derive(Clone)]
pub struct WebState {
    pub db: Arc<PgClient>,
    pub chunk_storage_dir: String,
    pub auth: Option<Arc<AuthConfig>>,
    pub http_client: reqwest::Client,
}

pub struct AuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub session_secret: String,
    pub redirect_uri: String,
    pub guild_id: String,
    pub secure_cookie: bool,
}

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
        .merge(auth_routes)
        .merge(protected)
        .with_state(state)
}

// ========== Auth: middleware ==========

async fn require_auth(
    State(state): State<WebState>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let Some(ref auth) = state.auth else {
        return (StatusCode::SERVICE_UNAVAILABLE, "OAuth not configured").into_response();
    };

    if let Some(cookie_val) = get_cookie(&headers, SESSION_COOKIE_NAME)
        && let Some(session) = verify_session(&cookie_val, &auth.session_secret)
        && session.gid == auth.guild_id
    {
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
    let token_res = state
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
        .await;

    let token: TokenResponse = match token_res {
        Ok(resp) if resp.status().is_success() => match resp.json().await {
            Ok(t) => t,
            Err(_) => return (StatusCode::BAD_GATEWAY, "invalid token response").into_response(),
        },
        _ => return (StatusCode::BAD_GATEWAY, "token exchange failed").into_response(),
    };

    // Check guild membership
    let guilds_res = state
        .http_client
        .get("https://discord.com/api/users/@me/guilds")
        .header("Authorization", format!("Bearer {}", token.access_token))
        .send()
        .await;

    let guilds: Vec<DiscordGuild> = match guilds_res {
        Ok(resp) if resp.status().is_success() => match resp.json().await {
            Ok(g) => g,
            Err(_) => return (StatusCode::BAD_GATEWAY, "invalid guilds response").into_response(),
        },
        _ => return (StatusCode::BAD_GATEWAY, "failed to fetch guilds").into_response(),
    };

    let is_member = guilds.iter().any(|g| g.id == auth.guild_id);
    if !is_member {
        return (StatusCode::FORBIDDEN, "not a member of this server").into_response();
    }

    // Create session cookie
    let redirect = sanitize_redirect(&redirect);
    let session_value = sign_session(&auth.guild_id, &auth.session_secret);
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
    gid: String,
    exp: u64,
}

fn sign_session(guild_id: &str, secret: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let payload = SessionPayload {
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
    // Reject expired state (10 minute window)
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

async fn meeting_page(Path(meeting_id): Path<String>) -> impl IntoResponse {
    // JSON-encode for safe JS string literal injection, then strip surrounding quotes.
    // Also escape HTML entities for defense-in-depth (template only uses this in a
    // JS string literal, but escaping <, >, & prevents injection if context changes).
    let escaped = serde_json::to_string(&meeting_id).unwrap_or_default();
    let escaped = escaped[1..escaped.len() - 1]
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let html = MEETING_HTML.replace("{{MEETING_ID}}", &escaped);
    Html(html)
}

async fn api_meeting(
    State(state): State<WebState>,
    Path(meeting_id): Path<String>,
) -> Result<Json<MeetingResponse>, StatusCode> {
    let row = state
        .db
        .query_opt(
            "SELECT id, title, status, \
             to_char(started_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') as started_at, \
             to_char(stopped_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') as stopped_at, \
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
    Path(meeting_id): Path<String>,
) -> Result<Json<Vec<TranscriptSegmentResponse>>, StatusCode> {
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
    Path(meeting_id): Path<String>,
) -> Result<Json<SummaryResponse>, StatusCode> {
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
    Path(meeting_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let safe_id = crate::storage_fs::sanitize_path_component(&meeting_id);
    let path = std::path::PathBuf::from(&state.chunk_storage_dir)
        .join(&safe_id)
        .join("mixdown.wav");

    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let file_size = metadata.len();

    // Parse Range header for partial content
    if let Some(range_header) = headers.get(header::RANGE) {
        let range_str = range_header.to_str().map_err(|_| StatusCode::BAD_REQUEST)?;
        if let Some((start, end)) = parse_range(range_str, file_size) {
            let length = end - start + 1;
            let data = read_file_range(&path, start, length).await?;

            let content_range = format!("bytes {start}-{end}/{file_size}");
            return Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_TYPE, "audio/wav")
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::CONTENT_LENGTH, length.to_string())
                .header(header::CONTENT_RANGE, content_range)
                .body(axum::body::Body::from(data))
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR);
        }
    }

    // Stream full file without buffering entirely into memory
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
    let range_str = range_str.strip_prefix("bytes=")?;
    let mut parts = range_str.splitn(2, '-');
    let start_str = parts.next()?.trim();
    let end_str = parts.next()?.trim();

    if start_str.is_empty() {
        let suffix_len: u64 = end_str.parse().ok()?;
        let start = file_size.saturating_sub(suffix_len);
        Some((start, file_size - 1))
    } else {
        let start: u64 = start_str.parse().ok()?;
        let end = if end_str.is_empty() {
            file_size - 1
        } else {
            end_str.parse().ok()?
        };
        if start > end || start >= file_size {
            return None;
        }
        let end = end.min(file_size - 1);
        Some((start, end))
    }
}

async fn read_file_range(
    path: &std::path::Path,
    start: u64,
    length: u64,
) -> Result<Vec<u8>, StatusCode> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let buf_len =
        usize::try_from(length.min(MAX_RANGE_BYTES)).map_err(|_| StatusCode::BAD_REQUEST)?;

    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut buf = vec![0u8; buf_len];
    file.read_exact(&mut buf)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(buf)
}
