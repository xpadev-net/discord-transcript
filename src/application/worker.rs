use crate::application::ai_memory_extraction::{
    AiMemoryExtractionStore, extract_ai_memory_candidates,
};
use crate::application::runtime::merge_user_chunks_to_mixdown;
use crate::application::summary::{
    ClaudeSummaryClient, SpeakerAudioInput, SummaryContextInput, SummaryError, SummaryRequest,
    TranscriptionOutput, build_correction_prompt_with_context, build_summary_prompt_with_context,
    correct_transcript_with_prompt_and_workdir, materialize_new_summary_agent_workspace,
    materialize_or_load_summary_context, persist_correction_prompt_debug_artifact,
    persist_pre_correction_transcript_debug_artifact, persist_summary_prompt_debug_artifact,
    run_transcription, write_transcript_files,
};
use crate::audio::meeting_audio::build_speaker_audio_inputs;
use crate::domain::confidence::ConfidencePermille;
use crate::domain::feedback::TranscriptFeedbackStatus;
use crate::domain::person_alias::{
    NewPersonAlias, PersonAlias, PersonAliasReviewStatus, PersonAliasSourceType,
};
use crate::domain::speaker::SpeakerProfile;
use crate::domain::summary_template::SummaryTemplate;
use crate::domain::usage::{
    EntitlementAction, EntitlementEvaluator, NewUsageEvent, UsageDetailJson, UsageMetric,
    UsageSnapshot,
};
use crate::domain::{JobStatus, JobType, MeetingStatus};
use crate::infrastructure::asr::WhisperClient;
use crate::infrastructure::queue::{Job, JobQueue, QueueError};
use crate::infrastructure::sql_store::{SqlExecutor, SqlMeetingStore};
use crate::infrastructure::storage::{
    EffectiveMeetingSettings, InMemoryMeetingStore, MeetingStore, StoreError, UsageEventStore,
};
use crate::infrastructure::workspace::{MeetingWorkspaceLayout, MeetingWorkspacePaths};
use crate::interfaces::posting::{DISCORD_MESSAGE_LIMIT, split_discord_message};
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use tracing::{debug, error, info, warn};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessMeetingInput {
    pub meeting_id: String,
    pub job_id: Option<String>,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub voice_channel_name: Option<String>,
    pub title: Option<String>,
    pub audio_path: String,
    pub speaker_audio: Vec<SpeakerAudioInput>,
    pub language: Option<String>,
    pub workspace: MeetingWorkspacePaths,
    pub summary_context: SummaryContextInput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessMeetingOutput {
    pub meeting_id: String,
    pub markdown: String,
    pub chunks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerError {
    Queue(String),
    Store(String),
    Summary(String),
    /// A summary job with the same ID was already present in the queue.
    /// The caller should treat this as "a claimable job already exists" and
    /// proceed to claim it rather than treating it as a fatal error.
    AlreadyExists,
}

impl Display for WorkerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Queue(err) => write!(f, "queue error: {err}"),
            Self::Store(err) => write!(f, "store error: {err}"),
            Self::Summary(err) => write!(f, "summary error: {err}"),
            Self::AlreadyExists => write!(f, "summary job already exists in queue"),
        }
    }
}

impl std::error::Error for WorkerError {}

impl From<StoreError> for WorkerError {
    fn from(value: StoreError) -> Self {
        Self::Store(value.to_string())
    }
}

impl From<QueueError> for WorkerError {
    fn from(value: QueueError) -> Self {
        Self::Queue(value.to_string())
    }
}

impl From<SummaryError> for WorkerError {
    fn from(value: SummaryError) -> Self {
        Self::Summary(value.to_string())
    }
}

pub trait SummaryContextStore {
    /// Loads context used by summary generation.
    ///
    /// SQL-backed stores may also materialize low-confidence VC participant
    /// alias candidates from the meeting speaker snapshot as part of this
    /// summary-preparation step.
    fn load_summary_context(
        &mut self,
        meeting_id: &str,
        guild_id: &str,
        effective_settings: Option<&EffectiveMeetingSettings>,
    ) -> Result<SummaryContextInput, StoreError>;
}

pub const LOAD_MEETING_SPEAKERS_SQL: &str = "SELECT speaker_id, username, nickname, display_name \
             FROM meeting_speakers WHERE meeting_id=$1 ORDER BY speaker_id";

#[derive(Debug, Clone, PartialEq, Eq)]
struct AliasCandidateTenantGuild {
    tenant_discord_guild_id: String,
    tenant_id: String,
    guild_id: String,
}

impl SummaryContextStore for InMemoryMeetingStore {
    fn load_summary_context(
        &mut self,
        _meeting_id: &str,
        _guild_id: &str,
        effective_settings: Option<&EffectiveMeetingSettings>,
    ) -> Result<SummaryContextInput, StoreError> {
        Ok(SummaryContextInput {
            effective_summary_template_id: effective_settings
                .and_then(|settings| settings.summary_template_id.clone()),
            effective_domain_knowledge_version_id: effective_settings
                .and_then(|settings| settings.domain_knowledge_version_id.clone()),
            ..SummaryContextInput::default()
        })
    }
}

impl<E: SqlExecutor> SummaryContextStore for SqlMeetingStore<E> {
    fn load_summary_context(
        &mut self,
        meeting_id: &str,
        guild_id: &str,
        effective_settings: Option<&EffectiveMeetingSettings>,
    ) -> Result<SummaryContextInput, StoreError> {
        let speakers = load_meeting_speakers(self, meeting_id)?;
        let domain_knowledge = self.list_domain_knowledge(guild_id, false, None)?;
        let summary_template = load_effective_summary_template(self, guild_id, effective_settings)?;
        if let Err(err) =
            upsert_vc_participant_alias_candidates_for_guild(self, meeting_id, guild_id, &speakers)
        {
            warn!(
                meeting_id = %meeting_id,
                guild_id = %guild_id,
                error = %err,
                "failed to upsert VC participant alias candidates"
            );
        }
        let tenant = self.resolve_tenant_by_guild(guild_id)?;
        let (ai_memory, user_feedback, person_aliases) = if let Some(tenant) = tenant.as_ref() {
            (
                self.list_ai_memory_notes(&tenant.tenant_id, guild_id, false, None)?,
                self.list_transcript_feedback(
                    &tenant.tenant_id,
                    guild_id,
                    Some(TranscriptFeedbackStatus::Accepted),
                    None,
                )?,
                self.list_person_aliases(
                    &tenant.tenant_id,
                    guild_id,
                    false,
                    Some(PersonAliasReviewStatus::Accepted),
                )?,
            )
        } else {
            (Vec::new(), Vec::new(), Vec::new())
        };

        Ok(SummaryContextInput {
            speakers,
            domain_knowledge,
            ai_memory,
            user_feedback,
            person_aliases,
            summary_template,
            effective_summary_template_id: effective_settings
                .and_then(|settings| settings.summary_template_id.clone()),
            effective_domain_knowledge_version_id: effective_settings
                .and_then(|settings| settings.domain_knowledge_version_id.clone()),
        })
    }
}

pub(crate) fn upsert_vc_participant_alias_candidates_for_guild<E: SqlExecutor>(
    store: &mut SqlMeetingStore<E>,
    meeting_id: &str,
    guild_id: &str,
    speakers: &[SpeakerProfile],
) -> Result<Vec<PersonAlias>, StoreError> {
    if speakers.is_empty() {
        return Ok(Vec::new());
    }
    let Some(tenant_guild) = resolve_alias_candidate_tenant_guild(store, guild_id)? else {
        warn!(
            meeting_id = %meeting_id,
            guild_id = %guild_id,
            "skipping VC participant alias candidates because tenant/guild ownership is unavailable"
        );
        return Ok(Vec::new());
    };
    let mut speakers = speakers.to_vec();
    speakers.sort_by(|left, right| left.speaker_id.cmp(&right.speaker_id));
    let candidates = build_vc_participant_alias_candidates(&tenant_guild, meeting_id, &speakers);
    let mut upserted = Vec::new();
    for candidate in candidates {
        match store.upsert_vc_participant_person_alias_candidate(&candidate) {
            Ok(Some(alias)) => upserted.push(alias),
            Ok(None) => {
                debug!(
                    meeting_id = %meeting_id,
                    guild_id = %guild_id,
                    canonical_name = %candidate.canonical_name,
                    alias = %candidate.alias,
                    "skipped VC participant alias candidate because existing alias was not eligible for automatic update"
                );
            }
            Err(err) => {
                warn!(
                    meeting_id = %meeting_id,
                    guild_id = %guild_id,
                    canonical_name = %candidate.canonical_name,
                    alias = %candidate.alias,
                    error = %err,
                    "failed to upsert VC participant alias candidate"
                );
            }
        }
    }
    Ok(upserted)
}

fn resolve_alias_candidate_tenant_guild<E: SqlExecutor>(
    store: &mut SqlMeetingStore<E>,
    guild_id: &str,
) -> Result<Option<AliasCandidateTenantGuild>, StoreError> {
    let rows = store
        .executor
        .query_rows(
            crate::infrastructure::sql::RESOLVE_SINGLE_ACTIVE_TENANT_GUILD_SQL,
            &[guild_id.to_owned()],
        )
        .map_err(StoreError::Backend)?;
    let Some(row) = rows.into_iter().next() else {
        return Ok(None);
    };
    if row.len() < 3 {
        return Err(StoreError::Backend(format!(
            "invalid tenant guild row length for alias candidates: {}",
            row.len()
        )));
    }
    let tenant_discord_guild_id = row
        .first()
        .and_then(|value| value.clone())
        .ok_or_else(|| StoreError::Backend("tenant_discord_guild_id is NULL".to_owned()))?;
    let tenant_id = row
        .get(1)
        .and_then(|value| value.clone())
        .ok_or_else(|| StoreError::Backend("tenant_id is NULL".to_owned()))?;
    let guild_id = row
        .get(2)
        .and_then(|value| value.clone())
        .ok_or_else(|| StoreError::Backend("guild_id is NULL".to_owned()))?;
    Ok(Some(AliasCandidateTenantGuild {
        tenant_discord_guild_id,
        tenant_id,
        guild_id,
    }))
}

fn build_vc_participant_alias_candidates(
    tenant_guild: &AliasCandidateTenantGuild,
    meeting_id: &str,
    speakers: &[SpeakerProfile],
) -> Vec<NewPersonAlias> {
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    for speaker in speakers {
        if speaker.speaker_id.parse::<u64>().is_err() {
            continue;
        }
        let Some(canonical_name) = normalize_person_alias_candidate_text(&speaker.display_label())
        else {
            continue;
        };
        for alias in [
            speaker.nickname.as_deref(),
            speaker.display_name.as_deref(),
            speaker.username.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            let Some(alias) = normalize_person_alias_candidate_text(alias) else {
                continue;
            };
            if alias.eq_ignore_ascii_case(&canonical_name) {
                continue;
            }
            let identity_key = (canonical_name.to_lowercase(), alias.to_lowercase());
            if !seen.insert(identity_key) {
                continue;
            }
            candidates.push(NewPersonAlias {
                id: vc_participant_alias_candidate_id(
                    &tenant_guild.tenant_id,
                    &tenant_guild.guild_id,
                    &canonical_name,
                    &alias,
                ),
                tenant_discord_guild_id: tenant_guild.tenant_discord_guild_id.clone(),
                tenant_id: tenant_guild.tenant_id.clone(),
                guild_id: tenant_guild.guild_id.clone(),
                canonical_name: canonical_name.clone(),
                alias,
                discord_user_id: Some(speaker.speaker_id.clone()),
                source_type: PersonAliasSourceType::VcParticipant,
                source_meeting_id: Some(meeting_id.to_owned()),
                source_feedback_id: None,
                confidence: Some(vc_participant_alias_confidence()),
                active: true,
                review_status: PersonAliasReviewStatus::Unreviewed,
                actor_user_id: "system:vc_participant".to_owned(),
            });
        }
    }
    candidates
}

fn normalize_person_alias_candidate_text(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.len() > 200
        || trimmed.chars().any(char::is_control)
        || trimmed.parse::<u64>().is_ok()
    {
        return None;
    }
    Some(trimmed.to_owned())
}

fn vc_participant_alias_candidate_id(
    tenant_id: &str,
    guild_id: &str,
    canonical_name: &str,
    alias: &str,
) -> String {
    let mut hasher = Sha256::new();
    for part in [
        "vc_participant",
        tenant_id,
        guild_id,
        &canonical_name.to_lowercase(),
        &alias.to_lowercase(),
    ] {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    let digest = hasher.finalize();
    let suffix = digest
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("vc-participant-{suffix}")
}

fn vc_participant_alias_confidence() -> ConfidencePermille {
    ConfidencePermille::new(650).expect("static VC participant confidence is valid")
}

fn load_meeting_speakers<E: SqlExecutor>(
    store: &mut SqlMeetingStore<E>,
    meeting_id: &str,
) -> Result<Vec<SpeakerProfile>, StoreError> {
    let rows = store
        .executor
        .query_rows(LOAD_MEETING_SPEAKERS_SQL, &[meeting_id.to_owned()])
        .map_err(StoreError::Backend)?;
    let mut speakers = Vec::with_capacity(rows.len());
    for row in rows {
        if row.len() < 4 {
            return Err(StoreError::Backend(format!(
                "invalid meeting speaker row length: {}",
                row.len()
            )));
        }
        let Some(speaker_id) = row.first().and_then(|value| value.clone()) else {
            continue;
        };
        speakers.push(SpeakerProfile {
            speaker_id,
            username: row.get(1).and_then(|value| value.clone()),
            nickname: row.get(2).and_then(|value| value.clone()),
            display_name: row.get(3).and_then(|value| value.clone()),
        });
    }
    Ok(speakers)
}

pub(crate) fn load_effective_summary_template<E: SqlExecutor>(
    store: &mut SqlMeetingStore<E>,
    guild_id: &str,
    effective_settings: Option<&EffectiveMeetingSettings>,
) -> Result<Option<SummaryTemplate>, StoreError> {
    if let Some(template_id) = effective_settings
        .and_then(|settings| settings.summary_template_id.as_deref())
        .filter(|template_id| !template_id.trim().is_empty())
    {
        return store.get_summary_template(guild_id, template_id);
    }
    store.get_active_summary_template(guild_id)
}

fn advance_to_transcribing<S: MeetingStore>(
    store: &mut S,
    meeting_id: &str,
) -> Result<(), WorkerError> {
    for expected in [
        MeetingStatus::Stopping,
        MeetingStatus::Transcribing,
        MeetingStatus::Summarizing,
    ] {
        match store.set_meeting_status(meeting_id, MeetingStatus::Transcribing, Some(expected)) {
            Ok(()) => return Ok(()),
            Err(StoreError::CasConflict { .. }) => continue,
            Err(err) => return Err(err.into()),
        }
    }
    Err(WorkerError::Store(format!(
        "could not advance meeting {meeting_id} to transcribing"
    )))
}

fn revert_to_stopping_for_retry<S: MeetingStore>(
    store: &mut S,
    meeting_id: &str,
    from: MeetingStatus,
) {
    if let Err(err) = store.set_meeting_status(meeting_id, MeetingStatus::Stopping, Some(from)) {
        warn!(
            meeting_id = %meeting_id,
            from = ?from,
            error = %err,
            "failed to revert meeting to stopping after pipeline error"
        );
    }
}

pub(crate) fn mark_summary_meeting_failed_from_summary_state<S: MeetingStore>(
    store: &mut S,
    meeting_id: &str,
    error_message: String,
) -> Result<bool, StoreError> {
    for expected in [
        MeetingStatus::Stopping,
        MeetingStatus::Transcribing,
        MeetingStatus::Summarizing,
    ] {
        match store.set_meeting_status(meeting_id, MeetingStatus::Failed, Some(expected)) {
            Ok(()) => {
                if let Err(err) = store.set_error_message(meeting_id, Some(error_message)) {
                    warn!(
                        meeting_id = %meeting_id,
                        error = %err,
                        "mark_summary_meeting_failed_from_summary_state set_meeting_status succeeded but set_error_message failed"
                    );
                    return Err(err);
                }
                return Ok(true);
            }
            Err(StoreError::CasConflict { .. }) => continue,
            Err(err) => return Err(err),
        }
    }
    warn!(
        meeting_id = %meeting_id,
        "summary job exhausted retries but meeting is no longer in a summary-owned status"
    );
    Ok(false)
}

pub fn process_meeting_summary<S, W, C>(
    store: &mut S,
    whisper: &W,
    claude: &C,
    input: &ProcessMeetingInput,
) -> Result<ProcessMeetingOutput, WorkerError>
where
    S: MeetingStore + AiMemoryExtractionStore,
    W: WhisperClient,
    C: ClaudeSummaryClient,
{
    process_meeting_summary_with_ownership_check(store, whisper, claude, input, || Ok(()))
}

fn process_meeting_summary_with_ownership_check<S, W, C, F>(
    store: &mut S,
    whisper: &W,
    claude: &C,
    input: &ProcessMeetingInput,
    mut ensure_owned: F,
) -> Result<ProcessMeetingOutput, WorkerError>
where
    S: MeetingStore + AiMemoryExtractionStore,
    W: WhisperClient,
    C: ClaudeSummaryClient,
    F: FnMut() -> Result<(), WorkerError>,
{
    info!(meeting_id = %input.meeting_id, "summary pipeline started");

    let request = SummaryRequest {
        meeting_id: input.meeting_id.clone(),
        guild_id: input.guild_id.clone(),
        voice_channel_id: input.voice_channel_id.clone(),
        voice_channel_name: input.voice_channel_name.clone(),
        title: input.title.clone(),
        audio_path: input.audio_path.clone(),
        speaker_audio: input.speaker_audio.clone(),
        language: input.language.clone(),
        workspace: input.workspace.clone(),
    };

    ensure_owned()?;
    advance_to_transcribing(store, &input.meeting_id)?;
    let transcription_result = run_transcription(whisper, &request);
    ensure_owned()?;
    let transcription = match transcription_result {
        Ok(value) => value,
        Err(err) => {
            error!(meeting_id = %input.meeting_id, error = %err, "transcription failed");
            revert_to_stopping_for_retry(store, &input.meeting_id, MeetingStatus::Transcribing);
            return Err(WorkerError::from(err));
        }
    };
    match asr_seconds_from_audio_path(&input.audio_path) {
        Ok(asr_seconds) => {
            // The runtime scaffold and batch worker are deployment alternatives today.
            // Keep the same ASR event id so retries remain idempotent if topology changes.
            record_usage_event_observe_only(
                store,
                NewUsageEvent {
                    id: format!("usage:asr_seconds:{}", input.meeting_id),
                    tenant_id: None,
                    guild_id: input.guild_id.clone(),
                    meeting_id: Some(input.meeting_id.clone()),
                    job_id: input.job_id.clone(),
                    resource_type: Some("meeting".to_owned()),
                    resource_id: Some(input.meeting_id.clone()),
                    metric: UsageMetric::AsrSeconds,
                    quantity: asr_seconds,
                    detail_json: UsageDetailJson::new(serde_json::json!({
                        "source": "audio_duration",
                        "whisper_segment_count": transcription.segments.len(),
                        "surface": "process_meeting_summary_done"
                    }))
                    .expect("usage detail must be a JSON object"),
                    observed_at: Utc::now(),
                },
            );
        }
        Err(err) => warn!(
            meeting_id = %input.meeting_id,
            audio_path = %input.audio_path,
            error = %err,
            "skipping ASR usage event because audio duration is unavailable"
        ),
    }

    persist_pre_correction_transcript_debug_artifact(
        &request.workspace,
        &transcription.transcript_for_summary,
    );
    ensure_owned()?;
    store.set_meeting_status(
        &input.meeting_id,
        MeetingStatus::Summarizing,
        Some(MeetingStatus::Transcribing),
    )?;
    let context_manifest_result =
        materialize_or_load_summary_context(&request, &input.summary_context);
    ensure_owned()?;
    let context_manifest = match context_manifest_result {
        Ok(value) => value,
        Err(err) => {
            error!(meeting_id = %input.meeting_id, error = %err, "summary context materialization failed");
            revert_to_stopping_for_retry(store, &input.meeting_id, MeetingStatus::Summarizing);
            return Err(WorkerError::from(err));
        }
    };

    // Apply LLM-based error correction to the transcript before summarization.
    let transcription = if !claude.supports_transcript_correction() {
        info!(
            meeting_id = %input.meeting_id,
            "skipping LLM transcript correction (not supported by summary harness)"
        );
        transcription
    } else {
        // Reuse the persisted prompt (when one was written) instead of rebuilding
        // it inside `correct_transcript`. Falls back to a fresh build only for the
        // (rare) case where the artifact was skipped — e.g., transcript fully
        // empty — but `correct_transcript_with_prompt` still short-circuits there.
        let correction_prompt = persist_correction_prompt_debug_artifact(
            &request.workspace,
            &transcription.transcript_for_summary,
            request.language.as_deref(),
            Some(&context_manifest),
        )
        .unwrap_or_else(|| {
            build_correction_prompt_with_context(
                &transcription.transcript_for_summary,
                request.language.as_deref(),
                Some(&context_manifest),
            )
        });

        match correct_transcript_with_prompt_and_workdir(
            claude,
            &transcription.transcript_for_summary,
            &correction_prompt,
            Some(request.workspace.root()),
        ) {
            Ok(corrected) => TranscriptionOutput {
                transcript_for_summary: corrected,
                ..transcription
            },
            Err(err) => {
                warn!(meeting_id = %input.meeting_id, error = %err, "transcript correction failed, using original");
                transcription
            }
        }
    };

    let manifest_result = write_transcript_files(&request, &transcription);
    ensure_owned()?;
    let manifest = match manifest_result {
        Ok(value) => value,
        Err(err) => {
            error!(meeting_id = %input.meeting_id, error = %err, "transcript materialization failed");
            revert_to_stopping_for_retry(store, &input.meeting_id, MeetingStatus::Summarizing);
            return Err(WorkerError::from(err));
        }
    };
    let prompt = build_summary_prompt_with_context(&request, &manifest, Some(&context_manifest));
    persist_summary_prompt_debug_artifact(&request.workspace, &prompt);
    let agent_workspace_result = materialize_new_summary_agent_workspace(&request);
    ensure_owned()?;
    let agent_workspace = match agent_workspace_result {
        Ok(value) => value,
        Err(err) => {
            error!(meeting_id = %input.meeting_id, error = %err, "summary agent workspace materialization failed");
            revert_to_stopping_for_retry(store, &input.meeting_id, MeetingStatus::Summarizing);
            return Err(WorkerError::from(err));
        }
    };
    let markdown_result = claude.summarize(&prompt, Some(agent_workspace.root()));
    ensure_owned()?;
    let markdown = match markdown_result {
        Ok(value) => value,
        Err(err) => {
            error!(meeting_id = %input.meeting_id, error = %err, "summarization failed");
            revert_to_stopping_for_retry(store, &input.meeting_id, MeetingStatus::Summarizing);
            return Err(WorkerError::from(err));
        }
    };
    if store.supports_ai_memory_extraction() {
        match extract_ai_memory_candidates(
            claude,
            &request,
            &transcription.transcript_for_summary,
            &markdown,
            &context_manifest,
        ) {
            Ok(candidates) => {
                ensure_owned()?;
                match store.persist_ai_memory_extraction_candidates(
                    &input.meeting_id,
                    &input.guild_id,
                    &candidates,
                ) {
                    Ok(saved) => info!(
                        meeting_id = %input.meeting_id,
                        proposed = candidates.len(),
                        saved,
                        "AI memory extraction completed"
                    ),
                    Err(err) => warn!(
                        meeting_id = %input.meeting_id,
                        error = %err,
                        "failed to persist AI memory extraction candidates; keeping summary completion successful"
                    ),
                }
            }
            Err(err) => warn!(
                meeting_id = %input.meeting_id,
                error = %err,
                "AI memory extraction failed; keeping summary completion successful"
            ),
        }
    }
    ensure_owned()?;

    let chunks = split_discord_message(&markdown, DISCORD_MESSAGE_LIMIT);
    info!(
        meeting_id = %input.meeting_id,
        chunks = chunks.len(),
        "summary pipeline completed"
    );

    Ok(ProcessMeetingOutput {
        meeting_id: input.meeting_id.clone(),
        markdown,
        chunks,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessJobResult {
    pub job_id: String,
    pub output: ProcessMeetingOutput,
}

fn has_nonempty_audio_chunk(meeting_dir: &Path) -> Result<bool, String> {
    let entries = match std::fs::read_dir(meeting_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(format!(
                "failed to read meeting dir {}: {err}",
                meeting_dir.display()
            ));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|err| format!("failed to read dir entry: {err}"))?;
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase());
        let is_candidate = matches!(ext.as_deref(), Some("wav"))
            && path.file_stem().and_then(|stem| stem.to_str()) != Some("mixdown");
        if !is_candidate {
            continue;
        }
        let size = entry
            .metadata()
            .map_err(|err| format!("failed to read metadata {}: {err}", path.display()))?
            .len();
        if size > 44 {
            return Ok(true);
        }
    }
    Ok(false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryJobOptions {
    pub max_retries: u32,
    pub audio_base_dir: String,
    pub language: Option<String>,
    pub resample_to_16k: bool,
}

pub fn process_next_summary_job<S, Q, W, C>(
    store: &mut S,
    queue: &mut Q,
    whisper: &W,
    claude: &C,
    options: &SummaryJobOptions,
) -> Result<Option<ProcessJobResult>, WorkerError>
where
    S: MeetingStore + SummaryContextStore + AiMemoryExtractionStore,
    Q: JobQueue,
    W: WhisperClient,
    C: ClaudeSummaryClient,
{
    let Some(job) = queue.claim_next(JobType::Summarize)? else {
        return Ok(None);
    };
    info!(job_id = %job.id, meeting_id = %job.meeting_id, "claimed summary job");

    let result = (|| {
        let meeting = store
            .get_meeting(&job.meeting_id)
            .map_err(WorkerError::from)?
            .ok_or_else(|| {
                WorkerError::Store(format!("meeting not found for summary: {}", job.meeting_id))
            })?;
        let effective_settings = store
            .get_effective_meeting_settings(&job.meeting_id)
            .map_err(WorkerError::from)?;
        if effective_settings
            .as_ref()
            .is_some_and(|settings| !settings.summary_enabled)
        {
            match meeting.status {
                MeetingStatus::Posted => {}
                MeetingStatus::Stopping => {
                    queue.heartbeat(&job)?;
                    store.set_meeting_status(
                        &job.meeting_id,
                        MeetingStatus::Posted,
                        Some(MeetingStatus::Stopping),
                    )?;
                }
                status => {
                    return Err(WorkerError::Store(format!(
                        "cannot suppress disabled summary job for meeting {} in status {}",
                        job.meeting_id,
                        status.as_str()
                    )));
                }
            }
            queue.heartbeat(&job)?;
            queue.mark_done(&job)?;
            info!(
                job_id = %job.id,
                meeting_id = %job.meeting_id,
                "summary job marked done without processing because meeting snapshot disabled summaries"
            );
            return Ok(None);
        }
        let layout = MeetingWorkspaceLayout::new(&options.audio_base_dir);
        let workspace = layout.for_meeting(
            &meeting.guild_id,
            &meeting.voice_channel_id,
            &job.meeting_id,
        );
        workspace.ensure_base_dirs().map_err(|err| {
            WorkerError::from(SummaryError::SummaryEngine(format!(
                "failed to prepare workspace: {err}"
            )))
        })?;
        let legacy_dir = layout.legacy_meeting_dir(&job.meeting_id);
        let primary_dir = workspace.audio_dir();
        let primary_has_nonempty =
            has_nonempty_audio_chunk(&primary_dir).map_err(WorkerError::Summary)?;
        let resample_to_16k = effective_settings
            .as_ref()
            .map(|settings| settings.whisper_resample_to_16k)
            .unwrap_or(options.resample_to_16k);
        let meeting_dir = if primary_has_nonempty {
            primary_dir.clone()
        } else {
            let legacy_has_nonempty =
                has_nonempty_audio_chunk(&legacy_dir).map_err(WorkerError::Summary)?;
            if legacy_has_nonempty {
                let expected_mixdown_path = legacy_dir.join("mixdown.wav");
                warn!(
                    meeting_id = %job.meeting_id,
                    path = %expected_mixdown_path.display(),
                    "workspace audio dir missing non-empty chunks; falling back to legacy mixdown path"
                );
                legacy_dir.clone()
            } else {
                return Err(WorkerError::Summary(format!(
                    "no non-empty audio chunks found for meeting {} in {} or {}",
                    job.meeting_id,
                    primary_dir.display(),
                    legacy_dir.display()
                )));
            }
        };

        let mixdown_path = merge_user_chunks_to_mixdown(&meeting_dir, resample_to_16k)
            .map_err(WorkerError::Summary)?;
        let input = ProcessMeetingInput {
            meeting_id: job.meeting_id.clone(),
            job_id: Some(job.id.clone()),
            guild_id: meeting.guild_id.clone(),
            voice_channel_id: meeting.voice_channel_id.clone(),
            voice_channel_name: meeting.voice_channel_name.clone(),
            title: meeting.title.clone(),
            audio_path: mixdown_path,
            speaker_audio: build_speaker_audio_inputs(&meeting_dir, resample_to_16k)
                .map_err(WorkerError::Summary)?,
            language: effective_settings
                .as_ref()
                .and_then(|settings| settings.whisper_language.clone())
                .or_else(|| options.language.clone()),
            workspace,
            summary_context: store
                .load_summary_context(
                    &job.meeting_id,
                    &meeting.guild_id,
                    effective_settings.as_ref(),
                )
                .map_err(WorkerError::from)?,
        };
        process_meeting_summary_with_ownership_check(store, whisper, claude, &input, || {
            queue.heartbeat(&job).map_err(WorkerError::from)
        })
        .map(Some)
    })();
    match result {
        Ok(Some(output)) => {
            queue.heartbeat(&job)?;
            // Set meeting status first: if this fails the job stays Running
            // and can be retried. The reverse order (mark_done first) would
            // leave the meeting stuck in Summarizing with no way to recover.
            store.set_meeting_status(
                &job.meeting_id,
                MeetingStatus::Posted,
                Some(MeetingStatus::Summarizing),
            )?;
            queue.mark_done(&job)?;
            record_summary_run_usage_observe_only(
                store,
                &job.meeting_id,
                &job.id,
                output.chunks.len(),
            );
            info!(job_id = %job.id, "summary job marked done");
            Ok(Some(ProcessJobResult {
                job_id: job.id,
                output,
            }))
        }
        Ok(None) => Ok(None),
        Err(err) => {
            queue.heartbeat(&job)?;
            let status = queue.retry(&job, err.to_string(), options.max_retries)?;
            if status == JobStatus::Failed {
                mark_summary_meeting_failed_from_summary_state(
                    store,
                    &job.meeting_id,
                    err.to_string(),
                )
                .map_err(WorkerError::from)?;
                warn!(
                    job_id = %job.id,
                    meeting_id = %job.meeting_id,
                    "summary job exhausted retries"
                );
            } else {
                info!(
                    job_id = %job.id,
                    meeting_id = %job.meeting_id,
                    "summary job queued for retry"
                );
            }
            Err(err)
        }
    }
}

/// Compute the audio duration in whole seconds from a WAV file header.
/// Only the canonical 44-byte PCM layout is supported (RIFF/WAVE, `fmt `
/// chunk size == 16, `data` chunk starting at byte 36). Files with extended
/// format chunks or interleaved metadata chunks return `Err` so the caller
/// can warn and skip the observe-only ASR usage event without failing.
pub(crate) fn asr_seconds_from_audio_path(audio_path: &str) -> Result<i64, String> {
    let mut file =
        File::open(audio_path).map_err(|err| format!("failed to open ASR audio file: {err}"))?;
    let mut header = [0_u8; 44];
    file.read_exact(&mut header)
        .map_err(|err| format!("failed to read ASR audio header: {err}"))?;
    let duration_ms = wav_header_duration_ms(&header)?;
    Ok(duration_ms.div_ceil(1000).min(i64::MAX as u64) as i64)
}

fn wav_header_duration_ms(header: &[u8; 44]) -> Result<u64, String> {
    let fmt_chunk_size = u32::from_le_bytes([header[16], header[17], header[18], header[19]]);
    let audio_format = u16::from_le_bytes([header[20], header[21]]);
    // Only the canonical 44-byte PCM layout is supported here. Files with
    // metadata chunks between `fmt ` and `data` return Err so the caller can
    // warn and skip the observe-only ASR usage event.
    if &header[0..4] != b"RIFF"
        || &header[8..12] != b"WAVE"
        || &header[12..16] != b"fmt "
        || fmt_chunk_size != 16
        || audio_format != 1
        || &header[36..40] != b"data"
    {
        return Err(format!(
            "ASR audio file is not a supported PCM WAV: audio_format={audio_format}, fmt_chunk_size={fmt_chunk_size}"
        ));
    }
    let byte_rate = u32::from_le_bytes([header[28], header[29], header[30], header[31]]) as u128;
    if byte_rate == 0 {
        return Err("ASR audio file is not a supported PCM WAV: byte_rate=0".to_owned());
    }
    let data_size = u32::from_le_bytes([header[40], header[41], header[42], header[43]]);
    // Conforming RF64 streaming files should fail the canonical-header checks
    // above, but keep this defensive guard for malformed sentinel-like inputs.
    if data_size == 0 || data_size == u32::MAX {
        return Err(format!(
            "ASR audio file is not a supported PCM WAV: data_size={data_size}"
        ));
    }
    let data_size = data_size as u128;
    Ok(data_size.saturating_mul(1_000).div_ceil(byte_rate) as u64)
}

fn record_usage_event_observe_only<S: UsageEventStore>(store: &mut S, event: NewUsageEvent) {
    if let Err(err) = store.append_usage_event(&event) {
        warn!(
            usage_event_id = %event.id,
            metric = %event.metric.as_str(),
            error = %err,
            "failed to append usage event; continuing in observe-only mode"
        );
    }
}

fn record_summary_run_usage_observe_only<S: MeetingStore>(
    store: &mut S,
    meeting_id: &str,
    job_id: &str,
    chunk_count: usize,
) {
    let meeting = match store.get_meeting(meeting_id) {
        Ok(Some(meeting)) => meeting,
        Ok(None) => {
            warn!(meeting_id, "meeting missing while recording summary usage");
            return;
        }
        Err(err) => {
            warn!(
                meeting_id,
                error = %err,
                "failed to load meeting for summary usage"
            );
            return;
        }
    };
    record_usage_event_observe_only(
        store,
        NewUsageEvent {
            id: format!("usage:summary_runs:{meeting_id}"),
            tenant_id: None,
            guild_id: meeting.guild_id.clone(),
            meeting_id: Some(meeting_id.to_owned()),
            job_id: Some(job_id.to_owned()),
            resource_type: Some("meeting".to_owned()),
            resource_id: Some(meeting_id.to_owned()),
            metric: UsageMetric::SummaryRuns,
            quantity: 1,
            detail_json: UsageDetailJson::new(serde_json::json!({
                "chunk_count": chunk_count,
                "surface": "process_next_summary_job_done"
            }))
            .expect("usage detail must be a JSON object"),
            observed_at: Utc::now(),
        },
    );
    // Unlike the scaffold path, the batch worker observes synchronously.
    // The aggregate includes the summary_runs event just written above,
    // giving the desired post-completion usage snapshot.
    observe_worker_completion_entitlement(store, &meeting.guild_id);
}

pub(crate) fn observe_worker_completion_entitlement<S: UsageEventStore>(
    store: &mut S,
    guild_id: &str,
) {
    let aggregates = match store.aggregate_recent_usage(None, Some(guild_id), 30 * 24 * 60 * 60) {
        Ok(aggregates) => aggregates,
        Err(err) => {
            warn!(
                guild_id,
                error = %err,
                "usage entitlement observation failed after worker completion"
            );
            return;
        }
    };
    let snapshot = UsageSnapshot::from_aggregates(aggregates);
    let decision =
        EntitlementEvaluator::observe_only().evaluate(EntitlementAction::CompleteWorker, &snapshot);
    if decision
        .observations
        .iter()
        .any(|observation| observation.exceeded)
    {
        warn!(
            guild_id,
            observations = ?decision.observations,
            "usage entitlement would exceed policy; observe-only mode allows worker completion"
        );
    }
}

pub fn enqueue_summary_job<Q: JobQueue>(
    queue: &mut Q,
    job_id: &str,
    meeting_id: &str,
) -> Result<(), WorkerError> {
    match queue.enqueue(Job {
        id: job_id.to_owned(),
        meeting_id: meeting_id.to_owned(),
        job_type: JobType::Summarize,
        status: JobStatus::Queued,
        retry_count: 0,
        error_message: None,
        next_run_at: None,
        claim_token: None,
    }) {
        Ok(()) => {}
        Err(QueueError::AlreadyExists { .. }) => return Err(WorkerError::AlreadyExists),
        Err(err) => return Err(WorkerError::Queue(err.to_string())),
    }
    info!(job_id = %job_id, meeting_id = %meeting_id, "summary job enqueued");
    Ok(())
}
