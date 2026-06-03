use crate::domain::domain_knowledge::DomainKnowledgeItem;
use crate::domain::privacy::{MaskingStats, mask_pii};
use crate::domain::speaker::SpeakerProfile;
use crate::domain::summary_template::{
    SummaryTemplate, SummaryTemplateValidationError, SummaryTemplateVariables,
    render_summary_template,
};
use crate::domain::transcript::{NormalizationConfig, normalize_segments, render_for_summary};
use crate::infrastructure::asr::{WhisperClient, WhisperInferenceRequest, WhisperParseError};
use crate::infrastructure::workspace::{
    CONTEXT_DOMAIN_KNOWLEDGE_FILENAME, CONTEXT_MANIFEST_FILENAME, CONTEXT_SPEAKERS_FILENAME,
    MASKED_TRANSCRIPT_FILENAME, MeetingWorkspacePaths, TRANSCRIPT_MANIFEST_FILENAME,
};
use crate::interfaces::posting::{DISCORD_MESSAGE_LIMIT, split_discord_message};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::Path;
use tracing::warn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryRequest {
    pub meeting_id: String,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub title: Option<String>,
    pub audio_path: String,
    pub speaker_audio: Vec<SpeakerAudioInput>,
    pub language: Option<String>,
    pub workspace: MeetingWorkspacePaths,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SummaryContextInput {
    pub speakers: Vec<SpeakerProfile>,
    /// Raw domain knowledge candidates. [`materialize_summary_context`] is
    /// responsible for selecting active, non-archived items.
    pub domain_knowledge: Vec<DomainKnowledgeItem>,
    /// Raw summary template candidate. [`materialize_summary_context`] is
    /// responsible for selecting active, non-archived templates.
    pub summary_template: Option<SummaryTemplate>,
    pub effective_summary_template_id: Option<String>,
    pub effective_domain_knowledge_version_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeakerAudioInput {
    pub speaker_id: String,
    pub audio_path: String,
    /// Offset from meeting start in milliseconds to align segments from this
    /// speaker's audio back onto the meeting timeline.
    pub offset_ms: u64,
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
    InvalidSummaryTemplate(String),
}

impl Display for SummaryError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Asr(err) => write!(f, "asr failed: {err}"),
            Self::SummaryEngine(err) => write!(f, "summary engine failed: {err}"),
            Self::InvalidSummaryTemplate(err) => write!(f, "invalid summary template: {err}"),
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
    pub segments: Vec<crate::domain::transcript::TranscriptSegment>,
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct SummaryContextManifest {
    pub meeting_id: String,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub generated_at: String,
    pub manifest_path: String,
    pub speaker_roster_path: String,
    pub speaker_count: usize,
    pub domain_knowledge_path: String,
    pub domain_knowledge_count: usize,
    pub domain_knowledge_items: Vec<DomainKnowledgeContextMetadata>,
    pub effective_domain_knowledge_version_id: Option<String>,
    pub summary_template_path: Option<String>,
    pub summary_template: Option<SummaryTemplateContextMetadata>,
    pub effective_summary_template_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct DomainKnowledgeContextMetadata {
    pub id: String,
    pub content_type: String,
    pub version: u32,
    pub active: bool,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct SummaryTemplateContextMetadata {
    pub id: String,
    pub version: u32,
    pub active: bool,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SpeakerRosterEntry {
    speaker_id: String,
    display_label: String,
    username: Option<String>,
    nickname: Option<String>,
    display_name: Option<String>,
}

pub fn run_transcription<W: WhisperClient>(
    whisper: &W,
    request: &SummaryRequest,
) -> Result<TranscriptionOutput, SummaryError> {
    if request.speaker_audio.is_empty() {
        let transcription = whisper.infer(&WhisperInferenceRequest {
            audio_path: request.audio_path.clone(),
            language: request.language.clone(),
            prompt: build_whisper_prompt(request, None),
        })?;
        persist_whisper_debug_response(
            &request.workspace.mixdown_whisper_response_path(),
            &transcription.raw_body,
        );
        return build_transcription_output(transcription.segments);
    }

    let mut merged_segments = Vec::new();
    for speaker in &request.speaker_audio {
        let transcription = whisper.infer(&WhisperInferenceRequest {
            audio_path: speaker.audio_path.clone(),
            language: request.language.clone(),
            prompt: build_whisper_prompt(request, Some(speaker)),
        })?;
        persist_whisper_debug_response(
            &request.workspace.whisper_response_path(&speaker.speaker_id),
            &transcription.raw_body,
        );
        for mut segment in transcription.segments {
            segment.speaker_id = speaker.speaker_id.clone();
            segment.start_ms = segment.start_ms.saturating_add(speaker.offset_ms);
            segment.end_ms = segment.end_ms.saturating_add(speaker.offset_ms);
            merged_segments.push(segment);
        }
    }

    merged_segments.sort_by(|a, b| {
        a.start_ms
            .cmp(&b.start_ms)
            .then(a.end_ms.cmp(&b.end_ms))
            .then(a.speaker_id.cmp(&b.speaker_id))
    });
    build_transcription_output(merged_segments)
}

pub fn build_whisper_prompt(
    request: &SummaryRequest,
    speaker: Option<&SpeakerAudioInput>,
) -> Option<String> {
    build_whisper_context_prompt(
        request.title.as_deref(),
        speaker.map(|speaker| speaker.speaker_id.as_str()),
    )
}

pub fn build_whisper_context_prompt(
    title: Option<&str>,
    speaker_id: Option<&str>,
) -> Option<String> {
    let mut lines = Vec::new();
    if let Some(title) = title.map(str::trim)
        && !title.is_empty()
    {
        let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        lines.push(format!("Meeting title: {title}"));
    }
    if let Some(speaker_id) = speaker_id.map(str::trim)
        && !speaker_id.is_empty()
    {
        let speaker_id = speaker_id.split_whitespace().collect::<Vec<_>>().join(" ");
        lines.push(format!("Speaker ID: {speaker_id}"));
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

/// Best-effort write of a Whisper raw response JSON for debugging. Failures
/// are logged but do not interrupt the transcription pipeline.
fn persist_whisper_debug_response(path: &Path, body: &str) {
    if let Some(parent) = path.parent()
        && let Err(err) = fs::create_dir_all(parent)
    {
        warn!(
            path = %parent.display(),
            error = %err,
            "failed to create whisper debug directory"
        );
        return;
    }
    if let Err(err) = fs::write(path, body) {
        warn!(
            path = %path.display(),
            error = %err,
            "failed to persist whisper raw response for debugging"
        );
    }
}

/// Persist the transcript that the summary pipeline produced *before* any
/// optional LLM correction step. Always safe to call: the artifact is
/// accurate regardless of whether [`correct_transcript`] is subsequently
/// invoked, because by definition this is the transcript prior to correction.
/// Best-effort: I/O failures are logged but do not interrupt the pipeline.
pub fn persist_pre_correction_transcript_debug_artifact(
    workspace: &MeetingWorkspacePaths,
    pre_correction_transcript: &str,
) {
    persist_debug_text(
        &workspace.pre_correction_transcript_path(),
        pre_correction_transcript,
    );
}

/// Persist the prompt that will be sent to [`correct_transcript`]. Should
/// only be called immediately before the correction step actually runs;
/// otherwise the artifact is misleading because it implies the GEC step
/// executed when it didn't.
///
/// Returns the built prompt (so the caller can pass it to
/// [`correct_transcript_with_prompt`] without re-building) or `None` if the
/// transcript is empty/whitespace, in which case the correction step would
/// short-circuit and no artifact is written. Best-effort: I/O failures are
/// logged but do not interrupt the pipeline.
#[must_use = "the returned prompt should be reused by `correct_transcript_with_prompt` to avoid rebuilding it"]
pub fn persist_correction_prompt_debug_artifact(
    workspace: &MeetingWorkspacePaths,
    pre_correction_transcript: &str,
    language: Option<&str>,
) -> Option<String> {
    if pre_correction_transcript.trim().is_empty() {
        return None;
    }
    let prompt = build_correction_prompt(pre_correction_transcript, language);
    persist_debug_text(&workspace.correction_prompt_path(), &prompt);
    Some(prompt)
}

/// Persist the prompt sent to the summary harness to the workspace's
/// `debug/` directory. Best-effort.
pub fn persist_summary_prompt_debug_artifact(workspace: &MeetingWorkspacePaths, prompt: &str) {
    persist_debug_text(&workspace.summary_prompt_path(), prompt);
}

/// Best-effort write of a debug artifact (transcript or prompt). Failures are
/// logged but do not interrupt the summary pipeline.
fn persist_debug_text(path: &Path, contents: &str) {
    if let Some(parent) = path.parent()
        && let Err(err) = fs::create_dir_all(parent)
    {
        warn!(
            path = %parent.display(),
            error = %err,
            "failed to create debug directory"
        );
        return;
    }
    if let Err(err) = fs::write(path, contents) {
        warn!(
            path = %path.display(),
            error = %err,
            "failed to persist debug artifact"
        );
    }
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
            .ok_or_else(|| {
                SummaryError::SummaryEngine(format!(
                    "transcript path {:?} escaped workspace {:?}",
                    transcript_path,
                    request.workspace.root()
                ))
            })?
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

pub fn materialize_summary_context(
    request: &SummaryRequest,
    context: &SummaryContextInput,
) -> Result<SummaryContextManifest, SummaryError> {
    request.workspace.ensure_base_dirs().map_err(|err| {
        SummaryError::SummaryEngine(format!(
            "failed to prepare workspace {}: {err}",
            request.workspace.root().display()
        ))
    })?;

    let mut speakers = context.speakers.clone();
    speakers.sort_by(|left, right| left.speaker_id.cmp(&right.speaker_id));
    let speaker_entries = speakers
        .iter()
        .map(|speaker| SpeakerRosterEntry {
            speaker_id: speaker.speaker_id.clone(),
            display_label: speaker.display_label(),
            username: speaker.username.clone(),
            nickname: speaker.nickname.clone(),
            display_name: speaker.display_name.clone(),
        })
        .collect::<Vec<_>>();
    let speaker_path = request.workspace.context_speakers_path();
    write_json_file(&speaker_path, &speaker_entries, "speaker roster")?;

    let mut domain_knowledge = context
        .domain_knowledge
        .iter()
        .filter(|item| item.active && item.archived_at.is_none())
        .cloned()
        .collect::<Vec<_>>();
    domain_knowledge.sort_by(|left, right| {
        left.content_type
            .as_str()
            .cmp(right.content_type.as_str())
            .then(left.title.cmp(&right.title))
            .then(left.id.cmp(&right.id))
    });
    let domain_path = request.workspace.context_domain_knowledge_path();
    fs::write(
        &domain_path,
        render_domain_knowledge_context(&domain_knowledge),
    )
    .map_err(|err| {
        SummaryError::SummaryEngine(format!(
            "failed to write domain knowledge context {}: {err}",
            domain_path.display()
        ))
    })?;

    let summary_template_path = if let Some(template) = context.summary_template.as_ref() {
        if !template.active || template.archived_at.is_some() {
            remove_stale_optional_context_file(&request.workspace.context_summary_template_path())?;
            None
        } else {
            let path = request.workspace.context_summary_template_path();
            fs::write(&path, &template.template).map_err(|err| {
                SummaryError::SummaryEngine(format!(
                    "failed to write summary template context {}: {err}",
                    path.display()
                ))
            })?;
            Some(relative_workspace_path(&request.workspace, &path)?)
        }
    } else {
        remove_stale_optional_context_file(&request.workspace.context_summary_template_path())?;
        None
    };
    let summary_template_metadata = context
        .summary_template
        .as_ref()
        .filter(|template| template.active && template.archived_at.is_none())
        .map(|template| SummaryTemplateContextMetadata {
            id: template.id.clone(),
            version: template.version,
            active: template.active,
            updated_at: template
                .updated_at
                .to_rfc3339_opts(SecondsFormat::Secs, true),
        });

    let manifest_path = request.workspace.context_manifest_path();
    let manifest = SummaryContextManifest {
        meeting_id: request.meeting_id.clone(),
        guild_id: request.guild_id.clone(),
        voice_channel_id: request.voice_channel_id.clone(),
        generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        manifest_path: relative_workspace_path(&request.workspace, &manifest_path)?,
        speaker_roster_path: relative_workspace_path(&request.workspace, &speaker_path)?,
        speaker_count: speaker_entries.len(),
        domain_knowledge_path: relative_workspace_path(&request.workspace, &domain_path)?,
        domain_knowledge_count: domain_knowledge.len(),
        domain_knowledge_items: domain_knowledge
            .iter()
            .map(|item| DomainKnowledgeContextMetadata {
                id: item.id.clone(),
                content_type: item.content_type.as_str().to_owned(),
                version: item.version,
                active: item.active,
                updated_at: item.updated_at.to_rfc3339_opts(SecondsFormat::Secs, true),
            })
            .collect(),
        effective_domain_knowledge_version_id: context
            .effective_domain_knowledge_version_id
            .clone(),
        summary_template_path,
        summary_template: summary_template_metadata,
        effective_summary_template_id: context.effective_summary_template_id.clone(),
    };
    write_json_file(&manifest_path, &manifest, "summary context manifest")?;

    Ok(manifest)
}

pub fn materialize_or_load_summary_context(
    request: &SummaryRequest,
    context: &SummaryContextInput,
) -> Result<SummaryContextManifest, SummaryError> {
    if let Some(manifest) = load_summary_context_manifest(request)? {
        return Ok(manifest);
    }

    materialize_summary_context(request, context)
}

pub fn load_summary_context_manifest(
    request: &SummaryRequest,
) -> Result<Option<SummaryContextManifest>, SummaryError> {
    let manifest_path = request.workspace.context_manifest_path();
    if !manifest_path.exists() {
        return Ok(None);
    }

    let manifest_json = fs::read(&manifest_path).map_err(|err| {
        SummaryError::SummaryEngine(format!(
            "failed to read summary context manifest {}: {err}",
            manifest_path.display()
        ))
    })?;
    serde_json::from_slice(&manifest_json)
        .map(Some)
        .map_err(|err| {
            SummaryError::SummaryEngine(format!(
                "failed to parse summary context manifest {}: {err}",
                manifest_path.display()
            ))
        })
}

fn remove_stale_optional_context_file(path: &Path) -> Result<(), SummaryError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(SummaryError::SummaryEngine(format!(
            "failed to remove stale optional context file {}: {err}",
            path.display()
        ))),
    }
}

fn write_json_file<T: Serialize>(path: &Path, value: &T, label: &str) -> Result<(), SummaryError> {
    let json = serde_json::to_vec_pretty(value)
        .map_err(|err| SummaryError::SummaryEngine(err.to_string()))?;
    fs::write(path, json).map_err(|err| {
        SummaryError::SummaryEngine(format!("failed to write {label} {}: {err}", path.display()))
    })
}

fn relative_workspace_path(
    workspace: &MeetingWorkspacePaths,
    path: &Path,
) -> Result<String, SummaryError> {
    workspace
        .relative_path(path)
        .ok_or_else(|| {
            SummaryError::SummaryEngine(format!(
                "context path {:?} escaped workspace {:?}",
                path,
                workspace.root()
            ))
        })
        .map(|path| path.to_string_lossy().to_string())
}

fn render_domain_knowledge_context(items: &[DomainKnowledgeItem]) -> String {
    if items.is_empty() {
        return "# Domain Knowledge\n\nNo active domain knowledge was materialized.\n".to_owned();
    }

    let mut rendered = String::from("# Domain Knowledge\n");
    for item in items {
        rendered.push_str(&format!(
            "\n## {}\n\n- id: {}\n- content_type: {}\n- version: {}\n\n{}\n",
            item.title,
            item.id,
            item.content_type.as_str(),
            item.version,
            item.body
        ));
    }
    rendered
}

pub fn run_summary_pipeline<W: WhisperClient, C: ClaudeSummaryClient>(
    whisper: &W,
    claude: &C,
    request: &SummaryRequest,
) -> Result<SummaryResult, SummaryError> {
    let transcription = run_transcription(whisper, request)?;
    // Note: this entry point intentionally does NOT run `correct_transcript`,
    // so we do not write the correction-prompt debug artifact here — doing so
    // would mislead a reader into thinking the GEC step had executed.
    let manifest = write_transcript_files(request, &transcription)?;
    let context_manifest = load_summary_context_manifest(request)?;
    let prompt = build_summary_prompt_with_context(request, &manifest, context_manifest.as_ref());
    persist_summary_prompt_debug_artifact(&request.workspace, &prompt);
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
    build_summary_prompt_with_context(request, manifest, None)
}

/// Build the default summary prompt while referencing materialized context
/// only when a context manifest is supplied.
pub fn build_summary_prompt_with_context(
    request: &SummaryRequest,
    manifest: &TranscriptManifest,
    context: Option<&SummaryContextManifest>,
) -> String {
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
    let context_files = summary_context_file_list(context);
    let context_instructions = context
        .map(|_| {
            format!(
                "- Read `context/{CONTEXT_MANIFEST_FILENAME}` first; it records reproducibility metadata and paths for materialized context without inlining sensitive context bodies.\n\
- Read the materialized speaker roster and domain knowledge files; do not assume live database context beyond those files.\n"
            )
        })
        .unwrap_or_default();
    let summary_template_instruction = if let Some(context) = context
        && let Some(path) = context.summary_template_path.as_deref()
    {
        format!(
            "- If `{path}` is present, read it as the materialized active summary template and follow it as the primary summary structure/instruction set. Interpret any template variables using these values: transcript_path={transcript_path}, manifest_path={manifest_path}, language={language}, speaker_roster={}, domain_context_path={}.\n",
            context.speaker_roster_path, context.domain_knowledge_path
        )
    } else {
        String::new()
    };

    format!(
        "You are an assistant that summarizes meeting transcripts.\n\
The transcript is provided as a file in the current workspace (not inline in this prompt).\n\
Files available:\n\
- {transcript_path}: PII-masked transcript to read\n\
- {manifest_path}: metadata about the meeting and transcript (including masking counts)\n\
{context_files}\
\n\
Keep speaker attributions by reading the materialized speaker roster when available and using the provided speaker names when describing Summary, Decisions, TODO, and Open Questions.\n\
Output in markdown using the exact sections below:\n\
## Summary\n\
## Decisions\n\
## TODO\n\
## Open Questions\n\n\
Meeting ID: {}\n\
Guild ID: {}\n\
Voice channel ID: {}\n\
Meeting title: {}\n\
Whisper language (ISO 639-1, speech-recognition setting): {}\n\
Masking stats: mentions={}, emails={}, phones={}\n\
\n\
Instructions:\n\
- Read the transcript file to produce the summary; do not expect transcript text inline.\n\
{context_instructions}\
{summary_template_instruction}\
- Output language: Write the **entire** markdown output in the **same language** as the Whisper setting above (this matches how the transcript was transcribed). That includes all section headings, paragraphs, and list items. Examples: if the setting is `ja`, use Japanese throughout; if `en`, English throughout; if `de`, German throughout.\n\
- If the Whisper language is shown as `unknown or auto-detected`, infer the output language from the dominant language of the transcript text.\n\
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

fn summary_context_file_list(context: Option<&SummaryContextManifest>) -> String {
    let Some(context) = context else {
        return "- context/: reserved for additional knowledge (may be empty)\n".to_owned();
    };

    let mut lines = format!(
        "- {}: materialized context metadata and file paths\n\
- {}: materialized speaker roster ({} speakers)\n\
- {}: materialized active domain knowledge ({} items)\n",
        context.manifest_path,
        context.speaker_roster_path,
        context.speaker_count,
        context.domain_knowledge_path,
        context.domain_knowledge_count
    );
    if let Some(path) = &context.summary_template_path {
        lines.push_str(&format!(
            "- {path}: materialized active summary template instructions\n"
        ));
    }
    lines
}

/// Render a custom summary template. Templates that use
/// `{{speaker_roster}}` or `{{domain_context_path}}` expect the caller to have
/// materialized summary context in the workspace first.
pub fn build_summary_prompt_with_template(
    request: &SummaryRequest,
    manifest: &TranscriptManifest,
    template: Option<&str>,
) -> Result<String, SummaryError> {
    let Some(template) = template else {
        return Ok(build_summary_prompt(request, manifest));
    };
    let values = summary_template_values(request, manifest);
    render_summary_template(template, &values).map_err(summary_template_error)
}

fn summary_template_values(
    request: &SummaryRequest,
    _manifest: &TranscriptManifest,
) -> SummaryTemplateVariables {
    SummaryTemplateVariables {
        transcript_path: format!("transcript/{MASKED_TRANSCRIPT_FILENAME}"),
        manifest_path: format!("transcript/{TRANSCRIPT_MANIFEST_FILENAME}"),
        language: request
            .language
            .as_deref()
            .unwrap_or("unknown or auto-detected")
            .to_owned(),
        speaker_roster: format!("context/{CONTEXT_SPEAKERS_FILENAME}"),
        domain_context_path: format!("context/{CONTEXT_DOMAIN_KNOWLEDGE_FILENAME}"),
    }
}

fn summary_template_error(err: SummaryTemplateValidationError) -> SummaryError {
    SummaryError::InvalidSummaryTemplate(match err {
        SummaryTemplateValidationError::Empty => "template is empty".to_owned(),
        SummaryTemplateValidationError::TooLarge => "template is too large".to_owned(),
        SummaryTemplateValidationError::UnclosedVariable => {
            "template variable is unclosed".to_owned()
        }
        SummaryTemplateValidationError::EmptyVariable => "template variable is empty".to_owned(),
        SummaryTemplateValidationError::UnknownVariable(name) => {
            format!("unknown template variable '{name}'")
        }
    })
}

pub fn build_transcription_output(
    segments: Vec<crate::domain::transcript::TranscriptSegment>,
) -> Result<TranscriptionOutput, SummaryError> {
    let normalized = normalize_segments(&segments, NormalizationConfig::default());
    // Standalone callers render with only speaker IDs; the runtime path re-renders
    // with resolved speaker profiles before summarization.
    let rendered = render_for_summary(&normalized, None);
    let masked = mask_pii(&rendered);
    Ok(TranscriptionOutput {
        segments: normalized,
        transcript_for_summary: masked.text,
        masking_stats: masked.stats,
    })
}

/// Build the prompt sent to the Claude harness for ASR error correction.
///
/// Returns an empty string when the transcript is empty so callers can skip
/// invoking the LLM entirely. The output is exposed as a debug artifact, so
/// the prompt construction is intentionally a pure function.
pub fn build_correction_prompt(transcript: &str, language: Option<&str>) -> String {
    if transcript.trim().is_empty() {
        return String::new();
    }

    let is_japanese = language == Some("ja");
    let language_rules = if is_japanese {
        "- Fix misrecognized kanji/characters (e.g. homophone errors)\n\
         - Add or fix punctuation (。、！？) where appropriate\n\
         - Normalize spoken numbers to digits (e.g. 「ひゃくにじゅうさん」→「123」)"
    } else {
        "- Fix misrecognized words and spelling errors\n\
         - Add or fix punctuation where appropriate for the language\n\
         - Normalize spoken numbers to digits (e.g. \"one hundred twenty three\" → \"123\")"
    };
    format!(
        "You are a speech-recognition error corrector.\n\
\n\
Below is an ASR (automatic speech recognition) transcript. Each line has the format:\n\
[start_ms-end_ms] Speaker [optional-tags]: text\n\
\n\
Optional tags that may appear between the speaker name and the colon include [VC_TEXT] (VC chat message) and [NOISY] (low-confidence segment).\n\
\n\
Fix recognition errors in the **text** portion of each line while keeping the \
timestamp/speaker prefix and line structure exactly as-is. Specifically:\n\
{language_rules}\n\
- Preserve bracketed placeholder tokens exactly as-is (e.g. [MENTION_1], [EMAIL_1], [PHONE_1])\n\
- If a line contains [VC_TEXT] before the colon (i.e. a VC chat segment), keep that line's text content unchanged\n\
- Do NOT change speaker names, timestamps, or line structure\n\
- Do NOT add, remove, or reorder lines\n\
- Do NOT add commentary or explanation\n\
- Output ONLY the corrected transcript, nothing else\n\
\n\
Transcript:\n\
{transcript}"
    )
}

/// Apply LLM-based Generative Error Correction to the transcript text.
///
/// This step corrects misrecognized kanji, adds proper punctuation, and
/// normalizes numbers in the Whisper output using Claude.
pub fn correct_transcript<C: ClaudeSummaryClient>(
    claude: &C,
    transcript: &str,
    language: Option<&str>,
) -> Result<String, SummaryError> {
    if transcript.trim().is_empty() {
        return Ok(transcript.to_owned());
    }
    let prompt = build_correction_prompt(transcript, language);
    correct_transcript_with_prompt(claude, transcript, &prompt)
}

/// Run the LLM-based transcript correction step using a pre-built prompt.
/// Use this variant when the prompt has already been constructed (for
/// example by [`persist_correction_prompt_debug_artifact`]) to avoid the
/// non-trivial cost of rebuilding it for large transcripts.
pub fn correct_transcript_with_prompt<C: ClaudeSummaryClient>(
    claude: &C,
    transcript: &str,
    prompt: &str,
) -> Result<String, SummaryError> {
    if transcript.trim().is_empty() {
        return Ok(transcript.to_owned());
    }
    claude.summarize(prompt, None)
}
