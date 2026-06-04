use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use chrono::Utc;
use futures_util::stream::{self, Stream};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Notify, watch};
use tokio_postgres::Client as PgClient;
use tokio_postgres::error::SqlState;
use tower_http::services::{ServeDir, ServeFile};
use tracing::warn;
use uuid::Uuid;

use crate::bootstrap::config::is_iso639_1_format;
use crate::domain::audit::AuditEvent;
use crate::domain::domain_knowledge::DomainKnowledgeContentType;
use crate::domain::speaker::SpeakerProfile;
use crate::domain::summary_template::{summary_template_variables, validate_summary_template};
use crate::domain::transcript::{
    TranscriptSource, TranscriptTimelineOrderKey, compare_transcript_timeline_order,
};
use crate::domain::usage::{NewUsageEvent, UsageDetailJson, UsageMetric};
use crate::infrastructure::bot_token::{
    BotTokenCipher, BotTokenResolveError, resolve_effective_bot_token,
};
use crate::infrastructure::sql::{
    ACTIVATE_DOMAIN_KNOWLEDGE_SQL, ACTIVATE_SUMMARY_TEMPLATE_SQL, ARCHIVE_DOMAIN_KNOWLEDGE_SQL,
    ARCHIVE_SUMMARY_TEMPLATE_SQL, CLEAR_GUILD_BOT_TOKEN_SQL, COUNT_GUILD_MEETINGS_SQL,
    GET_DOMAIN_KNOWLEDGE_SQL, GET_GUILD_SETTINGS_SQL, GET_SUMMARY_TEMPLATE_SQL,
    INSERT_AUDIT_EVENT_SQL, INSERT_DOMAIN_KNOWLEDGE_SQL, INSERT_SUMMARY_TEMPLATE_SQL,
    INSERT_USAGE_EVENT_SQL, LIST_ACTIVE_TENANT_GUILDS_BY_GUILD_IDS_SQL, LIST_DOMAIN_KNOWLEDGE_SQL,
    LIST_GUILD_MEETING_VOICE_CHANNELS_SQL, LIST_GUILD_MEETINGS_SQL, LIST_SUMMARY_TEMPLATES_SQL,
    UPDATE_DOMAIN_KNOWLEDGE_SQL, UPDATE_SUMMARY_TEMPLATE_SQL, UPSERT_GUILD_BOT_TOKEN_SQL,
    UPSERT_GUILD_SETTINGS_SQL,
};
use crate::infrastructure::sql_store::{audit_event_params, usage_event_params};
use crate::infrastructure::storage_fs::sanitize_path_component;

type HmacSha256 = Hmac<Sha256>;
const SESSION_COOKIE_NAME: &str = "dt_session";
const SESSION_TTL_SECS: u64 = 7 * 24 * 3600; // 7 days
const SESSION_MEMBERSHIP_VERIFY_INTERVAL_SECS: u64 = 15 * 60; // 15 minutes
const OAUTH_NONCE_COOKIE_NAME: &str = "dt_oauth_nonce";
const OAUTH_NONCE_COOKIE_PATH: &str = "/auth/callback";
const OAUTH_STATE_TTL_SECS: u64 = 600; // 10 minutes
const VIEW_CHANNEL: u64 = 1 << 10;
const ADMINISTRATOR: u64 = 1 << 3;
const LIST_GUILD_MEETINGS_FOR_VISIBILITY_SQL: &str = r#"
SELECT id,
       status,
       to_char(started_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') as started_at,
       to_char(stopped_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') as stopped_at,
       meeting_duration_seconds,
       title,
       stop_reason,
       voice_channel_id
FROM meetings
WHERE guild_id = $1
  AND ($2::TEXT IS NULL OR voice_channel_id = $2)
ORDER BY started_at DESC
LIMIT $3
"#;
const OPERATIONAL_SCHEMA_READY_SQL: &str = r#"
SELECT
  to_regclass('public.meetings') IS NOT NULL AS meetings_ready,
  to_regclass('public.jobs') IS NOT NULL AS jobs_ready,
  to_regclass('public.live_transcription_chunks') IS NOT NULL AS live_chunks_ready
"#;
const OPERATIONAL_COUNTERS_SQL: &str = r#"
WITH job_counts AS (
  SELECT
    COUNT(*) FILTER (WHERE status = 'failed') AS failed_jobs,
    COUNT(*) FILTER (WHERE status = 'running') AS running_jobs,
    COUNT(*) FILTER (WHERE status = 'queued') AS queued_jobs
  FROM jobs
),
meeting_counts AS (
  SELECT
    COUNT(*) FILTER (WHERE status IN ('recording', 'stopping', 'transcribing', 'summarizing')) AS running_meetings
  FROM meetings
),
live_chunk_counts AS (
  SELECT
    COUNT(*) FILTER (WHERE status = 'failed') AS failed_live_transcription_chunks
  FROM live_transcription_chunks
)
SELECT
  job_counts.failed_jobs,
  job_counts.running_jobs,
  job_counts.queued_jobs,
  meeting_counts.running_meetings,
  live_chunk_counts.failed_live_transcription_chunks
FROM job_counts, meeting_counts, live_chunk_counts
"#;

// ---------- State ----------

const PERMISSION_CACHE_TTL_SECS: u64 = 300;
const PERMISSION_CACHE_SENSITIVE_POSITIVE_TTL_SECS: u64 = 15;
const MEMBERSHIP_CACHE_TTL_SECS: u64 = 5;
const MEMBERSHIP_REVERIFY_INFLIGHT_SECS: u64 = 5;
const GUILD_CACHE_TTL_SECS: u64 = 15;
const GUILD_CACHE_REFRESH_INFLIGHT_SECS: u64 = 5;
const GUILD_CACHE_FAILURE_TTL_SECS: u64 = 5;
const BOT_TOKEN_CACHE_TTL_SECS: u64 = 300;
const BOT_TOKEN_CACHE_REFRESH_INFLIGHT_SECS: u64 = 5;
const BOT_TOKEN_CACHE_FAILURE_TTL_SECS: u64 = 5;
const OPERATIONAL_METRICS_CACHE_TTL_SECS: u64 = 15;
const MIN_AUDIO_RANGE_BYTES: u64 = 64 * 1024;
const AUDIO_RANGE_BUCKET_CAPACITY: f64 = 30.0;
const AUDIO_RANGE_REFILL_PER_SEC: f64 = 10.0;
const GUILD_MEETINGS_VISIBILITY_SCAN_MIN: u32 = 100;
const GUILD_MEETINGS_VISIBILITY_SCAN_LIMIT: u32 = 500;
const GUILD_MEETINGS_VISIBILITY_CHANNEL_CAP: usize = 32;
const TRANSCRIPT_SSE_MAX_PER_USER_MEETING: usize = 2;
const TRANSCRIPT_SSE_BASE_POLL_SECS: u64 = 2;
const TRANSCRIPT_SSE_MAX_POLL_SECS: u64 = 10;
const TRANSCRIPT_SSE_MAX_IDLE_POLLS: u32 = 60;
const USER_GUILDS_CACHE_TTL_SECS: u64 = 60;
const OAUTH_ACCESS_TOKEN_DEFAULT_TTL_SECS: u64 = 3600;
const OAUTH_ACCESS_TOKEN_CLOCK_SKEW_SECS: u64 = 60;

type PermissionCache =
    Arc<tokio::sync::RwLock<HashMap<(String, String), (CachedChannelPermission, Instant)>>>;
type GuildCache = Arc<tokio::sync::RwLock<GuildCacheState>>;
type BotTokenCache = Arc<tokio::sync::RwLock<BotTokenCacheState>>;
type OperationalMetricsCache = Arc<Mutex<Option<OperationalMetricsCacheEntry>>>;
type TranscriptSseLimiter = Arc<std::sync::Mutex<HashMap<(String, String), usize>>>;
type UserGuildsCache = Arc<tokio::sync::RwLock<HashMap<String, UserGuildsCacheEntry>>>;
type MembershipCache =
    Arc<tokio::sync::RwLock<HashMap<String, (Result<bool, StatusCode>, Instant)>>>;
type MembershipReverifyInflight = Arc<tokio::sync::Mutex<HashMap<String, MembershipInflightEntry>>>;

#[derive(Debug, Default)]
struct BotTokenCacheState {
    entry: Option<(String, Instant)>,
    failure: Option<(StatusCode, Instant)>,
    revision: u64,
    refresh: Option<BotTokenRefreshEntry>,
}

#[derive(Clone)]
struct UserGuildsCacheEntry {
    bearer: String,
    token_expires_at: Instant,
    guilds: Vec<DiscordGuild>,
    guilds_expires_at: Instant,
}

#[derive(Default)]
struct GuildCacheState {
    entry: Option<(DiscordGuildFull, Instant)>,
    failure: Option<(StatusCode, Instant)>,
    revision: u64,
    refresh: Option<GuildRefreshEntry>,
}

#[derive(Debug, Clone)]
struct GuildRefreshEntry {
    notify: Arc<Notify>,
    started_at: Instant,
}

#[derive(Debug, Clone)]
struct BotTokenRefreshEntry {
    notify: Arc<Notify>,
    started_at: Instant,
}

#[derive(Clone)]
struct MembershipInflightEntry {
    notify: Arc<Notify>,
    started_at: Instant,
}

enum MembershipReverifyStart {
    Leader(Arc<Notify>),
    Follower(Arc<Notify>),
}

#[derive(Debug, Default)]
struct AudioRangeRateLimiter {
    buckets: HashMap<String, AudioRangeBucket>,
}

#[derive(Debug)]
struct AudioRangeBucket {
    tokens: f64,
    last_refill: Instant,
}

#[derive(Debug)]
struct TranscriptSsePermit {
    limiter: TranscriptSseLimiter,
    key: (String, String),
}

impl Drop for TranscriptSsePermit {
    fn drop(&mut self) {
        let Ok(mut counts) = self.limiter.lock() else {
            return;
        };
        if let Some(count) = counts.get_mut(&self.key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(&self.key);
            }
        }
    }
}

fn try_acquire_transcript_sse_permit(
    limiter: &TranscriptSseLimiter,
    user_id: &str,
    meeting_id: &str,
) -> Result<TranscriptSsePermit, StatusCode> {
    let key = (user_id.to_owned(), meeting_id.to_owned());
    let mut counts = limiter
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let count = counts.entry(key.clone()).or_insert(0);
    if *count >= TRANSCRIPT_SSE_MAX_PER_USER_MEETING {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    *count += 1;
    Ok(TranscriptSsePermit {
        limiter: limiter.clone(),
        key,
    })
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuildAdminCheck {
    Admin,
    NotAdmin,
    BotAccessDenied,
    RateLimited,
}

impl GuildAdminCheck {
    fn into_status_result(self) -> Result<bool, StatusCode> {
        match self {
            Self::Admin => Ok(true),
            Self::NotAdmin => Ok(false),
            Self::BotAccessDenied | Self::RateLimited => Err(StatusCode::BAD_GATEWAY),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GuildSettingsDefaults {
    pub whisper_language: Option<String>,
    pub whisper_vad: bool,
    pub auto_stop_grace_seconds: i64,
    pub retention_raw_audio_ttl_days: i32,
    pub retention_transcript_ttl_days: i32,
    pub summary_enabled: bool,
}

#[derive(Clone, Default)]
pub struct GuildBotTokenRuntimeConfig {
    pub cipher: Option<Arc<BotTokenCipher>>,
    pub revision_tx: Option<watch::Sender<u64>>,
    pub operational_metrics_bearer_token: Option<String>,
}

#[derive(Clone)]
pub struct WebState {
    pub db: Arc<PgClient>,
    pub chunk_storage_dir: String,
    pub auth: Option<Arc<AuthConfig>>,
    pub http_client: reqwest::Client,
    pub guild_bot_token_cipher: Option<Arc<BotTokenCipher>>,
    operational_metrics_bearer_token: Option<Arc<str>>,
    /// Cache: resolved effective bot token for the configured guild.
    bot_token_cache: BotTokenCache,
    bot_token_revision_tx: Option<watch::Sender<u64>>,
    operational_metrics_cache: OperationalMetricsCache,
    /// Cache: (user_id, channel_id) -> (computed channel access, expires_at)
    pub permission_cache: PermissionCache,
    /// Cache: guild info (shared across all requests)
    guild_cache: GuildCache,
    /// Cache: current user's OAuth-visible guild list, populated at login.
    user_guilds_cache: UserGuildsCache,
    /// Short-lived guild membership cache to bound Discord lookups during API bursts.
    membership_cache: MembershipCache,
    /// In-flight guild membership verification per user id
    membership_reverify_inflight: MembershipReverifyInflight,
    audio_range_limiter: Arc<Mutex<AudioRangeRateLimiter>>,
    transcript_sse_limiter: TranscriptSseLimiter,
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
        guild_bot_token: GuildBotTokenRuntimeConfig,
        static_files_dir: String,
        guild_settings_defaults: GuildSettingsDefaults,
    ) -> Self {
        Self {
            db,
            chunk_storage_dir,
            auth,
            http_client,
            guild_bot_token_cipher: guild_bot_token.cipher,
            operational_metrics_bearer_token: guild_bot_token
                .operational_metrics_bearer_token
                .map(Arc::<str>::from),
            bot_token_cache: Arc::new(tokio::sync::RwLock::new(BotTokenCacheState::default())),
            bot_token_revision_tx: guild_bot_token.revision_tx,
            operational_metrics_cache: Arc::new(Mutex::new(None)),
            permission_cache: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            guild_cache: Arc::new(tokio::sync::RwLock::new(GuildCacheState::default())),
            user_guilds_cache: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            membership_cache: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            membership_reverify_inflight: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            audio_range_limiter: Arc::new(Mutex::new(AudioRangeRateLimiter::default())),
            transcript_sse_limiter: Arc::new(std::sync::Mutex::new(HashMap::new())),
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

fn audit_request_metadata(headers: &HeaderMap, method: &str, path: &str) -> String {
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.chars().take(512).collect::<String>());
    json!({
        "method": method,
        "path": path,
        "user_agent": user_agent,
    })
    .to_string()
}

fn web_audit_event(
    guild_id: Option<String>,
    actor_user_id: Option<String>,
    action: &str,
    resource_type: &str,
    resource_id: Option<String>,
    request_metadata_json: String,
    detail: Value,
) -> AuditEvent {
    let now = Utc::now();
    AuditEvent {
        id: Uuid::new_v4().to_string(),
        tenant_id: None,
        guild_id,
        actor_user_id,
        action: action.to_owned(),
        resource_type: resource_type.to_owned(),
        resource_id,
        request_metadata_json,
        detail_json: detail.to_string(),
        occurred_at: now,
        created_at: now,
    }
}

async fn record_audit_event(state: &WebState, event: AuditEvent) {
    let params = audit_event_params(&event);
    let bind: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
        .iter()
        .map(|value| value as &(dyn tokio_postgres::types::ToSql + Sync))
        .collect();
    if let Err(err) = state.db.execute(INSERT_AUDIT_EVENT_SQL, &bind).await {
        warn!(
            error = %err,
            action = %event.action,
            resource_type = %event.resource_type,
            "failed to persist audit event"
        );
    }
}

async fn record_usage_event(state: &WebState, event: NewUsageEvent) {
    let params = usage_event_params(&event);
    let bind: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
        .iter()
        .map(|value| value as &(dyn tokio_postgres::types::ToSql + Sync))
        .collect();
    if let Err(err) = state.db.execute(INSERT_USAGE_EVENT_SQL, &bind).await {
        warn!(
            error = %err,
            usage_event_id = %event.id,
            metric = %event.metric.as_str(),
            "failed to persist usage event"
        );
    }
}

fn debug_download_usage_event_id(
    guild_id: &str,
    meeting_id: &str,
    artifact_id: &str,
    filename: &str,
    content_type: &str,
    user_id: &str,
) -> String {
    let mut hasher = Sha256::new();
    for part in [
        "debug_downloads",
        guild_id,
        meeting_id,
        artifact_id,
        filename,
        content_type,
        user_id,
    ] {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    let digest = hasher.finalize();
    let mut id = String::from("usage:debug_downloads:");
    for byte in &digest[..16] {
        id.push_str(&format!("{byte:02x}"));
    }
    id
}

#[derive(Clone)]
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
        .route("/api/me/guilds", get(api_me_guilds))
        .route("/api/guild/meetings", get(api_guild_meetings))
        .route(
            "/api/guilds/{guild_id}/meetings",
            get(api_target_guild_meetings),
        )
        .route(
            "/api/guilds/{guild_id}/settings",
            get(api_target_guild_settings).put(api_update_target_guild_settings),
        )
        .route(
            "/api/guilds/{guild_id}/settings/bot-token",
            put(api_update_target_guild_bot_token).delete(api_delete_target_guild_bot_token),
        )
        .route(
            "/api/guild/settings",
            get(api_guild_settings).put(api_update_guild_settings),
        )
        .route(
            "/api/guild/settings/bot-token",
            put(api_update_guild_bot_token).delete(api_delete_guild_bot_token),
        )
        .route(
            "/api/guild/domain-knowledge",
            get(api_list_domain_knowledge).post(api_create_domain_knowledge),
        )
        .route(
            "/api/guild/domain-knowledge/{item_id}",
            get(api_get_domain_knowledge).put(api_update_domain_knowledge),
        )
        .route(
            "/api/guild/domain-knowledge/{item_id}/activate",
            post(api_activate_domain_knowledge),
        )
        .route(
            "/api/guild/domain-knowledge/{item_id}/archive",
            post(api_archive_domain_knowledge),
        )
        .route(
            "/api/guild/summary-templates",
            get(api_list_summary_templates).post(api_create_summary_template),
        )
        .route(
            "/api/guild/summary-templates/{template_id}",
            get(api_get_summary_template).put(api_update_summary_template),
        )
        .route(
            "/api/guild/summary-templates/{template_id}/activate",
            post(api_activate_summary_template),
        )
        .route(
            "/api/guild/summary-templates/{template_id}/archive",
            post(api_archive_summary_template),
        )
        .route("/api/meetings/{meeting_id}", get(api_meeting))
        .route("/api/meetings/{meeting_id}/transcript", get(api_transcript))
        .route(
            "/api/meetings/{meeting_id}/transcript/state",
            get(api_transcript_state),
        )
        .route(
            "/api/meetings/{meeting_id}/transcript/events",
            get(api_transcript_events),
        )
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
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metricsz", get(metricsz))
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
    if !session_matches_guild(&session, &auth.guild_id) {
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

    let membership = if allows_settings_token_recovery(request.uri().path()) {
        verify_current_guild_member_for_settings_recovery(&state, auth, &session.uid).await
    } else {
        verify_current_guild_member(&state, auth, &session.uid).await
    };
    let refreshed_session_cookie = match membership {
        Ok(true) => {
            if session_needs_cookie_refresh(&session) {
                Some(session_cookie_with_membership(
                    &session.uid,
                    &auth.guild_id,
                    auth,
                    session.exp,
                    session.issued_at,
                    unix_now_secs(),
                    0,
                ))
            } else {
                None
            }
        }
        Ok(false) => {
            invalidate_permission_cache_for_user(&state.permission_cache, &session.uid).await;
            warn!(
                user_id = %session.uid,
                guild_id = %auth.guild_id,
                "denying session after failed guild membership verification"
            );
            return guild_membership_forbidden_response()
                .with_cleared_session_cookie(auth.secure_cookie);
        }
        Err(status) => {
            warn!(
                status = %status,
                user_id = %session.uid,
                "guild membership verification unavailable; denying protected request"
            );
            return status.into_response();
        }
    };

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

fn guild_membership_forbidden_response() -> Response {
    StatusCode::FORBIDDEN.into_response()
}

fn session_matches_guild(session: &SessionPayload, guild_id: &str) -> bool {
    session.gid == guild_id
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn session_needs_cookie_refresh(session: &SessionPayload) -> bool {
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

fn allows_settings_token_recovery(path: &str) -> bool {
    matches!(
        path,
        "/api/me" | "/api/me/guilds" | "/api/guild/settings" | "/api/guild/settings/bot-token"
    ) || is_target_guild_settings_recovery_path(path)
}

fn is_target_guild_settings_recovery_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/api/guilds/") else {
        return false;
    };
    let mut parts = rest.split('/');
    let Some(guild_id) = parts.next() else {
        return false;
    };
    if guild_id.is_empty() {
        return false;
    }
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some("settings"), None, None) | (Some("settings"), Some("bot-token"), None)
    )
}

fn bot_token_resolve_status(err: &BotTokenResolveError) -> StatusCode {
    match err {
        BotTokenResolveError::MissingCipher => StatusCode::SERVICE_UNAVAILABLE,
        BotTokenResolveError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
        BotTokenResolveError::Crypto(_) => StatusCode::BAD_GATEWAY,
    }
}

fn cached_bot_token_result(cache: &BotTokenCacheState) -> Option<Result<String, StatusCode>> {
    let now = Instant::now();
    if let Some((token, expires_at)) = cache.entry.as_ref()
        && now < *expires_at
    {
        return Some(Ok(format!("Bot {token}")));
    }
    if let Some((status, expires_at)) = cache.failure.as_ref()
        && now < *expires_at
    {
        return Some(Err(*status));
    }
    None
}

fn fresh_bot_token_refresh_notify(cache: &BotTokenCacheState) -> Option<Arc<Notify>> {
    let refresh = cache.refresh.as_ref()?;
    if Instant::now().duration_since(refresh.started_at).as_secs()
        < BOT_TOKEN_CACHE_REFRESH_INFLIGHT_SECS
    {
        return Some(refresh.notify.clone());
    }
    None
}

async fn bot_auth_header_for_guild(
    state: &WebState,
    auth: &AuthConfig,
) -> Result<String, StatusCode> {
    if !is_configured_auth_guild(state, &auth.guild_id) {
        return resolve_bot_auth_header_for_guild_uncached(state, auth).await;
    }
    bot_auth_header_from_cache_with_resolver(&state.bot_token_cache, || async {
        resolve_effective_bot_token(
            &state.db,
            &auth.guild_id,
            &auth.bot_token,
            state.guild_bot_token_cipher.as_deref(),
        )
        .await
        .map_err(|err| {
            let status = bot_token_resolve_status(&err);
            warn!(
                error = %err,
                guild_id = %auth.guild_id,
                status = %status,
                "failed to resolve guild bot token"
            );
            status
        })
    })
    .await
}

fn is_configured_auth_guild(state: &WebState, guild_id: &str) -> bool {
    state
        .auth
        .as_ref()
        .is_some_and(|configured| configured.guild_id == guild_id)
}

async fn resolve_bot_auth_header_for_guild_uncached(
    state: &WebState,
    auth: &AuthConfig,
) -> Result<String, StatusCode> {
    resolve_effective_bot_token(
        &state.db,
        &auth.guild_id,
        &auth.bot_token,
        state.guild_bot_token_cipher.as_deref(),
    )
    .await
    .map(|token| format!("Bot {token}"))
    .map_err(|err| {
        let status = bot_token_resolve_status(&err);
        warn!(
            error = %err,
            guild_id = %auth.guild_id,
            status = %status,
            "failed to resolve guild bot token"
        );
        status
    })
}

async fn bot_auth_header_from_cache_with_resolver<F, Fut>(
    bot_token_cache: &BotTokenCache,
    resolve: F,
) -> Result<String, StatusCode>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<String, StatusCode>>,
{
    loop {
        {
            let cache = bot_token_cache.read().await;
            if let Some(result) = cached_bot_token_result(&cache) {
                return result;
            }
            if let Some(notify) = fresh_bot_token_refresh_notify(&cache) {
                drop(cache);
                let _ = tokio::time::timeout(
                    Duration::from_secs(BOT_TOKEN_CACHE_REFRESH_INFLIGHT_SECS),
                    notify.notified(),
                )
                .await;
                continue;
            }
        }

        let (observed_revision, leader_notify) = {
            let mut cache = bot_token_cache.write().await;
            if let Some(result) = cached_bot_token_result(&cache) {
                return result;
            }
            if let Some(notify) = fresh_bot_token_refresh_notify(&cache) {
                drop(cache);
                let _ = tokio::time::timeout(
                    Duration::from_secs(BOT_TOKEN_CACHE_REFRESH_INFLIGHT_SECS),
                    notify.notified(),
                )
                .await;
                continue;
            }
            if let Some(stale_refresh) = cache.refresh.take() {
                stale_refresh.notify.notify_waiters();
            }
            let notify = Arc::new(Notify::new());
            cache.refresh = Some(BotTokenRefreshEntry {
                notify: notify.clone(),
                started_at: Instant::now(),
            });
            (cache.revision, notify)
        };

        let resolved = resolve().await;

        let mut cache = bot_token_cache.write().await;
        let is_current_refresh = cache
            .refresh
            .as_ref()
            .is_some_and(|refresh| Arc::ptr_eq(&refresh.notify, &leader_notify));
        if !is_current_refresh {
            continue;
        }
        let mut refresh_notify = cache.refresh.take();
        if let Some(result) = cached_bot_token_result(&cache) {
            if let Some(notify) = refresh_notify.take() {
                notify.notify.notify_waiters();
            }
            return result;
        }
        if cache.revision != observed_revision {
            if let Some(notify) = refresh_notify.take() {
                notify.notify.notify_waiters();
            }
            continue;
        }
        match resolved {
            Ok(token) => {
                let expires_at = Instant::now() + Duration::from_secs(BOT_TOKEN_CACHE_TTL_SECS);
                cache.entry = Some((token.clone(), expires_at));
                cache.failure = None;
                if let Some(notify) = refresh_notify.take() {
                    notify.notify.notify_waiters();
                }
                return Ok(format!("Bot {token}"));
            }
            Err(status) => {
                let expires_at =
                    Instant::now() + Duration::from_secs(BOT_TOKEN_CACHE_FAILURE_TTL_SECS);
                cache.failure = Some((status, expires_at));
                if let Some(notify) = refresh_notify.take() {
                    notify.notify.notify_waiters();
                }
                return Err(status);
            }
        }
    }
}

async fn cached_guild_membership(
    cache: &MembershipCache,
    user_id: &str,
) -> Option<Result<bool, StatusCode>> {
    let now = Instant::now();
    {
        let cache = cache.read().await;
        if let Some(&(membership, expires_at)) = cache.get(user_id)
            && now < expires_at
        {
            return Some(membership);
        }
    }

    let mut cache = cache.write().await;
    if let Some(&(_, expires_at)) = cache.get(user_id)
        && now >= expires_at
    {
        cache.remove(user_id);
    }
    None
}

async fn cache_guild_membership(
    cache: &MembershipCache,
    user_id: &str,
    membership: Result<bool, StatusCode>,
) {
    let mut cache = cache.write().await;
    let expires_at = Instant::now() + Duration::from_secs(MEMBERSHIP_CACHE_TTL_SECS);
    cache.insert(user_id.to_owned(), (membership, expires_at));
    if cache.len() > 5000 {
        let now = Instant::now();
        cache.retain(|_, (_, expires_at)| *expires_at > now);
    }
}

async fn begin_membership_reverify(
    inflight: &MembershipReverifyInflight,
    user_id: &str,
) -> MembershipReverifyStart {
    let mut map = inflight.lock().await;
    let now = Instant::now();
    let stale_user_ids = map
        .iter()
        .filter(|(_, entry)| {
            now.duration_since(entry.started_at).as_secs() >= MEMBERSHIP_REVERIFY_INFLIGHT_SECS
        })
        .map(|(stale_user_id, _)| stale_user_id.clone())
        .collect::<Vec<_>>();
    for stale_user_id in stale_user_ids {
        if let Some(entry) = map.remove(&stale_user_id) {
            entry.notify.notify_waiters();
        }
    }

    if let Some(entry) = map.get(user_id) {
        return MembershipReverifyStart::Follower(entry.notify.clone());
    }
    let notify = Arc::new(Notify::new());
    map.insert(
        user_id.to_owned(),
        MembershipInflightEntry {
            notify: notify.clone(),
            started_at: now,
        },
    );
    MembershipReverifyStart::Leader(notify)
}

async fn publish_membership_reverify_result(
    inflight: &MembershipReverifyInflight,
    cache: &MembershipCache,
    user_id: &str,
    leader_notify: &Arc<Notify>,
    membership: Result<bool, StatusCode>,
) -> bool {
    let notify = {
        let mut map = inflight.lock().await;
        let Some(entry) = map.get(user_id) else {
            return false;
        };
        if !Arc::ptr_eq(&entry.notify, leader_notify) {
            return false;
        }
        map.remove(user_id).map(|entry| entry.notify)
    };
    cache_guild_membership(cache, user_id, membership).await;
    if let Some(notify) = notify {
        notify.notify_waiters();
    }
    true
}

async fn verify_current_guild_member(
    state: &WebState,
    auth: &AuthConfig,
    user_id: &str,
) -> Result<bool, StatusCode> {
    verify_guild_membership_reverify_with(
        &state.membership_cache,
        &state.membership_reverify_inflight,
        user_id,
        true,
        || is_guild_member(state, auth, user_id),
    )
    .await
}

async fn verify_guild_membership_reverify_with<F, Fut>(
    cache: &MembershipCache,
    inflight: &MembershipReverifyInflight,
    user_id: &str,
    use_cached_result: bool,
    verify: F,
) -> Result<bool, StatusCode>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<bool, StatusCode>>,
{
    if use_cached_result && let Some(membership) = cached_guild_membership(cache, user_id).await {
        return membership;
    }
    loop {
        if use_cached_result && let Some(membership) = cached_guild_membership(cache, user_id).await
        {
            return membership;
        }
        match begin_membership_reverify(inflight, user_id).await {
            MembershipReverifyStart::Follower(notify) => {
                let _ = tokio::time::timeout(
                    Duration::from_secs(MEMBERSHIP_REVERIFY_INFLIGHT_SECS),
                    notify.notified(),
                )
                .await;
                if let Some(membership) = cached_guild_membership(cache, user_id).await {
                    return membership;
                }
            }
            MembershipReverifyStart::Leader(leader_notify) => {
                let membership = verify().await;
                if publish_membership_reverify_result(
                    inflight,
                    cache,
                    user_id,
                    &leader_notify,
                    membership,
                )
                .await
                {
                    return membership;
                }
            }
        }
    }
}

async fn is_guild_member(
    state: &WebState,
    auth: &AuthConfig,
    user_id: &str,
) -> Result<bool, StatusCode> {
    let bot_auth = bot_auth_header_for_guild(state, auth).await?;
    is_guild_member_with_bot_auth(state, auth, user_id, &bot_auth).await
}

async fn is_guild_member_with_bot_auth(
    state: &WebState,
    auth: &AuthConfig,
    user_id: &str,
    bot_auth: &str,
) -> Result<bool, StatusCode> {
    let response = state
        .http_client
        .get(format!(
            "https://discord.com/api/guilds/{}/members/{user_id}",
            auth.guild_id
        ))
        .header("Authorization", bot_auth)
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
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        warn!(
            status = %status,
            user_id = %user_id,
            "discord guild member re-verify rate limited"
        );
        return Err(StatusCode::TOO_MANY_REQUESTS);
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

async fn verify_current_guild_member_for_settings_recovery(
    state: &WebState,
    auth: &AuthConfig,
    user_id: &str,
) -> Result<bool, StatusCode> {
    let membership = verify_current_guild_member(state, auth, user_id).await;
    if !should_retry_settings_membership_check_with_global(&membership) {
        return membership;
    }

    warn!(
        user_id = %user_id,
        guild_id = %auth.guild_id,
        "guild-scoped bot token membership verification failed; trying global token for settings recovery"
    );
    let global_bot_auth = format!("Bot {}", auth.bot_token);
    verify_guild_membership_reverify_with(
        &state.membership_cache,
        &state.membership_reverify_inflight,
        user_id,
        false,
        || is_guild_member_with_bot_auth(state, auth, user_id, &global_bot_auth),
    )
    .await
}

fn should_retry_settings_membership_check_with_global(result: &Result<bool, StatusCode>) -> bool {
    matches!(
        result,
        Err(StatusCode::SERVICE_UNAVAILABLE | StatusCode::BAD_GATEWAY)
    )
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
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct DiscordUserInfo {
    id: String,
}

#[derive(Debug, Deserialize, Clone)]
struct DiscordGuild {
    id: String,
    name: String,
    icon: Option<String>,
    #[serde(default)]
    owner: bool,
    #[serde(
        default = "zero_permission_bits",
        deserialize_with = "deserialize_permission_bits"
    )]
    permissions: u64,
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

    let token_expires_at = oauth_access_token_expires_at(token.expires_in);
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
    cache_user_discord_guilds(
        &state.user_guilds_cache,
        &user.id,
        bearer,
        guilds.clone(),
        token_expires_at,
    )
    .await;

    // Create session cookie with user ID
    let redirect = sanitize_redirect(&redirect);
    let session_cookie = session_cookie_value(&user.id, &auth.guild_id, auth);
    let clear_oauth_nonce_cookie = format_oauth_nonce_cookie("", auth.secure_cookie, 0);
    record_audit_event(
        &state,
        web_audit_event(
            Some(auth.guild_id.clone()),
            Some(user.id.clone()),
            "auth.login",
            "session",
            Some(user.id.clone()),
            audit_request_metadata(&headers, "GET", "/auth/callback"),
            json!({"result": "success"}),
        ),
    )
    .await;

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
    let mut revoked_session: Option<(String, String)> = None;
    if let Some(ref auth) = state.auth
        && let Some(cookie_val) = get_cookie(&headers, SESSION_COOKIE_NAME)
        && let Some(session) = verify_session(&cookie_val, &auth.session_secret)
    {
        if let Err(err) = revoke_session(&state.db, &session.uid, session.issued_at).await {
            warn!(
                error = %err,
                user_id = %session.uid,
                "failed to persist session revocation"
            );
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        revoked_session = Some((auth.guild_id.clone(), session.uid));
    }
    if let Some((guild_id, user_id)) = revoked_session {
        record_audit_event(
            &state,
            web_audit_event(
                Some(guild_id),
                Some(user_id.clone()),
                "auth.logout",
                "session",
                Some(user_id),
                audit_request_metadata(&headers, "POST", "/auth/logout"),
                json!({"revoked": true}),
            ),
        )
        .await;
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

fn permission_cache_ttl(permission: CachedChannelPermission) -> u64 {
    if permission.can_view || permission.is_admin {
        PERMISSION_CACHE_SENSITIVE_POSITIVE_TTL_SECS
    } else {
        PERMISSION_CACHE_TTL_SECS
    }
}

async fn cache_channel_permission(
    permission_cache: &PermissionCache,
    cache_key: (String, String),
    permission: CachedChannelPermission,
) {
    let mut cache = permission_cache.write().await;
    let expires_at = Instant::now() + Duration::from_secs(permission_cache_ttl(permission));
    cache.insert(cache_key, (permission, expires_at));

    if cache.len() > 5000 {
        let now = Instant::now();
        cache.retain(|_, (_, exp)| *exp > now);
    }
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

    cache_channel_permission(permission_cache, cache_key, permission).await;

    if permission.can_view {
        Ok(access)
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

async fn guild_meeting_channel_visible_after_row<Fut>(
    guild_id: String,
    channel_id: String,
    authenticated_guild_id: &str,
    user_id: &str,
    permission_cache: &PermissionCache,
    permission_check: Fut,
) -> Result<bool, StatusCode>
where
    Fut: Future<Output = Result<CachedChannelPermission, StatusCode>>,
{
    match verify_meeting_access_after_row(
        guild_id,
        channel_id,
        authenticated_guild_id,
        user_id,
        permission_cache,
        permission_check,
    )
    .await
    {
        Ok(_) => Ok(true),
        Err(StatusCode::FORBIDDEN) => Ok(false),
        Err(status) => Err(status),
    }
}

/// Verify that the authenticated user has VIEW_CHANNEL permission on the
/// voice channel where the meeting was recorded. Returns the meeting's
/// guild/voice-channel IDs so callers can build paths without an extra
/// DB round-trip.
/// Results are cached per (user_id, channel_id) to avoid Discord API
/// rate-limit exhaustion on page loads (which trigger ~4 requests). Positive
/// allows use a short reverify window so permission revocations take effect
/// quickly; denials use the longer cache TTL.
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
    let bot_auth = bot_auth_header_for_guild(state, auth).await?;
    get_guild_info_with_bot_auth(state, auth, &bot_auth).await
}

fn cached_guild_result(cache: &GuildCacheState) -> Option<Result<DiscordGuildFull, StatusCode>> {
    let now = Instant::now();
    if let Some((guild, expires_at)) = cache.entry.as_ref()
        && now < *expires_at
    {
        return Some(Ok(guild.clone()));
    }
    if let Some((status, expires_at)) = cache.failure.as_ref()
        && now < *expires_at
    {
        return Some(Err(*status));
    }
    None
}

fn fresh_guild_refresh_notify(cache: &GuildCacheState) -> Option<Arc<Notify>> {
    let refresh = cache.refresh.as_ref()?;
    if Instant::now().duration_since(refresh.started_at).as_secs()
        < GUILD_CACHE_REFRESH_INFLIGHT_SECS
    {
        return Some(refresh.notify.clone());
    }
    None
}

async fn guild_info_from_cache_with_resolver<F, Fut>(
    guild_cache: &GuildCache,
    fetch_guild: F,
) -> Result<DiscordGuildFull, StatusCode>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<DiscordGuildFull, StatusCode>>,
{
    loop {
        {
            let cache = guild_cache.read().await;
            if let Some(result) = cached_guild_result(&cache) {
                return result;
            }
            if let Some(notify) = fresh_guild_refresh_notify(&cache) {
                let notified = notify.notified();
                drop(cache);
                let _ = tokio::time::timeout(
                    Duration::from_secs(GUILD_CACHE_REFRESH_INFLIGHT_SECS),
                    notified,
                )
                .await;
                continue;
            }
        }

        let (observed_revision, leader_notify) = {
            let mut cache = guild_cache.write().await;
            if let Some(result) = cached_guild_result(&cache) {
                return result;
            }
            if let Some(notify) = fresh_guild_refresh_notify(&cache) {
                let notified = notify.notified();
                drop(cache);
                let _ = tokio::time::timeout(
                    Duration::from_secs(GUILD_CACHE_REFRESH_INFLIGHT_SECS),
                    notified,
                )
                .await;
                continue;
            }
            if let Some(stale_refresh) = cache.refresh.take() {
                stale_refresh.notify.notify_waiters();
            }
            let notify = Arc::new(Notify::new());
            cache.refresh = Some(GuildRefreshEntry {
                notify: notify.clone(),
                started_at: Instant::now(),
            });
            (cache.revision, notify)
        };

        let resolved = fetch_guild().await;

        let mut cache = guild_cache.write().await;
        let is_current_refresh = cache
            .refresh
            .as_ref()
            .is_some_and(|refresh| Arc::ptr_eq(&refresh.notify, &leader_notify));
        if !is_current_refresh {
            if cache.revision != observed_revision {
                return Err(StatusCode::SERVICE_UNAVAILABLE);
            }
            continue;
        }
        let mut refresh_notify = cache.refresh.take();
        if let Some(result) = cached_guild_result(&cache) {
            if let Some(notify) = refresh_notify.take() {
                notify.notify.notify_waiters();
            }
            return result;
        }
        if cache.revision != observed_revision {
            if let Some(notify) = refresh_notify.take() {
                notify.notify.notify_waiters();
            }
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        match resolved {
            Ok(guild) => {
                cache.entry = Some((
                    guild.clone(),
                    Instant::now() + Duration::from_secs(GUILD_CACHE_TTL_SECS),
                ));
                cache.failure = None;
                if let Some(notify) = refresh_notify.take() {
                    notify.notify.notify_waiters();
                }
                return Ok(guild);
            }
            Err(status) => {
                cache.failure = Some((
                    status,
                    Instant::now() + Duration::from_secs(GUILD_CACHE_FAILURE_TTL_SECS),
                ));
                if let Some(notify) = refresh_notify.take() {
                    notify.notify.notify_waiters();
                }
                return Err(status);
            }
        }
    }
}

async fn get_guild_info_with_bot_auth(
    state: &WebState,
    auth: &AuthConfig,
    bot_auth: &str,
) -> Result<DiscordGuildFull, StatusCode> {
    if !is_configured_auth_guild(state, &auth.guild_id) {
        return fetch_guild_info_with_bot_auth(state, auth, bot_auth).await;
    }
    guild_info_from_cache_with_resolver(&state.guild_cache, || async {
        fetch_guild_info_with_bot_auth(state, auth, bot_auth).await
    })
    .await
}

async fn fetch_guild_info_with_bot_auth(
    state: &WebState,
    auth: &AuthConfig,
    bot_auth: &str,
) -> Result<DiscordGuildFull, StatusCode> {
    let guild_resp = state
        .http_client
        .get(format!("https://discord.com/api/guilds/{}", auth.guild_id))
        .header("Authorization", bot_auth)
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
    let bot_auth = bot_auth_header_for_guild(state, auth).await?;

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
    let bot_auth = bot_auth_header_for_guild(state, auth).await?;
    check_guild_admin_permission_with_bot_auth(state, auth, user_id, &bot_auth, true)
        .await?
        .into_status_result()
}

async fn check_guild_admin_permission_with_bot_auth(
    state: &WebState,
    auth: &AuthConfig,
    user_id: &str,
    bot_auth: &str,
    use_cache: bool,
) -> Result<GuildAdminCheck, StatusCode> {
    let cache_key = guild_admin_permission_cache_key(&auth.guild_id, user_id);

    // Check cache first
    if use_cache {
        let cache = state.permission_cache.read().await;
        if let Some(&(permission, expires_at)) = cache.get(&cache_key)
            && Instant::now() < expires_at
        {
            return Ok(if permission.is_admin {
                GuildAdminCheck::Admin
            } else {
                GuildAdminCheck::NotAdmin
            });
        }
    }

    // Fast path: check if user is guild owner
    let guild = get_guild_info_with_bot_auth(state, auth, bot_auth).await?;
    if user_id == guild.owner_id {
        if use_cache {
            cache_guild_admin_permission(state, &auth.guild_id, user_id, true).await;
        }
        return Ok(GuildAdminCheck::Admin);
    }

    // Slow path: fetch member roles and check ADMINISTRATOR bit
    let member_resp = state
        .http_client
        .get(format!(
            "https://discord.com/api/guilds/{}/members/{user_id}",
            auth.guild_id
        ))
        .header("Authorization", bot_auth)
        .send()
        .await;

    // Handle request errors as retryable upstream failures.
    if let Err(err) = member_resp {
        warn!(error = %err, "discord member API request failed");
        return Err(StatusCode::BAD_GATEWAY);
    }

    let resp_status = member_resp.as_ref().unwrap().status();

    if let Some(decision) = guild_admin_member_status_decision(resp_status) {
        match decision {
            GuildAdminCheck::NotAdmin => {
                if use_cache {
                    cache_guild_admin_permission(state, &auth.guild_id, user_id, false).await;
                }
                return Ok(GuildAdminCheck::NotAdmin);
            }
            GuildAdminCheck::BotAccessDenied => {
                warn!(
                    status = %resp_status,
                    user_id = %user_id,
                    "discord member API denied bot token during admin check"
                );
                return Ok(GuildAdminCheck::BotAccessDenied);
            }
            GuildAdminCheck::RateLimited => {
                warn!(status = %resp_status, "discord member API rate limited");
                return Ok(GuildAdminCheck::RateLimited);
            }
            GuildAdminCheck::Admin => {}
        }
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

    if use_cache {
        cache_guild_admin_permission(state, &auth.guild_id, user_id, is_admin).await;
    }
    Ok(if is_admin {
        GuildAdminCheck::Admin
    } else {
        GuildAdminCheck::NotAdmin
    })
}

async fn check_guild_admin_permission_for_settings(
    state: &WebState,
    auth: &AuthConfig,
    user_id: &str,
) -> Result<bool, StatusCode> {
    let effective_result = match bot_auth_header_for_guild(state, auth).await {
        Ok(bot_auth) => {
            check_guild_admin_permission_with_bot_auth(state, auth, user_id, &bot_auth, true).await
        }
        Err(status) => Err(status),
    };
    if !should_retry_settings_admin_check_with_global(&effective_result) {
        return effective_result.and_then(GuildAdminCheck::into_status_result);
    }
    match &effective_result {
        Ok(GuildAdminCheck::BotAccessDenied) => warn!(
            user_id = %user_id,
            guild_id = %auth.guild_id,
            "guild-scoped bot token was denied during settings admin check; trying global token for settings recovery"
        ),
        Err(status) => warn!(
            status = %status,
            user_id = %user_id,
            guild_id = %auth.guild_id,
            "guild-scoped bot token admin check failed; trying global token for settings recovery"
        ),
        Ok(GuildAdminCheck::Admin | GuildAdminCheck::NotAdmin | GuildAdminCheck::RateLimited) => {}
    }

    let global_bot_auth = format!("Bot {}", auth.bot_token);
    check_guild_admin_permission_with_bot_auth(state, auth, user_id, &global_bot_auth, false)
        .await
        .and_then(GuildAdminCheck::into_status_result)
}

fn should_retry_settings_admin_check_with_global(
    result: &Result<GuildAdminCheck, StatusCode>,
) -> bool {
    match result {
        Ok(GuildAdminCheck::Admin | GuildAdminCheck::NotAdmin) => false,
        Ok(GuildAdminCheck::BotAccessDenied) => true,
        Ok(GuildAdminCheck::RateLimited) => false,
        Err(status) => matches!(
            *status,
            StatusCode::SERVICE_UNAVAILABLE | StatusCode::BAD_GATEWAY
        ),
    }
}

fn guild_admin_member_status_decision(status: reqwest::StatusCode) -> Option<GuildAdminCheck> {
    match status {
        reqwest::StatusCode::NOT_FOUND => Some(GuildAdminCheck::NotAdmin),
        reqwest::StatusCode::FORBIDDEN | reqwest::StatusCode::UNAUTHORIZED => {
            Some(GuildAdminCheck::BotAccessDenied)
        }
        reqwest::StatusCode::TOO_MANY_REQUESTS => Some(GuildAdminCheck::RateLimited),
        _ => None,
    }
}

fn guild_admin_permission_cache_key(guild_id: &str, user_id: &str) -> (String, String) {
    (user_id.to_owned(), format!("__guild__:{guild_id}"))
}

async fn cache_guild_admin_permission(
    state: &WebState,
    guild_id: &str,
    user_id: &str,
    is_admin: bool,
) {
    let mut cache = state.permission_cache.write().await;
    let permission = CachedChannelPermission {
        can_view: is_admin,
        is_admin,
    };
    let expires_at = Instant::now() + Duration::from_secs(permission_cache_ttl(permission));
    cache.insert(
        guild_admin_permission_cache_key(guild_id, user_id),
        (permission, expires_at),
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
    cache_channel_permission(&state.permission_cache, cache_key, permission).await;
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct CurrentUserGuildResponse {
    guild_id: String,
    name: String,
    icon: Option<String>,
    is_member: bool,
    is_admin: bool,
    tenant_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GuildMeetingsQuery {
    page: Option<u32>,
    limit: Option<u32>,
    voice_channel_id: Option<String>,
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
    voice_channel_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct GuildMeetingVoiceChannelResponse {
    id: String,
    label: String,
}

#[derive(Serialize)]
struct GuildMeetingsResponse {
    guild_id: String,
    meetings: Vec<GuildMeetingEntryResponse>,
    voice_channels: Vec<GuildMeetingVoiceChannelResponse>,
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
    discord_bot_token_registered: bool,
    discord_bot_token_updated_at: Option<String>,
    discord_bot_token_last_validated_at: Option<String>,
    discord_bot_user_id: Option<String>,
    discord_bot_username: Option<String>,
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

#[derive(Deserialize)]
struct GuildBotTokenUpdateRequest {
    bot_token: String,
}

#[derive(Debug, Deserialize)]
struct DomainKnowledgeListQuery {
    include_archived: Option<bool>,
    content_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DomainKnowledgeUpsertRequest {
    content_type: String,
    title: String,
    body: String,
    active: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct SummaryTemplateListQuery {
    include_archived: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct SummaryTemplateUpsertRequest {
    name: String,
    template: String,
    active: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct DomainKnowledgeItemResponse {
    id: String,
    content_type: String,
    title: String,
    body: String,
    active: bool,
    version: i32,
    updated_actor_user_id: Option<String>,
    archived_at: Option<String>,
    archived_actor_user_id: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SummaryTemplateResponse {
    id: String,
    name: String,
    template: String,
    active: bool,
    version: i32,
    updated_actor_user_id: Option<String>,
    archived_at: Option<String>,
    archived_actor_user_id: Option<String>,
    created_at: String,
    updated_at: String,
}

impl std::fmt::Debug for GuildBotTokenUpdateRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuildBotTokenUpdateRequest")
            .field("bot_token", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Serialize)]
struct ApiErrorResponse {
    code: &'static str,
    message: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum OperationalStatus {
    Ok,
    Unavailable,
    NotChecked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum OperationalMetricsStatus {
    Ok,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct OperationalCheck {
    status: OperationalStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

impl OperationalCheck {
    fn ok() -> Self {
        Self {
            status: OperationalStatus::Ok,
            reason: None,
        }
    }

    fn unavailable(reason: &'static str) -> Self {
        Self {
            status: OperationalStatus::Unavailable,
            reason: Some(reason),
        }
    }

    fn not_checked(reason: &'static str) -> Self {
        Self {
            status: OperationalStatus::NotChecked,
            reason: Some(reason),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct OperationalReadinessChecks {
    database: OperationalCheck,
    migrations: OperationalCheck,
    queue: OperationalCheck,
    integrations: OperationalCheck,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct OperationalReadinessResponse {
    status: &'static str,
    checks: OperationalReadinessChecks,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PublicOperationalReadinessResponse {
    status: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
struct OperationalCounters {
    failed_jobs: i64,
    running_jobs: i64,
    queued_jobs: i64,
    running_meetings: i64,
    failed_live_transcription_chunks: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct OperationalMetricsResponse {
    status: OperationalMetricsStatus,
    counters: OperationalCounters,
}

#[derive(Debug, Clone)]
struct OperationalMetricsCacheEntry {
    snapshot: OperationalMetricsResponse,
    cached_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OperationalSchemaStatus {
    meetings_ready: bool,
    jobs_ready: bool,
    live_chunks_ready: bool,
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
    discord_bot_token_registered: bool,
    discord_bot_token_updated_at: Option<String>,
    discord_bot_token_last_validated_at: Option<String>,
    discord_bot_user_id: Option<String>,
    discord_bot_username: Option<String>,
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
    id: String,
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
struct TranscriptResponse {
    segments: Vec<TranscriptSegmentResponse>,
    status: String,
    is_final: bool,
    updated_at: Option<String>,
}

#[derive(Serialize)]
struct TranscriptStateResponse {
    status: String,
    is_final: bool,
    updated_at: Option<String>,
}

#[derive(Debug, Clone)]
struct TranscriptStreamCursor {
    created_at: String,
    id: String,
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

async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn readyz(State(state): State<WebState>) -> Response {
    let checks = operational_readiness_checks(&state.db).await;
    let (status, response) = public_operational_readiness_response(checks);
    (status, Json(response)).into_response()
}

async fn metricsz(State(state): State<WebState>, headers: HeaderMap) -> Response {
    metricsz_response_with_loader(
        state.operational_metrics_bearer_token.as_deref(),
        &headers,
        &state.operational_metrics_cache,
        || load_operational_metrics(&state.db),
    )
    .await
}

async fn metricsz_response_with_loader<F, Fut>(
    expected_token: Option<&str>,
    headers: &HeaderMap,
    cache: &OperationalMetricsCache,
    loader: F,
) -> Response
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = OperationalMetricsResponse>,
{
    if let Err(status) = authorize_operational_metrics_request(expected_token, headers) {
        return metrics_auth_failure_response(status);
    }

    let snapshot = load_cached_operational_metrics_with(cache, loader).await;
    let status = match snapshot.status {
        OperationalMetricsStatus::Ok => StatusCode::OK,
        OperationalMetricsStatus::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
    };
    (status, Json(snapshot)).into_response()
}

fn authorize_operational_metrics_request(
    expected_token: Option<&str>,
    headers: &HeaderMap,
) -> Result<(), StatusCode> {
    let Some(expected) = expected_token else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let Some(actual) = bearer_token_from_headers(headers) else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    if bearer_tokens_match(actual, expected) {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

fn bearer_token_from_headers(headers: &HeaderMap) -> Option<&str> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = raw.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") {
        let token = token.trim();
        if !token.is_empty() {
            return Some(token);
        }
    }
    None
}

fn bearer_tokens_match(actual: &str, expected: &str) -> bool {
    let actual_hash = Sha256::digest(actual.as_bytes());
    let expected_hash = Sha256::digest(expected.as_bytes());
    constant_time_eq(&actual_hash, &expected_hash)
}

fn metrics_auth_failure_response(status: StatusCode) -> Response {
    let mut response = status.into_response();
    if status == StatusCode::UNAUTHORIZED {
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            header::HeaderValue::from_static(r#"Bearer realm="metricsz""#),
        );
    }
    response
}

async fn load_cached_operational_metrics_with<F, Fut>(
    cache: &OperationalMetricsCache,
    loader: F,
) -> OperationalMetricsResponse
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = OperationalMetricsResponse>,
{
    let now = Instant::now();
    let mut guard = cache.lock().await;
    if let Some(entry) = guard.as_ref()
        && now.duration_since(entry.cached_at).as_secs() < OPERATIONAL_METRICS_CACHE_TTL_SECS
    {
        return entry.snapshot.clone();
    }

    let snapshot = loader().await;
    *guard = Some(OperationalMetricsCacheEntry {
        snapshot: snapshot.clone(),
        cached_at: now,
    });
    snapshot
}

async fn load_operational_metrics(db: &PgClient) -> OperationalMetricsResponse {
    match load_operational_counters(db).await {
        Ok(counters) => OperationalMetricsResponse {
            status: OperationalMetricsStatus::Ok,
            counters,
        },
        Err(err) => {
            warn!(error = %err, "failed to load operational counters");
            OperationalMetricsResponse {
                status: OperationalMetricsStatus::Unavailable,
                counters: OperationalCounters::default(),
            }
        }
    }
}

async fn operational_readiness_checks(db: &PgClient) -> OperationalReadinessChecks {
    let database = match db.query_one("SELECT 1", &[]).await {
        Ok(_) => OperationalCheck::ok(),
        Err(err) => {
            warn!(error = %err, "database readiness check failed");
            return OperationalReadinessChecks {
                database: OperationalCheck::unavailable("database query failed"),
                migrations: OperationalCheck::unavailable("database unavailable"),
                queue: OperationalCheck::unavailable("database unavailable"),
                integrations: integration_readiness_not_checked(),
            };
        }
    };

    let schema = match load_operational_schema_status(db).await {
        Ok(schema) => schema,
        Err(err) => {
            warn!(error = %err, "schema readiness check failed");
            return OperationalReadinessChecks {
                database,
                migrations: OperationalCheck::unavailable("schema readiness query failed"),
                queue: OperationalCheck::unavailable("schema readiness query failed"),
                integrations: integration_readiness_not_checked(),
            };
        }
    };

    let migrations = if schema.meetings_ready && schema.jobs_ready && schema.live_chunks_ready {
        OperationalCheck::ok()
    } else {
        OperationalCheck::unavailable("required database tables are missing")
    };
    let queue = if schema.jobs_ready {
        OperationalCheck::ok()
    } else {
        OperationalCheck::unavailable("jobs table is missing")
    };

    OperationalReadinessChecks {
        database,
        migrations,
        queue,
        integrations: integration_readiness_not_checked(),
    }
}

#[cfg(test)]
fn operational_readiness_response(
    checks: OperationalReadinessChecks,
) -> (StatusCode, OperationalReadinessResponse) {
    let (http_status, public) = public_operational_readiness_response(checks.clone());
    (
        http_status,
        OperationalReadinessResponse {
            status: public.status,
            checks,
        },
    )
}

fn public_operational_readiness_response(
    checks: OperationalReadinessChecks,
) -> (StatusCode, PublicOperationalReadinessResponse) {
    let ready = matches!(checks.database.status, OperationalStatus::Ok)
        && matches!(checks.migrations.status, OperationalStatus::Ok)
        && matches!(checks.queue.status, OperationalStatus::Ok);
    let status = if ready { "ready" } else { "not_ready" };
    let http_status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (http_status, PublicOperationalReadinessResponse { status })
}

fn integration_readiness_not_checked() -> OperationalCheck {
    OperationalCheck::not_checked(
        "runtime integration state is not shared with the web server in this operational slice",
    )
}

async fn load_operational_schema_status(
    db: &PgClient,
) -> Result<OperationalSchemaStatus, tokio_postgres::Error> {
    let row = db.query_one(OPERATIONAL_SCHEMA_READY_SQL, &[]).await?;
    Ok(OperationalSchemaStatus {
        meetings_ready: row.get("meetings_ready"),
        jobs_ready: row.get("jobs_ready"),
        live_chunks_ready: row.get("live_chunks_ready"),
    })
}

async fn load_operational_counters(
    db: &PgClient,
) -> Result<OperationalCounters, tokio_postgres::Error> {
    let row = db.query_one(OPERATIONAL_COUNTERS_SQL, &[]).await?;
    Ok(OperationalCounters {
        failed_jobs: row.get("failed_jobs"),
        running_jobs: row.get("running_jobs"),
        queued_jobs: row.get("queued_jobs"),
        running_meetings: row.get("running_meetings"),
        failed_live_transcription_chunks: row.get("failed_live_transcription_chunks"),
    })
}

fn normalize_guild_meetings_pagination(query: &GuildMeetingsQuery) -> (u32, u32) {
    let page = query.page.unwrap_or(1).max(1);
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    (page, limit)
}

fn normalize_guild_meetings_voice_channel_id(query: &GuildMeetingsQuery) -> Option<String> {
    query
        .voice_channel_id
        .as_deref()
        .map(str::trim)
        .filter(|voice_channel_id| !voice_channel_id.is_empty())
        .map(str::to_owned)
}

fn guild_meetings_visibility_scan_limit(page: u32, limit: u32) -> i64 {
    let requested = u64::from(page.saturating_sub(1))
        .saturating_mul(u64::from(limit))
        .saturating_add(u64::from(limit));
    i64::from(
        requested
            .max(u64::from(limit))
            .max(u64::from(GUILD_MEETINGS_VISIBILITY_SCAN_MIN))
            .min(u64::from(GUILD_MEETINGS_VISIBILITY_SCAN_LIMIT)) as u32,
    )
}

fn can_scan_new_visibility_channel(known_channel_count: usize) -> bool {
    known_channel_count < GUILD_MEETINGS_VISIBILITY_CHANNEL_CAP
}

fn guild_meetings_response_total(is_admin: bool, raw_total: i64, visible_count: usize) -> i64 {
    if is_admin {
        raw_total
    } else {
        i64::try_from(visible_count).unwrap_or(i64::MAX)
    }
}

fn user_can_access_target_guild(discord_guilds: &[DiscordGuild], guild_id: &str) -> bool {
    discord_guilds.iter().any(|guild| guild.id == guild_id)
}

fn target_guild_has_active_installation(
    tenant_by_guild_id: &HashMap<String, String>,
    guild_id: &str,
) -> bool {
    tenant_by_guild_id.contains_key(guild_id)
}

fn next_transcript_sse_poll_delay(current: Duration, had_segments: bool) -> Duration {
    if had_segments {
        return Duration::from_secs(TRANSCRIPT_SSE_BASE_POLL_SECS);
    }
    let current_secs = current.as_secs();
    let next_secs = if current_secs == 0 {
        TRANSCRIPT_SSE_BASE_POLL_SECS
    } else {
        current_secs.saturating_mul(2)
    };
    Duration::from_secs(next_secs.min(TRANSCRIPT_SSE_MAX_POLL_SECS))
}

fn next_transcript_sse_idle_polls(current: u32, had_segments: bool) -> u32 {
    if had_segments {
        0
    } else {
        current.saturating_add(1)
    }
}

fn transcript_sse_idle_limit_reached(idle_polls: u32) -> bool {
    idle_polls >= TRANSCRIPT_SSE_MAX_IDLE_POLLS
}

fn guild_meeting_entry_from_row(row: &tokio_postgres::Row) -> GuildMeetingEntryResponse {
    GuildMeetingEntryResponse {
        id: row.get("id"),
        title: row.get("title"),
        status: row.get("status"),
        started_at: row.get("started_at"),
        stopped_at: row.get("stopped_at"),
        duration_seconds: row.get("meeting_duration_seconds"),
        stop_reason: row.get("stop_reason"),
        voice_channel_id: row.get("voice_channel_id"),
    }
}

fn guild_meeting_voice_channel_response(id: String) -> GuildMeetingVoiceChannelResponse {
    GuildMeetingVoiceChannelResponse {
        label: format!("VC ID: {id}"),
        id,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NormalizedDomainKnowledgeRequest {
    content_type: DomainKnowledgeContentType,
    title: String,
    body: String,
    active: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NormalizedSummaryTemplateRequest {
    name: String,
    template: String,
    active: Option<bool>,
    variables: Vec<String>,
}

fn validate_domain_knowledge_item_id(id: &str) -> Result<(), StatusCode> {
    if id.trim().is_empty() || id.len() > 128 {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

fn validate_summary_template_id(id: &str) -> Result<(), StatusCode> {
    if id.trim().is_empty() || id.len() > 128 {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

fn parse_domain_knowledge_content_type(
    content_type: &str,
) -> Result<DomainKnowledgeContentType, StatusCode> {
    DomainKnowledgeContentType::parse_str(content_type).ok_or(StatusCode::BAD_REQUEST)
}

fn normalize_domain_knowledge_request(
    request: &DomainKnowledgeUpsertRequest,
) -> Result<NormalizedDomainKnowledgeRequest, StatusCode> {
    let content_type = parse_domain_knowledge_content_type(&request.content_type)?;
    let title = request.title.trim();
    let body = request.body.trim();
    if title.is_empty() || title.len() > 200 || body.is_empty() || body.len() > 20_000 {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(NormalizedDomainKnowledgeRequest {
        content_type,
        title: title.to_owned(),
        body: body.to_owned(),
        active: request.active,
    })
}

fn normalize_domain_knowledge_list_filter(
    query: &DomainKnowledgeListQuery,
) -> Result<(bool, String), StatusCode> {
    let content_type = query
        .content_type
        .as_deref()
        .map(parse_domain_knowledge_content_type)
        .transpose()?
        .map(|content_type| content_type.as_str().to_owned())
        .unwrap_or_default();
    Ok((query.include_archived.unwrap_or(false), content_type))
}

fn normalize_summary_template_request(
    request: &SummaryTemplateUpsertRequest,
) -> Result<NormalizedSummaryTemplateRequest, StatusCode> {
    let name = request.name.trim();
    let template = request.template.trim();
    if name.is_empty() || name.len() > 200 {
        return Err(StatusCode::BAD_REQUEST);
    }
    validate_summary_template(template).map_err(|_| StatusCode::BAD_REQUEST)?;
    let variables = summary_template_variables(template).map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(NormalizedSummaryTemplateRequest {
        name: name.to_owned(),
        template: template.to_owned(),
        active: request.active,
        variables,
    })
}

fn validate_authorized_summary_template_request(
    is_admin: bool,
    request: &SummaryTemplateUpsertRequest,
) -> Result<NormalizedSummaryTemplateRequest, StatusCode> {
    guild_admin_required_result(is_admin)?;
    normalize_summary_template_request(request)
}

fn domain_knowledge_response_from_row(row: &tokio_postgres::Row) -> DomainKnowledgeItemResponse {
    DomainKnowledgeItemResponse {
        id: row.get("id"),
        content_type: row.get("content_type"),
        title: row.get("title"),
        body: row.get("body"),
        active: row.get("active"),
        version: row.get("version"),
        updated_actor_user_id: row.get("updated_actor_user_id"),
        archived_at: row.get("archived_at"),
        archived_actor_user_id: row.get("archived_actor_user_id"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

fn summary_template_response_from_row(row: &tokio_postgres::Row) -> SummaryTemplateResponse {
    SummaryTemplateResponse {
        id: row.get("id"),
        name: row.get("name"),
        template: row.get("template"),
        active: row.get("active"),
        version: row.get("version"),
        updated_actor_user_id: row.get("updated_actor_user_id"),
        archived_at: row.get("archived_at"),
        archived_actor_user_id: row.get("archived_actor_user_id"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

fn summary_template_mutation_status(err: &tokio_postgres::Error) -> StatusCode {
    if err.code() == Some(&SqlState::UNIQUE_VIOLATION) {
        StatusCode::CONFLICT
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
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

fn normalize_guild_bot_token_update(
    request: &GuildBotTokenUpdateRequest,
) -> Result<String, StatusCode> {
    let token = request.bot_token.trim();
    if token.is_empty() || token.len() > 4096 {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(token.to_owned())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiscordBotTokenValidationStage {
    User,
    Guild,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DiscordBotTokenValidationError {
    InvalidToken,
    NotBotToken,
    GuildAccessDenied,
    Upstream,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ValidatedDiscordBotToken {
    bot_user_id: String,
    bot_username: String,
}

#[derive(Debug, Deserialize)]
struct DiscordBotSelfResponse {
    id: String,
    username: String,
    bot: Option<bool>,
}

fn classify_discord_bot_token_validation_status(
    stage: DiscordBotTokenValidationStage,
    status: reqwest::StatusCode,
) -> Option<DiscordBotTokenValidationError> {
    if status.is_success() {
        return None;
    }
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Some(DiscordBotTokenValidationError::InvalidToken);
    }
    match stage {
        DiscordBotTokenValidationStage::User => Some(DiscordBotTokenValidationError::Upstream),
        DiscordBotTokenValidationStage::Guild => {
            if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::NOT_FOUND
            {
                Some(DiscordBotTokenValidationError::GuildAccessDenied)
            } else {
                Some(DiscordBotTokenValidationError::Upstream)
            }
        }
    }
}

fn bot_token_validation_error_response(error: DiscordBotTokenValidationError) -> Response {
    match error {
        DiscordBotTokenValidationError::InvalidToken => api_error_response(
            StatusCode::BAD_REQUEST,
            "invalid_bot_token",
            "Discord bot token is invalid.",
        ),
        DiscordBotTokenValidationError::NotBotToken => api_error_response(
            StatusCode::BAD_REQUEST,
            "not_bot_token",
            "Discord token must belong to a bot user.",
        ),
        DiscordBotTokenValidationError::GuildAccessDenied => api_error_response(
            StatusCode::FORBIDDEN,
            "bot_token_guild_access_denied",
            "Discord bot token cannot access this guild.",
        ),
        DiscordBotTokenValidationError::Upstream => api_error_response(
            StatusCode::BAD_GATEWAY,
            "discord_validation_failed",
            "Discord bot token validation failed.",
        ),
    }
}

fn api_error_response(status: StatusCode, code: &'static str, message: &'static str) -> Response {
    (status, Json(ApiErrorResponse { code, message })).into_response()
}

async fn validate_discord_bot_token_for_guild(
    state: &WebState,
    guild_id: &str,
    token: &str,
) -> Result<ValidatedDiscordBotToken, DiscordBotTokenValidationError> {
    let bot_auth = format!("Bot {token}");
    let user_response = state
        .http_client
        .get("https://discord.com/api/users/@me")
        .header("Authorization", &bot_auth)
        .send()
        .await
        .map_err(|err| {
            warn!(error = %err, "discord bot token validation user request failed");
            DiscordBotTokenValidationError::Upstream
        })?;
    let user_status = user_response.status();
    if let Some(error) = classify_discord_bot_token_validation_status(
        DiscordBotTokenValidationStage::User,
        user_status,
    ) {
        warn!(
            status = %user_status,
            "discord bot token validation user request returned non-success"
        );
        return Err(error);
    }
    let bot_user: DiscordBotSelfResponse = user_response.json().await.map_err(|err| {
        warn!(error = %err, "discord bot token validation user response parse failed");
        DiscordBotTokenValidationError::Upstream
    })?;
    if bot_user.bot != Some(true) {
        return Err(DiscordBotTokenValidationError::NotBotToken);
    }

    let guild_response = state
        .http_client
        .get(format!("https://discord.com/api/guilds/{guild_id}"))
        .header("Authorization", &bot_auth)
        .send()
        .await
        .map_err(|err| {
            warn!(error = %err, guild_id = %guild_id, "discord bot token validation guild request failed");
            DiscordBotTokenValidationError::Upstream
        })?;
    let guild_status = guild_response.status();
    if let Some(error) = classify_discord_bot_token_validation_status(
        DiscordBotTokenValidationStage::Guild,
        guild_status,
    ) {
        warn!(
            status = %guild_status,
            guild_id = %guild_id,
            "discord bot token validation guild request returned non-success"
        );
        return Err(error);
    }

    Ok(ValidatedDiscordBotToken {
        bot_user_id: bot_user.id,
        bot_username: bot_user.username,
    })
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
        discord_bot_token_registered: false,
        discord_bot_token_updated_at: None,
        discord_bot_token_last_validated_at: None,
        discord_bot_user_id: None,
        discord_bot_username: None,
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
        summary_enabled: stored.summary_enabled.unwrap_or(defaults.summary_enabled),
        discord_bot_token_registered: stored.discord_bot_token_registered,
        discord_bot_token_updated_at: stored.discord_bot_token_updated_at,
        discord_bot_token_last_validated_at: stored.discord_bot_token_last_validated_at,
        discord_bot_user_id: stored.discord_bot_user_id,
        discord_bot_username: stored.discord_bot_username,
        is_admin,
    }
}

fn guild_bot_token_delete_is_noop(stored: Option<&StoredGuildSettings>) -> bool {
    stored.is_none_or(|settings| !settings.discord_bot_token_registered)
}

async fn current_user_is_guild_admin(state: &WebState, user_id: &str) -> Result<bool, StatusCode> {
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    check_guild_admin_permission_for_settings(state, auth, user_id).await
}

fn guild_admin_required_result(is_admin: bool) -> Result<(), StatusCode> {
    if is_admin {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

async fn require_current_user_is_guild_admin(
    state: &WebState,
    user_id: &str,
) -> Result<(), StatusCode> {
    guild_admin_required_result(current_user_is_guild_admin(state, user_id).await?)
}

fn validate_authorized_guild_settings_update(
    is_admin: bool,
    request: &GuildSettingsUpdateRequest,
) -> Result<(), StatusCode> {
    guild_admin_required_result(is_admin)?;
    validate_guild_settings_update(request)
}

fn validate_authorized_guild_bot_token_update(
    is_admin: bool,
    request: &GuildBotTokenUpdateRequest,
) -> Result<String, StatusCode> {
    guild_admin_required_result(is_admin)?;
    normalize_guild_bot_token_update(request)
}

fn oauth_access_token_expires_at(expires_in: Option<u64>) -> Instant {
    let ttl = expires_in
        .unwrap_or(OAUTH_ACCESS_TOKEN_DEFAULT_TTL_SECS)
        .saturating_sub(OAUTH_ACCESS_TOKEN_CLOCK_SKEW_SECS)
        .max(1);
    Instant::now() + Duration::from_secs(ttl)
}

async fn cache_user_discord_guilds(
    cache: &UserGuildsCache,
    user_id: &str,
    bearer: String,
    guilds: Vec<DiscordGuild>,
    token_expires_at: Instant,
) {
    let mut cache = cache.write().await;
    cache.insert(
        user_id.to_owned(),
        UserGuildsCacheEntry {
            bearer,
            token_expires_at,
            guilds,
            guilds_expires_at: Instant::now() + Duration::from_secs(USER_GUILDS_CACHE_TTL_SECS),
        },
    );
}

enum UserGuildsCacheLookup {
    Guilds(Vec<DiscordGuild>),
    Bearer(String),
    Missing,
}

async fn user_guilds_cache_lookup(cache: &UserGuildsCache, user_id: &str) -> UserGuildsCacheLookup {
    let now = Instant::now();
    let cache = cache.read().await;
    let Some(entry) = cache.get(user_id) else {
        return UserGuildsCacheLookup::Missing;
    };
    if now < entry.guilds_expires_at {
        return UserGuildsCacheLookup::Guilds(entry.guilds.clone());
    }
    if now < entry.token_expires_at {
        return UserGuildsCacheLookup::Bearer(entry.bearer.clone());
    }
    UserGuildsCacheLookup::Missing
}

async fn refresh_cached_user_discord_guilds(
    cache: &UserGuildsCache,
    user_id: &str,
    guilds: Vec<DiscordGuild>,
) {
    let mut cache = cache.write().await;
    let Some(entry) = cache.get_mut(user_id) else {
        return;
    };
    entry.guilds = guilds;
    entry.guilds_expires_at = Instant::now() + Duration::from_secs(USER_GUILDS_CACHE_TTL_SECS);
}

fn discord_user_guilds_api_status(status: reqwest::StatusCode) -> Result<(), StatusCode> {
    if status.is_success() {
        return Ok(());
    }
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(StatusCode::FORBIDDEN);
    }
    Err(StatusCode::BAD_GATEWAY)
}

async fn fetch_discord_user_guilds(
    http_client: &reqwest::Client,
    bearer: &str,
) -> Result<Vec<DiscordGuild>, StatusCode> {
    let response = http_client
        .get("https://discord.com/api/users/@me/guilds")
        .header("Authorization", bearer)
        .send()
        .await
        .map_err(|err| {
            warn!(error = %err, "discord current-user guilds request failed");
            StatusCode::BAD_GATEWAY
        })?;
    let status = response.status();
    discord_user_guilds_api_status(status)?;
    let body = response.text().await.map_err(|err| {
        warn!(error = %err, "discord current-user guilds response read failed");
        StatusCode::BAD_GATEWAY
    })?;
    serde_json::from_str(&body).map_err(|err| {
        warn!(
            error = %err,
            status = %status,
            body_len = body.len(),
            "discord current-user guilds response parse failed"
        );
        StatusCode::BAD_GATEWAY
    })
}

async fn load_current_user_discord_guilds(
    state: &WebState,
    user_id: &str,
) -> Result<Vec<DiscordGuild>, StatusCode> {
    match user_guilds_cache_lookup(&state.user_guilds_cache, user_id).await {
        UserGuildsCacheLookup::Guilds(guilds) => Ok(guilds),
        UserGuildsCacheLookup::Bearer(bearer) => {
            let guilds = fetch_discord_user_guilds(&state.http_client, &bearer).await?;
            refresh_cached_user_discord_guilds(&state.user_guilds_cache, user_id, guilds.clone())
                .await;
            Ok(guilds)
        }
        UserGuildsCacheLookup::Missing => Err(StatusCode::SERVICE_UNAVAILABLE),
    }
}

async fn list_active_tenant_guilds_for_visible_ids(
    state: &WebState,
    guild_ids: &[String],
) -> Result<HashMap<String, String>, StatusCode> {
    if guild_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let guild_ids = guild_ids.to_vec();
    let rows = state
        .db
        .query(LIST_ACTIVE_TENANT_GUILDS_BY_GUILD_IDS_SQL, &[&guild_ids])
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut tenants = HashMap::new();
    for row in rows {
        tenants.insert(row.get("guild_id"), row.get("tenant_id"));
    }
    Ok(tenants)
}

fn discord_guild_is_admin(guild: &DiscordGuild) -> bool {
    guild.owner || guild.permissions & ADMINISTRATOR != 0
}

fn current_user_guilds_response(
    discord_guilds: &[DiscordGuild],
    tenant_by_guild_id: &HashMap<String, String>,
) -> Vec<CurrentUserGuildResponse> {
    let mut seen = HashSet::new();
    let mut guilds = Vec::new();
    for guild in discord_guilds {
        let guild_id = guild.id.trim();
        let name = guild.name.trim();
        if guild_id.is_empty() || name.is_empty() || !seen.insert(guild_id.to_owned()) {
            continue;
        }
        guilds.push(CurrentUserGuildResponse {
            guild_id: guild_id.to_owned(),
            name: name.to_owned(),
            icon: guild
                .icon
                .as_deref()
                .map(str::trim)
                .filter(|icon| !icon.is_empty())
                .map(str::to_owned),
            is_member: true,
            is_admin: discord_guild_is_admin(guild),
            tenant_id: tenant_by_guild_id.get(guild_id).cloned(),
        });
    }
    guilds.sort_by(|a, b| {
        b.tenant_id
            .is_some()
            .cmp(&a.tenant_id.is_some())
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.guild_id.cmp(&b.guild_id))
    });
    guilds
}

async fn api_me(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
) -> Result<Json<CurrentUserResponse>, StatusCode> {
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let is_admin = check_guild_admin_permission_for_settings(&state, auth, &user_id).await?;
    Ok(Json(CurrentUserResponse {
        user_id,
        guild_id: auth.guild_id.clone(),
        is_admin,
    }))
}

async fn api_me_guilds(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
) -> Result<Json<Vec<CurrentUserGuildResponse>>, StatusCode> {
    let discord_guilds = load_current_user_discord_guilds(&state, &user_id).await?;
    let visible_guild_ids = discord_guilds
        .iter()
        .map(|guild| guild.id.trim())
        .filter(|guild_id| !guild_id.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let tenant_by_guild_id =
        list_active_tenant_guilds_for_visible_ids(&state, &visible_guild_ids).await?;
    Ok(Json(current_user_guilds_response(
        &discord_guilds,
        &tenant_by_guild_id,
    )))
}

async fn api_guild_meetings(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Query(query): Query<GuildMeetingsQuery>,
) -> Result<Json<GuildMeetingsResponse>, StatusCode> {
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    list_guild_meetings_for_auth(&state, &user_id, auth, query).await
}

async fn api_target_guild_meetings(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(guild_id): Path<String>,
    Query(query): Query<GuildMeetingsQuery>,
) -> Result<Json<GuildMeetingsResponse>, StatusCode> {
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let guild_id = normalize_target_guild_id(&guild_id)?;
    require_active_target_guild_installation(&state, &guild_id).await?;
    let discord_guilds = load_current_user_discord_guilds(&state, &user_id).await?;
    if !user_can_access_target_guild(&discord_guilds, &guild_id) {
        return Err(StatusCode::FORBIDDEN);
    }
    let target_auth = target_auth_config(auth, &guild_id);
    list_guild_meetings_for_auth(&state, &user_id, &target_auth, query).await
}

async fn list_guild_meetings_for_auth(
    state: &WebState,
    user_id: &str,
    auth: &AuthConfig,
    query: GuildMeetingsQuery,
) -> Result<Json<GuildMeetingsResponse>, StatusCode> {
    let (page, limit) = normalize_guild_meetings_pagination(&query);
    let voice_channel_filter = normalize_guild_meetings_voice_channel_id(&query);
    let offset = i64::from(page.saturating_sub(1)) * i64::from(limit);
    let limit_i64 = i64::from(limit);
    let is_admin = check_guild_admin_permission(state, auth, user_id).await?;
    let (voice_channels, mut channel_visibility) =
        list_guild_meeting_voice_channels(state, auth, user_id, is_admin).await?;

    if is_admin {
        let count_row = state
            .db
            .query_one(
                COUNT_GUILD_MEETINGS_SQL,
                &[&auth.guild_id, &voice_channel_filter],
            )
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let total: i64 = count_row.get(0);

        let rows = state
            .db
            .query(
                LIST_GUILD_MEETINGS_SQL,
                &[&auth.guild_id, &voice_channel_filter, &limit_i64, &offset],
            )
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let meetings = rows.iter().map(guild_meeting_entry_from_row).collect();

        return Ok(Json(GuildMeetingsResponse {
            guild_id: auth.guild_id.clone(),
            meetings,
            voice_channels,
            page,
            limit,
            total,
        }));
    }

    let visibility_scan_limit = guild_meetings_visibility_scan_limit(page, limit);
    let rows = state
        .db
        .query(
            LIST_GUILD_MEETINGS_FOR_VISIBILITY_SQL,
            &[
                &auth.guild_id,
                &voice_channel_filter,
                &visibility_scan_limit,
            ],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut visible_meetings = Vec::new();

    for row in &rows {
        let row_voice_channel_id: String = row.get("voice_channel_id");
        let visible = if let Some(visible) = channel_visibility.get(&row_voice_channel_id) {
            *visible
        } else {
            let filter_targets_row =
                voice_channel_filter.as_deref() == Some(row_voice_channel_id.as_str());
            if !filter_targets_row && !can_scan_new_visibility_channel(channel_visibility.len()) {
                continue;
            }
            let visible = guild_meeting_channel_visible_after_row(
                auth.guild_id.clone(),
                row_voice_channel_id.clone(),
                &auth.guild_id,
                user_id,
                &state.permission_cache,
                resolve_channel_permission_flags(state, auth, &row_voice_channel_id, user_id),
            )
            .await?;
            channel_visibility.insert(row_voice_channel_id.clone(), visible);
            visible
        };

        if visible {
            visible_meetings.push(guild_meeting_entry_from_row(row));
        }
    }

    // For non-admins we filter before pagination so hidden rows cannot create
    // sparse pages or leak the guild-wide count. The scan and unique-channel
    // caps keep Discord permission work bounded until permission-aware
    // persistence exists.
    let total = guild_meetings_response_total(false, 0, visible_meetings.len());
    let meetings = visible_meetings
        .into_iter()
        .skip(usize::try_from(offset).unwrap_or(usize::MAX))
        .take(usize::try_from(limit).unwrap_or(usize::MAX))
        .collect();

    Ok(Json(GuildMeetingsResponse {
        guild_id: auth.guild_id.clone(),
        meetings,
        voice_channels,
        page,
        limit,
        total,
    }))
}

async fn list_guild_meeting_voice_channels(
    state: &WebState,
    auth: &AuthConfig,
    user_id: &str,
    is_admin: bool,
) -> Result<(Vec<GuildMeetingVoiceChannelResponse>, HashMap<String, bool>), StatusCode> {
    let channel_limit = i64::try_from(GUILD_MEETINGS_VISIBILITY_CHANNEL_CAP).unwrap_or(i64::MAX);
    let rows = state
        .db
        .query(
            LIST_GUILD_MEETING_VOICE_CHANNELS_SQL,
            &[&auth.guild_id, &channel_limit],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut voice_channels = Vec::new();
    let mut channel_visibility = HashMap::new();

    for row in rows {
        let voice_channel_id: String = row.get("voice_channel_id");
        if is_admin {
            channel_visibility.insert(voice_channel_id.clone(), true);
            voice_channels.push(guild_meeting_voice_channel_response(voice_channel_id));
            continue;
        }

        let visible = guild_meeting_channel_visible_after_row(
            auth.guild_id.clone(),
            voice_channel_id.clone(),
            &auth.guild_id,
            user_id,
            &state.permission_cache,
            resolve_channel_permission_flags(state, auth, &voice_channel_id, user_id),
        )
        .await?;
        channel_visibility.insert(voice_channel_id.clone(), visible);
        if visible {
            voice_channels.push(guild_meeting_voice_channel_response(voice_channel_id));
        }
    }

    Ok((voice_channels, channel_visibility))
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
        discord_bot_token_registered: row.get("discord_bot_token_registered"),
        discord_bot_token_updated_at: row.get("bot_token_updated_at"),
        discord_bot_token_last_validated_at: row.get("bot_token_last_validated_at"),
        discord_bot_user_id: row.get("bot_user_id"),
        discord_bot_username: row.get("bot_username"),
    }))
}

fn normalize_target_guild_id(guild_id: &str) -> Result<String, StatusCode> {
    let trimmed = guild_id.trim();
    if trimmed.is_empty()
        || trimmed.len() > 128
        || trimmed.contains('/')
        || trimmed.chars().any(char::is_control)
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(trimmed.to_owned())
}

fn target_auth_config(auth: &AuthConfig, guild_id: &str) -> AuthConfig {
    let mut target = auth.clone();
    target.guild_id = guild_id.to_owned();
    target
}

fn target_guild_settings_path(guild_id: &str, suffix: &str) -> String {
    format!("/api/guilds/{guild_id}/settings{suffix}")
}

async fn require_active_target_guild_installation(
    state: &WebState,
    guild_id: &str,
) -> Result<(), StatusCode> {
    let tenant_by_guild_id =
        list_active_tenant_guilds_for_visible_ids(state, &[guild_id.to_owned()]).await?;
    let installed = target_guild_has_active_installation(&tenant_by_guild_id, guild_id);
    if installed {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

async fn require_user_is_target_guild_admin(
    state: &WebState,
    auth: &AuthConfig,
    user_id: &str,
    guild_id: &str,
) -> Result<AuthConfig, StatusCode> {
    require_active_target_guild_installation(state, guild_id).await?;
    let target_auth = target_auth_config(auth, guild_id);
    let is_admin = check_guild_admin_permission_for_settings(state, &target_auth, user_id).await?;
    guild_admin_required_result(is_admin)?;
    Ok(target_auth)
}

async fn api_target_guild_settings(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(guild_id): Path<String>,
) -> Result<Json<GuildSettingsResponse>, StatusCode> {
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let guild_id = normalize_target_guild_id(&guild_id)?;
    let target_auth = require_user_is_target_guild_admin(&state, auth, &user_id, &guild_id).await?;
    let stored = load_guild_settings(&state, &target_auth.guild_id).await?;

    Ok(Json(guild_settings_response(
        &state.guild_settings_defaults,
        stored,
        true,
    )))
}

async fn api_guild_settings(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
) -> Result<Json<GuildSettingsResponse>, StatusCode> {
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    require_current_user_is_guild_admin(&state, &user_id).await?;
    let stored = load_guild_settings(&state, &auth.guild_id).await?;

    Ok(Json(guild_settings_response(
        &state.guild_settings_defaults,
        stored,
        true,
    )))
}

async fn api_update_target_guild_settings(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(guild_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<GuildSettingsUpdateRequest>,
) -> Result<Json<GuildSettingsResponse>, StatusCode> {
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let guild_id = normalize_target_guild_id(&guild_id)?;
    let target_auth = require_user_is_target_guild_admin(&state, auth, &user_id, &guild_id).await?;
    validate_guild_settings_update(&request)?;

    let whisper_language_explicit = request.whisper_language.is_some();
    state
        .db
        .execute(
            UPSERT_GUILD_SETTINGS_SQL,
            &[
                &target_auth.guild_id,
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
    record_audit_event(
        &state,
        web_audit_event(
            Some(target_auth.guild_id.clone()),
            Some(user_id.clone()),
            "guild_settings.update",
            "guild_settings",
            Some(target_auth.guild_id.clone()),
            audit_request_metadata(
                &headers,
                "PUT",
                &target_guild_settings_path(&target_auth.guild_id, ""),
            ),
            json!({
                "whisper_language_set": request.whisper_language.is_some(),
                "whisper_vad": request.whisper_vad,
                "auto_stop_grace_seconds": request.auto_stop_grace_seconds,
                "retention_raw_audio_ttl_days": request.retention_raw_audio_ttl_days,
                "retention_transcript_ttl_days": request.retention_transcript_ttl_days,
                "summary_enabled": request.summary_enabled,
            }),
        ),
    )
    .await;

    let stored = load_guild_settings(&state, &target_auth.guild_id).await?;
    Ok(Json(guild_settings_response(
        &state.guild_settings_defaults,
        stored,
        true,
    )))
}

async fn api_update_guild_settings(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    headers: HeaderMap,
    Json(request): Json<GuildSettingsUpdateRequest>,
) -> Result<Json<GuildSettingsResponse>, StatusCode> {
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let is_admin = current_user_is_guild_admin(&state, &user_id).await?;
    validate_authorized_guild_settings_update(is_admin, &request)?;

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
    record_audit_event(
        &state,
        web_audit_event(
            Some(auth.guild_id.clone()),
            Some(user_id.clone()),
            "guild_settings.update",
            "guild_settings",
            Some(auth.guild_id.clone()),
            audit_request_metadata(&headers, "PUT", "/api/guild/settings"),
            json!({
                "whisper_language_set": request.whisper_language.is_some(),
                "whisper_vad": request.whisper_vad,
                "auto_stop_grace_seconds": request.auto_stop_grace_seconds,
                "retention_raw_audio_ttl_days": request.retention_raw_audio_ttl_days,
                "retention_transcript_ttl_days": request.retention_transcript_ttl_days,
                "summary_enabled": request.summary_enabled,
            }),
        ),
    )
    .await;

    let stored = load_guild_settings(&state, &auth.guild_id).await?;
    Ok(Json(guild_settings_response(
        &state.guild_settings_defaults,
        stored,
        true,
    )))
}

async fn api_list_domain_knowledge(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Query(query): Query<DomainKnowledgeListQuery>,
) -> Result<Json<Vec<DomainKnowledgeItemResponse>>, StatusCode> {
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    require_current_user_is_guild_admin(&state, &user_id).await?;
    let (include_archived, content_type_filter) = normalize_domain_knowledge_list_filter(&query)?;
    let include_archived_text = include_archived.to_string();
    let rows = state
        .db
        .query(
            LIST_DOMAIN_KNOWLEDGE_SQL,
            &[&auth.guild_id, &include_archived_text, &content_type_filter],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(
        rows.iter()
            .map(domain_knowledge_response_from_row)
            .collect(),
    ))
}

async fn api_get_domain_knowledge(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(item_id): Path<String>,
) -> Result<Json<DomainKnowledgeItemResponse>, StatusCode> {
    validate_domain_knowledge_item_id(&item_id)?;
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    require_current_user_is_guild_admin(&state, &user_id).await?;
    let row = state
        .db
        .query_opt(GET_DOMAIN_KNOWLEDGE_SQL, &[&auth.guild_id, &item_id])
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(domain_knowledge_response_from_row(&row)))
}

async fn api_create_domain_knowledge(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    headers: HeaderMap,
    Json(request): Json<DomainKnowledgeUpsertRequest>,
) -> Result<(StatusCode, Json<DomainKnowledgeItemResponse>), StatusCode> {
    let normalized = normalize_domain_knowledge_request(&request)?;
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    require_current_user_is_guild_admin(&state, &user_id).await?;
    let id = Uuid::new_v4().to_string();
    let content_type = normalized.content_type.as_str().to_owned();
    let active = normalized.active.unwrap_or(true).to_string();
    let row = state
        .db
        .query_opt(
            INSERT_DOMAIN_KNOWLEDGE_SQL,
            &[
                &id,
                &auth.guild_id,
                &content_type,
                &normalized.title,
                &normalized.body,
                &active,
                &user_id,
            ],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let response = domain_knowledge_response_from_row(&row);
    record_audit_event(
        &state,
        web_audit_event(
            Some(auth.guild_id.clone()),
            Some(user_id.clone()),
            "domain_knowledge.create",
            "domain_knowledge",
            Some(response.id.clone()),
            audit_request_metadata(&headers, "POST", "/api/guild/domain-knowledge"),
            json!({
                "content_type": response.content_type.clone(),
                "active": response.active,
                "version": response.version,
            }),
        ),
    )
    .await;
    Ok((StatusCode::CREATED, Json(response)))
}

async fn api_update_domain_knowledge(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<DomainKnowledgeUpsertRequest>,
) -> Result<Json<DomainKnowledgeItemResponse>, StatusCode> {
    validate_domain_knowledge_item_id(&item_id)?;
    let normalized = normalize_domain_knowledge_request(&request)?;
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    require_current_user_is_guild_admin(&state, &user_id).await?;
    let content_type = normalized.content_type.as_str().to_owned();
    let active = normalized
        .active
        .map(|active| active.to_string())
        .unwrap_or_default();
    let row = state
        .db
        .query_opt(
            UPDATE_DOMAIN_KNOWLEDGE_SQL,
            &[
                &item_id,
                &auth.guild_id,
                &content_type,
                &normalized.title,
                &normalized.body,
                &active,
                &user_id,
            ],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let response = domain_knowledge_response_from_row(&row);
    record_audit_event(
        &state,
        web_audit_event(
            Some(auth.guild_id.clone()),
            Some(user_id.clone()),
            "domain_knowledge.update",
            "domain_knowledge",
            Some(response.id.clone()),
            audit_request_metadata(
                &headers,
                "PUT",
                &format!("/api/guild/domain-knowledge/{item_id}"),
            ),
            json!({
                "content_type": response.content_type.clone(),
                "active": response.active,
                "version": response.version,
            }),
        ),
    )
    .await;
    Ok(Json(response))
}

async fn api_activate_domain_knowledge(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<DomainKnowledgeItemResponse>, StatusCode> {
    validate_domain_knowledge_item_id(&item_id)?;
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    require_current_user_is_guild_admin(&state, &user_id).await?;
    let row = state
        .db
        .query_opt(
            ACTIVATE_DOMAIN_KNOWLEDGE_SQL,
            &[&item_id, &auth.guild_id, &user_id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let response = domain_knowledge_response_from_row(&row);
    record_audit_event(
        &state,
        web_audit_event(
            Some(auth.guild_id.clone()),
            Some(user_id.clone()),
            "domain_knowledge.activate",
            "domain_knowledge",
            Some(response.id.clone()),
            audit_request_metadata(
                &headers,
                "POST",
                &format!("/api/guild/domain-knowledge/{item_id}/activate"),
            ),
            json!({
                "active": response.active,
                "version": response.version,
                "archived": response.archived_at.is_some(),
            }),
        ),
    )
    .await;
    Ok(Json(response))
}

async fn api_archive_domain_knowledge(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<DomainKnowledgeItemResponse>, StatusCode> {
    validate_domain_knowledge_item_id(&item_id)?;
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    require_current_user_is_guild_admin(&state, &user_id).await?;
    let row = state
        .db
        .query_opt(
            ARCHIVE_DOMAIN_KNOWLEDGE_SQL,
            &[&item_id, &auth.guild_id, &user_id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let response = domain_knowledge_response_from_row(&row);
    record_audit_event(
        &state,
        web_audit_event(
            Some(auth.guild_id.clone()),
            Some(user_id.clone()),
            "domain_knowledge.archive",
            "domain_knowledge",
            Some(response.id.clone()),
            audit_request_metadata(
                &headers,
                "POST",
                &format!("/api/guild/domain-knowledge/{item_id}/archive"),
            ),
            json!({
                "active": response.active,
                "version": response.version,
                "archived": response.archived_at.is_some(),
            }),
        ),
    )
    .await;
    Ok(Json(response))
}

async fn api_list_summary_templates(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Query(query): Query<SummaryTemplateListQuery>,
) -> Result<Json<Vec<SummaryTemplateResponse>>, StatusCode> {
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    require_current_user_is_guild_admin(&state, &user_id).await?;
    let include_archived = query.include_archived.unwrap_or(false).to_string();
    let rows = state
        .db
        .query(
            LIST_SUMMARY_TEMPLATES_SQL,
            &[&auth.guild_id, &include_archived],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(
        rows.iter()
            .map(summary_template_response_from_row)
            .collect(),
    ))
}

async fn api_get_summary_template(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(template_id): Path<String>,
) -> Result<Json<SummaryTemplateResponse>, StatusCode> {
    validate_summary_template_id(&template_id)?;
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    require_current_user_is_guild_admin(&state, &user_id).await?;
    let row = state
        .db
        .query_opt(GET_SUMMARY_TEMPLATE_SQL, &[&auth.guild_id, &template_id])
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(summary_template_response_from_row(&row)))
}

async fn api_create_summary_template(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    headers: HeaderMap,
    Json(request): Json<SummaryTemplateUpsertRequest>,
) -> Result<(StatusCode, Json<SummaryTemplateResponse>), StatusCode> {
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let is_admin = current_user_is_guild_admin(&state, &user_id).await?;
    let normalized = validate_authorized_summary_template_request(is_admin, &request)?;
    let id = Uuid::new_v4().to_string();
    let active = normalized.active.unwrap_or(true).to_string();
    let row = state
        .db
        .query_opt(
            INSERT_SUMMARY_TEMPLATE_SQL,
            &[
                &id,
                &auth.guild_id,
                &normalized.name,
                &normalized.template,
                &active,
                &user_id,
            ],
        )
        .await
        .map_err(|err| summary_template_mutation_status(&err))?
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let response = summary_template_response_from_row(&row);
    record_audit_event(
        &state,
        web_audit_event(
            Some(auth.guild_id.clone()),
            Some(user_id.clone()),
            "summary_template.create",
            "summary_template",
            Some(response.id.clone()),
            audit_request_metadata(&headers, "POST", "/api/guild/summary-templates"),
            json!({
                "active": response.active,
                "version": response.version,
                "variables": normalized.variables,
            }),
        ),
    )
    .await;
    Ok((StatusCode::CREATED, Json(response)))
}

async fn api_update_summary_template(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(template_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SummaryTemplateUpsertRequest>,
) -> Result<Json<SummaryTemplateResponse>, StatusCode> {
    validate_summary_template_id(&template_id)?;
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let is_admin = current_user_is_guild_admin(&state, &user_id).await?;
    let normalized = validate_authorized_summary_template_request(is_admin, &request)?;
    let active = normalized
        .active
        .map(|active| active.to_string())
        .unwrap_or_default();
    let row = state
        .db
        .query_opt(
            UPDATE_SUMMARY_TEMPLATE_SQL,
            &[
                &template_id,
                &auth.guild_id,
                &normalized.name,
                &normalized.template,
                &active,
                &user_id,
            ],
        )
        .await
        .map_err(|err| summary_template_mutation_status(&err))?
        .ok_or(StatusCode::NOT_FOUND)?;
    let response = summary_template_response_from_row(&row);
    record_audit_event(
        &state,
        web_audit_event(
            Some(auth.guild_id.clone()),
            Some(user_id.clone()),
            "summary_template.update",
            "summary_template",
            Some(response.id.clone()),
            audit_request_metadata(
                &headers,
                "PUT",
                &format!("/api/guild/summary-templates/{template_id}"),
            ),
            json!({
                "active": response.active,
                "version": response.version,
                "variables": normalized.variables,
            }),
        ),
    )
    .await;
    Ok(Json(response))
}

async fn api_activate_summary_template(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(template_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<SummaryTemplateResponse>, StatusCode> {
    validate_summary_template_id(&template_id)?;
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    require_current_user_is_guild_admin(&state, &user_id).await?;
    let row = state
        .db
        .query_opt(
            ACTIVATE_SUMMARY_TEMPLATE_SQL,
            &[&template_id, &auth.guild_id, &user_id],
        )
        .await
        .map_err(|err| summary_template_mutation_status(&err))?
        .ok_or(StatusCode::NOT_FOUND)?;
    let response = summary_template_response_from_row(&row);
    record_audit_event(
        &state,
        web_audit_event(
            Some(auth.guild_id.clone()),
            Some(user_id.clone()),
            "summary_template.activate",
            "summary_template",
            Some(response.id.clone()),
            audit_request_metadata(
                &headers,
                "POST",
                &format!("/api/guild/summary-templates/{template_id}/activate"),
            ),
            json!({
                "active": response.active,
                "version": response.version,
                "archived": response.archived_at.is_some(),
            }),
        ),
    )
    .await;
    Ok(Json(response))
}

async fn api_archive_summary_template(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(template_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<SummaryTemplateResponse>, StatusCode> {
    validate_summary_template_id(&template_id)?;
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    require_current_user_is_guild_admin(&state, &user_id).await?;
    let row = state
        .db
        .query_opt(
            ARCHIVE_SUMMARY_TEMPLATE_SQL,
            &[&template_id, &auth.guild_id, &user_id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let response = summary_template_response_from_row(&row);
    record_audit_event(
        &state,
        web_audit_event(
            Some(auth.guild_id.clone()),
            Some(user_id.clone()),
            "summary_template.archive",
            "summary_template",
            Some(response.id.clone()),
            audit_request_metadata(
                &headers,
                "POST",
                &format!("/api/guild/summary-templates/{template_id}/archive"),
            ),
            json!({
                "active": response.active,
                "version": response.version,
                "archived": response.archived_at.is_some(),
            }),
        ),
    )
    .await;
    Ok(Json(response))
}

async fn api_update_target_guild_bot_token(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(guild_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<GuildBotTokenUpdateRequest>,
) -> Result<Json<GuildSettingsResponse>, Response> {
    let auth = state
        .auth
        .as_ref()
        .ok_or_else(|| StatusCode::SERVICE_UNAVAILABLE.into_response())?;
    let guild_id = normalize_target_guild_id(&guild_id).map_err(|status| status.into_response())?;
    let target_auth = require_user_is_target_guild_admin(&state, auth, &user_id, &guild_id)
        .await
        .map_err(|status| status.into_response())?;
    let token = validate_authorized_guild_bot_token_update(true, &request).map_err(|status| {
        api_error_response(
            status,
            "invalid_bot_token_request",
            "Discord bot token is required.",
        )
    })?;

    let cipher = state.guild_bot_token_cipher.as_ref().ok_or_else(|| {
        api_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "missing_guild_bot_token_encryption_key",
            "Guild bot token encryption key is not configured.",
        )
    })?;
    let validated = validate_discord_bot_token_for_guild(&state, &target_auth.guild_id, &token)
        .await
        .map_err(bot_token_validation_error_response)?;
    let encrypted = cipher
        .encrypt_for_guild(&target_auth.guild_id, &token)
        .map_err(|err| {
            warn!(
                error = %err,
                guild_id = %target_auth.guild_id,
                "failed to encrypt guild bot token"
            );
            api_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "guild_bot_token_encrypt_failed",
                "Failed to store Discord bot token.",
            )
        })?;

    state
        .db
        .execute(
            UPSERT_GUILD_BOT_TOKEN_SQL,
            &[
                &target_auth.guild_id,
                &encrypted.ciphertext,
                &encrypted.nonce,
                &encrypted.key_version,
                &validated.bot_user_id,
                &validated.bot_username,
            ],
        )
        .await
        .map_err(|err| {
            warn!(
                error = %err,
                guild_id = %target_auth.guild_id,
                "failed to store guild bot token"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?;
    invalidate_discord_caches(&state).await;
    notify_bot_token_changed(&state);
    record_audit_event(
        &state,
        web_audit_event(
            Some(target_auth.guild_id.clone()),
            Some(user_id.clone()),
            "guild_bot_token.update",
            "guild_bot_token",
            Some(target_auth.guild_id.clone()),
            audit_request_metadata(
                &headers,
                "PUT",
                &target_guild_settings_path(&target_auth.guild_id, "/bot-token"),
            ),
            json!({
                "bot_user_id": validated.bot_user_id,
                "bot_username": validated.bot_username,
                "token_registered": true,
            }),
        ),
    )
    .await;

    let stored = load_guild_settings(&state, &target_auth.guild_id)
        .await
        .map_err(|status| status.into_response())?;
    Ok(Json(guild_settings_response(
        &state.guild_settings_defaults,
        stored,
        true,
    )))
}

async fn api_delete_target_guild_bot_token(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(guild_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<GuildSettingsResponse>, Response> {
    let auth = state
        .auth
        .as_ref()
        .ok_or_else(|| StatusCode::SERVICE_UNAVAILABLE.into_response())?;
    let guild_id = normalize_target_guild_id(&guild_id).map_err(|status| status.into_response())?;
    let target_auth = require_user_is_target_guild_admin(&state, auth, &user_id, &guild_id)
        .await
        .map_err(|status| status.into_response())?;

    let current = load_guild_settings(&state, &target_auth.guild_id)
        .await
        .map_err(|status| status.into_response())?;
    if guild_bot_token_delete_is_noop(current.as_ref()) {
        return Ok(Json(guild_settings_response(
            &state.guild_settings_defaults,
            current,
            true,
        )));
    }

    state
        .db
        .execute(CLEAR_GUILD_BOT_TOKEN_SQL, &[&target_auth.guild_id])
        .await
        .map_err(|err| {
            warn!(
                error = %err,
                guild_id = %target_auth.guild_id,
                "failed to delete guild bot token"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?;
    invalidate_discord_caches(&state).await;
    notify_bot_token_changed(&state);
    record_audit_event(
        &state,
        web_audit_event(
            Some(target_auth.guild_id.clone()),
            Some(user_id.clone()),
            "guild_bot_token.delete",
            "guild_bot_token",
            Some(target_auth.guild_id.clone()),
            audit_request_metadata(
                &headers,
                "DELETE",
                &target_guild_settings_path(&target_auth.guild_id, "/bot-token"),
            ),
            json!({
                "token_registered": false,
                "previously_registered": true,
            }),
        ),
    )
    .await;

    let stored = load_guild_settings(&state, &target_auth.guild_id)
        .await
        .map_err(|status| status.into_response())?;
    Ok(Json(guild_settings_response(
        &state.guild_settings_defaults,
        stored,
        true,
    )))
}

async fn api_update_guild_bot_token(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    headers: HeaderMap,
    Json(request): Json<GuildBotTokenUpdateRequest>,
) -> Result<Json<GuildSettingsResponse>, Response> {
    let token = normalize_guild_bot_token_update(&request).map_err(|status| {
        api_error_response(
            status,
            "invalid_bot_token_request",
            "Discord bot token is required.",
        )
    })?;
    let auth = state
        .auth
        .as_ref()
        .ok_or_else(|| StatusCode::SERVICE_UNAVAILABLE.into_response())?;
    if !check_guild_admin_permission_for_settings(&state, auth, &user_id)
        .await
        .map_err(|status| status.into_response())?
    {
        return Err(api_error_response(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Guild administrator permission is required.",
        ));
    }

    let cipher = state.guild_bot_token_cipher.as_ref().ok_or_else(|| {
        api_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "missing_guild_bot_token_encryption_key",
            "Guild bot token encryption key is not configured.",
        )
    })?;
    let validated = validate_discord_bot_token_for_guild(&state, &auth.guild_id, &token)
        .await
        .map_err(bot_token_validation_error_response)?;
    let encrypted = cipher
        .encrypt_for_guild(&auth.guild_id, &token)
        .map_err(|err| {
            warn!(
                error = %err,
                guild_id = %auth.guild_id,
                "failed to encrypt guild bot token"
            );
            api_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "guild_bot_token_encrypt_failed",
                "Failed to store Discord bot token.",
            )
        })?;

    state
        .db
        .execute(
            UPSERT_GUILD_BOT_TOKEN_SQL,
            &[
                &auth.guild_id,
                &encrypted.ciphertext,
                &encrypted.nonce,
                &encrypted.key_version,
                &validated.bot_user_id,
                &validated.bot_username,
            ],
        )
        .await
        .map_err(|err| {
            warn!(
                error = %err,
                guild_id = %auth.guild_id,
                "failed to store guild bot token"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?;
    invalidate_discord_caches(&state).await;
    notify_bot_token_changed(&state);
    record_audit_event(
        &state,
        web_audit_event(
            Some(auth.guild_id.clone()),
            Some(user_id.clone()),
            "guild_bot_token.update",
            "guild_bot_token",
            Some(auth.guild_id.clone()),
            audit_request_metadata(&headers, "PUT", "/api/guild/settings/bot-token"),
            json!({
                "bot_user_id": validated.bot_user_id,
                "bot_username": validated.bot_username,
                "token_registered": true,
            }),
        ),
    )
    .await;

    let stored = load_guild_settings(&state, &auth.guild_id)
        .await
        .map_err(|status| status.into_response())?;
    Ok(Json(guild_settings_response(
        &state.guild_settings_defaults,
        stored,
        true,
    )))
}

async fn api_delete_guild_bot_token(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    headers: HeaderMap,
) -> Result<Json<GuildSettingsResponse>, Response> {
    let auth = state
        .auth
        .as_ref()
        .ok_or_else(|| StatusCode::SERVICE_UNAVAILABLE.into_response())?;
    if !check_guild_admin_permission_for_settings(&state, auth, &user_id)
        .await
        .map_err(|status| status.into_response())?
    {
        return Err(api_error_response(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Guild administrator permission is required.",
        ));
    }

    let current = load_guild_settings(&state, &auth.guild_id)
        .await
        .map_err(|status| status.into_response())?;
    if guild_bot_token_delete_is_noop(current.as_ref()) {
        return Ok(Json(guild_settings_response(
            &state.guild_settings_defaults,
            current,
            true,
        )));
    }

    state
        .db
        .execute(CLEAR_GUILD_BOT_TOKEN_SQL, &[&auth.guild_id])
        .await
        .map_err(|err| {
            warn!(
                error = %err,
                guild_id = %auth.guild_id,
                "failed to delete guild bot token"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })?;
    invalidate_discord_caches(&state).await;
    notify_bot_token_changed(&state);
    record_audit_event(
        &state,
        web_audit_event(
            Some(auth.guild_id.clone()),
            Some(user_id.clone()),
            "guild_bot_token.delete",
            "guild_bot_token",
            Some(auth.guild_id.clone()),
            audit_request_metadata(&headers, "DELETE", "/api/guild/settings/bot-token"),
            json!({
                "token_registered": false,
                "previously_registered": true,
            }),
        ),
    )
    .await;

    let stored = load_guild_settings(&state, &auth.guild_id)
        .await
        .map_err(|status| status.into_response())?;
    Ok(Json(guild_settings_response(
        &state.guild_settings_defaults,
        stored,
        true,
    )))
}

async fn invalidate_discord_caches(state: &WebState) {
    let bot_token_refresh = {
        let mut cache = state.bot_token_cache.write().await;
        cache.entry = None;
        cache.failure = None;
        cache.revision = cache.revision.wrapping_add(1);
        cache.refresh.take()
    };
    if let Some(refresh) = bot_token_refresh {
        refresh.notify.notify_waiters();
    }
    let guild_refresh = {
        let mut cache = state.guild_cache.write().await;
        cache.entry = None;
        cache.failure = None;
        cache.revision = cache.revision.wrapping_add(1);
        cache.refresh.take()
    };
    if let Some(refresh) = guild_refresh {
        refresh.notify.notify_waiters();
    }
    state.membership_cache.write().await.clear();
    state.permission_cache.write().await.clear();
}

fn advance_bot_token_revision(sender: &watch::Sender<u64>) {
    sender.send_modify(|revision| *revision = revision.wrapping_add(1));
}

fn notify_bot_token_changed(state: &WebState) {
    if let Some(sender) = &state.bot_token_revision_tx {
        advance_bot_token_revision(sender);
    }
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

fn api_transcript_sql() -> &'static str {
    "SELECT t.id, t.speaker_id, \
            CASE WHEN t.transcript_stage='live' AND c.timeline_base_ms IS NOT NULL AND lb.min_base_ms IS NOT NULL \
                 THEN (t.start_ms::BIGINT + (c.timeline_base_ms - lb.min_base_ms))::INTEGER \
                 ELSE t.start_ms \
            END AS start_ms, \
            CASE WHEN t.transcript_stage='live' AND c.timeline_base_ms IS NOT NULL AND lb.min_base_ms IS NOT NULL \
                 THEN (t.end_ms::BIGINT + (c.timeline_base_ms - lb.min_base_ms))::INTEGER \
                 ELSE t.end_ms \
            END AS end_ms, \
            t.text, t.confidence, t.is_noisy, t.source, \
            to_char(t.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') as created_at, \
            ms.username, ms.nickname, ms.display_name \
     FROM transcripts t \
     LEFT JOIN live_transcription_chunks c \
       ON c.id = t.live_chunk_id AND c.status='done' \
     LEFT JOIN LATERAL ( \
       SELECT MIN(timeline_base_ms) AS min_base_ms \
       FROM live_transcription_chunks \
       WHERE meeting_id=$1 AND timeline_base_ms IS NOT NULL \
     ) lb ON true \
     LEFT JOIN LATERAL ( \
       SELECT EXISTS (SELECT 1 FROM transcripts ft WHERE ft.meeting_id=$1 AND ft.transcript_stage='final' AND NOT ft.is_deleted) AS has_final_rows \
     ) fb ON true \
     LEFT JOIN meeting_speakers ms \
       ON ms.meeting_id = t.meeting_id AND ms.speaker_id = t.speaker_id \
     WHERE t.meeting_id=$1 AND NOT t.is_deleted \
       AND (t.transcript_stage='final' OR (NOT fb.has_final_rows AND c.id IS NOT NULL)) \
       AND ($2::text IS NULL OR (t.created_at, t.id) > (($2::text)::timestamptz, $3)) \
     ORDER BY t.created_at, t.id"
}

async fn api_transcript(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(meeting_id): Path<String>,
) -> Result<Json<TranscriptResponse>, StatusCode> {
    verify_meeting_access(&state, &meeting_id, &user_id).await?;

    let (response, _) = load_transcript_response(&state, &meeting_id, None).await?;
    Ok(Json(response))
}

async fn api_transcript_events(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(meeting_id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, StatusCode> {
    verify_meeting_access(&state, &meeting_id, &user_id).await?;
    let permit =
        try_acquire_transcript_sse_permit(&state.transcript_sse_limiter, &user_id, &meeting_id)?;

    let stream = stream::unfold(
        (
            state,
            user_id,
            meeting_id,
            None::<TranscriptStreamCursor>,
            Duration::ZERO,
            0_u32,
            permit,
            false,
        ),
        |(state, user_id, meeting_id, cursor, poll_delay, idle_polls, permit, finished)| async move {
            if finished {
                return None;
            }

            tokio::time::sleep(poll_delay).await;

            if let Err(status) = verify_meeting_access(&state, &meeting_id, &user_id).await {
                let code = match status {
                    StatusCode::FORBIDDEN => "forbidden",
                    StatusCode::NOT_FOUND => "not_found",
                    StatusCode::SERVICE_UNAVAILABLE => "auth_unavailable",
                    _ => "unavailable",
                };
                let event = Event::default()
                    .event("stream-error")
                    .data(format!(r#"{{"code":"{code}"}}"#));
                return Some((
                    Ok(event),
                    (
                        state, user_id, meeting_id, cursor, poll_delay, idle_polls, permit, true,
                    ),
                ));
            }

            match load_transcript_response(&state, &meeting_id, cursor.as_ref()).await {
                Ok((response, next_cursor)) => {
                    let had_segments = !response.segments.is_empty();
                    let is_final = response.is_final;
                    let next_idle_polls = next_transcript_sse_idle_polls(idle_polls, had_segments);
                    let stop_for_idle =
                        !is_final && transcript_sse_idle_limit_reached(next_idle_polls);
                    let next_poll_delay = next_transcript_sse_poll_delay(poll_delay, had_segments);
                    let event = if stop_for_idle {
                        Event::default()
                            .event("stream-closed")
                            .data(r#"{"code":"idle_timeout"}"#)
                    } else {
                        match serde_json::to_string(&response) {
                            Ok(data) => Event::default().event("segments").data(data),
                            Err(err) => Event::default().event("stream-error").data(
                                serde_json::json!({
                                    "code": "encode",
                                    "message": err.to_string(),
                                })
                                .to_string(),
                            ),
                        }
                    };
                    Some((
                        Ok(event),
                        (
                            state,
                            user_id,
                            meeting_id,
                            next_cursor.or(cursor),
                            next_poll_delay,
                            next_idle_polls,
                            permit,
                            is_final || stop_for_idle,
                        ),
                    ))
                }
                Err(_) => {
                    let next_idle_polls = next_transcript_sse_idle_polls(idle_polls, false);
                    let next_poll_delay = next_transcript_sse_poll_delay(poll_delay, false);
                    let stop_for_idle = transcript_sse_idle_limit_reached(next_idle_polls);
                    let event = if stop_for_idle {
                        Event::default()
                            .event("stream-closed")
                            .data(r#"{"code":"error_limit"}"#)
                    } else {
                        Event::default()
                            .event("stream-error")
                            .data(r#"{"code":"transcript_unavailable"}"#)
                    };
                    Some((
                        Ok(event),
                        (
                            state,
                            user_id,
                            meeting_id,
                            cursor,
                            next_poll_delay,
                            next_idle_polls,
                            permit,
                            stop_for_idle,
                        ),
                    ))
                }
            }
        },
    );

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn load_transcript_response(
    state: &WebState,
    meeting_id: &str,
    cursor: Option<&TranscriptStreamCursor>,
) -> Result<(TranscriptResponse, Option<TranscriptStreamCursor>), StatusCode> {
    let metadata = load_transcript_metadata(state, meeting_id).await?;
    let (segments, next_cursor) = load_transcript_segments_after(state, meeting_id, cursor).await?;
    let updated_at = next_cursor
        .as_ref()
        .map(|cursor| cursor.created_at.clone())
        .or(metadata.updated_at.clone());
    let is_final = transcript_metadata_is_final(&metadata);

    Ok((
        TranscriptResponse {
            segments,
            status: metadata.status,
            is_final,
            updated_at,
        },
        next_cursor,
    ))
}

#[derive(Debug, Clone)]
struct TranscriptMetadata {
    status: String,
    has_final_rows: bool,
    has_live_rows: bool,
    updated_at: Option<String>,
}

fn transcript_metadata_is_final(metadata: &TranscriptMetadata) -> bool {
    metadata.has_final_rows
        || matches!(
            metadata.status.as_str(),
            "posted" | "failed" | "aborted" | "done"
        )
        || (!metadata.has_live_rows
            && !matches!(
                metadata.status.as_str(),
                "recording" | "stopping" | "transcribing" | "summarizing" | "processing"
            ))
}

async fn load_transcript_metadata(
    state: &WebState,
    meeting_id: &str,
) -> Result<TranscriptMetadata, StatusCode> {
    let row = state
        .db
        .query_opt(
            "SELECT m.status, \
                    EXISTS (SELECT 1 FROM transcripts t WHERE t.meeting_id=m.id AND t.transcript_stage='final' AND NOT t.is_deleted) as has_final_rows, \
                    EXISTS (SELECT 1 FROM transcripts t JOIN live_transcription_chunks c ON c.id=t.live_chunk_id AND c.status='done' WHERE t.meeting_id=m.id AND t.transcript_stage='live' AND NOT t.is_deleted) as has_live_rows, \
                    to_char((SELECT MAX(created_at) FROM transcripts WHERE meeting_id=m.id AND NOT is_deleted) AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') as updated_at \
             FROM meetings m WHERE m.id=$1",
            &[&meeting_id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(TranscriptMetadata {
        status: row.get("status"),
        has_final_rows: row.get("has_final_rows"),
        has_live_rows: row.get("has_live_rows"),
        updated_at: row.get("updated_at"),
    })
}

async fn load_transcript_segments_after(
    state: &WebState,
    meeting_id: &str,
    cursor: Option<&TranscriptStreamCursor>,
) -> Result<
    (
        Vec<TranscriptSegmentResponse>,
        Option<TranscriptStreamCursor>,
    ),
    StatusCode,
> {
    let cursor_created_at = cursor.map(|value| value.created_at.as_str());
    let cursor_id = cursor.map(|value| value.id.as_str()).unwrap_or("");

    let rows = state
        .db
        .query(
            api_transcript_sql(),
            &[&meeting_id, &cursor_created_at, &cursor_id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut segments = Vec::with_capacity(rows.len());
    let mut next_cursor = cursor.cloned();
    for row in &rows {
        let id: String = row.get("id");
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
            id: id.clone(),
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
        next_cursor = Some(TranscriptStreamCursor {
            created_at: row.get("created_at"),
            id,
        });
    }

    sort_transcript_segment_responses(&mut segments);

    Ok((segments, next_cursor))
}

fn sort_transcript_segment_responses(segments: &mut [TranscriptSegmentResponse]) {
    segments.sort_by(|left, right| {
        compare_transcript_timeline_order(
            transcript_response_order_key(left),
            transcript_response_order_key(right),
        )
    });
}

fn transcript_response_order_key(
    segment: &TranscriptSegmentResponse,
) -> TranscriptTimelineOrderKey<'_> {
    let source = TranscriptSource::parse_str(&segment.source)
        .expect("transcript response source is validated before sorting");
    TranscriptTimelineOrderKey::new(
        i64::from(segment.start_ms),
        i64::from(segment.end_ms),
        source,
        Some(&segment.id),
    )
}

async fn api_transcript_state(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(meeting_id): Path<String>,
) -> Result<Json<TranscriptStateResponse>, StatusCode> {
    verify_meeting_access(&state, &meeting_id, &user_id).await?;

    let metadata = load_transcript_metadata(&state, &meeting_id).await?;
    let is_final = transcript_metadata_is_final(&metadata);

    Ok(Json(TranscriptStateResponse {
        status: metadata.status,
        is_final,
        updated_at: metadata.updated_at,
    }))
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

impl DebugArtifactSource {
    fn filename(&self) -> &str {
        match self {
            Self::File { filename, .. } | Self::Inline { filename, .. } => filename,
        }
    }

    fn content_type(&self) -> &'static str {
        match self {
            Self::File { content_type, .. } | Self::Inline { content_type, .. } => content_type,
        }
    }
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
    headers: HeaderMap,
    Path((meeting_id, artifact_id)): Path<(String, String)>,
) -> Result<Response, StatusCode> {
    let access = verify_meeting_access(&state, &meeting_id, &user_id).await?;
    if debug_artifact_requires_admin(&artifact_id) {
        let allowed = verify_raw_debug_artifact_access(&state, &access, &user_id).await?;
        authorize_debug_artifact_download(allowed)?;
    }

    let source = resolve_debug_artifact(&state, &meeting_id, &access, &artifact_id).await?;
    let audit_filename = source.filename().to_owned();
    let audit_content_type = source.content_type();
    let response = match source {
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
    }?;
    record_audit_event(
        &state,
        web_audit_event(
            Some(access.guild_id.clone()),
            Some(user_id.clone()),
            "debug_artifact.download",
            "debug_artifact",
            Some(artifact_id.clone()),
            audit_request_metadata(
                &headers,
                "GET",
                &format!("/api/meetings/{meeting_id}/debug/files/{artifact_id}"),
            ),
            json!({
                "meeting_id": meeting_id,
                "filename": audit_filename,
                "content_type": audit_content_type,
                "admin_only": debug_artifact_requires_admin(&artifact_id),
            }),
        ),
    )
    .await;
    let usage_state = state.clone();
    let usage_guild_id = access.guild_id.clone();
    let usage_meeting_id = meeting_id.clone();
    let usage_artifact_id = artifact_id.clone();
    let usage_filename = audit_filename.clone();
    let usage_content_type = audit_content_type.to_owned();
    let usage_user_id = user_id.clone();
    let usage_admin_only = debug_artifact_requires_admin(&artifact_id);
    let observed_at = Utc::now();
    tokio::spawn(async move {
        record_usage_event(
            &usage_state,
            NewUsageEvent {
                id: format!(
                    "{}:{}",
                    debug_download_usage_event_id(
                        &usage_guild_id,
                        &usage_meeting_id,
                        &usage_artifact_id,
                        &usage_filename,
                        &usage_content_type,
                        &usage_user_id,
                    ),
                    observed_at.timestamp_micros()
                ),
                tenant_id: None,
                guild_id: usage_guild_id,
                meeting_id: Some(usage_meeting_id),
                job_id: None,
                resource_type: Some("debug_artifact".to_owned()),
                resource_id: Some(usage_artifact_id),
                metric: UsageMetric::DebugDownloads,
                quantity: 1,
                detail_json: UsageDetailJson::new(json!({
                    "filename": usage_filename,
                    "content_type": usage_content_type,
                    "admin_only": usage_admin_only,
                    "user_id": usage_user_id,
                }))
                .expect("usage detail must be a JSON object"),
                observed_at,
            },
        )
        .await;
    });
    Ok(response)
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
mod operational_endpoint_tests {
    use super::{
        OperationalCheck, OperationalCounters, OperationalMetricsResponse,
        OperationalMetricsStatus, OperationalReadinessChecks, OperationalStatus,
        authorize_operational_metrics_request, healthz, integration_readiness_not_checked,
        metricsz_response_with_loader, operational_readiness_response,
        public_operational_readiness_response,
    };
    use axum::body::to_bytes;
    use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
    use axum::response::IntoResponse;
    use serde_json::Value;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::sync::Mutex;

    #[tokio::test]
    async fn healthz_returns_liveness_without_state_or_auth() {
        let response = healthz().await.into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("health response body should be readable");
        let json: Value =
            serde_json::from_slice(&body).expect("health response should be valid JSON");

        assert_eq!(json, serde_json::json!({ "status": "ok" }));
    }

    #[test]
    fn readiness_is_ready_when_required_checks_are_ok() {
        let checks = OperationalReadinessChecks {
            database: OperationalCheck::ok(),
            migrations: OperationalCheck::ok(),
            queue: OperationalCheck::ok(),
            integrations: integration_readiness_not_checked(),
        };

        let (status, response) = operational_readiness_response(checks);

        assert_eq!(status, StatusCode::OK);
        assert_eq!(response.status, "ready");
        assert_eq!(
            response.checks.integrations.status,
            OperationalStatus::NotChecked
        );
        let json = serde_json::to_value(&response).expect("readiness response should serialize");
        assert_eq!(json["checks"]["integrations"]["status"], "not_checked");
        assert_eq!(
            response.checks.integrations.reason,
            Some(
                "runtime integration state is not shared with the web server in this operational slice"
            )
        );
    }

    #[test]
    fn public_readiness_response_hides_operational_check_details() {
        let checks = OperationalReadinessChecks {
            database: OperationalCheck::unavailable("database query failed"),
            migrations: OperationalCheck::unavailable("database unavailable"),
            queue: OperationalCheck::unavailable("database unavailable"),
            integrations: integration_readiness_not_checked(),
        };

        let (status, response) = public_operational_readiness_response(checks);
        let json =
            serde_json::to_value(&response).expect("public readiness response should serialize");

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(json, serde_json::json!({ "status": "not_ready" }));
        assert!(json.get("checks").is_none());
    }

    #[test]
    fn readiness_is_unready_when_database_is_unavailable() {
        let checks = OperationalReadinessChecks {
            database: OperationalCheck::unavailable("database query failed"),
            migrations: OperationalCheck::unavailable("database unavailable"),
            queue: OperationalCheck::unavailable("database unavailable"),
            integrations: integration_readiness_not_checked(),
        };

        let (status, response) = operational_readiness_response(checks);

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.status, "not_ready");
        assert_eq!(
            response.checks.database.status,
            OperationalStatus::Unavailable
        );
        assert_eq!(
            response.checks.database.reason,
            Some("database query failed")
        );
    }

    #[test]
    fn readiness_is_unready_when_migrations_are_unavailable() {
        let checks = OperationalReadinessChecks {
            database: OperationalCheck::ok(),
            migrations: OperationalCheck::unavailable("required database tables are missing"),
            queue: OperationalCheck::ok(),
            integrations: integration_readiness_not_checked(),
        };

        let (status, response) = operational_readiness_response(checks);

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.status, "not_ready");
        assert_eq!(response.checks.database.status, OperationalStatus::Ok);
        assert_eq!(
            response.checks.migrations.status,
            OperationalStatus::Unavailable
        );
    }

    #[test]
    fn metrics_response_exposes_only_aggregate_counters() {
        let response = OperationalMetricsResponse {
            status: OperationalMetricsStatus::Ok,
            counters: OperationalCounters {
                failed_jobs: 2,
                running_jobs: 1,
                queued_jobs: 3,
                running_meetings: 4,
                failed_live_transcription_chunks: 5,
            },
        };

        let json = serde_json::to_value(response).expect("metrics response should serialize");

        assert_eq!(
            json,
            serde_json::json!({
                "status": "ok",
                "counters": {
                    "failed_jobs": 2,
                    "running_jobs": 1,
                    "queued_jobs": 3,
                    "running_meetings": 4,
                    "failed_live_transcription_chunks": 5
                }
            })
        );
        let serialized = json.to_string();
        assert!(!serialized.contains("token"));
        assert!(!serialized.contains("user_id"));
        assert!(!serialized.contains("guild_id"));
    }

    #[test]
    fn metrics_route_rejects_missing_or_invalid_bearer_token() {
        let empty_headers = HeaderMap::new();
        assert_eq!(
            authorize_operational_metrics_request(Some("expected-token"), &empty_headers),
            Err(StatusCode::UNAUTHORIZED)
        );
        assert_eq!(
            authorize_operational_metrics_request(None, &empty_headers),
            Err(StatusCode::SERVICE_UNAVAILABLE)
        );

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer wrong-token"),
        );
        assert_eq!(
            authorize_operational_metrics_request(Some("expected-token"), &headers),
            Err(StatusCode::UNAUTHORIZED)
        );
    }

    #[test]
    fn metrics_route_accepts_matching_bearer_token() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer expected-token"),
        );

        assert_eq!(
            authorize_operational_metrics_request(Some("expected-token"), &headers),
            Ok(())
        );
    }

    #[tokio::test]
    async fn metrics_response_denies_without_loading_metrics() {
        let cache = Arc::new(Mutex::new(None));
        let loads = Arc::new(AtomicUsize::new(0));
        let first_loads = Arc::clone(&loads);

        let response =
            metricsz_response_with_loader(None, &HeaderMap::new(), &cache, move || async move {
                first_loads.fetch_add(1, Ordering::SeqCst);
                OperationalMetricsResponse {
                    status: OperationalMetricsStatus::Ok,
                    counters: OperationalCounters::default(),
                }
            })
            .await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(loads.load(Ordering::SeqCst), 0);

        let second_loads = Arc::clone(&loads);
        let response = metricsz_response_with_loader(
            Some("expected-token"),
            &HeaderMap::new(),
            &cache,
            move || async move {
                second_loads.fetch_add(1, Ordering::SeqCst);
                OperationalMetricsResponse {
                    status: OperationalMetricsStatus::Ok,
                    counters: OperationalCounters::default(),
                }
            },
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(loads.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn metrics_response_accepts_token_and_reuses_recent_snapshot() {
        let cache = Arc::new(Mutex::new(None));
        let loads = Arc::new(AtomicUsize::new(0));
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer expected-token"),
        );
        let first_loads = Arc::clone(&loads);

        let first = metricsz_response_with_loader(
            Some("expected-token"),
            &headers,
            &cache,
            move || async move {
                first_loads.fetch_add(1, Ordering::SeqCst);
                OperationalMetricsResponse {
                    status: OperationalMetricsStatus::Ok,
                    counters: OperationalCounters {
                        failed_jobs: 1,
                        running_jobs: 2,
                        queued_jobs: 3,
                        running_meetings: 4,
                        failed_live_transcription_chunks: 5,
                    },
                }
            },
        )
        .await;

        let second_loads = Arc::clone(&loads);
        let second = metricsz_response_with_loader(
            Some("expected-token"),
            &headers,
            &cache,
            move || async move {
                second_loads.fetch_add(1, Ordering::SeqCst);
                OperationalMetricsResponse {
                    status: OperationalMetricsStatus::Unavailable,
                    counters: OperationalCounters::default(),
                }
            },
        )
        .await;

        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(second.status(), StatusCode::OK);
        assert_eq!(loads.load(Ordering::SeqCst), 1);

        let body = to_bytes(second.into_body(), usize::MAX)
            .await
            .expect("metrics response body should be readable");
        let json: Value =
            serde_json::from_slice(&body).expect("metrics response should be valid JSON");
        assert_eq!(json["counters"]["failed_jobs"], 1);
    }
}

#[cfg(test)]
mod guild_api_tests {
    use super::{
        ADMINISTRATOR, AuthConfig, BOT_TOKEN_CACHE_REFRESH_INFLIGHT_SECS, BotTokenCacheState,
        CachedChannelPermission, DiscordBotTokenValidationError, DiscordBotTokenValidationStage,
        DiscordGuild, DiscordGuildFull, DiscordRoleFull, DomainKnowledgeListQuery,
        DomainKnowledgeUpsertRequest, GUILD_CACHE_REFRESH_INFLIGHT_SECS,
        GUILD_MEETINGS_VISIBILITY_CHANNEL_CAP, GUILD_MEETINGS_VISIBILITY_SCAN_LIMIT,
        GUILD_MEETINGS_VISIBILITY_SCAN_MIN, GuildAdminCheck, GuildBotTokenUpdateRequest,
        GuildCache, GuildCacheState, GuildMeetingsQuery, GuildSettingsDefaults,
        GuildSettingsUpdateRequest, PERMISSION_CACHE_SENSITIVE_POSITIVE_TTL_SECS,
        StoredGuildSettings, SummaryTemplateUpsertRequest, advance_bot_token_revision,
        bot_auth_header_from_cache_with_resolver, can_scan_new_visibility_channel,
        classify_discord_bot_token_validation_status, current_user_guilds_response,
        discord_user_guilds_api_status, guild_admin_member_status_decision,
        guild_admin_permission_cache_key, guild_admin_required_result,
        guild_bot_token_delete_is_noop, guild_info_from_cache_with_resolver,
        guild_meetings_visibility_scan_limit, guild_settings_response,
        normalize_domain_knowledge_list_filter, normalize_domain_knowledge_request,
        normalize_guild_bot_token_update, normalize_guild_meetings_pagination,
        normalize_guild_meetings_voice_channel_id, normalize_summary_template_request,
        normalize_target_guild_id, permission_cache_ttl, target_auth_config,
        target_guild_has_active_installation, target_guild_settings_path,
        user_can_access_target_guild, validate_authorized_guild_bot_token_update,
        validate_authorized_guild_settings_update, validate_authorized_summary_template_request,
        validate_domain_knowledge_item_id, validate_guild_settings_update,
        validate_summary_template_id,
    };
    use crate::infrastructure::sql::{
        COUNT_GUILD_MEETINGS_SQL, LIST_GUILD_MEETING_VOICE_CHANNELS_SQL, LIST_GUILD_MEETINGS_SQL,
    };
    use axum::http::StatusCode;
    use std::collections::HashMap;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::{Duration, Instant};
    use tokio::sync::{Barrier, Notify, RwLock, watch};

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
            summary_enabled: true,
        }
    }

    fn visible_guild(id: &str, name: &str, permissions: u64) -> DiscordGuild {
        DiscordGuild {
            id: id.to_owned(),
            name: name.to_owned(),
            icon: None,
            owner: false,
            permissions,
        }
    }

    #[test]
    fn me_guilds_reconciles_discord_visibility_with_active_tenants() {
        let visible = vec![
            visible_guild("guild-admin", "Admin Guild", ADMINISTRATOR),
            visible_guild("guild-member", "Member Guild", 0),
            visible_guild("guild-uninstalled", "Uninstalled Guild", ADMINISTRATOR),
        ];
        let tenant_by_guild_id = HashMap::from([
            ("guild-admin".to_owned(), "tenant-admin".to_owned()),
            ("guild-member".to_owned(), "tenant-member".to_owned()),
            ("guild-lost".to_owned(), "tenant-lost".to_owned()),
        ]);

        let response = current_user_guilds_response(&visible, &tenant_by_guild_id);

        assert_eq!(response.len(), 3);
        let admin = response
            .iter()
            .find(|guild| guild.guild_id == "guild-admin")
            .expect("admin guild should be listed");
        assert!(admin.is_member);
        assert!(admin.is_admin);
        assert_eq!(admin.tenant_id.as_deref(), Some("tenant-admin"));

        let member = response
            .iter()
            .find(|guild| guild.guild_id == "guild-member")
            .expect("member guild should be listed");
        assert!(member.is_member);
        assert!(!member.is_admin);
        assert_eq!(member.tenant_id.as_deref(), Some("tenant-member"));

        let uninstalled = response
            .iter()
            .find(|guild| guild.guild_id == "guild-uninstalled")
            .expect("visible bot-not-installed guild should be listed");
        assert!(uninstalled.is_member);
        assert!(uninstalled.is_admin);
        assert_eq!(uninstalled.tenant_id, None);
        assert!(
            !response.iter().any(|guild| guild.guild_id == "guild-lost"),
            "installed guilds missing from Discord visibility must fail closed"
        );
    }

    #[test]
    fn me_guilds_treats_guild_owner_as_admin_and_sanitizes_empty_rows() {
        let mut owner = visible_guild("guild-owner", " Owner Guild ", 0);
        owner.owner = true;
        owner.icon = Some(" icon-hash ".to_owned());
        let visible = vec![
            visible_guild("", "No id", ADMINISTRATOR),
            visible_guild("blank-name", "   ", ADMINISTRATOR),
            owner,
            visible_guild("guild-owner", "Duplicate Owner", 0),
        ];
        let tenant_by_guild_id =
            HashMap::from([("guild-owner".to_owned(), "tenant-owner".to_owned())]);

        let response = current_user_guilds_response(&visible, &tenant_by_guild_id);

        assert_eq!(response.len(), 1);
        assert_eq!(response[0].guild_id, "guild-owner");
        assert_eq!(response[0].name, "Owner Guild");
        assert_eq!(response[0].icon.as_deref(), Some("icon-hash"));
        assert!(response[0].is_admin);
        assert_eq!(response[0].tenant_id.as_deref(), Some("tenant-owner"));
    }

    #[test]
    fn me_guilds_upstream_statuses_fail_closed() {
        assert_eq!(
            discord_user_guilds_api_status(reqwest::StatusCode::OK),
            Ok(())
        );
        assert_eq!(
            discord_user_guilds_api_status(reqwest::StatusCode::UNAUTHORIZED),
            Err(StatusCode::FORBIDDEN)
        );
        assert_eq!(
            discord_user_guilds_api_status(reqwest::StatusCode::FORBIDDEN),
            Err(StatusCode::FORBIDDEN)
        );
        assert_eq!(
            discord_user_guilds_api_status(reqwest::StatusCode::TOO_MANY_REQUESTS),
            Err(StatusCode::BAD_GATEWAY)
        );
    }

    fn stored_settings_with_token(registered: bool) -> StoredGuildSettings {
        StoredGuildSettings {
            whisper_language: Some("fr".to_owned()),
            whisper_language_explicit: true,
            whisper_vad: Some(true),
            auto_stop_grace_seconds: Some(300),
            retention_raw_audio_ttl_days: Some(21),
            retention_transcript_ttl_days: Some(90),
            summary_enabled: Some(false),
            discord_bot_token_registered: registered,
            discord_bot_token_updated_at: registered.then(|| "2026-05-31T00:00:00Z".to_owned()),
            discord_bot_token_last_validated_at: registered
                .then(|| "2026-05-31T00:01:00Z".to_owned()),
            discord_bot_user_id: registered.then(|| "bot-1".to_owned()),
            discord_bot_username: registered.then(|| "GuildBot".to_owned()),
        }
    }

    #[test]
    fn guild_bot_token_delete_noops_without_registered_token() {
        let registered = stored_settings_with_token(true);
        let unregistered = stored_settings_with_token(false);

        assert!(guild_bot_token_delete_is_noop(None));
        assert!(guild_bot_token_delete_is_noop(Some(&unregistered)));
        assert!(!guild_bot_token_delete_is_noop(Some(&registered)));
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
        assert!(!response.discord_bot_token_registered);
        assert_eq!(response.discord_bot_token_updated_at, None);
        assert_eq!(response.discord_bot_token_last_validated_at, None);
        assert_eq!(response.discord_bot_user_id, None);
        assert_eq!(response.discord_bot_username, None);
        assert!(!response.is_admin);
    }

    #[test]
    fn guild_settings_response_honors_stored_values() {
        let response = guild_settings_response(
            &default_settings(),
            Some(stored_settings_with_token(true)),
            true,
        );

        assert_eq!(response.whisper_language.as_deref(), Some("fr"));
        assert!(response.whisper_language_explicit);
        assert!(response.whisper_vad);
        assert_eq!(response.auto_stop_grace_seconds, 300);
        assert_eq!(response.retention_raw_audio_ttl_days, 21);
        assert_eq!(response.retention_transcript_ttl_days, 90);
        assert!(!response.summary_enabled);
        assert!(response.discord_bot_token_registered);
        assert_eq!(
            response.discord_bot_token_updated_at.as_deref(),
            Some("2026-05-31T00:00:00Z")
        );
        assert_eq!(
            response.discord_bot_token_last_validated_at.as_deref(),
            Some("2026-05-31T00:01:00Z")
        );
        assert_eq!(response.discord_bot_user_id.as_deref(), Some("bot-1"));
        assert_eq!(response.discord_bot_username.as_deref(), Some("GuildBot"));
        assert!(response.is_admin);
    }

    #[test]
    fn guild_bot_token_update_validation_rejects_blank_or_oversized_token() {
        assert_eq!(
            normalize_guild_bot_token_update(&GuildBotTokenUpdateRequest {
                bot_token: "   ".to_owned()
            }),
            Err(StatusCode::BAD_REQUEST)
        );
        assert_eq!(
            normalize_guild_bot_token_update(&GuildBotTokenUpdateRequest {
                bot_token: "x".repeat(4097)
            }),
            Err(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn guild_settings_access_requires_admin() {
        assert_eq!(guild_admin_required_result(true), Ok(()));
        assert_eq!(
            guild_admin_required_result(false),
            Err(StatusCode::FORBIDDEN)
        );
    }

    #[test]
    fn guild_settings_update_checks_admin_before_domain_validation() {
        let mut request = valid_settings_request();
        request.auto_stop_grace_seconds = 0;

        assert_eq!(
            validate_authorized_guild_settings_update(false, &request),
            Err(StatusCode::FORBIDDEN)
        );
        assert_eq!(
            validate_authorized_guild_settings_update(true, &request),
            Err(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn guild_bot_token_update_checks_admin_before_token_validation() {
        let request = GuildBotTokenUpdateRequest {
            bot_token: "   ".to_owned(),
        };

        assert_eq!(
            validate_authorized_guild_bot_token_update(false, &request),
            Err(StatusCode::FORBIDDEN)
        );
        assert_eq!(
            validate_authorized_guild_bot_token_update(true, &request),
            Err(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn target_guild_settings_helpers_scope_to_requested_guild() {
        let auth = AuthConfig {
            client_id: "client".to_owned(),
            client_secret: "secret".to_owned(),
            session_secret: "session".to_owned(),
            redirect_uri: "http://localhost/auth/callback".to_owned(),
            guild_id: "guild-current".to_owned(),
            bot_token: "bot-token".to_owned(),
            secure_cookie: false,
        };

        let target = target_auth_config(&auth, "guild-target");

        assert_eq!(auth.guild_id, "guild-current");
        assert_eq!(target.guild_id, "guild-target");
        assert_eq!(target.bot_token, auth.bot_token);
        assert_eq!(
            target_guild_settings_path("guild-target", "/bot-token"),
            "/api/guilds/guild-target/settings/bot-token"
        );
    }

    #[test]
    fn target_guild_id_validation_fails_closed() {
        assert_eq!(
            normalize_target_guild_id(" guild-1 "),
            Ok("guild-1".to_owned())
        );
        assert_eq!(normalize_target_guild_id(""), Err(StatusCode::BAD_REQUEST));
        assert_eq!(
            normalize_target_guild_id("guild/1"),
            Err(StatusCode::BAD_REQUEST)
        );
        assert_eq!(
            normalize_target_guild_id("guild\n1"),
            Err(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn guild_admin_permission_cache_key_includes_target_guild() {
        assert_ne!(
            guild_admin_permission_cache_key("guild-a", "user-1"),
            guild_admin_permission_cache_key("guild-b", "user-1")
        );
        assert_eq!(
            guild_admin_permission_cache_key("guild-a", "user-1"),
            ("user-1".to_owned(), "__guild__:guild-a".to_owned())
        );
    }

    #[test]
    fn guild_bot_token_update_validation_trims_token() {
        assert_eq!(
            normalize_guild_bot_token_update(&GuildBotTokenUpdateRequest {
                bot_token: "  token-value  ".to_owned()
            }),
            Ok("token-value".to_owned())
        );
    }

    #[test]
    fn domain_knowledge_validation_accepts_and_trims_valid_request() {
        let normalized = normalize_domain_knowledge_request(&DomainKnowledgeUpsertRequest {
            content_type: "project_context".to_owned(),
            title: "  Launch plan  ".to_owned(),
            body: "  Internal wording guidance.  ".to_owned(),
            active: None,
        })
        .expect("valid domain knowledge request should normalize");

        assert_eq!(normalized.content_type.as_str(), "project_context");
        assert_eq!(normalized.title, "Launch plan");
        assert_eq!(normalized.body, "Internal wording guidance.");
        assert_eq!(normalized.active, None);
    }

    #[test]
    fn domain_knowledge_validation_rejects_invalid_type_and_blank_body() {
        assert_eq!(
            normalize_domain_knowledge_request(&DomainKnowledgeUpsertRequest {
                content_type: "secret".to_owned(),
                title: "Title".to_owned(),
                body: "Body".to_owned(),
                active: Some(true),
            }),
            Err(StatusCode::BAD_REQUEST)
        );
        assert_eq!(
            normalize_domain_knowledge_request(&DomainKnowledgeUpsertRequest {
                content_type: "glossary".to_owned(),
                title: "Title".to_owned(),
                body: "   ".to_owned(),
                active: Some(true),
            }),
            Err(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn domain_knowledge_list_filter_accepts_archived_and_type() {
        assert_eq!(
            normalize_domain_knowledge_list_filter(&DomainKnowledgeListQuery {
                include_archived: Some(true),
                content_type: Some("prohibited_item".to_owned()),
            }),
            Ok((true, "prohibited_item".to_owned()))
        );
        assert_eq!(
            normalize_domain_knowledge_list_filter(&DomainKnowledgeListQuery {
                include_archived: None,
                content_type: None,
            }),
            Ok((false, String::new()))
        );
    }

    #[test]
    fn domain_knowledge_item_id_validation_rejects_blank_or_oversized_ids() {
        assert_eq!(validate_domain_knowledge_item_id("dk-1"), Ok(()));
        assert_eq!(
            validate_domain_knowledge_item_id("   "),
            Err(StatusCode::BAD_REQUEST)
        );
        assert_eq!(
            validate_domain_knowledge_item_id(&"x".repeat(129)),
            Err(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn summary_template_validation_accepts_and_trims_valid_request() {
        let normalized = normalize_summary_template_request(&SummaryTemplateUpsertRequest {
            name: "  Default summary  ".to_owned(),
            template: "  Read {{ transcript_path }} and {{manifest_path}}.  ".to_owned(),
            active: None,
        })
        .expect("valid summary template request should normalize");

        assert_eq!(normalized.name, "Default summary");
        assert_eq!(
            normalized.template,
            "Read {{ transcript_path }} and {{manifest_path}}."
        );
        assert_eq!(
            normalized.variables,
            vec!["transcript_path".to_owned(), "manifest_path".to_owned()]
        );
        assert_eq!(normalized.active, None);
    }

    #[test]
    fn summary_template_validation_rejects_unknown_variables_and_blank_name() {
        assert_eq!(
            normalize_summary_template_request(&SummaryTemplateUpsertRequest {
                name: "Summary".to_owned(),
                template: "Read {{secret_path}}.".to_owned(),
                active: Some(true),
            }),
            Err(StatusCode::BAD_REQUEST)
        );
        assert_eq!(
            normalize_summary_template_request(&SummaryTemplateUpsertRequest {
                name: "   ".to_owned(),
                template: "Read {{transcript_path}}.".to_owned(),
                active: Some(true),
            }),
            Err(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn summary_template_update_checks_admin_before_template_validation() {
        let request = SummaryTemplateUpsertRequest {
            name: "Summary".to_owned(),
            template: "Read {{secret_path}}.".to_owned(),
            active: Some(true),
        };

        assert_eq!(
            validate_authorized_summary_template_request(false, &request),
            Err(StatusCode::FORBIDDEN)
        );
        assert_eq!(
            validate_authorized_summary_template_request(true, &request),
            Err(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn summary_template_id_validation_rejects_blank_or_oversized_ids() {
        assert_eq!(validate_summary_template_id("st-1"), Ok(()));
        assert_eq!(
            validate_summary_template_id("   "),
            Err(StatusCode::BAD_REQUEST)
        );
        assert_eq!(
            validate_summary_template_id(&"x".repeat(129)),
            Err(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn discord_bot_token_validation_status_maps_invalid_and_access_denied() {
        assert_eq!(
            classify_discord_bot_token_validation_status(
                DiscordBotTokenValidationStage::User,
                reqwest::StatusCode::UNAUTHORIZED,
            ),
            Some(DiscordBotTokenValidationError::InvalidToken)
        );
        assert_eq!(
            classify_discord_bot_token_validation_status(
                DiscordBotTokenValidationStage::Guild,
                reqwest::StatusCode::FORBIDDEN,
            ),
            Some(DiscordBotTokenValidationError::GuildAccessDenied)
        );
        assert_eq!(
            classify_discord_bot_token_validation_status(
                DiscordBotTokenValidationStage::Guild,
                reqwest::StatusCode::NOT_FOUND,
            ),
            Some(DiscordBotTokenValidationError::GuildAccessDenied)
        );
        assert_eq!(
            classify_discord_bot_token_validation_status(
                DiscordBotTokenValidationStage::Guild,
                reqwest::StatusCode::OK,
            ),
            None
        );
    }

    #[test]
    fn bot_token_revision_advances_on_update_notification() {
        let (sender, receiver) = watch::channel(u64::MAX);

        advance_bot_token_revision(&sender);

        assert_eq!(*receiver.borrow(), 0);
    }

    #[tokio::test]
    async fn bot_token_cache_refresh_failure_is_shared_with_followers() {
        let cache = Arc::new(RwLock::new(BotTokenCacheState::default()));
        let calls = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(6));
        let release = Arc::new(Notify::new());
        let tasks = (0..5)
            .map(|_| {
                let cache = cache.clone();
                let calls = calls.clone();
                let start = start.clone();
                let release = release.clone();
                tokio::spawn(async move {
                    start.wait().await;
                    bot_auth_header_from_cache_with_resolver(&cache, || {
                        let calls = calls.clone();
                        let release = release.clone();
                        async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            release.notified().await;
                            Err(StatusCode::BAD_GATEWAY)
                        }
                    })
                    .await
                })
            })
            .collect::<Vec<_>>();

        start.wait().await;
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if calls.load(Ordering::SeqCst) == 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("one refresh leader should start");
        release.notify_waiters();

        for task in tasks {
            assert_eq!(task.await.unwrap(), Err(StatusCode::BAD_GATEWAY));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stale_bot_token_cache_refresh_can_be_replaced() {
        let cache = Arc::new(RwLock::new(BotTokenCacheState::default()));
        let calls = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(Notify::new());
        let leader = {
            let cache = cache.clone();
            let calls = calls.clone();
            let release = release.clone();
            tokio::spawn(async move {
                bot_auth_header_from_cache_with_resolver(&cache, || {
                    let calls = calls.clone();
                    let release = release.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        release.notified().await;
                        Ok("stale-token".to_owned())
                    }
                })
                .await
            })
        };

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let has_refresh = cache.read().await.refresh.is_some();
                if calls.load(Ordering::SeqCst) == 1 && has_refresh {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("first refresh leader should install sentinel");
        leader.abort();
        let _ = leader.await;
        {
            let mut cache_state = cache.write().await;
            cache_state
                .refresh
                .as_mut()
                .expect("aborted leader leaves refresh sentinel")
                .started_at =
                Instant::now() - Duration::from_secs(BOT_TOKEN_CACHE_REFRESH_INFLIGHT_SECS + 1);
        }

        let replacement_calls = Arc::new(AtomicUsize::new(0));
        let result = bot_auth_header_from_cache_with_resolver(&cache, || {
            let replacement_calls = replacement_calls.clone();
            async move {
                replacement_calls.fetch_add(1, Ordering::SeqCst);
                Ok("fresh-token".to_owned())
            }
        })
        .await;

        assert_eq!(result, Ok("Bot fresh-token".to_owned()));
        assert_eq!(replacement_calls.load(Ordering::SeqCst), 1);
    }

    fn test_guild(owner_id: &str) -> DiscordGuildFull {
        DiscordGuildFull {
            owner_id: owner_id.to_owned(),
            roles: vec![DiscordRoleFull {
                id: "guild".to_owned(),
                permissions: 0,
            }],
        }
    }

    #[tokio::test]
    async fn guild_cache_miss_singleflight_does_not_hold_write_lock_during_fetch() {
        let cache: GuildCache = Arc::new(RwLock::new(GuildCacheState::default()));
        let calls = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(6));
        let release = Arc::new(Notify::new());
        let tasks = (0..5)
            .map(|_| {
                let cache = cache.clone();
                let calls = calls.clone();
                let start = start.clone();
                let release = release.clone();
                tokio::spawn(async move {
                    start.wait().await;
                    guild_info_from_cache_with_resolver(&cache, || {
                        let calls = calls.clone();
                        let release = release.clone();
                        async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            release.notified().await;
                            Ok(test_guild("owner-1"))
                        }
                    })
                    .await
                })
            })
            .collect::<Vec<_>>();

        start.wait().await;
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let has_refresh = cache.read().await.refresh.is_some();
                if calls.load(Ordering::SeqCst) == 1 && has_refresh {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("one guild refresh leader should start");

        {
            let _guard = tokio::time::timeout(Duration::from_millis(100), cache.read())
                .await
                .expect("readers should not queue behind a guild-cache write lock during fetch");
        }
        release.notify_waiters();

        for task in tasks {
            let guild = task.await.unwrap().expect("guild fetch should resolve");
            assert_eq!(guild.owner_id, "owner-1");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stale_guild_cache_refresh_can_be_replaced() {
        let cache: GuildCache = Arc::new(RwLock::new(GuildCacheState::default()));
        let calls = Arc::new(AtomicUsize::new(0));
        let block_fetch = Arc::new(Barrier::new(2));
        let release_fetch = Arc::new(Barrier::new(2));
        let leader = {
            let cache = cache.clone();
            let calls = calls.clone();
            let block_fetch = block_fetch.clone();
            let release_fetch = release_fetch.clone();
            tokio::spawn(async move {
                guild_info_from_cache_with_resolver(&cache, || {
                    let calls = calls.clone();
                    let block_fetch = block_fetch.clone();
                    let release_fetch = release_fetch.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        block_fetch.wait().await;
                        release_fetch.wait().await;
                        Ok(test_guild("stale-owner"))
                    }
                })
                .await
            })
        };

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let has_refresh = cache.read().await.refresh.is_some();
                if calls.load(Ordering::SeqCst) == 1 && has_refresh {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("first guild refresh leader should install sentinel");
        leader.abort();
        let _ = leader.await;
        {
            let mut cache_state = cache.write().await;
            cache_state
                .refresh
                .as_mut()
                .expect("aborted leader leaves guild refresh sentinel")
                .started_at =
                Instant::now() - Duration::from_secs(GUILD_CACHE_REFRESH_INFLIGHT_SECS + 1);
        }

        let replacement_calls = Arc::new(AtomicUsize::new(0));
        let result = guild_info_from_cache_with_resolver(&cache, || {
            let replacement_calls = replacement_calls.clone();
            async move {
                replacement_calls.fetch_add(1, Ordering::SeqCst);
                Ok(test_guild("fresh-owner"))
            }
        })
        .await
        .expect("replacement guild fetch should resolve");

        assert_eq!(result.owner_id, "fresh-owner");
        assert_eq!(replacement_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn guild_cache_revision_change_rejects_stale_leader_result() {
        let cache: GuildCache = Arc::new(RwLock::new(GuildCacheState::default()));
        let calls = Arc::new(AtomicUsize::new(0));
        let release_fetch = Arc::new(tokio::sync::Semaphore::new(0));
        let leader = {
            let cache = cache.clone();
            let calls = calls.clone();
            let release_fetch = release_fetch.clone();
            tokio::spawn(async move {
                guild_info_from_cache_with_resolver(&cache, || {
                    let calls = calls.clone();
                    let release_fetch = release_fetch.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        let _permit = release_fetch
                            .acquire()
                            .await
                            .expect("test semaphore should stay open");
                        Ok(test_guild("stale-owner"))
                    }
                })
                .await
            })
        };

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let has_refresh = cache.read().await.refresh.is_some();
                if calls.load(Ordering::SeqCst) == 1 && has_refresh {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("first guild refresh leader should install sentinel");

        let refresh = {
            let mut cache_state = cache.write().await;
            cache_state.revision = cache_state.revision.wrapping_add(1);
            cache_state.refresh.take()
        };
        if let Some(refresh) = refresh {
            refresh.notify.notify_waiters();
        }
        release_fetch.add_permits(1);

        let result = tokio::time::timeout(Duration::from_secs(1), leader)
            .await
            .expect("stale leader should finish after release")
            .unwrap();
        match result {
            Err(status) => assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE),
            Ok(guild) => panic!("stale leader unexpectedly cached guild {}", guild.owner_id),
        }
        let cache_state = cache.read().await;
        assert!(cache_state.entry.is_none());
        assert!(cache_state.failure.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn positive_permission_cache_entries_use_reverify_window() {
        let admin_allow = CachedChannelPermission {
            can_view: true,
            is_admin: true,
        };
        let admin_deny = CachedChannelPermission {
            can_view: false,
            is_admin: false,
        };

        assert_eq!(
            permission_cache_ttl(admin_allow),
            PERMISSION_CACHE_SENSITIVE_POSITIVE_TTL_SECS
        );
        assert_eq!(
            permission_cache_ttl(admin_deny),
            super::PERMISSION_CACHE_TTL_SECS
        );
    }

    #[test]
    fn guild_admin_member_status_treats_bot_auth_failures_as_upstream_errors() {
        assert_eq!(
            guild_admin_member_status_decision(reqwest::StatusCode::NOT_FOUND),
            Some(GuildAdminCheck::NotAdmin)
        );
        assert_eq!(
            guild_admin_member_status_decision(reqwest::StatusCode::FORBIDDEN),
            Some(GuildAdminCheck::BotAccessDenied)
        );
        assert_eq!(
            guild_admin_member_status_decision(reqwest::StatusCode::UNAUTHORIZED),
            Some(GuildAdminCheck::BotAccessDenied)
        );
        assert_eq!(
            guild_admin_member_status_decision(reqwest::StatusCode::TOO_MANY_REQUESTS),
            Some(GuildAdminCheck::RateLimited)
        );
        assert_eq!(
            guild_admin_member_status_decision(reqwest::StatusCode::OK),
            None
        );
    }

    #[test]
    fn guild_admin_check_bot_access_denied_fails_closed() {
        assert_eq!(
            GuildAdminCheck::BotAccessDenied.into_status_result(),
            Err(StatusCode::BAD_GATEWAY)
        );
        assert_eq!(
            GuildAdminCheck::RateLimited.into_status_result(),
            Err(StatusCode::BAD_GATEWAY)
        );
        assert_eq!(GuildAdminCheck::NotAdmin.into_status_result(), Ok(false));
    }

    #[test]
    fn settings_admin_check_retries_global_for_token_recovery_failures() {
        assert!(!super::should_retry_settings_admin_check_with_global(&Ok(
            super::GuildAdminCheck::Admin
        )));
        assert!(!super::should_retry_settings_admin_check_with_global(&Ok(
            super::GuildAdminCheck::NotAdmin
        )));
        assert!(super::should_retry_settings_admin_check_with_global(&Ok(
            super::GuildAdminCheck::BotAccessDenied
        )));
        assert!(!super::should_retry_settings_admin_check_with_global(&Ok(
            super::GuildAdminCheck::RateLimited
        )));
        assert!(super::should_retry_settings_admin_check_with_global(&Err(
            StatusCode::BAD_GATEWAY
        )));
        assert!(super::should_retry_settings_admin_check_with_global(&Err(
            StatusCode::SERVICE_UNAVAILABLE
        )));
        assert!(!super::should_retry_settings_admin_check_with_global(&Err(
            StatusCode::INTERNAL_SERVER_ERROR
        )));
    }

    #[test]
    fn guild_meetings_pagination_is_bounded() {
        assert_eq!(
            normalize_guild_meetings_pagination(&GuildMeetingsQuery {
                page: None,
                limit: None,
                voice_channel_id: None
            }),
            (1, 20)
        );
        assert_eq!(
            normalize_guild_meetings_pagination(&GuildMeetingsQuery {
                page: Some(0),
                limit: Some(0),
                voice_channel_id: None
            }),
            (1, 1)
        );
        assert_eq!(
            normalize_guild_meetings_pagination(&GuildMeetingsQuery {
                page: Some(2),
                limit: Some(250),
                voice_channel_id: None
            }),
            (2, 100)
        );
    }

    #[test]
    fn guild_meetings_voice_channel_filter_ignores_blank_values() {
        assert_eq!(
            normalize_guild_meetings_voice_channel_id(&GuildMeetingsQuery {
                page: None,
                limit: None,
                voice_channel_id: None,
            }),
            None
        );
        assert_eq!(
            normalize_guild_meetings_voice_channel_id(&GuildMeetingsQuery {
                page: None,
                limit: None,
                voice_channel_id: Some("  ".to_owned()),
            }),
            None
        );
        assert_eq!(
            normalize_guild_meetings_voice_channel_id(&GuildMeetingsQuery {
                page: None,
                limit: None,
                voice_channel_id: Some(" vc-2 ".to_owned()),
            }),
            Some("vc-2".to_owned())
        );
    }

    #[test]
    fn target_guild_meetings_require_visible_installed_guild() {
        let visible = vec![
            visible_guild("guild-current", "Current", 0),
            visible_guild("guild-target", "Target", 0),
        ];
        let tenant_by_guild_id =
            HashMap::from([("guild-target".to_owned(), "tenant-target".to_owned())]);

        assert!(user_can_access_target_guild(&visible, "guild-target"));
        assert!(target_guild_has_active_installation(
            &tenant_by_guild_id,
            "guild-target"
        ));
        assert!(!user_can_access_target_guild(&visible, "guild-stale"));
        assert!(!target_guild_has_active_installation(
            &tenant_by_guild_id,
            "guild-stale"
        ));
    }

    #[test]
    fn guild_meetings_queries_filter_guild_before_pagination() {
        let list_sql = LIST_GUILD_MEETINGS_SQL.to_ascii_uppercase();
        let count_sql = COUNT_GUILD_MEETINGS_SQL.to_ascii_uppercase();
        let visibility_sql = super::LIST_GUILD_MEETINGS_FOR_VISIBILITY_SQL.to_ascii_uppercase();
        let channel_sql = LIST_GUILD_MEETING_VOICE_CHANNELS_SQL.to_ascii_uppercase();

        let list_where = list_sql.find("WHERE GUILD_ID = $1").unwrap();
        let list_voice = list_sql.find("VOICE_CHANNEL_ID = $2").unwrap();
        let list_order = list_sql.find("ORDER BY").unwrap();
        let list_limit = list_sql.find("LIMIT $3").unwrap();
        let list_offset = list_sql.find("OFFSET $4").unwrap();
        assert!(list_where < list_order);
        assert!(list_where < list_voice);
        assert!(list_voice < list_order);
        assert!(list_where < list_limit);
        assert!(list_where < list_offset);

        let visibility_where = visibility_sql.find("WHERE GUILD_ID = $1").unwrap();
        let visibility_voice = visibility_sql.find("VOICE_CHANNEL_ID = $2").unwrap();
        let visibility_limit = visibility_sql.find("LIMIT $3").unwrap();
        assert!(visibility_where < visibility_voice);
        assert!(visibility_voice < visibility_limit);
        assert!(visibility_where < visibility_limit);
        assert!(!visibility_sql.contains("OFFSET"));

        assert!(count_sql.contains("WHERE GUILD_ID = $1"));
        assert!(count_sql.contains("VOICE_CHANNEL_ID = $2"));
        assert!(channel_sql.contains("WHERE GUILD_ID = $1"));
        assert!(!channel_sql.contains("VOICE_CHANNEL_ID = $2"));
    }

    #[test]
    fn non_admin_guild_meetings_visibility_scan_is_bounded() {
        assert_eq!(
            guild_meetings_visibility_scan_limit(1, 20),
            i64::from(GUILD_MEETINGS_VISIBILITY_SCAN_MIN)
        );
        assert_eq!(guild_meetings_visibility_scan_limit(2, 100), 200);
        assert_eq!(
            guild_meetings_visibility_scan_limit(10_000, 100),
            i64::from(GUILD_MEETINGS_VISIBILITY_SCAN_LIMIT)
        );
        assert_eq!(
            guild_meetings_visibility_scan_limit(1, GUILD_MEETINGS_VISIBILITY_SCAN_LIMIT + 1),
            i64::from(GUILD_MEETINGS_VISIBILITY_SCAN_LIMIT)
        );
    }

    #[test]
    fn non_admin_guild_meetings_channel_scan_is_bounded() {
        assert!(can_scan_new_visibility_channel(0));
        assert!(can_scan_new_visibility_channel(
            GUILD_MEETINGS_VISIBILITY_CHANNEL_CAP - 1
        ));
        assert!(!can_scan_new_visibility_channel(
            GUILD_MEETINGS_VISIBILITY_CHANNEL_CAP
        ));
    }
}

#[cfg(test)]
mod discord_channel_full_tests {
    use super::{
        CachedChannelPermission, DiscordChannelFull, DiscordOverwrite, DiscordOverwriteType,
        DiscordRoleFull, PERMISSION_CACHE_SENSITIVE_POSITIVE_TTL_SECS, PERMISSION_CACHE_TTL_SECS,
        PermissionCache, VIEW_CHANNEL, authorize_debug_artifact_download,
        build_content_disposition, compute_channel_permissions, debug_artifact_requires_admin,
        guild_meeting_channel_visible_after_row, guild_meetings_response_total,
        meeting_access_from_row, verify_meeting_access_after_row,
    };
    use axum::http::StatusCode;
    use std::collections::HashMap;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::{Duration, Instant};

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

    #[tokio::test]
    async fn sensitive_access_denies_after_cached_allow_reverify_window() {
        let cache: PermissionCache = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        cache.write().await.insert(
            ("user".to_owned(), "voice".to_owned()),
            (
                CachedChannelPermission {
                    can_view: true,
                    is_admin: false,
                },
                Instant::now()
                    - Duration::from_secs(PERMISSION_CACHE_SENSITIVE_POSITIVE_TTL_SECS + 1),
            ),
        );
        let permission_check_called = Arc::new(AtomicBool::new(false));
        let permission_check_called_in_future = Arc::clone(&permission_check_called);

        let result = verify_meeting_access_after_row(
            "auth-guild".to_owned(),
            "voice".to_owned(),
            "auth-guild",
            "user",
            &cache,
            async move {
                permission_check_called_in_future.store(true, Ordering::SeqCst);
                Ok(CachedChannelPermission {
                    can_view: false,
                    is_admin: false,
                })
            },
        )
        .await;

        assert!(matches!(result, Err(StatusCode::FORBIDDEN)));
        assert!(permission_check_called.load(Ordering::SeqCst));
        let cache = cache.read().await;
        let (permission, expires_at) = cache
            .get(&("user".to_owned(), "voice".to_owned()))
            .expect("denial should replace stale allow");
        assert!(!permission.can_view);
        assert!(*expires_at > Instant::now() + Duration::from_secs(PERMISSION_CACHE_TTL_SECS / 2));
    }

    #[tokio::test]
    async fn guild_meeting_channel_visibility_omits_forbidden_rows() {
        let cache: PermissionCache = Arc::new(tokio::sync::RwLock::new(HashMap::new()));

        let result = guild_meeting_channel_visible_after_row(
            "auth-guild".to_owned(),
            "private-voice".to_owned(),
            "auth-guild",
            "user",
            &cache,
            async {
                Ok(CachedChannelPermission {
                    can_view: false,
                    is_admin: false,
                })
            },
        )
        .await;

        assert_eq!(result, Ok(false));
    }

    #[tokio::test]
    async fn guild_meeting_channel_visibility_allows_viewable_rows() {
        let cache: PermissionCache = Arc::new(tokio::sync::RwLock::new(HashMap::new()));

        let result = guild_meeting_channel_visible_after_row(
            "auth-guild".to_owned(),
            "public-voice".to_owned(),
            "auth-guild",
            "user",
            &cache,
            async {
                Ok(CachedChannelPermission {
                    can_view: true,
                    is_admin: false,
                })
            },
        )
        .await;

        assert_eq!(result, Ok(true));
    }

    #[test]
    fn guild_meetings_total_preserves_admin_count_and_hides_member_count() {
        assert_eq!(guild_meetings_response_total(true, 42, 3), 42);
        assert_eq!(guild_meetings_response_total(false, 42, 3), 3);
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
        MEMBERSHIP_REVERIFY_INFLIGHT_SECS, MembershipCache, MembershipInflightEntry,
        MembershipReverifyInflight, MembershipReverifyStart,
        SESSION_MEMBERSHIP_VERIFY_INTERVAL_SECS, SESSION_TTL_SECS, SessionPayload,
        allows_settings_token_recovery, begin_membership_reverify, cache_guild_membership,
        cached_guild_membership, guild_member_status_indicates_membership,
        guild_membership_forbidden_response, publish_membership_reverify_result,
        session_matches_guild, session_needs_cookie_refresh,
        should_retry_settings_membership_check_with_global, sign_session,
        verify_guild_membership_reverify_with, verify_session,
    };
    use axum::http::StatusCode as AxumStatusCode;
    use reqwest::StatusCode as HttpStatus;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};
    use tokio::sync::{Mutex, Notify, RwLock};

    const SECRET: &str = "test-session-secret";

    fn membership_cache() -> MembershipCache {
        Arc::new(RwLock::new(HashMap::new()))
    }

    fn membership_inflight() -> MembershipReverifyInflight {
        Arc::new(Mutex::new(HashMap::new()))
    }

    #[test]
    fn settings_token_recovery_paths_are_limited() {
        assert!(allows_settings_token_recovery("/api/me"));
        assert!(allows_settings_token_recovery("/api/me/guilds"));
        assert!(allows_settings_token_recovery("/api/guild/settings"));
        assert!(allows_settings_token_recovery(
            "/api/guild/settings/bot-token"
        ));
        assert!(allows_settings_token_recovery("/api/guilds/123/settings"));
        assert!(allows_settings_token_recovery(
            "/api/guilds/123/settings/bot-token"
        ));
        assert!(!allows_settings_token_recovery(
            "/api/guild/settings/notifications"
        ));
        assert!(!allows_settings_token_recovery(
            "/api/guilds/123/settings/notifications"
        ));
        assert!(!allows_settings_token_recovery("/api/meetings/meeting-1"));
        assert!(!allows_settings_token_recovery("/"));
    }

    #[test]
    fn settings_membership_recovery_does_not_retry_rate_limits() {
        assert!(should_retry_settings_membership_check_with_global(&Err(
            AxumStatusCode::BAD_GATEWAY
        )));
        assert!(should_retry_settings_membership_check_with_global(&Err(
            AxumStatusCode::SERVICE_UNAVAILABLE
        )));
        assert!(!should_retry_settings_membership_check_with_global(&Err(
            AxumStatusCode::TOO_MANY_REQUESTS
        )));
        assert!(!should_retry_settings_membership_check_with_global(&Ok(
            false
        )));
        assert!(!should_retry_settings_membership_check_with_global(&Ok(
            true
        )));
    }

    #[tokio::test]
    async fn forced_membership_reverify_replaces_cached_retriable_error() {
        let cache = membership_cache();
        let inflight = membership_inflight();
        let calls = Arc::new(AtomicUsize::new(0));
        cache_guild_membership(&cache, "user-1", Err(AxumStatusCode::BAD_GATEWAY)).await;

        let result =
            verify_guild_membership_reverify_with(&cache, &inflight, "user-1", false, || {
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(true)
                }
            })
            .await;

        assert_eq!(result, Ok(true));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            cached_guild_membership(&cache, "user-1").await,
            Some(Ok(true))
        );
    }

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
        assert!(session_needs_cookie_refresh(&session));
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
        assert!(!session_needs_cookie_refresh(&session));
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
        assert!(!session_needs_cookie_refresh(&session));
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
        assert!(!session_needs_cookie_refresh(&session));
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

    #[tokio::test]
    async fn fresh_membership_cache_entry_is_reused() {
        let cache = membership_cache();

        cache_guild_membership(&cache, "user-1", Ok(true)).await;

        assert_eq!(
            cached_guild_membership(&cache, "user-1").await,
            Some(Ok(true))
        );
    }

    #[tokio::test]
    async fn expired_membership_cache_entry_is_ignored() {
        let cache = membership_cache();
        cache.write().await.insert(
            "user-1".to_owned(),
            (Ok(true), Instant::now() - Duration::from_secs(1)),
        );

        assert_eq!(cached_guild_membership(&cache, "user-1").await, None);
    }

    #[tokio::test]
    async fn same_user_membership_reverify_is_deduplicated_until_finished() {
        let inflight = membership_inflight();
        let cache = membership_cache();

        let leader_notify = match begin_membership_reverify(&inflight, "user-1").await {
            MembershipReverifyStart::Leader(notify) => notify,
            MembershipReverifyStart::Follower(_) => panic!("first caller must lead"),
        };
        match begin_membership_reverify(&inflight, "user-1").await {
            MembershipReverifyStart::Leader(_) => panic!("second caller must follow"),
            MembershipReverifyStart::Follower(_) => {}
        }

        assert!(
            publish_membership_reverify_result(
                &inflight,
                &cache,
                "user-1",
                &leader_notify,
                Ok(true)
            )
            .await
        );

        match begin_membership_reverify(&inflight, "user-1").await {
            MembershipReverifyStart::Leader(_) => {}
            MembershipReverifyStart::Follower(_) => panic!("new caller must lead after cleanup"),
        }
    }

    #[tokio::test]
    async fn stale_membership_reverify_inflight_entry_is_replaced() {
        let inflight = membership_inflight();
        let stale_started_at =
            Instant::now() - Duration::from_secs(MEMBERSHIP_REVERIFY_INFLIGHT_SECS + 1);
        inflight.lock().await.insert(
            "user-1".to_owned(),
            MembershipInflightEntry {
                notify: Arc::new(Notify::new()),
                started_at: stale_started_at,
            },
        );

        match begin_membership_reverify(&inflight, "user-1").await {
            MembershipReverifyStart::Leader(_) => {}
            MembershipReverifyStart::Follower(_) => panic!("stale entry must be replaced"),
        }

        let map = inflight.lock().await;
        let entry = map.get("user-1").expect("fresh entry exists");
        assert!(entry.started_at > stale_started_at);
    }

    #[tokio::test]
    async fn stale_leader_cannot_remove_replacement_inflight_entry() {
        let inflight = membership_inflight();
        let cache = membership_cache();
        let stale_leader_notify = match begin_membership_reverify(&inflight, "user-1").await {
            MembershipReverifyStart::Leader(notify) => notify,
            MembershipReverifyStart::Follower(_) => panic!("first caller must lead"),
        };
        {
            let mut map = inflight.lock().await;
            map.get_mut("user-1")
                .expect("leader entry exists")
                .started_at =
                Instant::now() - Duration::from_secs(MEMBERSHIP_REVERIFY_INFLIGHT_SECS + 1);
        }
        let replacement_notify = match begin_membership_reverify(&inflight, "user-1").await {
            MembershipReverifyStart::Leader(notify) => notify,
            MembershipReverifyStart::Follower(_) => panic!("stale entry must be replaced"),
        };

        assert!(
            !publish_membership_reverify_result(
                &inflight,
                &cache,
                "user-1",
                &stale_leader_notify,
                Ok(true),
            )
            .await
        );

        let map = inflight.lock().await;
        let entry = map.get("user-1").expect("replacement must remain");
        assert!(Arc::ptr_eq(&entry.notify, &replacement_notify));
    }

    #[tokio::test]
    async fn stale_leader_cannot_overwrite_replacement_membership_cache() {
        let inflight = membership_inflight();
        let cache = membership_cache();
        let stale_leader_notify = match begin_membership_reverify(&inflight, "user-1").await {
            MembershipReverifyStart::Leader(notify) => notify,
            MembershipReverifyStart::Follower(_) => panic!("first caller must lead"),
        };
        {
            let mut map = inflight.lock().await;
            map.get_mut("user-1")
                .expect("leader entry exists")
                .started_at =
                Instant::now() - Duration::from_secs(MEMBERSHIP_REVERIFY_INFLIGHT_SECS + 1);
        }
        let replacement_notify = match begin_membership_reverify(&inflight, "user-1").await {
            MembershipReverifyStart::Leader(notify) => notify,
            MembershipReverifyStart::Follower(_) => panic!("stale entry must be replaced"),
        };

        assert!(
            publish_membership_reverify_result(
                &inflight,
                &cache,
                "user-1",
                &replacement_notify,
                Ok(false),
            )
            .await
        );
        assert!(
            !publish_membership_reverify_result(
                &inflight,
                &cache,
                "user-1",
                &stale_leader_notify,
                Ok(true),
            )
            .await
        );

        assert_eq!(
            cached_guild_membership(&cache, "user-1").await,
            Some(Ok(false))
        );
    }

    #[tokio::test]
    async fn stale_membership_reverify_sweep_reclaims_other_users() {
        let inflight = membership_inflight();
        let stale_started_at =
            Instant::now() - Duration::from_secs(MEMBERSHIP_REVERIFY_INFLIGHT_SECS + 1);
        inflight.lock().await.insert(
            "stale-user".to_owned(),
            MembershipInflightEntry {
                notify: Arc::new(Notify::new()),
                started_at: stale_started_at,
            },
        );

        match begin_membership_reverify(&inflight, "fresh-user").await {
            MembershipReverifyStart::Leader(_) => {}
            MembershipReverifyStart::Follower(_) => panic!("fresh user must lead"),
        }

        let map = inflight.lock().await;
        assert!(!map.contains_key("stale-user"));
        assert!(map.contains_key("fresh-user"));
    }

    #[test]
    fn api_membership_denial_uses_forbidden_status() {
        assert_eq!(
            guild_membership_forbidden_response().status(),
            axum::http::StatusCode::FORBIDDEN
        );
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
    fn expired_session_cookie_is_rejected() {
        let now = super::unix_now_secs();
        let cookie = super::sign_session_with_exp(
            "user-1",
            "guild-1",
            SECRET,
            now.saturating_sub(1),
            now.saturating_sub(SESSION_TTL_SECS),
            now.saturating_sub(SESSION_TTL_SECS),
            0,
        );

        assert!(verify_session(&cookie, SECRET).is_none());
    }

    #[test]
    fn signed_session_must_match_configured_guild() {
        let now = super::unix_now_secs();
        let cookie = sign_session("user-1", "guild-1", SECRET, now, now);
        let session = verify_session(&cookie, SECRET).expect("session should verify");

        assert!(session_matches_guild(&session, "guild-1"));
        assert!(!session_matches_guild(&session, "guild-2"));
    }

    #[test]
    fn stale_verified_at_session_still_parses_for_reverify_gate() {
        let now = super::unix_now_secs();
        let stale = now.saturating_sub(SESSION_MEMBERSHIP_VERIFY_INTERVAL_SECS + 120);
        let cookie = sign_session("user-1", "guild-1", SECRET, stale, stale);
        // issued_at preserved separately from verified_at in production cookies
        let session = verify_session(&cookie, SECRET).expect("session should verify");
        assert!(session_needs_cookie_refresh(&session));
    }
}

#[cfg(test)]
mod transcript_source_api_tests {
    use super::{
        SpeakerResponse, TranscriptSegmentResponse, api_transcript_sql,
        sort_transcript_segment_responses, transcript_source_for_api,
    };
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

    #[test]
    fn api_transcript_orders_by_rebased_live_timestamps() {
        let sql = api_transcript_sql();

        assert!(sql.contains("END AS start_ms"));
        assert!(sql.contains("END AS end_ms"));
        assert!(
            sql.contains("NOT fb.has_final_rows AND c.id IS NOT NULL"),
            "live rows should not be exposed after final transcript rows exist"
        );
        assert!(
            sql.contains("(t.created_at, t.id)"),
            "streaming cursor should advance over stable transcript row identity"
        );
        assert!(
            sql.contains("ORDER BY t.created_at, t.id"),
            "database order must match the streaming cursor"
        );
        assert!(
            !sql.contains("ORDER BY t.start_ms, t.end_ms"),
            "raw live timestamps can be out of final timeline order after rebasing"
        );
    }

    #[test]
    fn api_transcript_response_uses_canonical_timeline_order() {
        let mut segments = vec![
            transcript_segment_response(
                "late-alice",
                "alice",
                2_200,
                2_600,
                TranscriptSource::Voice,
            ),
            transcript_segment_response("early-alice", "alice", 0, 5_000, TranscriptSource::Voice),
            transcript_segment_response("bob", "bob", 1_200, 1_800, TranscriptSource::Voice),
            transcript_segment_response("vc", "carol", 1_200, 1_800, TranscriptSource::VcText),
        ];

        sort_transcript_segment_responses(&mut segments);

        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.id.as_str())
                .collect::<Vec<_>>(),
            vec!["early-alice", "bob", "vc", "late-alice"]
        );
    }

    fn transcript_segment_response(
        id: &str,
        speaker_id: &str,
        start_ms: i32,
        end_ms: i32,
        source: TranscriptSource,
    ) -> TranscriptSegmentResponse {
        TranscriptSegmentResponse {
            id: id.to_owned(),
            speaker_id: speaker_id.to_owned(),
            speaker: SpeakerResponse {
                id: speaker_id.to_owned(),
                username: None,
                nickname: None,
                display_name: None,
                display_label: speaker_id.to_owned(),
            },
            start_ms,
            end_ms,
            text: id.to_owned(),
            confidence: None,
            is_noisy: false,
            source: source.as_str().to_owned(),
        }
    }
}

#[cfg(test)]
mod transcript_sse_limit_tests {
    use super::{
        TRANSCRIPT_SSE_BASE_POLL_SECS, TRANSCRIPT_SSE_MAX_IDLE_POLLS,
        TRANSCRIPT_SSE_MAX_PER_USER_MEETING, TRANSCRIPT_SSE_MAX_POLL_SECS, TranscriptSseLimiter,
        next_transcript_sse_idle_polls, next_transcript_sse_poll_delay,
        transcript_sse_idle_limit_reached, try_acquire_transcript_sse_permit,
    };
    use axum::http::StatusCode;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    fn limiter() -> TranscriptSseLimiter {
        Arc::new(std::sync::Mutex::new(HashMap::new()))
    }

    #[test]
    fn transcript_sse_per_user_meeting_cap_rejects_extra_connection() {
        let limiter = limiter();
        let mut permits = Vec::new();
        for _ in 0..TRANSCRIPT_SSE_MAX_PER_USER_MEETING {
            permits.push(
                try_acquire_transcript_sse_permit(&limiter, "user-1", "meeting-1")
                    .expect("connection below cap should be accepted"),
            );
        }

        assert_eq!(
            try_acquire_transcript_sse_permit(&limiter, "user-1", "meeting-1")
                .map(|_| ())
                .unwrap_err(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert!(
            try_acquire_transcript_sse_permit(&limiter, "user-1", "meeting-2").is_ok(),
            "the cap is per user and meeting"
        );
        drop(permits);
    }

    #[test]
    fn transcript_sse_permit_drop_releases_slot() {
        let limiter = limiter();
        let permit = try_acquire_transcript_sse_permit(&limiter, "user-1", "meeting-1")
            .expect("first connection should be accepted");
        assert_eq!(
            limiter
                .lock()
                .expect("limiter lock should not be poisoned")
                .get(&("user-1".to_owned(), "meeting-1".to_owned()))
                .copied(),
            Some(1)
        );

        drop(permit);

        assert!(
            limiter
                .lock()
                .expect("limiter lock should not be poisoned")
                .is_empty()
        );
        assert!(try_acquire_transcript_sse_permit(&limiter, "user-1", "meeting-1").is_ok());
    }

    #[test]
    fn transcript_sse_polling_backs_off_and_resets_on_segments() {
        let base = Duration::from_secs(TRANSCRIPT_SSE_BASE_POLL_SECS);
        let max = Duration::from_secs(TRANSCRIPT_SSE_MAX_POLL_SECS);

        assert_eq!(next_transcript_sse_poll_delay(Duration::ZERO, false), base);
        assert_eq!(next_transcript_sse_poll_delay(base, false), base * 2);
        assert_eq!(next_transcript_sse_poll_delay(max, false), max);
        assert_eq!(next_transcript_sse_poll_delay(max, true), base);
    }

    #[test]
    fn transcript_sse_idle_counter_is_bounded_and_resets_on_segments() {
        assert_eq!(next_transcript_sse_idle_polls(0, false), 1);
        assert_eq!(next_transcript_sse_idle_polls(42, true), 0);
        assert!(!transcript_sse_idle_limit_reached(
            TRANSCRIPT_SSE_MAX_IDLE_POLLS - 1
        ));
        assert!(transcript_sse_idle_limit_reached(
            TRANSCRIPT_SSE_MAX_IDLE_POLLS
        ));
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
