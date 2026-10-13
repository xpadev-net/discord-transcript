use crate::domain::transcript::TranscriptSegment;
use std::fmt::{Debug, Display, Formatter};
use std::path::{Path, PathBuf};

/// ASR input owned by the application. S3 inputs are object keys in the
/// application's configured bucket, never caller supplied URLs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioSource {
    LocalFile(PathBuf),
    S3Object(S3AudioObjectReference),
}

impl AudioSource {
    pub fn local_file(path: impl Into<PathBuf>) -> Self {
        Self::LocalFile(path.into())
    }

    pub fn local_path(&self) -> Option<&Path> {
        match self {
            Self::LocalFile(path) => Some(path),
            Self::S3Object(_) => None,
        }
    }
}

/// A reference to an object in the application's own configured S3 bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3AudioObjectReference {
    key: String,
    pub speaker_id: Option<String>,
    pub offset_ms: Option<u64>,
}

impl S3AudioObjectReference {
    pub fn new(
        key: impl Into<String>,
        speaker_id: Option<String>,
        offset_ms: Option<u64>,
    ) -> Result<Self, AsrError> {
        let key = key.into();
        if key.trim().is_empty()
            || key.starts_with('/')
            || key.contains("://")
            || key.chars().any(char::is_control)
        {
            return Err(AsrError::InvalidAudioSource(
                "S3 audio reference must be a relative object key".to_owned(),
            ));
        }
        Ok(Self {
            key,
            speaker_id,
            offset_ms,
        })
    }

    pub fn key(&self) -> &str {
        &self.key
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct AsrRequest {
    pub source: AudioSource,
    pub language: Option<String>,
    /// Meeting-specific prompt hints; implementations must avoid logging it.
    pub prompt: Option<String>,
}

impl Debug for AsrRequest {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsrRequest")
            .field("source", &self.source)
            .field("language", &self.language)
            .field("prompt", &self.prompt.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl AsrRequest {
    pub fn local_file(
        path: impl Into<PathBuf>,
        language: Option<String>,
        prompt: Option<String>,
    ) -> Self {
        Self {
            source: AudioSource::local_file(path),
            language,
            prompt,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AsrResult {
    pub text: String,
    pub segments: Vec<TranscriptSegment>,
    /// Raw adapter response persisted for diagnostics without another request.
    pub raw_body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsrError {
    InvalidAudioSource(String),
    InvalidJson(String),
    InvalidSegment(String),
}

impl Display for AsrError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidAudioSource(err) => write!(f, "invalid ASR audio source: {err}"),
            Self::InvalidJson(err) => write!(f, "invalid whisper response json: {err}"),
            Self::InvalidSegment(err) => write!(f, "invalid whisper segment: {err}"),
        }
    }
}

impl std::error::Error for AsrError {}

impl AsrError {
    pub fn is_retriable(&self) -> bool {
        match self {
            Self::InvalidAudioSource(_) | Self::InvalidSegment(_) => false,
            Self::InvalidJson(message) => {
                if message.starts_with("malformed successful whisper response")
                    || message.starts_with("non-retriable whisper command")
                {
                    return false;
                }
                if message.starts_with("retriable whisper transport failure") {
                    return true;
                }
                if let Some(status) = http_status_from_message(message) {
                    return status == 0 || status == 429 || (500..=599).contains(&status);
                }
                true
            }
        }
    }
}

fn http_status_from_message(message: &str) -> Option<u16> {
    let lower = message.to_ascii_lowercase();
    for marker in ["http_status=", "status="] {
        if let Some(status) = parse_status_after_marker(&lower, marker) {
            return Some(status);
        }
    }
    if let Some(status) = parse_status_after_marker(&lower, "http status ") {
        return Some(status);
    }
    lower
        .find("http/")
        .and_then(|index| first_status_token(&lower[index..]))
}

fn parse_status_after_marker(message: &str, marker: &str) -> Option<u16> {
    let index = message.find(marker)? + marker.len();
    let digits = message[index..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    parse_http_status_token(&digits)
}

fn first_status_token(message: &str) -> Option<u16> {
    message
        .split(|ch: char| !ch.is_ascii_digit())
        .find_map(parse_http_status_token)
}

fn parse_http_status_token(token: &str) -> Option<u16> {
    if token.len() != 3 {
        return None;
    }
    let status = token.parse::<u16>().ok()?;
    (status <= 599).then_some(status)
}

/// Application-owned ASR boundary. Implementations adapt local or trusted
/// object inputs to their backend protocol.
pub trait AsrPort {
    fn infer(&self, request: &AsrRequest) -> Result<AsrResult, AsrError>;
}

#[cfg(test)]
mod tests {
    use super::{AsrError, AudioSource, S3AudioObjectReference};

    #[test]
    fn s3_audio_reference_keeps_key_and_timeline_metadata() {
        let reference = S3AudioObjectReference::new(
            "meetings/meeting-1/audio/alice.wav",
            Some("alice".to_owned()),
            Some(1_250),
        )
        .unwrap();
        let source = AudioSource::S3Object(reference.clone());

        assert_eq!(reference.key(), "meetings/meeting-1/audio/alice.wav");
        assert_eq!(reference.speaker_id.as_deref(), Some("alice"));
        assert_eq!(reference.offset_ms, Some(1_250));
        assert!(source.local_path().is_none());
    }

    #[test]
    fn s3_audio_reference_rejects_external_urls_and_absolute_paths() {
        for key in ["https://example.com/audio.wav", "/audio.wav", "  "] {
            assert!(matches!(
                S3AudioObjectReference::new(key, None, None),
                Err(AsrError::InvalidAudioSource(_))
            ));
        }
    }

    #[test]
    fn asr_request_debug_redacts_context_prompt() {
        let request = super::AsrRequest::local_file("audio.wav", None, Some("private".to_owned()));
        assert!(!format!("{request:?}").contains("private"));
        assert!(format!("{request:?}").contains("[REDACTED]"));
    }
}
