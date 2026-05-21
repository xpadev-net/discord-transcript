use crate::application::auto_stop::{AutoStopSignal, AutoStopState};
use crate::application::bot::{BotCommandService, StartCommandInput, StopCommandInput};
use crate::application::command::{CommandError, PermissionSet, authorize_record_stop_for_meeting};
use crate::application::recovery_runner::{RecoveryEffect, run_recovery};
use crate::application::retention_cleanup::{
    apply_retention_database_cleanup, apply_retention_filesystem_cleanup,
    collect_retention_cleanup_plan,
};
use crate::application::stop::StopOutcome;
use crate::application::summary::ClaudeSummaryClient;
use crate::application::worker::enqueue_summary_job;
use crate::audio::meeting_audio::{
    build_speaker_audio_inputs, compute_meeting_start_ms, load_chunks,
};
use crate::audio::receiver::ReceiverConfig;
use crate::audio::recording_session::RecordingSession;
use crate::audio::songbird_adapter::{AdaptedVoiceFrames, SsrcTracker, adapt_voice_tick};
use crate::bootstrap::config::{AppConfig, SummaryHarness};
use crate::domain::authz::UserRole;
use crate::domain::recovery::RecoveryCandidate;
use crate::domain::speaker::SpeakerProfile;
use crate::domain::transcript::{
    MAX_DB_TIMESTAMP_MS, NormalizationConfig, TranscriptSegment, TranscriptSource,
    normalize_segments, render_for_summary,
};
use crate::domain::{MeetingStatus, StopReason};
use crate::infrastructure::integrations::{
    CommandWhisperClient, DEFAULT_COMMAND_TIMEOUT, HarnessCliSummaryClient,
};
use crate::infrastructure::queue::{Job, JobQueue};
use crate::infrastructure::retry::RetryPolicy;
use crate::infrastructure::sql::{
    INCREMENTAL_MIGRATIONS_SQL, INITIAL_SCHEMA_SQL, RECOVERY_REQUEUE_STALE_RUNNING_SUMMARY_JOB_SQL,
    RECOVERY_SCAN_SQL, RECOVERY_SUMMARY_JOB_STATUS_SQL,
};
use crate::infrastructure::sql_store::{PgSqlExecutor, SqlExecutor, SqlJobQueue, SqlMeetingStore};
use crate::infrastructure::storage::{MeetingStore, StatusMessageMetadata, StoredMeeting};
use crate::infrastructure::storage_fs::{ChunkStorage, LocalChunkStorage};
use crate::interfaces::posting::{DISCORD_MESSAGE_LIMIT, split_discord_message};
use crate::interfaces::vc_text::{fetch_vc_text_messages, warn_and_fallback_on_vc_text_error};
use serenity::all::{
    ChannelId, CommandDataOptionValue, CommandInteraction, CreateCommand,
    CreateInteractionResponse, CreateInteractionResponseMessage, EditInteractionResponse,
    EditMessage, GatewayIntents, GuildId, Interaction, Member, Ready, UserId, VoiceState,
};
use serenity::async_trait;
use serenity::http::Http;
use serenity::prelude::{Client, Context, EventHandler};
use songbird::driver::{DecodeConfig, DecodeMode};
use songbird::{
    Config as SongbirdConfig, CoreEvent, Event, EventContext, EventHandler as SongbirdEventHandler,
    SerenityInit,
};
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::fs;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

pub const RECORD_START_COMMAND: &str = "record-start";
pub const RECORD_STOP_COMMAND: &str = "record-stop";
const AUTO_STOP_FINAL_FLUSH_MAX_RETRIES: u32 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommandSpec {
    pub name: &'static str,
    pub description: &'static str,
}

pub fn slash_command_specs() -> Vec<SlashCommandSpec> {
    vec![
        SlashCommandSpec {
            name: RECORD_START_COMMAND,
            description: "Start recording in your current voice channel",
        },
        SlashCommandSpec {
            name: RECORD_STOP_COMMAND,
            description: "Stop the active recording in this guild",
        },
    ]
}

pub fn create_serenity_commands() -> Vec<CreateCommand> {
    slash_command_specs()
        .into_iter()
        .map(|spec| match spec.name {
            RECORD_START_COMMAND => CreateCommand::new(spec.name).description(spec.description),
            RECORD_STOP_COMMAND => CreateCommand::new(spec.name).description(spec.description),
            _ => CreateCommand::new(spec.name).description(spec.description),
        })
        .collect()
}

pub fn validate_command_guild(
    command_guild_id: Option<GuildId>,
    configured_guild_id: GuildId,
) -> Result<GuildId, String> {
    let guild_id =
        command_guild_id.ok_or_else(|| "guild_id is required for this command".to_owned())?;
    if guild_id != configured_guild_id {
        return Err("command is not configured for this guild".to_owned());
    }
    Ok(guild_id)
}

pub async fn run_guild_scoped_command<F, Fut>(
    command_guild_id: Option<GuildId>,
    configured_guild_id: GuildId,
    command_work: F,
) -> String
where
    F: FnOnce(GuildId) -> Fut,
    Fut: Future<Output = Result<String, String>>,
{
    let result = match validate_command_guild(command_guild_id, configured_guild_id) {
        Ok(guild_id) => command_work(guild_id).await,
        Err(err) => Err(err),
    };
    match result {
        Ok(message) => message,
        Err(err) => format!("error: {err}"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeCommandInput {
    RecordStart(StartCommandInput),
    RecordStop {
        guild_id: String,
        user_id: String,
        caller_role: UserRole,
        reason: StopReason,
    },
}

pub fn dispatch_runtime_command<S: MeetingStore>(
    service: &mut BotCommandService<S>,
    input: RuntimeCommandInput,
) -> Result<String, CommandError> {
    match input {
        RuntimeCommandInput::RecordStart(value) => service.handle_record_start(value),
        RuntimeCommandInput::RecordStop {
            guild_id,
            user_id,
            caller_role,
            reason,
        } => service.handle_record_stop(StopCommandInput {
            guild_id,
            user_id,
            caller_role,
            reason,
        }),
    }
}

fn complete_record_start_after_runtime_setup<S>(
    service: &mut BotCommandService<S>,
    input: StartCommandInput,
) -> Result<String, String>
where
    S: MeetingStore,
{
    service
        .handle_record_start(input)
        .map_err(|err| err.to_string())
}

pub fn stop_and_enqueue_summary_job<S, Q>(
    service: &mut BotCommandService<S>,
    queue: &mut Q,
    guild_id: &str,
    user_id: &str,
    caller_role: UserRole,
    expected_meeting_id: Option<&str>,
    reason: StopReason,
) -> Result<crate::application::bot::StopCommandResult, String>
where
    S: MeetingStore,
    Q: crate::infrastructure::queue::JobQueue,
{
    if let Some(expected_meeting_id) = expected_meeting_id {
        let active = service
            .store
            .find_active_meeting_by_guild(guild_id)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| CommandError::NoActiveMeeting.to_string())?;
        if active.id != expected_meeting_id {
            return Err(format!(
                "active meeting changed before stop: expected={expected_meeting_id}, actual={}",
                active.id
            ));
        }
    }

    let stop_result = service
        .handle_record_stop_result(StopCommandInput {
            guild_id: guild_id.to_owned(),
            user_id: user_id.to_owned(),
            caller_role,
            reason,
        })
        .map_err(|err| err.to_string())?;

    let should_enqueue = match stop_result.outcome {
        StopOutcome::Owner => true,
        StopOutcome::AlreadyHandled => service
            .store
            .get_meeting(&stop_result.meeting_id)
            .map_err(|err| err.to_string())?
            .is_some_and(|meeting| meeting.status == MeetingStatus::Stopping),
    };

    if should_enqueue {
        let job_id = format!("summary-{}", stop_result.meeting_id);
        match enqueue_summary_job(queue, &job_id, &stop_result.meeting_id) {
            Ok(()) => {
                info!(
                    meeting_id = %stop_result.meeting_id,
                    job_id = %job_id,
                    "summary job enqueued after stop"
                );
            }
            Err(crate::application::worker::WorkerError::AlreadyExists) => {
                debug!(
                    meeting_id = %stop_result.meeting_id,
                    job_id = %job_id,
                    "summary job already exists after stop"
                );
            }
            Err(err) => return Err(err.to_string()),
        }
    }

    Ok(stop_result)
}

pub fn meeting_audio_dir(
    base_dir: &str,
    guild_id: &str,
    voice_channel_id: &str,
    meeting_id: &str,
) -> PathBuf {
    crate::infrastructure::workspace::MeetingWorkspaceLayout::new(base_dir)
        .for_meeting(guild_id, voice_channel_id, meeting_id)
        .audio_dir()
}

pub fn meeting_audio_path(
    base_dir: &str,
    guild_id: &str,
    voice_channel_id: &str,
    meeting_id: &str,
) -> String {
    crate::infrastructure::workspace::MeetingWorkspaceLayout::new(base_dir)
        .for_meeting(guild_id, voice_channel_id, meeting_id)
        .mixdown_path()
        .to_string_lossy()
        .to_string()
}

fn flush_session_for_teardown<S: ChunkStorage>(
    session: &mut RecordingSession<S>,
    guild_id: &str,
    phase: &str,
) -> Result<(), String> {
    match session.flush_all() {
        Ok(result) if result.failed.is_empty() => Ok(()),
        Ok(result) => {
            warn!(
                guild_id = %guild_id,
                failed = result.failed.len(),
                phase,
                "some chunks failed to persist during final flush; retaining session for retry"
            );
            Err(format!(
                "failed to persist {} final audio chunk(s)",
                result.failed.len()
            ))
        }
        Err(err) => {
            // Recorder errors leave in-flight recorder buffers undrained. Treat
            // them as retryable during teardown so we do not discard audio that
            // was never moved into the session's pending failed chunk buffer.
            warn!(guild_id = %guild_id, error = %err, phase, "failed to flush final audio; retaining session for retry");
            Err(err.to_string())
        }
    }
}

fn summary_retry_exhausted(
    retry_status: Result<crate::domain::JobStatus, crate::infrastructure::queue::QueueError>,
    meeting_id: &str,
    job_id: &str,
    phase: &str,
) -> bool {
    match retry_status {
        Ok(crate::domain::JobStatus::Failed) => true,
        Ok(_) => false,
        Err(err @ crate::infrastructure::queue::QueueError::Backend(_)) => {
            warn!(
                meeting_id,
                job_id,
                phase,
                error = %err,
                "failed to update summary job retry state; leaving meeting status unchanged"
            );
            false
        }
        Err(err) => {
            warn!(
                meeting_id,
                job_id,
                phase,
                error = %err,
                "summary job retry cannot be durably scheduled"
            );
            true
        }
    }
}

fn retry_claimed_summary_job<Q: JobQueue>(
    queue: &mut Q,
    job: &Job,
    error_message: String,
    max_retries: u32,
    phase: &str,
) -> bool {
    let retry_status = queue.retry(&job.id, error_message, max_retries);
    summary_retry_exhausted(retry_status, &job.meeting_id, &job.id, phase)
}

fn db_safe_transcript_timestamp_ms(
    segment_index: usize,
    field: &str,
    value: u64,
) -> Result<i32, String> {
    if value > MAX_DB_TIMESTAMP_MS {
        return Err(format!(
            "transcript segment {segment_index} {field} timestamp {value}ms exceeds database integer range"
        ));
    }
    Ok(value as i32)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TranscriptPersistError {
    Validation(String),
    Database(String),
}

impl std::fmt::Display for TranscriptPersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Validation(message) | Self::Database(message) => f.write_str(message),
        }
    }
}

fn persist_transcript_segments<E: SqlExecutor>(
    executor: &mut E,
    meeting_id: &str,
    segments: &[TranscriptSegment],
) -> Result<(), TranscriptPersistError> {
    if segments.is_empty() {
        executor
            .execute(
                "DELETE FROM transcripts WHERE meeting_id=$1",
                &[meeting_id.to_owned()],
            )
            .map(|_| ())
            .map_err(|err| {
                TranscriptPersistError::Database(format!(
                    "failed to clear old transcript segments: {err}"
                ))
            })?;
        return Ok(());
    }

    let base_sql =
        crate::infrastructure::sql::build_insert_transcripts_sql_with_offset(segments.len(), 1);
    let sql = format!("WITH cleared AS (DELETE FROM transcripts WHERE meeting_id=$1) {base_sql}");
    let mut params = Vec::with_capacity(segments.len() * 9 + 1);
    params.push(meeting_id.to_owned());
    for (i, seg) in segments.iter().enumerate() {
        let start_ms = db_safe_transcript_timestamp_ms(i, "start_ms", seg.start_ms)
            .map_err(TranscriptPersistError::Validation)?;
        let end_ms = db_safe_transcript_timestamp_ms(i, "end_ms", seg.end_ms)
            .map_err(TranscriptPersistError::Validation)?;
        params.push(format!("{meeting_id}-t-{i}"));
        params.push(meeting_id.to_owned());
        params.push(seg.speaker_id.clone());
        params.push(start_ms.to_string());
        params.push(end_ms.to_string());
        params.push(seg.text.clone());
        params.push(seg.confidence.map(|c| c.to_string()).unwrap_or_default());
        params.push(seg.is_noisy.to_string());
        params.push(seg.source.as_str().to_owned());
    }

    executor.execute(&sql, &params).map(|_| ()).map_err(|err| {
        TranscriptPersistError::Database(format!("failed to persist transcript segments: {err}"))
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SummaryJobRunError {
    Terminal(String),
    TerminalStatusUpdated(String),
    RetryScheduled(String),
}

impl From<String> for SummaryJobRunError {
    fn from(value: String) -> Self {
        Self::Terminal(value)
    }
}

fn retry_summary_job_after_posting_failure<S, Q>(
    store: &mut S,
    queue: &mut Q,
    meeting_id: &str,
    job_id: &str,
    error_message: String,
    max_retries: u32,
) -> Result<bool, String>
where
    S: MeetingStore,
    Q: JobQueue,
{
    match queue.retry(job_id, error_message.clone(), max_retries) {
        Ok(crate::domain::JobStatus::Queued) => {
            store
                .set_meeting_status(
                    meeting_id,
                    MeetingStatus::Stopping,
                    Some(MeetingStatus::Summarizing),
                )
                .map_err(|err| {
                    format!("summary post retry queued but meeting status update failed: {err}")
                })?;
            store
                .set_error_message(meeting_id, Some(error_message))
                .map_err(|err| {
                    format!("summary post retry queued but error message update failed: {err}")
                })?;
            Ok(false)
        }
        Ok(crate::domain::JobStatus::Failed) => {
            store
                .set_meeting_status(meeting_id, MeetingStatus::Failed, None)
                .map_err(|err| {
                    format!("summary post retry exhausted but meeting failure update failed: {err}")
                })?;
            store
                .set_error_message(meeting_id, Some(error_message))
                .map_err(|err| {
                    format!("summary post retry exhausted but error message update failed: {err}")
                })?;
            Ok(true)
        }
        Ok(status) => {
            warn!(
                meeting_id,
                job_id,
                status = %status.as_str(),
                "unexpected summary post retry status; leaving meeting status unchanged"
            );
            Ok(false)
        }
        Err(err @ crate::infrastructure::queue::QueueError::Backend(_)) => {
            warn!(
                meeting_id,
                job_id,
                error = %err,
                "failed to durably retry summary post failure; leaving meeting status unchanged"
            );
            let _ = store.set_error_message(meeting_id, Some(error_message));
            Ok(false)
        }
        Err(err) => {
            warn!(
                meeting_id,
                job_id,
                error = %err,
                "summary post retry cannot be durably scheduled"
            );
            store
                .set_meeting_status(meeting_id, MeetingStatus::Failed, None)
                .map_err(|store_err| {
                    format!(
                        "summary post retry failed ({err}) and meeting failure update failed: {store_err}"
                    )
                })?;
            store
                .set_error_message(meeting_id, Some(error_message))
                .map_err(|store_err| {
                    format!(
                        "summary post retry failed ({err}) and error message update failed: {store_err}"
                    )
                })?;
            Ok(true)
        }
    }
}

fn retry_summary_job_after_transcript_persist_failure<S, Q>(
    store: &mut S,
    queue: &mut Q,
    meeting_id: &str,
    job_id: &str,
    error_message: String,
    max_retries: u32,
) -> Result<bool, String>
where
    S: MeetingStore,
    Q: JobQueue,
{
    if let Err(err) = store.set_meeting_status(
        meeting_id,
        MeetingStatus::Stopping,
        Some(MeetingStatus::Transcribing),
    ) {
        warn!(
            meeting_id,
            job_id,
            error = %err,
            "transcript persist retry cannot restore meeting to retryable state; marking job failed"
        );
        let _ = queue.mark_failed(job_id, error_message.clone());
        store
            .set_meeting_status(meeting_id, MeetingStatus::Failed, None)
            .map_err(|store_err| {
                format!(
                    "transcript persist retry status restore failed ({err}) and meeting failure update failed: {store_err}"
                )
            })?;
        store
            .set_error_message(meeting_id, Some(error_message))
            .map_err(|store_err| {
                format!(
                    "transcript persist retry status restore failed ({err}) and error message update failed: {store_err}"
                )
            })?;
        return Ok(true);
    }

    match queue.retry(job_id, error_message.clone(), max_retries) {
        Ok(crate::domain::JobStatus::Queued) => {
            if let Err(err) = store.set_error_message(meeting_id, Some(error_message)) {
                warn!(
                    meeting_id,
                    job_id,
                    error = %err,
                    "transcript persist retry queued but error message update failed"
                );
            }
            Ok(false)
        }
        Ok(crate::domain::JobStatus::Failed) => {
            store
                .set_meeting_status(meeting_id, MeetingStatus::Failed, None)
                .map_err(|err| {
                    format!(
                        "transcript persist retry exhausted but meeting failure update failed: {err}"
                    )
                })?;
            store
                .set_error_message(meeting_id, Some(error_message))
                .map_err(|err| {
                    format!(
                        "transcript persist retry exhausted but error message update failed: {err}"
                    )
                })?;
            Ok(true)
        }
        Ok(status) => {
            warn!(
                meeting_id,
                job_id,
                status = %status.as_str(),
                "unexpected transcript persist retry status; marking meeting failed"
            );
            store
                .set_meeting_status(meeting_id, MeetingStatus::Failed, None)
                .map_err(|err| {
                    format!(
                        "transcript persist retry returned unexpected status {status:?} and meeting failure update failed: {err}"
                    )
                })?;
            store
                .set_error_message(meeting_id, Some(error_message))
                .map_err(|err| {
                    format!(
                        "transcript persist retry returned unexpected status {status:?} and error message update failed: {err}"
                    )
                })?;
            Ok(true)
        }
        Err(err @ crate::infrastructure::queue::QueueError::Backend(_)) => {
            warn!(
                meeting_id,
                job_id,
                error = %err,
                "failed to durably retry transcript persist failure; leaving meeting status unchanged"
            );
            if let Err(status_err) = store.set_meeting_status(
                meeting_id,
                MeetingStatus::Transcribing,
                Some(MeetingStatus::Stopping),
            ) {
                warn!(
                    meeting_id,
                    job_id,
                    error = %status_err,
                    "failed to restore meeting status after transcript persist retry backend error"
                );
            }
            let _ = store.set_error_message(meeting_id, Some(error_message));
            Ok(false)
        }
        Err(err) => {
            warn!(
                meeting_id,
                job_id,
                error = %err,
                "transcript persist retry cannot be durably scheduled"
            );
            store
                .set_meeting_status(meeting_id, MeetingStatus::Failed, None)
                .map_err(|store_err| {
                    format!(
                        "transcript persist retry failed ({err}) and meeting failure update failed: {store_err}"
                    )
                })?;
            store
                .set_error_message(meeting_id, Some(error_message))
                .map_err(|store_err| {
                    format!(
                        "transcript persist retry failed ({err}) and error message update failed: {store_err}"
                    )
                })?;
            Ok(true)
        }
    }
}

fn recover_summary_job_for_startup<E: SqlExecutor>(
    queue: &mut SqlJobQueue<E>,
    job_id: &str,
    meeting_id: &str,
) -> bool {
    if let Err(err) = queue.executor.execute(
        RECOVERY_REQUEUE_STALE_RUNNING_SUMMARY_JOB_SQL,
        &[job_id.to_owned()],
    ) {
        warn!(meeting_id, job_id, error = %err, "failed to requeue stale running summary job during recovery");
        return false;
    }

    match enqueue_summary_job(queue, job_id, meeting_id) {
        Ok(()) => true,
        Err(crate::application::worker::WorkerError::AlreadyExists) => {
            recovery_existing_summary_job_is_claimable(queue, job_id, meeting_id)
        }
        Err(err) => {
            warn!(meeting_id, job_id, error = %err, "failed to enqueue summary job during recovery");
            false
        }
    }
}

fn recovery_existing_summary_job_is_claimable<E: SqlExecutor>(
    queue: &mut SqlJobQueue<E>,
    job_id: &str,
    meeting_id: &str,
) -> bool {
    match queue
        .executor
        .query_rows(RECOVERY_SUMMARY_JOB_STATUS_SQL, &[job_id.to_owned()])
    {
        Ok(rows) => rows
            .first()
            .and_then(|row| row.first())
            .is_some_and(|status| status == "queued"),
        Err(err) => {
            warn!(meeting_id, job_id, error = %err, "failed to inspect existing summary job during recovery");
            false
        }
    }
}

/// Place every chunk on a shared wall-clock timeline so speakers with
/// different join times (and thus independent per-user sequence numbers)
/// stay aligned in the mixdown. `meeting_start_ms` anchors t=0 of the output.
fn mix_chunks_by_wallclock(
    chunks: &[crate::audio::meeting_audio::LoadedChunk],
    sample_rate: u32,
) -> Vec<u8> {
    let meeting_start_ms = compute_meeting_start_ms(chunks);
    // Derive buffer length from actual PCM byte counts rather than
    // duration_ms so that sub-millisecond tails aren't truncated by the
    // round-trip through integer milliseconds.
    let offset_samples_for = |start_ms: u64| -> usize {
        let offset_ms = start_ms.saturating_sub(meeting_start_ms);
        ((offset_ms as u128).saturating_mul(sample_rate as u128) / 1_000u128) as usize
    };
    let total_samples = chunks
        .iter()
        .map(|c| offset_samples_for(c.start_ms) + c.pcm.len() / 2)
        .max()
        .unwrap_or(0);

    let mut mixed = vec![0i32; total_samples];
    for chunk in chunks {
        let offset_samples = offset_samples_for(chunk.start_ms);
        let chunk_samples = chunk.pcm.len() / 2;
        for i in 0..chunk_samples {
            let sample = i16::from_le_bytes([chunk.pcm[i * 2], chunk.pcm[i * 2 + 1]]) as i32;
            mixed[offset_samples + i] = mixed[offset_samples + i].saturating_add(sample);
        }
    }

    let mut out = Vec::with_capacity(mixed.len() * 2);
    for sample in &mixed {
        let clamped = (*sample).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        out.extend_from_slice(&clamped.to_le_bytes());
    }
    out
}

pub fn merge_user_chunks_to_mixdown(
    audio_dir: &std::path::Path,
    resample_to_16k: bool,
) -> Result<String, String> {
    use crate::audio::build_wav_bytes_raw;

    let mixdown_path = audio_dir.join("mixdown.wav");

    let chunks = load_chunks(audio_dir)?;
    let sample_rate = chunks.first().map(|c| c.sample_rate).unwrap_or(48_000);
    if chunks.iter().any(|c| c.sample_rate != sample_rate) {
        return Err("mixed sample rates are not supported for mixdown".to_owned());
    }

    let all_pcm = mix_chunks_by_wallclock(&chunks, sample_rate);

    let (final_pcm, final_rate) = if resample_to_16k {
        let (pcm, rate) = crate::audio::wav::resample_pcm_16le(&all_pcm, sample_rate, 16_000);
        if rate != 16_000 {
            warn!(
                sample_rate,
                "mixdown resampling skipped: unsupported sample rate (expected 48000)"
            );
        }
        (pcm, rate)
    } else {
        (all_pcm, sample_rate)
    };
    let wav_bytes = build_wav_bytes_raw(&final_pcm, final_rate, 1, 16)
        .map_err(|err| format!("failed to build mixdown WAV: {err}"))?;
    fs::write(&mixdown_path, &wav_bytes)
        .map_err(|err| format!("failed to write mixdown: {err}"))?;

    Ok(mixdown_path.to_string_lossy().to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecoverySnapshot {
    meeting_id: String,
    status: MeetingStatus,
    voice_channel_id: Option<u64>,
}

fn parse_meeting_status(value: &str) -> Result<MeetingStatus, String> {
    MeetingStatus::parse_str(value).ok_or_else(|| format!("unknown meeting status: {value}"))
}

fn parse_u64_with_warning(meeting_id: &str, field_name: &str, value: &str) -> Option<u64> {
    match value.parse::<u64>() {
        Ok(parsed) => Some(parsed),
        Err(err) => {
            warn!(
                meeting_id = %meeting_id,
                field = %field_name,
                value = %value,
                error = %err,
                "failed to parse numeric field in recovery snapshot"
            );
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    InvalidGuildId(String),
    DatabaseConnect(String),
    DatabaseMigration(String),
    ClientInit(String),
    ClientRun(String),
}

impl Display for RuntimeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidGuildId(err) => write!(f, "invalid guild id: {err}"),
            Self::DatabaseConnect(err) => write!(f, "failed to connect database: {err}"),
            Self::DatabaseMigration(err) => write!(f, "failed to run migration: {err}"),
            Self::ClientInit(err) => write!(f, "failed to initialize serenity client: {err}"),
            Self::ClientRun(err) => write!(f, "failed while running serenity client: {err}"),
        }
    }
}

#[derive(Debug, Clone)]
enum StatusMessageUpdate<'a> {
    RecordingStarted {
        voice_channel_id: u64,
        report_channel_id: u64,
    },
    RecordingStopped,
    SummaryStarted,
    SummaryCompleted {
        summary_url: Option<String>,
    },
    Failed {
        phase: &'a str,
        error: &'a str,
    },
}

struct DiscordStatusMessenger<'a> {
    http: &'a Http,
}

#[async_trait]
trait StatusMessenger {
    async fn send(&self, channel_id: u64, content: &str) -> Result<u64, String>;
    async fn edit(&self, channel_id: u64, message_id: u64, content: &str) -> Result<(), String>;
}

#[async_trait]
impl StatusMessenger for DiscordStatusMessenger<'_> {
    async fn send(&self, channel_id: u64, content: &str) -> Result<u64, String> {
        ChannelId::new(channel_id)
            .say(self.http, content)
            .await
            .map(|msg| msg.id.get())
            .map_err(|err| err.to_string())
    }

    async fn edit(&self, channel_id: u64, message_id: u64, content: &str) -> Result<(), String> {
        ChannelId::new(channel_id)
            .edit_message(self.http, message_id, EditMessage::new().content(content))
            .await
            .map(|_| ())
            .map_err(|err| err.to_string())
    }
}

fn format_status_message_content(meeting_id: &str, update: &StatusMessageUpdate<'_>) -> String {
    match update {
        StatusMessageUpdate::RecordingStarted {
            voice_channel_id,
            report_channel_id,
        } => format!(
            "🎙️ 録音を開始しました\nmeeting_id={meeting_id}\nVC: <#{}>\nレポート: <#{}>",
            voice_channel_id, report_channel_id
        ),
        StatusMessageUpdate::RecordingStopped => {
            format!("⏹️ 録音を終了しました。要約を準備しています。\nmeeting_id={meeting_id}")
        }
        StatusMessageUpdate::SummaryStarted => {
            format!("📝 要約を開始しました (文字起こし/要約を実行中)\nmeeting_id={meeting_id}")
        }
        StatusMessageUpdate::SummaryCompleted { summary_url } => {
            let base = format!("✅ 要約が完了しました\nmeeting_id={meeting_id}");
            match summary_url {
                Some(url) => format!("{base}\n要約ページ: {url}"),
                None => base,
            }
        }
        StatusMessageUpdate::Failed { phase, error } => {
            let trimmed = truncate_error_for_status(error);
            format!("⚠️ 処理に失敗しました ({phase})\nmeeting_id={meeting_id}\nerror={trimmed}")
        }
    }
}

fn truncate_error_for_status(error: &str) -> String {
    const LIMIT: usize = 1400;
    if error.len() <= LIMIT {
        return error.to_owned();
    }

    let mut end = 0usize;
    for (idx, ch) in error.char_indices() {
        let next = idx + ch.len_utf8();
        if next > LIMIT {
            break;
        }
        end = next;
    }

    if end == 0 {
        return error
            .chars()
            .next()
            .map(|c| format!("{c}…"))
            .unwrap_or_default();
    }

    let mut truncated = error[..end].to_owned();
    truncated.push('…');
    truncated
}

async fn upsert_status_message_via_messenger<M: StatusMessenger + Sync>(
    messenger: &M,
    meeting_id: &str,
    channel_id: u64,
    existing_message_id: Option<u64>,
    content: &str,
) -> Result<Option<u64>, String> {
    let mut edit_error = None;
    if let Some(message_id) = existing_message_id {
        match messenger.edit(channel_id, message_id, content).await {
            Ok(_) => return Ok(None),
            Err(err) => {
                edit_error = Some(err);
            }
        }
    }

    match messenger.send(channel_id, content).await {
        Ok(message_id) => {
            if let Some(err) = edit_error {
                warn!(
                    meeting_id = %meeting_id,
                    channel_id = channel_id,
                    error = %err,
                    "failed to edit status message, posted a new one instead"
                );
            }
            Ok(Some(message_id))
        }
        Err(err) => {
            if let Some(edit_err) = edit_error {
                Err(format!(
                    "status message update failed (edit failed: {edit_err}; send failed: {err})"
                ))
            } else {
                Err(err)
            }
        }
    }
}

impl std::error::Error for RuntimeError {}

pub async fn run_bot(config: &AppConfig) -> Result<(), RuntimeError> {
    let guild_id = config
        .discord_guild_id
        .parse::<u64>()
        .map(GuildId::new)
        .map_err(|err| RuntimeError::InvalidGuildId(err.to_string()))?;

    let base_executor =
        PgSqlExecutor::connect_with_ssl_mode(&config.database_url, &config.database_ssl_mode)
            .map_err(RuntimeError::DatabaseConnect)?;
    let mut migration_store = SqlMeetingStore::new(base_executor);
    migration_store
        .apply_initial_migration(INITIAL_SCHEMA_SQL)
        .map_err(RuntimeError::DatabaseMigration)?;
    migration_store
        .apply_initial_migration(INCREMENTAL_MIGRATIONS_SQL)
        .map_err(RuntimeError::DatabaseMigration)?;
    let base_executor = migration_store.executor;

    let handler = ScaffoldHandler {
        guild_id,
        service: Arc::new(Mutex::new(BotCommandService::new(SqlMeetingStore::new(
            base_executor,
        )))),
        queue: Arc::new(Mutex::new(SqlJobQueue::new(
            PgSqlExecutor::connect_with_ssl_mode(&config.database_url, &config.database_ssl_mode)
                .map_err(RuntimeError::DatabaseConnect)?,
        ))),
        ssrc_tracker: Arc::new(Mutex::new(SsrcTracker::new())),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        auto_stop_states: Arc::new(Mutex::new(HashMap::new())),
        chunk_storage_dir: config.chunk_storage_dir.clone(),
        auto_stop_grace_seconds: config.auto_stop_grace_seconds,
        whisper_endpoint: config.whisper_endpoint.clone(),
        summary_harness: config.summary_harness,
        summary_command: config.summary_command.clone(),
        summary_model: config.summary_model.clone(),
        whisper_language: config.whisper_language.clone(),
        whisper_beam_size: config.whisper_beam_size,
        whisper_suppress_non_speech: config.whisper_suppress_non_speech,
        whisper_prompt: config.whisper_prompt.clone(),
        whisper_vad: config.whisper_vad,
        whisper_temperature: config.whisper_temperature,
        whisper_resample_to_16k: config.whisper_resample_to_16k,
        summary_max_retries: config.summary_max_retries,
        retention_policy: config.retention_policy,
        integration_retry_policy: RetryPolicy {
            max_attempts: config.integration_retry_max_attempts,
            initial_delay: std::time::Duration::from_millis(
                config.integration_retry_initial_delay_ms,
            ),
            backoff_multiplier: config.integration_retry_backoff_multiplier,
            max_delay: std::time::Duration::from_millis(config.integration_retry_max_delay_ms),
        },
        public_base_url: config.public_base_url.clone(),
        bot_admin_user_ids: config
            .discord_bot_admin_user_ids
            .iter()
            .cloned()
            .collect::<HashSet<_>>(),
    };

    let intents = GatewayIntents::GUILDS | GatewayIntents::GUILD_VOICE_STATES;
    let songbird_config =
        SongbirdConfig::default().decode_mode(DecodeMode::Decode(DecodeConfig::default()));
    let mut client = Client::builder(&config.discord_token, intents)
        .event_handler(handler)
        .register_songbird_from_config(songbird_config)
        .await
        .map_err(|err| RuntimeError::ClientInit(err.to_string()))?;

    client
        .start()
        .await
        .map_err(|err| RuntimeError::ClientRun(err.to_string()))
}

#[derive(Clone)]
struct ScaffoldHandler {
    guild_id: GuildId,
    service: Arc<Mutex<BotCommandService<SqlMeetingStore<PgSqlExecutor>>>>,
    queue: Arc<Mutex<SqlJobQueue<PgSqlExecutor>>>,
    ssrc_tracker: Arc<Mutex<SsrcTracker>>,
    sessions: Arc<Mutex<HashMap<String, RecordingSession<LocalChunkStorage>>>>,
    auto_stop_states: Arc<Mutex<HashMap<String, AutoStopState>>>,
    chunk_storage_dir: String,
    auto_stop_grace_seconds: u64,
    whisper_endpoint: String,
    summary_harness: SummaryHarness,
    summary_command: String,
    summary_model: String,
    whisper_language: Option<String>,
    whisper_beam_size: u32,
    whisper_suppress_non_speech: bool,
    whisper_prompt: Option<String>,
    whisper_vad: bool,
    whisper_temperature: f32,
    whisper_resample_to_16k: bool,
    summary_max_retries: u32,
    retention_policy: crate::domain::retention::RetentionPolicy,
    integration_retry_policy: RetryPolicy,
    public_base_url: Option<String>,
    bot_admin_user_ids: HashSet<String>,
}

#[async_trait]
impl EventHandler for ScaffoldHandler {
    async fn ready(&self, ctx: Context, _data_about_bot: Ready) {
        if let Err(err) = self
            .guild_id
            .set_commands(&ctx.http, create_serenity_commands())
            .await
            .map(|_| ())
        {
            error!(error = %err, "failed to register guild commands");
        }

        let retention_handler = self.clone();
        tokio::spawn(async move {
            if let Err(err) = retention_handler.run_startup_retention_cleanup().await {
                error!(error = %err, "startup retention cleanup failed");
            }
        });

        let recovery_handler = self.clone();
        let recovery_ctx = ctx.clone();
        tokio::spawn(async move {
            if let Err(err) = recovery_handler.run_startup_recovery(&recovery_ctx).await {
                error!(error = %err, "startup recovery failed");
            }
        });
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::Command(command) = interaction {
            // Acknowledge immediately to avoid Discord's 3-second timeout
            let guild_error = validate_command_guild(command.guild_id, self.guild_id).err();
            if let Err(err) = command
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Defer(
                        CreateInteractionResponseMessage::new().ephemeral(true),
                    ),
                )
                .await
            {
                error!(error = %err, "failed to defer interaction response");
                return;
            }

            let message = match guild_error {
                Some(err) => format!("error: {err}"),
                None => self.handle_command(&ctx, &command).await,
            };

            let mut delay = Duration::from_millis(200);
            let mut last_err = None;
            for attempt in 1..=3u32 {
                match command
                    .edit_response(&ctx.http, EditInteractionResponse::new().content(&message))
                    .await
                {
                    Ok(_) => {
                        last_err = None;
                        break;
                    }
                    Err(err) => {
                        error!(attempt, error = %err, "failed to edit interaction response");
                        last_err = Some(err);
                        if attempt < 3 {
                            sleep(delay).await;
                            delay *= 2;
                        }
                    }
                }
            }
            if let Some(err) = last_err {
                error!(error = %err, "all retries exhausted for edit interaction response");
            }
        }
    }

    async fn voice_state_update(&self, ctx: Context, _old: Option<VoiceState>, _new: VoiceState) {
        if _new.guild_id != Some(self.guild_id) {
            return;
        }
        let guild_key = self.guild_id.get().to_string();
        let Some(target_voice_channel_id) = self.active_meeting_voice_channel_id().await else {
            let mut states = self.auto_stop_states.lock().await;
            states.remove(&guild_key);
            return;
        };
        let Some(non_bot) =
            count_non_bot_members_in_target_voice(&ctx, self.guild_id, target_voice_channel_id)
        else {
            warn!(
                guild_id = %self.guild_id,
                target_voice_channel_id,
                "voice state cache unavailable; skipping auto-stop evaluation to avoid false trigger"
            );
            return;
        };
        let grace = Duration::from_secs(self.auto_stop_grace_seconds);
        let (signal, timer_generation) = {
            let mut states = self.auto_stop_states.lock().await;
            let state = states
                .entry(guild_key.clone())
                .or_insert_with(|| AutoStopState::new(grace));
            let signal = state.on_non_bot_member_count_changed(non_bot, now_ms());
            (signal, state.timer_generation())
        };

        if signal == AutoStopSignal::StartTimer {
            // timer_active was already set atomically inside
            // on_non_bot_member_count_changed — no separate reservation needed.
            let handler = self.clone();
            let ctx_for_task = ctx.clone();
            let guild_for_task = guild_key;
            let expected_meeting_id = self.active_meeting_id().await;
            let grace_for_task = grace;
            let target_channel_for_task = target_voice_channel_id;
            tokio::spawn(async move {
                let mut final_flush_failures = 0u32;
                loop {
                    sleep(grace_for_task).await;
                    // Verify the same meeting is still active (not a new recording)
                    let current_meeting_id = handler.active_meeting_id().await;
                    if current_meeting_id != expected_meeting_id || expected_meeting_id.is_none() {
                        // Clear timer flag before returning.
                        let mut states = handler.auto_stop_states.lock().await;
                        if let Some(state) = states.get_mut(&guild_for_task) {
                            state.clear_timer_active_for_generation(timer_generation);
                        }
                        return;
                    }
                    // Re-verify the voice channel state at fire time. A prior cache-miss
                    // in voice_state_update may have skipped cancelling this timer even
                    // after members rejoined, so we must not rely solely on the state
                    // machine's stale empty_since_ms here.
                    match count_non_bot_members_in_target_voice(
                        &ctx_for_task,
                        handler.guild_id,
                        target_channel_for_task,
                    ) {
                        None => {
                            warn!(
                                    guild_id = %handler.guild_id,
                                    target_voice_channel_id = target_channel_for_task,
                                    "voice state cache unavailable at auto-stop grace expiry; skipping stop"
                            );
                            let mut states = handler.auto_stop_states.lock().await;
                            if let Some(state) = states.get_mut(&guild_for_task) {
                                state.clear_timer_active_for_generation(timer_generation);
                            }
                            return;
                        }
                        Some(n) if n > 0 => {
                            debug!(
                                guild_id = %handler.guild_id,
                                target_voice_channel_id = target_channel_for_task,
                                non_bot = n,
                                "members rejoined during grace period; cancelling auto-stop"
                            );
                            let mut states = handler.auto_stop_states.lock().await;
                            if let Some(state) = states.get_mut(&guild_for_task) {
                                let _ = state.on_non_bot_member_count_changed(n, now_ms());
                            }
                            return;
                        }
                        Some(_) => {}
                    }
                    let trigger = {
                        let mut states = handler.auto_stop_states.lock().await;
                        let Some(state) = states.get_mut(&guild_for_task) else {
                            return;
                        };
                        state.tick(now_ms()) == AutoStopSignal::Trigger
                    };
                    if !trigger {
                        let mut states = handler.auto_stop_states.lock().await;
                        if let Some(state) = states.get_mut(&guild_for_task) {
                            state.clear_timer_active_for_generation(timer_generation);
                        }
                        return;
                    }
                    // Flush remaining audio before stopping. Failed chunks stay
                    // attached to the session and can be retried by a later stop.
                    let flush_failed = {
                        let mut sessions = handler.sessions.lock().await;
                        if let Some(session) = sessions.get_mut(&guild_for_task)
                            && flush_session_for_teardown(session, &guild_for_task, "auto-stop")
                                .is_err()
                        {
                            true
                        } else {
                            false
                        }
                    };
                    if flush_failed {
                        final_flush_failures += 1;
                        let retry_limit_reached = {
                            let mut states = handler.auto_stop_states.lock().await;
                            let Some(state) = states.get_mut(&guild_for_task) else {
                                return;
                            };
                            if final_flush_failures >= AUTO_STOP_FINAL_FLUSH_MAX_RETRIES {
                                warn!(
                                    guild_id = %guild_for_task,
                                    attempts = final_flush_failures,
                                    "auto-stop final flush retry limit reached; retaining recording session for manual stop retry"
                                );
                                state.clear_timer_active_for_generation(timer_generation);
                                true
                            } else {
                                state.retry_after_failed_stop(now_ms());
                                false
                            }
                        };
                        if retry_limit_reached {
                            if let Some(meeting_id) = expected_meeting_id.as_deref()
                                && let Err(err) = handler
                                    .update_status_message(
                                        &ctx_for_task.http,
                                        meeting_id,
                                        StatusMessageUpdate::Failed {
                                            phase: "Recording persist",
                                            error: "final audio flush kept failing; recording session is retained for manual stop retry",
                                        },
                                    )
                                    .await
                            {
                                warn!(
                                    guild_id = %guild_for_task,
                                    meeting_id,
                                    error = %err,
                                    "failed to notify final flush retry exhaustion"
                                );
                            }
                            return;
                        }
                        continue;
                    }
                    let removed_session = {
                        let mut sessions = handler.sessions.lock().await;
                        match (
                            expected_meeting_id.as_deref(),
                            sessions
                                .get(&guild_for_task)
                                .map(|session| session.meeting_id.as_str()),
                        ) {
                            (Some(expected), Some(current)) if expected == current => {
                                sessions.remove(&guild_for_task)
                            }
                            _ => None,
                        }
                    };
                    {
                        let mut states = handler.auto_stop_states.lock().await;
                        states.remove(&guild_for_task);
                    }
                    if let Some(manager) = songbird::get(&ctx_for_task).await {
                        let _ = manager.leave(handler.guild_id).await;
                    }
                    if let Some(session) = &removed_session {
                        let tracker = handler.ssrc_tracker.lock().await;
                        session.persist_ssrc_mapping(&tracker);
                    }
                    let stop_result = {
                        let mut service = handler.service.lock().await;
                        let mut queue = handler.queue.lock().await;
                        stop_and_enqueue_summary_job(
                            &mut service,
                            &mut *queue,
                            &guild_for_task,
                            "auto-stop",
                            UserRole::BotAdmin,
                            None,
                            StopReason::AutoEmpty,
                        )
                    };
                    match stop_result {
                        Ok(result) => {
                            if result.outcome == StopOutcome::Owner
                                && let Err(err) = handler
                                    .update_status_message(
                                        &ctx_for_task.http,
                                        &result.meeting_id,
                                        StatusMessageUpdate::RecordingStopped,
                                    )
                                    .await
                            {
                                warn!(
                                    guild_id = %guild_for_task,
                                    meeting_id = %result.meeting_id,
                                    error = %err,
                                    "failed to update status message after auto stop"
                                );
                            }
                            info!(
                                guild_id = %guild_for_task,
                                meeting_id = %result.meeting_id,
                                "auto stop triggered due to empty voice channel"
                            );
                            if result.outcome == StopOutcome::Owner
                                && let Err(err) = run_summary_background(
                                    &handler,
                                    &ctx_for_task.http,
                                    &result.meeting_id,
                                )
                                .await
                            {
                                warn!(
                                    guild_id = %guild_for_task,
                                    meeting_id = %result.meeting_id,
                                    error = %err,
                                    "failed to process summary after auto stop"
                                );
                            }
                        }
                        Err(err) => {
                            warn!(
                                guild_id = %guild_for_task,
                                error = %err,
                                "auto stop failed"
                            );
                        }
                    }
                    return;
                }
            });
        }
    }
}

impl ScaffoldHandler {
    async fn run_startup_retention_cleanup(&self) -> Result<(), String> {
        let policy = self.retention_policy;
        let plan = {
            let mut service = self.service.lock().await;
            collect_retention_cleanup_plan(&mut service.store.executor, policy)
        };
        if !plan.errors.is_empty() {
            warn!(
                errors = %plan.errors.join("; "),
                "retention cleanup plan collection had errors; continuing with partial plan"
            );
        };
        let chunk_storage_dir = self.chunk_storage_dir.clone();
        let filesystem_result = tokio::task::spawn_blocking(move || {
            let layout =
                crate::infrastructure::workspace::MeetingWorkspaceLayout::new(&chunk_storage_dir);
            apply_retention_filesystem_cleanup(&layout, &plan)
        })
        .await
        .map_err(|err| format!("retention filesystem cleanup task failed: {err}"))?;
        let mut report = match filesystem_result {
            Ok(report) => report,
            Err(err) => {
                let report = err.report;
                warn!(
                    error = %err.message,
                    "retention filesystem cleanup failed; continuing with database cleanup"
                );
                report
            }
        };
        let database_result = {
            let mut service = self.service.lock().await;
            apply_retention_database_cleanup(&mut service.store.executor, policy)
        };
        let database_error = match database_result {
            Ok(database_report) => {
                report.merge(database_report);
                None
            }
            Err(err) => {
                report.merge(err.report);
                Some(err.message)
            }
        };
        if let Some(err) = database_error {
            warn!(
                raw_workspaces_scanned = report.raw_workspaces_scanned,
                raw_audio_dirs_removed = report.raw_audio_dirs_removed,
                legacy_meetings_cleaned = report.legacy_meetings_cleaned,
                speaker_dirs_removed = report.speaker_dirs_removed,
                context_dirs_removed = report.context_dirs_removed,
                transcript_dirs_removed = report.transcript_dirs_removed,
                empty_summary_dirs_removed = report.empty_summary_dirs_removed,
                summary_dirs_removed = report.summary_dirs_removed,
                debug_dirs_removed = report.debug_dirs_removed,
                transcripts_marked_deleted = report.transcripts_marked_deleted,
                summaries_deleted = report.summaries_deleted,
                artifacts_deleted = report.artifacts_deleted,
                error = %err,
                "startup retention cleanup failed after partial work"
            );
            Err(format!("retention database cleanup failed: {err}"))
        } else {
            info!(
                raw_workspaces_scanned = report.raw_workspaces_scanned,
                raw_audio_dirs_removed = report.raw_audio_dirs_removed,
                legacy_meetings_cleaned = report.legacy_meetings_cleaned,
                speaker_dirs_removed = report.speaker_dirs_removed,
                context_dirs_removed = report.context_dirs_removed,
                transcript_dirs_removed = report.transcript_dirs_removed,
                empty_summary_dirs_removed = report.empty_summary_dirs_removed,
                summary_dirs_removed = report.summary_dirs_removed,
                debug_dirs_removed = report.debug_dirs_removed,
                transcripts_marked_deleted = report.transcripts_marked_deleted,
                summaries_deleted = report.summaries_deleted,
                artifacts_deleted = report.artifacts_deleted,
                "startup retention cleanup completed"
            );
            Ok(())
        }
    }

    async fn run_startup_recovery(&self, ctx: &Context) -> Result<(), String> {
        let snapshots: Vec<RecoverySnapshot> = {
            let mut service = self.service.lock().await;
            let rows = service.store.executor.query_rows(RECOVERY_SCAN_SQL, &[])?;
            rows.into_iter()
                .map(|row| {
                    if row.len() < 3 {
                        return Err(format!("invalid recovery row length: {}", row.len()));
                    }
                    Ok(RecoverySnapshot {
                        meeting_id: row[0].clone(),
                        status: parse_meeting_status(&row[1])?,
                        voice_channel_id: parse_u64_with_warning(
                            &row[0],
                            "voice_channel_id",
                            &row[2],
                        ),
                    })
                })
                .collect::<Result<Vec<_>, String>>()?
        };

        for snapshot in snapshots {
            let meeting = self.load_meeting(&snapshot.meeting_id).await.ok();
            let workspace = meeting.as_ref().map(|m| self.workspace_for_meeting(m));
            let audio_dir = workspace
                .as_ref()
                .map(|w| w.audio_dir())
                .filter(|dir| dir.is_dir())
                .unwrap_or_else(|| {
                    crate::infrastructure::workspace::MeetingWorkspaceLayout::new(
                        &self.chunk_storage_dir,
                    )
                    .legacy_meeting_dir(&snapshot.meeting_id)
                });
            let has_recording_file = audio_dir.is_dir()
                && fs::read_dir(&audio_dir)
                    .map(|entries| {
                        entries.filter_map(Result::ok).any(|e| {
                            e.path().extension().and_then(|ext| ext.to_str()) == Some("wav")
                        })
                    })
                    .unwrap_or(false);
            let voice_connected = snapshot
                .voice_channel_id
                .and_then(|voice_channel_id| {
                    is_bot_connected_to_voice_channel(ctx, self.guild_id, voice_channel_id)
                })
                .unwrap_or(false);
            let candidate = RecoveryCandidate {
                meeting_id: snapshot.meeting_id.clone(),
                status: snapshot.status,
                voice_connected,
                has_recording_file,
            };

            let effect = {
                let mut service = self.service.lock().await;
                match run_recovery(&mut service.store, &candidate) {
                    Ok(e) => e,
                    Err(err) => {
                        warn!(
                            meeting_id = %snapshot.meeting_id,
                            error = %err,
                            "run_recovery failed for meeting, skipping to next"
                        );
                        continue;
                    }
                }
            };

            match effect {
                RecoveryEffect::SummaryRequeued { .. }
                | RecoveryEffect::StopConfirmedClientDisconnect { .. } => {
                    let job_id = format!("summary-{}", snapshot.meeting_id);
                    let job_available = {
                        let mut queue = self.queue.lock().await;
                        recover_summary_job_for_startup(&mut queue, &job_id, &snapshot.meeting_id)
                    };
                    if !job_available {
                        // No claimable job — skip run_summary_and_notify for this meeting.
                        // Recovery will be retried on the next restart.
                        continue;
                    }
                    if let Err(err) = self
                        .run_summary_and_notify(&ctx.http, &snapshot.meeting_id)
                        .await
                    {
                        warn!(
                            meeting_id = %snapshot.meeting_id,
                            error = %err,
                            "failed to process summary during startup recovery"
                        );
                    }
                }
                RecoveryEffect::MarkedFailed { meeting_id } => {
                    if let Err(err) = self
                        .post_failure_for_meeting(
                            &ctx.http,
                            &meeting_id,
                            "録音ファイルが見つからず復旧に失敗しました。meeting を failed として処理しました。",
                        )
                        .await
                    {
                        warn!(
                            meeting_id = %meeting_id,
                            error = %err,
                            "failed to post recovery failure notification"
                        );
                    }
                }
                RecoveryEffect::Noop { .. } => {}
            }
        }
        Ok(())
    }

    async fn active_meeting_voice_channel_id(&self) -> Option<u64> {
        let mut service = self.service.lock().await;
        service
            .store
            .find_active_meeting_by_guild(&self.guild_id.get().to_string())
            .ok()
            .flatten()
            .and_then(|meeting| meeting.voice_channel_id.parse::<u64>().ok())
    }

    async fn active_meeting_id(&self) -> Option<String> {
        let mut service = self.service.lock().await;
        service
            .store
            .find_active_meeting_by_guild(&self.guild_id.get().to_string())
            .ok()
            .flatten()
            .map(|m| m.id)
    }

    async fn status_message_metadata(
        &self,
        meeting_id: &str,
    ) -> Result<StatusMessageMetadata, String> {
        let mut service = self.service.lock().await;
        service
            .store
            .get_status_message_metadata(meeting_id)
            .map_err(|err| err.to_string())
    }

    async fn update_status_message(
        &self,
        http: &Http,
        meeting_id: &str,
        update: StatusMessageUpdate<'_>,
    ) -> Result<(), String> {
        let messenger = DiscordStatusMessenger { http };
        self.update_status_message_with_messenger(&messenger, meeting_id, update)
            .await
    }

    async fn update_status_message_with_messenger<M: StatusMessenger + Sync>(
        &self,
        messenger: &M,
        meeting_id: &str,
        update: StatusMessageUpdate<'_>,
    ) -> Result<(), String> {
        let metadata = self.status_message_metadata(meeting_id).await?;
        let channel_id_str = metadata
            .status_message_channel_id
            .as_deref()
            .unwrap_or(&metadata.report_channel_id);
        let channel_id = channel_id_str.parse::<u64>().map_err(|err| {
            format!(
                "invalid status message channel id: meeting_id={meeting_id}, value={channel_id_str}, error={err}"
            )
        })?;
        let content = format_status_message_content(meeting_id, &update);

        let existing_message_id = match metadata.status_message_id {
            Some(ref message_id_str) => match message_id_str.parse::<u64>() {
                Ok(message_id) => Some(message_id),
                Err(err) => {
                    warn!(
                        meeting_id = %meeting_id,
                        message_id = message_id_str,
                        error = %err,
                        "invalid status message id, recreating status message"
                    );
                    None
                }
            },
            None => None,
        };

        let message_id = upsert_status_message_via_messenger(
            messenger,
            meeting_id,
            channel_id,
            existing_message_id,
            &content,
        )
        .await?;

        if let Some(message_id) = message_id {
            let mut service = self.service.lock().await;
            service
                .store
                .set_status_message(meeting_id, channel_id.to_string(), message_id.to_string())
                .map_err(|err| err.to_string())?;
        }
        Ok(())
    }

    async fn handle_command(&self, ctx: &Context, command: &CommandInteraction) -> String {
        run_guild_scoped_command(command.guild_id, self.guild_id, |_| async {
            match command.data.name.as_str() {
                RECORD_START_COMMAND => self.handle_record_start(ctx, command).await,
                RECORD_STOP_COMMAND => self.handle_record_stop(ctx, command).await,
                _ => Err("unsupported command".to_owned()),
            }
        })
        .await
    }

    async fn handle_record_start(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
    ) -> Result<String, String> {
        let guild_id = validate_command_guild(command.guild_id, self.guild_id)?;
        let voice_channel_id_u64 = resolve_user_voice_channel_id(ctx, guild_id, command.user.id);

        let meeting_id = format!("{}-{}", guild_id.get(), command.id.get());
        let permissions = resolve_bot_permissions(
            ctx,
            guild_id,
            voice_channel_id_u64,
            Some(command.channel_id.get()),
        );
        let caller_role = resolve_command_user_role(
            ctx,
            guild_id,
            command.user.id,
            command.member.as_deref(),
            &self.bot_admin_user_ids,
        );
        let voice_channel_id_u64 =
            voice_channel_id_u64.ok_or_else(|| CommandError::UserNotInVoice.to_string())?;

        let manager = songbird::get(ctx)
            .await
            .ok_or_else(|| "songbird not initialized".to_owned())?;
        let layout =
            crate::infrastructure::workspace::MeetingWorkspaceLayout::new(&self.chunk_storage_dir);
        let workspace = layout.for_meeting(
            &guild_id.get().to_string(),
            &voice_channel_id_u64.to_string(),
            &meeting_id,
        );
        workspace
            .ensure_base_dirs()
            .map_err(|err| format!("failed to prepare workspace: {err}"))?;

        let mut service = self.service.lock().await;
        let response = complete_record_start_after_runtime_setup(
            &mut service,
            StartCommandInput {
                meeting_id: meeting_id.clone(),
                guild_id: guild_id.get().to_string(),
                user_id: command.user.id.get().to_string(),
                command_channel_id: command.channel_id.get().to_string(),
                user_voice_channel_id: Some(voice_channel_id_u64.to_string()),
                permissions,
                caller_role,
            },
        )?;
        drop(service);

        // Reset SSRC tracker so stale mappings from previous recordings
        // cannot mis-attribute audio when Discord reuses an SSRC value.
        {
            let mut tracker = self.ssrc_tracker.lock().await;
            *tracker = SsrcTracker::new();
        }
        // Insert session BEFORE joining VC so voice events aren't dropped
        {
            let mut sessions = self.sessions.lock().await;
            sessions.insert(
                guild_id.get().to_string(),
                RecordingSession::new(
                    meeting_id.clone(),
                    LocalChunkStorage::new(workspace.clone(), meeting_id.clone()),
                    ReceiverConfig::default(),
                    48_000,
                ),
            );
        }

        let call_lock = {
            let channel_id = ChannelId::new(voice_channel_id_u64);
            let mut join_delay = Duration::from_millis(500);
            let mut last_err = None;
            let mut result = None;
            for attempt in 1..=3u32 {
                match manager.join(guild_id, channel_id).await {
                    Ok(call) => {
                        result = Some(call);
                        break;
                    }
                    Err(err) => {
                        warn!(
                            attempt,
                            guild_id = %guild_id.get(),
                            meeting_id = %meeting_id,
                            error = %err,
                            error_debug = ?err,
                            "voice join attempt failed"
                        );
                        last_err = Some(err);
                        // Clean up partial gateway state before retrying
                        if let Err(leave_err) = manager.leave(guild_id).await {
                            warn!(
                                attempt,
                                guild_id = %guild_id.get(),
                                meeting_id = %meeting_id,
                                error = %leave_err,
                                "failed to leave voice channel during retry cleanup"
                            );
                        }
                        if attempt < 3 {
                            sleep(join_delay).await;
                            join_delay *= 2;
                        }
                    }
                }
            }
            match result {
                Some(call) => call,
                None => {
                    let err = last_err.expect("last_err must be set when all attempts fail");
                    let err_msg = format!("{err}");
                    error!(
                        guild_id = %guild_id.get(),
                        meeting_id = %meeting_id,
                        error = %err,
                        error_debug = ?err,
                        "failed to join voice channel after 3 attempts"
                    );
                    let mut sessions = self.sessions.lock().await;
                    sessions.remove(&guild_id.get().to_string());
                    drop(sessions);
                    // manager.leave() already called in the retry loop above
                    let mut service = self.service.lock().await;
                    if let Err(e) =
                        service
                            .store
                            .set_meeting_status(&meeting_id, MeetingStatus::Failed, None)
                    {
                        error!(
                            meeting_id = %meeting_id,
                            error = %e,
                            "failed to mark meeting as failed in database"
                        );
                    }
                    if let Err(e) = service
                        .store
                        .set_error_message(&meeting_id, Some(err_msg.clone()))
                    {
                        error!(
                            meeting_id = %meeting_id,
                            error = %e,
                            "failed to persist error message in database"
                        );
                    }
                    return Err(err_msg);
                }
            }
        };
        {
            let mut call = call_lock.lock().await;
            let voice_handler = VoiceReceiveHandler {
                tracker: Arc::clone(&self.ssrc_tracker),
                sessions: Arc::clone(&self.sessions),
                guild_id: guild_id.get().to_string(),
                runtime: self.clone(),
                http: Arc::clone(&ctx.http),
                ctx: ctx.clone(),
            };
            call.add_global_event(
                Event::Core(CoreEvent::SpeakingStateUpdate),
                voice_handler.clone(),
            );
            call.add_global_event(Event::Core(CoreEvent::VoiceTick), voice_handler.clone());
            call.add_global_event(Event::Core(CoreEvent::DriverDisconnect), voice_handler);
        }

        info!(
            guild_id = %guild_id.get(),
            meeting_id = %meeting_id,
            "recording started"
        );

        let status_update = self
            .update_status_message(
                &ctx.http,
                &meeting_id,
                StatusMessageUpdate::RecordingStarted {
                    voice_channel_id: voice_channel_id_u64,
                    report_channel_id: command.channel_id.get(),
                },
            )
            .await;
        if let Err(err) = status_update {
            warn!(
                guild_id = %guild_id.get(),
                meeting_id = %meeting_id,
                error = %err,
                "failed to post or update status message after record start"
            );
            Ok(format!(
                "{response}\n(ステータスメッセージ更新に失敗しました: {err})"
            ))
        } else {
            Ok(response)
        }
    }

    async fn handle_record_stop(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
    ) -> Result<String, String> {
        let guild_id = validate_command_guild(command.guild_id, self.guild_id)?;
        let guild_key = guild_id.get().to_string();
        let caller_role = resolve_command_user_role(
            ctx,
            guild_id,
            command.user.id,
            command.member.as_deref(),
            &self.bot_admin_user_ids,
        );
        let caller_user_id = command.user.id.get().to_string();

        let authorized_meeting_id = {
            let mut service = self.service.lock().await;
            let meeting = service
                .store
                .find_active_meeting_by_guild(&guild_key)
                .map_err(|err| err.to_string())?
                .ok_or_else(|| CommandError::NoActiveMeeting.to_string())?;
            authorize_record_stop_for_meeting(&meeting, &caller_user_id, caller_role)
                .map_err(|err| err.to_string())?;
            meeting.id
        };

        let flushed_meeting_id = {
            let mut sessions = self.sessions.lock().await;
            // Flush remaining audio before stopping. Failed chunks stay
            // attached to the session and will be retried on the next stop
            // attempt.
            if let Some(session) = sessions
                .get_mut(&guild_key)
                .filter(|session| session.meeting_id == authorized_meeting_id)
            {
                flush_session_for_teardown(session, &guild_key, "manual stop")?;
                Some(session.meeting_id.clone())
            } else {
                None
            }
        };

        let stop_result = {
            let mut service = self.service.lock().await;
            let mut queue = self.queue.lock().await;
            stop_and_enqueue_summary_job(
                &mut service,
                &mut *queue,
                &guild_key,
                &caller_user_id,
                caller_role,
                Some(&authorized_meeting_id),
                StopReason::Manual,
            )?
        };

        let removed_session = {
            let mut sessions = self.sessions.lock().await;
            match (
                flushed_meeting_id.as_deref(),
                sessions
                    .get(&guild_key)
                    .map(|session| session.meeting_id.as_str()),
            ) {
                (Some(flushed), Some(current)) if flushed == current => sessions.remove(&guild_key),
                _ => None,
            }
        };
        {
            let mut states = self.auto_stop_states.lock().await;
            states.remove(&guild_key);
        }

        if let Some(manager) = songbird::get(ctx).await {
            let _ = manager.leave(guild_id).await;
        }

        // Persist SSRC mapping after voice teardown so all events
        // received up to disconnect are captured in the tracker.
        if let Some(session) = &removed_session {
            let tracker = self.ssrc_tracker.lock().await;
            session.persist_ssrc_mapping(&tracker);
        }

        {
            let result = stop_result;
            let meeting_id = result.meeting_id.clone();
            let outcome = result.outcome;

            if outcome == StopOutcome::Owner {
                if let Err(err) = self
                    .update_status_message(
                        &ctx.http,
                        &meeting_id,
                        StatusMessageUpdate::RecordingStopped,
                    )
                    .await
                {
                    warn!(
                        guild_id = %guild_key,
                        meeting_id = %meeting_id,
                        error = %err,
                        "failed to update status message after manual stop"
                    );
                }
                // Spawn summary processing in background — transcription and
                // AI summarization can take minutes, far beyond the interaction
                // response window, and should not block the command reply.
                let handler = self.clone();
                let http = Arc::clone(&ctx.http);
                tokio::spawn(async move {
                    let result = run_summary_background(&handler, &http, &meeting_id).await;
                    if let Err(err) = result {
                        error!(meeting_id = %meeting_id, error = %err, "summary background task failed");
                    }
                });
            }

            info!(
                guild_id = %guild_key,
                meeting_id = %result.meeting_id,
                outcome = ?outcome,
                "recording stop handled"
            );
            Ok(result.message)
        }
    }

    async fn run_summary_and_notify(&self, http: &Http, meeting_id: &str) -> Result<(), String> {
        let report_channel_id = match self.report_channel_id_for_meeting(meeting_id).await {
            Ok(value) => value,
            Err(err) => {
                let mut service = self.service.lock().await;
                let _ = service
                    .store
                    .set_meeting_status(meeting_id, MeetingStatus::Failed, None);
                let _ = service
                    .store
                    .set_error_message(meeting_id, Some(err.clone()));
                return Err(err);
            }
        };
        match self.process_enqueued_summary_job(http, meeting_id).await {
            Ok(output) => {
                let summary_url = self.public_base_url.as_ref().map(|base_url| {
                    format!("{}/meetings/{}", base_url.trim_end_matches('/'), meeting_id)
                });
                let chunks = if output.chunks.iter().all(|c| c.trim().is_empty()) {
                    vec!["会議が終了しました。要約内容がありません。".to_owned()]
                } else {
                    output.chunks
                };
                if let Err(err) =
                    post_summary_to_report_channel(http, report_channel_id, &chunks).await
                {
                    let error_message = format!("summary posting failed: {err}");
                    let exhausted = {
                        let mut service = self.service.lock().await;
                        let mut queue = self.queue.lock().await;
                        let job_id = format!("summary-{meeting_id}");
                        retry_summary_job_after_posting_failure(
                            &mut service.store,
                            &mut *queue,
                            meeting_id,
                            &job_id,
                            error_message,
                            self.summary_max_retries,
                        )
                    };
                    let exhausted = exhausted.map_err(|state_err| format!("{err}; {state_err}"))?;
                    if let Err(status_err) = self
                        .update_status_message(
                            http,
                            meeting_id,
                            StatusMessageUpdate::Failed {
                                phase: "summary_post",
                                error: &err,
                            },
                        )
                        .await
                    {
                        warn!(
                            meeting_id = %meeting_id,
                            error = %status_err,
                            "failed to update status message after summary posting failure"
                        );
                    }
                    if exhausted {
                        let _ = post_failure_to_report_channel(
                            http,
                            report_channel_id,
                            meeting_id,
                            &err,
                        )
                        .await;
                    }
                    return Err(err);
                }
                // Post meeting URL if PUBLIC_BASE_URL is configured
                if let Some(ref url) = summary_url {
                    let url_msg = format!("詳細はこちら: {url}");
                    if let Err(err) =
                        post_summary_to_report_channel(http, report_channel_id, &[url_msg]).await
                    {
                        warn!(meeting_id = %meeting_id, error = %err, "failed to post meeting URL");
                    }
                }
                if let Err(err) = self
                    .update_status_message(
                        http,
                        meeting_id,
                        StatusMessageUpdate::SummaryCompleted {
                            summary_url: summary_url.clone(),
                        },
                    )
                    .await
                {
                    warn!(
                        meeting_id = %meeting_id,
                        error = %err,
                        "failed to update status message after summary completion"
                    );
                }
                // Mark meeting as Posted and job as Done only after successful posting.
                // This order prevents data loss: if posting fails, the job stays
                // Running and can be recovered on restart.
                // Trade-off: if a concurrent recovery resets the status between
                // posting and this CAS, the CAS will fail and the summary may be
                // posted again on the next recovery cycle. Idempotent double-post
                // is preferred over losing the summary entirely.
                let mut service = self.service.lock().await;
                service
                    .store
                    .set_meeting_status(
                        meeting_id,
                        MeetingStatus::Posted,
                        Some(MeetingStatus::Summarizing),
                    )
                    .map_err(|err| err.to_string())?;
                service
                    .store
                    .set_error_message(meeting_id, None)
                    .map_err(|err| err.to_string())?;
                drop(service);
                {
                    let job_id = format!("summary-{meeting_id}");
                    let mut queue = self.queue.lock().await;
                    if let Err(err) = queue.mark_done(&job_id) {
                        error!(
                            job_id = %job_id,
                            meeting_id = %meeting_id,
                            error = %err,
                            "failed to mark summary job as done — job may be re-processed on restart"
                        );
                    }
                }
                Ok(())
            }
            Err(SummaryJobRunError::RetryScheduled(err)) => Err(err),
            Err(SummaryJobRunError::TerminalStatusUpdated(err)) => {
                let _ =
                    post_failure_to_report_channel(http, report_channel_id, meeting_id, &err).await;
                Err(err)
            }
            Err(SummaryJobRunError::Terminal(err)) => {
                // process_enqueued_summary_job already handles Failed/retry status.
                // Also update the status message so users see the failure.
                if let Err(status_err) = self
                    .update_status_message(
                        http,
                        meeting_id,
                        StatusMessageUpdate::Failed {
                            phase: "summary",
                            error: &err,
                        },
                    )
                    .await
                {
                    warn!(
                        meeting_id = %meeting_id,
                        error = %status_err,
                        "failed to update status message after summary failure"
                    );
                }
                let _ =
                    post_failure_to_report_channel(http, report_channel_id, meeting_id, &err).await;
                Err(err)
            }
        }
    }

    async fn post_failure_for_meeting(
        &self,
        http: &Http,
        meeting_id: &str,
        error_message: &str,
    ) -> Result<(), String> {
        let report_channel_id = self.report_channel_id_for_meeting(meeting_id).await?;
        if let Err(status_err) = self
            .update_status_message(
                http,
                meeting_id,
                StatusMessageUpdate::Failed {
                    phase: "summary",
                    error: error_message,
                },
            )
            .await
        {
            warn!(
                meeting_id = %meeting_id,
                error = %status_err,
                "failed to update status message while posting failure"
            );
        }
        post_failure_to_report_channel(http, report_channel_id, meeting_id, error_message).await
    }

    async fn report_channel_id_for_meeting(&self, meeting_id: &str) -> Result<u64, String> {
        let metadata = self.status_message_metadata(meeting_id).await?;
        metadata.report_channel_id.parse::<u64>().map_err(|err| {
            format!(
                "invalid report channel id: meeting_id={meeting_id}, value={}, error={err}",
                metadata.report_channel_id
            )
        })
    }

    async fn load_meeting(&self, meeting_id: &str) -> Result<StoredMeeting, String> {
        let mut service = self.service.lock().await;
        service
            .store
            .get_meeting(meeting_id)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| format!("meeting not found: meeting_id={meeting_id}"))
    }

    fn workspace_for_meeting(
        &self,
        meeting: &StoredMeeting,
    ) -> crate::infrastructure::workspace::MeetingWorkspacePaths {
        crate::infrastructure::workspace::MeetingWorkspaceLayout::new(&self.chunk_storage_dir)
            .for_meeting(&meeting.guild_id, &meeting.voice_channel_id, &meeting.id)
    }

    async fn process_enqueued_summary_job(
        &self,
        http: &Http,
        meeting_id: &str,
    ) -> Result<crate::application::worker::ProcessMeetingOutput, SummaryJobRunError> {
        let whisper = CommandWhisperClient {
            endpoint: self.whisper_endpoint.clone(),
            curl_bin: "curl".to_owned(),
            retry_policy: self.integration_retry_policy,
            beam_size: self.whisper_beam_size,
            suppress_non_speech: self.whisper_suppress_non_speech,
            prompt: self.whisper_prompt.clone(),
            vad: self.whisper_vad,
            temperature: self.whisper_temperature,
            command_timeout: DEFAULT_COMMAND_TIMEOUT,
        };
        let summary_client = HarnessCliSummaryClient {
            harness: self.summary_harness,
            command_path: self.summary_command.clone(),
            model: self.summary_model.clone(),
            retry_policy: self.integration_retry_policy,
            command_timeout: DEFAULT_COMMAND_TIMEOUT,
        };
        let job_id = format!("summary-{meeting_id}");
        let meeting = self.load_meeting(meeting_id).await?;
        let workspace = self.workspace_for_meeting(&meeting);
        workspace
            .ensure_base_dirs()
            .map_err(|err| format!("failed to prepare workspace: {err}"))?;
        let (meeting_dir, using_legacy_audio) = {
            let primary_dir = workspace.audio_dir();
            let primary_has_chunks = fs::read_dir(&primary_dir)
                .map(|entries| {
                    entries.filter_map(Result::ok).any(|entry| {
                        let path = entry.path();
                        path.file_stem()
                            .and_then(|stem| stem.to_str())
                            .map(|stem| stem != "mixdown")
                            .unwrap_or(false)
                            && path
                                .extension()
                                .and_then(|ext| ext.to_str())
                                .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
                    })
                })
                .unwrap_or(false);
            if primary_has_chunks {
                (primary_dir, false)
            } else {
                let legacy_dir = crate::infrastructure::workspace::MeetingWorkspaceLayout::new(
                    &self.chunk_storage_dir,
                )
                .legacy_meeting_dir(&meeting.id);
                (legacy_dir, true)
            }
        };
        if using_legacy_audio {
            warn!(
                meeting_id = %meeting.id,
                path = %meeting_dir.display(),
                "falling back to legacy mixdown path"
            );
        }

        let claimed_job = {
            let mut queue = self.queue.lock().await;
            queue.claim_by_id(&job_id).map_err(|err| err.to_string())?
        };
        let Some(claimed_job) = claimed_job else {
            return Err(SummaryJobRunError::Terminal(format!(
                "summary job was not available for job_id={job_id}"
            )));
        };
        if claimed_job.meeting_id != meeting_id {
            warn!(
                expected_meeting_id = %meeting_id,
                processed_meeting_id = %claimed_job.meeting_id,
                job_id = %claimed_job.id,
                "processed summary job for different meeting"
            );
        }

        let audio_path =
            match merge_user_chunks_to_mixdown(&meeting_dir, self.whisper_resample_to_16k) {
                Ok(path) => path,
                Err(err) => {
                    let err_string = format!("merge failed: {err}");
                    let mut queue = self.queue.lock().await;
                    let exhausted = retry_claimed_summary_job(
                        &mut *queue,
                        &claimed_job,
                        err_string.clone(),
                        self.summary_max_retries,
                        "merge",
                    );
                    drop(queue);
                    if exhausted {
                        let mut service = self.service.lock().await;
                        let _ = service.store.set_meeting_status(
                            &claimed_job.meeting_id,
                            MeetingStatus::Failed,
                            None,
                        );
                        let _ = service
                            .store
                            .set_error_message(&claimed_job.meeting_id, Some(err_string.clone()));
                    }
                    return Err(SummaryJobRunError::Terminal(err_string));
                }
            };

        let speaker_audio =
            match build_speaker_audio_inputs(&meeting_dir, self.whisper_resample_to_16k) {
                Ok(value) => value,
                Err(err) => {
                    let mut queue = self.queue.lock().await;
                    let exhausted = retry_claimed_summary_job(
                        &mut *queue,
                        &claimed_job,
                        err.to_string(),
                        self.summary_max_retries,
                        "transcription_input",
                    );
                    drop(queue);
                    if exhausted {
                        let mut service = self.service.lock().await;
                        let _ = service.store.set_meeting_status(
                            &claimed_job.meeting_id,
                            MeetingStatus::Failed,
                            None,
                        );
                        let _ = service
                            .store
                            .set_error_message(&claimed_job.meeting_id, Some(err.to_string()));
                        drop(service);
                        if let Err(status_err) = self
                            .update_status_message(
                                http,
                                &claimed_job.meeting_id,
                                StatusMessageUpdate::Failed {
                                    phase: "transcription_input",
                                    error: &err,
                                },
                            )
                            .await
                        {
                            warn!(
                                meeting_id = %claimed_job.meeting_id,
                                error = %status_err,
                                "failed to update status message after speaker audio error"
                            );
                        }
                    }
                    return Err(SummaryJobRunError::Terminal(err));
                }
            };

        let request = crate::application::summary::SummaryRequest {
            meeting_id: claimed_job.meeting_id.clone(),
            guild_id: meeting.guild_id.clone(),
            voice_channel_id: meeting.voice_channel_id.clone(),
            title: meeting.title.clone(),
            speaker_audio,
            audio_path,
            language: self.whisper_language.clone(),
            workspace: workspace.clone(),
        };

        // Phase 1: Transcription (mutex held only for status update)
        if let Err(cas_err) = {
            let mut service = self.service.lock().await;
            service.store.set_meeting_status(
                &claimed_job.meeting_id,
                MeetingStatus::Transcribing,
                Some(MeetingStatus::Stopping),
            )
        } {
            let cas_err_string = cas_err.to_string();
            // CAS failed — another process may own this meeting.  Mark the
            // job failed so it does not stay Running forever.
            warn!(meeting_id = %claimed_job.meeting_id, error = %cas_err, "CAS Stopping→Transcribing failed; marking job failed");
            let mut queue = self.queue.lock().await;
            let _ = queue.mark_failed(&claimed_job.id, cas_err_string.clone());
            drop(queue);
            if let Err(status_err) = self
                .update_status_message(
                    http,
                    &claimed_job.meeting_id,
                    StatusMessageUpdate::Failed {
                        phase: "summary_start",
                        error: &cas_err_string,
                    },
                )
                .await
            {
                warn!(
                    meeting_id = %claimed_job.meeting_id,
                    error = %status_err,
                    "failed to update status message after summary start CAS failure"
                );
            }
            return Err(SummaryJobRunError::Terminal(cas_err_string));
        }

        if let Err(err) = self
            .update_status_message(
                http,
                &claimed_job.meeting_id,
                StatusMessageUpdate::SummaryStarted,
            )
            .await
        {
            warn!(
                meeting_id = %claimed_job.meeting_id,
                error = %err,
                "failed to update status message at summary start"
            );
        }

        let transcription = tokio::task::block_in_place(|| {
            crate::application::summary::run_transcription(&whisper, &request)
        });
        let mut transcription = match transcription {
            Ok(t) => t,
            Err(err) => {
                let err_string = err.to_string();
                // Revert to Stopping so the next retry attempt's CAS guard succeeds.
                let reverted = {
                    let mut service = self.service.lock().await;
                    service
                        .store
                        .set_meeting_status(
                            &claimed_job.meeting_id,
                            MeetingStatus::Stopping,
                            Some(MeetingStatus::Transcribing),
                        )
                        .is_ok()
                };
                if reverted {
                    let mut queue = self.queue.lock().await;
                    let exhausted = retry_claimed_summary_job(
                        &mut *queue,
                        &claimed_job,
                        err_string.clone(),
                        self.summary_max_retries,
                        "transcription",
                    );
                    drop(queue);
                    if exhausted {
                        let mut service = self.service.lock().await;
                        let _ = service.store.set_meeting_status(
                            &claimed_job.meeting_id,
                            MeetingStatus::Failed,
                            None,
                        );
                        let _ = service
                            .store
                            .set_error_message(&claimed_job.meeting_id, Some(err_string.clone()));
                        drop(service);
                        if let Err(status_err) = self
                            .update_status_message(
                                http,
                                &claimed_job.meeting_id,
                                StatusMessageUpdate::Failed {
                                    phase: "transcription",
                                    error: &err_string,
                                },
                            )
                            .await
                        {
                            warn!(
                                meeting_id = %claimed_job.meeting_id,
                                error = %status_err,
                                "failed to update status message after transcription failure"
                            );
                        }
                    }
                } else {
                    // Revert failed — another process may have progressed the
                    // state.  Mark the job failed so it does not stay Running.
                    warn!(
                        meeting_id = %claimed_job.meeting_id,
                        "CAS revert to Stopping failed; marking job failed"
                    );
                    let mut queue = self.queue.lock().await;
                    let _ = queue.mark_failed(&claimed_job.id, err_string.clone());
                    if let Err(status_err) = self
                        .update_status_message(
                            http,
                            &claimed_job.meeting_id,
                            StatusMessageUpdate::Failed {
                                phase: "transcription",
                                error: &err_string,
                            },
                        )
                        .await
                    {
                        warn!(
                            meeting_id = %claimed_job.meeting_id,
                            error = %status_err,
                            "failed to update status message after transcription CAS failure"
                        );
                    }
                }
                return Err(SummaryJobRunError::Terminal(err_string));
            }
        };

        if let (Some(started_at), Some(stopped_at)) = (meeting.started_at, meeting.stopped_at) {
            match fetch_vc_text_messages(http, &meeting.voice_channel_id, started_at, stopped_at)
                .await
            {
                Ok(messages) => {
                    let started_at_ms = started_at.timestamp_millis();
                    let mut vc_segments = Vec::with_capacity(messages.len());
                    for msg in messages {
                        let delta_ms = msg.timestamp_ms.saturating_sub(started_at_ms);
                        let start_ms = delta_ms.max(0) as u64;
                        vc_segments.push(TranscriptSegment {
                            speaker_id: msg.speaker_id,
                            start_ms,
                            end_ms: start_ms.saturating_add(1),
                            text: msg.text,
                            confidence: None,
                            is_noisy: false,
                            source: TranscriptSource::VcText,
                            merged_count: 1,
                        });
                    }
                    if !vc_segments.is_empty() {
                        transcription.segments.extend(vc_segments);
                        transcription.segments.sort_by(|a, b| {
                            a.start_ms
                                .cmp(&b.start_ms)
                                .then(a.end_ms.cmp(&b.end_ms))
                                .then(a.speaker_id.cmp(&b.speaker_id))
                        });
                        transcription.segments = normalize_segments(
                            &transcription.segments,
                            NormalizationConfig::default(),
                        );
                        let masked = crate::domain::privacy::mask_pii(&render_for_summary(
                            &transcription.segments,
                            None,
                        ));
                        transcription.transcript_for_summary = masked.text;
                        transcription.masking_stats = masked.stats;
                    }
                }
                Err(err) => warn_and_fallback_on_vc_text_error(&claimed_job.meeting_id, &err),
            }
        } else {
            warn!(
                meeting_id = %claimed_job.meeting_id,
                started_at = %meeting.started_at.is_some(),
                stopped_at = %meeting.stopped_at.is_some(),
                "skipping VC text fetch: meeting timestamps unavailable"
            );
        }

        if let Err(err) = {
            let mut service = self.service.lock().await;
            persist_transcript_segments(
                &mut service.store.executor,
                &claimed_job.meeting_id,
                &transcription.segments,
            )
        } {
            let err = match err {
                TranscriptPersistError::Validation(message) => {
                    warn!(
                        meeting_id = %claimed_job.meeting_id,
                        error = %message,
                        "invalid transcript segment timestamps; failing summary job without retry"
                    );
                    {
                        let mut service = self.service.lock().await;
                        let mut queue = self.queue.lock().await;
                        let _ = queue.mark_failed(&claimed_job.id, message.clone());
                        let _ = service.store.set_meeting_status(
                            &claimed_job.meeting_id,
                            MeetingStatus::Failed,
                            None,
                        );
                        let _ = service
                            .store
                            .set_error_message(&claimed_job.meeting_id, Some(message.clone()));
                    }
                    if let Err(status_err) = self
                        .update_status_message(
                            http,
                            &claimed_job.meeting_id,
                            StatusMessageUpdate::Failed {
                                phase: "transcript_persist",
                                error: &message,
                            },
                        )
                        .await
                    {
                        warn!(
                            meeting_id = %claimed_job.meeting_id,
                            error = %status_err,
                            "failed to update status message after transcript validation failure"
                        );
                    }
                    return Err(SummaryJobRunError::TerminalStatusUpdated(message));
                }
                TranscriptPersistError::Database(message) => message,
            };
            warn!(
                meeting_id = %claimed_job.meeting_id,
                error = %err,
                "failed to persist transcript segments"
            );
            let retry_result = {
                let mut service = self.service.lock().await;
                let mut queue = self.queue.lock().await;
                retry_summary_job_after_transcript_persist_failure(
                    &mut service.store,
                    &mut *queue,
                    &claimed_job.meeting_id,
                    &claimed_job.id,
                    err.clone(),
                    self.summary_max_retries,
                )
            };
            let exhausted = match retry_result {
                Ok(exhausted) => exhausted,
                Err(retry_err) => {
                    warn!(
                        meeting_id = %claimed_job.meeting_id,
                        error = %retry_err,
                        "failed to update transcript persist retry state"
                    );
                    true
                }
            };
            if exhausted
                && let Err(status_err) = self
                    .update_status_message(
                        http,
                        &claimed_job.meeting_id,
                        StatusMessageUpdate::Failed {
                            phase: "transcript_persist",
                            error: &err,
                        },
                    )
                    .await
            {
                warn!(
                    meeting_id = %claimed_job.meeting_id,
                    error = %status_err,
                    "failed to update status message after transcript persist failure"
                );
            }
            if exhausted {
                return Err(SummaryJobRunError::TerminalStatusUpdated(err));
            }
            return Err(SummaryJobRunError::RetryScheduled(err));
        }

        // Resolve speaker labels for summarization and snapshot to DB (best-effort)
        let mut summary_transcript = transcription.transcript_for_summary.clone();
        let mut summary_masking_stats = transcription.masking_stats;
        if !transcription.segments.is_empty() {
            let speaker_profiles = self
                .resolve_and_upsert_speakers(http, &claimed_job.meeting_id, &transcription.segments)
                .await;
            if !speaker_profiles.is_empty() {
                let rendered = crate::domain::transcript::render_for_summary(
                    &transcription.segments,
                    Some(&speaker_profiles),
                );
                let masked = crate::domain::privacy::mask_pii(&rendered);
                summary_transcript = masked.text;
                summary_masking_stats = masked.stats;
            }
        }

        // The pre-correction transcript is accurate regardless of whether the
        // optional GEC step runs, so it is always persisted.
        crate::application::summary::persist_pre_correction_transcript_debug_artifact(
            &request.workspace,
            &summary_transcript,
        );

        // LLM transcript correction uses one large prompt; only stdin-based harnesses (Claude) are safe.
        // argv-based OpenCode / Cursor would pass the full transcript on the command line.
        let corrected_transcript = if !summary_client.can_run_llm_transcript_correction() {
            info!(
                meeting_id = %claimed_job.meeting_id,
                harness = %self.summary_harness,
                "skipping LLM transcript correction (not supported for argv-based summary harness)"
            );
            summary_transcript
        } else {
            // Persist the correction prompt only when the GEC step is actually
            // about to run; otherwise the artifact would falsely imply the
            // correction step executed. Reuse the built prompt for the
            // correction call to avoid building it twice.
            let correction_prompt =
                crate::application::summary::persist_correction_prompt_debug_artifact(
                    &request.workspace,
                    &summary_transcript,
                    self.whisper_language.as_deref(),
                )
                .unwrap_or_else(|| {
                    crate::application::summary::build_correction_prompt(
                        &summary_transcript,
                        self.whisper_language.as_deref(),
                    )
                });
            match tokio::task::block_in_place(|| {
                crate::application::summary::correct_transcript_with_prompt(
                    &summary_client,
                    &summary_transcript,
                    &correction_prompt,
                )
            }) {
                Ok(corrected) => corrected,
                Err(err) => {
                    warn!(meeting_id = %claimed_job.meeting_id, error = %err, "transcript correction failed, using original");
                    summary_transcript
                }
            }
        };

        // Phase 2: Summarization (mutex held only for status update)
        let transcription_for_summary = crate::application::summary::TranscriptionOutput {
            transcript_for_summary: corrected_transcript,
            masking_stats: summary_masking_stats,
            ..transcription.clone()
        };
        if let Err(cas_err) = {
            let mut service = self.service.lock().await;
            service.store.set_meeting_status(
                &claimed_job.meeting_id,
                MeetingStatus::Summarizing,
                Some(MeetingStatus::Transcribing),
            )
        } {
            let cas_err_string = cas_err.to_string();
            warn!(meeting_id = %claimed_job.meeting_id, error = %cas_err, "CAS Transcribing→Summarizing failed; marking job failed");
            let mut queue = self.queue.lock().await;
            let _ = queue.mark_failed(&claimed_job.id, cas_err_string.clone());
            if let Err(status_err) = self
                .update_status_message(
                    http,
                    &claimed_job.meeting_id,
                    StatusMessageUpdate::Failed {
                        phase: "summary_start",
                        error: &cas_err_string,
                    },
                )
                .await
            {
                warn!(
                    meeting_id = %claimed_job.meeting_id,
                    error = %status_err,
                    "failed to update status message after summary start CAS failure"
                );
            }
            return Err(SummaryJobRunError::Terminal(cas_err_string));
        }

        let markdown = tokio::task::block_in_place(|| {
            let manifest = crate::application::summary::write_transcript_files(
                &request,
                &transcription_for_summary,
            )?;
            let prompt = crate::application::summary::build_summary_prompt(&request, &manifest);
            crate::application::summary::persist_summary_prompt_debug_artifact(
                &request.workspace,
                &prompt,
            );
            summary_client.summarize(&prompt, Some(request.workspace.root()))
        });
        let markdown = match markdown {
            Ok(m) => m,
            Err(err) => {
                let err_string = err.to_string();
                // Revert to Stopping so the next retry attempt starts from a consistent state.
                let reverted = {
                    let mut service = self.service.lock().await;
                    service
                        .store
                        .set_meeting_status(
                            &claimed_job.meeting_id,
                            MeetingStatus::Stopping,
                            Some(MeetingStatus::Summarizing),
                        )
                        .is_ok()
                };
                if reverted {
                    let mut queue = self.queue.lock().await;
                    let exhausted = retry_claimed_summary_job(
                        &mut *queue,
                        &claimed_job,
                        err_string.clone(),
                        self.summary_max_retries,
                        "summary",
                    );
                    drop(queue);
                    if exhausted {
                        let mut service = self.service.lock().await;
                        let _ = service.store.set_meeting_status(
                            &claimed_job.meeting_id,
                            MeetingStatus::Failed,
                            None,
                        );
                        let _ = service
                            .store
                            .set_error_message(&claimed_job.meeting_id, Some(err_string.clone()));
                        drop(service);
                        if let Err(status_err) = self
                            .update_status_message(
                                http,
                                &claimed_job.meeting_id,
                                StatusMessageUpdate::Failed {
                                    phase: "summary",
                                    error: &err_string,
                                },
                            )
                            .await
                        {
                            warn!(
                                meeting_id = %claimed_job.meeting_id,
                                error = %status_err,
                                "failed to update status message after summary failure"
                            );
                        }
                    }
                } else {
                    warn!(
                        meeting_id = %claimed_job.meeting_id,
                        "CAS revert to Stopping failed; marking job failed"
                    );
                    let mut queue = self.queue.lock().await;
                    let _ = queue.mark_failed(&claimed_job.id, err_string.clone());
                    if let Err(status_err) = self
                        .update_status_message(
                            http,
                            &claimed_job.meeting_id,
                            StatusMessageUpdate::Failed {
                                phase: "summary",
                                error: &err_string,
                            },
                        )
                        .await
                    {
                        warn!(
                            meeting_id = %claimed_job.meeting_id,
                            error = %status_err,
                            "failed to update status message after summary CAS failure"
                        );
                    }
                }
                return Err(SummaryJobRunError::Terminal(err_string));
            }
        };

        // Persist summary markdown to DB (best-effort)
        {
            let summary_id = format!("{}-s-1", claimed_job.meeting_id);
            let mut service = self.service.lock().await;
            if let Err(err) = service.store.executor.execute(
                crate::infrastructure::sql::INSERT_SUMMARY_SQL,
                &[summary_id, claimed_job.meeting_id.clone(), markdown.clone()],
            ) {
                warn!(
                    meeting_id = %claimed_job.meeting_id,
                    error = %err,
                    "failed to persist summary"
                );
            }
        }

        let chunks = split_discord_message(&markdown, DISCORD_MESSAGE_LIMIT);

        // NOTE: mark_done is NOT called here. The caller (run_summary_and_notify)
        // must call it after the Discord posting succeeds. This prevents data loss
        // if posting fails -- the job stays Running and can be recovered on restart.

        Ok(crate::application::worker::ProcessMeetingOutput {
            meeting_id: claimed_job.meeting_id,
            markdown,
            chunks,
        })
    }

    async fn resolve_and_upsert_speakers(
        &self,
        http: &Http,
        meeting_id: &str,
        segments: &[crate::domain::transcript::TranscriptSegment],
    ) -> HashMap<String, SpeakerProfile> {
        let speaker_ids: HashSet<String> =
            segments.iter().map(|seg| seg.speaker_id.clone()).collect();
        if speaker_ids.is_empty() {
            return HashMap::new();
        }

        let (guild_id, mut profiles) = self.load_guild_and_speakers(meeting_id).await;

        let Some(guild_id) = guild_id else {
            warn!(
                meeting_id = %meeting_id,
                "meeting not found while resolving speakers"
            );
            return profiles;
        };
        let guild_id_u64 = match guild_id.parse::<u64>() {
            Ok(value) => value,
            Err(err) => {
                warn!(
                    meeting_id = %meeting_id,
                    guild_id = %guild_id,
                    error = %err,
                    "failed to parse guild_id while resolving speakers"
                );
                return profiles;
            }
        };

        let mut newly_resolved = Vec::new();
        for speaker_id in speaker_ids {
            if let Some(existing) = profiles.get(&speaker_id) {
                let has_profile_data = existing.nickname.is_some()
                    || existing.display_name.is_some()
                    || existing.username.is_some();
                if has_profile_data {
                    continue;
                }
            }
            if speaker_id.trim().is_empty() {
                continue;
            }
            let user_id_u64 = match speaker_id.parse::<u64>() {
                Ok(value) => value,
                Err(err) => {
                    if SsrcTracker::parse_ssrc_fallback(&speaker_id).is_some() {
                        debug!(
                            meeting_id = %meeting_id,
                            speaker_id = %speaker_id,
                            "skipping SSRC-based speaker (mapping was unavailable)"
                        );
                    } else {
                        warn!(
                            meeting_id = %meeting_id,
                            speaker_id = %speaker_id,
                            error = %err,
                            "failed to parse speaker_id while resolving speakers"
                        );
                    }
                    continue;
                }
            };

            match http
                .get_member(GuildId::new(guild_id_u64), UserId::new(user_id_u64))
                .await
            {
                Ok(member) => {
                    let profile = SpeakerProfile {
                        speaker_id: speaker_id.clone(),
                        username: Some(member.user.name.clone()),
                        nickname: member.nick.clone(),
                        display_name: member.user.global_name.clone(),
                    };
                    profiles.insert(speaker_id, profile.clone());
                    newly_resolved.push(profile);
                }
                Err(err) => {
                    warn!(
                        meeting_id = %meeting_id,
                        speaker_id = %speaker_id,
                        error = %err,
                        "failed to fetch member while resolving speakers"
                    );
                }
            }
        }

        if !newly_resolved.is_empty() {
            let mut service = self.service.lock().await;
            for profile in &newly_resolved {
                if let Err(err) = service.store.executor.execute(
                    crate::infrastructure::sql::UPSERT_MEETING_SPEAKER_SQL,
                    &[
                        meeting_id.to_owned(),
                        profile.speaker_id.clone(),
                        profile.username.clone().unwrap_or_default(),
                        profile.nickname.clone().unwrap_or_default(),
                        profile.display_name.clone().unwrap_or_default(),
                    ],
                ) {
                    warn!(
                        meeting_id = %meeting_id,
                        speaker_id = %profile.speaker_id,
                        error = %err,
                        "failed to upsert meeting speaker snapshot"
                    );
                }
            }
        }

        profiles
    }

    async fn load_guild_and_speakers(
        &self,
        meeting_id: &str,
    ) -> (Option<String>, HashMap<String, SpeakerProfile>) {
        let (guild_row, speaker_rows) = {
            let mut service = self.service.lock().await;
            let guild_row = match service.store.executor.query_rows(
                "SELECT guild_id FROM meetings WHERE id=$1 LIMIT 1",
                &[meeting_id.to_owned()],
            ) {
                Ok(rows) => rows,
                Err(err) => {
                    warn!(
                        meeting_id = %meeting_id,
                        error = %err,
                        "failed to load guild_id while resolving speakers"
                    );
                    Vec::new()
                }
            };
            let speaker_rows = match service.store.executor.query_rows(
                "SELECT speaker_id, username, nickname, display_name \
                     FROM meeting_speakers WHERE meeting_id=$1",
                &[meeting_id.to_owned()],
            ) {
                Ok(rows) => rows,
                Err(err) => {
                    warn!(
                        meeting_id = %meeting_id,
                        error = %err,
                        "failed to load existing meeting speakers"
                    );
                    Vec::new()
                }
            };
            (guild_row, speaker_rows)
        };

        let guild_id = guild_row
            .into_iter()
            .next()
            .and_then(|row| row.into_iter().next());

        let mut profiles = HashMap::new();
        for row in speaker_rows {
            if row.len() < 4 {
                continue;
            }
            let profile = SpeakerProfile {
                speaker_id: row[0].clone(),
                username: optional_string(&row[1]),
                nickname: optional_string(&row[2]),
                display_name: optional_string(&row[3]),
            };
            profiles.insert(profile.speaker_id.clone(), profile);
        }

        (guild_id, profiles)
    }
}

fn optional_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

#[derive(Clone)]
struct VoiceReceiveHandler {
    tracker: Arc<Mutex<SsrcTracker>>,
    sessions: Arc<Mutex<HashMap<String, RecordingSession<LocalChunkStorage>>>>,
    guild_id: String,
    runtime: ScaffoldHandler,
    http: Arc<Http>,
    ctx: Context,
}

#[serenity::async_trait]
impl SongbirdEventHandler for VoiceReceiveHandler {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        match ctx {
            EventContext::SpeakingStateUpdate(evt) => {
                if let Some(user_id) = evt.user_id {
                    let mut tracker = self.tracker.lock().await;
                    let user_id_u64 = user_id.0;
                    let user_id_str = user_id_u64.to_string();
                    tracker.update_mapping(evt.ssrc, user_id_u64);
                    drop(tracker);

                    // Re-key any in-memory frames buffered under the SSRC fallback ID
                    let ssrc_key = SsrcTracker::fallback_key(evt.ssrc);
                    let mut sessions = self.sessions.lock().await;
                    if let Some(session) = sessions.get_mut(&self.guild_id) {
                        let moved = session.rekey_user(&ssrc_key, &user_id_str);
                        if moved > 0 {
                            info!(
                                ssrc = evt.ssrc,
                                user_id = user_id_u64,
                                frames_moved = moved,
                                "re-keyed in-memory audio frames from SSRC fallback to user ID"
                            );
                        }
                    }
                }
            }
            EventContext::VoiceTick(tick) => {
                let ts = now_ms();
                let tracker = self.tracker.lock().await;
                let adapted = adapt_voice_tick(tick, ts, &tracker);
                drop(tracker);
                let mut sessions = self.sessions.lock().await;
                if let Some(session) = sessions.get_mut(&self.guild_id)
                    && let Err(err) = ingest_voice_frames_into_session(session, &adapted)
                {
                    warn!(guild_id = %self.guild_id, error = %err, "failed to ingest voice tick");
                }
            }
            EventContext::DriverDisconnect(data) => {
                warn!(
                    guild_id = %self.guild_id,
                    channel_id = data.channel_id.0.get(),
                    kind = ?data.kind,
                    reason = ?data.reason,
                    "bot voice driver disconnected"
                );
                {
                    let runtime = self.runtime.clone();
                    let guild_key = self.guild_id.clone();
                    let http = Arc::clone(&self.http);
                    let ctx_for_task = self.ctx.clone();
                    let expected_meeting_id = runtime.active_meeting_id().await;
                    let grace = Duration::from_secs(runtime.auto_stop_grace_seconds);
                    tokio::spawn(async move {
                        sleep(grace).await;
                        let current_meeting_id = runtime.active_meeting_id().await;
                        if current_meeting_id != expected_meeting_id || current_meeting_id.is_none()
                        {
                            return;
                        }
                        let Some(target_voice_channel_id) =
                            runtime.active_meeting_voice_channel_id().await
                        else {
                            return;
                        };
                        let reconnected = is_bot_connected_to_voice_channel(
                            &ctx_for_task,
                            runtime.guild_id,
                            target_voice_channel_id,
                        );
                        let non_bot = count_non_bot_members_in_target_voice(
                            &ctx_for_task,
                            runtime.guild_id,
                            target_voice_channel_id,
                        );
                        // Treat cache misses as "unknown" rather than "empty/disconnected" to
                        // avoid stopping an active recording when the guild cache is transiently
                        // unavailable (e.g. during gateway reconnect / warm-up).
                        let (Some(reconnected), Some(non_bot)) = (reconnected, non_bot) else {
                            warn!(
                                guild_id = %runtime.guild_id,
                                target_voice_channel_id,
                                "voice state cache unavailable on driver-disconnect grace expiry; skipping stop"
                            );
                            return;
                        };
                        if reconnected || non_bot > 0 {
                            return;
                        }
                        // Flush remaining audio before stopping. Failed
                        // chunks stay attached to the session for retry.
                        let removed_session = {
                            let mut sessions = runtime.sessions.lock().await;
                            if let Some(session) = sessions.get_mut(&guild_key)
                                && flush_session_for_teardown(
                                    session,
                                    &guild_key,
                                    "driver disconnect",
                                )
                                .is_err()
                            {
                                drop(sessions);
                                if let Some(meeting_id) = expected_meeting_id.as_deref()
                                    && let Err(err) = runtime
                                        .update_status_message(
                                            &http,
                                            meeting_id,
                                            StatusMessageUpdate::Failed {
                                                phase: "Recording persist",
                                                error: "final audio flush failed after voice disconnect; recording session is retained for manual stop retry",
                                            },
                                        )
                                        .await
                                {
                                    warn!(
                                        guild_id = %guild_key,
                                        meeting_id,
                                        error = %err,
                                        "failed to notify driver-disconnect final flush failure"
                                    );
                                }
                                return;
                            }
                            sessions.remove(&guild_key)
                        };
                        {
                            let mut states = runtime.auto_stop_states.lock().await;
                            states.remove(&guild_key);
                        }
                        // Persist SSRC mapping after session removal so all
                        // events received up to disconnect are captured.
                        if let Some(session) = &removed_session {
                            let tracker = runtime.ssrc_tracker.lock().await;
                            session.persist_ssrc_mapping(&tracker);
                        }
                        let stop_result = {
                            let mut service = runtime.service.lock().await;
                            let mut queue = runtime.queue.lock().await;
                            stop_and_enqueue_summary_job(
                                &mut service,
                                &mut *queue,
                                &guild_key,
                                "driver-disconnect",
                                UserRole::BotAdmin,
                                None,
                                StopReason::ClientDisconnect,
                            )
                        };
                        match stop_result {
                            Ok(result) => {
                                if result.outcome == StopOutcome::Owner
                                    && let Err(err) = runtime
                                        .update_status_message(
                                            &http,
                                            &result.meeting_id,
                                            StatusMessageUpdate::RecordingStopped,
                                        )
                                        .await
                                {
                                    warn!(
                                        guild_id = %guild_key,
                                        meeting_id = %result.meeting_id,
                                        error = %err,
                                        "failed to update status message after driver disconnect stop"
                                    );
                                }
                                if result.outcome == StopOutcome::Owner
                                    && let Err(err) =
                                        run_summary_background(&runtime, &http, &result.meeting_id)
                                            .await
                                {
                                    warn!(
                                        guild_id = %guild_key,
                                        meeting_id = %result.meeting_id,
                                        error = %err,
                                        "failed to process summary after driver disconnect"
                                    );
                                }
                            }
                            Err(err) => {
                                warn!(
                                    guild_id = %guild_key,
                                    error = %err,
                                    "failed to stop recording on driver disconnect"
                                );
                            }
                        }
                    });
                }
            }
            _ => {}
        }
        None
    }
}

pub fn ingest_voice_frames_into_session(
    session: &mut RecordingSession<LocalChunkStorage>,
    adapted: &AdaptedVoiceFrames,
) -> Result<usize, String> {
    for (user_id, frame) in &adapted.per_user {
        session.ingest_frame(user_id, frame.clone());
    }

    session
        .flush_due(Instant::now())
        .map(|result| {
            if result.newly_failed > 0 {
                tracing::warn!(
                    failed_count = result.failed.len(),
                    newly_failed = result.newly_failed,
                    "some audio chunks could not be persisted during ingest flush"
                );
            }
            result.persisted.len()
        })
        .map_err(|err| err.to_string())
}

fn now_ms() -> u64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_millis() as u64,
        Err(err) => {
            warn!(error = %err, "system clock is before UNIX epoch, returning 0");
            0
        }
    }
}

fn is_bot_connected_to_voice_channel(
    ctx: &Context,
    guild_id: GuildId,
    target_voice_channel_id: u64,
) -> Option<bool> {
    let guild = ctx.cache.guild(guild_id)?;
    let bot_user_id = ctx.cache.current_user().id;
    let connected_channel = guild
        .voice_states
        .get(&bot_user_id)
        .and_then(|voice| voice.channel_id)
        .map(|channel| channel.get());
    Some(connected_channel == Some(target_voice_channel_id))
}

fn count_non_bot_members_in_target_voice(
    ctx: &Context,
    guild_id: GuildId,
    target_voice_channel_id: u64,
) -> Option<usize> {
    let guild = ctx.cache.guild(guild_id)?;
    let mut non_bot_count = 0usize;
    for (user_id, voice_state) in &guild.voice_states {
        if voice_state.channel_id.map(|channel| channel.get()) != Some(target_voice_channel_id) {
            continue;
        }
        let is_bot = voice_state
            .member
            .as_ref()
            .map(|member| member.user.bot)
            .or_else(|| ctx.cache.user(*user_id).map(|user| user.bot))
            .unwrap_or(false);
        if !is_bot {
            non_bot_count += 1;
        }
    }
    Some(non_bot_count)
}

fn resolve_bot_permissions(
    ctx: &Context,
    guild_id: GuildId,
    voice_channel_id: Option<u64>,
    text_channel_id: Option<u64>,
) -> PermissionSet {
    use serenity::all::Permissions;

    let Some(guild) = ctx.cache.guild(guild_id) else {
        warn!(guild_id = %guild_id, "guild not found in cache, denying bot permissions");
        return denied_bot_permissions();
    };
    let bot_id = ctx.cache.current_user().id;
    let Some(member) = guild.members.get(&bot_id) else {
        warn!(guild_id = %guild_id, bot_id = %bot_id, "bot member not found in cache, denying bot permissions");
        return denied_bot_permissions();
    };

    let voice_channel_permission = voice_channel_id.and_then(|vc_id| {
        let channel = guild.channels.get(&ChannelId::new(vc_id))?;
        let perms = guild.user_permissions_in(channel, member);
        Some(perms.contains(Permissions::CONNECT))
    });

    let text_channel_permission = text_channel_id.and_then(|tc_id| {
        let channel = guild.channels.get(&ChannelId::new(tc_id))?;
        let perms = guild.user_permissions_in(channel, member);
        Some(perms.contains(Permissions::SEND_MESSAGES))
    });

    bot_permissions_from_cache_state(
        true,
        true,
        voice_channel_permission,
        text_channel_permission,
    )
}

fn resolve_command_user_role(
    ctx: &Context,
    guild_id: GuildId,
    user_id: UserId,
    interaction_member: Option<&Member>,
    bot_admin_user_ids: &HashSet<String>,
) -> UserRole {
    use serenity::all::{Permissions, RoleId};

    if bot_admin_user_ids.contains(&user_id.get().to_string()) {
        return UserRole::BotAdmin;
    }

    let Some(guild) = ctx.cache.guild(guild_id) else {
        return interaction_member
            .and_then(|member| member.permissions)
            .filter(|permissions| permissions.contains(Permissions::ADMINISTRATOR))
            .map(|_| UserRole::GuildAdmin)
            .unwrap_or_else(|| {
                warn!(guild_id = %guild_id, user_id = %user_id, "guild not found in cache, treating command user as member");
                UserRole::Member
            });
    };
    if guild.owner_id == user_id {
        return UserRole::GuildAdmin;
    }
    if interaction_member
        .and_then(|member| member.permissions)
        .is_some_and(|permissions| permissions.contains(Permissions::ADMINISTRATOR))
    {
        return UserRole::GuildAdmin;
    }
    let cached_member = guild.members.get(&user_id);
    let role_ids = interaction_member
        .map(|member| member.roles.as_slice())
        .or_else(|| cached_member.map(|member| member.roles.as_slice()));
    let Some(role_ids) = role_ids else {
        warn!(guild_id = %guild_id, user_id = %user_id, "command user not found in cache, treating as member");
        return UserRole::Member;
    };
    let everyone_is_admin = guild
        .roles
        .get(&RoleId::new(guild_id.get()))
        .is_some_and(|role| role.permissions.contains(Permissions::ADMINISTRATOR));
    let role_is_admin = role_ids.iter().any(|role_id| {
        guild
            .roles
            .get(role_id)
            .is_some_and(|role| role.permissions.contains(Permissions::ADMINISTRATOR))
    });
    if everyone_is_admin || role_is_admin {
        UserRole::GuildAdmin
    } else {
        UserRole::Member
    }
}

pub fn denied_bot_permissions() -> PermissionSet {
    PermissionSet {
        can_connect_voice: false,
        can_send_messages: false,
    }
}

pub fn bot_permissions_from_cache_state(
    guild_present: bool,
    bot_member_present: bool,
    voice_channel_permission: Option<bool>,
    text_channel_permission: Option<bool>,
) -> PermissionSet {
    if !guild_present || !bot_member_present {
        return denied_bot_permissions();
    }
    PermissionSet {
        can_connect_voice: voice_channel_permission.unwrap_or(false),
        can_send_messages: text_channel_permission.unwrap_or(false),
    }
}

fn resolve_user_voice_channel_id(ctx: &Context, guild_id: GuildId, user_id: UserId) -> Option<u64> {
    let guild = ctx.cache.guild(guild_id)?;
    guild
        .voice_states
        .get(&user_id)
        .and_then(|state| state.channel_id)
        .map(|id| id.get())
}

pub fn stop_reason_from_interaction(command: &CommandInteraction) -> Result<StopReason, String> {
    for option in &command.data.options {
        if option.name != "reason" {
            continue;
        }
        if let CommandDataOptionValue::String(value) = &option.value {
            return parse_stop_reason(value);
        }
    }
    Ok(StopReason::Manual)
}

pub fn parse_stop_reason(value: &str) -> Result<StopReason, String> {
    StopReason::parse_str(value).ok_or_else(|| format!("invalid stop reason: {value}"))
}

/// Runs summary + notify in a background context (merge is handled inside the job).
/// All errors are handled internally (failure notification + status update).
async fn run_summary_background(
    handler: &ScaffoldHandler,
    http: &Http,
    meeting_id: &str,
) -> Result<(), String> {
    handler.run_summary_and_notify(http, meeting_id).await
}

async fn post_summary_to_report_channel(
    http: &Http,
    report_channel_id: u64,
    chunks: &[String],
) -> Result<(), String> {
    let channel = ChannelId::new(report_channel_id);
    for chunk in chunks {
        if chunk.trim().is_empty() {
            continue;
        }
        channel
            .say(http, chunk)
            .await
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

async fn post_failure_to_report_channel(
    http: &Http,
    report_channel_id: u64,
    meeting_id: &str,
    error_message: &str,
) -> Result<(), String> {
    let base = format!("要約処理に失敗しました: meeting_id={meeting_id}\nerror={error_message}");
    let channel = ChannelId::new(report_channel_id);
    for part in split_discord_message(&base, DISCORD_MESSAGE_LIMIT) {
        if part.trim().is_empty() {
            continue;
        }
        channel
            .say(http, part)
            .await
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod status_message_tests {
    use super::*;
    use crate::audio::receiver::{BufferedFrame, ReceiverConfig};
    use crate::infrastructure::storage_fs::{ChunkStorageError, SavedChunk};
    use serenity::async_trait;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Debug, Clone)]
    struct RuntimeFlakyChunkStorage {
        failures_remaining: Arc<AtomicUsize>,
    }

    impl ChunkStorage for RuntimeFlakyChunkStorage {
        fn save_chunk(
            &self,
            _meeting_id: &str,
            user_id: &str,
            sequence: u64,
            start_ms: u64,
            bytes: &[u8],
        ) -> Result<SavedChunk, ChunkStorageError> {
            if self
                .failures_remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                    if value > 0 { Some(value - 1) } else { None }
                })
                .is_ok()
            {
                return Err(ChunkStorageError::Io("injected failure".to_owned()));
            }
            Ok(SavedChunk {
                path: std::env::temp_dir().join(format!("{user_id}_{sequence}_{start_ms}.wav")),
                size_bytes: bytes.len(),
            })
        }
    }

    fn session_with_one_flaky_chunk(failures: usize) -> RecordingSession<RuntimeFlakyChunkStorage> {
        let mut session = RecordingSession::new(
            "meeting-1".to_owned(),
            RuntimeFlakyChunkStorage {
                failures_remaining: Arc::new(AtomicUsize::new(failures)),
            },
            ReceiverConfig {
                chunk_duration: Duration::from_secs(20),
            },
            48_000,
        );
        session.ingest_frame(
            "u1",
            BufferedFrame {
                timestamp_ms: 1_000,
                pcm_16le_bytes: vec![0, 0, 1, 0],
            },
        );
        session
    }

    fn assert_final_flush_failure_is_retryable(phase: &str) {
        let mut session = session_with_one_flaky_chunk(1);

        assert!(flush_session_for_teardown(&mut session, "g1", phase).is_err());
        assert!(
            flush_session_for_teardown(&mut session, "g1", phase).is_ok(),
            "{phase} should be able to retry retained failed chunks"
        );
    }

    #[test]
    fn summary_retry_exhaustion_only_on_durable_failed_status() {
        assert!(summary_retry_exhausted(
            Ok(crate::domain::JobStatus::Failed),
            "m1",
            "j1",
            "summary"
        ));
        assert!(!summary_retry_exhausted(
            Ok(crate::domain::JobStatus::Queued),
            "m1",
            "j1",
            "summary"
        ));
    }

    #[test]
    fn recovery_meeting_status_rejects_unknown_values() {
        assert_eq!(
            parse_meeting_status("recording"),
            Ok(crate::domain::MeetingStatus::Recording)
        );
        assert_eq!(
            parse_meeting_status("corrupt"),
            Err("unknown meeting status: corrupt".to_owned())
        );
    }

    #[test]
    fn summary_retry_backend_error_does_not_mark_exhausted() {
        assert!(!summary_retry_exhausted(
            Err(crate::infrastructure::queue::QueueError::Backend(
                "retry backend down".to_owned()
            )),
            "m1",
            "j1",
            "summary"
        ));
    }

    #[test]
    fn summary_retry_missing_or_invalid_job_marks_exhausted() {
        assert!(summary_retry_exhausted(
            Err(crate::infrastructure::queue::QueueError::NotFound {
                job_id: "j1".to_owned()
            }),
            "m1",
            "j1",
            "summary"
        ));
        assert!(summary_retry_exhausted(
            Err(crate::infrastructure::queue::QueueError::InvalidState {
                job_id: "j1".to_owned(),
                expected: "running".to_owned(),
                actual: "done".to_owned()
            }),
            "m1",
            "j1",
            "summary"
        ));
    }

    fn fake_recovery_queue_with_existing_summary_job(
        status: &str,
    ) -> SqlJobQueue<crate::infrastructure::sql_store::FakeSqlExecutor> {
        let mut executor = crate::infrastructure::sql_store::FakeSqlExecutor::default();
        let job_id = "summary-m1";
        let enqueue_key = format!(
            "{}|{}",
            crate::infrastructure::sql::ENQUEUE_JOB_SQL,
            ["summary-m1", "m1", "summarize"].join("\u{1f}")
        );
        executor.execute_error.insert(
            enqueue_key,
            format!(
                "{}duplicate job",
                crate::infrastructure::sql_store::UNIQUE_VIOLATION_PREFIX
            ),
        );
        let status_key = format!("{}|{}", RECOVERY_SUMMARY_JOB_STATUS_SQL, job_id);
        executor
            .query_rows_result
            .insert(status_key, vec![vec![status.to_owned()]]);
        SqlJobQueue::new(executor)
    }

    #[test]
    fn recovery_existing_queued_summary_job_is_claimable() {
        let mut queue = fake_recovery_queue_with_existing_summary_job("queued");

        assert!(recover_summary_job_for_startup(
            &mut queue,
            "summary-m1",
            "m1"
        ));
    }

    #[test]
    fn recovery_existing_running_or_failed_summary_job_is_not_claimable() {
        let mut running_queue = fake_recovery_queue_with_existing_summary_job("running");
        assert!(!recover_summary_job_for_startup(
            &mut running_queue,
            "summary-m1",
            "m1"
        ));

        let mut failed_queue = fake_recovery_queue_with_existing_summary_job("failed");
        assert!(!recover_summary_job_for_startup(
            &mut failed_queue,
            "summary-m1",
            "m1"
        ));
    }

    #[test]
    fn recovery_existing_summary_job_status_error_is_not_claimable() {
        let mut queue = fake_recovery_queue_with_existing_summary_job("queued");
        let status_key = format!("{}|{}", RECOVERY_SUMMARY_JOB_STATUS_SQL, "summary-m1");
        queue
            .executor
            .query_rows_error
            .insert(status_key, "status lookup failed".to_owned());

        assert!(!recover_summary_job_for_startup(
            &mut queue,
            "summary-m1",
            "m1"
        ));
    }

    #[test]
    fn recovery_stale_running_reset_does_not_target_failed_jobs() {
        assert!(RECOVERY_REQUEUE_STALE_RUNNING_SUMMARY_JOB_SQL.contains("status='running'"));
        assert!(!RECOVERY_REQUEUE_STALE_RUNNING_SUMMARY_JOB_SQL.contains("status IN"));
        assert!(!RECOVERY_REQUEUE_STALE_RUNNING_SUMMARY_JOB_SQL.contains("'failed'"));
        assert!(RECOVERY_REQUEUE_STALE_RUNNING_SUMMARY_JOB_SQL.contains("updated_at <"));
    }

    fn running_summary_job() -> crate::infrastructure::queue::Job {
        crate::infrastructure::queue::Job {
            id: "summary-m1".to_owned(),
            meeting_id: "m1".to_owned(),
            job_type: crate::domain::JobType::Summarize,
            status: crate::domain::JobStatus::Running,
            retry_count: 0,
            error_message: None,
        }
    }

    fn summarizing_meeting() -> crate::infrastructure::storage::StoredMeeting {
        crate::infrastructure::storage::StoredMeeting {
            id: "m1".to_owned(),
            guild_id: "g1".to_owned(),
            voice_channel_id: "vc1".to_owned(),
            report_channel_id: "tc1".to_owned(),
            status_message_channel_id: None,
            status_message_id: None,
            started_by_user_id: "u1".to_owned(),
            title: None,
            status: crate::domain::MeetingStatus::Summarizing,
            stop_reason: None,
            error_message: None,
            started_at: None,
            stopped_at: None,
        }
    }

    fn transcribing_meeting() -> crate::infrastructure::storage::StoredMeeting {
        let mut meeting = summarizing_meeting();
        meeting.status = crate::domain::MeetingStatus::Transcribing;
        meeting
    }

    fn transcript_segment(start_ms: u64, end_ms: u64) -> TranscriptSegment {
        TranscriptSegment {
            speaker_id: "alice".to_owned(),
            start_ms,
            end_ms,
            text: "hello".to_owned(),
            confidence: Some(0.9),
            is_noisy: false,
            source: TranscriptSource::Voice,
            merged_count: 1,
        }
    }

    #[test]
    fn transcript_persist_rejects_timestamp_above_db_integer_range() {
        let mut executor = crate::infrastructure::sql_store::FakeSqlExecutor::default();
        let err = persist_transcript_segments(
            &mut executor,
            "m1",
            &[transcript_segment(0, MAX_DB_TIMESTAMP_MS + 1)],
        )
        .expect_err("overflowing transcript timestamp should fail before SQL");

        assert!(matches!(err, TranscriptPersistError::Validation(_)));
        assert!(err.to_string().contains("exceeds database integer range"));
        assert!(
            executor.executed.is_empty(),
            "invalid timestamps should not reach the SQL executor"
        );
    }

    #[test]
    fn transcript_persist_surfaces_insert_failure() {
        let mut executor = crate::infrastructure::sql_store::FakeSqlExecutor::default();
        let segment = transcript_segment(0, 1_000);
        let base_sql = crate::infrastructure::sql::build_insert_transcripts_sql_with_offset(1, 1);
        let sql =
            format!("WITH cleared AS (DELETE FROM transcripts WHERE meeting_id=$1) {base_sql}");
        let params = vec![
            "m1".to_owned(),
            "m1-t-0".to_owned(),
            "m1".to_owned(),
            "alice".to_owned(),
            "0".to_owned(),
            "1000".to_owned(),
            "hello".to_owned(),
            "0.9".to_owned(),
            "false".to_owned(),
            "voice".to_owned(),
        ];
        let key = format!("{}|{}", sql, params.join("\u{1f}"));
        executor
            .execute_error
            .insert(key, "integer out of range".to_owned());

        let err = persist_transcript_segments(&mut executor, "m1", &[segment])
            .expect_err("insert failure should be surfaced");

        assert!(matches!(err, TranscriptPersistError::Database(_)));
        assert!(
            err.to_string()
                .contains("failed to persist transcript segments")
        );
        assert!(err.to_string().contains("integer out of range"));
    }

    #[test]
    fn merge_phase_error_requeues_claimed_summary_job_before_exhaustion() {
        let mut queue = crate::infrastructure::queue::InMemoryJobQueue::new();
        let job = running_summary_job();
        queue.enqueue(job.clone()).expect("enqueue should succeed");

        let exhausted =
            retry_claimed_summary_job(&mut queue, &job, "merge failed".to_owned(), 2, "merge");

        assert!(!exhausted);
        let updated = queue.get(&job.id).expect("job should remain");
        assert_eq!(updated.status, crate::domain::JobStatus::Queued);
        assert_eq!(updated.retry_count, 1);
        assert_eq!(updated.error_message.as_deref(), Some("merge failed"));
    }

    #[test]
    fn merge_phase_error_marks_claimed_summary_job_failed_after_exhaustion() {
        let mut queue = crate::infrastructure::queue::InMemoryJobQueue::new();
        let job = running_summary_job();
        queue.enqueue(job.clone()).expect("enqueue should succeed");

        let exhausted =
            retry_claimed_summary_job(&mut queue, &job, "merge failed".to_owned(), 0, "merge");

        assert!(exhausted);
        let updated = queue.get(&job.id).expect("job should remain");
        assert_eq!(updated.status, crate::domain::JobStatus::Failed);
        assert_eq!(updated.retry_count, 1);
        assert_eq!(updated.error_message.as_deref(), Some("merge failed"));
    }

    #[test]
    fn summary_post_error_requeues_job_and_reverts_meeting_before_exhaustion() {
        let mut store = crate::infrastructure::storage::InMemoryMeetingStore::new();
        store.insert(summarizing_meeting());
        let mut queue = crate::infrastructure::queue::InMemoryJobQueue::new();
        let job = running_summary_job();
        queue.enqueue(job.clone()).expect("enqueue should succeed");

        let exhausted = retry_summary_job_after_posting_failure(
            &mut store,
            &mut queue,
            "m1",
            &job.id,
            "summary posting failed: discord 500".to_owned(),
            2,
        )
        .expect("retry should update meeting");

        assert!(!exhausted);
        let updated = queue.get(&job.id).expect("job should remain");
        assert_eq!(updated.status, crate::domain::JobStatus::Queued);
        assert_eq!(updated.retry_count, 1);
        let meeting = store.get("m1").expect("meeting should remain");
        assert_eq!(meeting.status, crate::domain::MeetingStatus::Stopping);
        assert_eq!(
            meeting.error_message.as_deref(),
            Some("summary posting failed: discord 500")
        );
    }

    #[test]
    fn summary_post_error_marks_job_and_meeting_failed_after_exhaustion() {
        let mut store = crate::infrastructure::storage::InMemoryMeetingStore::new();
        store.insert(summarizing_meeting());
        let mut queue = crate::infrastructure::queue::InMemoryJobQueue::new();
        let job = running_summary_job();
        queue.enqueue(job.clone()).expect("enqueue should succeed");

        let exhausted = retry_summary_job_after_posting_failure(
            &mut store,
            &mut queue,
            "m1",
            &job.id,
            "summary posting failed: discord 500".to_owned(),
            0,
        )
        .expect("retry exhaustion should update meeting");

        assert!(exhausted);
        let updated = queue.get(&job.id).expect("job should remain");
        assert_eq!(updated.status, crate::domain::JobStatus::Failed);
        assert_eq!(updated.retry_count, 1);
        let meeting = store.get("m1").expect("meeting should remain");
        assert_eq!(meeting.status, crate::domain::MeetingStatus::Failed);
        assert_eq!(
            meeting.error_message.as_deref(),
            Some("summary posting failed: discord 500")
        );
    }

    #[test]
    fn transcript_persist_error_requeues_job_and_reverts_meeting_before_exhaustion() {
        let mut store = crate::infrastructure::storage::InMemoryMeetingStore::new();
        store.insert(transcribing_meeting());
        let mut queue = crate::infrastructure::queue::InMemoryJobQueue::new();
        let job = running_summary_job();
        queue.enqueue(job.clone()).expect("enqueue should succeed");

        let exhausted = retry_summary_job_after_transcript_persist_failure(
            &mut store,
            &mut queue,
            "m1",
            &job.id,
            "failed to persist transcript segments: integer out of range".to_owned(),
            2,
        )
        .expect("retry should update meeting");

        assert!(!exhausted);
        let updated = queue.get(&job.id).expect("job should remain");
        assert_eq!(updated.status, crate::domain::JobStatus::Queued);
        assert_eq!(updated.retry_count, 1);
        let meeting = store.get("m1").expect("meeting should remain");
        assert_eq!(meeting.status, crate::domain::MeetingStatus::Stopping);
        assert_eq!(
            meeting.error_message.as_deref(),
            Some("failed to persist transcript segments: integer out of range")
        );
    }

    #[test]
    fn transcript_persist_error_marks_job_and_meeting_failed_after_exhaustion() {
        let mut store = crate::infrastructure::storage::InMemoryMeetingStore::new();
        store.insert(transcribing_meeting());
        let mut queue = crate::infrastructure::queue::InMemoryJobQueue::new();
        let job = running_summary_job();
        queue.enqueue(job.clone()).expect("enqueue should succeed");

        let exhausted = retry_summary_job_after_transcript_persist_failure(
            &mut store,
            &mut queue,
            "m1",
            &job.id,
            "failed to persist transcript segments: integer out of range".to_owned(),
            0,
        )
        .expect("retry exhaustion should update meeting");

        assert!(exhausted);
        let updated = queue.get(&job.id).expect("job should remain");
        assert_eq!(updated.status, crate::domain::JobStatus::Failed);
        assert_eq!(updated.retry_count, 1);
        let meeting = store.get("m1").expect("meeting should remain");
        assert_eq!(meeting.status, crate::domain::MeetingStatus::Failed);
        assert_eq!(
            meeting.error_message.as_deref(),
            Some("failed to persist transcript segments: integer out of range")
        );
    }

    #[test]
    fn transcript_persist_retry_status_update_failure_marks_terminal() {
        let mut store = crate::infrastructure::storage::InMemoryMeetingStore::new();
        let mut meeting = transcribing_meeting();
        meeting.status = crate::domain::MeetingStatus::Posted;
        store.insert(meeting);
        let mut queue = crate::infrastructure::queue::InMemoryJobQueue::new();
        let job = running_summary_job();
        queue.enqueue(job.clone()).expect("enqueue should succeed");

        let exhausted = retry_summary_job_after_transcript_persist_failure(
            &mut store,
            &mut queue,
            "m1",
            &job.id,
            "failed to persist transcript segments: database unavailable".to_owned(),
            2,
        )
        .expect("status restore failure should be terminalized");

        assert!(exhausted);
        let updated = queue.get(&job.id).expect("job should remain");
        assert_eq!(updated.status, crate::domain::JobStatus::Failed);
        assert_eq!(updated.retry_count, 0);
        let meeting = store.get("m1").expect("meeting should remain");
        assert_eq!(meeting.status, crate::domain::MeetingStatus::Failed);
        assert_eq!(
            meeting.error_message.as_deref(),
            Some("failed to persist transcript segments: database unavailable")
        );
    }

    struct UnexpectedRetryStatusQueue;

    impl crate::infrastructure::queue::JobQueue for UnexpectedRetryStatusQueue {
        fn enqueue(
            &mut self,
            _job: crate::infrastructure::queue::Job,
        ) -> Result<(), crate::infrastructure::queue::QueueError> {
            Ok(())
        }

        fn claim_next(
            &mut self,
            _job_type: crate::domain::JobType,
        ) -> Result<
            Option<crate::infrastructure::queue::Job>,
            crate::infrastructure::queue::QueueError,
        > {
            Ok(None)
        }

        fn claim_by_id(
            &mut self,
            _job_id: &str,
        ) -> Result<
            Option<crate::infrastructure::queue::Job>,
            crate::infrastructure::queue::QueueError,
        > {
            Ok(None)
        }

        fn mark_done(
            &mut self,
            _job_id: &str,
        ) -> Result<(), crate::infrastructure::queue::QueueError> {
            Ok(())
        }

        fn mark_failed(
            &mut self,
            _job_id: &str,
            _error_message: String,
        ) -> Result<(), crate::infrastructure::queue::QueueError> {
            Ok(())
        }

        fn retry(
            &mut self,
            _job_id: &str,
            _error_message: String,
            _max_retries: u32,
        ) -> Result<crate::domain::JobStatus, crate::infrastructure::queue::QueueError> {
            Ok(crate::domain::JobStatus::Running)
        }
    }

    #[test]
    fn transcript_persist_unexpected_retry_status_marks_meeting_failed() {
        let mut store = crate::infrastructure::storage::InMemoryMeetingStore::new();
        store.insert(transcribing_meeting());
        let mut queue = UnexpectedRetryStatusQueue;

        let exhausted = retry_summary_job_after_transcript_persist_failure(
            &mut store,
            &mut queue,
            "m1",
            "summary-m1",
            "failed to persist transcript segments: database unavailable".to_owned(),
            2,
        )
        .expect("unexpected retry status should be terminalized");

        assert!(exhausted);
        let meeting = store.get("m1").expect("meeting should remain");
        assert_eq!(meeting.status, crate::domain::MeetingStatus::Failed);
        assert_eq!(
            meeting.error_message.as_deref(),
            Some("failed to persist transcript segments: database unavailable")
        );
    }

    struct BackendRetryQueue;

    impl crate::infrastructure::queue::JobQueue for BackendRetryQueue {
        fn enqueue(
            &mut self,
            _job: crate::infrastructure::queue::Job,
        ) -> Result<(), crate::infrastructure::queue::QueueError> {
            Ok(())
        }

        fn claim_next(
            &mut self,
            _job_type: crate::domain::JobType,
        ) -> Result<
            Option<crate::infrastructure::queue::Job>,
            crate::infrastructure::queue::QueueError,
        > {
            Ok(None)
        }

        fn claim_by_id(
            &mut self,
            _job_id: &str,
        ) -> Result<
            Option<crate::infrastructure::queue::Job>,
            crate::infrastructure::queue::QueueError,
        > {
            Ok(None)
        }

        fn mark_done(
            &mut self,
            _job_id: &str,
        ) -> Result<(), crate::infrastructure::queue::QueueError> {
            Ok(())
        }

        fn mark_failed(
            &mut self,
            _job_id: &str,
            _error_message: String,
        ) -> Result<(), crate::infrastructure::queue::QueueError> {
            Ok(())
        }

        fn retry(
            &mut self,
            _job_id: &str,
            _error_message: String,
            _max_retries: u32,
        ) -> Result<crate::domain::JobStatus, crate::infrastructure::queue::QueueError> {
            Err(crate::infrastructure::queue::QueueError::Backend(
                "database unavailable".to_owned(),
            ))
        }
    }

    #[test]
    fn summary_post_retry_backend_error_leaves_meeting_summarizing() {
        let mut store = crate::infrastructure::storage::InMemoryMeetingStore::new();
        store.insert(summarizing_meeting());
        let mut queue = BackendRetryQueue;

        let exhausted = retry_summary_job_after_posting_failure(
            &mut store,
            &mut queue,
            "m1",
            "summary-m1",
            "summary posting failed: discord 500".to_owned(),
            2,
        )
        .expect("backend retry failure should leave meeting unchanged");

        assert!(!exhausted);
        let meeting = store.get("m1").expect("meeting should remain");
        assert_eq!(meeting.status, crate::domain::MeetingStatus::Summarizing);
        assert_eq!(
            meeting.error_message.as_deref(),
            Some("summary posting failed: discord 500")
        );
    }

    #[test]
    fn summary_post_retry_surfaces_meeting_update_failure() {
        let mut store = crate::infrastructure::storage::InMemoryMeetingStore::new();
        let mut meeting = summarizing_meeting();
        meeting.status = crate::domain::MeetingStatus::Posted;
        store.insert(meeting);
        let mut queue = crate::infrastructure::queue::InMemoryJobQueue::new();
        let job = running_summary_job();
        queue.enqueue(job.clone()).expect("enqueue should succeed");

        let result = retry_summary_job_after_posting_failure(
            &mut store,
            &mut queue,
            "m1",
            &job.id,
            "summary posting failed: discord 500".to_owned(),
            2,
        );

        assert!(result.is_err());
        assert!(
            result
                .expect_err("status update should fail")
                .contains("meeting status update failed")
        );
        let updated = queue.get(&job.id).expect("job should remain");
        assert_eq!(updated.status, crate::domain::JobStatus::Queued);
        let meeting = store.get("m1").expect("meeting should remain");
        assert_eq!(meeting.status, crate::domain::MeetingStatus::Posted);
    }

    #[derive(Default)]
    struct StubMessenger {
        edits: Mutex<Vec<(u64, u64, String)>>,
        sends: Mutex<Vec<(u64, String)>>,
        edit_error: Option<String>,
        send_id: Mutex<u64>,
    }

    #[async_trait]
    impl StatusMessenger for StubMessenger {
        async fn send(&self, channel_id: u64, content: &str) -> Result<u64, String> {
            self.sends
                .lock()
                .unwrap()
                .push((channel_id, content.to_owned()));
            let mut id = self.send_id.lock().unwrap();
            *id += 1;
            Ok(*id)
        }

        async fn edit(
            &self,
            channel_id: u64,
            message_id: u64,
            content: &str,
        ) -> Result<(), String> {
            self.edits
                .lock()
                .unwrap()
                .push((channel_id, message_id, content.to_owned()));
            if let Some(err) = &self.edit_error {
                return Err(err.clone());
            }
            Ok(())
        }
    }

    #[test]
    fn manual_stop_final_flush_failure_blocks_teardown_and_can_retry() {
        assert_final_flush_failure_is_retryable("manual stop");
    }

    #[test]
    fn auto_stop_final_flush_failure_blocks_teardown_and_can_retry() {
        assert_final_flush_failure_is_retryable("auto-stop");
    }

    #[test]
    fn driver_disconnect_final_flush_failure_blocks_teardown_and_can_retry() {
        assert_final_flush_failure_is_retryable("driver disconnect");
    }

    fn start_input_for_runtime_setup_test() -> StartCommandInput {
        StartCommandInput {
            meeting_id: "m1".to_owned(),
            guild_id: "g1".to_owned(),
            user_id: "u1".to_owned(),
            command_channel_id: "c1".to_owned(),
            user_voice_channel_id: Some("vc1".to_owned()),
            permissions: PermissionSet {
                can_connect_voice: true,
                can_send_messages: true,
            },
            caller_role: UserRole::GuildAdmin,
        }
    }

    #[test]
    fn runtime_setup_completion_creates_recording_row() {
        let store = crate::infrastructure::storage::InMemoryMeetingStore::new();
        let mut service = BotCommandService::new(store);

        let result = complete_record_start_after_runtime_setup(
            &mut service,
            start_input_for_runtime_setup_test(),
        )
        .expect("start should succeed after setup");

        assert!(result.contains("meeting_id=m1"));
        let meeting = service
            .store
            .find_active_meeting_by_guild("g1")
            .expect("store lookup should succeed")
            .expect("meeting should exist");
        assert_eq!(meeting.id, "m1");
        assert_eq!(meeting.status, MeetingStatus::Recording);
    }

    #[tokio::test]
    async fn upsert_edits_when_existing_message_available() {
        let messenger = StubMessenger::default();
        let result =
            upsert_status_message_via_messenger(&messenger, "meeting-1", 1, Some(10), "hello")
                .await
                .expect("upsert should succeed");

        assert!(result.is_none());
        assert_eq!(messenger.edits.lock().unwrap().len(), 1);
        assert!(messenger.sends.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn upsert_posts_new_when_edit_fails() {
        let messenger = StubMessenger {
            edit_error: Some("boom".to_owned()),
            ..Default::default()
        };
        let result =
            upsert_status_message_via_messenger(&messenger, "meeting-1", 1, Some(10), "hello")
                .await
                .expect("upsert should succeed");

        assert_eq!(result, Some(1));
        assert_eq!(messenger.edits.lock().unwrap().len(), 1);
        assert_eq!(messenger.sends.lock().unwrap().len(), 1);
    }

    #[test]
    fn mix_chunks_by_wallclock_aligns_speakers_on_shared_timeline() {
        use crate::audio::meeting_audio::LoadedChunk;
        use std::path::PathBuf;

        // User A speaks at t=0..1000ms with amplitude 1000.
        // User B joins late and speaks at t=2000..3000ms with amplitude 2000.
        // Their chunks have independent per-user sequence numbers (both seq=1),
        // so the old mixdown logic collapsed them to the same position.
        let sample_rate = 48_000;
        let samples_per_sec = sample_rate as usize;

        let pcm_a = i16_bytes(&vec![1000i16; samples_per_sec]);
        let pcm_b = i16_bytes(&vec![2000i16; samples_per_sec]);

        let chunks = vec![
            LoadedChunk {
                user_id: "user-a".into(),
                sequence: 1,
                start_ms: 1_000,
                duration_ms: 1_000,
                sample_rate,
                pcm: pcm_a,
                path: PathBuf::from("a.wav"),
            },
            LoadedChunk {
                user_id: "user-b".into(),
                sequence: 1,
                start_ms: 3_000,
                duration_ms: 1_000,
                sample_rate,
                pcm: pcm_b,
                path: PathBuf::from("b.wav"),
            },
        ];

        let mixed = super::mix_chunks_by_wallclock(&chunks, sample_rate);
        let sample_at = |ms: u64| {
            let i = (ms as usize) * samples_per_sec / 1000;
            i16::from_le_bytes([mixed[i * 2], mixed[i * 2 + 1]])
        };

        // meeting_start is 1000ms, so output spans 0..3000ms (3s).
        assert_eq!(mixed.len(), 3 * samples_per_sec * 2);
        assert_eq!(sample_at(500), 1000, "user A audio in first second");
        assert_eq!(sample_at(1500), 0, "silence while no one speaks");
        assert_eq!(sample_at(2500), 2000, "user B audio in third second");
    }

    #[test]
    fn mix_chunks_by_wallclock_preserves_sub_ms_tail_samples() {
        use crate::audio::meeting_audio::LoadedChunk;
        use std::path::PathBuf;

        // 47999 samples @ 48 kHz = 999 ms (floor) — if total_samples were
        // derived from duration_ms, the last 47 samples would be clipped.
        let sample_rate = 48_000;
        let sample_count = 47_999;
        let pcm = i16_bytes(&vec![1234i16; sample_count]);

        let chunks = vec![LoadedChunk {
            user_id: "user-a".into(),
            sequence: 1,
            start_ms: 0,
            duration_ms: 999,
            sample_rate,
            pcm,
            path: PathBuf::from("a.wav"),
        }];

        let mixed = super::mix_chunks_by_wallclock(&chunks, sample_rate);
        assert_eq!(mixed.len(), sample_count * 2);
        let last = i16::from_le_bytes([mixed[mixed.len() - 2], mixed[mixed.len() - 1]]);
        assert_eq!(last, 1234, "tail sample preserved despite ms rounding");
    }

    fn i16_bytes(samples: &[i16]) -> Vec<u8> {
        let mut out = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    #[test]
    fn summary_completion_message_includes_url() {
        let message = format_status_message_content(
            "meeting-1",
            &StatusMessageUpdate::SummaryCompleted {
                summary_url: Some("https://example.test/meetings/meeting-1".to_owned()),
            },
        );
        assert!(message.contains("https://example.test/meetings/meeting-1"));
        assert!(message.contains("✅"));
    }
}
