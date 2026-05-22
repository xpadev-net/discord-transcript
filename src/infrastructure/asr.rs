use crate::domain::transcript::{MAX_DB_TIMESTAMP_MS, TranscriptSegment, TranscriptSource};
use serde::Deserialize;
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhisperInferenceRequest {
    pub audio_path: String,
    pub language: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WhisperTranscriptionResult {
    pub text: String,
    pub segments: Vec<TranscriptSegment>,
    /// Raw response body returned by the Whisper server. Captured so callers
    /// can persist it as a debug artifact without re-running inference.
    pub raw_body: String,
}

#[derive(Debug)]
pub enum WhisperParseError {
    InvalidJson(String),
    InvalidSegment(String),
}

impl Display for WhisperParseError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidJson(err) => write!(f, "invalid whisper response json: {err}"),
            Self::InvalidSegment(err) => write!(f, "invalid whisper segment: {err}"),
        }
    }
}

impl std::error::Error for WhisperParseError {}

fn message_contains_http_status(message: &str, status: u16) -> bool {
    let lower = message.to_ascii_lowercase();
    let spaced = format!(" {status} ");
    lower.contains(&spaced) || lower.contains(&format!(" {status}\r"))
}

impl WhisperParseError {
    pub fn is_retriable(&self) -> bool {
        match self {
            Self::InvalidSegment(_) => false,
            Self::InvalidJson(message) => {
                if !message.contains("whisper command failed") {
                    return true;
                }
                const NON_RETRIABLE_STATUS: &[u16] = &[400, 401, 403, 404, 405, 413, 415, 422];
                !NON_RETRIABLE_STATUS
                    .iter()
                    .any(|code| message_contains_http_status(message, *code))
            }
        }
    }
}

#[cfg(test)]
mod is_retriable_tests {
    use super::{WhisperParseError, message_contains_http_status};

    #[test]
    fn http_status_match_ignores_port_numbers() {
        assert!(!message_contains_http_status(
            "Failed to connect to localhost port 404: Connection refused",
            404
        ));
        assert!(message_contains_http_status(
            "whisper command failed: status=404 HTTP/1.1 404 Not Found",
            404
        ));
    }

    #[test]
    fn io_errors_are_retriable() {
        assert!(WhisperParseError::InvalidJson("curl: timeout".to_owned()).is_retriable());
    }

    #[test]
    fn segment_parse_errors_are_not_retriable() {
        assert!(!WhisperParseError::InvalidSegment("bad segment".to_owned()).is_retriable());
    }

    #[test]
    fn client_http_errors_are_not_retriable() {
        assert!(!WhisperParseError::InvalidJson(
            "whisper command failed: status=400 HTTP/1.1 400 Bad Request".to_owned()
        )
        .is_retriable());
    }

    #[test]
    fn server_http_errors_are_retriable() {
        assert!(WhisperParseError::InvalidJson(
            "whisper command failed: status=500 HTTP/1.1 500 Internal Server Error".to_owned()
        )
        .is_retriable());
    }

    #[test]
    fn rate_limit_responses_remain_retriable() {
        assert!(WhisperParseError::InvalidJson(
            "whisper command failed: status=429 HTTP/1.1 429 Too Many Requests".to_owned()
        )
        .is_retriable());
    }
}

pub trait WhisperClient {
    fn infer(
        &self,
        request: &WhisperInferenceRequest,
    ) -> Result<WhisperTranscriptionResult, WhisperParseError>;
}

#[derive(Debug, Clone)]
pub struct StubWhisperClient {
    pub mocked_response_json: String,
}

impl WhisperClient for StubWhisperClient {
    fn infer(
        &self,
        _request: &WhisperInferenceRequest,
    ) -> Result<WhisperTranscriptionResult, WhisperParseError> {
        parse_whisper_response(&self.mocked_response_json)
    }
}

#[derive(Debug, Deserialize)]
struct WhisperResponse {
    text: String,
    segments: Vec<WhisperSegment>,
}

#[derive(Debug, Deserialize)]
struct WhisperSegment {
    #[serde(default)]
    speaker: String,
    start: f32,
    end: f32,
    text: String,
    #[serde(default)]
    confidence: Option<f32>,
}

pub fn parse_whisper_response(body: &str) -> Result<WhisperTranscriptionResult, WhisperParseError> {
    let parsed: WhisperResponse = serde_json::from_str(body)
        .map_err(|err| WhisperParseError::InvalidJson(err.to_string()))?;

    if parsed.segments.is_empty() && !parsed.text.trim().is_empty() {
        return Err(WhisperParseError::InvalidSegment(
            "whisper response has non-empty text but no segments".to_owned(),
        ));
    }

    let mut segments = Vec::with_capacity(parsed.segments.len());
    for (index, segment) in parsed.segments.into_iter().enumerate() {
        let start_ms = seconds_to_ms(segment.start);
        let end_ms = seconds_to_ms(segment.end);
        validate_segment_timing(
            index,
            segment.start,
            segment.end,
            start_ms,
            end_ms,
            segment.confidence,
        )?;
        validate_segment_text(index, &segment.text)?;
        let speaker_id = if segment.speaker.trim().is_empty() {
            "unknown".to_owned()
        } else {
            segment.speaker
        };

        segments.push(TranscriptSegment {
            speaker_id,
            start_ms,
            end_ms,
            text: segment.text,
            confidence: segment.confidence,
            is_noisy: false,
            source: TranscriptSource::Voice,
            merged_count: 1,
        });
    }

    Ok(WhisperTranscriptionResult {
        text: parsed.text,
        segments,
        raw_body: body.to_owned(),
    })
}

fn validate_segment_timing(
    index: usize,
    start: f32,
    end: f32,
    start_ms: u64,
    end_ms: u64,
    confidence: Option<f32>,
) -> Result<(), WhisperParseError> {
    if !start.is_finite() || start.is_sign_negative() {
        return Err(WhisperParseError::InvalidSegment(format!(
            "segment {index} start must be finite and non-negative"
        )));
    }
    if !end.is_finite() || end.is_sign_negative() {
        return Err(WhisperParseError::InvalidSegment(format!(
            "segment {index} end must be finite and non-negative"
        )));
    }
    if end <= start {
        return Err(WhisperParseError::InvalidSegment(format!(
            "segment {index} end must be strictly greater than start"
        )));
    }
    if end_ms <= start_ms {
        return Err(WhisperParseError::InvalidSegment(format!(
            "segment {index} duration must be at least one millisecond"
        )));
    }
    if start_ms > MAX_DB_TIMESTAMP_MS || end_ms > MAX_DB_TIMESTAMP_MS {
        return Err(WhisperParseError::InvalidSegment(format!(
            "segment {index} timestamp exceeds database integer range"
        )));
    }
    if let Some(confidence) = confidence
        && !confidence.is_finite()
    {
        return Err(WhisperParseError::InvalidSegment(format!(
            "segment {index} confidence must be finite"
        )));
    }
    Ok(())
}

fn validate_segment_text(index: usize, text: &str) -> Result<(), WhisperParseError> {
    if text.trim().is_empty() {
        Err(WhisperParseError::InvalidSegment(format!(
            "segment {index} text must be non-blank"
        )))
    } else {
        Ok(())
    }
}

fn seconds_to_ms(value: f32) -> u64 {
    if value.is_nan() || value.is_sign_negative() || value.is_infinite() {
        0
    } else {
        // Keep 1_000ms headroom so downstream `+ 1_000` window checks cannot overflow.
        const MERGE_WINDOW_MS: u64 = 1_000;
        let max_safe_ms = u64::MAX.saturating_sub(MERGE_WINDOW_MS);
        let ms = (value as f64 * 1_000.0).round();
        if ms.is_sign_negative() {
            0
        } else {
            ms.min(max_safe_ms as f64) as u64
        }
    }
}
