use std::collections::HashMap;
use std::env;
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    pub discord_token: String,
    pub discord_guild_id: String,
    pub whisper_endpoint: String,
    pub claude_command: String,
    pub database_url: String,
    pub database_ssl_mode: String,
    pub chunk_storage_dir: String,
    pub summary_max_retries: u32,
    pub integration_retry_max_attempts: u32,
    pub integration_retry_initial_delay_ms: u64,
    pub integration_retry_backoff_multiplier: u32,
    pub integration_retry_max_delay_ms: u64,
    pub whisper_language: Option<String>,
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
        Ok(Self {
            discord_token: required_env("DISCORD_TOKEN")?,
            discord_guild_id: required_env("DISCORD_GUILD_ID")?,
            whisper_endpoint: required_env("WHISPER_ENDPOINT")?,
            claude_command: required_env("CLAUDE_COMMAND")?,
            database_url: required_env("DATABASE_URL")?,
            database_ssl_mode: optional_env("DATABASE_SSL_MODE")
                .unwrap_or_else(|| "disable".to_owned()),
            chunk_storage_dir: required_env("CHUNK_STORAGE_DIR")?,
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
            whisper_language: optional_env("WHISPER_LANGUAGE").map(|s| s.trim().to_owned()),
        })
    }

    pub fn from_map(values: &HashMap<String, String>) -> Result<Self, ConfigError> {
        Ok(Self {
            discord_token: required_from_map(values, "DISCORD_TOKEN")?,
            discord_guild_id: required_from_map(values, "DISCORD_GUILD_ID")?,
            whisper_endpoint: required_from_map(values, "WHISPER_ENDPOINT")?,
            claude_command: required_from_map(values, "CLAUDE_COMMAND")?,
            database_url: required_from_map(values, "DATABASE_URL")?,
            database_ssl_mode: optional_from_map(values, "DATABASE_SSL_MODE")
                .unwrap_or_else(|| "disable".to_owned()),
            chunk_storage_dir: required_from_map(values, "CHUNK_STORAGE_DIR")?,
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
            whisper_language: optional_from_map(values, "WHISPER_LANGUAGE"),
        })
    }
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

fn optional_env_parse_u64(key: &'static str) -> Result<Option<u64>, ConfigError> {
    let Some(value) = optional_env(key) else {
        return Ok(None);
    };
    value
        .parse::<u64>()
        .map(Some)
        .map_err(|_| ConfigError::InvalidEnv { key, value })
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
