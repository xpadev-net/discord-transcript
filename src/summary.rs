use crate::asr::{WhisperClient, WhisperInferenceRequest, WhisperParseError};
use crate::posting::{DISCORD_MESSAGE_LIMIT, split_discord_message};
use crate::privacy::{MaskingStats, mask_pii};
use crate::transcript::{NormalizationConfig, normalize_segments, render_for_summary};
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryRequest {
    pub meeting_id: String,
    pub title: Option<String>,
    pub audio_path: String,
    pub language: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryResult {
    pub meeting_id: String,
    pub markdown: String,
    pub transcript_for_summary: String,
    pub message_chunks: Vec<String>,
    pub masking_stats: MaskingStats,
}

pub trait ClaudeSummaryClient {
    fn summarize(&self, prompt: &str) -> Result<String, SummaryError>;
}

#[derive(Debug, Clone)]
pub struct StubClaudeSummaryClient {
    pub mocked_markdown: String,
}

impl ClaudeSummaryClient for StubClaudeSummaryClient {
    fn summarize(&self, _prompt: &str) -> Result<String, SummaryError> {
        Ok(self.mocked_markdown.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SummaryError {
    Asr(String),
    SummaryEngine(String),
}

impl Display for SummaryError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Asr(err) => write!(f, "asr failed: {err}"),
            Self::SummaryEngine(err) => write!(f, "summary engine failed: {err}"),
        }
    }
}

impl std::error::Error for SummaryError {}

impl From<WhisperParseError> for SummaryError {
    fn from(value: WhisperParseError) -> Self {
        Self::Asr(value.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptionOutput {
    pub transcript_for_summary: String,
    pub masking_stats: MaskingStats,
}

pub fn run_transcription<W: WhisperClient>(
    whisper: &W,
    request: &SummaryRequest,
) -> Result<TranscriptionOutput, SummaryError> {
    let transcription = whisper.infer(&WhisperInferenceRequest {
        audio_path: request.audio_path.clone(),
        language: request.language.clone(),
    })?;
    let normalized = normalize_segments(&transcription.segments, NormalizationConfig::default());
    let rendered = render_for_summary(&normalized);
    let masked = mask_pii(&rendered);
    Ok(TranscriptionOutput {
        transcript_for_summary: masked.text,
        masking_stats: masked.stats,
    })
}

pub fn run_summary_pipeline<W: WhisperClient, C: ClaudeSummaryClient>(
    whisper: &W,
    claude: &C,
    request: &SummaryRequest,
) -> Result<SummaryResult, SummaryError> {
    let transcription = run_transcription(whisper, request)?;
    let prompt = build_summary_prompt(request, &transcription.transcript_for_summary);
    let markdown = claude.summarize(&prompt)?;
    let message_chunks = split_discord_message(&markdown, DISCORD_MESSAGE_LIMIT);

    Ok(SummaryResult {
        meeting_id: request.meeting_id.clone(),
        markdown,
        transcript_for_summary: transcription.transcript_for_summary,
        message_chunks,
        masking_stats: transcription.masking_stats,
    })
}

pub fn build_summary_prompt(request: &SummaryRequest, masked_transcript: &str) -> String {
    let title = request
        .title
        .as_ref()
        .map_or_else(|| "Untitled meeting".to_owned(), Clone::clone);

    format!(
        "You are an assistant that summarizes meeting transcripts.\n\
Output in markdown using the exact sections below:\n\
## Summary\n\
## Decisions\n\
## TODO\n\
## Open Questions\n\n\
Meeting ID: {}\n\
Meeting title: {}\n\n\
Transcript (PII-masked):\n{}\n",
        request.meeting_id, title, masked_transcript
    )
}
