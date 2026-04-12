use std::collections::HashMap;
use std::env;
use std::fmt::{Display, Formatter};

/// Which CLI drives meeting summary and transcript correction (`summarize` integration).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryHarness {
    Claude,
    CursorAgent,
    OpenCode,
}

impl SummaryHarness {
    pub fn parse(raw: &str) -> Result<Self, ConfigError> {
        let key = "SUMMARY_HARNESS";
        match raw.trim().to_ascii_lowercase().as_str() {
            "claude" => Ok(Self::Claude),
            "cursor_agent" => Ok(Self::CursorAgent),
            "opencode" => Ok(Self::OpenCode),
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
        }
    }
}

impl Display for SummaryHarness {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppConfig {
    pub discord_token: String,
    pub discord_guild_id: String,
    pub whisper_endpoint: String,
    pub summary_harness: SummaryHarness,
    pub summary_command: String,
    pub summary_model: String,
    pub database_url: String,
    pub database_ssl_mode: String,
    pub chunk_storage_dir: String,
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
    pub static_files_dir: String,
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
        let discord_token = required_env("DISCORD_TOKEN")?;
        let discord_guild_id = required_env("DISCORD_GUILD_ID")?;
        let whisper_endpoint = required_env("WHISPER_ENDPOINT")?;
        let database_url = required_env("DATABASE_URL")?;
        let chunk_storage_dir = required_env("CHUNK_STORAGE_DIR")?;

        let summary_harness = optional_env("SUMMARY_HARNESS")
            .filter(|s| !s.trim().is_empty())
            .map(|s| SummaryHarness::parse(&s))
            .transpose()?
            .unwrap_or(SummaryHarness::Claude);
        let (summary_command, summary_model) = resolve_summary_settings(
            summary_harness,
            optional_env("SUMMARY_COMMAND"),
            || required_env("CLAUDE_COMMAND"),
            optional_env("SUMMARY_MODEL"),
            optional_env("CLAUDE_MODEL"),
        )?;

        Ok(Self {
            discord_token,
            discord_guild_id,
            whisper_endpoint,
            summary_harness,
            summary_command,
            summary_model,
            database_url,
            database_ssl_mode: optional_env("DATABASE_SSL_MODE")
                .unwrap_or_else(|| "disable".to_owned()),
            chunk_storage_dir,
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
            whisper_beam_size: optional_env_parse_u32_nonzero("WHISPER_BEAM_SIZE")?.unwrap_or(5),
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
            static_files_dir: optional_env("STATIC_FILES_DIR")
                .unwrap_or_else(|| "web/dist".to_owned()),
        })
    }

    pub fn from_map(values: &HashMap<String, String>) -> Result<Self, ConfigError> {
        let discord_token = required_from_map(values, "DISCORD_TOKEN")?;
        let discord_guild_id = required_from_map(values, "DISCORD_GUILD_ID")?;
        let whisper_endpoint = required_from_map(values, "WHISPER_ENDPOINT")?;
        let database_url = required_from_map(values, "DATABASE_URL")?;
        let chunk_storage_dir = required_from_map(values, "CHUNK_STORAGE_DIR")?;

        let summary_harness = optional_from_map(values, "SUMMARY_HARNESS")
            .filter(|s| !s.trim().is_empty())
            .map(|s| SummaryHarness::parse(&s))
            .transpose()?
            .unwrap_or(SummaryHarness::Claude);
        let (summary_command, summary_model) = resolve_summary_settings(
            summary_harness,
            optional_from_map(values, "SUMMARY_COMMAND"),
            || required_from_map(values, "CLAUDE_COMMAND"),
            optional_from_map(values, "SUMMARY_MODEL"),
            optional_from_map(values, "CLAUDE_MODEL"),
        )?;

        Ok(Self {
            discord_token,
            discord_guild_id,
            whisper_endpoint,
            summary_harness,
            summary_command,
            summary_model,
            database_url,
            database_ssl_mode: optional_from_map(values, "DATABASE_SSL_MODE")
                .unwrap_or_else(|| "disable".to_owned()),
            chunk_storage_dir,
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
            static_files_dir: optional_from_map(values, "STATIC_FILES_DIR")
                .unwrap_or_else(|| "web/dist".to_owned()),
        })
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
    } else {
        return Err(ConfigError::MissingEnv {
            key: "SUMMARY_COMMAND",
        });
    };

    let mut model = summary_model.or(claude_model).unwrap_or_default();
    if model.trim().is_empty() {
        model = match harness {
            SummaryHarness::Claude => "haiku".to_owned(),
            SummaryHarness::CursorAgent | SummaryHarness::OpenCode => String::new(),
        };
    }

    if harness == SummaryHarness::OpenCode && model.trim().is_empty() {
        return Err(ConfigError::MissingEnv {
            key: "SUMMARY_MODEL",
        });
    }

    Ok((command, model))
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

fn optional_env_parse_u32_nonzero(key: &'static str) -> Result<Option<u32>, ConfigError> {
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
    Ok(Some(parsed))
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
) -> Result<Option<u32>, ConfigError> {
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
    Ok(Some(parsed))
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

fn is_iso639_1_format(s: &str) -> bool {
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
