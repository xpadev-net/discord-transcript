use std::collections::HashMap;
use std::env;
use std::fmt::{Display, Formatter};
use std::num::NonZeroU32;

use crate::domain::retention::RetentionPolicy;
use crate::infrastructure::s3::{
    DEFAULT_PRESIGN_TTL_SECONDS, DEFAULT_REGION, MAX_PRESIGN_TTL_SECONDS, S3Settings,
    validate_endpoint,
};

/// Process role used to decide which config surface must be present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppRole {
    All,
    WebBot,
    Worker,
}

impl AppRole {
    pub fn parse(raw: &str) -> Result<Self, ConfigError> {
        let key = "APP_ROLE";
        match raw.trim().to_ascii_lowercase().as_str() {
            "all" => Ok(Self::All),
            "web-bot" => Ok(Self::WebBot),
            "worker" => Ok(Self::Worker),
            _ => Err(ConfigError::InvalidEnv {
                key,
                value: raw.to_owned(),
            }),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::WebBot => "web-bot",
            Self::Worker => "worker",
        }
    }

    const fn requires_discord_gateway_config(self) -> bool {
        matches!(self, Self::All | Self::WebBot)
    }

    const fn requires_summary_harness_config(self) -> bool {
        matches!(self, Self::All | Self::Worker)
    }
}

impl Display for AppRole {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Storage backend that owns the canonical copy of recorded audio,
/// selected by `CHUNK_STORAGE_BACKEND`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkStorageBackend {
    /// Local filesystem under `CHUNK_STORAGE_DIR` (default).
    Local,
    /// S3-compatible object storage. Local files still exist as recording-time
    /// staging under `CHUNK_STORAGE_DIR` but S3 is the durable store; playback
    /// is served via presigned GET URLs.
    S3,
}

impl ChunkStorageBackend {
    pub fn parse(raw: &str) -> Result<Self, ConfigError> {
        let key = "CHUNK_STORAGE_BACKEND";
        match raw.trim().to_ascii_lowercase().as_str() {
            "local" => Ok(Self::Local),
            "s3" => Ok(Self::S3),
            _ => Err(ConfigError::InvalidEnv {
                key,
                value: raw.to_owned(),
            }),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::S3 => "s3",
        }
    }
}

impl Display for ChunkStorageBackend {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which CLI drives meeting summary and transcript correction (`summarize` integration).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryHarness {
    Claude,
    CursorAgent,
    OpenCode,
    /// In-process rig-based agent (`src/infrastructure/agent.rs`); no CLI.
    Native,
}

impl SummaryHarness {
    pub fn parse(raw: &str) -> Result<Self, ConfigError> {
        let key = "SUMMARY_HARNESS";
        match raw.trim().to_ascii_lowercase().as_str() {
            "claude" => Ok(Self::Claude),
            "cursor_agent" => Ok(Self::CursorAgent),
            "opencode" => Ok(Self::OpenCode),
            "native" => Ok(Self::Native),
            _ => Err(ConfigError::InvalidEnv {
                key,
                value: raw.to_owned(),
            }),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::CursorAgent => "cursor_agent",
            Self::OpenCode => "opencode",
            Self::Native => "native",
        }
    }

    /// Whether this harness shells out to a coding CLI (vs the in-process agent).
    pub const fn is_cli(self) -> bool {
        !matches!(self, Self::Native)
    }
}

/// Model provider for `SUMMARY_HARNESS=native` (`SUMMARY_PROVIDER`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryProvider {
    /// OpenCode Go subscription: API key from `OPENCODE_API_KEY`, models on
    /// `opencode.ai/zen/go/v1` (Responses or Chat Completions by model).
    OpenCodeGo,
}

impl SummaryProvider {
    pub fn parse(raw: &str) -> Result<Self, ConfigError> {
        let key = "SUMMARY_PROVIDER";
        match raw.trim().to_ascii_lowercase().as_str() {
            "opencode_go" | "opencode-go" => Ok(Self::OpenCodeGo),
            _ => Err(ConfigError::InvalidEnv {
                key,
                value: raw.to_owned(),
            }),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenCodeGo => "opencode_go",
        }
    }

    /// Env var the provider's credential is read from, when it needs one.
    pub const fn api_key_env(self) -> &'static str {
        match self {
            Self::OpenCodeGo => "OPENCODE_API_KEY",
        }
    }
}

impl Display for SummaryProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Display for SummaryHarness {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppConfig {
    pub app_role: AppRole,
    pub discord_token: String,
    pub discord_guild_id: String,
    pub whisper_endpoint: String,
    pub summary_harness: SummaryHarness,
    pub summary_command: String,
    pub summary_model: String,
    /// Provider behind `SUMMARY_HARNESS=native` (`SUMMARY_PROVIDER`).
    pub summary_provider: Option<SummaryProvider>,
    /// Credential for `summary_provider` (`<provider>.api_key_env()`).
    pub summary_api_key: Option<String>,
    pub summary_allow_unsafe_agent_harness: bool,
    pub summary_enabled: bool,
    pub database_url: String,
    pub database_ssl_mode: String,
    pub chunk_storage_dir: String,
    pub chunk_storage_backend: ChunkStorageBackend,
    /// Present iff `chunk_storage_backend == S3`.
    pub chunk_storage_s3: Option<S3Settings>,
    pub auto_stop_grace_seconds: u64,
    pub summary_max_retries: u32,
    pub integration_retry_max_attempts: u32,
    pub integration_retry_initial_delay_ms: u64,
    pub integration_retry_backoff_multiplier: u32,
    pub integration_retry_max_delay_ms: u64,
    pub whisper_language: Option<String>,
    pub whisper_beam_size: u32,
    pub whisper_suppress_non_speech: bool,
    pub whisper_prompt: Option<String>,
    pub whisper_vad: bool,
    pub whisper_temperature: f32,
    pub whisper_resample_to_16k: bool,
    pub public_base_url: Option<String>,
    pub web_port: u16,
    pub web_bind_host: String,
    pub discord_client_id: Option<String>,
    pub discord_client_secret: Option<String>,
    pub web_session_secret: Option<String>,
    pub operational_metrics_bearer_token: Option<String>,
    pub guild_bot_token_encryption_key: Option<String>,
    pub static_files_dir: String,
    pub discord_bot_admin_user_ids: Vec<String>,
    pub retention_policy: RetentionPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    MissingEnv { key: &'static str },
    InvalidEnv { key: &'static str, value: String },
}

impl Display for ConfigError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingEnv { key } => write!(f, "missing required env var: {key}"),
            Self::InvalidEnv { key, value } => {
                write!(f, "invalid value for env var {key}: {value}")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let app_role = optional_env("APP_ROLE")
            .filter(|s| !s.trim().is_empty())
            .map(|s| AppRole::parse(&s))
            .transpose()?
            .unwrap_or(AppRole::All);
        let discord_token = if app_role.requires_discord_gateway_config() {
            required_env("DISCORD_TOKEN")?
        } else {
            optional_env("DISCORD_TOKEN").unwrap_or_default()
        };
        let discord_guild_id = if app_role.requires_discord_gateway_config() {
            required_env("DISCORD_GUILD_ID")?
        } else {
            optional_env("DISCORD_GUILD_ID").unwrap_or_default()
        };
        let whisper_endpoint = required_env("WHISPER_ENDPOINT")?;
        let database_url = required_env("DATABASE_URL")?;
        let chunk_storage_dir = required_env("CHUNK_STORAGE_DIR")?;
        let chunk_storage_backend = optional_env("CHUNK_STORAGE_BACKEND")
            .map(|value| ChunkStorageBackend::parse(&value))
            .transpose()?
            .unwrap_or(ChunkStorageBackend::Local);
        let chunk_storage_s3 = if chunk_storage_backend == ChunkStorageBackend::S3 {
            Some(parse_s3_settings(optional_env)?)
        } else {
            None
        };

        let summary_enabled = optional_env_parse_bool("SUMMARY_ENABLED", true)?;
        let summary_harness = optional_env("SUMMARY_HARNESS")
            .filter(|s| !s.trim().is_empty())
            .map(|s| SummaryHarness::parse(&s))
            .transpose()?
            .unwrap_or(SummaryHarness::Claude);
        let summary_allow_unsafe_agent_harness =
            optional_env_parse_bool("SUMMARY_ALLOW_UNSAFE_AGENT_HARNESS", false)?;
        // The unsafe-agent opt-in acknowledgement is validated whenever the
        // role can execute summary jobs, regardless of SUMMARY_ENABLED: that
        // flag is only the default for new meeting settings, so stored or
        // per-guild summary_enabled=true values can still enqueue jobs that
        // run the harness against untrusted transcripts.
        if app_role.requires_summary_harness_config() {
            validate_unsafe_agent_harness_opt_in(
                summary_harness,
                summary_allow_unsafe_agent_harness,
                optional_env("SUMMARY_UNSAFE_AGENT_HARNESS_PROFILE"),
            )?;
        }
        let summary_api_key = optional_env("OPENCODE_API_KEY");
        let (summary_command, summary_model) =
            if summary_enabled && app_role.requires_summary_harness_config() {
                resolve_summary_settings(
                    summary_harness,
                    optional_env("SUMMARY_COMMAND"),
                    || required_env("CLAUDE_COMMAND"),
                    optional_env("SUMMARY_MODEL"),
                    optional_env("CLAUDE_MODEL"),
                )?
            } else {
                disabled_summary_settings(
                    summary_harness,
                    optional_env("SUMMARY_COMMAND"),
                    optional_env("CLAUDE_COMMAND"),
                    optional_env("SUMMARY_MODEL"),
                    optional_env("CLAUDE_MODEL"),
                )
            };
        let (summary_provider, summary_api_key) = resolve_summary_provider(
            summary_enabled && app_role.requires_summary_harness_config(),
            summary_harness,
            optional_env("SUMMARY_PROVIDER"),
            summary_api_key,
        )?;

        Ok(Self {
            app_role,
            discord_token,
            discord_guild_id,
            whisper_endpoint,
            summary_harness,
            summary_command,
            summary_model,
            summary_provider,
            summary_api_key,
            summary_allow_unsafe_agent_harness,
            summary_enabled,
            database_url,
            database_ssl_mode: parse_database_ssl_mode(optional_env("DATABASE_SSL_MODE"))?,
            chunk_storage_dir,
            chunk_storage_backend,
            chunk_storage_s3,
            auto_stop_grace_seconds: optional_env_parse_u64_nonzero("AUTO_STOP_GRACE_SECONDS")?
                .unwrap_or(60),
            summary_max_retries: optional_env_parse_u32("SUMMARY_MAX_RETRIES")?.unwrap_or(3),
            integration_retry_max_attempts: optional_env_parse_u32(
                "INTEGRATION_RETRY_MAX_ATTEMPTS",
            )?
            .unwrap_or(3),
            integration_retry_initial_delay_ms: optional_env_parse_u64(
                "INTEGRATION_RETRY_INITIAL_DELAY_MS",
            )?
            .unwrap_or(200),
            integration_retry_backoff_multiplier: optional_env_parse_u32(
                "INTEGRATION_RETRY_BACKOFF_MULTIPLIER",
            )?
            .unwrap_or(2),
            integration_retry_max_delay_ms: optional_env_parse_u64(
                "INTEGRATION_RETRY_MAX_DELAY_MS",
            )?
            .unwrap_or(5_000),
            whisper_language: optional_env_language("WHISPER_LANGUAGE")?,
            whisper_beam_size: optional_env_parse_u32_nonzero("WHISPER_BEAM_SIZE")?
                .map(NonZeroU32::get)
                .unwrap_or(5),
            whisper_suppress_non_speech: optional_env_parse_bool(
                "WHISPER_SUPPRESS_NON_SPEECH",
                true,
            )?,
            whisper_prompt: optional_env("WHISPER_PROMPT"),
            whisper_vad: optional_env_parse_bool("WHISPER_VAD", true)?,
            whisper_temperature: optional_env_parse_f32("WHISPER_TEMPERATURE")?.unwrap_or(0.0),
            whisper_resample_to_16k: optional_env_parse_bool("WHISPER_RESAMPLE_TO_16K", true)?,
            public_base_url: optional_env("PUBLIC_BASE_URL"),
            web_port: optional_env_parse_u16("WEB_PORT")?.unwrap_or(3000),
            web_bind_host: optional_env("WEB_BIND_HOST").unwrap_or_else(|| "127.0.0.1".to_owned()),
            discord_client_id: optional_env("DISCORD_CLIENT_ID"),
            discord_client_secret: optional_env("DISCORD_CLIENT_SECRET"),
            web_session_secret: optional_env("WEB_SESSION_SECRET"),
            operational_metrics_bearer_token: optional_env("OPERATIONAL_METRICS_BEARER_TOKEN")
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty()),
            guild_bot_token_encryption_key: optional_env("GUILD_BOT_TOKEN_ENCRYPTION_KEY")
                .map(|value| value.trim().to_owned()),
            static_files_dir: optional_env("STATIC_FILES_DIR")
                .unwrap_or_else(|| "web/dist".to_owned()),
            discord_bot_admin_user_ids: parse_csv_list(optional_env("DISCORD_BOT_ADMIN_USER_IDS")),
            retention_policy: RetentionPolicy {
                raw_audio_ttl_days: optional_env_parse_u32_nonzero("RETENTION_RAW_AUDIO_TTL_DAYS")?
                    .unwrap_or_else(|| RetentionPolicy::default().raw_audio_ttl_days),
                transcript_ttl_days: optional_env_parse_u32_nonzero(
                    "RETENTION_TRANSCRIPT_TTL_DAYS",
                )?
                .unwrap_or_else(|| RetentionPolicy::default().transcript_ttl_days),
                summary_ttl_days: optional_env_parse_u32_nonzero("RETENTION_SUMMARY_TTL_DAYS")?,
            },
        })
    }

    pub fn from_map(values: &HashMap<String, String>) -> Result<Self, ConfigError> {
        let app_role = optional_from_map(values, "APP_ROLE")
            .filter(|s| !s.trim().is_empty())
            .map(|s| AppRole::parse(&s))
            .transpose()?
            .unwrap_or(AppRole::All);
        let discord_token = if app_role.requires_discord_gateway_config() {
            required_from_map(values, "DISCORD_TOKEN")?
        } else {
            optional_from_map(values, "DISCORD_TOKEN").unwrap_or_default()
        };
        let discord_guild_id = if app_role.requires_discord_gateway_config() {
            required_from_map(values, "DISCORD_GUILD_ID")?
        } else {
            optional_from_map(values, "DISCORD_GUILD_ID").unwrap_or_default()
        };
        let whisper_endpoint = required_from_map(values, "WHISPER_ENDPOINT")?;
        let database_url = required_from_map(values, "DATABASE_URL")?;
        let chunk_storage_dir = required_from_map(values, "CHUNK_STORAGE_DIR")?;
        let chunk_storage_backend = optional_from_map(values, "CHUNK_STORAGE_BACKEND")
            .map(|value| ChunkStorageBackend::parse(&value))
            .transpose()?
            .unwrap_or(ChunkStorageBackend::Local);
        let chunk_storage_s3 = if chunk_storage_backend == ChunkStorageBackend::S3 {
            Some(parse_s3_settings(|key| optional_from_map(values, key))?)
        } else {
            None
        };

        let summary_enabled = optional_from_map_parse_bool(values, "SUMMARY_ENABLED", true)?;
        let summary_harness = optional_from_map(values, "SUMMARY_HARNESS")
            .filter(|s| !s.trim().is_empty())
            .map(|s| SummaryHarness::parse(&s))
            .transpose()?
            .unwrap_or(SummaryHarness::Claude);
        let summary_allow_unsafe_agent_harness =
            optional_from_map_parse_bool(values, "SUMMARY_ALLOW_UNSAFE_AGENT_HARNESS", false)?;
        if app_role.requires_summary_harness_config() {
            validate_unsafe_agent_harness_opt_in(
                summary_harness,
                summary_allow_unsafe_agent_harness,
                optional_from_map(values, "SUMMARY_UNSAFE_AGENT_HARNESS_PROFILE"),
            )?;
        }
        let summary_api_key = optional_from_map(values, "OPENCODE_API_KEY");
        let (summary_command, summary_model) =
            if summary_enabled && app_role.requires_summary_harness_config() {
                resolve_summary_settings(
                    summary_harness,
                    optional_from_map(values, "SUMMARY_COMMAND"),
                    || required_from_map(values, "CLAUDE_COMMAND"),
                    optional_from_map(values, "SUMMARY_MODEL"),
                    optional_from_map(values, "CLAUDE_MODEL"),
                )?
            } else {
                disabled_summary_settings(
                    summary_harness,
                    optional_from_map(values, "SUMMARY_COMMAND"),
                    optional_from_map(values, "CLAUDE_COMMAND"),
                    optional_from_map(values, "SUMMARY_MODEL"),
                    optional_from_map(values, "CLAUDE_MODEL"),
                )
            };
        let (summary_provider, summary_api_key) = resolve_summary_provider(
            summary_enabled && app_role.requires_summary_harness_config(),
            summary_harness,
            optional_from_map(values, "SUMMARY_PROVIDER"),
            summary_api_key,
        )?;

        Ok(Self {
            app_role,
            discord_token,
            discord_guild_id,
            whisper_endpoint,
            summary_harness,
            summary_command,
            summary_model,
            summary_provider,
            summary_api_key,
            summary_allow_unsafe_agent_harness,
            summary_enabled,
            database_url,
            database_ssl_mode: parse_database_ssl_mode(optional_from_map(
                values,
                "DATABASE_SSL_MODE",
            ))?,
            chunk_storage_dir,
            chunk_storage_backend,
            chunk_storage_s3,
            auto_stop_grace_seconds: optional_from_map_parse_u64_nonzero(
                values,
                "AUTO_STOP_GRACE_SECONDS",
            )?
            .unwrap_or(60),
            summary_max_retries: optional_from_map_parse_u32(values, "SUMMARY_MAX_RETRIES")?
                .unwrap_or(3),
            integration_retry_max_attempts: optional_from_map_parse_u32(
                values,
                "INTEGRATION_RETRY_MAX_ATTEMPTS",
            )?
            .unwrap_or(3),
            integration_retry_initial_delay_ms: optional_from_map_parse_u64(
                values,
                "INTEGRATION_RETRY_INITIAL_DELAY_MS",
            )?
            .unwrap_or(200),
            integration_retry_backoff_multiplier: optional_from_map_parse_u32(
                values,
                "INTEGRATION_RETRY_BACKOFF_MULTIPLIER",
            )?
            .unwrap_or(2),
            integration_retry_max_delay_ms: optional_from_map_parse_u64(
                values,
                "INTEGRATION_RETRY_MAX_DELAY_MS",
            )?
            .unwrap_or(5_000),
            whisper_language: optional_from_map_language(values, "WHISPER_LANGUAGE")?,
            whisper_beam_size: optional_from_map_parse_u32_nonzero(values, "WHISPER_BEAM_SIZE")?
                .map(NonZeroU32::get)
                .unwrap_or(5),
            whisper_suppress_non_speech: optional_from_map_parse_bool(
                values,
                "WHISPER_SUPPRESS_NON_SPEECH",
                true,
            )?,
            whisper_prompt: optional_from_map(values, "WHISPER_PROMPT"),
            whisper_vad: optional_from_map_parse_bool(values, "WHISPER_VAD", true)?,
            whisper_temperature: optional_from_map_parse_f32(values, "WHISPER_TEMPERATURE")?
                .unwrap_or(0.0),
            whisper_resample_to_16k: optional_from_map_parse_bool(
                values,
                "WHISPER_RESAMPLE_TO_16K",
                true,
            )?,
            public_base_url: optional_from_map(values, "PUBLIC_BASE_URL"),
            web_port: optional_from_map_parse_u16(values, "WEB_PORT")?.unwrap_or(3000),
            web_bind_host: optional_from_map(values, "WEB_BIND_HOST")
                .unwrap_or_else(|| "127.0.0.1".to_owned()),
            discord_client_id: optional_from_map(values, "DISCORD_CLIENT_ID"),
            discord_client_secret: optional_from_map(values, "DISCORD_CLIENT_SECRET"),
            web_session_secret: optional_from_map(values, "WEB_SESSION_SECRET"),
            operational_metrics_bearer_token: optional_from_map(
                values,
                "OPERATIONAL_METRICS_BEARER_TOKEN",
            ),
            guild_bot_token_encryption_key: optional_from_map(
                values,
                "GUILD_BOT_TOKEN_ENCRYPTION_KEY",
            ),
            static_files_dir: optional_from_map(values, "STATIC_FILES_DIR")
                .unwrap_or_else(|| "web/dist".to_owned()),
            discord_bot_admin_user_ids: parse_csv_list(optional_from_map(
                values,
                "DISCORD_BOT_ADMIN_USER_IDS",
            )),
            retention_policy: RetentionPolicy {
                raw_audio_ttl_days: optional_from_map_parse_u32_nonzero(
                    values,
                    "RETENTION_RAW_AUDIO_TTL_DAYS",
                )?
                .unwrap_or_else(|| RetentionPolicy::default().raw_audio_ttl_days),
                transcript_ttl_days: optional_from_map_parse_u32_nonzero(
                    values,
                    "RETENTION_TRANSCRIPT_TTL_DAYS",
                )?
                .unwrap_or_else(|| RetentionPolicy::default().transcript_ttl_days),
                summary_ttl_days: optional_from_map_parse_u32_nonzero(
                    values,
                    "RETENTION_SUMMARY_TTL_DAYS",
                )?,
            },
        })
    }
}

/// Parses the `CHUNK_STORAGE_S3_*` env group when the backend is `s3`.
/// `lookup` resolves a key to its raw (already non-empty-filtered) value so
/// `from_env` and `from_map` share this one implementation.
fn parse_s3_settings(
    lookup: impl Fn(&'static str) -> Option<String>,
) -> Result<S3Settings, ConfigError> {
    let required = |key: &'static str| lookup(key).ok_or(ConfigError::MissingEnv { key });
    let optional_u64 = |key: &'static str| -> Result<Option<u64>, ConfigError> {
        lookup(key)
            .map(|value| {
                value
                    .parse::<u64>()
                    .map_err(|_| ConfigError::InvalidEnv { key, value })
            })
            .transpose()
    };
    let optional_bool = |key: &'static str| -> Result<Option<bool>, ConfigError> {
        lookup(key)
            .map(|value| parse_bool(&value).ok_or(ConfigError::InvalidEnv { key, value }))
            .transpose()
    };

    let endpoint = lookup("CHUNK_STORAGE_S3_ENDPOINT")
        .map(|value| value.trim().trim_end_matches('/').to_owned())
        .filter(|value| !value.is_empty());
    if let Some(value) = &endpoint {
        validate_endpoint(value).map_err(|_| ConfigError::InvalidEnv {
            key: "CHUNK_STORAGE_S3_ENDPOINT",
            value: value.clone(),
        })?;
    }
    let key_prefix = lookup("CHUNK_STORAGE_S3_KEY_PREFIX")
        .map(|value| value.trim().trim_matches('/').to_owned())
        .filter(|value| !value.is_empty())
        .map(|value| format!("{value}/"))
        .unwrap_or_default();
    let path_style_default = endpoint.is_some();

    Ok(S3Settings {
        bucket: required("CHUNK_STORAGE_S3_BUCKET")?.trim().to_owned(),
        endpoint,
        region: lookup("CHUNK_STORAGE_S3_REGION")
            .map(|value| value.trim().to_owned())
            .unwrap_or_else(|| DEFAULT_REGION.to_owned()),
        access_key_id: required("CHUNK_STORAGE_S3_ACCESS_KEY_ID")?
            .trim()
            .to_owned(),
        secret_access_key: required("CHUNK_STORAGE_S3_SECRET_ACCESS_KEY")?
            .trim()
            .to_owned(),
        key_prefix,
        path_style: optional_bool("CHUNK_STORAGE_S3_FORCE_PATH_STYLE")?
            .unwrap_or(path_style_default),
        presign_ttl_seconds: optional_u64("CHUNK_STORAGE_S3_PRESIGN_TTL_SECONDS")?
            .unwrap_or(DEFAULT_PRESIGN_TTL_SECONDS)
            .clamp(1, MAX_PRESIGN_TTL_SECONDS),
    })
}

fn parse_csv_list(value: Option<String>) -> Vec<String> {
    value
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_database_ssl_mode(value: Option<String>) -> Result<String, ConfigError> {
    let value = value.unwrap_or_else(|| "disable".to_owned());
    if value == "disable" {
        Ok(value)
    } else {
        Err(ConfigError::InvalidEnv {
            key: "DATABASE_SSL_MODE",
            value,
        })
    }
}

fn validate_unsafe_agent_harness_opt_in(
    _harness: SummaryHarness,
    allow_unsafe_agent_harness: bool,
    unsafe_agent_harness_profile: Option<String>,
) -> Result<(), ConfigError> {
    if !allow_unsafe_agent_harness {
        return Err(ConfigError::MissingEnv {
            key: "SUMMARY_ALLOW_UNSAFE_AGENT_HARNESS",
        });
    }
    let Some(profile) = unsafe_agent_harness_profile else {
        return Err(ConfigError::MissingEnv {
            key: "SUMMARY_UNSAFE_AGENT_HARNESS_PROFILE",
        });
    };
    let normalized = profile.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "local" | "local-dev" | "dev" | "development" | "test" | "testing" => Ok(()),
        _ => Err(ConfigError::InvalidEnv {
            key: "SUMMARY_UNSAFE_AGENT_HARNESS_PROFILE",
            value: profile.trim().to_owned(),
        }),
    }
}

fn resolve_summary_settings(
    harness: SummaryHarness,
    summary_command: Option<String>,
    get_claude_command: impl FnOnce() -> Result<String, ConfigError>,
    summary_model: Option<String>,
    claude_model: Option<String>,
) -> Result<(String, String), ConfigError> {
    let command = if let Some(c) = summary_command.filter(|s| !s.trim().is_empty()) {
        c
    } else if harness == SummaryHarness::Claude {
        get_claude_command()?
    } else if harness == SummaryHarness::Native {
        // The native agent runs in-process; there is no command to resolve.
        String::new()
    } else {
        return Err(ConfigError::MissingEnv {
            key: "SUMMARY_COMMAND",
        });
    };

    let mut model = if matches!(harness, SummaryHarness::OpenCode | SummaryHarness::Native) {
        summary_model.unwrap_or_default()
    } else {
        summary_model.or(claude_model).unwrap_or_default()
    };
    if model.trim().is_empty() {
        model = match harness {
            SummaryHarness::Claude => "haiku".to_owned(),
            SummaryHarness::CursorAgent | SummaryHarness::OpenCode | SummaryHarness::Native => {
                String::new()
            }
        };
    }

    if matches!(harness, SummaryHarness::OpenCode | SummaryHarness::Native)
        && model.trim().is_empty()
    {
        return Err(ConfigError::MissingEnv {
            key: "SUMMARY_MODEL",
        });
    }

    Ok((command, model))
}

/// Resolve `SUMMARY_PROVIDER` / the provider credential for the native
/// harness. Strict only when the role actually runs summaries: then the
/// native harness requires a provider and (per provider) an API key. Any
/// other state — CLI harnesses, roles that never run summaries, or
/// `SUMMARY_ENABLED=false` — ignores both settings entirely so a stray or
/// invalid value cannot stop the process from booting.
fn resolve_summary_provider(
    summary_runtime_enabled: bool,
    harness: SummaryHarness,
    provider: Option<String>,
    api_key: Option<String>,
) -> Result<(Option<SummaryProvider>, Option<String>), ConfigError> {
    if harness != SummaryHarness::Native || !summary_runtime_enabled {
        return Ok((None, None));
    }
    let provider = provider.ok_or(ConfigError::MissingEnv {
        key: "SUMMARY_PROVIDER",
    })?;
    let provider = SummaryProvider::parse(&provider)?;
    let api_key =
        api_key
            .filter(|value| !value.trim().is_empty())
            .ok_or(ConfigError::MissingEnv {
                key: provider.api_key_env(),
            })?;
    Ok((Some(provider), Some(api_key)))
}

fn disabled_summary_settings(
    harness: SummaryHarness,
    summary_command: Option<String>,
    claude_command: Option<String>,
    summary_model: Option<String>,
    claude_model: Option<String>,
) -> (String, String) {
    let command = summary_command
        .or_else(|| {
            if harness == SummaryHarness::Claude {
                claude_command
            } else {
                None
            }
        })
        .unwrap_or_default();

    let mut model = if matches!(harness, SummaryHarness::OpenCode | SummaryHarness::Native) {
        summary_model.unwrap_or_default()
    } else {
        summary_model.or(claude_model).unwrap_or_default()
    };
    if model.trim().is_empty() && harness == SummaryHarness::Claude {
        model = "haiku".to_owned();
    }

    (command, model)
}

fn required_env(key: &'static str) -> Result<String, ConfigError> {
    match env::var(key) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(ConfigError::MissingEnv { key }),
    }
}

fn required_from_map(
    values: &HashMap<String, String>,
    key: &'static str,
) -> Result<String, ConfigError> {
    match values.get(key) {
        Some(value) if !value.trim().is_empty() => Ok(value.clone()),
        _ => Err(ConfigError::MissingEnv { key }),
    }
}

fn optional_env(key: &'static str) -> Option<String> {
    match env::var(key) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        _ => None,
    }
}

fn optional_from_map(values: &HashMap<String, String>, key: &'static str) -> Option<String> {
    values
        .get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn optional_env_parse_u32(key: &'static str) -> Result<Option<u32>, ConfigError> {
    let Some(value) = optional_env(key) else {
        return Ok(None);
    };
    value
        .parse::<u32>()
        .map(Some)
        .map_err(|_| ConfigError::InvalidEnv { key, value })
}

fn optional_env_parse_u32_nonzero(key: &'static str) -> Result<Option<NonZeroU32>, ConfigError> {
    let Some(value) = optional_env(key) else {
        return Ok(None);
    };
    let parsed = value.parse::<u32>().map_err(|_| ConfigError::InvalidEnv {
        key,
        value: value.clone(),
    })?;
    if parsed == 0 {
        return Err(ConfigError::InvalidEnv { key, value });
    }
    Ok(NonZeroU32::new(parsed))
}

fn optional_env_parse_u64(key: &'static str) -> Result<Option<u64>, ConfigError> {
    let Some(value) = optional_env(key) else {
        return Ok(None);
    };
    value
        .parse::<u64>()
        .map(Some)
        .map_err(|_| ConfigError::InvalidEnv { key, value })
}

fn optional_env_parse_u64_nonzero(key: &'static str) -> Result<Option<u64>, ConfigError> {
    let Some(value) = optional_env(key) else {
        return Ok(None);
    };
    let parsed = value.parse::<u64>().map_err(|_| ConfigError::InvalidEnv {
        key,
        value: value.clone(),
    })?;
    if parsed == 0 {
        return Err(ConfigError::InvalidEnv { key, value });
    }
    Ok(Some(parsed))
}

fn optional_from_map_parse_u32(
    values: &HashMap<String, String>,
    key: &'static str,
) -> Result<Option<u32>, ConfigError> {
    let Some(value) = optional_from_map(values, key) else {
        return Ok(None);
    };
    value
        .parse::<u32>()
        .map(Some)
        .map_err(|_| ConfigError::InvalidEnv { key, value })
}

fn optional_from_map_parse_u32_nonzero(
    values: &HashMap<String, String>,
    key: &'static str,
) -> Result<Option<NonZeroU32>, ConfigError> {
    let Some(value) = optional_from_map(values, key) else {
        return Ok(None);
    };
    let parsed = value.parse::<u32>().map_err(|_| ConfigError::InvalidEnv {
        key,
        value: value.clone(),
    })?;
    if parsed == 0 {
        return Err(ConfigError::InvalidEnv { key, value });
    }
    Ok(NonZeroU32::new(parsed))
}

fn optional_from_map_parse_u64(
    values: &HashMap<String, String>,
    key: &'static str,
) -> Result<Option<u64>, ConfigError> {
    let Some(value) = optional_from_map(values, key) else {
        return Ok(None);
    };
    value
        .parse::<u64>()
        .map(Some)
        .map_err(|_| ConfigError::InvalidEnv { key, value })
}

fn optional_from_map_parse_u64_nonzero(
    values: &HashMap<String, String>,
    key: &'static str,
) -> Result<Option<u64>, ConfigError> {
    let Some(value) = optional_from_map(values, key) else {
        return Ok(None);
    };
    let parsed = value.parse::<u64>().map_err(|_| ConfigError::InvalidEnv {
        key,
        value: value.clone(),
    })?;
    if parsed == 0 {
        return Err(ConfigError::InvalidEnv { key, value });
    }
    Ok(Some(parsed))
}

fn optional_env_parse_u16(key: &'static str) -> Result<Option<u16>, ConfigError> {
    let Some(value) = optional_env(key) else {
        return Ok(None);
    };
    let parsed = value.parse::<u16>().map_err(|_| ConfigError::InvalidEnv {
        key,
        value: value.clone(),
    })?;
    if parsed == 0 {
        return Err(ConfigError::InvalidEnv { key, value });
    }
    Ok(Some(parsed))
}

fn optional_from_map_parse_u16(
    values: &HashMap<String, String>,
    key: &'static str,
) -> Result<Option<u16>, ConfigError> {
    let Some(value) = optional_from_map(values, key) else {
        return Ok(None);
    };
    let parsed = value.parse::<u16>().map_err(|_| ConfigError::InvalidEnv {
        key,
        value: value.clone(),
    })?;
    if parsed == 0 {
        return Err(ConfigError::InvalidEnv { key, value });
    }
    Ok(Some(parsed))
}

fn parse_f32_unit_range(key: &'static str, value: String) -> Result<f32, ConfigError> {
    let parsed = value.parse::<f32>().map_err(|_| ConfigError::InvalidEnv {
        key,
        value: value.clone(),
    })?;
    if !parsed.is_finite() || !(0.0..=1.0).contains(&parsed) {
        return Err(ConfigError::InvalidEnv { key, value });
    }
    Ok(parsed)
}

fn optional_env_parse_f32(key: &'static str) -> Result<Option<f32>, ConfigError> {
    let Some(value) = optional_env(key) else {
        return Ok(None);
    };
    parse_f32_unit_range(key, value).map(Some)
}

fn optional_from_map_parse_f32(
    values: &HashMap<String, String>,
    key: &'static str,
) -> Result<Option<f32>, ConfigError> {
    let Some(value) = optional_from_map(values, key) else {
        return Ok(None);
    };
    parse_f32_unit_range(key, value).map(Some)
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "y" | "on" => Some(true),
        "false" | "0" | "no" | "n" | "off" => Some(false),
        _ => None,
    }
}

fn optional_env_parse_bool(key: &'static str, default: bool) -> Result<bool, ConfigError> {
    let Some(value) = optional_env(key) else {
        return Ok(default);
    };
    parse_bool(&value).ok_or(ConfigError::InvalidEnv { key, value })
}

fn optional_from_map_parse_bool(
    values: &HashMap<String, String>,
    key: &'static str,
    default: bool,
) -> Result<bool, ConfigError> {
    let Some(value) = optional_from_map(values, key) else {
        return Ok(default);
    };
    parse_bool(&value).ok_or(ConfigError::InvalidEnv { key, value })
}

pub(crate) fn is_iso639_1_format(s: &str) -> bool {
    s.len() == 2 && s.bytes().all(|b| b.is_ascii_lowercase())
}

fn optional_env_language(key: &'static str) -> Result<Option<String>, ConfigError> {
    let Some(raw) = optional_env(key) else {
        return Ok(None);
    };
    let value = raw.trim().to_owned();
    if is_iso639_1_format(&value) {
        Ok(Some(value))
    } else {
        Err(ConfigError::InvalidEnv { key, value })
    }
}

fn optional_from_map_language(
    values: &HashMap<String, String>,
    key: &'static str,
) -> Result<Option<String>, ConfigError> {
    let Some(value) = optional_from_map(values, key) else {
        return Ok(None);
    };
    if is_iso639_1_format(&value) {
        Ok(Some(value))
    } else {
        Err(ConfigError::InvalidEnv { key, value })
    }
}
