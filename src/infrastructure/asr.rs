use crate::domain::transcript::{TranscriptSegment, TranscriptSource};
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
        return Err(WhisperParseError::InvalidJson(
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
    if end < start {
        return Err(WhisperParseError::InvalidSegment(format!(
            "segment {index} end must be greater than or equal to start"
        )));
    }
    if end == start {
        return Err(WhisperParseError::InvalidSegment(format!(
            "segment {index} duration must be greater than zero"
        )));
    }
    if end_ms <= start_ms {
        return Err(WhisperParseError::InvalidSegment(format!(
            "segment {index} duration must be at least one millisecond"
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
