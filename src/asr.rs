use crate::transcript::TranscriptSegment;
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
}

#[derive(Debug)]
pub enum WhisperParseError {
    InvalidJson(String),
}

impl Display for WhisperParseError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidJson(err) => write!(f, "invalid whisper response json: {err}"),
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
    #[serde(default)]
    text: String,
    #[serde(default)]
    segments: Vec<WhisperSegment>,
}

#[derive(Debug, Deserialize)]
struct WhisperSegment {
    #[serde(default)]
    speaker: String,
    start: f32,
    end: f32,
    #[serde(default)]
    text: String,
    #[serde(default)]
    confidence: Option<f32>,
}

pub fn parse_whisper_response(body: &str) -> Result<WhisperTranscriptionResult, WhisperParseError> {
    let parsed: WhisperResponse = serde_json::from_str(body)
        .map_err(|err| WhisperParseError::InvalidJson(err.to_string()))?;

    let mut segments = Vec::with_capacity(parsed.segments.len());
    for segment in parsed.segments {
        let speaker_id = if segment.speaker.trim().is_empty() {
            "unknown".to_owned()
        } else {
            segment.speaker
        };

        segments.push(TranscriptSegment {
            speaker_id,
            start_ms: seconds_to_ms(segment.start),
            end_ms: seconds_to_ms(segment.end),
            text: segment.text,
            confidence: segment.confidence,
            is_noisy: false,
            merged_count: 1,
        });
    }

    Ok(WhisperTranscriptionResult {
        text: parsed.text,
        segments,
    })
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
