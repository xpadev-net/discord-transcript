use crate::application::ai_memory_extraction::{
    AiMemoryExtractionStore, extract_ai_memory_candidates,
};
use crate::application::runtime::{has_nonempty_audio_chunk, merge_user_chunks_to_mixdown};
use crate::application::summary::{
    ClaudeSummaryClient, SpeakerAudioInput, SummaryContextInput, SummaryError, SummaryRequest,
    TranscriptionOutput, build_correction_prompt_with_context, build_summary_prompt_with_context,
    correct_transcript_with_prompt_and_workdir, ensure_untrusted_agent_workspace_supported,
    materialize_new_summary_agent_workspace, materialize_or_load_summary_context,
    persist_correction_prompt_debug_artifact, persist_meeting_title_debug_artifact,
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
use crate::infrastructure::workspace::{
    AGENT_SUMMARY_OUTPUT_FILENAME, MeetingWorkspaceLayout, MeetingWorkspacePaths,
};
use crate::interfaces::posting::{DISCORD_MESSAGE_LIMIT, split_discord_message};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::fs::{self, File};
use std::io::Read;
use std::num::NonZeroUsize;
use tracing::{debug, error, info, warn};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessMeetingInput {
    pub meeting_id: String,
    pub job_id: Option<String>,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub voice_channel_name: Option<String>,
    pub title: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub duration_seconds: Option<u64>,
    pub audio_path: String,
    pub speaker_audio: Vec<SpeakerAudioInput>,
    pub language: Option<String>,
    pub workspace: MeetingWorkspacePaths,
    pub summary_context: SummaryContextInput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessMeetingOutput {
    pub meeting_id: String,
    pub title: String,
    pub markdown: String,
    pub chunks: Vec<String>,
}

const MEETING_TITLE_MAX_CHARS: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerError {
    Queue(String),
    Store(String),
    Summary(String),
    Completion(String),
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
            Self::Completion(err) => write!(f, "summary completion error: {err}"),
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
        started_at: input.started_at,
        stopped_at: input.stopped_at,
        duration_seconds: input.duration_seconds,
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

    ensure_owned()?;
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
    ensure_owned()?;
    let context_manifest_result = materialize_or_load_summary_context(
        &request,
        &input.summary_context,
        Some(&transcription.transcript_for_summary),
    );
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
        ensure_owned()?;
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

    ensure_owned()?;
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
    if let Err(err) = ensure_untrusted_agent_workspace_supported(claude) {
        error!(meeting_id = %input.meeting_id, error = %err, "summary harness cannot safely process untrusted agent workspace");
        revert_to_stopping_for_retry(store, &input.meeting_id, MeetingStatus::Summarizing);
        return Err(WorkerError::from(err));
    }
    let prompt = build_summary_prompt_with_context(&request, &manifest, Some(&context_manifest));
    ensure_owned()?;
    persist_summary_prompt_debug_artifact(&request.workspace, &prompt);
    ensure_owned()?;
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
    ensure_owned()?;
    if let Err(err) = persist_generated_summary_markdown(&request.workspace, &markdown) {
        error!(meeting_id = %input.meeting_id, error = %err, "generated summary persistence failed");
        revert_to_stopping_for_retry(store, &input.meeting_id, MeetingStatus::Summarizing);
        return Err(WorkerError::from(err));
    }
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

    let title = derive_meeting_title(
        input.title.as_deref(),
        &markdown,
        input.voice_channel_name.as_deref(),
        &input.voice_channel_id,
        &input.meeting_id,
    );
    ensure_owned()?;
    store.set_meeting_title(&input.meeting_id, title.clone())?;
    ensure_owned()?;
    persist_meeting_title_debug_artifact(&request.workspace, &title);
    ensure_owned()?;

    let post_markdown = markdown_with_title_for_post(&title, &markdown);
    let chunks = split_discord_message(&post_markdown, DISCORD_MESSAGE_LIMIT);
    info!(
        meeting_id = %input.meeting_id,
        title = %title,
        chunks = chunks.len(),
        "summary pipeline completed"
    );

    Ok(ProcessMeetingOutput {
        meeting_id: input.meeting_id.clone(),
        title,
        markdown,
        chunks,
    })
}

pub(crate) fn derive_meeting_title(
    existing_title: Option<&str>,
    markdown: &str,
    voice_channel_name: Option<&str>,
    voice_channel_id: &str,
    meeting_id: &str,
) -> String {
    sanitize_meeting_title(existing_title)
        .or_else(|| extract_title_from_summary_markdown(markdown))
        .unwrap_or_else(|| fallback_meeting_title(voice_channel_name, voice_channel_id, meeting_id))
}

fn sanitize_meeting_title(value: Option<&str>) -> Option<String> {
    let normalized = value?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(['#', '*', '_', '`', '"', '\''])
        .trim()
        .to_owned();
    if normalized.is_empty()
        || normalized.chars().any(char::is_control)
        || normalized.chars().count() > MEETING_TITLE_MAX_CHARS
    {
        return None;
    }
    Some(normalized)
}

fn extract_title_from_summary_markdown(markdown: &str) -> Option<String> {
    let mut saw_summary_heading = false;
    let mut in_code_block = false;
    for line in markdown.lines() {
        let line = line.trim();
        if line.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block || line.is_empty() || line == "---" {
            continue;
        }
        if let Some(heading) = line.strip_prefix('#') {
            let heading = heading.trim_start_matches('#').trim();
            if let Some(title) = sanitize_meeting_title(Some(heading)) {
                if !matches_builtin_summary_heading(&title) {
                    return Some(title);
                }
                saw_summary_heading = true;
            }
            continue;
        }
        if let Some(title) = line
            .strip_prefix("Title:")
            .or_else(|| line.strip_prefix("タイトル:"))
            .and_then(|value| sanitize_meeting_title(Some(value)))
        {
            return Some(title);
        }
        if saw_summary_heading && let Some(title) = title_from_summary_text_line(line) {
            return Some(title);
        }
    }
    None
}

fn title_from_summary_text_line(line: &str) -> Option<String> {
    let without_marker = line
        .trim_start_matches(['-', '*', '・'])
        .trim_start_matches(|ch: char| ch.is_ascii_digit() || ch == '.')
        .trim();
    let first_sentence = without_marker
        .split(['。', '.', '!', '?', '\n'])
        .next()
        .unwrap_or(without_marker)
        .trim();
    let title = first_sentence
        .trim_matches(['#', '*', '_', '`', '"', '\'', ':', '：'])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let title = truncate_title_at_word_boundary(&title);
    sanitize_meeting_title(Some(&title))
}

fn truncate_title_at_word_boundary(title: &str) -> String {
    if title.chars().count() <= MEETING_TITLE_MAX_CHARS {
        return title.to_owned();
    }
    let mut truncated = title
        .chars()
        .take(MEETING_TITLE_MAX_CHARS)
        .collect::<String>();
    if let Some((prefix, _)) = truncated.rsplit_once(' ')
        && !prefix.trim().is_empty()
    {
        truncated = prefix.to_owned();
    }
    truncated.trim().to_owned()
}

fn matches_builtin_summary_heading(title: &str) -> bool {
    matches!(
        title.to_ascii_lowercase().as_str(),
        "summary" | "decisions" | "todo" | "open questions"
    ) || matches!(title, "概要" | "決定事項" | "TODO" | "未解決事項" | "課題")
}

fn fallback_meeting_title(
    voice_channel_name: Option<&str>,
    voice_channel_id: &str,
    meeting_id: &str,
) -> String {
    let channel = voice_channel_name
        .and_then(|name| sanitize_meeting_title(Some(name)))
        .unwrap_or_else(|| format!("VC {}", voice_channel_id));
    sanitize_meeting_title(Some(&format!("{} meeting", channel)))
        .unwrap_or_else(|| format!("Meeting {}", meeting_id))
}

pub(crate) fn markdown_with_title_for_post(title: &str, markdown: &str) -> String {
    let trimmed = markdown.trim_start();
    let expected_heading = format!("# {title}");
    if trimmed
        .lines()
        .next()
        .is_some_and(|line| line.trim() == expected_heading)
    {
        return markdown.to_owned();
    }
    format!("# {title}\n\n{}", markdown.trim_start())
}

pub(crate) fn persist_generated_summary_markdown(
    workspace: &MeetingWorkspacePaths,
    markdown: &str,
) -> Result<(), SummaryError> {
    let summary_dir = workspace.summary_dir();
    fs::create_dir_all(&summary_dir).map_err(|err| {
        SummaryError::SummaryEngine(format!(
            "failed to prepare generated summary directory {}: {err}",
            summary_dir.display()
        ))
    })?;
    let summary_path = summary_dir.join(AGENT_SUMMARY_OUTPUT_FILENAME);
    fs::write(&summary_path, markdown).map_err(|err| {
        SummaryError::SummaryEngine(format!(
            "failed to persist generated summary {}: {err}",
            summary_path.display()
        ))
    })?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessJobResult {
    pub job_id: String,
    pub job: Job,
    pub output: ProcessMeetingOutput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryUrlNotification {
    NotConfigured,
    Posted,
    FailedBestEffort,
    NotAttempted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryStatusNotification {
    Updated,
    FailedBestEffort,
    NotAttempted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SummaryNotificationReceipt {
    posted_chunk_count: NonZeroUsize,
    pub url_notification: SummaryUrlNotification,
    pub status_notification: SummaryStatusNotification,
}

impl SummaryNotificationReceipt {
    pub fn new(
        posted_chunk_count: usize,
        url_notification: SummaryUrlNotification,
        status_notification: SummaryStatusNotification,
    ) -> Result<Self, WorkerError> {
        let posted_chunk_count = NonZeroUsize::new(posted_chunk_count).ok_or_else(|| {
            WorkerError::Completion(
                "summary completion requires at least one successful Discord post chunk".to_owned(),
            )
        })?;
        if url_notification == SummaryUrlNotification::NotAttempted {
            return Err(WorkerError::Completion(
                "summary completion requires an explicit meeting URL notification outcome"
                    .to_owned(),
            ));
        }
        if status_notification == SummaryStatusNotification::NotAttempted {
            return Err(WorkerError::Completion(
                "summary completion requires an explicit status message update outcome".to_owned(),
            ));
        }
        Ok(Self {
            posted_chunk_count,
            url_notification,
            status_notification,
        })
    }

    pub fn posted_chunk_count(self) -> usize {
        self.posted_chunk_count.get()
    }
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
            started_at: meeting.started_at,
            stopped_at: meeting.stopped_at,
            duration_seconds: meeting.duration_seconds,
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
            info!(
                job_id = %job.id,
                meeting_id = %job.meeting_id,
                "summary job generated; awaiting notification before completion"
            );
            Ok(Some(ProcessJobResult {
                job_id: job.id.clone(),
                job,
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

pub fn complete_summary_job_after_notification<S, Q>(
    store: &mut S,
    queue: &mut Q,
    job: &Job,
    receipt: SummaryNotificationReceipt,
) -> Result<bool, WorkerError>
where
    S: MeetingStore,
    Q: JobQueue,
{
    let posted_chunk_count = receipt.posted_chunk_count();
    queue.heartbeat(job)?;
    // Set meeting status first: if this fails the job stays Running and can be
    // retried. The reverse order (mark_done first) would leave the meeting
    // stuck in Summarizing with no way to recover.
    store.set_meeting_status(
        &job.meeting_id,
        MeetingStatus::Posted,
        Some(MeetingStatus::Summarizing),
    )?;
    store.set_error_message(&job.meeting_id, None)?;
    match queue.mark_done(job) {
        Ok(()) => {
            info!(
                job_id = %job.id,
                posted_chunk_count,
                "summary job marked done after notification"
            );
            Ok(true)
        }
        Err(err) => {
            error!(
                job_id = %job.id,
                meeting_id = %job.meeting_id,
                error = %err,
                "failed to mark summary job as done after notification"
            );
            Ok(false)
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
        leased_until: None,
    }) {
        Ok(()) => {}
        Err(QueueError::AlreadyExists { .. }) => return Err(WorkerError::AlreadyExists),
        Err(err) => return Err(WorkerError::Queue(err.to_string())),
    }
    info!(job_id = %job_id, meeting_id = %meeting_id, "summary job enqueued");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::build_wav_bytes_raw;
    use crate::infrastructure::asr::StubWhisperClient;
    use crate::infrastructure::storage::StoredMeeting;
    use std::path::PathBuf;

    struct SummaryOnlyTestClient;

    impl ClaudeSummaryClient for SummaryOnlyTestClient {
        fn supports_transcript_correction(&self) -> bool {
            false
        }

        fn supports_untrusted_agent_workspace(&self) -> bool {
            true
        }

        fn summarize(
            &self,
            _prompt: &str,
            _workdir: Option<&std::path::Path>,
        ) -> Result<String, SummaryError> {
            Ok("## Summary\nshould not run".to_owned())
        }
    }

    struct TempWorkspaceGuard {
        base: PathBuf,
        workspace: MeetingWorkspacePaths,
    }

    impl Drop for TempWorkspaceGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    fn temp_workspace(meeting_id: &str) -> TempWorkspaceGuard {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "discord_transcript_worker_ownership_{meeting_id}_{nanos}"
        ));
        let layout = MeetingWorkspaceLayout::new(&base);
        let workspace = layout.for_meeting("g1", "vc", meeting_id);
        std::fs::create_dir_all(workspace.audio_dir()).expect("audio dir should be created");
        let wav = build_wav_bytes_raw(&vec![0; 2_000], 1_000, 1, 16).expect("wav should build");
        std::fs::write(workspace.mixdown_path(), wav).expect("mixdown should be written");
        TempWorkspaceGuard { base, workspace }
    }

    fn stopping_meeting() -> StoredMeeting {
        StoredMeeting {
            id: "m1".to_owned(),
            guild_id: "g1".to_owned(),
            voice_channel_id: "vc".to_owned(),
            voice_channel_name: None,
            report_channel_id: "c1".to_owned(),
            status_message_channel_id: None,
            status_message_id: None,
            started_by_user_id: "u1".to_owned(),
            title: None,
            status: MeetingStatus::Stopping,
            stop_reason: None,
            error_message: None,
            started_at: None,
            stopped_at: None,
            duration_seconds: None,
        }
    }

    #[test]
    fn stale_worker_cannot_write_transcript_files_after_losing_ownership() {
        let mut store = InMemoryMeetingStore::new();
        store.insert(stopping_meeting());
        let whisper = StubWhisperClient {
            mocked_response_json: r#"{
              "text":"ok",
              "segments":[{"speaker":"alice","start":0.0,"end":1.0,"text":"hello"}]
            }"#
            .to_owned(),
        };
        let client = SummaryOnlyTestClient;
        let temp = temp_workspace("m1");
        let workspace = temp.workspace.clone();
        let mut ownership_checks = 0usize;

        let result = process_meeting_summary_with_ownership_check(
            &mut store,
            &whisper,
            &client,
            &ProcessMeetingInput {
                meeting_id: "m1".to_owned(),
                job_id: Some("summary-m1".to_owned()),
                guild_id: "g1".to_owned(),
                voice_channel_id: "vc".to_owned(),
                voice_channel_name: None,
                title: None,
                started_at: None,
                stopped_at: None,
                duration_seconds: None,
                audio_path: workspace.mixdown_path().to_string_lossy().to_string(),
                speaker_audio: vec![SpeakerAudioInput {
                    speaker_id: "alice".to_owned(),
                    audio_path: "audio.wav".to_owned(),
                    offset_ms: 0,
                }],
                language: None,
                workspace: workspace.clone(),
                summary_context: SummaryContextInput::default(),
            },
            || {
                ownership_checks += 1;
                if ownership_checks == 7 {
                    Err(WorkerError::Queue("lost summary job ownership".to_owned()))
                } else {
                    Ok(())
                }
            },
        );

        assert!(result.is_err());
        assert!(workspace.pre_correction_transcript_path().is_file());
        assert!(!workspace.masked_transcript_path().exists());
        assert!(!workspace.transcript_manifest_path().exists());
    }
}
