use crate::asr::{WhisperClient, WhisperInferenceRequest, WhisperParseError};
use crate::posting::{DISCORD_MESSAGE_LIMIT, split_discord_message};
use crate::privacy::{MaskingStats, mask_pii};
use crate::transcript::{NormalizationConfig, normalize_segments, render_for_summary};
use crate::workspace::{
    MASKED_TRANSCRIPT_FILENAME, MeetingWorkspacePaths, TRANSCRIPT_MANIFEST_FILENAME,
};
use chrono::{SecondsFormat, Utc};
use serde::Serialize;
use serde_json;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryRequest {
    pub meeting_id: String,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub title: Option<String>,
    pub audio_path: String,
    pub language: Option<String>,
    pub workspace: MeetingWorkspacePaths,
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
    fn summarize(&self, prompt: &str, workdir: Option<&Path>) -> Result<String, SummaryError>;
}

#[derive(Debug, Clone)]
pub struct StubClaudeSummaryClient {
    pub mocked_markdown: String,
}

impl ClaudeSummaryClient for StubClaudeSummaryClient {
    fn summarize(&self, _prompt: &str, _workdir: Option<&Path>) -> Result<String, SummaryError> {
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

#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptionOutput {
    pub segments: Vec<crate::transcript::TranscriptSegment>,
    pub transcript_for_summary: String,
    pub masking_stats: MaskingStats,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranscriptManifest {
    pub meeting_id: String,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub language: Option<String>,
    /// Relative path from the workspace root to the masked transcript file.
    pub masked_transcript_path: String,
    pub generated_at: String,
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
        segments: normalized,
        transcript_for_summary: masked.text,
        masking_stats: masked.stats,
    })
}

pub fn write_transcript_files(
    request: &SummaryRequest,
    transcription: &TranscriptionOutput,
) -> Result<TranscriptManifest, SummaryError> {
    request.workspace.ensure_base_dirs().map_err(|err| {
        SummaryError::SummaryEngine(format!(
            "failed to prepare workspace {}: {err}",
            request.workspace.root().display()
        ))
    })?;

    let transcript_path = request.workspace.masked_transcript_path();
    fs::write(&transcript_path, &transcription.transcript_for_summary).map_err(|err| {
        SummaryError::SummaryEngine(format!(
            "failed to write transcript file {}: {err}",
            transcript_path.display()
        ))
    })?;

    let manifest = TranscriptManifest {
        meeting_id: request.meeting_id.clone(),
        guild_id: request.guild_id.clone(),
        voice_channel_id: request.voice_channel_id.clone(),
        language: request.language.clone(),
        masked_transcript_path: request
            .workspace
            .relative_path(&transcript_path)
            .to_string_lossy()
            .to_string(),
        generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        masking_stats: transcription.masking_stats,
    };

    let manifest_json = serde_json::to_vec_pretty(&manifest)
        .map_err(|err| SummaryError::SummaryEngine(err.to_string()))?;
    let manifest_path = request.workspace.transcript_manifest_path();
    fs::write(&manifest_path, manifest_json).map_err(|err| {
        SummaryError::SummaryEngine(format!(
            "failed to write transcript manifest {}: {err}",
            manifest_path.display()
        ))
    })?;

    Ok(manifest)
}

pub fn run_summary_pipeline<W: WhisperClient, C: ClaudeSummaryClient>(
    whisper: &W,
    claude: &C,
    request: &SummaryRequest,
) -> Result<SummaryResult, SummaryError> {
    let transcription = run_transcription(whisper, request)?;
    let manifest = write_transcript_files(request, &transcription)?;
    let prompt = build_summary_prompt(request, &manifest);
    let markdown = claude.summarize(&prompt, Some(request.workspace.root()))?;
    let message_chunks = split_discord_message(&markdown, DISCORD_MESSAGE_LIMIT);

    Ok(SummaryResult {
        meeting_id: request.meeting_id.clone(),
        markdown,
        transcript_for_summary: transcription.transcript_for_summary,
        message_chunks,
        masking_stats: transcription.masking_stats,
    })
}

pub fn build_summary_prompt(request: &SummaryRequest, manifest: &TranscriptManifest) -> String {
    let title = request
        .title
        .as_ref()
        .map_or_else(|| "Untitled meeting".to_owned(), Clone::clone);
    let transcript_path = format!("transcript/{MASKED_TRANSCRIPT_FILENAME}");
    let manifest_path = format!("transcript/{TRANSCRIPT_MANIFEST_FILENAME}");
    let language = request
        .language
        .as_deref()
        .unwrap_or("unknown or auto-detected");

    format!(
        "You are an assistant that summarizes meeting transcripts.\n\
The transcript is provided as a file in the current workspace (not inline in this prompt).\n\
Files available:\n\
- {transcript_path}: PII-masked transcript to read\n\
- {manifest_path}: metadata about the meeting and transcript (including masking counts)\n\
- context/: reserved for additional knowledge (may be empty)\n\
\n\
Output in markdown using the exact sections below:\n\
## Summary\n\
## Decisions\n\
## TODO\n\
## Open Questions\n\n\
Meeting ID: {}\n\
Guild ID: {}\n\
Voice channel ID: {}\n\
Meeting title: {}\n\
Language hint: {}\n\
Masking stats: mentions={}, emails={}, phones={}\n\
\n\
Instructions:\n\
- Read the transcript file to produce the summary; do not expect transcript text inline.\n\
- Keep the summary concise and actionable without leaking placeholder tokens.\n",
        request.meeting_id,
        request.guild_id,
        request.voice_channel_id,
        title,
        language,
        manifest.masking_stats.mention_replacements,
        manifest.masking_stats.email_replacements,
        manifest.masking_stats.phone_replacements
    )
}
