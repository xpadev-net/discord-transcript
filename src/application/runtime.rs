use crate::application::auto_stop::{AutoStopSignal, AutoStopState};
use crate::application::bot::{
    BotCommandService, StartCommandInput, StopCommandInput, StopCommandResult,
};
use crate::application::command::{
    CommandError, PermissionSet, RecordStartPreflight, RecordStartRequest,
    authorize_record_stop_for_meeting, record_start_after_preflight,
    validate_record_start_preconditions,
};
use crate::application::recovery_runner::{RecoveryEffect, run_recovery};
use crate::application::retention_cleanup::{
    apply_retention_database_cleanup, apply_retention_filesystem_cleanup,
    collect_retention_cleanup_plan,
};
use crate::application::stop::StopOutcome;
use crate::application::summary::ClaudeSummaryClient;
use crate::application::worker::enqueue_summary_job;
use crate::audio::meeting_audio::{
    ProcessedAudioChunk, build_speaker_audio_inputs,
    build_speaker_audio_inputs_excluding_processed_chunks, compute_meeting_start_ms, load_chunks,
};
use crate::audio::receiver::ReceiverConfig;
use crate::audio::recording_session::{PersistedChunk, RecordingSession};
use crate::audio::songbird_adapter::{AdaptedVoiceFrames, SsrcTracker, adapt_voice_tick};
use crate::bootstrap::config::{AppConfig, SummaryHarness};
use crate::domain::authz::UserRole;
use crate::domain::recovery::RecoveryCandidate;
use crate::domain::speaker::SpeakerProfile;
use crate::domain::transcript::{
    MAX_DB_TIMESTAMP_MS, NormalizationConfig, TranscriptSegment, TranscriptSource,
    normalize_segments, ordered_transcript_segments, render_for_summary, sort_transcript_segments,
};
use crate::domain::usage::{
    EntitlementAction, EntitlementEvaluator, NewUsageEvent, UsageDetailJson, UsageMetric,
    UsageSnapshot, recording_minutes_from_seconds,
};
use crate::domain::{MeetingStatus, StopReason};
use crate::infrastructure::asr::{WhisperClient, WhisperInferenceRequest};
use crate::infrastructure::integrations::{
    CommandWhisperClient, DEFAULT_COMMAND_TIMEOUT, HarnessCliSummaryClient,
};
use crate::infrastructure::queue::{Job, JobQueue};
use crate::infrastructure::retry::RetryPolicy;
use crate::infrastructure::sql::{
    HEARTBEAT_RUNNING_JOB_SQL, RECOVERY_REQUEUE_STALE_RUNNING_SUMMARY_JOB_SQL, RECOVERY_SCAN_SQL,
    RECOVERY_SUMMARY_JOB_STATUS_SQL,
};
use crate::infrastructure::sql_store::{PgSqlExecutor, SqlExecutor, SqlJobQueue, SqlMeetingStore};
use crate::infrastructure::storage::{
    EffectiveMeetingSettings, MeetingSettingsDefaults, MeetingStore, StatusMessageMetadata,
    StoreError, StoredMeeting,
};
use crate::infrastructure::storage_fs::{ChunkStorage, LocalChunkStorage};
use crate::interfaces::posting::{DISCORD_MESSAGE_LIMIT, split_discord_message};
use crate::interfaces::vc_text::{fetch_vc_text_messages, warn_and_fallback_on_vc_text_error};
use chrono::Utc;
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
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, RwLockWriteGuard, Semaphore, watch};
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{debug, error, info, warn};

pub const RECORD_START_COMMAND: &str = "record-start";
pub const RECORD_STOP_COMMAND: &str = "record-stop";
const FINAL_FLUSH_MAX_RETRIES: u32 = 10;
const AUTO_STOP_GRACE_MAX_CACHE_MISS_CHECKS: u32 = 10;
const DRIVER_DISCONNECT_GRACE_MAX_CACHE_MISS_CHECKS: u32 = 10;
const RECORDING_STOP_MAX_RETRIES: u32 = 10;
const RECORDING_LOOKUP_MAX_RETRIES: u32 = 10;
const RECORDING_TERMINAL_CLEANUP_MAX_RETRIES: u32 = 3;
const RECORDING_START_CLEANUP_MAX_RETRIES: u32 = 3;
const RECORDING_START_CLEANUP_RETRY_DELAY: Duration = Duration::from_secs(1);
const SHUTDOWN_GRACE_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_VOICE_LEAVE_TIMEOUT: Duration = Duration::from_secs(5);
const RECORDING_VOICE_JOIN_TIMEOUT: Duration = Duration::from_secs(10);

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

#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeCommandInput {
    RecordStart(Box<StartCommandInput>),
    RecordStop {
        guild_id: String,
        user_id: String,
        caller_role: UserRole,
        reason: StopReason,
    },
}

pub fn dispatch_runtime_command<S>(
    service: &mut BotCommandService<S>,
    input: RuntimeCommandInput,
) -> Result<String, CommandError>
where
    S: MeetingStore,
{
    match input {
        RuntimeCommandInput::RecordStart(value) => service.handle_record_start(*value),
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
    preflight: RecordStartPreflight,
) -> Result<String, String>
where
    S: MeetingStore,
{
    let result = record_start_after_preflight(
        &mut service.store,
        RecordStartRequest {
            meeting_id: input.meeting_id,
            guild_id: input.guild_id,
            started_by_user_id: input.user_id,
            command_channel_id: input.command_channel_id,
            user_voice_channel_id: input.user_voice_channel_id,
            permissions: input.permissions,
            caller_role: input.caller_role,
            effective_settings: input.effective_settings,
        },
        preflight,
    )
    .map_err(|err| err.to_string())?;

    Ok(format!(
        "録音を開始しました: meeting_id={}, vc={}, report_channel={}",
        result.meeting_id, result.voice_channel_id, result.report_channel_id
    ))
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
    stop_and_enqueue_summary_job_for_teardown(
        service,
        queue,
        guild_id,
        user_id,
        caller_role,
        expected_meeting_id,
        reason,
    )
    .map_err(|err| err.to_string())
}

fn stop_and_enqueue_summary_job_for_teardown<S, Q>(
    service: &mut BotCommandService<S>,
    queue: &mut Q,
    guild_id: &str,
    user_id: &str,
    caller_role: UserRole,
    expected_meeting_id: Option<&str>,
    reason: StopReason,
) -> Result<crate::application::bot::StopCommandResult, TeardownStopError>
where
    S: MeetingStore,
    Q: crate::infrastructure::queue::JobQueue,
{
    if let Some(expected_meeting_id) = expected_meeting_id {
        let active = service
            .store
            .find_active_meeting_by_guild(guild_id)
            .map_err(|err| TeardownStopError::Other(err.to_string()))?
            .ok_or_else(|| {
                TeardownStopError::TargetAbsent(CommandError::NoActiveMeeting.to_string())
            })?;
        if active.id != expected_meeting_id {
            return Err(TeardownStopError::TargetAbsent(format!(
                "active meeting changed before stop: expected={expected_meeting_id}, actual={}",
                active.id
            )));
        }
    }

    let stop_result = service
        .handle_record_stop_result(StopCommandInput {
            guild_id: guild_id.to_owned(),
            user_id: user_id.to_owned(),
            caller_role,
            reason,
        })
        .map_err(|err| match err {
            CommandError::NoActiveMeeting => TeardownStopError::TargetAbsent(err.to_string()),
            err => TeardownStopError::Other(err.to_string()),
        })?;

    if stop_result.outcome == StopOutcome::Owner {
        record_recording_duration_usage(&mut service.store, &stop_result.meeting_id);
    }

    let should_enqueue = match stop_result.outcome {
        StopOutcome::Owner => true,
        StopOutcome::AlreadyHandled => service
            .store
            .get_meeting(&stop_result.meeting_id)
            .map_err(|err| TeardownStopError::Other(err.to_string()))?
            .is_some_and(|meeting| meeting.status == MeetingStatus::Stopping),
    };

    if should_enqueue {
        if service
            .store
            .get_effective_meeting_settings(&stop_result.meeting_id)
            .map_err(|err| TeardownStopError::Other(err.to_string()))?
            .is_some_and(|settings| !settings.summary_enabled)
        {
            info!(
                meeting_id = %stop_result.meeting_id,
                "summary job not enqueued because meeting snapshot disabled summaries"
            );
            return Ok(stop_result);
        }
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
            Err(err) => return Err(TeardownStopError::Other(err.to_string())),
        }
    }

    Ok(stop_result)
}

fn record_recording_duration_usage<S: MeetingStore>(store: &mut S, meeting_id: &str) {
    let meeting = match store.get_meeting(meeting_id) {
        Ok(Some(meeting)) => meeting,
        Ok(None) => {
            warn!(meeting_id, "meeting missing while recording usage duration");
            return;
        }
        Err(err) => {
            warn!(
                meeting_id,
                error = %err,
                "failed to load meeting for recording usage duration"
            );
            return;
        }
    };
    let Some(duration_seconds) = recording_duration_seconds(&meeting) else {
        warn!(
            meeting_id,
            started_at = %meeting.started_at.is_some(),
            stopped_at = %meeting.stopped_at.is_some(),
            "skipping recording usage duration because timestamps are unavailable"
        );
        return;
    };
    let event = NewUsageEvent {
        id: format!("usage:recording_minutes:{meeting_id}"),
        tenant_id: None,
        guild_id: meeting.guild_id,
        meeting_id: Some(meeting_id.to_owned()),
        job_id: None,
        resource_type: Some("meeting".to_owned()),
        resource_id: Some(meeting_id.to_owned()),
        metric: UsageMetric::RecordingMinutes,
        quantity: recording_minutes_from_seconds(duration_seconds),
        detail_json: UsageDetailJson::new(serde_json::json!({
            "duration_seconds": duration_seconds
        }))
        .expect("usage detail must be a JSON object"),
        observed_at: Utc::now(),
    };
    if let Err(err) = store.append_usage_event(&event) {
        warn!(
            meeting_id,
            usage_event_id = %event.id,
            error = %err,
            "failed to append recording usage event; continuing in observe-only mode"
        );
    }
}

fn recording_duration_seconds(meeting: &StoredMeeting) -> Option<u64> {
    let started_at = meeting.started_at?;
    let stopped_at = meeting.stopped_at?;
    let seconds = stopped_at.signed_duration_since(started_at).num_seconds();
    Some(seconds.max(0) as u64)
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

fn flush_removed_session_after_stop<S: ChunkStorage>(
    session: &mut RecordingSession<S>,
    _guild_id: &str,
    _phase: &str,
) -> Result<(), String> {
    match session.flush_all() {
        Ok(result) if result.failed.is_empty() => Ok(()),
        Ok(result) => Err(format!(
            "failed to persist {} tail audio chunk(s)",
            result.failed.len()
        )),
        Err(err) => Err(err.to_string()),
    }
}

fn warn_removed_session_flush_failure(guild_id: &str, phase: &str, err: &str) {
    warn!(
            guild_id = %guild_id,
            error = %err,
            phase,
            "failed to flush tail audio after recording stop; dropping removed session"
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GraceExpiryDecision {
    Stop,
    Cancel,
    Reschedule,
}

fn decide_auto_stop_grace_expiry(non_bot_member_count: Option<usize>) -> GraceExpiryDecision {
    match non_bot_member_count {
        None => GraceExpiryDecision::Reschedule,
        Some(0) => GraceExpiryDecision::Stop,
        Some(_) => GraceExpiryDecision::Cancel,
    }
}

fn decide_driver_disconnect_grace_expiry(
    reconnected: Option<bool>,
    non_bot_member_count: Option<usize>,
) -> GraceExpiryDecision {
    match (reconnected, non_bot_member_count) {
        (Some(false), Some(0)) => GraceExpiryDecision::Stop,
        (Some(true), _) => GraceExpiryDecision::Cancel,
        // Bot is still disconnected after grace, but members are present. Do
        // not auto-stop an occupied recording; a later empty-channel grace or
        // manual stop can end it.
        (Some(false), Some(_)) => GraceExpiryDecision::Cancel,
        _ => GraceExpiryDecision::Reschedule,
    }
}

fn voice_state_cache_miss_terminal_error(
    cache_misses: &mut u32,
    max_cache_miss_checks: u32,
    context: &str,
) -> Option<String> {
    *cache_misses = cache_misses.saturating_add(1);
    if *cache_misses < max_cache_miss_checks {
        return None;
    }
    Some(format!(
        "voice state cache remained unavailable after {} {context} stop check(s)",
        *cache_misses,
    ))
}

fn driver_disconnect_cache_miss_terminal_error(cache_misses: &mut u32) -> Option<String> {
    voice_state_cache_miss_terminal_error(
        cache_misses,
        DRIVER_DISCONNECT_GRACE_MAX_CACHE_MISS_CHECKS,
        "driver-disconnect",
    )
}

fn auto_stop_cache_miss_terminal_error(cache_misses: &mut u32) -> Option<String> {
    voice_state_cache_miss_terminal_error(
        cache_misses,
        AUTO_STOP_GRACE_MAX_CACHE_MISS_CHECKS,
        "auto-stop grace",
    )
}

fn recording_stop_terminal_error(
    stop_failures: &mut u32,
    phase: &str,
    err: &str,
) -> Option<String> {
    *stop_failures = stop_failures.saturating_add(1);
    if *stop_failures < RECORDING_STOP_MAX_RETRIES {
        return None;
    }
    Some(format!(
        "recording stop failed after {} {phase} attempt(s): {err}",
        *stop_failures,
    ))
}

fn recording_lookup_terminal_error(
    lookup_failures: &mut u32,
    phase: &str,
    err: &str,
) -> Option<String> {
    *lookup_failures = lookup_failures.saturating_add(1);
    if *lookup_failures < RECORDING_LOOKUP_MAX_RETRIES {
        return None;
    }
    Some(format!(
        "recording state lookup failed after {} {phase} check(s): {err}",
        *lookup_failures,
    ))
}

fn reset_recording_lookup_failures(lookup_failures: &mut u32) {
    *lookup_failures = 0;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordingStartJoinVerification {
    Active,
    AlreadyStopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VoiceJoinRetryCleanup {
    RetryCurrentSession,
    StopAfterSessionRemoved,
    StopAfterSessionReplaced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailedRecordingStartLocalCleanup {
    StartupOnly,
    FullRuntimeState,
}

fn classify_voice_join_retry_cleanup(
    current_session_meeting_id: Option<&str>,
    expected_meeting_id: &str,
) -> VoiceJoinRetryCleanup {
    match current_session_meeting_id {
        Some(current) if current == expected_meeting_id => {
            VoiceJoinRetryCleanup::RetryCurrentSession
        }
        Some(_) => VoiceJoinRetryCleanup::StopAfterSessionReplaced,
        None => VoiceJoinRetryCleanup::StopAfterSessionRemoved,
    }
}

fn voice_join_retry_cleanup_leave_phase(cleanup: VoiceJoinRetryCleanup) -> Option<&'static str> {
    match cleanup {
        VoiceJoinRetryCleanup::RetryCurrentSession => Some("record-start retry cleanup"),
        VoiceJoinRetryCleanup::StopAfterSessionRemoved => {
            Some("record-start retry cleanup after stop")
        }
        VoiceJoinRetryCleanup::StopAfterSessionReplaced => None,
    }
}

// Join completion can race with stop or teardown exhaustion after the DB
// row/session were created but before Songbird reports success. Treat those
// downstream terminal states as benign so cleanup stays conditional on local
// session ownership instead of falling through to the generic failed-start path.
fn recording_start_join_completed_after_stop(status: MeetingStatus) -> bool {
    matches!(
        status,
        MeetingStatus::Stopping
            | MeetingStatus::Transcribing
            | MeetingStatus::Summarizing
            | MeetingStatus::Posted
            | MeetingStatus::Failed
    )
}

fn mark_recording_failed_after_teardown_exhaustion<S: MeetingStore>(
    store: &mut S,
    meeting_id: &str,
    error_message: &str,
) -> Result<(), StoreError> {
    match store.set_meeting_status(
        meeting_id,
        MeetingStatus::Failed,
        Some(MeetingStatus::Recording),
    ) {
        Ok(()) => {}
        Err(StoreError::NotFound { .. }) => {
            debug!(
                meeting_id,
                "meeting not found during teardown exhaustion; treating as already handled"
            );
            return Ok(());
        }
        Err(StoreError::CasConflict { .. }) => {
            match store.set_meeting_status(
                meeting_id,
                MeetingStatus::Failed,
                Some(MeetingStatus::Stopping),
            ) {
                Ok(()) => {}
                Err(StoreError::NotFound { .. }) => {
                    debug!(
                        meeting_id,
                        "meeting not found during teardown exhaustion; treating as already handled"
                    );
                    return Ok(());
                }
                Err(StoreError::CasConflict { .. }) => {
                    if let Some(meeting) = store.get_meeting(meeting_id)? {
                        if matches!(
                            meeting.status,
                            MeetingStatus::Recording | MeetingStatus::Stopping
                        ) {
                            warn!(
                                meeting_id,
                                status = ?meeting.status,
                                "forcing recording failure after repeated teardown status CAS conflicts"
                            );
                            store.set_meeting_status(meeting_id, MeetingStatus::Failed, None)?;
                        } else {
                            debug!(
                                meeting_id,
                                status = ?meeting.status,
                                "teardown exhaustion reached meeting that is no longer recording or stopping"
                            );
                            return Ok(());
                        }
                    } else {
                        debug!(
                            meeting_id,
                            "meeting not found during teardown exhaustion; treating as already handled"
                        );
                        return Ok(());
                    }
                }
                Err(err) => return Err(err),
            }
        }
        Err(err) => return Err(err),
    }
    if let Err(err) = store.set_error_message(meeting_id, Some(error_message.to_owned())) {
        warn!(
            meeting_id,
            error = %err,
            "failed to persist teardown exhaustion error message after marking recording failed"
        );
    }
    Ok(())
}

fn mark_recording_start_failed_after_setup_error<S: MeetingStore>(
    store: &mut S,
    meeting_id: &str,
    error_message: &str,
) -> Result<(), StoreError> {
    match store.set_meeting_status(
        meeting_id,
        MeetingStatus::Failed,
        Some(MeetingStatus::Recording),
    ) {
        Ok(()) => {
            if let Err(err) = store.set_error_message(meeting_id, Some(error_message.to_owned())) {
                warn!(
                    meeting_id,
                    error = %err,
                    "failed to persist record-start setup error"
                );
            }
            Ok(())
        }
        Err(StoreError::CasConflict { .. } | StoreError::NotFound { .. }) => {
            debug!(
                meeting_id,
                "record-start setup cleanup found meeting already transitioned; skipping forced failure"
            );
            Ok(())
        }
        Err(err) => Err(err),
    }
}

fn best_effort_mark_recording_start_failed_after_cleanup_retry_exhaustion<S: MeetingStore>(
    store: &mut S,
    meeting_id: &str,
    error_message: &str,
) -> bool {
    match store.set_meeting_status(meeting_id, MeetingStatus::Failed, None) {
        Ok(()) => {
            if let Err(err) = store.set_error_message(meeting_id, Some(error_message.to_owned())) {
                warn!(
                    meeting_id,
                    error = %err,
                    "failed to persist record-start cleanup exhaustion error message"
                );
            }
            true
        }
        Err(err) => {
            warn!(
                meeting_id,
                error = %err,
                "failed to force record-start cleanup exhaustion status"
            );
            false
        }
    }
}

fn recording_startup_conflict(
    startups: &HashMap<String, String>,
    guild_key: &str,
) -> Option<CommandError> {
    startups
        .get(guild_key)
        .map(|meeting_id| CommandError::ActiveMeetingExists {
            meeting_id: meeting_id.clone(),
        })
}

fn clear_matching_recording_startup(
    startups: &mut HashMap<String, String>,
    guild_key: &str,
    meeting_id: &str,
) {
    if startups
        .get(guild_key)
        .is_some_and(|current| current == meeting_id)
    {
        startups.remove(guild_key);
    }
}

fn remove_auto_stop_state_for_meeting(
    states: &mut HashMap<String, AutoStopState>,
    guild_key: &str,
    meeting_id: &str,
) {
    if states
        .get(guild_key)
        .is_some_and(|state| state.should_remove_for_meeting_cleanup(meeting_id))
    {
        states.remove(guild_key);
    }
}

fn remove_auto_stop_state_for_failed_recording_start_cleanup(
    states: &mut HashMap<String, AutoStopState>,
    guild_key: &str,
    meeting_id: &str,
    cleanup_scope: FailedRecordingStartLocalCleanup,
) {
    if cleanup_scope == FailedRecordingStartLocalCleanup::FullRuntimeState {
        remove_auto_stop_state_for_meeting(states, guild_key, meeting_id);
    }
}

fn rearm_auto_stop_state_for_retry(
    states: &mut HashMap<String, AutoStopState>,
    guild_key: &str,
    expected_meeting_id: &str,
) -> bool {
    if let Some(state) = states
        .get_mut(guild_key)
        .filter(|state| state.belongs_to_meeting(expected_meeting_id))
    {
        state.retry_after_failed_stop();
        true
    } else {
        false
    }
}

fn remove_matching_recording_session_for_meeting<S: ChunkStorage>(
    sessions: &mut HashMap<String, RecordingSession<S>>,
    guild_key: &str,
    expected_meeting_id: &str,
) -> Option<RecordingSession<S>> {
    if sessions
        .get(guild_key)
        .is_some_and(|session| session.meeting_id == expected_meeting_id)
    {
        sessions.remove(guild_key)
    } else {
        None
    }
}

fn clear_local_recording_state_maps_after_terminal_absence(
    auto_stop_states: &mut HashMap<String, AutoStopState>,
    live_transcription_titles: &mut HashMap<String, Option<String>>,
    recording_startups: &mut HashMap<String, String>,
    guild_key: &str,
    expected_meeting_id: &str,
) {
    remove_auto_stop_state_for_meeting(auto_stop_states, guild_key, expected_meeting_id);
    live_transcription_titles.remove(expected_meeting_id);
    clear_matching_recording_startup(recording_startups, guild_key, expected_meeting_id);
}

async fn remove_local_recording_state_after_terminal_absence_with_dependencies<
    C: ChunkStorage + Send,
>(
    local: &RecordingLifecycleLocalState<'_, C>,
    guild_key: &str,
    expected_meeting_id: &str,
) -> Option<RecordingSession<C>> {
    let removed_session = {
        let _voice_event_guard = local.voice_event_gate.write().await;
        let mut sessions = local.sessions.lock().await;
        remove_matching_recording_session_for_meeting(&mut sessions, guild_key, expected_meeting_id)
    };
    {
        let mut states = local.auto_stop_states.lock().await;
        let mut titles = local.live_transcription_titles.lock().await;
        let mut startups = local.recording_startups.lock().await;
        clear_local_recording_state_maps_after_terminal_absence(
            &mut states,
            &mut titles,
            &mut startups,
            guild_key,
            expected_meeting_id,
        );
    }
    removed_session
}

fn clear_auto_stop_timer_generation_for_meeting(
    states: &mut HashMap<String, AutoStopState>,
    guild_key: &str,
    meeting_id: &str,
    timer_generation: u64,
) {
    if let Some(state) = states.get_mut(guild_key)
        && state.belongs_to_meeting(meeting_id)
    {
        state.clear_timer_active_for_generation(timer_generation);
    }
}

fn terminal_cleanup_retry_reached_limit(terminal_cleanup_failures: &mut u32) -> bool {
    *terminal_cleanup_failures = (*terminal_cleanup_failures).saturating_add(1);
    *terminal_cleanup_failures >= RECORDING_TERMINAL_CLEANUP_MAX_RETRIES
}

fn restart_terminal_cleanup_retry_window_after_persistence_failure(
    terminal_cleanup_failures: &mut u32,
) {
    *terminal_cleanup_failures = 0;
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TerminalCleanupRetryOutcome {
    attempts: u32,
    terminal_error: Option<String>,
}

fn record_terminal_cleanup_retry_failure(
    terminal_cleanup_failures: &mut u32,
    phase: &str,
    err: &str,
) -> TerminalCleanupRetryOutcome {
    let reached_limit = terminal_cleanup_retry_reached_limit(terminal_cleanup_failures);
    let terminal_error = reached_limit.then(|| {
        format!(
            "terminal recording cleanup failed after {} attempt(s) during {}: {}",
            *terminal_cleanup_failures, phase, err
        )
    });

    TerminalCleanupRetryOutcome {
        attempts: *terminal_cleanup_failures,
        terminal_error,
    }
}

fn persist_terminal_cleanup_retry_exhaustion<S: MeetingStore>(
    store: &mut S,
    expected_meeting_id: &str,
    terminal_error: &str,
) -> Result<(), StoreError> {
    mark_recording_failed_after_teardown_exhaustion(store, expected_meeting_id, terminal_error)
}

#[async_trait]
trait RecordingVoiceGateway {
    async fn leave_recording_voice(&self, guild_id: GuildId) -> Result<(), String>;
}

#[async_trait]
impl RecordingVoiceGateway for songbird::Songbird {
    async fn leave_recording_voice(&self, guild_id: GuildId) -> Result<(), String> {
        self.leave(guild_id).await.map_err(|err| err.to_string())
    }
}

#[async_trait]
trait RecordingVoiceLeaveDependency {
    async fn leave_recording_voice_with_timeout(
        &self,
        guild_id: GuildId,
        meeting_id: &str,
        phase: &str,
    ) -> Option<RecordingVoiceLeaveOutcome>;
}

struct ContextRecordingVoiceLeave<'a> {
    ctx: &'a Context,
}

#[async_trait]
impl RecordingVoiceLeaveDependency for ContextRecordingVoiceLeave<'_> {
    async fn leave_recording_voice_with_timeout(
        &self,
        guild_id: GuildId,
        meeting_id: &str,
        phase: &str,
    ) -> Option<RecordingVoiceLeaveOutcome> {
        let manager = songbird::get(self.ctx).await?;
        Some(leave_voice_with_timeout(manager.as_ref(), guild_id, meeting_id, phase).await)
    }
}

#[async_trait]
impl<V> RecordingVoiceLeaveDependency for Option<&V>
where
    V: RecordingVoiceGateway + Sync + ?Sized,
{
    async fn leave_recording_voice_with_timeout(
        &self,
        guild_id: GuildId,
        meeting_id: &str,
        phase: &str,
    ) -> Option<RecordingVoiceLeaveOutcome> {
        match self {
            Some(voice) => {
                Some(leave_voice_with_timeout(*voice, guild_id, meeting_id, phase).await)
            }
            None => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordingVoiceLeaveOutcome {
    Succeeded,
    Failed,
    TimedOut,
}

async fn leave_voice_with_timeout<V: RecordingVoiceGateway + Sync + ?Sized>(
    manager: &V,
    guild_id: GuildId,
    meeting_id: &str,
    phase: &str,
) -> RecordingVoiceLeaveOutcome {
    match timeout(
        SHUTDOWN_VOICE_LEAVE_TIMEOUT,
        manager.leave_recording_voice(guild_id),
    )
    .await
    {
        Ok(Ok(())) => RecordingVoiceLeaveOutcome::Succeeded,
        Ok(Err(err)) => {
            warn!(
                guild_id = %guild_id.get(),
                meeting_id,
                error = %err,
                phase,
                "failed to leave voice channel during recording lifecycle teardown"
            );
            RecordingVoiceLeaveOutcome::Failed
        }
        Err(_) => {
            warn!(
                guild_id = %guild_id.get(),
                meeting_id,
                phase,
                timeout_ms = SHUTDOWN_VOICE_LEAVE_TIMEOUT.as_millis(),
                "timed out leaving voice channel during recording lifecycle teardown"
            );
            RecordingVoiceLeaveOutcome::TimedOut
        }
    }
}

struct RecordingStopTeardownRequest<'a> {
    guild_key: &'a str,
    caller_user_id: &'a str,
    caller_role: UserRole,
    expected_meeting_id: &'a str,
    reason: StopReason,
    phase: &'a str,
}

struct RecordingLookupFailureRequest<'a> {
    guild_id: GuildId,
    guild_key: &'a str,
    expected_meeting_id: &'a str,
    terminal_error: &'a str,
    context: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActiveMeetingVoiceChannel {
    meeting_id: String,
    voice_channel_id: u64,
}

#[derive(Clone, Copy)]
struct TerminalCleanupRetryFailureRequest<'a> {
    guild_key: &'a str,
    expected_meeting_id: &'a str,
    phase: &'a str,
    err: &'a str,
}

struct TerminalAbsenceCleanupRequest<'a> {
    guild_id: GuildId,
    guild_key: &'a str,
    expected_meeting_id: &'a str,
    phase: &'a str,
}

struct RecordingLifecycleLocalState<'a, C: ChunkStorage> {
    sessions: &'a Arc<Mutex<HashMap<String, RecordingSession<C>>>>,
    auto_stop_states: &'a Arc<Mutex<HashMap<String, AutoStopState>>>,
    live_transcription_titles: &'a Arc<Mutex<HashMap<String, Option<String>>>>,
    recording_startups: &'a Arc<Mutex<HashMap<String, String>>>,
    voice_event_gate: &'a Arc<RwLock<()>>,
    ssrc_tracker: &'a Arc<Mutex<SsrcTracker>>,
    ssrc_tracker_reset_gate: &'a Arc<Mutex<()>>,
}

enum TerminalCleanupRetryDecision<C: ChunkStorage> {
    Reschedule,
    Cleared {
        removed_session: Box<Option<RecordingSession<C>>>,
    },
}

#[must_use]
struct RecordingLifecycleWritePermit<'a> {
    _guard: RwLockWriteGuard<'a, ()>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TeardownStopError {
    TargetAbsent(String),
    Other(String),
}

impl TeardownStopError {
    fn is_target_absent(&self) -> bool {
        matches!(self, Self::TargetAbsent(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RecordingTeardownError {
    FinalFlush(String),
    Stop(TeardownStopError),
}

async fn recording_lifecycle_write_permit_for_gate(
    command_gate: &RwLock<()>,
) -> RecordingLifecycleWritePermit<'_> {
    RecordingLifecycleWritePermit {
        _guard: command_gate.write().await,
    }
}

impl Display for TeardownStopError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TargetAbsent(err) | Self::Other(err) => write!(f, "{err}"),
        }
    }
}

impl Display for RecordingTeardownError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FinalFlush(err) => write!(f, "{err}"),
            Self::Stop(err) => write!(f, "{err}"),
        }
    }
}

async fn finish_terminal_absence_cleanup_with_dependencies<
    C: ChunkStorage + Send,
    L: RecordingVoiceLeaveDependency + Sync,
    P: Fn(&RecordingSession<C>, &SsrcTracker) + Send + Sync,
>(
    local: &RecordingLifecycleLocalState<'_, C>,
    voice_leave: &L,
    request: TerminalAbsenceCleanupRequest<'_>,
    mut removed_session: Option<RecordingSession<C>>,
    persist_ssrc_mapping: P,
) -> Option<RecordingVoiceLeaveOutcome> {
    let _reset_guard = local.ssrc_tracker_reset_gate.lock().await;
    let leave_outcome = if removed_session.is_some() {
        voice_leave
            .leave_recording_voice_with_timeout(
                request.guild_id,
                request.expected_meeting_id,
                request.phase,
            )
            .await
    } else {
        None
    };
    if let Some(session) = removed_session.as_mut()
        && let Err(err) =
            flush_removed_session_after_stop(session, request.guild_key, request.phase)
    {
        warn_removed_session_flush_failure(request.guild_key, request.phase, &err);
    }
    let latest_tracker = {
        let _voice_event_guard = local.voice_event_gate.write().await;
        let tracker = local.ssrc_tracker.lock().await;
        tracker.clone()
    };
    if let Some(session) = &removed_session {
        persist_ssrc_mapping(session, &latest_tracker);
    }
    leave_outcome
}

async fn clear_failed_recording_start_local_state_with_dependencies<
    C: ChunkStorage + Send,
    P: Fn(&RecordingSession<C>, &SsrcTracker) + Send + Sync,
>(
    local: &RecordingLifecycleLocalState<'_, C>,
    guild_key: &str,
    meeting_id: &str,
    cleanup_scope: FailedRecordingStartLocalCleanup,
    persist_ssrc_mapping: P,
) {
    {
        let mut startups = local.recording_startups.lock().await;
        clear_matching_recording_startup(&mut startups, guild_key, meeting_id);
    }
    if cleanup_scope == FailedRecordingStartLocalCleanup::StartupOnly {
        return;
    }

    let (removed_session, latest_tracker) = {
        let _voice_event_guard = local.voice_event_gate.write().await;
        let mut removed_session = {
            let mut sessions = local.sessions.lock().await;
            remove_matching_recording_session_for_meeting(&mut sessions, guild_key, meeting_id)
        };
        if let Some(session) = removed_session.as_mut()
            && let Err(err) =
                flush_removed_session_after_stop(session, guild_key, "record-start failure cleanup")
        {
            warn_removed_session_flush_failure(guild_key, "record-start failure cleanup", &err);
        }
        let tracker = local.ssrc_tracker.lock().await;
        (removed_session, tracker.clone())
    };
    if let Some(session) = &removed_session {
        persist_ssrc_mapping(session, &latest_tracker);
    }

    {
        let mut states = local.auto_stop_states.lock().await;
        remove_auto_stop_state_for_failed_recording_start_cleanup(
            &mut states,
            guild_key,
            meeting_id,
            cleanup_scope,
        );
    }
    {
        let mut titles = local.live_transcription_titles.lock().await;
        titles.remove(meeting_id);
    }
}

async fn try_cleanup_failed_recording_start_with_dependencies<S, C, P>(
    service: &Arc<Mutex<BotCommandService<S>>>,
    local: &RecordingLifecycleLocalState<'_, C>,
    guild_key: &str,
    meeting_id: &str,
    error_message: &str,
    cleanup_scope: FailedRecordingStartLocalCleanup,
    persist_ssrc_mapping: P,
) -> bool
where
    S: MeetingStore + Send,
    C: ChunkStorage + Send,
    P: Fn(&RecordingSession<C>, &SsrcTracker) + Send + Sync,
{
    {
        let mut service = service.lock().await;
        match mark_recording_start_failed_after_setup_error(
            &mut service.store,
            meeting_id,
            error_message,
        ) {
            Ok(()) => {
                debug!(meeting_id, "record-start setup failure cleanup completed");
            }
            Err(err) => {
                error!(
                    meeting_id,
                    error = %err,
                    "failed to mark meeting as failed after voice join error; preserving local recording setup state for retry"
                );
                return false;
            }
        }
    }
    clear_failed_recording_start_local_state_with_dependencies(
        local,
        guild_key,
        meeting_id,
        cleanup_scope,
        persist_ssrc_mapping,
    )
    .await;
    true
}

async fn finish_failed_recording_start_cleanup_retry_exhaustion_with_dependencies<S, C, P>(
    service: &Arc<Mutex<BotCommandService<S>>>,
    local: &RecordingLifecycleLocalState<'_, C>,
    guild_key: &str,
    meeting_id: &str,
    error_message: &str,
    cleanup_scope: FailedRecordingStartLocalCleanup,
    persist_ssrc_mapping: P,
) where
    S: MeetingStore + Send,
    C: ChunkStorage + Send,
    P: Fn(&RecordingSession<C>, &SsrcTracker) + Send + Sync,
{
    {
        let mut service = service.lock().await;
        best_effort_mark_recording_start_failed_after_cleanup_retry_exhaustion(
            &mut service.store,
            meeting_id,
            error_message,
        );
    }
    clear_failed_recording_start_local_state_with_dependencies(
        local,
        guild_key,
        meeting_id,
        cleanup_scope,
        persist_ssrc_mapping,
    )
    .await;
}

async fn handle_terminal_cleanup_retry_failure_with_dependencies<S, C>(
    service: &Arc<Mutex<BotCommandService<S>>>,
    local: &RecordingLifecycleLocalState<'_, C>,
    request: TerminalCleanupRetryFailureRequest<'_>,
    terminal_cleanup_failures: &mut u32,
) -> TerminalCleanupRetryDecision<C>
where
    S: MeetingStore + Send,
    C: ChunkStorage + Send,
{
    let outcome = record_terminal_cleanup_retry_failure(
        terminal_cleanup_failures,
        request.phase,
        request.err,
    );
    warn!(
        guild_id = %request.guild_key,
        meeting_id = request.expected_meeting_id,
        phase = request.phase,
        error = %request.err,
        attempts = outcome.attempts,
        max_attempts = RECORDING_TERMINAL_CLEANUP_MAX_RETRIES,
        "terminal recording cleanup failed; rescheduling"
    );

    let Some(terminal_error) = outcome.terminal_error else {
        return TerminalCleanupRetryDecision::Reschedule;
    };

    error!(
        guild_id = %request.guild_key,
        meeting_id = request.expected_meeting_id,
        phase = request.phase,
        error = %request.err,
        attempts = outcome.attempts,
        "terminal recording cleanup retry limit reached; marking recording failed before local cleanup"
    );
    {
        let mut service = service.lock().await;
        if let Err(err) = persist_terminal_cleanup_retry_exhaustion(
            &mut service.store,
            request.expected_meeting_id,
            &terminal_error,
        ) {
            warn!(
                guild_id = %request.guild_key,
                meeting_id = request.expected_meeting_id,
                phase = request.phase,
                error = %err,
                "terminal cleanup status update failed; preserving local state for retry"
            );
            restart_terminal_cleanup_retry_window_after_persistence_failure(
                terminal_cleanup_failures,
            );
            return TerminalCleanupRetryDecision::Reschedule;
        }
    }
    let removed_session = remove_local_recording_state_after_terminal_absence_with_dependencies(
        local,
        request.guild_key,
        request.expected_meeting_id,
    )
    .await;
    TerminalCleanupRetryDecision::Cleared {
        removed_session: Box::new(removed_session),
    }
}

async fn fail_recording_after_teardown_exhaustion_with_dependencies<
    S,
    C,
    L: RecordingVoiceLeaveDependency + Sync,
    P: Fn(&RecordingSession<C>, &SsrcTracker) + Send + Sync,
>(
    service: &Arc<Mutex<BotCommandService<S>>>,
    local: &RecordingLifecycleLocalState<'_, C>,
    voice_leave: &L,
    request: TerminalAbsenceCleanupRequest<'_>,
    error_message: &str,
    persist_ssrc_mapping: P,
) -> Result<Option<RecordingVoiceLeaveOutcome>, String>
where
    S: MeetingStore + Send,
    C: ChunkStorage + Send,
{
    let _reset_guard = local.ssrc_tracker_reset_gate.lock().await;
    {
        let mut service = service.lock().await;
        mark_recording_failed_after_teardown_exhaustion(
            &mut service.store,
            request.expected_meeting_id,
            error_message,
        )
        .map_err(|err| err.to_string())?;
    }

    // Local runtime cleanup is intentionally idempotent and happens only
    // after the terminal DB transition is known handled. If the store is
    // unavailable, callers retry with the session/startup handles intact.
    let mut removed_session = {
        let _voice_event_guard = local.voice_event_gate.write().await;
        let removed_session = {
            let mut sessions = local.sessions.lock().await;
            remove_matching_recording_session_for_meeting(
                &mut sessions,
                request.guild_key,
                request.expected_meeting_id,
            )
        };
        {
            let mut states = local.auto_stop_states.lock().await;
            remove_auto_stop_state_for_meeting(
                &mut states,
                request.guild_key,
                request.expected_meeting_id,
            );
        }
        if let Some(session) = &removed_session {
            let mut titles = local.live_transcription_titles.lock().await;
            titles.remove(&session.meeting_id);
        }
        {
            let mut startups = local.recording_startups.lock().await;
            clear_matching_recording_startup(
                &mut startups,
                request.guild_key,
                request.expected_meeting_id,
            );
        }
        removed_session
    };

    let leave_outcome = voice_leave
        .leave_recording_voice_with_timeout(
            request.guild_id,
            request.expected_meeting_id,
            request.phase,
        )
        .await;

    if let Some(session) = removed_session.as_mut()
        && let Err(err) =
            flush_removed_session_after_stop(session, request.guild_key, request.phase)
    {
        warn_removed_session_flush_failure(request.guild_key, request.phase, &err);
    }

    let latest_tracker = {
        let _voice_event_guard = local.voice_event_gate.write().await;
        let tracker = local.ssrc_tracker.lock().await;
        tracker.clone()
    };
    if let Some(session) = &removed_session {
        persist_ssrc_mapping(session, &latest_tracker);
    }

    Ok(leave_outcome)
}

fn flush_sessions_for_shutdown<S: ChunkStorage>(
    sessions: &mut HashMap<String, RecordingSession<S>>,
) -> usize {
    let mut flushed = 0usize;
    for (guild_id, session) in sessions.iter_mut() {
        match flush_session_for_teardown(session, guild_id, "shutdown") {
            Ok(()) => flushed += 1,
            Err(err) => warn!(
                guild_id = %guild_id,
                meeting_id = %session.meeting_id,
                error = %err,
                "failed to drain recording session during shutdown"
            ),
        }
    }
    flushed
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

    let ordered_segments = ordered_transcript_segments(segments);
    let base_sql = crate::infrastructure::sql::build_insert_transcripts_sql_with_offset(
        ordered_segments.len(),
        1,
    );
    let sql = format!("WITH cleared AS (DELETE FROM transcripts WHERE meeting_id=$1) {base_sql}");
    let mut params = Vec::with_capacity(ordered_segments.len() * 9 + 1);
    params.push(meeting_id.to_owned());
    for (i, seg) in ordered_segments.iter().enumerate() {
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

fn live_chunk_id(chunk: &PersistedChunk) -> String {
    format!(
        "{}-{}-{}-{}",
        chunk.meeting_id, chunk.user_id, chunk.sequence, chunk.start_ms
    )
}

fn live_transcript_row_id(chunk_id: &str, segment_index: usize) -> String {
    format!("{chunk_id}-seg-{segment_index}")
}

fn build_live_insert_transcripts_sql(count: usize, param_offset: usize) -> String {
    let mut sql = String::from(
        "INSERT INTO transcripts (id, meeting_id, speaker_id, start_ms, end_ms, text, confidence, is_noisy, source, transcript_stage, live_chunk_id) VALUES ",
    );
    for i in 0..count {
        let base = i * 10 + param_offset;
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&format!(
            "(${}, ${}, ${}, ${}::TEXT::INTEGER, ${}::TEXT::INTEGER, ${}, NULLIF(${},'')::TEXT::DOUBLE PRECISION, ${}::TEXT::BOOLEAN, ${}, 'live', ${})",
            base + 1,
            base + 2,
            base + 3,
            base + 4,
            base + 5,
            base + 6,
            base + 7,
            base + 8,
            base + 9,
            base + 10,
        ));
    }
    sql.push_str(
        " ON CONFLICT (id) DO UPDATE SET \
        meeting_id = EXCLUDED.meeting_id, \
        speaker_id = EXCLUDED.speaker_id, \
        start_ms = EXCLUDED.start_ms, \
        end_ms = EXCLUDED.end_ms, \
        text = EXCLUDED.text, \
        confidence = EXCLUDED.confidence, \
        is_noisy = EXCLUDED.is_noisy, \
        source = EXCLUDED.source, \
        transcript_stage = EXCLUDED.transcript_stage, \
        live_chunk_id = EXCLUDED.live_chunk_id",
    );
    sql
}

fn mark_live_transcription_chunk_running<E: SqlExecutor>(
    executor: &mut E,
    chunk: &PersistedChunk,
    timeline_base_ms: u64,
) -> Result<(), String> {
    executor
        .execute(
            "INSERT INTO live_transcription_chunks \
             (id, meeting_id, speaker_id, sequence, start_ms, timeline_base_ms, status, error_message, attempt_count, updated_at) \
             VALUES ($1, $2, $3, $4::TEXT::BIGINT, $5::TEXT::BIGINT, $6::TEXT::BIGINT, 'running', NULL, 1, NOW()) \
             ON CONFLICT (id) DO UPDATE SET \
             status='running', error_message=NULL, timeline_base_ms=EXCLUDED.timeline_base_ms, attempt_count=live_transcription_chunks.attempt_count + 1, updated_at=NOW()",
            &[
                live_chunk_id(chunk),
                chunk.meeting_id.clone(),
                chunk.user_id.clone(),
                chunk.sequence.to_string(),
                chunk.start_ms.to_string(),
                timeline_base_ms.to_string(),
            ],
        )
        .map(|_| ())
}

fn persist_live_transcription_success<E: SqlExecutor>(
    executor: &mut E,
    chunk: &PersistedChunk,
    segments: &[TranscriptSegment],
) -> Result<(), TranscriptPersistError> {
    let chunk_id = live_chunk_id(chunk);
    executor.execute("BEGIN", &[]).map(|_| ()).map_err(|err| {
        TranscriptPersistError::Database(format!(
            "failed to begin live transcript transaction: {err}"
        ))
    })?;

    let result = (|| {
        executor
            .execute(
                "DELETE FROM transcripts WHERE meeting_id=$1 AND transcript_stage='live' AND live_chunk_id=$2",
                &[chunk.meeting_id.clone(), chunk_id.clone()],
            )
            .map_err(|err| {
                TranscriptPersistError::Database(format!(
                    "failed to clear old live transcript chunk rows: {err}"
                ))
            })?;

        if !segments.is_empty() {
            let sql = build_live_insert_transcripts_sql(segments.len(), 0);
            let mut params = Vec::with_capacity(segments.len() * 10);
            for (i, seg) in segments.iter().enumerate() {
                let start_ms = db_safe_transcript_timestamp_ms(i, "start_ms", seg.start_ms)
                    .map_err(TranscriptPersistError::Validation)?;
                let end_ms = db_safe_transcript_timestamp_ms(i, "end_ms", seg.end_ms)
                    .map_err(TranscriptPersistError::Validation)?;
                params.push(live_transcript_row_id(&chunk_id, i));
                params.push(chunk.meeting_id.clone());
                params.push(seg.speaker_id.clone());
                params.push(start_ms.to_string());
                params.push(end_ms.to_string());
                params.push(seg.text.clone());
                params.push(seg.confidence.map(|c| c.to_string()).unwrap_or_default());
                params.push(seg.is_noisy.to_string());
                params.push(seg.source.as_str().to_owned());
                params.push(chunk_id.clone());
            }
            executor.execute(&sql, &params).map_err(|err| {
                TranscriptPersistError::Database(format!(
                    "failed to persist live transcript segments: {err}"
                ))
            })?;
        }

        executor
            .execute(
                "UPDATE live_transcription_chunks SET status='done', error_message=NULL, updated_at=NOW() WHERE id=$1",
                &[chunk_id],
            )
            .map(|_| ())
            .map_err(|err| {
                TranscriptPersistError::Database(format!(
                    "failed to mark live transcription chunk done: {err}"
                ))
            })
    })();

    match result {
        Ok(()) => executor.execute("COMMIT", &[]).map(|_| ()).map_err(|err| {
            TranscriptPersistError::Database(format!(
                "failed to commit live transcript transaction: {err}"
            ))
        }),
        Err(err) => {
            let _ = executor.execute("ROLLBACK", &[]);
            Err(err)
        }
    }
}

fn mark_live_transcription_chunk_failed<E: SqlExecutor>(
    executor: &mut E,
    chunk: &PersistedChunk,
    error_message: &str,
) -> Result<(), String> {
    let chunk_id = live_chunk_id(chunk);
    executor
        .execute("BEGIN", &[])
        .map_err(|err| format!("failed to begin live failure transaction: {err}"))?;
    let result = (|| {
        executor.execute(
            "DELETE FROM transcripts WHERE meeting_id=$1 AND transcript_stage='live' AND live_chunk_id=$2",
            &[chunk.meeting_id.clone(), chunk_id.clone()],
        )?;
        executor
            .execute(
                "UPDATE live_transcription_chunks SET status='failed', error_message=$2, updated_at=NOW() WHERE id=$1",
                &[chunk_id, error_message.to_owned()],
            )
            .map(|_| ())
    })();
    match result {
        Ok(()) => executor
            .execute("COMMIT", &[])
            .map(|_| ())
            .map_err(|err| format!("failed to commit live failure transaction: {err}")),
        Err(err) => {
            let _ = executor.execute("ROLLBACK", &[]);
            Err(err)
        }
    }
}

fn parse_transcript_row(row: &[Option<String>]) -> Result<TranscriptSegment, String> {
    let get = |index: usize, name: &str| -> Result<String, String> {
        row.get(index)
            .and_then(|value| value.clone())
            .ok_or_else(|| format!("transcript row missing {name}"))
    };
    let speaker_id = get(0, "speaker_id")?;
    let start_ms = get(1, "start_ms")?
        .parse::<u64>()
        .map_err(|err| format!("invalid transcript start_ms: {err}"))?;
    let end_ms = get(2, "end_ms")?
        .parse::<u64>()
        .map_err(|err| format!("invalid transcript end_ms: {err}"))?;
    let text = get(3, "text")?;
    let confidence = row
        .get(4)
        .and_then(|value| value.as_ref())
        .map(|value| {
            value
                .parse::<f32>()
                .map_err(|err| format!("invalid transcript confidence: {err}"))
        })
        .transpose()?;
    let is_noisy = get(5, "is_noisy")?
        .parse::<bool>()
        .map_err(|err| format!("invalid transcript is_noisy: {err}"))?;
    let source = TranscriptSource::parse_str(&get(6, "source")?)
        .ok_or_else(|| "invalid transcript source".to_owned())?;
    Ok(TranscriptSegment {
        speaker_id,
        start_ms,
        end_ms,
        text,
        confidence,
        is_noisy,
        source,
        merged_count: 1,
    })
}

fn load_live_transcript_segments<E: SqlExecutor>(
    executor: &mut E,
    meeting_id: &str,
    final_timeline_base_ms: u64,
) -> Result<Vec<TranscriptSegment>, String> {
    let rows = executor.query_rows(
        "SELECT t.speaker_id, t.start_ms, t.end_ms, t.text, t.confidence, t.is_noisy, t.source, c.timeline_base_ms \
         FROM transcripts t \
         INNER JOIN live_transcription_chunks c ON c.id = t.live_chunk_id AND c.status='done' \
         WHERE t.meeting_id=$1 AND t.transcript_stage='live' AND NOT t.is_deleted \
         ORDER BY t.start_ms, t.end_ms, t.speaker_id, t.id",
        &[meeting_id.to_owned()],
    )?;
    let mut segments = rows
        .iter()
        .map(|row| {
            let mut segment = parse_transcript_row(row)?;
            if let Some(Some(base)) = row.get(7) {
                let live_base = base
                    .parse::<u64>()
                    .map_err(|err| format!("invalid live timeline_base_ms: {err}"))?;
                if live_base >= final_timeline_base_ms {
                    let delta = live_base - final_timeline_base_ms;
                    segment.start_ms = segment.start_ms.saturating_add(delta);
                    segment.end_ms = segment.end_ms.saturating_add(delta);
                } else {
                    let delta = final_timeline_base_ms - live_base;
                    segment.start_ms = segment.start_ms.saturating_sub(delta);
                    segment.end_ms = segment.end_ms.saturating_sub(delta);
                }
            }
            Ok(segment)
        })
        .collect::<Result<Vec<_>, String>>()?;
    sort_transcript_segments(&mut segments);
    Ok(segments)
}

fn final_transcript_rows_exist<E: SqlExecutor>(
    executor: &mut E,
    meeting_id: &str,
) -> Result<bool, String> {
    executor
        .query_rows(
            "SELECT 1 FROM transcripts WHERE meeting_id=$1 AND transcript_stage='final' AND NOT is_deleted LIMIT 1",
            &[meeting_id.to_owned()],
        )
        .map(|rows| !rows.is_empty())
}

fn load_completed_live_transcription_chunks<E: SqlExecutor>(
    executor: &mut E,
    meeting_id: &str,
) -> Result<Vec<ProcessedAudioChunk>, String> {
    let rows = executor.query_rows(
        "SELECT speaker_id, sequence, start_ms \
         FROM live_transcription_chunks \
         WHERE meeting_id=$1 AND status='done' \
         ORDER BY start_ms, speaker_id, sequence",
        &[meeting_id.to_owned()],
    )?;
    rows.iter()
        .map(|row| {
            let speaker_id = row
                .first()
                .and_then(|value| value.clone())
                .ok_or_else(|| "live chunk row missing speaker_id".to_owned())?;
            let sequence = row
                .get(1)
                .and_then(|value| value.clone())
                .ok_or_else(|| "live chunk row missing sequence".to_owned())?
                .parse::<u64>()
                .map_err(|err| format!("invalid live chunk sequence: {err}"))?;
            let start_ms = row
                .get(2)
                .and_then(|value| value.clone())
                .ok_or_else(|| "live chunk row missing start_ms".to_owned())?
                .parse::<u64>()
                .map_err(|err| format!("invalid live chunk start_ms: {err}"))?;
            Ok(ProcessedAudioChunk {
                speaker_id,
                sequence,
                start_ms,
            })
        })
        .collect()
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
    summary_enabled: bool,
) -> bool {
    if !summary_enabled {
        info!(
            meeting_id,
            job_id, "summary job not recovered because meeting snapshot disabled summaries"
        );
        return false;
    }

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
            .is_some_and(|status| status.as_deref() == Some("queued")),
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
    use crate::audio::meeting_audio::MAX_MEETING_AUDIO_SPAN_MS;

    let meeting_start_ms = compute_meeting_start_ms(chunks);
    let mut placements = Vec::new();
    for chunk in chunks {
        let offset_ms = chunk.start_ms.saturating_sub(meeting_start_ms);
        if offset_ms > MAX_MEETING_AUDIO_SPAN_MS {
            warn!(
                start_ms = chunk.start_ms,
                meeting_start_ms,
                offset_ms,
                "skipping chunk with wall-clock offset beyond meeting cap"
            );
            continue;
        }
        let offset_samples =
            ((offset_ms as u128).saturating_mul(sample_rate as u128) / 1_000u128) as usize;
        placements.push((offset_samples, chunk));
    }
    let total_samples = placements
        .iter()
        .map(|(offset, chunk)| *offset + chunk.pcm.len() / 2)
        .max()
        .unwrap_or(0);
    let capped_total_samples = total_samples.min(
        ((MAX_MEETING_AUDIO_SPAN_MS as u128).saturating_mul(sample_rate as u128) / 1_000u128)
            as usize,
    );

    let mut mixed = vec![0i32; capped_total_samples];
    for (offset_samples, chunk) in placements {
        let chunk_samples = chunk.pcm.len() / 2;
        let usable_samples = chunk_samples.min(capped_total_samples.saturating_sub(offset_samples));
        if usable_samples < chunk_samples {
            warn!(
                start_ms = chunk.start_ms,
                offset_samples,
                chunk_samples,
                usable_samples,
                capped_total_samples,
                "truncating chunk PCM tail beyond meeting wall-clock cap"
            );
        }
        for i in 0..usable_samples {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotRunExit {
    Shutdown,
    TokenChanged,
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

fn format_status_message_content(
    meeting_id: &str,
    update: &StatusMessageUpdate<'_>,
    meeting_url: Option<&str>,
) -> String {
    let with_meeting_url = |base: String| match meeting_url {
        Some(url) if !base.contains(url) => format!("{base}\n会議ページ: {url}"),
        None => base,
        _ => base,
    };

    match update {
        StatusMessageUpdate::RecordingStarted {
            voice_channel_id,
            report_channel_id,
        } => with_meeting_url(format!(
            "🎙️ 録音を開始しました\nmeeting_id={meeting_id}\nVC: <#{}>\nレポート: <#{}>",
            voice_channel_id, report_channel_id
        )),
        StatusMessageUpdate::RecordingStopped => with_meeting_url(format!(
            "⏹️ 録音を終了しました。要約を準備しています。\nmeeting_id={meeting_id}"
        )),
        StatusMessageUpdate::SummaryStarted => with_meeting_url(format!(
            "📝 要約を開始しました (文字起こし/要約を実行中)\nmeeting_id={meeting_id}"
        )),
        StatusMessageUpdate::SummaryCompleted { summary_url } => {
            let base = format!("✅ 要約が完了しました\nmeeting_id={meeting_id}");
            with_meeting_url(
                summary_url
                    .as_deref()
                    .map_or(base.clone(), |url| format!("{base}\n詳細ページ: {url}")),
            )
        }
        StatusMessageUpdate::Failed { phase, error } => {
            let trimmed = truncate_error_for_status(error);
            with_meeting_url(format!(
                "⚠️ 処理に失敗しました ({phase})\nmeeting_id={meeting_id}\nerror={trimmed}"
            ))
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

pub async fn run_bot(
    config: &AppConfig,
    mut bot_token_revision: watch::Receiver<u64>,
) -> Result<BotRunExit, RuntimeError> {
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
        .apply_pending_migrations()
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
        recording_startups: Arc::new(Mutex::new(HashMap::new())),
        recording_start_cleanup_retries: Arc::new(StdMutex::new(HashSet::new())),
        live_transcription_bases: Arc::new(Mutex::new(HashMap::new())),
        live_transcription_titles: Arc::new(Mutex::new(HashMap::new())),
        live_transcription_gate: Arc::new(Semaphore::new(1)),
        auto_stop_states: Arc::new(Mutex::new(HashMap::new())),
        command_gate: Arc::new(RwLock::new(())),
        voice_event_gate: Arc::new(RwLock::new(())),
        ssrc_tracker_reset_gate: Arc::new(Mutex::new(())),
        background_spawn_gate: Arc::new(StdMutex::new(())),
        retention_cleanup_running: Arc::new(AtomicBool::new(false)),
        shutting_down: Arc::new(AtomicBool::new(false)),
        shutdown_token: CancellationToken::new(),
        task_tracker: TaskTracker::new(),
        chunk_storage_dir: config.chunk_storage_dir.clone(),
        auto_stop_grace_seconds: config.auto_stop_grace_seconds,
        whisper_endpoint: config.whisper_endpoint.clone(),
        summary_harness: config.summary_harness,
        summary_command: config.summary_command.clone(),
        summary_model: config.summary_model.clone(),
        summary_allow_unsafe_agent_harness: config.summary_allow_unsafe_agent_harness,
        whisper_language: config.whisper_language.clone(),
        whisper_beam_size: config.whisper_beam_size,
        whisper_suppress_non_speech: config.whisper_suppress_non_speech,
        whisper_prompt: config.whisper_prompt.clone(),
        whisper_vad: config.whisper_vad,
        whisper_temperature: config.whisper_temperature,
        whisper_resample_to_16k: config.whisper_resample_to_16k,
        summary_max_retries: config.summary_max_retries,
        summary_enabled: config.summary_enabled,
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
    let voice_manager = songbird::Songbird::serenity_from_config(songbird_config);
    let mut client = Client::builder(&config.discord_token, intents)
        .event_handler(handler.clone())
        .register_songbird_with(Arc::clone(&voice_manager))
        .await
        .map_err(|err| RuntimeError::ClientInit(err.to_string()))?;
    let shard_manager = Arc::clone(&client.shard_manager);

    tokio::select! {
        result = client.start() => {
            handler.shutdown(Arc::clone(&voice_manager), SHUTDOWN_GRACE_TIMEOUT).await;
            result
                .map(|_| BotRunExit::Shutdown)
                .map_err(|err| RuntimeError::ClientRun(err.to_string()))
        }
        () = shutdown_signal() => {
            info!("shutdown signal received");
            handler.shutting_down.store(true, Ordering::Release);
            shard_manager.shutdown_all().await;
            handler.shutdown(voice_manager, SHUTDOWN_GRACE_TIMEOUT).await;
            Ok(BotRunExit::Shutdown)
        }
        () = wait_for_bot_token_revision_change(&mut bot_token_revision) => {
            info!("guild bot token changed; restarting Discord gateway client");
            handler.shutting_down.store(true, Ordering::Release);
            shard_manager.shutdown_all().await;
            handler.shutdown(voice_manager, SHUTDOWN_GRACE_TIMEOUT).await;
            Ok(BotRunExit::TokenChanged)
        }
    }
}

async fn wait_for_bot_token_revision_change(revision: &mut watch::Receiver<u64>) {
    if revision.changed().await.is_err() {
        std::future::pending::<()>().await;
    }
}

fn load_runtime_summary_context(
    store: &mut SqlMeetingStore<PgSqlExecutor>,
    meeting_id: &str,
    guild_id: &str,
    effective_settings: &EffectiveMeetingSettings,
) -> Result<
    crate::application::summary::SummaryContextInput,
    crate::application::summary::SummaryError,
> {
    let domain_knowledge = store
        .list_domain_knowledge(guild_id, false, None)
        .map_err(|err| {
            crate::application::summary::SummaryError::SummaryEngine(format!(
                "failed to load domain knowledge for meeting {meeting_id}: {err}"
            ))
        })?;

    let summary_template = crate::application::worker::load_effective_summary_template(
        store,
        guild_id,
        Some(effective_settings),
    )
    .map_err(|err| {
        crate::application::summary::SummaryError::SummaryEngine(format!(
            "failed to load summary template for meeting {meeting_id}: {err}"
        ))
    })?;

    Ok(crate::application::summary::SummaryContextInput {
        speakers: Vec::new(),
        domain_knowledge,
        summary_template,
        effective_summary_template_id: effective_settings.summary_template_id.clone(),
        effective_domain_knowledge_version_id: effective_settings
            .domain_knowledge_version_id
            .clone(),
    })
}

#[derive(Clone)]
struct ScaffoldHandler {
    guild_id: GuildId,
    service: Arc<Mutex<BotCommandService<SqlMeetingStore<PgSqlExecutor>>>>,
    queue: Arc<Mutex<SqlJobQueue<PgSqlExecutor>>>,
    ssrc_tracker: Arc<Mutex<SsrcTracker>>,
    sessions: Arc<Mutex<HashMap<String, RecordingSession<LocalChunkStorage>>>>,
    recording_startups: Arc<Mutex<HashMap<String, String>>>,
    recording_start_cleanup_retries: Arc<StdMutex<HashSet<String>>>,
    live_transcription_bases: Arc<Mutex<HashMap<String, u64>>>,
    live_transcription_titles: Arc<Mutex<HashMap<String, Option<String>>>>,
    live_transcription_gate: Arc<Semaphore>,
    auto_stop_states: Arc<Mutex<HashMap<String, AutoStopState>>>,
    command_gate: Arc<RwLock<()>>,
    voice_event_gate: Arc<RwLock<()>>,
    ssrc_tracker_reset_gate: Arc<Mutex<()>>,
    background_spawn_gate: Arc<StdMutex<()>>,
    retention_cleanup_running: Arc<AtomicBool>,
    shutting_down: Arc<AtomicBool>,
    shutdown_token: CancellationToken,
    task_tracker: TaskTracker,
    chunk_storage_dir: String,
    auto_stop_grace_seconds: u64,
    whisper_endpoint: String,
    summary_harness: SummaryHarness,
    summary_command: String,
    summary_model: String,
    summary_allow_unsafe_agent_harness: bool,
    whisper_language: Option<String>,
    whisper_beam_size: u32,
    whisper_suppress_non_speech: bool,
    whisper_prompt: Option<String>,
    whisper_vad: bool,
    whisper_temperature: f32,
    whisper_resample_to_16k: bool,
    summary_max_retries: u32,
    summary_enabled: bool,
    retention_policy: crate::domain::retention::RetentionPolicy,
    integration_retry_policy: RetryPolicy,
    public_base_url: Option<String>,
    bot_admin_user_ids: HashSet<String>,
}

impl ScaffoldHandler {
    fn meeting_settings_defaults(&self) -> MeetingSettingsDefaults {
        MeetingSettingsDefaults {
            whisper_language: self.whisper_language.clone(),
            whisper_vad: self.whisper_vad,
            whisper_beam_size: self.whisper_beam_size,
            whisper_suppress_non_speech: self.whisper_suppress_non_speech,
            whisper_prompt: self.whisper_prompt.clone(),
            whisper_temperature: self.whisper_temperature,
            whisper_resample_to_16k: self.whisper_resample_to_16k,
            auto_stop_grace_seconds: self.auto_stop_grace_seconds,
            retention_raw_audio_ttl_days: self.retention_policy.raw_audio_ttl_days.get(),
            retention_transcript_ttl_days: self.retention_policy.transcript_ttl_days.get(),
            retention_summary_ttl_days: self
                .retention_policy
                .summary_ttl_days
                .map(std::num::NonZeroU32::get),
            summary_enabled: self.summary_enabled,
        }
    }

    async fn effective_settings_for_meeting(
        &self,
        meeting_id: &str,
    ) -> Result<EffectiveMeetingSettings, String> {
        let mut service = self.service.lock().await;
        service
            .store
            .get_effective_meeting_settings(meeting_id)
            .map_err(|err| err.to_string())
            .map(|settings| {
                settings.unwrap_or_else(|| {
                    EffectiveMeetingSettings::from_defaults(&self.meeting_settings_defaults())
                })
            })
    }

    async fn auto_stop_grace_for_meeting(&self, meeting_id: Option<&str>) -> Duration {
        let seconds = match meeting_id {
            Some(meeting_id) => self
                .effective_settings_for_meeting(meeting_id)
                .await
                .map(|settings| settings.auto_stop_grace_seconds)
                .unwrap_or(self.auto_stop_grace_seconds),
            None => self.auto_stop_grace_seconds,
        };
        Duration::from_secs(seconds)
    }

    fn spawn_background<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let _spawn_guard = self
            .background_spawn_gate
            .lock()
            .expect("background spawn gate poisoned");
        if self.shutting_down.load(Ordering::Acquire) {
            debug!("spawn_background: shutdown in progress, dropping background task");
            return;
        }
        self.task_tracker.spawn(future);
    }

    fn spawn_lifecycle_cleanup_retry<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let _spawn_guard = self
            .background_spawn_gate
            .lock()
            .expect("background spawn gate poisoned");
        self.task_tracker.spawn(future);
    }

    async fn live_transcription_base_for_chunks(&self, chunks: &[PersistedChunk]) -> Option<u64> {
        let base_start_ms = chunks.iter().map(|chunk| chunk.start_ms).min()?;
        let mut bases = self.live_transcription_bases.lock().await;
        let entry = bases
            .entry(chunks[0].meeting_id.clone())
            .or_insert(base_start_ms);
        if base_start_ms < *entry {
            *entry = base_start_ms;
        }
        Some(*entry)
    }

    async fn live_transcription_title(&self, meeting_id: &str) -> Option<String> {
        let titles = self.live_transcription_titles.lock().await;
        titles.get(meeting_id).cloned().flatten()
    }

    fn spawn_live_transcription_tasks(
        &self,
        chunks: Vec<PersistedChunk>,
        base_start_ms: u64,
        meeting_title: Option<String>,
    ) {
        for chunk in chunks {
            let runtime = self.clone();
            let meeting_title = meeting_title.clone();
            self.spawn_background(async move {
                runtime
                    .process_live_transcription_chunk(chunk, base_start_ms, meeting_title)
                    .await;
            });
        }
    }

    async fn process_live_transcription_chunk(
        &self,
        chunk: PersistedChunk,
        base_start_ms: u64,
        meeting_title: Option<String>,
    ) {
        let Ok(_permit) = Arc::clone(&self.live_transcription_gate)
            .acquire_owned()
            .await
        else {
            warn!(
                meeting_id = %chunk.meeting_id,
                user_id = %chunk.user_id,
                sequence = chunk.sequence,
                "live transcription semaphore closed; skipping chunk"
            );
            return;
        };

        if let Err(err) = {
            let mut service = self.service.lock().await;
            mark_live_transcription_chunk_running(
                &mut service.store.executor,
                &chunk,
                base_start_ms,
            )
        } {
            warn!(
                meeting_id = %chunk.meeting_id,
                user_id = %chunk.user_id,
                sequence = chunk.sequence,
                error = %err,
                "failed to mark live transcription chunk running"
            );
            return;
        }

        let effective_settings = self
            .effective_settings_for_meeting(&chunk.meeting_id)
            .await
            .unwrap_or_else(|err| {
                warn!(
                    meeting_id = %chunk.meeting_id,
                    error = %err,
                    "failed to load meeting settings snapshot for live transcription; using runtime defaults"
                );
                EffectiveMeetingSettings::from_defaults(&self.meeting_settings_defaults())
            });
        let whisper = CommandWhisperClient {
            endpoint: self.whisper_endpoint.clone(),
            curl_bin: "curl".to_owned(),
            retry_policy: self.integration_retry_policy,
            beam_size: effective_settings.whisper_beam_size,
            suppress_non_speech: effective_settings.whisper_suppress_non_speech,
            prompt: effective_settings.whisper_prompt.clone(),
            vad: effective_settings.whisper_vad,
            temperature: effective_settings.whisper_temperature,
            command_timeout: DEFAULT_COMMAND_TIMEOUT,
        };
        let request = WhisperInferenceRequest {
            audio_path: chunk.saved.path.to_string_lossy().to_string(),
            language: effective_settings.whisper_language.clone(),
            prompt: crate::application::summary::build_whisper_context_prompt(
                meeting_title.as_deref(),
                Some(&chunk.user_id),
            ),
        };
        let transcription = tokio::task::block_in_place(|| whisper.infer(&request));
        let mut segments = match transcription {
            Ok(output) => {
                let offset_ms = chunk.start_ms.saturating_sub(base_start_ms);
                let mut segments = output
                    .segments
                    .into_iter()
                    .map(|mut segment| {
                        segment.speaker_id = chunk.user_id.clone();
                        segment.start_ms = segment.start_ms.saturating_add(offset_ms);
                        segment.end_ms = segment.end_ms.saturating_add(offset_ms);
                        segment
                    })
                    .collect::<Vec<_>>();
                sort_transcript_segments(&mut segments);
                normalize_segments(&segments, NormalizationConfig::default())
            }
            Err(err) => {
                let error_message = err.to_string();
                let mark_result = {
                    let mut service = self.service.lock().await;
                    mark_live_transcription_chunk_failed(
                        &mut service.store.executor,
                        &chunk,
                        &error_message,
                    )
                };
                if let Err(mark_err) = mark_result {
                    warn!(
                        meeting_id = %chunk.meeting_id,
                        user_id = %chunk.user_id,
                        sequence = chunk.sequence,
                        error = %mark_err,
                        "failed to mark live transcription chunk failed"
                    );
                }
                warn!(
                    meeting_id = %chunk.meeting_id,
                    user_id = %chunk.user_id,
                    sequence = chunk.sequence,
                    error = %error_message,
                    "live transcription chunk failed; final transcription will retry this audio"
                );
                return;
            }
        };
        segments.retain(|segment| !segment.text.trim().is_empty());

        let persist_result = {
            let mut service = self.service.lock().await;
            // Keep the finalization admission check and live row write in one
            // critical section. Final transcript persistence uses the same
            // service lock, so it cannot interleave between the status check
            // and the live insert.
            let status_allows_live_write = service
                .store
                .get_meeting(&chunk.meeting_id)
                .ok()
                .flatten()
                .is_some_and(|meeting| {
                    matches!(
                        meeting.status,
                        MeetingStatus::Recording | MeetingStatus::Stopping
                    )
                });
            let final_rows_exist =
                match final_transcript_rows_exist(&mut service.store.executor, &chunk.meeting_id) {
                    Ok(exists) => exists,
                    Err(err) => {
                        warn!(
                            meeting_id = %chunk.meeting_id,
                            user_id = %chunk.user_id,
                            sequence = chunk.sequence,
                            error = %err,
                            "failed to inspect final transcript rows before live write"
                        );
                        true
                    }
                };
            let live_write_allowed = status_allows_live_write && !final_rows_exist;
            if !live_write_allowed {
                let error_message =
                    "live transcription finished after final transcription started".to_owned();
                let mark_result = mark_live_transcription_chunk_failed(
                    &mut service.store.executor,
                    &chunk,
                    &error_message,
                );
                if let Err(mark_err) = mark_result {
                    warn!(
                        meeting_id = %chunk.meeting_id,
                        user_id = %chunk.user_id,
                        sequence = chunk.sequence,
                        error = %mark_err,
                        "failed to mark late live transcription chunk failed"
                    );
                }
                debug!(
                    meeting_id = %chunk.meeting_id,
                    user_id = %chunk.user_id,
                    sequence = chunk.sequence,
                    "discarded late live transcription result"
                );
                return;
            }
            persist_live_transcription_success(&mut service.store.executor, &chunk, &segments)
        };
        if let Err(err) = persist_result {
            let error_message = err.to_string();
            let mark_result = {
                let mut service = self.service.lock().await;
                mark_live_transcription_chunk_failed(
                    &mut service.store.executor,
                    &chunk,
                    &error_message,
                )
            };
            if let Err(mark_err) = mark_result {
                warn!(
                    meeting_id = %chunk.meeting_id,
                    user_id = %chunk.user_id,
                    sequence = chunk.sequence,
                    error = %mark_err,
                    "failed to mark live transcription persist failure"
                );
            }
            warn!(
                meeting_id = %chunk.meeting_id,
                user_id = %chunk.user_id,
                sequence = chunk.sequence,
                error = %error_message,
                "failed to persist live transcription chunk"
            );
        }
    }

    fn reject_if_shutting_down(&self) -> Result<(), String> {
        if self.shutting_down.load(Ordering::Acquire) {
            Err("shutdown in progress; try again after restart".to_owned())
        } else {
            Ok(())
        }
    }

    async fn shutdown(&self, voice_manager: Arc<songbird::Songbird>, grace: Duration) {
        self.shutting_down.store(true, Ordering::Release);
        self.shutdown_token.cancel();

        {
            let _command_guard = self.command_gate.write().await;
            let _voice_event_guard = self.voice_event_gate.write().await;
            {
                let _spawn_guard = self
                    .background_spawn_gate
                    .lock()
                    .expect("background spawn gate poisoned");
                self.task_tracker.close();
            }

            match timeout(
                SHUTDOWN_VOICE_LEAVE_TIMEOUT,
                voice_manager.leave(self.guild_id),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    warn!(
                        guild_id = %self.guild_id,
                        error = %err,
                        "failed to leave voice channel during shutdown"
                    );
                }
                Err(_) => {
                    warn!(
                        guild_id = %self.guild_id,
                        timeout_secs = SHUTDOWN_VOICE_LEAVE_TIMEOUT.as_secs(),
                        "timed out leaving voice channel during shutdown"
                    );
                }
            }

            {
                let tracker = self.ssrc_tracker.lock().await.clone();
                let mut sessions = self.sessions.lock().await;
                let flushed = flush_sessions_for_shutdown(&mut sessions);
                for session in sessions.values() {
                    session.persist_ssrc_mapping(&tracker);
                }
                info!(
                    sessions = sessions.len(),
                    flushed, "recording sessions drained during shutdown"
                );
            }
            {
                let mut states = self.auto_stop_states.lock().await;
                states.clear();
            }
        }

        match timeout(grace, self.task_tracker.wait()).await {
            Ok(()) => info!("background tasks drained during shutdown"),
            Err(_) => warn!(
                timeout_secs = grace.as_secs(),
                "timed out waiting for background tasks during shutdown"
            ),
        }
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(signal) => Some(signal),
            Err(err) => {
                warn!(
                    error = %err,
                    "failed to register SIGTERM handler; only CTRL-C will trigger graceful shutdown"
                );
                None
            }
        };
        tokio::select! {
            _ = async {
                match tokio::signal::ctrl_c().await {
                    Ok(()) => {}
                    Err(err) => {
                        warn!(
                            error = %err,
                            "failed to register CTRL-C handler; waiting for other shutdown signals"
                        );
                        std::future::pending::<()>().await;
                    }
                }
            } => {}
            _ = async {
                if let Some(signal) = terminate.as_mut() {
                    signal.recv().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {}
        }
    }

    #[cfg(not(unix))]
    {
        if let Err(err) = tokio::signal::ctrl_c().await {
            warn!(
                error = %err,
                "failed to register CTRL-C handler; graceful shutdown signal disabled"
            );
            std::future::pending::<()>().await;
        }
    }
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

        if self
            .retention_cleanup_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let retention_handler = self.clone();
            self.spawn_background(async move {
                if let Err(err) = retention_handler.run_startup_retention_cleanup().await {
                    error!(error = %err, "startup retention cleanup failed");
                }
                retention_handler
                    .retention_cleanup_running
                    .store(false, Ordering::Release);
            });
        } else {
            info!("startup retention cleanup already running; skipping duplicate ready event");
        }

        let recovery_handler = self.clone();
        let recovery_ctx = ctx.clone();
        self.spawn_background(async move {
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
                None if self.shutting_down.load(Ordering::Acquire) => {
                    "error: shutdown in progress; try again after restart".to_owned()
                }
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
        if self.shutting_down.load(Ordering::Acquire) {
            return;
        }
        if _new.guild_id != Some(self.guild_id) {
            return;
        }
        let guild_key = self.guild_id.get().to_string();
        let active_voice_channel = match self.active_meeting_voice_channel_result().await {
            Ok(Some(active_voice_channel)) => active_voice_channel,
            Ok(None) => {
                let lifecycle_permit = self.recording_lifecycle_write_permit().await;
                match self.active_meeting_voice_channel_result().await {
                    Ok(Some(active_voice_channel)) => active_voice_channel,
                    Ok(None) => {
                        let session_meeting_id = {
                            let sessions = self.sessions.lock().await;
                            sessions
                                .get(&guild_key)
                                .map(|session| session.meeting_id.clone())
                        };
                        if let Some(meeting_id) = session_meeting_id {
                            let removed_session = self
                                .remove_local_recording_state_after_terminal_absence(
                                    &guild_key,
                                    &meeting_id,
                                )
                                .await;
                            drop(lifecycle_permit);
                            self.finish_terminal_absence_cleanup(
                                &ctx,
                                self.guild_id,
                                &guild_key,
                                &meeting_id,
                                "voice-state inactive meeting cleanup",
                                removed_session,
                            )
                            .await;
                        } else if let Some(meeting_id) = {
                            let startups = self.recording_startups.lock().await;
                            startups.get(&guild_key).cloned()
                        } {
                            drop(lifecycle_permit);
                            self.cleanup_failed_recording_start(
                                &guild_key,
                                &meeting_id,
                                "active meeting disappeared before recording setup completed",
                            )
                            .await;
                        } else {
                            let mut states = self.auto_stop_states.lock().await;
                            states.remove(&guild_key);
                        }
                        return;
                    }
                    Err(err) => {
                        warn!(
                            guild_id = %self.guild_id,
                            error = %err,
                            "failed to resolve active meeting voice channel"
                        );
                        return;
                    }
                }
            }
            Err(err) => {
                warn!(
                    guild_id = %self.guild_id,
                    error = %err,
                    "failed to resolve active meeting voice channel"
                );
                return;
            }
        };
        let target_voice_channel_id = active_voice_channel.voice_channel_id;
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
        let active_meeting_id = active_voice_channel.meeting_id;
        let grace = self
            .auto_stop_grace_for_meeting(Some(&active_meeting_id))
            .await;
        let (signal, timer_generation) = {
            let mut states = self.auto_stop_states.lock().await;
            let state = states.entry(guild_key.clone()).or_insert_with(|| {
                AutoStopState::new_for_meeting(grace, Some(active_meeting_id.clone()))
            });
            state.refresh_for_meeting(grace, &active_meeting_id);
            let signal = state.on_non_bot_member_count_changed(non_bot);
            (signal, state.timer_generation())
        };

        if signal == AutoStopSignal::StartTimer {
            // timer_active was already set atomically inside
            // on_non_bot_member_count_changed — no separate reservation needed.
            let handler = self.clone();
            let ctx_for_task = ctx.clone();
            let guild_for_task = guild_key;
            let expected_meeting_id = active_meeting_id;
            let grace_for_task = grace;
            let target_channel_for_task = target_voice_channel_id;
            self.spawn_background(async move {
                // Keep these counters independent: cache misses decide grace
                // rechecks, final-flush failures protect persisted audio, and
                // stop failures protect the DB/job transition.
                let mut final_flush_failures = 0u32;
                let mut grace_cache_misses = 0u32;
                let mut lookup_failures = 0u32;
                let mut stop_failures = 0u32;
                let mut terminal_cleanup_failures = 0u32;
                let stop_result = loop {
                    tokio::select! {
                        _ = sleep(grace_for_task) => {}
                        _ = handler.shutdown_token.cancelled() => {
                            return;
                        }
                    }
                    let lifecycle_permit = handler.recording_lifecycle_write_permit().await;
                    if handler.shutting_down.load(Ordering::Acquire) {
                        return;
                    }
                    let expected_meeting_id_ref = expected_meeting_id.as_str();
                    // Verify the same meeting is still active (not a new recording)
                    let current_meeting_id = match handler.active_meeting_id_result().await {
                        Ok(current_meeting_id) => current_meeting_id,
                        Err(err) => {
                            let lookup_error = err.to_string();
                            let terminal_error = recording_lookup_terminal_error(
                                &mut lookup_failures,
                                "auto-stop grace",
                                &lookup_error,
                            );
                            warn!(
                                guild_id = %guild_for_task,
                                meeting_id = expected_meeting_id_ref,
                                error = %err,
                                attempts = lookup_failures,
                                "failed to verify active meeting during auto-stop grace; rescheduling"
                            );
                            if let Some(terminal_error) = terminal_error {
                                warn!(
                                    guild_id = %guild_for_task,
                                    meeting_id = expected_meeting_id_ref,
                                    attempts = lookup_failures,
                                    "auto-stop active-meeting lookup retry limit reached; marking recording failed"
                                );
                                match handler
                                    .fail_recording_after_lookup_exhaustion(
                                        &lifecycle_permit,
                                        &ctx_for_task,
                                        ctx_for_task.http.as_ref(),
                                        &RecordingLookupFailureRequest {
                                            guild_id: handler.guild_id,
                                            guild_key: &guild_for_task,
                                            expected_meeting_id: expected_meeting_id_ref,
                                            terminal_error: &terminal_error,
                                            context: "auto-stop active-meeting lookup",
                                        },
                                    )
                                    .await
                                {
                                    Ok(()) => return,
                                    Err(mark_err) => {
                                        warn!(
                                            guild_id = %guild_for_task,
                                            meeting_id = expected_meeting_id_ref,
                                            error = %mark_err,
                                            "failed to mark recording failed after auto-stop lookup exhaustion; rescheduling"
                                        );
                                        if let TerminalCleanupRetryDecision::Cleared {
                                            removed_session,
                                        } = handler
                                            .handle_terminal_cleanup_retry_failure(
                                                TerminalCleanupRetryFailureRequest {
                                                    guild_key: &guild_for_task,
                                                    expected_meeting_id: expected_meeting_id_ref,
                                                    phase: "auto-stop active-meeting lookup",
                                                    err: &mark_err,
                                                },
                                                &mut terminal_cleanup_failures,
                                            )
                                            .await
                                        {
                                            drop(lifecycle_permit);
                                            handler
                                                .finish_terminal_absence_cleanup(
                                                    &ctx_for_task,
                                                    handler.guild_id,
                                                    &guild_for_task,
                                                    expected_meeting_id_ref,
                                                    "auto-stop active-meeting lookup",
                                                    *removed_session,
                                            )
                                            .await;
                                            return;
                                        }
                                    }
                                }
                            }
                            continue;
                        }
                    };
                    reset_recording_lookup_failures(&mut lookup_failures);
                    if current_meeting_id.as_deref() != Some(expected_meeting_id_ref) {
                        let local = handler.lifecycle_local_state();
                        clear_failed_recording_start_local_state_with_dependencies(
                            &local,
                            &guild_for_task,
                            expected_meeting_id_ref,
                            FailedRecordingStartLocalCleanup::FullRuntimeState,
                            |session: &RecordingSession<LocalChunkStorage>, tracker| {
                                session.persist_ssrc_mapping(tracker);
                            },
                        )
                        .await;
                        return;
                    }
                    // Re-verify the voice channel state at fire time. A prior cache-miss
                    // in voice_state_update may have skipped cancelling this timer even
                    // after members rejoined, so we must not rely solely on the state
                    // machine's stale empty_since_ms here.
                    let non_bot_at_fire = count_non_bot_members_in_target_voice(
                        &ctx_for_task,
                        handler.guild_id,
                        target_channel_for_task,
                    );
                    match decide_auto_stop_grace_expiry(non_bot_at_fire) {
                        GraceExpiryDecision::Reschedule => {
                            let terminal_error =
                                auto_stop_cache_miss_terminal_error(&mut grace_cache_misses);
                            warn!(
                                guild_id = %handler.guild_id,
                                target_voice_channel_id = target_channel_for_task,
                                cache_misses = grace_cache_misses,
                                "voice state cache unavailable at auto-stop grace expiry; rescheduling stop check"
                            );
                            if let Some(terminal_error) = terminal_error {
                                warn!(
                                    guild_id = %guild_for_task,
                                    meeting_id = expected_meeting_id_ref,
                                    cache_misses = grace_cache_misses,
                                    "auto-stop cache-miss retry limit reached; marking recording failed"
                                );
                                match handler
                                    .fail_recording_after_teardown_exhaustion(
                                        &lifecycle_permit,
                                        &ctx_for_task,
                                        handler.guild_id,
                                        &guild_for_task,
                                        expected_meeting_id_ref,
                                        &terminal_error,
                                    )
                                    .await
                                {
                                    Ok(()) => {
                                        if let Err(status_err) = handler
                                            .update_status_message(
                                                &ctx_for_task.http,
                                                expected_meeting_id_ref,
                                                StatusMessageUpdate::Failed {
                                                    phase: "Voice state cache",
                                                    error: &terminal_error,
                                                },
                                            )
                                            .await
                                        {
                                            warn!(
                                                guild_id = %guild_for_task,
                                                meeting_id = expected_meeting_id_ref,
                                                error = %status_err,
                                                "failed to notify auto-stop cache-miss exhaustion"
                                            );
                                        }
                                        return;
                                    }
                                    Err(mark_err) => {
                                        warn!(
                                            guild_id = %guild_for_task,
                                            meeting_id = expected_meeting_id_ref,
                                            error = %mark_err,
                                            "failed to mark recording failed after auto-stop cache-miss exhaustion; rescheduling"
                                        );
                                        if let TerminalCleanupRetryDecision::Cleared {
                                            removed_session,
                                        } = handler
                                            .handle_terminal_cleanup_retry_failure(
                                                TerminalCleanupRetryFailureRequest {
                                                    guild_key: &guild_for_task,
                                                    expected_meeting_id: expected_meeting_id_ref,
                                                    phase: "auto-stop cache-miss exhaustion",
                                                    err: &mark_err,
                                                },
                                                &mut terminal_cleanup_failures,
                                            )
                                            .await
                                        {
                                            drop(lifecycle_permit);
                                            handler
                                                .finish_terminal_absence_cleanup(
                                                    &ctx_for_task,
                                                    handler.guild_id,
                                                    &guild_for_task,
                                                    expected_meeting_id_ref,
                                                    "auto-stop cache-miss exhaustion",
                                                    *removed_session,
                                                )
                                                .await;
                                            return;
                                        }
                                        continue;
                                    }
                                }
                            }
                            let mut states = handler.auto_stop_states.lock().await;
                            if rearm_auto_stop_state_for_retry(
                                &mut states,
                                &guild_for_task,
                                expected_meeting_id_ref,
                            ) {
                                continue;
                            }
                            return;
                        }
                        GraceExpiryDecision::Cancel => {
                            let Some(non_bot) = non_bot_at_fire else {
                                unreachable!("Cancel decision requires a known non-bot count")
                            };
                            debug!(
                                guild_id = %handler.guild_id,
                                target_voice_channel_id = target_channel_for_task,
                                non_bot,
                                "members rejoined during grace period; cancelling auto-stop"
                            );
                            let mut states = handler.auto_stop_states.lock().await;
                            if let Some(state) = states
                                .get_mut(&guild_for_task)
                                .filter(|state| state.belongs_to_meeting(expected_meeting_id_ref))
                            {
                                let _ = state.on_non_bot_member_count_changed(non_bot);
                            }
                            return;
                        }
                        GraceExpiryDecision::Stop => {
                            // Reset only the cache-miss counter; flush/stop
                            // failure counters keep enforcing their own limits
                            // across rescheduled Stop-path iterations.
                            grace_cache_misses = 0;
                        }
                    }
                    let (trigger, clear_stale_local_state) = {
                        let mut states = handler.auto_stop_states.lock().await;
                        match states.get_mut(&guild_for_task) {
                            Some(state) if state.belongs_to_meeting(expected_meeting_id_ref) => {
                                (state.tick() == AutoStopSignal::Trigger, false)
                            }
                            Some(state) => {
                                warn!(
                                    guild_id = %guild_for_task,
                                    meeting_id = expected_meeting_id_ref,
                                    current_meeting_id = ?state.meeting_id(),
                                    "auto-stop timer state belongs to another meeting; dropping stale timer"
                                );
                                (false, true)
                            }
                            None => {
                                warn!(
                                    guild_id = %guild_for_task,
                                    meeting_id = expected_meeting_id_ref,
                                    "auto-stop timer state missing at grace expiry; continuing teardown"
                                );
                                (true, false)
                            }
                        }
                    };
                    if !trigger {
                        if clear_stale_local_state {
                            let local = handler.lifecycle_local_state();
                            clear_failed_recording_start_local_state_with_dependencies(
                                &local,
                                &guild_for_task,
                                expected_meeting_id_ref,
                                FailedRecordingStartLocalCleanup::FullRuntimeState,
                                |session: &RecordingSession<LocalChunkStorage>, tracker| {
                                    session.persist_ssrc_mapping(tracker);
                                },
                            )
                            .await;
                        } else {
                            let mut states = handler.auto_stop_states.lock().await;
                            clear_auto_stop_timer_generation_for_meeting(
                                &mut states,
                                &guild_for_task,
                                expected_meeting_id_ref,
                                timer_generation,
                            );
                        }
                        return;
                    }
                    let teardown_request = RecordingStopTeardownRequest {
                        guild_key: &guild_for_task,
                        caller_user_id: "auto-stop",
                        caller_role: UserRole::BotAdmin,
                        expected_meeting_id: expected_meeting_id_ref,
                        reason: StopReason::AutoEmpty,
                        phase: "auto-stop",
                    };
                    match handler
                        .prepare_recording_stop_after_teardown(
                            &lifecycle_permit,
                            &teardown_request,
                        )
                        .await
                    {
                        Ok((result, removed_session)) => {
                            let reset_guard =
                                Arc::clone(&handler.ssrc_tracker_reset_gate).lock_owned().await;
                            drop(lifecycle_permit);
                            handler
                                .leave_after_recording_stop(
                                    &ctx_for_task,
                                    &guild_for_task,
                                    expected_meeting_id_ref,
                                    "auto-stop",
                                    removed_session,
                                    reset_guard,
                                )
                                .await;
                            break result;
                        }
                        Err(RecordingTeardownError::FinalFlush(err)) => {
                            final_flush_failures += 1;
                            if final_flush_failures >= FINAL_FLUSH_MAX_RETRIES {
                                warn!(
                                    guild_id = %guild_for_task,
                                    attempts = final_flush_failures,
                                    error = %err,
                                    "auto-stop final flush retry limit reached; marking recording failed"
                                );
                                let terminal_error = format!(
                                    "final audio flush failed after {final_flush_failures} auto-stop attempt(s): {err}"
                                );
                                match handler
                                    .fail_recording_after_teardown_exhaustion(
                                        &lifecycle_permit,
                                        &ctx_for_task,
                                        handler.guild_id,
                                        &guild_for_task,
                                        expected_meeting_id_ref,
                                        &terminal_error,
                                    )
                                    .await
                                {
                                    Ok(()) => {
                                        if let Err(status_err) = handler
                                            .update_status_message(
                                                &ctx_for_task.http,
                                                expected_meeting_id_ref,
                                                StatusMessageUpdate::Failed {
                                                    phase: "Recording persist",
                                                    error: &terminal_error,
                                                },
                                            )
                                            .await
                                        {
                                            warn!(
                                                guild_id = %guild_for_task,
                                                meeting_id = expected_meeting_id_ref,
                                                error = %status_err,
                                                "failed to notify final flush retry exhaustion"
                                            );
                                        }
                                        return;
                                    }
                                    Err(mark_err) => {
                                        warn!(
                                            guild_id = %guild_for_task,
                                            meeting_id = expected_meeting_id_ref,
                                            error = %mark_err,
                                            "failed to mark recording failed after auto-stop final flush exhaustion; rescheduling"
                                        );
                                        if let TerminalCleanupRetryDecision::Cleared {
                                            removed_session,
                                        } = handler
                                            .handle_terminal_cleanup_retry_failure(
                                                TerminalCleanupRetryFailureRequest {
                                                    guild_key: &guild_for_task,
                                                    expected_meeting_id: expected_meeting_id_ref,
                                                    phase: "auto-stop final flush exhaustion",
                                                    err: &mark_err,
                                                },
                                                &mut terminal_cleanup_failures,
                                            )
                                            .await
                                        {
                                            drop(lifecycle_permit);
                                            handler
                                                .finish_terminal_absence_cleanup(
                                                    &ctx_for_task,
                                                    handler.guild_id,
                                                    &guild_for_task,
                                                    expected_meeting_id_ref,
                                                    "auto-stop final flush exhaustion",
                                                    *removed_session,
                                                )
                                                .await;
                                            return;
                                        }
                                        continue;
                                    }
                                }
                            }
                            let mut states = handler.auto_stop_states.lock().await;
                            if rearm_auto_stop_state_for_retry(
                                &mut states,
                                &guild_for_task,
                                expected_meeting_id_ref,
                            ) {
                                continue;
                            }
                            {
                                drop(states);
                                let terminal_error = format!(
                                    "auto-stop timer state unavailable after final flush failure: {err}"
                                );
                                warn!(
                                    guild_id = %guild_for_task,
                                    meeting_id = expected_meeting_id_ref,
                                    error = %err,
                                    "auto-stop timer state missing or changed after final flush failure; marking recording failed"
                                );
                                match handler
                                    .fail_recording_after_teardown_exhaustion(
                                        &lifecycle_permit,
                                        &ctx_for_task,
                                        handler.guild_id,
                                        &guild_for_task,
                                        expected_meeting_id_ref,
                                        &terminal_error,
                                    )
                                    .await
                                {
                                    Ok(()) => {
                                        if let Err(status_err) = handler
                                            .update_status_message(
                                                &ctx_for_task.http,
                                                expected_meeting_id_ref,
                                                StatusMessageUpdate::Failed {
                                                    phase: "Recording persist",
                                                    error: &terminal_error,
                                                },
                                            )
                                            .await
                                        {
                                            warn!(
                                                guild_id = %guild_for_task,
                                                meeting_id = expected_meeting_id_ref,
                                                error = %status_err,
                                                "failed to notify auto-stop missing timer state"
                                            );
                                        }
                                    }
                                    Err(mark_err) => {
                                        warn!(
                                            guild_id = %guild_for_task,
                                            meeting_id = expected_meeting_id_ref,
                                            error = %mark_err,
                                            "failed to mark recording failed after auto-stop timer state disappeared; rescheduling"
                                        );
                                        if let TerminalCleanupRetryDecision::Cleared {
                                            removed_session,
                                        } = handler
                                            .handle_terminal_cleanup_retry_failure(
                                                TerminalCleanupRetryFailureRequest {
                                                    guild_key: &guild_for_task,
                                                    expected_meeting_id: expected_meeting_id_ref,
                                                    phase:
                                                        "auto-stop missing timer state after flush failure",
                                                    err: &mark_err,
                                                },
                                                &mut terminal_cleanup_failures,
                                            )
                                            .await
                                        {
                                            drop(lifecycle_permit);
                                            handler
                                                .finish_terminal_absence_cleanup(
                                                    &ctx_for_task,
                                                    handler.guild_id,
                                                    &guild_for_task,
                                                    expected_meeting_id_ref,
                                                    "auto-stop missing timer state after flush failure",
                                                    *removed_session,
                                                )
                                                .await;
                                            return;
                                        }
                                        continue;
                                    }
                                }
                                return;
                            }
                        }
                        Err(RecordingTeardownError::Stop(err)) => {
                            final_flush_failures = 0;
                            if err.is_target_absent() {
                                warn!(
                                    guild_id = %guild_for_task,
                                    meeting_id = expected_meeting_id_ref,
                                    error = %err,
                                    "auto-stop found no active meeting; treating as already handled"
                                );
                                let removed_session = handler
                                    .remove_local_recording_state_after_terminal_absence(
                                        &guild_for_task,
                                        expected_meeting_id_ref,
                                    )
                                    .await;
                                drop(lifecycle_permit);
                                handler
                                    .finish_terminal_absence_cleanup(
                                        &ctx_for_task,
                                        handler.guild_id,
                                        &guild_for_task,
                                        expected_meeting_id_ref,
                                        "auto-stop terminal cleanup retry",
                                        removed_session,
                                    )
                                    .await;
                                return;
                            }
                            let terminal_error =
                                recording_stop_terminal_error(
                                    &mut stop_failures,
                                    "auto-stop",
                                    &err.to_string(),
                                );
                            warn!(
                                guild_id = %guild_for_task,
                                meeting_id = expected_meeting_id_ref,
                                error = %err,
                                attempts = stop_failures,
                                "auto stop failed; rescheduling"
                            );
                            if let Some(terminal_error) = terminal_error {
                                warn!(
                                    guild_id = %guild_for_task,
                                    meeting_id = expected_meeting_id_ref,
                                    attempts = stop_failures,
                                    "auto-stop stop retry limit reached; marking recording failed"
                                );
                                match handler
                                    .fail_recording_after_teardown_exhaustion(
                                        &lifecycle_permit,
                                        &ctx_for_task,
                                        handler.guild_id,
                                        &guild_for_task,
                                        expected_meeting_id_ref,
                                        &terminal_error,
                                    )
                                    .await
                                {
                                    Ok(()) => {
                                        if let Err(status_err) = handler
                                            .update_status_message(
                                                &ctx_for_task.http,
                                                expected_meeting_id_ref,
                                                StatusMessageUpdate::Failed {
                                                    phase: "Recording stop",
                                                    error: &terminal_error,
                                                },
                                            )
                                            .await
                                        {
                                            warn!(
                                                guild_id = %guild_for_task,
                                                meeting_id = expected_meeting_id_ref,
                                                error = %status_err,
                                                "failed to notify auto-stop stop retry exhaustion"
                                            );
                                        }
                                        return;
                                    }
                                    Err(mark_err) => {
                                        warn!(
                                            guild_id = %guild_for_task,
                                            meeting_id = expected_meeting_id_ref,
                                            error = %mark_err,
                                            "failed to mark recording failed after auto-stop stop exhaustion; rescheduling"
                                        );
                                        if let TerminalCleanupRetryDecision::Cleared {
                                            removed_session,
                                        } = handler
                                            .handle_terminal_cleanup_retry_failure(
                                                TerminalCleanupRetryFailureRequest {
                                                    guild_key: &guild_for_task,
                                                    expected_meeting_id: expected_meeting_id_ref,
                                                    phase: "auto-stop stop exhaustion",
                                                    err: &mark_err,
                                                },
                                                &mut terminal_cleanup_failures,
                                            )
                                            .await
                                        {
                                            drop(lifecycle_permit);
                                            handler
                                                .finish_terminal_absence_cleanup(
                                                    &ctx_for_task,
                                                    handler.guild_id,
                                                    &guild_for_task,
                                                    expected_meeting_id_ref,
                                                    "auto-stop stop exhaustion",
                                                    *removed_session,
                                                )
                                                .await;
                                            return;
                                        }
                                        continue;
                                    }
                                }
                            }
                            let mut states = handler.auto_stop_states.lock().await;
                            if !rearm_auto_stop_state_for_retry(
                                &mut states,
                                &guild_for_task,
                                expected_meeting_id_ref,
                            ) {
                                drop(states);
                                let mut startups = handler.recording_startups.lock().await;
                                clear_matching_recording_startup(
                                    &mut startups,
                                    &guild_for_task,
                                    expected_meeting_id_ref,
                                );
                                return;
                            }
                            continue;
                        }
                    }
                };
                if stop_result.outcome == StopOutcome::Owner
                    && let Err(err) = handler
                        .update_status_message(
                            &ctx_for_task.http,
                            &stop_result.meeting_id,
                            StatusMessageUpdate::RecordingStopped,
                        )
                        .await
                {
                    warn!(
                        guild_id = %guild_for_task,
                        meeting_id = %stop_result.meeting_id,
                        error = %err,
                        "failed to update status message after auto stop"
                    );
                }
                info!(
                    guild_id = %guild_for_task,
                    meeting_id = %stop_result.meeting_id,
                    "auto stop triggered due to empty voice channel"
                );
                if stop_result.outcome == StopOutcome::Owner
                    && let Err(err) =
                        run_summary_background(&handler, &ctx_for_task.http, &stop_result.meeting_id)
                            .await
                {
                    warn!(
                        guild_id = %guild_for_task,
                        meeting_id = %stop_result.meeting_id,
                        error = %err,
                        "failed to process summary after auto stop"
                    );
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
                let report = *err.report;
                warn!(
                    error = %err.message,
                    "retention filesystem cleanup failed; continuing with database cleanup"
                );
                report
            }
        };
        let database_result = {
            let mut service = self.service.lock().await;
            apply_retention_database_cleanup(
                &mut service.store.executor,
                policy,
                &report.raw_workspace_cleaned_meeting_ids,
            )
        };
        let database_error = match database_result {
            Ok(database_report) => {
                report.merge(database_report);
                None
            }
            Err(err) => {
                report.merge(*err.report);
                Some(err.message)
            }
        };
        if let Some(err) = database_error {
            warn!(
                raw_workspaces_scanned = report.raw_workspaces_scanned,
                raw_audio_dirs_removed = report.raw_audio_dirs_removed,
                legacy_meetings_cleaned = report.legacy_meetings_cleaned,
                raw_workspaces_marked_cleaned = report.raw_workspaces_marked_cleaned,
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
                raw_workspaces_marked_cleaned = report.raw_workspaces_marked_cleaned,
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
                    let meeting_id = row
                        .first()
                        .and_then(|v| v.clone())
                        .ok_or_else(|| "recovery row missing meeting_id".to_owned())?;
                    let status_raw = row
                        .get(1)
                        .and_then(|v| v.clone())
                        .ok_or_else(|| "recovery row missing status".to_owned())?;
                    let voice_channel_id =
                        row.get(2).and_then(|v| v.as_deref()).and_then(|value| {
                            parse_u64_with_warning(&meeting_id, "voice_channel_id", value)
                        });
                    Ok(RecoverySnapshot {
                        meeting_id,
                        status: parse_meeting_status(&status_raw)?,
                        voice_channel_id,
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
                    let summary_enabled = self
                        .effective_settings_for_meeting(&snapshot.meeting_id)
                        .await
                        .map(|settings| settings.summary_enabled)
                        .unwrap_or(self.summary_enabled);
                    let job_id = format!("summary-{}", snapshot.meeting_id);
                    let job_available = {
                        let mut queue = self.queue.lock().await;
                        recover_summary_job_for_startup(
                            &mut queue,
                            &job_id,
                            &snapshot.meeting_id,
                            summary_enabled,
                        )
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

    async fn active_meeting_voice_channel_result(
        &self,
    ) -> Result<Option<ActiveMeetingVoiceChannel>, String> {
        let mut service = self.service.lock().await;
        let Some(meeting) = service
            .store
            .find_active_meeting_by_guild(&self.guild_id.get().to_string())
            .map_err(|err| err.to_string())?
        else {
            return Ok(None);
        };
        let meeting_id = meeting.id;
        let voice_channel_id = meeting.voice_channel_id.parse::<u64>().map_err(|err| {
            format!("invalid active meeting voice channel id for meeting {meeting_id}: {err}")
        })?;
        Ok(Some(ActiveMeetingVoiceChannel {
            meeting_id,
            voice_channel_id,
        }))
    }

    async fn active_meeting_id_result(&self) -> Result<Option<String>, String> {
        let mut service = self.service.lock().await;
        service
            .store
            .find_active_meeting_by_guild(&self.guild_id.get().to_string())
            .map_err(|err| err.to_string())
            .map(|meeting| meeting.map(|meeting| meeting.id))
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
        let meeting_url = self.meeting_url(meeting_id);
        let content = format_status_message_content(meeting_id, &update, meeting_url.as_deref());

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

    fn meeting_url(&self, meeting_id: &str) -> Option<String> {
        self.public_base_url
            .as_ref()
            .map(|base_url| format!("{}/meetings/{}", base_url.trim_end_matches('/'), meeting_id))
    }

    async fn recording_lifecycle_write_permit(&self) -> RecordingLifecycleWritePermit<'_> {
        recording_lifecycle_write_permit_for_gate(&self.command_gate).await
    }

    fn lifecycle_local_state(&self) -> RecordingLifecycleLocalState<'_, LocalChunkStorage> {
        RecordingLifecycleLocalState {
            sessions: &self.sessions,
            auto_stop_states: &self.auto_stop_states,
            live_transcription_titles: &self.live_transcription_titles,
            recording_startups: &self.recording_startups,
            voice_event_gate: &self.voice_event_gate,
            ssrc_tracker: &self.ssrc_tracker,
            ssrc_tracker_reset_gate: &self.ssrc_tracker_reset_gate,
        }
    }

    async fn prepare_recording_stop_after_teardown(
        &self,
        _permit: &RecordingLifecycleWritePermit<'_>,
        request: &RecordingStopTeardownRequest<'_>,
    ) -> Result<
        (
            StopCommandResult,
            Option<RecordingSession<LocalChunkStorage>>,
        ),
        RecordingTeardownError,
    > {
        {
            let _voice_event_guard = self.voice_event_gate.write().await;
            let tracker = {
                let tracker = self.ssrc_tracker.lock().await;
                tracker.clone()
            };
            let mut sessions = self.sessions.lock().await;
            if let Some(session) = sessions
                .get_mut(request.guild_key)
                .filter(|session| session.meeting_id == request.expected_meeting_id)
            {
                flush_session_for_teardown(session, request.guild_key, request.phase)
                    .map_err(RecordingTeardownError::FinalFlush)?;
                // The write voice_event_gate held for this block keeps
                // SpeakingStateUpdate from changing the tracker while the
                // successful final-flush mapping is persisted. Failed flushes
                // keep the session in memory and retry with a fresh snapshot.
                session.persist_ssrc_mapping(&tracker);
            }
        }

        // Release voice_event_gate before DB/queue I/O. Any VoiceTick that
        // lands before session removal mutates the session below and is
        // carried into the post-leave tail flush.
        let stop_result = {
            let mut service = self.service.lock().await;
            let mut queue = self.queue.lock().await;
            stop_and_enqueue_summary_job_for_teardown(
                &mut service,
                &mut *queue,
                request.guild_key,
                request.caller_user_id,
                request.caller_role,
                Some(request.expected_meeting_id),
                request.reason,
            )
            .map_err(RecordingTeardownError::Stop)?
        };

        let removed_session = {
            let mut sessions = self.sessions.lock().await;
            remove_matching_recording_session_for_meeting(
                &mut sessions,
                request.guild_key,
                request.expected_meeting_id,
            )
        };
        {
            let mut states = self.auto_stop_states.lock().await;
            remove_auto_stop_state_for_meeting(
                &mut states,
                request.guild_key,
                request.expected_meeting_id,
            );
        }
        if let Some(session) = &removed_session {
            let mut titles = self.live_transcription_titles.lock().await;
            titles.remove(&session.meeting_id);
        }
        {
            let mut startups = self.recording_startups.lock().await;
            clear_matching_recording_startup(
                &mut startups,
                request.guild_key,
                request.expected_meeting_id,
            );
        }
        Ok((stop_result, removed_session))
    }

    async fn leave_after_recording_stop(
        &self,
        ctx: &Context,
        guild_key: &str,
        meeting_id: &str,
        phase: &str,
        mut removed_session: Option<RecordingSession<LocalChunkStorage>>,
        _reset_guard: tokio::sync::OwnedMutexGuard<()>,
    ) {
        // Stop has already won the DB transition, so leave voice even if
        // another cleanup path removed the local session first. Terminal
        // absence cleanup only leaves when it removed the matching session,
        // which avoids disturbing a later recording after local state drift.
        // The held reset guard also keeps a successor start from resetting
        // SSRCs and joining voice until this leave attempt has returned.
        if let Some(manager) = songbird::get(ctx).await {
            leave_voice_with_timeout(manager.as_ref(), self.guild_id, meeting_id, phase).await;
        }

        if let Some(session) = removed_session.as_mut()
            && let Err(err) = flush_removed_session_after_stop(session, guild_key, phase)
        {
            warn_removed_session_flush_failure(guild_key, phase, &err);
        }

        let final_tracker = {
            let _voice_event_guard = self.voice_event_gate.write().await;
            let tracker = self.ssrc_tracker.lock().await;
            tracker.clone()
        };
        if let Some(session) = &removed_session {
            session.persist_ssrc_mapping(&final_tracker);
        }
    }

    async fn remove_local_recording_state_after_terminal_absence(
        &self,
        guild_key: &str,
        expected_meeting_id: &str,
    ) -> Option<RecordingSession<LocalChunkStorage>> {
        let local = self.lifecycle_local_state();
        remove_local_recording_state_after_terminal_absence_with_dependencies(
            &local,
            guild_key,
            expected_meeting_id,
        )
        .await
    }

    async fn finish_terminal_absence_cleanup(
        &self,
        ctx: &Context,
        guild_id: GuildId,
        guild_key: &str,
        expected_meeting_id: &str,
        phase: &str,
        removed_session: Option<RecordingSession<LocalChunkStorage>>,
    ) {
        let local = self.lifecycle_local_state();
        let voice_leave = ContextRecordingVoiceLeave { ctx };
        finish_terminal_absence_cleanup_with_dependencies(
            &local,
            &voice_leave,
            TerminalAbsenceCleanupRequest {
                guild_id,
                guild_key,
                expected_meeting_id,
                phase,
            },
            removed_session,
            |session: &RecordingSession<LocalChunkStorage>, tracker| {
                session.persist_ssrc_mapping(tracker);
            },
        )
        .await;
    }

    async fn handle_terminal_cleanup_retry_failure(
        &self,
        request: TerminalCleanupRetryFailureRequest<'_>,
        terminal_cleanup_failures: &mut u32,
    ) -> TerminalCleanupRetryDecision<LocalChunkStorage> {
        let local = self.lifecycle_local_state();
        handle_terminal_cleanup_retry_failure_with_dependencies(
            &self.service,
            &local,
            request,
            terminal_cleanup_failures,
        )
        .await
    }

    async fn fail_recording_after_lookup_exhaustion(
        &self,
        permit: &RecordingLifecycleWritePermit<'_>,
        ctx: &Context,
        http: &Http,
        request: &RecordingLookupFailureRequest<'_>,
    ) -> Result<(), String> {
        self.fail_recording_after_teardown_exhaustion(
            permit,
            ctx,
            request.guild_id,
            request.guild_key,
            request.expected_meeting_id,
            request.terminal_error,
        )
        .await?;

        if let Err(status_err) = self
            .update_status_message(
                http,
                request.expected_meeting_id,
                StatusMessageUpdate::Failed {
                    phase: "Recording lookup",
                    error: request.terminal_error,
                },
            )
            .await
        {
            warn!(
                guild_id = %request.guild_key,
                meeting_id = request.expected_meeting_id,
                error = %status_err,
                context = request.context,
                "failed to notify recording lookup exhaustion"
            );
        }

        Ok(())
    }

    async fn fail_recording_after_teardown_exhaustion(
        &self,
        _permit: &RecordingLifecycleWritePermit<'_>,
        ctx: &Context,
        guild_id: GuildId,
        guild_key: &str,
        expected_meeting_id: &str,
        error_message: &str,
    ) -> Result<(), String> {
        let local = self.lifecycle_local_state();
        let voice_leave = ContextRecordingVoiceLeave { ctx };
        fail_recording_after_teardown_exhaustion_with_dependencies(
            &self.service,
            &local,
            &voice_leave,
            TerminalAbsenceCleanupRequest {
                guild_id,
                guild_key,
                expected_meeting_id,
                phase: "teardown exhaustion",
            },
            error_message,
            |session: &RecordingSession<LocalChunkStorage>, tracker| {
                session.persist_ssrc_mapping(tracker);
            },
        )
        .await?;
        Ok(())
    }

    async fn cleanup_failed_recording_start(
        &self,
        guild_key: &str,
        meeting_id: &str,
        error_message: &str,
    ) {
        let _command_guard = self.command_gate.write().await;
        self.cleanup_failed_recording_start_locked(guild_key, meeting_id, error_message)
            .await;
    }

    async fn cleanup_failed_recording_start_locked(
        &self,
        guild_key: &str,
        meeting_id: &str,
        error_message: &str,
    ) {
        self.cleanup_failed_recording_start_locked_with_scope(
            guild_key,
            meeting_id,
            error_message,
            FailedRecordingStartLocalCleanup::FullRuntimeState,
        )
        .await;
    }

    async fn cleanup_failed_recording_start_before_session_locked(
        &self,
        guild_key: &str,
        meeting_id: &str,
        error_message: &str,
    ) {
        self.cleanup_failed_recording_start_locked_with_scope(
            guild_key,
            meeting_id,
            error_message,
            FailedRecordingStartLocalCleanup::StartupOnly,
        )
        .await;
    }

    async fn cleanup_failed_recording_start_locked_with_scope(
        &self,
        guild_key: &str,
        meeting_id: &str,
        error_message: &str,
        cleanup_scope: FailedRecordingStartLocalCleanup,
    ) {
        if !self
            .try_cleanup_failed_recording_start_locked(
                guild_key,
                meeting_id,
                error_message,
                cleanup_scope,
            )
            .await
        {
            if self.shutting_down.load(Ordering::Acquire) {
                self.retry_failed_recording_start_cleanup_locked_inline(
                    guild_key,
                    meeting_id,
                    error_message,
                    cleanup_scope,
                )
                .await;
            } else {
                self.spawn_failed_recording_start_cleanup_retry(
                    guild_key.to_owned(),
                    meeting_id.to_owned(),
                    error_message.to_owned(),
                    cleanup_scope,
                );
            }
        }
    }

    async fn try_cleanup_failed_recording_start_locked(
        &self,
        guild_key: &str,
        meeting_id: &str,
        error_message: &str,
        cleanup_scope: FailedRecordingStartLocalCleanup,
    ) -> bool {
        let local = self.lifecycle_local_state();
        try_cleanup_failed_recording_start_with_dependencies(
            &self.service,
            &local,
            guild_key,
            meeting_id,
            error_message,
            cleanup_scope,
            |session: &RecordingSession<LocalChunkStorage>, tracker| {
                session.persist_ssrc_mapping(tracker);
            },
        )
        .await
    }

    fn spawn_failed_recording_start_cleanup_retry(
        &self,
        guild_key: String,
        meeting_id: String,
        error_message: String,
        cleanup_scope: FailedRecordingStartLocalCleanup,
    ) {
        let retry_key = format!("{guild_key}:{meeting_id}");
        {
            let mut retries = self
                .recording_start_cleanup_retries
                .lock()
                .expect("recording start cleanup retry set poisoned");
            if !retries.insert(retry_key.clone()) {
                debug!(
                    guild_id = %guild_key,
                    meeting_id = %meeting_id,
                    "record-start setup failure cleanup retry already scheduled"
                );
                return;
            }
        }

        let handler = self.clone();
        self.spawn_lifecycle_cleanup_retry(async move {
            let retry_key = retry_key;
            for attempt in 1..=RECORDING_START_CLEANUP_MAX_RETRIES {
                tokio::select! {
                    _ = handler.shutdown_token.cancelled() => {
                        debug!(
                            guild_id = %guild_key,
                            meeting_id = %meeting_id,
                            "record-start setup failure cleanup retry cancelled during shutdown"
                        );
                        handler.clear_recording_start_cleanup_retry(&retry_key);
                        return;
                    }
                    _ = sleep(RECORDING_START_CLEANUP_RETRY_DELAY) => {}
                }
                let _command_guard = tokio::select! {
                    guard = handler.command_gate.write() => guard,
                    _ = handler.shutdown_token.cancelled() => {
                        debug!(
                            guild_id = %guild_key,
                            meeting_id = %meeting_id,
                            "record-start setup failure cleanup retry skipped command gate during shutdown"
                        );
                        handler.clear_recording_start_cleanup_retry(&retry_key);
                        return;
                    }
                };
                if handler
                    .try_cleanup_failed_recording_start_locked(
                        &guild_key,
                        &meeting_id,
                        &error_message,
                        cleanup_scope,
                    )
                    .await
                {
                    info!(
                        guild_id = %guild_key,
                        meeting_id = %meeting_id,
                        attempt,
                        "record-start setup failure cleanup retry completed"
                    );
                    handler.clear_recording_start_cleanup_retry(&retry_key);
                    return;
                }
            }

            error!(
                guild_id = %guild_key,
                meeting_id = %meeting_id,
                attempts = RECORDING_START_CLEANUP_MAX_RETRIES,
                "record-start setup failure cleanup retries exhausted; force-marking failed before releasing startup reservation"
            );
            let _command_guard = tokio::select! {
                guard = handler.command_gate.write() => guard,
                _ = handler.shutdown_token.cancelled() => {
                    debug!(
                        guild_id = %guild_key,
                        meeting_id = %meeting_id,
                        "record-start setup failure cleanup exhaustion skipped command gate during shutdown"
                    );
                    handler.clear_recording_start_cleanup_retry(&retry_key);
                    return;
                }
            };
            let local = handler.lifecycle_local_state();
            finish_failed_recording_start_cleanup_retry_exhaustion_with_dependencies(
                &handler.service,
                &local,
                &guild_key,
                &meeting_id,
                &error_message,
                cleanup_scope,
                |session: &RecordingSession<LocalChunkStorage>, tracker| {
                    session.persist_ssrc_mapping(tracker);
                },
            )
            .await;
            handler.clear_recording_start_cleanup_retry(&retry_key);
        });
    }

    async fn retry_failed_recording_start_cleanup_locked_inline(
        &self,
        guild_key: &str,
        meeting_id: &str,
        error_message: &str,
        cleanup_scope: FailedRecordingStartLocalCleanup,
    ) {
        for attempt in 1..=RECORDING_START_CLEANUP_MAX_RETRIES {
            if self
                .try_cleanup_failed_recording_start_locked(
                    guild_key,
                    meeting_id,
                    error_message,
                    cleanup_scope,
                )
                .await
            {
                info!(
                    guild_id = %guild_key,
                    meeting_id = %meeting_id,
                    attempt,
                    "record-start setup failure cleanup retry completed during shutdown"
                );
                return;
            }
        }

        error!(
            guild_id = %guild_key,
            meeting_id = %meeting_id,
            attempts = RECORDING_START_CLEANUP_MAX_RETRIES,
            "record-start setup failure cleanup retries exhausted during shutdown; force-marking failed before releasing startup reservation"
        );
        let local = self.lifecycle_local_state();
        finish_failed_recording_start_cleanup_retry_exhaustion_with_dependencies(
            &self.service,
            &local,
            guild_key,
            meeting_id,
            error_message,
            cleanup_scope,
            |session: &RecordingSession<LocalChunkStorage>, tracker| {
                session.persist_ssrc_mapping(tracker);
            },
        )
        .await;
    }

    fn clear_recording_start_cleanup_retry(&self, retry_key: &str) {
        let mut retries = self
            .recording_start_cleanup_retries
            .lock()
            .expect("recording start cleanup retry set poisoned");
        retries.remove(retry_key);
    }

    async fn cleanup_voice_join_retry_after_failed_attempt(
        &self,
        manager: &songbird::Songbird,
        guild_id: GuildId,
        guild_key: &str,
        meeting_id: &str,
    ) -> VoiceJoinRetryCleanup {
        let _command_guard = self.command_gate.write().await;
        let current_session_meeting_id = {
            let sessions = self.sessions.lock().await;
            sessions
                .get(guild_key)
                .map(|session| session.meeting_id.clone())
        };
        let cleanup =
            classify_voice_join_retry_cleanup(current_session_meeting_id.as_deref(), meeting_id);

        if let Some(phase) = voice_join_retry_cleanup_leave_phase(cleanup) {
            leave_voice_with_timeout(manager, guild_id, meeting_id, phase).await;
        }
        if matches!(
            cleanup,
            VoiceJoinRetryCleanup::StopAfterSessionRemoved
                | VoiceJoinRetryCleanup::StopAfterSessionReplaced
        ) {
            self.cleanup_failed_recording_start_locked(
                guild_key,
                meeting_id,
                "recording session changed during voice join retry cleanup",
            )
            .await;
        }

        cleanup
    }

    async fn verify_recording_start_after_join(
        &self,
        manager: &songbird::Songbird,
        guild_id: GuildId,
        guild_key: &str,
        meeting_id: &str,
    ) -> Result<RecordingStartJoinVerification, String> {
        let error_message = {
            let _command_guard = self.command_gate.write().await;
            let shutdown_error = self.reject_if_shutting_down().err();
            let (active_matches, meeting_status, lookup_error) = {
                let mut service = self.service.lock().await;
                let (active_matches, mut lookup_error) =
                    match service.store.find_active_meeting_by_guild(guild_key) {
                        Ok(active) => (
                            active.is_some_and(|meeting| {
                                meeting.id == meeting_id
                                    && meeting.status == MeetingStatus::Recording
                            }),
                            None,
                        ),
                        Err(err) => (false, Some(err.to_string())),
                    };
                let meeting_status = match service.store.get_meeting(meeting_id) {
                    Ok(meeting) => meeting.map(|meeting| meeting.status),
                    Err(err) => {
                        if lookup_error.is_none() {
                            lookup_error = Some(err.to_string());
                        }
                        None
                    }
                };
                (active_matches, meeting_status, lookup_error)
            };
            let session_matches = {
                let sessions = self.sessions.lock().await;
                sessions
                    .get(guild_key)
                    .is_some_and(|session| session.meeting_id == meeting_id)
            };

            if shutdown_error.is_none() && active_matches && session_matches {
                let mut startups = self.recording_startups.lock().await;
                clear_matching_recording_startup(&mut startups, guild_key, meeting_id);
                return Ok(RecordingStartJoinVerification::Active);
            }
            if shutdown_error.is_none()
                && meeting_status.is_some_and(recording_start_join_completed_after_stop)
            {
                info!(
                    guild_id = %guild_key,
                    meeting_id,
                    status = ?meeting_status,
                    "recording was stopped before voice join verification completed"
                );
                let mut removed_session = {
                    let _voice_event_guard = self.voice_event_gate.write().await;
                    let mut sessions = self.sessions.lock().await;
                    if sessions
                        .get(guild_key)
                        .is_some_and(|session| session.meeting_id == meeting_id)
                    {
                        sessions.remove(guild_key)
                    } else {
                        None
                    }
                };
                // Only leave if this startup still owned the local session. If
                // the stop path already removed it and issued its own leave, a
                // second leave could disconnect a successor recording that joined
                // while this startup was in the join retry loop.
                if removed_session.is_some() {
                    leave_voice_with_timeout(
                        manager,
                        guild_id,
                        meeting_id,
                        "already-stopped record-start",
                    )
                    .await;
                }
                if let Some(session) = removed_session.as_mut()
                    && let Err(err) = flush_removed_session_after_stop(
                        session,
                        guild_key,
                        "already-stopped record-start",
                    )
                {
                    warn_removed_session_flush_failure(
                        guild_key,
                        "already-stopped record-start",
                        &err,
                    );
                }
                let latest_tracker = {
                    let _voice_event_guard = self.voice_event_gate.write().await;
                    let tracker = self.ssrc_tracker.lock().await;
                    tracker.clone()
                };
                if let Some(session) = &removed_session {
                    session.persist_ssrc_mapping(&latest_tracker);
                }
                {
                    let mut states = self.auto_stop_states.lock().await;
                    remove_auto_stop_state_for_meeting(&mut states, guild_key, meeting_id);
                }
                {
                    let mut titles = self.live_transcription_titles.lock().await;
                    titles.remove(meeting_id);
                }
                let mut startups = self.recording_startups.lock().await;
                clear_matching_recording_startup(&mut startups, guild_key, meeting_id);
                return Ok(RecordingStartJoinVerification::AlreadyStopped);
            }
            if shutdown_error.is_none() && lookup_error.is_some() && session_matches {
                warn!(
                    guild_id = %guild_key,
                    meeting_id,
                    error = %lookup_error.as_deref().unwrap_or("unknown store error"),
                    "could not verify active recording after voice join; keeping joined recording because session still matches"
                );
                // Keep the startup reservation while the DB cannot confirm
                // the row. Stop/cleanup paths still use it as a local handle.
                return Ok(RecordingStartJoinVerification::Active);
            }

            let error_message = shutdown_error
                .or(lookup_error)
                .unwrap_or_else(|| "recording changed before voice join completed".to_owned());

            // Keep the lifecycle gate held until this leave and cleanup
            // complete so a follow-up start cannot join and then be kicked by
            // this cleanup.
            leave_voice_with_timeout(manager, guild_id, meeting_id, "record-start verification")
                .await;
            self.cleanup_failed_recording_start_locked(guild_key, meeting_id, &error_message)
                .await;
            error_message
        };

        Err(error_message)
    }

    async fn handle_command(&self, ctx: &Context, command: &CommandInteraction) -> String {
        run_guild_scoped_command(command.guild_id, self.guild_id, |_| async {
            self.reject_if_shutting_down()?;
            match command.data.name.as_str() {
                RECORD_START_COMMAND => self.handle_record_start(ctx, command).await,
                RECORD_STOP_COMMAND => self.handle_record_stop(ctx, command).await,
                _ => Err("unsupported command".to_owned()),
            }
        })
        .await
    }

    /// Register voice event handlers on the Call for this guild.
    /// Uses `remove_all_global_events` first to ensure a clean slate
    /// (avoids handler accumulation across consecutive recordings).
    async fn register_voice_handlers(
        &self,
        manager: &songbird::Songbird,
        ctx: &Context,
        guild_id: GuildId,
    ) {
        let call = manager.get_or_insert(guild_id);
        let mut lock = call.lock().await;
        lock.remove_all_global_events();
        let voice_handler = VoiceReceiveHandler {
            tracker: Arc::clone(&self.ssrc_tracker),
            sessions: Arc::clone(&self.sessions),
            guild_id: guild_id.get().to_string(),
            runtime: self.clone(),
            http: Arc::clone(&ctx.http),
            ctx: ctx.clone(),
        };
        lock.add_global_event(
            Event::Core(CoreEvent::SpeakingStateUpdate),
            voice_handler.clone(),
        );
        lock.add_global_event(Event::Core(CoreEvent::VoiceTick), voice_handler.clone());
        lock.add_global_event(Event::Core(CoreEvent::DriverDisconnect), voice_handler);
    }

    async fn handle_record_start(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
    ) -> Result<String, String> {
        self.reject_if_shutting_down()?;
        let guild_id = validate_command_guild(command.guild_id, self.guild_id)?;
        let guild_key = guild_id.get().to_string();
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

        let command_guard = self.command_gate.write().await;
        self.reject_if_shutting_down()?;
        {
            let startups = self.recording_startups.lock().await;
            if let Some(err) = recording_startup_conflict(&startups, &guild_key) {
                return Err(err.to_string());
            }
        }
        let manager = songbird::get(ctx)
            .await
            .ok_or_else(|| "songbird not initialized".to_owned())?;
        let response = {
            let mut service = self.service.lock().await;
            let preflight = validate_record_start_preconditions(
                &mut service.store,
                &RecordStartRequest {
                    meeting_id: meeting_id.clone(),
                    guild_id: guild_key.clone(),
                    started_by_user_id: command.user.id.get().to_string(),
                    command_channel_id: command.channel_id.get().to_string(),
                    user_voice_channel_id: Some(voice_channel_id_u64.to_string()),
                    permissions,
                    caller_role,
                    effective_settings: None,
                },
            )
            .map_err(|err| err.to_string())?;
            let defaults = self.meeting_settings_defaults();
            let guild_settings = service
                .store
                .get_guild_settings_for_meeting_snapshot(&guild_key)
                .map_err(|err| err.to_string())?;
            let effective_settings =
                EffectiveMeetingSettings::resolve(&defaults, guild_settings.as_ref());
            complete_record_start_after_runtime_setup(
                &mut service,
                StartCommandInput {
                    meeting_id: meeting_id.clone(),
                    guild_id: guild_key.clone(),
                    user_id: command.user.id.get().to_string(),
                    command_channel_id: command.channel_id.get().to_string(),
                    user_voice_channel_id: Some(voice_channel_id_u64.to_string()),
                    permissions,
                    caller_role,
                    effective_settings: Some(effective_settings),
                },
                preflight,
            )?
        };
        {
            let mut startups = self.recording_startups.lock().await;
            startups.insert(guild_key.clone(), meeting_id.clone());
        }
        let layout =
            crate::infrastructure::workspace::MeetingWorkspaceLayout::new(&self.chunk_storage_dir);
        let workspace =
            layout.for_meeting(&guild_key, &voice_channel_id_u64.to_string(), &meeting_id);
        drop(command_guard);
        if let Err(err) = workspace.ensure_base_dirs() {
            let _command_guard = self.command_gate.write().await;
            if let Err(err_msg) = self.reject_if_shutting_down() {
                self.cleanup_failed_recording_start_before_session_locked(
                    &guild_key,
                    &meeting_id,
                    &err_msg,
                )
                .await;
                return Err(err_msg);
            }
            let err_msg = format!("failed to prepare workspace: {err}");
            // Only the DB row and startup reservation exist here; cleanup
            // terminalizes the row and clears that reservation.
            self.cleanup_failed_recording_start_before_session_locked(
                &guild_key,
                &meeting_id,
                &err_msg,
            )
            .await;
            return Err(err_msg);
        }

        let meeting_title = {
            let mut service = self.service.lock().await;
            match service.store.get_meeting(&meeting_id) {
                Ok(Some(meeting)) => meeting.title,
                Ok(None) => None,
                Err(err) => {
                    warn!(
                        meeting_id = %meeting_id,
                        error = %err,
                        "failed to cache meeting title for live transcription prompt"
                    );
                    None
                }
            }
        };

        let command_guard = self.command_gate.write().await;
        if let Err(err_msg) = self.reject_if_shutting_down() {
            self.cleanup_failed_recording_start_before_session_locked(
                &guild_key,
                &meeting_id,
                &err_msg,
            )
            .await;
            return Err(err_msg);
        }
        let setup_still_recording = {
            let mut service = self.service.lock().await;
            match service.store.find_active_meeting_by_guild(&guild_key) {
                Ok(Some(meeting)) => {
                    Ok(meeting.id == meeting_id && meeting.status == MeetingStatus::Recording)
                }
                Ok(None) => Ok(false),
                Err(err) => Err(format!(
                    "failed to verify recording after audio setup wait: {err}"
                )),
            }
        };
        let setup_still_recording = match setup_still_recording {
            Ok(setup_still_recording) => setup_still_recording,
            Err(err_msg) => {
                self.cleanup_failed_recording_start_before_session_locked(
                    &guild_key,
                    &meeting_id,
                    &err_msg,
                )
                .await;
                return Err(err_msg);
            }
        };
        if !setup_still_recording {
            self.cleanup_failed_recording_start_before_session_locked(
                &guild_key,
                &meeting_id,
                "recording no longer active after audio setup",
            )
            .await;
            return Ok(
                "参加準備中に停止処理が始まりました。停止処理の完了を待っています。".to_owned(),
            );
        }
        // The startup reservation cannot change here: command_guard is held,
        // and every clearer for this guild also takes command_gate.write().

        // Reset SSRC tracker so stale mappings from previous recordings
        // cannot mis-attribute audio when Discord reuses an SSRC value.
        {
            let _reset_guard = Arc::clone(&self.ssrc_tracker_reset_gate).lock_owned().await;
            let _voice_event_guard = self.voice_event_gate.write().await;
            let mut tracker = self.ssrc_tracker.lock().await;
            *tracker = SsrcTracker::new();
        }
        self.spawn_record_start_entitlement_observation(guild_key.clone());

        {
            let mut bases = self.live_transcription_bases.lock().await;
            bases.remove(&meeting_id);
        }
        // Insert session BEFORE joining VC so voice events aren't dropped
        {
            let mut sessions = self.sessions.lock().await;
            sessions.insert(
                guild_key.clone(),
                RecordingSession::new(
                    meeting_id.clone(),
                    LocalChunkStorage::new(workspace.clone(), meeting_id.clone()),
                    ReceiverConfig::default(),
                    48_000,
                ),
            );
        }
        {
            let mut titles = self.live_transcription_titles.lock().await;
            titles.insert(meeting_id.clone(), meeting_title);
        }

        // Keep the SSRC reset gate scoped to the tracker reset only. Handler
        // registration awaits Songbird work and should not hold the reset
        // gate across that I/O; command_guard continues to serialize the
        // recording lifecycle until session/handler setup is complete. This
        // intentionally blocks concurrent stop/teardown while the local session
        // is becoming visible, but it can also wait behind Songbird's Call mutex
        // if a voice event is in flight.
        // Register handlers BEFORE voice WS connects to capture initial SSRC
        // mappings (SpeakingStateUpdate for users already speaking).
        self.register_voice_handlers(manager.as_ref(), ctx, guild_id)
            .await;
        drop(command_guard);

        let _call = {
            let channel_id = ChannelId::new(voice_channel_id_u64);
            let mut join_delay = Duration::from_millis(500);
            let mut last_err = None;
            let mut result = None;
            for attempt in 1..=3u32 {
                match timeout(
                    RECORDING_VOICE_JOIN_TIMEOUT,
                    manager.join(guild_id, channel_id),
                )
                .await
                {
                    Ok(Ok(call)) => {
                        result = Some(call);
                        break;
                    }
                    Ok(Err(err)) => {
                        let err_msg = format!("{err}");
                        warn!(
                            attempt,
                            guild_id = %guild_id.get(),
                            meeting_id = %meeting_id,
                            error = %err,
                            error_debug = ?err,
                            "voice join attempt failed"
                        );
                        last_err = Some(err_msg);
                        // Clean up partial gateway state before retrying.
                        if self
                            .cleanup_voice_join_retry_after_failed_attempt(
                                manager.as_ref(),
                                guild_id,
                                &guild_key,
                                &meeting_id,
                            )
                            .await
                            != VoiceJoinRetryCleanup::RetryCurrentSession
                        {
                            return Ok(
                                "参加再試行中に停止処理が始まりました。停止処理の完了を待っています。".to_owned(),
                            );
                        }
                        // Re-register after leave in case it cleared the Call's
                        // event handlers (defensive: Songbird docs say handlers
                        // survive leave(), but guard against implementation drift).
                        self.register_voice_handlers(manager.as_ref(), ctx, guild_id)
                            .await;
                        if attempt < 3 {
                            sleep(join_delay).await;
                            join_delay *= 2;
                        }
                    }
                    Err(_) => {
                        let err_msg = format!(
                            "timed out joining voice channel after {}ms",
                            RECORDING_VOICE_JOIN_TIMEOUT.as_millis()
                        );
                        warn!(
                            attempt,
                            guild_id = %guild_id.get(),
                            meeting_id = %meeting_id,
                            timeout_ms = RECORDING_VOICE_JOIN_TIMEOUT.as_millis(),
                            "voice join attempt timed out"
                        );
                        last_err = Some(err_msg);
                        // Clean up partial gateway state before retrying.
                        if self
                            .cleanup_voice_join_retry_after_failed_attempt(
                                manager.as_ref(),
                                guild_id,
                                &guild_key,
                                &meeting_id,
                            )
                            .await
                            != VoiceJoinRetryCleanup::RetryCurrentSession
                        {
                            return Ok(
                                "参加再試行中に停止処理が始まりました。停止処理の完了を待っています。".to_owned(),
                            );
                        }
                        // Re-register after leave in case it cleared the Call's
                        // event handlers (defensive: Songbird docs say handlers
                        // survive leave(), but guard against implementation drift).
                        self.register_voice_handlers(manager.as_ref(), ctx, guild_id)
                            .await;
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
                    let err_msg = last_err.expect("last_err must be set when all attempts fail");
                    error!(
                        guild_id = %guild_id.get(),
                        meeting_id = %meeting_id,
                        error = %err_msg,
                        "failed to join voice channel after 3 attempts"
                    );
                    // manager.leave() already called in the retry loop above
                    self.cleanup_failed_recording_start(&guild_key, &meeting_id, &err_msg)
                        .await;
                    return Err(err_msg);
                }
            }
        };
        let join_verification = self
            .verify_recording_start_after_join(manager.as_ref(), guild_id, &guild_key, &meeting_id)
            .await?;
        if join_verification == RecordingStartJoinVerification::AlreadyStopped {
            return Ok(
                "参加完了前に停止処理が始まりました。停止処理の完了を待っています。".to_owned(),
            );
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
        self.reject_if_shutting_down()?;
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

        let (stop_result, removed_session, authorized_meeting_id, reset_guard) = {
            let lifecycle_permit = self.recording_lifecycle_write_permit().await;
            self.reject_if_shutting_down()?;
            let mut service = self.service.lock().await;
            let meeting = service
                .store
                .find_active_meeting_by_guild(&guild_key)
                .map_err(|err| err.to_string())?
                .ok_or_else(|| CommandError::NoActiveMeeting.to_string())?;
            authorize_record_stop_for_meeting(&meeting, &caller_user_id, caller_role)
                .map_err(|err| err.to_string())?;
            let authorized_meeting_id = meeting.id;
            drop(service);

            let request = RecordingStopTeardownRequest {
                guild_key: &guild_key,
                caller_user_id: &caller_user_id,
                caller_role,
                expected_meeting_id: &authorized_meeting_id,
                reason: StopReason::Manual,
                phase: "manual stop",
            };
            let (stop_result, removed_session) = self
                .prepare_recording_stop_after_teardown(&lifecycle_permit, &request)
                .await
                .map_err(|err| err.to_string())?;
            let reset_guard = Arc::clone(&self.ssrc_tracker_reset_gate).lock_owned().await;
            (
                stop_result,
                removed_session,
                authorized_meeting_id,
                reset_guard,
            )
        };
        self.leave_after_recording_stop(
            ctx,
            &guild_key,
            &authorized_meeting_id,
            "manual stop",
            removed_session,
            reset_guard,
        )
        .await;

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
                let shutdown_token = handler.shutdown_token.clone();
                self.spawn_background(async move {
                    tokio::select! {
                        result = run_summary_background(&handler, &http, &meeting_id) => {
                            if let Err(err) = result {
                                error!(meeting_id = %meeting_id, error = %err, "summary background task failed");
                            }
                        }
                        _ = shutdown_token.cancelled() => {
                            debug!(meeting_id = %meeting_id, "summary background task deferred by shutdown");
                        }
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
                let summary_url = self.meeting_url(meeting_id);
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
                let mut summary_job_done = true;
                let job_id = format!("summary-{meeting_id}");
                {
                    let mut queue = self.queue.lock().await;
                    if let Err(err) = queue.mark_done(&job_id) {
                        error!(
                            job_id = %job_id,
                            meeting_id = %meeting_id,
                            error = %err,
                            "failed to mark summary job as done — job may be re-processed on restart"
                        );
                        summary_job_done = false;
                    }
                }
                if summary_job_done {
                    self.record_summary_run_usage(meeting_id, &job_id, chunks.len())
                        .await;
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

    async fn record_summary_run_usage(&self, meeting_id: &str, job_id: &str, chunk_count: usize) {
        let guild_id = {
            let mut service = self.service.lock().await;
            let meeting = match service.store.get_meeting(meeting_id) {
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
            let guild_id = meeting.guild_id.clone();
            let event = NewUsageEvent {
                id: format!("usage:summary_runs:{meeting_id}"),
                tenant_id: None,
                guild_id: guild_id.clone(),
                meeting_id: Some(meeting_id.to_owned()),
                job_id: Some(job_id.to_owned()),
                resource_type: Some("meeting".to_owned()),
                resource_id: Some(meeting_id.to_owned()),
                metric: UsageMetric::SummaryRuns,
                quantity: 1,
                detail_json: UsageDetailJson::new(serde_json::json!({
                    "chunk_count": chunk_count,
                    "surface": "runtime_post_success"
                }))
                .expect("usage detail must be a JSON object"),
                observed_at: Utc::now(),
            };
            if let Err(err) = service.store.append_usage_event(&event) {
                warn!(
                    meeting_id,
                    usage_event_id = %event.id,
                    error = %err,
                    "failed to append summary usage event; continuing in observe-only mode"
                );
            }
            guild_id
        };
        // Observe-only entitlement checks are intentionally asynchronous here:
        // they must not add an aggregate query to the just-finished usage write path.
        self.spawn_worker_completion_entitlement_observation(guild_id);
    }

    fn spawn_worker_completion_entitlement_observation(&self, guild_id: String) {
        let service = Arc::clone(&self.service);
        tokio::spawn(async move {
            let mut service = service.lock().await;
            crate::application::worker::observe_worker_completion_entitlement(
                &mut service.store,
                &guild_id,
            );
        });
    }

    fn spawn_record_start_entitlement_observation(&self, guild_id: String) {
        let service = Arc::clone(&self.service);
        tokio::spawn(async move {
            let mut service = service.lock().await;
            let aggregates =
                match service
                    .store
                    .aggregate_recent_usage(None, Some(&guild_id), 30 * 24 * 60 * 60)
                {
                    Ok(aggregates) => aggregates,
                    Err(err) => {
                        warn!(
                            guild_id,
                            error = %err,
                            "usage entitlement observation failed before recording start"
                        );
                        return;
                    }
                };
            let snapshot = UsageSnapshot::from_aggregates(aggregates);
            let decision = EntitlementEvaluator::observe_only()
                .evaluate(EntitlementAction::StartRecording, &snapshot);
            if decision
                .observations
                .iter()
                .any(|observation| observation.exceeded)
            {
                warn!(
                    guild_id,
                    observations = ?decision.observations,
                    "usage entitlement would exceed policy; observe-only mode allows recording"
                );
            }
        });
    }

    async fn record_asr_seconds_usage(
        &self,
        guild_id: &str,
        meeting_id: &str,
        job_id: &str,
        audio_path: &str,
        transcription: &crate::application::summary::TranscriptionOutput,
    ) {
        let audio_path_for_read = audio_path.to_owned();
        let quantity = match tokio::task::spawn_blocking(move || {
            crate::application::worker::asr_seconds_from_audio_path(&audio_path_for_read)
        })
        .await
        {
            Ok(Ok(quantity)) => quantity,
            Ok(Err(err)) => {
                warn!(
                    meeting_id,
                    audio_path,
                    error = %err,
                    "skipping ASR usage event because audio duration is unavailable"
                );
                return;
            }
            Err(err) => {
                warn!(
                    meeting_id,
                    audio_path,
                    error = %err,
                    "skipping ASR usage event because audio duration task failed"
                );
                return;
            }
        };
        let event = NewUsageEvent {
            id: format!("usage:asr_seconds:{meeting_id}"),
            tenant_id: None,
            guild_id: guild_id.to_owned(),
            meeting_id: Some(meeting_id.to_owned()),
            job_id: Some(job_id.to_owned()),
            resource_type: Some("meeting".to_owned()),
            resource_id: Some(meeting_id.to_owned()),
            metric: UsageMetric::AsrSeconds,
            quantity,
            detail_json: UsageDetailJson::new(serde_json::json!({
                "source": "audio_duration",
                "whisper_segment_count": transcription.segments.len(),
                "surface": "runtime_transcription_success"
            }))
            .expect("usage detail must be a JSON object"),
            observed_at: Utc::now(),
        };
        let mut service = self.service.lock().await;
        if let Err(err) = service.store.append_usage_event(&event) {
            warn!(
                meeting_id,
                usage_event_id = %event.id,
                error = %err,
                "failed to append ASR usage event; continuing in observe-only mode"
            );
        }
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
        let effective_settings = self.effective_settings_for_meeting(meeting_id).await?;
        let whisper = CommandWhisperClient {
            endpoint: self.whisper_endpoint.clone(),
            curl_bin: "curl".to_owned(),
            retry_policy: self.integration_retry_policy,
            beam_size: effective_settings.whisper_beam_size,
            suppress_non_speech: effective_settings.whisper_suppress_non_speech,
            prompt: effective_settings.whisper_prompt.clone(),
            vad: effective_settings.whisper_vad,
            temperature: effective_settings.whisper_temperature,
            command_timeout: DEFAULT_COMMAND_TIMEOUT,
        };
        let summary_client = HarnessCliSummaryClient {
            harness: self.summary_harness,
            command_path: self.summary_command.clone(),
            model: self.summary_model.clone(),
            allow_unsafe_agent_harness: self.summary_allow_unsafe_agent_harness,
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

        let heartbeat_job_id = claimed_job.id.clone();
        let heartbeat_queue = self.queue.clone();
        let heartbeat_shutdown = self.shutdown_token.clone();
        let heartbeat_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            interval.tick().await;
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let result = {
                            let mut queue = heartbeat_queue.lock().await;
                            queue.executor.execute(
                                HEARTBEAT_RUNNING_JOB_SQL,
                                std::slice::from_ref(&heartbeat_job_id),
                            )
                        };
                        if let Err(err) = result {
                            warn!(
                                job_id = %heartbeat_job_id,
                                error = %err,
                                "failed to refresh summary job lease heartbeat"
                            );
                        }
                    }
                    _ = heartbeat_shutdown.cancelled() => break,
                }
            }
        });
        struct SummaryJobHeartbeatGuard(tokio::task::JoinHandle<()>);
        impl Drop for SummaryJobHeartbeatGuard {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _heartbeat_guard = SummaryJobHeartbeatGuard(heartbeat_task);

        let audio_path = match merge_user_chunks_to_mixdown(
            &meeting_dir,
            effective_settings.whisper_resample_to_16k,
        ) {
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

        let final_timeline_base_ms = match load_chunks(&meeting_dir) {
            Ok(chunks) => compute_meeting_start_ms(&chunks),
            Err(err) => {
                warn!(
                    meeting_id = %meeting.id,
                    error = %err,
                    "failed to compute final transcript timeline base; live transcript rows will not be timeline-adjusted"
                );
                0
            }
        };

        let (live_segments, completed_live_chunks) = {
            let mut service = self.service.lock().await;
            let live_segments = match load_live_transcript_segments(
                &mut service.store.executor,
                &meeting.id,
                final_timeline_base_ms,
            ) {
                Ok(value) => value,
                Err(err) => {
                    warn!(
                        meeting_id = %meeting.id,
                        error = %err,
                        "failed to load live transcript segments; final transcription will process all audio"
                    );
                    Vec::new()
                }
            };
            let completed_live_chunks = match load_completed_live_transcription_chunks(
                &mut service.store.executor,
                &meeting.id,
            ) {
                Ok(value) => value,
                Err(err) => {
                    warn!(
                        meeting_id = %meeting.id,
                        error = %err,
                        "failed to load completed live chunks; final transcription will process all audio"
                    );
                    Vec::new()
                }
            };
            (live_segments, completed_live_chunks)
        };

        let speaker_audio = match if completed_live_chunks.is_empty() {
            build_speaker_audio_inputs(&meeting_dir, effective_settings.whisper_resample_to_16k)
        } else {
            build_speaker_audio_inputs_excluding_processed_chunks(
                &meeting_dir,
                effective_settings.whisper_resample_to_16k,
                &completed_live_chunks,
            )
        } {
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
            language: effective_settings.whisper_language.clone(),
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

        let transcription = if request.speaker_audio.is_empty() && !completed_live_chunks.is_empty()
        {
            crate::application::summary::build_transcription_output(live_segments.clone())
        } else {
            tokio::task::block_in_place(|| {
                crate::application::summary::run_transcription(&whisper, &request)
            })
            .and_then(|mut output| {
                if live_segments.is_empty() {
                    return Ok(output);
                }
                output.segments.extend(live_segments.clone());
                sort_transcript_segments(&mut output.segments);
                crate::application::summary::build_transcription_output(output.segments)
            })
        };
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

        self.record_asr_seconds_usage(
            &meeting.guild_id,
            &claimed_job.meeting_id,
            &claimed_job.id,
            &request.audio_path,
            &transcription,
        )
        .await;

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
                        sort_transcript_segments(&mut transcription.segments);
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
        let mut summary_context_speakers = HashMap::new();
        if !transcription.segments.is_empty() {
            summary_context_speakers = self
                .resolve_and_upsert_speakers(http, &claimed_job.meeting_id, &transcription.segments)
                .await;
            for segment in &transcription.segments {
                summary_context_speakers
                    .entry(segment.speaker_id.clone())
                    .or_insert_with(|| SpeakerProfile {
                        speaker_id: segment.speaker_id.clone(),
                        username: None,
                        nickname: None,
                        display_name: None,
                    });
            }
            let rendered = crate::domain::transcript::render_for_summary(
                &transcription.segments,
                Some(&summary_context_speakers),
            );
            let masked = crate::domain::privacy::mask_pii(&rendered);
            summary_transcript = masked.text;
            summary_masking_stats = masked.stats;
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
                    effective_settings.whisper_language.as_deref(),
                )
                .unwrap_or_else(|| {
                    crate::application::summary::build_correction_prompt(
                        &summary_transcript,
                        effective_settings.whisper_language.as_deref(),
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
            let mut summary_context = {
                let mut service = self.service.blocking_lock();
                load_runtime_summary_context(
                    &mut service.store,
                    &claimed_job.meeting_id,
                    &meeting.guild_id,
                    &effective_settings,
                )?
            };
            summary_context.speakers = summary_context_speakers
                .values()
                .cloned()
                .collect::<Vec<_>>();
            let context_manifest =
                crate::application::summary::materialize_or_load_summary_context(
                    &request,
                    &summary_context,
                )?;
            let prompt = crate::application::summary::build_summary_prompt_with_context(
                &request,
                &manifest,
                Some(&context_manifest),
            );
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
            .and_then(|row| row.first().cloned().flatten());

        let mut profiles = HashMap::new();
        for row in speaker_rows {
            if row.len() < 4 {
                continue;
            }
            let Some(speaker_id) = row.first().and_then(|v| v.clone()) else {
                continue;
            };
            let profile = SpeakerProfile {
                speaker_id: speaker_id.clone(),
                username: row.get(1).and_then(|v| v.clone()),
                nickname: row.get(2).and_then(|v| v.clone()),
                display_name: row.get(3).and_then(|v| v.clone()),
            };
            profiles.insert(profile.speaker_id.clone(), profile);
        }

        (guild_id, profiles)
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
        if self.runtime.shutting_down.load(Ordering::Acquire) {
            return None;
        }
        let _voice_event_guard = self.runtime.voice_event_gate.read().await;
        if self.runtime.shutting_down.load(Ordering::Acquire) {
            return None;
        }
        match ctx {
            EventContext::SpeakingStateUpdate(evt) => {
                if let Some(user_id) = evt.user_id {
                    let user_id_u64 = user_id.0;
                    let user_id_str = user_id_u64.to_string();
                    let (should_persist_mapping, snapshot_tracker) = {
                        let mut tracker = self.tracker.lock().await;
                        let previous_user = tracker.resolve_user(evt.ssrc).map(ToOwned::to_owned);
                        tracker.update_mapping(evt.ssrc, user_id_u64);
                        let changed = previous_user != Some(user_id_str.clone());
                        (changed, tracker.clone())
                    };

                    // Re-key any in-memory frames buffered under the SSRC fallback ID
                    // `should_persist_mapping` can be false for repeated updates
                    // when the mapping already points to this user. In that case,
                    // frames are expected to already be keyed to the user ID for
                    // the current session, so no re-key/persist is needed.
                    let ssrc_key = SsrcTracker::fallback_key(evt.ssrc);
                    if should_persist_mapping {
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
                            session.persist_ssrc_mapping(&snapshot_tracker);
                        }
                    }
                }
            }
            EventContext::VoiceTick(tick) => {
                let ts = now_ms();
                let tracker = self.tracker.lock().await;
                let adapted = adapt_voice_tick(tick, ts, &tracker);
                drop(tracker);
                let persisted_chunks = {
                    let mut sessions = self.sessions.lock().await;
                    if let Some(session) = sessions.get_mut(&self.guild_id) {
                        match ingest_voice_frames_into_session(session, &adapted) {
                            Ok(chunks) => chunks,
                            Err(err) => {
                                warn!(guild_id = %self.guild_id, error = %err, "failed to ingest voice tick");
                                Vec::new()
                            }
                        }
                    } else {
                        Vec::new()
                    }
                };
                if !persisted_chunks.is_empty()
                    && let Some(base_start_ms) = self
                        .runtime
                        .live_transcription_base_for_chunks(&persisted_chunks)
                        .await
                {
                    let meeting_title = self
                        .runtime
                        .live_transcription_title(&persisted_chunks[0].meeting_id)
                        .await;
                    self.runtime.spawn_live_transcription_tasks(
                        persisted_chunks,
                        base_start_ms,
                        meeting_title,
                    );
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
                    let disconnected_channel_id = data.channel_id.0.get();
                    let expected_meeting_id = match runtime
                        .active_meeting_voice_channel_result()
                        .await
                    {
                        Ok(Some(active_voice_channel))
                            if active_voice_channel.voice_channel_id == disconnected_channel_id =>
                        {
                            Some(active_voice_channel.meeting_id)
                        }
                        Ok(Some(active_voice_channel)) => {
                            warn!(
                                guild_id = %self.guild_id,
                                active_meeting_id = %active_voice_channel.meeting_id,
                                active_voice_channel_id = active_voice_channel.voice_channel_id,
                                disconnected_channel_id,
                                "driver-disconnect channel no longer matches the active recording; skipping targeted teardown"
                            );
                            None
                        }
                        Ok(None) => None,
                        Err(err) => {
                            warn!(
                                guild_id = %self.guild_id,
                                error = %err,
                                disconnected_channel_id,
                                "failed to resolve active meeting for driver disconnect; will retry without local-session fallback"
                            );
                            None
                        }
                    };
                    let grace = runtime
                        .auto_stop_grace_for_meeting(expected_meeting_id.as_deref())
                        .await;
                    self.runtime.spawn_background(async move {
                        // Driver-disconnect has no timer state to consult after
                        // grace expiry, so the counters below bound only their
                        // own failure classes before terminal cleanup is tried.
                        let mut final_flush_failures = 0u32;
                        let mut grace_cache_misses = 0u32;
                        let mut lookup_failures = 0u32;
                        let mut stop_failures = 0u32;
                        let mut terminal_cleanup_failures = 0u32;
                        let stop_result = loop {
                            tokio::select! {
                                _ = sleep(grace) => {}
                                _ = runtime.shutdown_token.cancelled() => {
                                    return;
                                }
                            }
                            let lifecycle_permit =
                                runtime.recording_lifecycle_write_permit().await;
                            if runtime.shutting_down.load(Ordering::Acquire) {
                                return;
                            }
                            if expected_meeting_id.is_none() {
                                match runtime.active_meeting_voice_channel_result().await {
                                    Ok(Some(active_voice_channel))
                                        if active_voice_channel.voice_channel_id
                                            == disconnected_channel_id =>
                                    {
                                        warn!(
                                            guild_id = %guild_key,
                                            meeting_id = %active_voice_channel.meeting_id,
                                            disconnected_channel_id,
                                            "driver-disconnect target was unavailable at disconnect time; skipping teardown to avoid stopping a later recording"
                                        );
                                    }
                                    Ok(Some(active_voice_channel)) => {
                                        warn!(
                                            guild_id = %guild_key,
                                            active_meeting_id = %active_voice_channel.meeting_id,
                                            active_voice_channel_id = active_voice_channel.voice_channel_id,
                                            disconnected_channel_id,
                                            "driver-disconnect retry found a different active voice channel; skipping teardown"
                                        );
                                    }
                                    Ok(None) => {}
                                    Err(err) => {
                                        let lookup_error = err.to_string();
                                        let terminal_error = recording_lookup_terminal_error(
                                            &mut lookup_failures,
                                            "driver-disconnect initial target",
                                            &lookup_error,
                                        );
                                        warn!(
                                            guild_id = %guild_key,
                                            disconnected_channel_id,
                                            error = %err,
                                            attempts = lookup_failures,
                                            "failed to resolve driver-disconnect target after grace; rescheduling"
                                        );
                                        if terminal_error.is_none() {
                                            continue;
                                        }
                                        let terminal_error = terminal_error
                                            .expect("checked terminal error presence above");
                                        warn!(
                                            guild_id = %guild_key,
                                            disconnected_channel_id,
                                            attempts = lookup_failures,
                                            "driver-disconnect target lookup retry limit reached; resolving active meeting for terminal cleanup"
                                        );
                                        match runtime.active_meeting_id_result().await {
                                            Ok(Some(active_meeting_id)) => {
                                                match runtime
                                                    .fail_recording_after_lookup_exhaustion(
                                                        &lifecycle_permit,
                                                        &ctx_for_task,
                                                        http.as_ref(),
                                                        &RecordingLookupFailureRequest {
                                                            guild_id: runtime.guild_id,
                                                            guild_key: &guild_key,
                                                            expected_meeting_id: &active_meeting_id,
                                                            terminal_error: &terminal_error,
                                                            context:
                                                                "driver-disconnect initial target",
                                                        },
                                                    )
                                                    .await
                                                {
                                                    Ok(()) => return,
                                                    Err(mark_err) => {
                                                        warn!(
                                                            guild_id = %guild_key,
                                                            meeting_id = %active_meeting_id,
                                                            error = %mark_err,
                                                            "failed to mark recording failed after driver-disconnect initial target exhaustion; rescheduling"
                                                        );
                                                        if let TerminalCleanupRetryDecision::Cleared {
                                                            removed_session,
                                                        } = runtime
                                                            .handle_terminal_cleanup_retry_failure(
                                                                TerminalCleanupRetryFailureRequest {
                                                                    guild_key: &guild_key,
                                                                    expected_meeting_id:
                                                                        &active_meeting_id,
                                                                    phase:
                                                                        "driver-disconnect initial target",
                                                                    err: &mark_err,
                                                                },
                                                                &mut terminal_cleanup_failures,
                                                            )
                                                            .await
                                                        {
                                                            drop(lifecycle_permit);
                                                            runtime
                                                                .finish_terminal_absence_cleanup(
                                                                    &ctx_for_task,
                                                                    runtime.guild_id,
                                                                    &guild_key,
                                                                    &active_meeting_id,
                                                                    "driver-disconnect initial target",
                                                                    *removed_session,
                                                                )
                                                                .await;
                                                            return;
                                                        }
                                                        continue;
                                                    }
                                                }
                                            }
                                            Ok(None) => {
                                                warn!(
                                                    guild_id = %guild_key,
                                                    disconnected_channel_id,
                                                    "driver-disconnect initial target retry limit reached with no active meeting"
                                                );
                                            }
                                            Err(err) => {
                                                warn!(
                                                    guild_id = %guild_key,
                                                    disconnected_channel_id,
                                                    error = %err,
                                                    "failed to resolve active meeting after driver-disconnect initial target retry limit"
                                                );
                                            }
                                        }
                                    }
                                }
                                return;
                            }
                            let Some(expected_meeting_id_ref) = expected_meeting_id.as_deref()
                            else {
                                return;
                            };
                            let current_meeting_id = match runtime.active_meeting_id_result().await
                            {
                                Ok(current_meeting_id) => current_meeting_id,
                                Err(err) => {
                                    let lookup_error = err.to_string();
                                    let terminal_error = recording_lookup_terminal_error(
                                        &mut lookup_failures,
                                        "driver-disconnect grace",
                                        &lookup_error,
                                    );
                                    warn!(
                                        guild_id = %guild_key,
                                        meeting_id = expected_meeting_id_ref,
                                        error = %err,
                                        attempts = lookup_failures,
                                        "failed to verify active meeting during driver-disconnect grace; rescheduling"
                                    );
                                    if let Some(terminal_error) = terminal_error {
                                        warn!(
                                            guild_id = %guild_key,
                                            meeting_id = expected_meeting_id_ref,
                                            attempts = lookup_failures,
                                            "driver-disconnect active-meeting lookup retry limit reached; marking recording failed"
                                        );
                                        match runtime
                                            .fail_recording_after_lookup_exhaustion(
                                                &lifecycle_permit,
                                                &ctx_for_task,
                                                http.as_ref(),
                                                &RecordingLookupFailureRequest {
                                                    guild_id: runtime.guild_id,
                                                    guild_key: &guild_key,
                                                    expected_meeting_id: expected_meeting_id_ref,
                                                    terminal_error: &terminal_error,
                                                    context:
                                                        "driver-disconnect active-meeting lookup",
                                                },
                                            )
                                            .await
                                        {
                                            Ok(()) => return,
                                            Err(mark_err) => {
                                                warn!(
                                                    guild_id = %guild_key,
                                                    meeting_id = expected_meeting_id_ref,
                                                    error = %mark_err,
                                                    "failed to mark recording failed after driver-disconnect lookup exhaustion; rescheduling"
                                                );
                                                if let TerminalCleanupRetryDecision::Cleared {
                                                    removed_session,
                                                } = runtime
                                                    .handle_terminal_cleanup_retry_failure(
                                                        TerminalCleanupRetryFailureRequest {
                                                            guild_key: &guild_key,
                                                            expected_meeting_id:
                                                                expected_meeting_id_ref,
                                                            phase: "driver-disconnect active-meeting lookup",
                                                            err: &mark_err,
                                                        },
                                                        &mut terminal_cleanup_failures,
                                                    )
                                                    .await
                                                {
                                                    drop(lifecycle_permit);
                                                    runtime
                                                        .finish_terminal_absence_cleanup(
                                                            &ctx_for_task,
                                                            runtime.guild_id,
                                                            &guild_key,
                                                            expected_meeting_id_ref,
                                                            "driver-disconnect active-meeting lookup",
                                                            *removed_session,
                                                    )
                                                    .await;
                                                    return;
                                                }
                                            }
                                        }
                                    }
                                    continue;
                                }
                            };
                            if current_meeting_id.as_deref() != Some(expected_meeting_id_ref) {
                                let mut startups = runtime.recording_startups.lock().await;
                                clear_matching_recording_startup(
                                    &mut startups,
                                    &guild_key,
                                    expected_meeting_id_ref,
                                );
                                return;
                            }
                            let target_voice_channel_id =
                                match runtime.active_meeting_voice_channel_result().await {
                                    Ok(Some(active_voice_channel))
                                        if active_voice_channel.meeting_id
                                            == expected_meeting_id_ref =>
                                    {
                                        active_voice_channel.voice_channel_id
                                    }
                                    Ok(Some(active_voice_channel)) => {
                                        warn!(
                                            guild_id = %guild_key,
                                            meeting_id = expected_meeting_id_ref,
                                            actual_meeting_id = %active_voice_channel.meeting_id,
                                            "driver-disconnect voice-channel lookup found a different active meeting"
                                        );
                                        let local = runtime.lifecycle_local_state();
                                        clear_failed_recording_start_local_state_with_dependencies(
                                            &local,
                                            &guild_key,
                                            expected_meeting_id_ref,
                                            FailedRecordingStartLocalCleanup::FullRuntimeState,
                                            |session: &RecordingSession<LocalChunkStorage>, tracker| {
                                                session.persist_ssrc_mapping(tracker);
                                            },
                                        )
                                        .await;
                                        return;
                                    }
                                    Ok(None) => {
                                        let terminal_error = "active recording voice channel disappeared during driver-disconnect grace";
                                        warn!(
                                            guild_id = %guild_key,
                                            meeting_id = expected_meeting_id_ref,
                                            "driver-disconnect voice-channel lookup returned no active meeting after active id verification; marking recording failed"
                                        );
                                        match runtime
                                            .fail_recording_after_lookup_exhaustion(
                                                &lifecycle_permit,
                                                &ctx_for_task,
                                                http.as_ref(),
                                                &RecordingLookupFailureRequest {
                                                    guild_id: runtime.guild_id,
                                                    guild_key: &guild_key,
                                                    expected_meeting_id:
                                                        expected_meeting_id_ref,
                                                    terminal_error,
                                                    context:
                                                        "driver-disconnect voice-channel absence",
                                                },
                                            )
                                            .await
                                        {
                                            Ok(()) => return,
                                            Err(mark_err) => {
                                                warn!(
                                                    guild_id = %guild_key,
                                                    meeting_id = expected_meeting_id_ref,
                                                    error = %mark_err,
                                                    "failed to mark recording failed after driver-disconnect voice-channel absence; rescheduling"
                                                );
                                                if let TerminalCleanupRetryDecision::Cleared {
                                                    removed_session,
                                                } = runtime
                                                    .handle_terminal_cleanup_retry_failure(
                                                        TerminalCleanupRetryFailureRequest {
                                                            guild_key: &guild_key,
                                                            expected_meeting_id:
                                                                expected_meeting_id_ref,
                                                            phase: "driver-disconnect voice-channel absence",
                                                            err: &mark_err,
                                                        },
                                                        &mut terminal_cleanup_failures,
                                                    )
                                                    .await
                                                {
                                                    drop(lifecycle_permit);
                                                    runtime
                                                        .finish_terminal_absence_cleanup(
                                                            &ctx_for_task,
                                                            runtime.guild_id,
                                                            &guild_key,
                                                            expected_meeting_id_ref,
                                                            "driver-disconnect voice-channel absence",
                                                            *removed_session,
                                                        )
                                                        .await;
                                                    return;
                                                }
                                                continue;
                                            }
                                        }
                                    }
                                    Err(err) => {
                                        let lookup_error = err.to_string();
                                        let terminal_error = recording_lookup_terminal_error(
                                            &mut lookup_failures,
                                            "driver-disconnect voice-channel lookup",
                                            &lookup_error,
                                        );
                                        warn!(
                                            guild_id = %guild_key,
                                            meeting_id = expected_meeting_id_ref,
                                            error = %err,
                                            attempts = lookup_failures,
                                            "failed to resolve active voice channel during driver-disconnect grace; rescheduling"
                                        );
                                        if let Some(terminal_error) = terminal_error {
                                            warn!(
                                                guild_id = %guild_key,
                                                meeting_id = expected_meeting_id_ref,
                                                attempts = lookup_failures,
                                                "driver-disconnect voice-channel lookup retry limit reached; marking recording failed"
                                            );
                                            match runtime
                                                .fail_recording_after_lookup_exhaustion(
                                                    &lifecycle_permit,
                                                    &ctx_for_task,
                                                    http.as_ref(),
                                                    &RecordingLookupFailureRequest {
                                                        guild_id: runtime.guild_id,
                                                        guild_key: &guild_key,
                                                        expected_meeting_id:
                                                            expected_meeting_id_ref,
                                                        terminal_error: &terminal_error,
                                                        context:
                                                            "driver-disconnect voice-channel lookup",
                                                    },
                                                )
                                                .await
                                            {
                                                Ok(()) => return,
                                                Err(mark_err) => {
                                                    warn!(
                                                        guild_id = %guild_key,
                                                        meeting_id = expected_meeting_id_ref,
                                                        error = %mark_err,
                                                        "failed to mark recording failed after driver-disconnect voice-channel lookup exhaustion; rescheduling"
                                                    );
                                                    if let TerminalCleanupRetryDecision::Cleared {
                                                        removed_session,
                                                    } = runtime
                                                        .handle_terminal_cleanup_retry_failure(
                                                            TerminalCleanupRetryFailureRequest {
                                                                guild_key: &guild_key,
                                                                expected_meeting_id:
                                                                    expected_meeting_id_ref,
                                                                phase: "driver-disconnect voice-channel lookup",
                                                                err: &mark_err,
                                                            },
                                                            &mut terminal_cleanup_failures,
                                                        )
                                                        .await
                                                    {
                                                        drop(lifecycle_permit);
                                                        runtime
                                                            .finish_terminal_absence_cleanup(
                                                                &ctx_for_task,
                                                                runtime.guild_id,
                                                                &guild_key,
                                                                expected_meeting_id_ref,
                                                                "driver-disconnect voice-channel lookup",
                                                                *removed_session,
                                                        )
                                                        .await;
                                                        return;
                                                    }
                                                }
                                            }
                                        }
                                        continue;
                                    }
                                };
                            reset_recording_lookup_failures(&mut lookup_failures);
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
                            match decide_driver_disconnect_grace_expiry(reconnected, non_bot) {
                                GraceExpiryDecision::Reschedule => {
                                    let terminal_error = driver_disconnect_cache_miss_terminal_error(
                                        &mut grace_cache_misses,
                                    );
                                    warn!(
                                        guild_id = %runtime.guild_id,
                                        target_voice_channel_id,
                                        cache_misses = grace_cache_misses,
                                        "voice state cache unavailable on driver-disconnect grace expiry; rescheduling stop check"
                                    );
                                    if let Some(terminal_error) = terminal_error {
                                        warn!(
                                            guild_id = %guild_key,
                                            meeting_id = expected_meeting_id_ref,
                                            cache_misses = grace_cache_misses,
                                            "driver-disconnect cache-miss retry limit reached; marking recording failed"
                                        );
                                        match runtime
                                            .fail_recording_after_teardown_exhaustion(
                                                &lifecycle_permit,
                                                &ctx_for_task,
                                                runtime.guild_id,
                                                &guild_key,
                                                expected_meeting_id_ref,
                                                &terminal_error,
                                            )
                                            .await
                                        {
                                            Ok(()) => {
                                                if let Err(status_err) = runtime
                                                    .update_status_message(
                                                        &http,
                                                        expected_meeting_id_ref,
                                                        StatusMessageUpdate::Failed {
                                                            phase: "Voice state cache",
                                                            error: &terminal_error,
                                                        },
                                                    )
                                                    .await
                                                {
                                                    warn!(
                                                        guild_id = %guild_key,
                                                        meeting_id = expected_meeting_id_ref,
                                                        error = %status_err,
                                                        "failed to notify driver-disconnect cache-miss exhaustion"
                                                    );
                                                }
                                                return;
                                            }
                                            Err(mark_err) => {
                                                warn!(
                                                    guild_id = %guild_key,
                                                    meeting_id = expected_meeting_id_ref,
                                                    error = %mark_err,
                                                    "failed to mark recording failed after driver-disconnect cache-miss exhaustion; rescheduling"
                                                );
                                                if let TerminalCleanupRetryDecision::Cleared {
                                                    removed_session,
                                                } = runtime
                                                    .handle_terminal_cleanup_retry_failure(
                                                        TerminalCleanupRetryFailureRequest {
                                                            guild_key: &guild_key,
                                                            expected_meeting_id:
                                                                expected_meeting_id_ref,
                                                            phase: "driver-disconnect cache-miss exhaustion",
                                                            err: &mark_err,
                                                        },
                                                        &mut terminal_cleanup_failures,
                                                    )
                                                    .await
                                                {
                                                    drop(lifecycle_permit);
                                                    runtime
                                                        .finish_terminal_absence_cleanup(
                                                            &ctx_for_task,
                                                            runtime.guild_id,
                                                            &guild_key,
                                                            expected_meeting_id_ref,
                                                            "driver-disconnect cache-miss exhaustion",
                                                            *removed_session,
                                                        )
                                                        .await;
                                                    return;
                                                }
                                            }
                                        }
                                    }
                                    continue;
                                }
                                GraceExpiryDecision::Cancel => return,
                                GraceExpiryDecision::Stop => {
                                    // Reset only the cache-miss counter; flush/stop
                                    // failure counters keep enforcing their own limits
                                    // across rescheduled Stop-path iterations.
                                    grace_cache_misses = 0;
                                }
                            }
                            let teardown_request = RecordingStopTeardownRequest {
                                guild_key: &guild_key,
                                caller_user_id: "driver-disconnect",
                                caller_role: UserRole::BotAdmin,
                                expected_meeting_id: expected_meeting_id_ref,
                                reason: StopReason::ClientDisconnect,
                                phase: "driver disconnect",
                            };
                            match runtime
                                .prepare_recording_stop_after_teardown(
                                    &lifecycle_permit,
                                    &teardown_request,
                                )
                                .await
                            {
                                Ok((result, removed_session)) => {
                                    let reset_guard =
                                        Arc::clone(&runtime.ssrc_tracker_reset_gate)
                                            .lock_owned()
                                            .await;
                                    drop(lifecycle_permit);
                                    runtime
                                        .leave_after_recording_stop(
                                            &ctx_for_task,
                                            &guild_key,
                                            expected_meeting_id_ref,
                                            "driver disconnect",
                                            removed_session,
                                            reset_guard,
                                        )
                                        .await;
                                    break result;
                                }
                                Err(RecordingTeardownError::FinalFlush(err)) => {
                                    final_flush_failures += 1;
                                    if final_flush_failures >= FINAL_FLUSH_MAX_RETRIES {
                                        warn!(
                                            guild_id = %guild_key,
                                            attempts = final_flush_failures,
                                            error = %err,
                                            "driver-disconnect final flush retry limit reached; marking recording failed"
                                        );
                                        let terminal_error = format!(
                                            "final audio flush failed after {final_flush_failures} driver-disconnect attempt(s): {err}"
                                        );
                                        match runtime
                                            .fail_recording_after_teardown_exhaustion(
                                                &lifecycle_permit,
                                                &ctx_for_task,
                                                runtime.guild_id,
                                                &guild_key,
                                                expected_meeting_id_ref,
                                                &terminal_error,
                                            )
                                            .await
                                        {
                                            Ok(()) => {
                                                if let Err(status_err) = runtime
                                                    .update_status_message(
                                                        &http,
                                                        expected_meeting_id_ref,
                                                        StatusMessageUpdate::Failed {
                                                            phase: "Recording persist",
                                                            error: &terminal_error,
                                                        },
                                                    )
                                                    .await
                                                {
                                                    warn!(
                                                        guild_id = %guild_key,
                                                        meeting_id = expected_meeting_id_ref,
                                                        error = %status_err,
                                                        "failed to notify driver-disconnect final flush exhaustion"
                                                    );
                                                }
                                                return;
                                            }
                                            Err(mark_err) => {
                                                warn!(
                                                    guild_id = %guild_key,
                                                    meeting_id = expected_meeting_id_ref,
                                                    error = %mark_err,
                                                    "failed to mark recording failed after driver-disconnect final flush exhaustion; rescheduling"
                                                );
                                                if let TerminalCleanupRetryDecision::Cleared {
                                                    removed_session,
                                                } = runtime
                                                    .handle_terminal_cleanup_retry_failure(
                                                        TerminalCleanupRetryFailureRequest {
                                                            guild_key: &guild_key,
                                                            expected_meeting_id:
                                                                expected_meeting_id_ref,
                                                            phase: "driver-disconnect final flush exhaustion",
                                                            err: &mark_err,
                                                        },
                                                        &mut terminal_cleanup_failures,
                                                    )
                                                    .await
                                                {
                                                    drop(lifecycle_permit);
                                                    runtime
                                                        .finish_terminal_absence_cleanup(
                                                            &ctx_for_task,
                                                            runtime.guild_id,
                                                            &guild_key,
                                                            expected_meeting_id_ref,
                                                            "driver-disconnect final flush exhaustion",
                                                            *removed_session,
                                                        )
                                                        .await;
                                                    return;
                                                }
                                            }
                                        }
                                    }
                                    continue;
                                }
                                Err(RecordingTeardownError::Stop(err)) => {
                                    final_flush_failures = 0;
                                    if err.is_target_absent() {
                                        warn!(
                                            guild_id = %guild_key,
                                            meeting_id = expected_meeting_id_ref,
                                            error = %err,
                                            "driver-disconnect terminal cleanup retry found no active meeting; treating as already handled"
                                        );
                                        let removed_session = runtime
                                            .remove_local_recording_state_after_terminal_absence(
                                                &guild_key,
                                                expected_meeting_id_ref,
                                            )
                                            .await;
                                        drop(lifecycle_permit);
                                        runtime
                                            .finish_terminal_absence_cleanup(
                                                &ctx_for_task,
                                                runtime.guild_id,
                                                &guild_key,
                                                expected_meeting_id_ref,
                                                "driver-disconnect terminal cleanup retry",
                                                removed_session,
                                            )
                                            .await;
                                        return;
                                    }
                                    let terminal_error = recording_stop_terminal_error(
                                        &mut stop_failures,
                                        "driver-disconnect",
                                        &err.to_string(),
                                    );
                                    warn!(
                                        guild_id = %guild_key,
                                        meeting_id = expected_meeting_id_ref,
                                        error = %err,
                                        attempts = stop_failures,
                                        "failed to stop recording on driver disconnect; rescheduling"
                                    );
                                    if let Some(terminal_error) = terminal_error {
                                        warn!(
                                            guild_id = %guild_key,
                                            meeting_id = expected_meeting_id_ref,
                                            attempts = stop_failures,
                                            "driver-disconnect stop retry limit reached; marking recording failed"
                                        );
                                        match runtime
                                            .fail_recording_after_teardown_exhaustion(
                                                &lifecycle_permit,
                                                &ctx_for_task,
                                                runtime.guild_id,
                                                &guild_key,
                                                expected_meeting_id_ref,
                                                &terminal_error,
                                            )
                                            .await
                                        {
                                            Ok(()) => {
                                                if let Err(status_err) = runtime
                                                    .update_status_message(
                                                        &http,
                                                        expected_meeting_id_ref,
                                                        StatusMessageUpdate::Failed {
                                                            phase: "Recording stop",
                                                            error: &terminal_error,
                                                        },
                                                    )
                                                    .await
                                                {
                                                    warn!(
                                                        guild_id = %guild_key,
                                                        meeting_id = expected_meeting_id_ref,
                                                        error = %status_err,
                                                        "failed to notify driver-disconnect stop retry exhaustion"
                                                    );
                                                }
                                                return;
                                            }
                                            Err(mark_err) => {
                                                warn!(
                                                    guild_id = %guild_key,
                                                    meeting_id = expected_meeting_id_ref,
                                                    error = %mark_err,
                                                    "failed to mark recording failed after driver-disconnect stop exhaustion; rescheduling"
                                                );
                                                if let TerminalCleanupRetryDecision::Cleared {
                                                    removed_session,
                                                } = runtime
                                                    .handle_terminal_cleanup_retry_failure(
                                                        TerminalCleanupRetryFailureRequest {
                                                            guild_key: &guild_key,
                                                            expected_meeting_id:
                                                                expected_meeting_id_ref,
                                                            phase: "driver-disconnect stop exhaustion",
                                                            err: &mark_err,
                                                        },
                                                        &mut terminal_cleanup_failures,
                                                    )
                                                    .await
                                                {
                                                    drop(lifecycle_permit);
                                                    runtime
                                                        .finish_terminal_absence_cleanup(
                                                            &ctx_for_task,
                                                            runtime.guild_id,
                                                            &guild_key,
                                                            expected_meeting_id_ref,
                                                            "driver-disconnect stop exhaustion",
                                                            *removed_session,
                                                        )
                                                        .await;
                                                    return;
                                                }
                                            }
                                        }
                                    }
                                    continue;
                                }
                            }
                        };
                        if stop_result.outcome == StopOutcome::Owner
                            && let Err(err) = runtime
                                .update_status_message(
                                    &http,
                                    &stop_result.meeting_id,
                                    StatusMessageUpdate::RecordingStopped,
                                )
                                .await
                        {
                            warn!(
                                guild_id = %guild_key,
                                meeting_id = %stop_result.meeting_id,
                                error = %err,
                                "failed to update status message after driver disconnect stop"
                            );
                        }
                        if stop_result.outcome == StopOutcome::Owner {
                            tokio::select! {
                                summary_result = run_summary_background(
                                    &runtime,
                                    &http,
                                    &stop_result.meeting_id,
                                ) => {
                                    if let Err(err) = summary_result {
                                        warn!(
                                            guild_id = %guild_key,
                                            meeting_id = %stop_result.meeting_id,
                                            error = %err,
                                            "failed to process summary after driver disconnect"
                                        );
                                    }
                                }
                                _ = runtime.shutdown_token.cancelled() => {
                                    debug!(
                                        guild_id = %guild_key,
                                        meeting_id = %stop_result.meeting_id,
                                        "driver-disconnect summary task deferred by shutdown"
                                    );
                                }
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
) -> Result<Vec<PersistedChunk>, String> {
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
            result.persisted
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
    use crate::infrastructure::sql_store::{FakeSqlExecutor, SqlExecutor};
    use crate::infrastructure::storage_fs::{ChunkStorageError, SavedChunk};
    use serenity::async_trait;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Debug, Clone)]
    struct RuntimeFlakyChunkStorage {
        failures_remaining: Arc<AtomicUsize>,
        saved_chunks: Arc<AtomicUsize>,
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
            self.saved_chunks.fetch_add(1, Ordering::SeqCst);
            Ok(SavedChunk {
                path: std::env::temp_dir().join(format!("{user_id}_{sequence}_{start_ms}.wav")),
                size_bytes: bytes.len(),
            })
        }
    }

    fn session_with_one_flaky_chunk(failures: usize) -> RecordingSession<RuntimeFlakyChunkStorage> {
        session_with_one_flaky_chunk_and_counter(failures, Arc::new(AtomicUsize::new(0)))
    }

    fn session_with_one_flaky_chunk_and_counter(
        failures: usize,
        saved_chunks: Arc<AtomicUsize>,
    ) -> RecordingSession<RuntimeFlakyChunkStorage> {
        let mut session = RecordingSession::new(
            "meeting-1".to_owned(),
            RuntimeFlakyChunkStorage {
                failures_remaining: Arc::new(AtomicUsize::new(failures)),
                saved_chunks,
            },
            ReceiverConfig {
                chunk_duration: Duration::from_secs(20),
                silence_flush_duration: Duration::from_secs(30),
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
    fn shutdown_flushes_pending_recording_chunks() {
        let saved_chunks = Arc::new(AtomicUsize::new(0));
        let session = session_with_one_flaky_chunk_and_counter(0, Arc::clone(&saved_chunks));
        let mut sessions = HashMap::from([("g1".to_owned(), session)]);

        assert_eq!(flush_sessions_for_shutdown(&mut sessions), 1);
        assert_eq!(saved_chunks.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn failed_start_cleanup_flushes_removed_session_chunks() {
        let saved_chunks = Arc::new(AtomicUsize::new(0));
        let mut session = session_with_one_flaky_chunk_and_counter(0, Arc::clone(&saved_chunks));

        flush_removed_session_after_stop(&mut session, "g1", "record-start failure cleanup")
            .expect("tail flush should succeed");

        assert_eq!(saved_chunks.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn failed_start_cleanup_reports_removed_session_flush_failure() {
        let mut session = session_with_one_flaky_chunk(1);

        let err =
            flush_removed_session_after_stop(&mut session, "g1", "record-start failure cleanup")
                .expect_err("tail flush failures should be surfaced to callers");

        assert!(err.contains("failed to persist 1 tail audio chunk"));
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

    #[derive(Debug)]
    struct RecoverySummaryJobExecutor {
        inner: FakeSqlExecutor,
        job_status: String,
        stale_running: bool,
    }

    impl RecoverySummaryJobExecutor {
        fn new(job_status: &str, stale_running: bool) -> Self {
            let mut inner = FakeSqlExecutor::default();
            let enqueue_key = format!(
                "{}|{}",
                crate::infrastructure::sql::ENQUEUE_JOB_SQL,
                ["summary-m1", "m1", "summarize"].join("\u{1f}")
            );
            inner.execute_error.insert(
                enqueue_key,
                format!(
                    "{}duplicate job",
                    crate::infrastructure::sql_store::UNIQUE_VIOLATION_PREFIX
                ),
            );
            Self {
                inner,
                job_status: job_status.to_owned(),
                stale_running,
            }
        }
    }

    impl SqlExecutor for RecoverySummaryJobExecutor {
        fn execute(&mut self, sql: &str, params: &[String]) -> Result<u64, String> {
            self.inner.executed.push((sql.to_owned(), params.to_vec()));
            if sql == RECOVERY_REQUEUE_STALE_RUNNING_SUMMARY_JOB_SQL {
                if self.job_status == "running" && self.stale_running {
                    self.job_status = "queued".to_owned();
                    return Ok(1);
                }
                return Ok(0);
            }
            let key = format!("{}|{}", sql, params.join("\u{1f}"));
            if let Some(err) = self.inner.execute_error.get(&key) {
                return Err(err.clone());
            }
            Ok(*self.inner.execute_result.get(&key).unwrap_or(&1))
        }

        fn query_active_meeting(
            &mut self,
            guild_id: &str,
        ) -> Result<Option<crate::infrastructure::storage::StoredMeeting>, String> {
            self.inner.query_active_meeting(guild_id)
        }

        fn query_rows(
            &mut self,
            sql: &str,
            params: &[String],
        ) -> Result<Vec<crate::infrastructure::sql_store::SqlRow>, String> {
            self.inner.executed.push((sql.to_owned(), params.to_vec()));
            let key = format!("{}|{}", sql, params.join("\u{1f}"));
            if let Some(err) = self.inner.query_rows_error.get(&key) {
                return Err(err.clone());
            }
            if sql == RECOVERY_SUMMARY_JOB_STATUS_SQL {
                return Ok(vec![vec![Some(self.job_status.clone())]]);
            }
            Ok(self
                .inner
                .query_rows_result
                .get(&key)
                .cloned()
                .unwrap_or_default())
        }

        fn run_migration(&mut self, migration_sql: &str) -> Result<(), String> {
            self.inner.run_migration(migration_sql)
        }
    }

    type RecoverySummaryJobQueue = SqlJobQueue<RecoverySummaryJobExecutor>;

    fn fake_recovery_queue_with_existing_summary_job_status(
        inspected_status: &str,
    ) -> RecoverySummaryJobQueue {
        fake_recovery_queue_with_existing_summary_job(inspected_status, false)
    }

    fn fake_recovery_queue_with_existing_summary_job(
        initial_status: &str,
        stale_running: bool,
    ) -> RecoverySummaryJobQueue {
        SqlJobQueue::new(RecoverySummaryJobExecutor::new(
            initial_status,
            stale_running,
        ))
    }

    #[test]
    fn recovery_existing_queued_summary_job_is_claimable() {
        let mut queue = fake_recovery_queue_with_existing_summary_job_status("queued");

        assert!(recover_summary_job_for_startup(
            &mut queue,
            "summary-m1",
            "m1",
            true
        ));
    }

    #[test]
    fn recovery_existing_running_or_failed_summary_job_is_not_claimable() {
        let mut running_queue = fake_recovery_queue_with_existing_summary_job_status("running");
        assert!(!recover_summary_job_for_startup(
            &mut running_queue,
            "summary-m1",
            "m1",
            true
        ));

        let mut failed_queue = fake_recovery_queue_with_existing_summary_job_status("failed");
        assert!(!recover_summary_job_for_startup(
            &mut failed_queue,
            "summary-m1",
            "m1",
            true
        ));
    }

    #[test]
    fn recovery_existing_summary_job_status_error_is_not_claimable() {
        let mut queue = fake_recovery_queue_with_existing_summary_job_status("queued");
        let status_key = format!("{}|{}", RECOVERY_SUMMARY_JOB_STATUS_SQL, "summary-m1");
        queue
            .executor
            .inner
            .query_rows_error
            .insert(status_key, "status lookup failed".to_owned());

        assert!(!recover_summary_job_for_startup(
            &mut queue,
            "summary-m1",
            "m1",
            true
        ));
    }

    #[test]
    fn recovery_stale_running_summary_job_becomes_claimable() {
        let mut queue = fake_recovery_queue_with_existing_summary_job("running", true);

        assert!(recover_summary_job_for_startup(
            &mut queue,
            "summary-m1",
            "m1",
            true
        ));
        assert_eq!(queue.executor.job_status, "queued");
        assert!(queue.executor.inner.executed.iter().any(|(sql, params)| {
            sql == RECOVERY_REQUEUE_STALE_RUNNING_SUMMARY_JOB_SQL
                && params == &vec!["summary-m1".to_owned()]
        }));
    }

    #[test]
    fn recovery_failed_summary_job_is_not_requeued_as_stale_running() {
        let mut queue = fake_recovery_queue_with_existing_summary_job("failed", false);

        assert!(!recover_summary_job_for_startup(
            &mut queue,
            "summary-m1",
            "m1",
            true
        ));
        assert_eq!(queue.executor.job_status, "failed");
    }

    #[test]
    fn recovery_fresh_running_summary_job_is_not_requeued_as_stale_running() {
        let mut queue = fake_recovery_queue_with_existing_summary_job("running", false);

        assert!(!recover_summary_job_for_startup(
            &mut queue,
            "summary-m1",
            "m1",
            true
        ));
        assert_eq!(queue.executor.job_status, "running");
    }

    #[test]
    fn recovery_stale_running_reset_does_not_target_failed_jobs() {
        assert!(RECOVERY_REQUEUE_STALE_RUNNING_SUMMARY_JOB_SQL.contains("status='running'"));
        assert!(!RECOVERY_REQUEUE_STALE_RUNNING_SUMMARY_JOB_SQL.contains("status IN"));
        assert!(!RECOVERY_REQUEUE_STALE_RUNNING_SUMMARY_JOB_SQL.contains("'failed'"));
        assert!(RECOVERY_REQUEUE_STALE_RUNNING_SUMMARY_JOB_SQL.contains("leased_until"));
    }

    #[test]
    fn recovery_requeues_running_job_when_lease_expired_before_updated_at_stale() {
        let mut queue = fake_recovery_queue_with_existing_summary_job("running", true);
        assert!(recover_summary_job_for_startup(
            &mut queue,
            "summary-m1",
            "m1",
            true
        ));
        assert_eq!(queue.executor.job_status, "queued");
    }

    #[test]
    fn recovery_does_not_recover_summary_job_when_snapshot_disables_summary() {
        let mut queue = fake_recovery_queue_with_existing_summary_job_status("queued");

        assert!(!recover_summary_job_for_startup(
            &mut queue,
            "summary-m1",
            "m1",
            false
        ));
        assert!(queue.executor.inner.executed.is_empty());
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

    fn recording_meeting() -> crate::infrastructure::storage::StoredMeeting {
        crate::infrastructure::storage::StoredMeeting {
            id: "m1".to_owned(),
            guild_id: "g1".to_owned(),
            voice_channel_id: "vc1".to_owned(),
            report_channel_id: "tc1".to_owned(),
            status_message_channel_id: None,
            status_message_id: None,
            started_by_user_id: "u1".to_owned(),
            title: None,
            status: crate::domain::MeetingStatus::Recording,
            stop_reason: None,
            error_message: None,
            started_at: None,
            stopped_at: None,
        }
    }

    struct RecordingLocalState<S: ChunkStorage> {
        sessions: HashMap<String, RecordingSession<S>>,
        auto_stop_states: HashMap<String, AutoStopState>,
        live_transcription_titles: HashMap<String, Option<String>>,
        recording_startups: HashMap<String, String>,
    }

    impl RecordingLocalState<RuntimeFlakyChunkStorage> {
        fn with_matching_session(failures: usize) -> Self {
            Self::with_matching_session_and_saved_counter(failures, Arc::new(AtomicUsize::new(0)))
        }

        fn with_matching_session_and_saved_counter(
            failures: usize,
            saved_chunks: Arc<AtomicUsize>,
        ) -> Self {
            let mut auto_stop_state =
                AutoStopState::new_for_meeting(Duration::from_secs(0), Some("m1".to_owned()));
            assert_eq!(
                auto_stop_state.on_non_bot_member_count_changed(0),
                AutoStopSignal::StartTimer
            );
            auto_stop_state.set_empty_since_elapsed_for_test(Duration::from_secs(1));
            let mut session = session_with_one_flaky_chunk_and_counter(failures, saved_chunks);
            session.meeting_id = "m1".to_owned();

            Self {
                sessions: HashMap::from([("g1".to_owned(), session)]),
                auto_stop_states: HashMap::from([("g1".to_owned(), auto_stop_state)]),
                live_transcription_titles: HashMap::from([(
                    "m1".to_owned(),
                    Some("title".to_owned()),
                )]),
                recording_startups: HashMap::from([("g1".to_owned(), "m1".to_owned())]),
            }
        }

        fn with_startup_only() -> Self {
            Self {
                sessions: HashMap::new(),
                auto_stop_states: HashMap::new(),
                live_transcription_titles: HashMap::new(),
                recording_startups: HashMap::from([("g1".to_owned(), "m1".to_owned())]),
            }
        }

        fn with_newer_session_on_same_guild() -> Self {
            let mut session = session_with_one_flaky_chunk(0);
            session.meeting_id = "m2".to_owned();
            Self {
                sessions: HashMap::from([("g1".to_owned(), session)]),
                auto_stop_states: HashMap::from([(
                    "g1".to_owned(),
                    AutoStopState::new_for_meeting(Duration::from_secs(0), Some("m2".to_owned())),
                )]),
                live_transcription_titles: HashMap::from([(
                    "m2".to_owned(),
                    Some("newer".to_owned()),
                )]),
                recording_startups: HashMap::from([("g1".to_owned(), "m2".to_owned())]),
            }
        }
    }

    impl<S: ChunkStorage> RecordingLocalState<S> {
        fn clear_expected_meeting(&mut self) -> Option<RecordingSession<S>> {
            let removed =
                remove_matching_recording_session_for_meeting(&mut self.sessions, "g1", "m1");
            clear_local_recording_state_maps_after_terminal_absence(
                &mut self.auto_stop_states,
                &mut self.live_transcription_titles,
                &mut self.recording_startups,
                "g1",
                "m1",
            );
            removed
        }

        fn assert_matching_state_present(&self) {
            assert!(
                self.sessions
                    .get("g1")
                    .is_some_and(|session| session.meeting_id == "m1"),
                "matching recording session should remain retryable"
            );
            assert!(
                self.auto_stop_states
                    .get("g1")
                    .is_some_and(|state| state.belongs_to_meeting("m1")),
                "auto-stop state should remain scoped to the retrying meeting"
            );
            assert!(
                self.live_transcription_titles.contains_key("m1"),
                "live title cache should remain until terminal cleanup succeeds"
            );
            assert_eq!(
                self.recording_startups.get("g1").map(String::as_str),
                Some("m1"),
                "startup reservation should remain while teardown can retry"
            );
        }

        fn assert_matching_state_cleared(&self) {
            assert!(!self.sessions.contains_key("g1"));
            assert!(!self.auto_stop_states.contains_key("g1"));
            assert!(!self.live_transcription_titles.contains_key("m1"));
            assert!(!self.recording_startups.contains_key("g1"));
        }
    }

    struct FaultInjectedMeetingStore {
        inner: crate::infrastructure::storage::InMemoryMeetingStore,
        set_status_failures_remaining: usize,
        set_error_message_failures_remaining: usize,
        find_active_failures_remaining: usize,
        set_status_attempts: usize,
    }

    impl FaultInjectedMeetingStore {
        fn with_recording_meeting() -> Self {
            let mut inner = crate::infrastructure::storage::InMemoryMeetingStore::new();
            inner.insert(recording_meeting());
            Self {
                inner,
                set_status_failures_remaining: 0,
                set_error_message_failures_remaining: 0,
                find_active_failures_remaining: 0,
                set_status_attempts: 0,
            }
        }

        fn meeting(&self) -> &crate::infrastructure::storage::StoredMeeting {
            self.inner.get("m1").expect("meeting should exist")
        }

        fn fail_status_updates(mut self, failures: usize) -> Self {
            self.set_status_failures_remaining = failures;
            self
        }

        fn fail_error_message_updates(mut self, failures: usize) -> Self {
            self.set_error_message_failures_remaining = failures;
            self
        }

        fn fail_active_meeting_lookups(mut self, failures: usize) -> Self {
            self.find_active_failures_remaining = failures;
            self
        }

        fn status_update_attempts(&self) -> usize {
            self.set_status_attempts
        }
    }

    impl crate::infrastructure::storage::UsageEventStore for FaultInjectedMeetingStore {
        fn append_usage_event(
            &mut self,
            event: &crate::domain::usage::NewUsageEvent,
        ) -> Result<(), crate::infrastructure::storage::StoreError> {
            self.inner.append_usage_event(event)
        }

        fn list_recent_usage_events(
            &mut self,
            tenant_id: Option<&str>,
            guild_id: Option<&str>,
            limit: u32,
        ) -> Result<Vec<crate::domain::usage::UsageEvent>, crate::infrastructure::storage::StoreError>
        {
            self.inner
                .list_recent_usage_events(tenant_id, guild_id, limit)
        }

        fn aggregate_recent_usage(
            &mut self,
            tenant_id: Option<&str>,
            guild_id: Option<&str>,
            window_seconds: u64,
        ) -> Result<
            Vec<crate::domain::usage::UsageAggregate>,
            crate::infrastructure::storage::StoreError,
        > {
            self.inner
                .aggregate_recent_usage(tenant_id, guild_id, window_seconds)
        }
    }

    impl crate::infrastructure::storage::MeetingStore for FaultInjectedMeetingStore {
        fn mark_stopping_if_recording(
            &mut self,
            meeting_id: &str,
            reason: StopReason,
        ) -> Result<
            crate::infrastructure::storage::StopTransition,
            crate::infrastructure::storage::StoreError,
        > {
            self.inner.mark_stopping_if_recording(meeting_id, reason)
        }

        fn find_active_meeting_by_guild(
            &mut self,
            guild_id: &str,
        ) -> Result<
            Option<crate::infrastructure::storage::StoredMeeting>,
            crate::infrastructure::storage::StoreError,
        > {
            if self.find_active_failures_remaining > 0 {
                self.find_active_failures_remaining -= 1;
                return Err(crate::infrastructure::storage::StoreError::Backend(
                    "active meeting lookup unavailable".to_owned(),
                ));
            }
            self.inner.find_active_meeting_by_guild(guild_id)
        }

        fn get_meeting(
            &mut self,
            meeting_id: &str,
        ) -> Result<
            Option<crate::infrastructure::storage::StoredMeeting>,
            crate::infrastructure::storage::StoreError,
        > {
            self.inner.get_meeting(meeting_id)
        }

        fn create_scheduled_meeting(
            &mut self,
            request: crate::infrastructure::storage::CreateMeetingRequest,
        ) -> Result<(), crate::infrastructure::storage::StoreError> {
            self.inner.create_scheduled_meeting(request)
        }

        fn create_meeting_as_recording(
            &mut self,
            request: crate::infrastructure::storage::CreateMeetingRequest,
        ) -> Result<(), crate::infrastructure::storage::StoreError> {
            self.inner.create_meeting_as_recording(request)
        }

        fn set_meeting_status(
            &mut self,
            meeting_id: &str,
            status: crate::domain::MeetingStatus,
            expected_current: Option<crate::domain::MeetingStatus>,
        ) -> Result<(), crate::infrastructure::storage::StoreError> {
            self.set_status_attempts += 1;
            if self.set_status_failures_remaining > 0 {
                self.set_status_failures_remaining -= 1;
                return Err(crate::infrastructure::storage::StoreError::Backend(
                    "status update unavailable".to_owned(),
                ));
            }
            self.inner
                .set_meeting_status(meeting_id, status, expected_current)
        }

        fn set_error_message(
            &mut self,
            meeting_id: &str,
            error_message: Option<String>,
        ) -> Result<(), crate::infrastructure::storage::StoreError> {
            if self.set_error_message_failures_remaining > 0 {
                self.set_error_message_failures_remaining -= 1;
                return Err(crate::infrastructure::storage::StoreError::Backend(
                    "error message update unavailable".to_owned(),
                ));
            }
            self.inner.set_error_message(meeting_id, error_message)
        }

        fn get_status_message_metadata(
            &mut self,
            meeting_id: &str,
        ) -> Result<
            crate::infrastructure::storage::StatusMessageMetadata,
            crate::infrastructure::storage::StoreError,
        > {
            self.inner.get_status_message_metadata(meeting_id)
        }

        fn set_status_message(
            &mut self,
            meeting_id: &str,
            channel_id: String,
            message_id: String,
        ) -> Result<(), crate::infrastructure::storage::StoreError> {
            self.inner
                .set_status_message(meeting_id, channel_id, message_id)
        }

        fn upsert_effective_meeting_settings(
            &mut self,
            meeting_id: &str,
            settings: crate::infrastructure::storage::EffectiveMeetingSettings,
        ) -> Result<(), crate::infrastructure::storage::StoreError> {
            self.inner
                .upsert_effective_meeting_settings(meeting_id, settings)
        }

        fn get_effective_meeting_settings(
            &mut self,
            meeting_id: &str,
        ) -> Result<
            Option<crate::infrastructure::storage::EffectiveMeetingSettings>,
            crate::infrastructure::storage::StoreError,
        > {
            self.inner.get_effective_meeting_settings(meeting_id)
        }
    }

    struct AsyncLifecycleHarness<S: crate::infrastructure::storage::MeetingStore, C: ChunkStorage> {
        service: Arc<tokio::sync::Mutex<BotCommandService<S>>>,
        sessions: Arc<tokio::sync::Mutex<HashMap<String, RecordingSession<C>>>>,
        auto_stop_states: Arc<tokio::sync::Mutex<HashMap<String, AutoStopState>>>,
        live_transcription_titles: Arc<tokio::sync::Mutex<HashMap<String, Option<String>>>>,
        recording_startups: Arc<tokio::sync::Mutex<HashMap<String, String>>>,
        voice_event_gate: Arc<RwLock<()>>,
        ssrc_tracker: Arc<tokio::sync::Mutex<SsrcTracker>>,
        ssrc_tracker_reset_gate: Arc<tokio::sync::Mutex<()>>,
    }

    fn async_lifecycle_harness<S: crate::infrastructure::storage::MeetingStore, C: ChunkStorage>(
        store: S,
        local_state: RecordingLocalState<C>,
    ) -> AsyncLifecycleHarness<S, C> {
        AsyncLifecycleHarness {
            service: Arc::new(tokio::sync::Mutex::new(BotCommandService::new(store))),
            sessions: Arc::new(tokio::sync::Mutex::new(local_state.sessions)),
            auto_stop_states: Arc::new(tokio::sync::Mutex::new(local_state.auto_stop_states)),
            live_transcription_titles: Arc::new(tokio::sync::Mutex::new(
                local_state.live_transcription_titles,
            )),
            recording_startups: Arc::new(tokio::sync::Mutex::new(local_state.recording_startups)),
            voice_event_gate: Arc::new(RwLock::new(())),
            ssrc_tracker: Arc::new(tokio::sync::Mutex::new(SsrcTracker::new())),
            ssrc_tracker_reset_gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    impl<S, C> AsyncLifecycleHarness<S, C>
    where
        S: crate::infrastructure::storage::MeetingStore,
        C: ChunkStorage,
    {
        fn local_state(&self) -> RecordingLifecycleLocalState<'_, C> {
            RecordingLifecycleLocalState {
                sessions: &self.sessions,
                auto_stop_states: &self.auto_stop_states,
                live_transcription_titles: &self.live_transcription_titles,
                recording_startups: &self.recording_startups,
                voice_event_gate: &self.voice_event_gate,
                ssrc_tracker: &self.ssrc_tracker,
                ssrc_tracker_reset_gate: &self.ssrc_tracker_reset_gate,
            }
        }

        async fn assert_matching_state_present(&self) {
            assert!(
                self.sessions
                    .lock()
                    .await
                    .get("g1")
                    .is_some_and(|session| session.meeting_id == "m1"),
                "matching recording session should remain retryable"
            );
            assert!(
                self.auto_stop_states
                    .lock()
                    .await
                    .get("g1")
                    .is_some_and(|state| state.belongs_to_meeting("m1")),
                "auto-stop state should remain scoped to the retrying meeting"
            );
            assert!(
                self.live_transcription_titles
                    .lock()
                    .await
                    .contains_key("m1"),
                "live title cache should remain until terminal cleanup succeeds"
            );
            assert_eq!(
                self.recording_startups
                    .lock()
                    .await
                    .get("g1")
                    .map(String::as_str),
                Some("m1"),
                "startup reservation should remain while cleanup can retry"
            );
        }

        async fn assert_matching_state_cleared(&self) {
            assert!(!self.sessions.lock().await.contains_key("g1"));
            assert!(!self.auto_stop_states.lock().await.contains_key("g1"));
            assert!(
                !self
                    .live_transcription_titles
                    .lock()
                    .await
                    .contains_key("m1")
            );
            assert!(!self.recording_startups.lock().await.contains_key("g1"));
        }
    }

    #[derive(Default)]
    struct StubVoiceGateway {
        failures_remaining: AtomicUsize,
        leaves: AtomicUsize,
    }

    impl StubVoiceGateway {
        fn failing_once() -> Self {
            Self {
                failures_remaining: AtomicUsize::new(1),
                leaves: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl RecordingVoiceGateway for StubVoiceGateway {
        async fn leave_recording_voice(&self, _guild_id: GuildId) -> Result<(), String> {
            self.leaves.fetch_add(1, Ordering::SeqCst);
            if self
                .failures_remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                    if value > 0 { Some(value - 1) } else { None }
                })
                .is_ok()
            {
                return Err("voice leave unavailable".to_owned());
            }
            Ok(())
        }
    }

    fn ignore_ssrc_mapping_persistence<C: ChunkStorage>(
        _session: &RecordingSession<C>,
        _tracker: &SsrcTracker,
    ) {
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

    fn persisted_chunk_for_live_test() -> PersistedChunk {
        PersistedChunk {
            meeting_id: "m1".to_owned(),
            user_id: "alice".to_owned(),
            sequence: 7,
            start_ms: 1_000_000,
            saved: SavedChunk {
                path: std::env::temp_dir().join("live-test.wav"),
                size_bytes: 44,
            },
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
    fn transcript_persist_orders_rows_by_canonical_timeline() {
        let mut executor = crate::infrastructure::sql_store::FakeSqlExecutor::default();
        let mut early_alice = transcript_segment(0, 5_000);
        early_alice.text = "first alice".to_owned();
        let mut late_alice = transcript_segment(2_200, 2_600);
        late_alice.text = "second alice".to_owned();
        let mut bob = transcript_segment(1_200, 1_800);
        bob.speaker_id = "bob".to_owned();
        bob.text = "bob cuts in".to_owned();

        persist_transcript_segments(&mut executor, "m1", &[late_alice, early_alice, bob])
            .expect("transcript should persist");

        let (_, params) = executor
            .executed
            .iter()
            .find(|(sql, _)| sql.contains("INSERT INTO transcripts"))
            .expect("transcript insert should execute");
        assert_eq!(params[3], "alice");
        assert_eq!(params[4], "0");
        assert_eq!(params[12], "bob");
        assert_eq!(params[13], "1200");
        assert_eq!(params[21], "alice");
        assert_eq!(params[22], "2200");
    }

    #[test]
    fn live_transcript_success_marks_chunk_done_and_uses_live_stage() {
        let mut executor = crate::infrastructure::sql_store::FakeSqlExecutor::default();
        let chunk = persisted_chunk_for_live_test();
        let segment = transcript_segment(500, 1_000);

        persist_live_transcription_success(&mut executor, &chunk, &[segment])
            .expect("live transcript should persist");

        assert!(
            executor.executed.iter().any(|(sql, params)| sql
                .contains("transcript_stage, live_chunk_id")
                && sql.contains("'live'")
                && params.contains(&"m1-alice-7-1000000".to_owned())),
            "live transcript insert should mark rows as live and link the source chunk"
        );
        assert!(
            executor.executed.iter().any(|(sql, params)| {
                sql.contains("UPDATE live_transcription_chunks SET status='done'")
                    && params == &vec!["m1-alice-7-1000000".to_owned()]
            }),
            "source chunk should be marked done after row persistence"
        );
    }

    #[test]
    fn failed_live_chunk_deletes_any_staged_rows() {
        let mut executor = crate::infrastructure::sql_store::FakeSqlExecutor::default();
        let chunk = persisted_chunk_for_live_test();

        mark_live_transcription_chunk_failed(&mut executor, &chunk, "boom")
            .expect("failed chunk should be marked");

        assert!(
            executor.executed.iter().any(|(sql, params)| {
                sql.contains("DELETE FROM transcripts")
                    && sql.contains("transcript_stage='live'")
                    && params == &vec!["m1".to_owned(), "m1-alice-7-1000000".to_owned()]
            }),
            "failed live chunks should remove staged live rows"
        );
        assert!(
            executor.executed.iter().any(|(sql, params)| {
                sql.contains("UPDATE live_transcription_chunks SET status='failed'")
                    && params == &vec!["m1-alice-7-1000000".to_owned(), "boom".to_owned()]
            }),
            "source chunk should be marked failed"
        );
    }

    #[test]
    fn final_transcript_rows_block_late_live_writes() {
        let mut executor = crate::infrastructure::sql_store::FakeSqlExecutor::default();
        let sql = "SELECT 1 FROM transcripts WHERE meeting_id=$1 AND transcript_stage='final' AND NOT is_deleted LIMIT 1";
        executor.query_rows_result.insert(
            format!("{}|{}", sql, "m1"),
            vec![vec![Some("1".to_owned())]],
        );

        assert!(
            final_transcript_rows_exist(&mut executor, "m1")
                .expect("final row check should succeed")
        );
    }

    #[test]
    fn live_transcript_loader_rebases_and_sorts_rows_to_final_timeline() {
        let mut executor = crate::infrastructure::sql_store::FakeSqlExecutor::default();
        let sql = "SELECT t.speaker_id, t.start_ms, t.end_ms, t.text, t.confidence, t.is_noisy, t.source, c.timeline_base_ms \
         FROM transcripts t \
         INNER JOIN live_transcription_chunks c ON c.id = t.live_chunk_id AND c.status='done' \
         WHERE t.meeting_id=$1 AND t.transcript_stage='live' AND NOT t.is_deleted \
         ORDER BY t.start_ms, t.end_ms, t.speaker_id, t.id";
        let key = format!("{}|{}", sql, "m1");
        executor.query_rows_result.insert(
            key,
            vec![
                vec![
                    Some("later".to_owned()),
                    Some("0".to_owned()),
                    Some("500".to_owned()),
                    Some("later".to_owned()),
                    Some("0.9".to_owned()),
                    Some("false".to_owned()),
                    Some("voice".to_owned()),
                    Some("2000".to_owned()),
                ],
                vec![
                    Some("earlier".to_owned()),
                    Some("0".to_owned()),
                    Some("500".to_owned()),
                    Some("earlier".to_owned()),
                    Some("0.9".to_owned()),
                    Some("false".to_owned()),
                    Some("voice".to_owned()),
                    Some("1000".to_owned()),
                ],
            ],
        );

        let segments = load_live_transcript_segments(&mut executor, "m1", 1_000)
            .expect("live transcript should load");

        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].speaker_id, "earlier");
        assert_eq!(segments[0].start_ms, 0);
        assert_eq!(segments[0].end_ms, 500);
        assert_eq!(segments[1].speaker_id, "later");
        assert_eq!(segments[1].start_ms, 1_000);
        assert_eq!(segments[1].end_ms, 1_500);
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

    #[test]
    fn auto_stop_cache_miss_reschedules_instead_of_stopping() {
        assert_eq!(
            decide_auto_stop_grace_expiry(None),
            GraceExpiryDecision::Reschedule
        );
        assert_eq!(
            decide_auto_stop_grace_expiry(Some(1)),
            GraceExpiryDecision::Cancel
        );
        assert_eq!(
            decide_auto_stop_grace_expiry(Some(0)),
            GraceExpiryDecision::Stop
        );
    }

    #[test]
    fn auto_stop_cache_miss_retry_limit_returns_terminal_error() {
        let mut cache_misses = 0;

        for expected in 1..AUTO_STOP_GRACE_MAX_CACHE_MISS_CHECKS {
            assert_eq!(auto_stop_cache_miss_terminal_error(&mut cache_misses), None);
            assert_eq!(cache_misses, expected);
        }

        let error = auto_stop_cache_miss_terminal_error(&mut cache_misses)
            .expect("retry limit should terminalize");
        assert_eq!(cache_misses, AUTO_STOP_GRACE_MAX_CACHE_MISS_CHECKS);
        assert!(error.contains("auto-stop grace stop check"));
    }

    #[test]
    fn driver_disconnect_cache_miss_reschedules_instead_of_stopping() {
        assert_eq!(
            decide_driver_disconnect_grace_expiry(None, Some(0)),
            GraceExpiryDecision::Reschedule
        );
        assert_eq!(
            decide_driver_disconnect_grace_expiry(Some(false), None),
            GraceExpiryDecision::Reschedule
        );
        assert_eq!(
            decide_driver_disconnect_grace_expiry(Some(true), Some(0)),
            GraceExpiryDecision::Cancel
        );
        assert_eq!(
            decide_driver_disconnect_grace_expiry(Some(true), None),
            GraceExpiryDecision::Cancel
        );
        assert_eq!(
            decide_driver_disconnect_grace_expiry(Some(false), Some(1)),
            GraceExpiryDecision::Cancel
        );
        assert_eq!(
            decide_driver_disconnect_grace_expiry(Some(false), Some(0)),
            GraceExpiryDecision::Stop
        );
    }

    #[test]
    fn driver_disconnect_cache_miss_retry_limit_returns_terminal_error() {
        let mut cache_misses = 0;

        for expected in 1..DRIVER_DISCONNECT_GRACE_MAX_CACHE_MISS_CHECKS {
            assert_eq!(
                driver_disconnect_cache_miss_terminal_error(&mut cache_misses),
                None
            );
            assert_eq!(cache_misses, expected);
        }

        let error = driver_disconnect_cache_miss_terminal_error(&mut cache_misses)
            .expect("retry limit should terminalize");
        assert_eq!(cache_misses, DRIVER_DISCONNECT_GRACE_MAX_CACHE_MISS_CHECKS);
        assert!(error.contains("voice state cache remained unavailable"));
    }

    #[test]
    fn recording_stop_retry_limit_returns_terminal_error() {
        let mut stop_failures = 0;

        for expected in 1..RECORDING_STOP_MAX_RETRIES {
            assert_eq!(
                recording_stop_terminal_error(&mut stop_failures, "auto-stop", "queue down"),
                None
            );
            assert_eq!(stop_failures, expected);
        }

        let error = recording_stop_terminal_error(&mut stop_failures, "auto-stop", "queue down")
            .expect("retry limit should terminalize");
        assert_eq!(stop_failures, RECORDING_STOP_MAX_RETRIES);
        assert!(error.contains("recording stop failed"));
        assert!(error.contains("queue down"));
    }

    #[test]
    fn recording_lookup_retry_limit_returns_terminal_error() {
        let mut lookup_failures = 0;

        for expected in 1..RECORDING_LOOKUP_MAX_RETRIES {
            assert_eq!(
                recording_lookup_terminal_error(
                    &mut lookup_failures,
                    "auto-stop grace",
                    "database down",
                ),
                None
            );
            assert_eq!(lookup_failures, expected);
        }

        let error = recording_lookup_terminal_error(
            &mut lookup_failures,
            "auto-stop grace",
            "database down",
        )
        .expect("retry limit should terminalize");
        assert_eq!(lookup_failures, RECORDING_LOOKUP_MAX_RETRIES);
        assert!(error.contains("recording state lookup failed"));
        assert!(error.contains("database down"));
    }

    #[test]
    fn recording_lookup_success_resets_retry_counter() {
        let mut lookup_failures = 0;

        assert_eq!(
            recording_lookup_terminal_error(&mut lookup_failures, "auto-stop grace", "db hiccup"),
            None
        );
        assert_eq!(lookup_failures, 1);

        reset_recording_lookup_failures(&mut lookup_failures);
        assert_eq!(lookup_failures, 0);

        assert_eq!(
            recording_lookup_terminal_error(&mut lookup_failures, "auto-stop grace", "db hiccup"),
            None
        );
        assert_eq!(lookup_failures, 1);
    }

    #[tokio::test]
    async fn lifecycle_write_permit_excludes_concurrent_callers() {
        let command_gate = Arc::new(RwLock::new(()));
        let first_permit = recording_lifecycle_write_permit_for_gate(command_gate.as_ref()).await;
        let (attempt_tx, attempt_rx) = tokio::sync::oneshot::channel();
        let (acquired_tx, mut acquired_rx) = tokio::sync::oneshot::channel();
        let gate_for_task = Arc::clone(&command_gate);
        let second_permit_task = tokio::spawn(async move {
            let _ = attempt_tx.send(());
            let _second_permit =
                recording_lifecycle_write_permit_for_gate(gate_for_task.as_ref()).await;
            let _ = acquired_tx.send(());
        });

        attempt_rx
            .await
            .expect("second lifecycle permit task should start");
        tokio::task::yield_now().await;
        assert!(
            matches!(
                acquired_rx.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ),
            "second lifecycle permit should wait while first permit is held"
        );

        drop(first_permit);
        tokio::time::timeout(Duration::from_secs(1), acquired_rx)
            .await
            .expect("second lifecycle permit should acquire after first permit drops")
            .expect("second lifecycle permit task should report acquisition");
        second_permit_task
            .await
            .expect("second permit task should not panic");
    }

    #[test]
    fn teardown_stop_error_classifies_terminal_absence_without_string_matching() {
        assert!(
            TeardownStopError::TargetAbsent(CommandError::NoActiveMeeting.to_string())
                .is_target_absent()
        );
        assert!(
            TeardownStopError::TargetAbsent(
                "active meeting changed before stop: expected=old, actual=new".to_owned(),
            )
            .is_target_absent()
        );
        assert!(!TeardownStopError::Other("database unavailable".to_owned()).is_target_absent());
    }

    #[test]
    fn teardown_stop_helper_returns_typed_target_absence() {
        let store = crate::infrastructure::storage::InMemoryMeetingStore::new();
        let mut service = BotCommandService::new(store);
        let mut queue = crate::infrastructure::queue::InMemoryJobQueue::new();

        let err = stop_and_enqueue_summary_job_for_teardown(
            &mut service,
            &mut queue,
            "g1",
            "u1",
            UserRole::BotAdmin,
            Some("m1"),
            StopReason::AutoEmpty,
        )
        .expect_err("missing active meeting should be typed as target absence");

        assert!(err.is_target_absent());
        assert_eq!(err.to_string(), CommandError::NoActiveMeeting.to_string());
    }

    #[test]
    fn final_flush_exhaustion_retains_session_until_terminal_cleanup_succeeds() {
        let mut store = FaultInjectedMeetingStore::with_recording_meeting();
        let mut local_state =
            RecordingLocalState::with_matching_session(FINAL_FLUSH_MAX_RETRIES as usize);

        for _ in 0..FINAL_FLUSH_MAX_RETRIES {
            let session = local_state
                .sessions
                .get_mut("g1")
                .expect("session should remain through flush retry attempts");
            let err = flush_session_for_teardown(session, "g1", "auto-stop")
                .expect_err("injected chunk persistence should fail");
            assert!(err.contains("failed to persist"));
            local_state.assert_matching_state_present();
        }

        mark_recording_failed_after_teardown_exhaustion(
            &mut store,
            "m1",
            "final audio flush failed after 10 auto-stop attempt(s): injected failure",
        )
        .expect("terminal status write should succeed after flush retry exhaustion");
        let removed = local_state.clear_expected_meeting();

        assert!(
            removed.is_some(),
            "terminal cleanup should evict the exhausted local session"
        );
        local_state.assert_matching_state_cleared();
        let meeting = store.meeting();
        assert_eq!(meeting.status, crate::domain::MeetingStatus::Failed);
        assert!(
            meeting
                .error_message
                .as_deref()
                .is_some_and(|message| message.contains("final audio flush failed"))
        );
    }

    #[test]
    fn teardown_exhaustion_status_update_failure_preserves_retryable_local_state() {
        let mut store = FaultInjectedMeetingStore::with_recording_meeting().fail_status_updates(1);
        let local_state = RecordingLocalState::with_matching_session(0);

        let err = mark_recording_failed_after_teardown_exhaustion(
            &mut store,
            "m1",
            "recording stop failed after retries",
        )
        .expect_err("backend write failure should be surfaced for retry");

        assert!(err.to_string().contains("status update unavailable"));
        assert_eq!(
            store.meeting().status,
            crate::domain::MeetingStatus::Recording
        );
        assert_eq!(store.meeting().error_message, None);
        local_state.assert_matching_state_present();
    }

    #[test]
    fn teardown_exhaustion_error_message_failure_still_terminalizes_recording() {
        let mut store =
            FaultInjectedMeetingStore::with_recording_meeting().fail_error_message_updates(1);
        let mut local_state = RecordingLocalState::with_matching_session(0);

        mark_recording_failed_after_teardown_exhaustion(
            &mut store,
            "m1",
            "recording stop failed after retries",
        )
        .expect("status update should win even if error text persistence fails");
        let removed = local_state.clear_expected_meeting();

        assert!(removed.is_some());
        local_state.assert_matching_state_cleared();
        let meeting = store.meeting();
        assert_eq!(meeting.status, crate::domain::MeetingStatus::Failed);
        assert_eq!(
            meeting.error_message, None,
            "error-message write failure is logged but does not revive recording state"
        );
    }

    #[test]
    fn terminal_cleanup_retry_exhaustion_preserves_local_state_when_db_update_fails() {
        let mut store =
            FaultInjectedMeetingStore::with_recording_meeting().fail_status_updates(usize::MAX);
        let local_state = RecordingLocalState::with_matching_session(0);
        let mut terminal_cleanup_failures = 0;

        for expected_attempts in 1..RECORDING_TERMINAL_CLEANUP_MAX_RETRIES {
            let outcome = record_terminal_cleanup_retry_failure(
                &mut terminal_cleanup_failures,
                "auto-stop stop exhaustion",
                "status update unavailable",
            );
            assert_eq!(outcome.attempts, expected_attempts);
            assert_eq!(outcome.terminal_error, None);
            local_state.assert_matching_state_present();
        }

        let outcome = record_terminal_cleanup_retry_failure(
            &mut terminal_cleanup_failures,
            "auto-stop stop exhaustion",
            "status update unavailable",
        );
        let terminal_error = outcome
            .terminal_error
            .as_deref()
            .expect("retry limit should request local cleanup");

        let err = persist_terminal_cleanup_retry_exhaustion(&mut store, "m1", terminal_error)
            .expect_err("DB outage should preserve local state for retry");

        assert!(err.to_string().contains("status update unavailable"));
        local_state.assert_matching_state_present();
        assert_eq!(
            store.meeting().status,
            crate::domain::MeetingStatus::Recording,
            "DB outage is not hidden by local cleanup"
        );
    }

    #[test]
    fn terminal_cleanup_persistence_failure_restarts_retry_window() {
        let mut terminal_cleanup_failures = RECORDING_TERMINAL_CLEANUP_MAX_RETRIES - 1;

        let outcome = record_terminal_cleanup_retry_failure(
            &mut terminal_cleanup_failures,
            "auto-stop stop exhaustion",
            "status update unavailable",
        );
        assert!(outcome.terminal_error.is_some());

        restart_terminal_cleanup_retry_window_after_persistence_failure(
            &mut terminal_cleanup_failures,
        );
        assert_eq!(terminal_cleanup_failures, 0);

        let outcome = record_terminal_cleanup_retry_failure(
            &mut terminal_cleanup_failures,
            "auto-stop stop exhaustion",
            "status update unavailable",
        );
        assert_eq!(outcome.attempts, 1);
        assert!(
            outcome.terminal_error.is_none(),
            "the next retry should wait for the cleanup window before terminal persistence"
        );
    }

    #[test]
    fn terminal_cleanup_retry_exhaustion_clears_local_state_after_db_terminalizes() {
        let mut store = FaultInjectedMeetingStore::with_recording_meeting();
        let mut local_state = RecordingLocalState::with_matching_session(0);
        let mut terminal_cleanup_failures = RECORDING_TERMINAL_CLEANUP_MAX_RETRIES - 1;

        let outcome = record_terminal_cleanup_retry_failure(
            &mut terminal_cleanup_failures,
            "auto-stop stop exhaustion",
            "status update unavailable",
        );
        let terminal_error = outcome
            .terminal_error
            .as_deref()
            .expect("retry limit should request local cleanup");
        persist_terminal_cleanup_retry_exhaustion(&mut store, "m1", terminal_error)
            .expect("terminal DB update must succeed before local cleanup");
        let removed = local_state.clear_expected_meeting();

        assert!(removed.is_some());
        local_state.assert_matching_state_cleared();
        assert_eq!(store.meeting().status, crate::domain::MeetingStatus::Failed);
    }

    #[tokio::test]
    async fn async_terminal_cleanup_retry_preserves_state_until_store_recovers() {
        let store = FaultInjectedMeetingStore::with_recording_meeting().fail_status_updates(1);
        let local_state = RecordingLocalState::with_matching_session(0);
        let harness = async_lifecycle_harness(store, local_state);
        let mut terminal_cleanup_failures = RECORDING_TERMINAL_CLEANUP_MAX_RETRIES - 1;
        let local = harness.local_state();

        let decision = handle_terminal_cleanup_retry_failure_with_dependencies(
            &harness.service,
            &local,
            TerminalCleanupRetryFailureRequest {
                guild_key: "g1",
                expected_meeting_id: "m1",
                phase: "auto-stop stop exhaustion",
                err: "status update unavailable",
            },
            &mut terminal_cleanup_failures,
        )
        .await;

        assert!(matches!(decision, TerminalCleanupRetryDecision::Reschedule));
        assert_eq!(
            terminal_cleanup_failures, 0,
            "persistence failures should preserve local state while restarting the retry window"
        );
        harness.assert_matching_state_present().await;
        {
            let service = harness.service.lock().await;
            assert_eq!(
                service.store.meeting().status,
                crate::domain::MeetingStatus::Recording
            );
        }

        terminal_cleanup_failures = RECORDING_TERMINAL_CLEANUP_MAX_RETRIES - 1;
        let decision = handle_terminal_cleanup_retry_failure_with_dependencies(
            &harness.service,
            &local,
            TerminalCleanupRetryFailureRequest {
                guild_key: "g1",
                expected_meeting_id: "m1",
                phase: "auto-stop stop exhaustion",
                err: "status update unavailable",
            },
            &mut terminal_cleanup_failures,
        )
        .await;

        match decision {
            TerminalCleanupRetryDecision::Cleared { removed_session } => {
                assert!(removed_session.is_some());
            }
            TerminalCleanupRetryDecision::Reschedule => {
                panic!("terminal cleanup should clear once the store recovers")
            }
        }
        harness.assert_matching_state_cleared().await;
        let service = harness.service.lock().await;
        assert_eq!(
            service.store.meeting().status,
            crate::domain::MeetingStatus::Failed
        );
    }

    #[tokio::test]
    async fn async_terminal_cleanup_retry_waits_full_window_after_store_failure() {
        let store = FaultInjectedMeetingStore::with_recording_meeting().fail_status_updates(1);
        let local_state = RecordingLocalState::with_matching_session(0);
        let harness = async_lifecycle_harness(store, local_state);
        let mut terminal_cleanup_failures = RECORDING_TERMINAL_CLEANUP_MAX_RETRIES - 1;
        let local = harness.local_state();
        let request = TerminalCleanupRetryFailureRequest {
            guild_key: "g1",
            expected_meeting_id: "m1",
            phase: "auto-stop stop exhaustion",
            err: "status update unavailable",
        };

        let decision = handle_terminal_cleanup_retry_failure_with_dependencies(
            &harness.service,
            &local,
            request,
            &mut terminal_cleanup_failures,
        )
        .await;
        assert!(matches!(decision, TerminalCleanupRetryDecision::Reschedule));
        assert_eq!(terminal_cleanup_failures, 0);
        {
            let service = harness.service.lock().await;
            assert_eq!(service.store.status_update_attempts(), 1);
        }

        for expected_attempts in 1..RECORDING_TERMINAL_CLEANUP_MAX_RETRIES {
            let decision = handle_terminal_cleanup_retry_failure_with_dependencies(
                &harness.service,
                &local,
                request,
                &mut terminal_cleanup_failures,
            )
            .await;

            assert!(matches!(decision, TerminalCleanupRetryDecision::Reschedule));
            assert_eq!(terminal_cleanup_failures, expected_attempts);
            harness.assert_matching_state_present().await;
            let service = harness.service.lock().await;
            assert_eq!(
                service.store.status_update_attempts(),
                1,
                "store terminalization must wait for the full retry window"
            );
            assert_eq!(
                service.store.meeting().status,
                crate::domain::MeetingStatus::Recording
            );
        }

        let decision = handle_terminal_cleanup_retry_failure_with_dependencies(
            &harness.service,
            &local,
            request,
            &mut terminal_cleanup_failures,
        )
        .await;

        match decision {
            TerminalCleanupRetryDecision::Cleared { removed_session } => {
                assert!(removed_session.is_some());
            }
            TerminalCleanupRetryDecision::Reschedule => {
                panic!("terminal cleanup should clear once the retry window elapses")
            }
        }
        harness.assert_matching_state_cleared().await;
        let service = harness.service.lock().await;
        assert_eq!(service.store.status_update_attempts(), 2);
        assert_eq!(
            service.store.meeting().status,
            crate::domain::MeetingStatus::Failed
        );
    }

    #[tokio::test]
    async fn async_teardown_exhaustion_store_failure_keeps_state_and_skips_voice_leave() {
        let store = FaultInjectedMeetingStore::with_recording_meeting().fail_status_updates(1);
        let saved_chunks = Arc::new(AtomicUsize::new(0));
        let local_state = RecordingLocalState::with_matching_session_and_saved_counter(
            0,
            Arc::clone(&saved_chunks),
        );
        let harness = async_lifecycle_harness(store, local_state);
        let voice = StubVoiceGateway::default();
        let local = harness.local_state();
        let voice_leave = Some(&voice);
        let persisted_mappings = Arc::new(AtomicUsize::new(0));

        let err = fail_recording_after_teardown_exhaustion_with_dependencies(
            &harness.service,
            &local,
            &voice_leave,
            TerminalAbsenceCleanupRequest {
                guild_id: GuildId::new(1),
                guild_key: "g1",
                expected_meeting_id: "m1",
                phase: "teardown exhaustion",
            },
            "recording stop failed after retries",
            {
                let persisted_mappings = Arc::clone(&persisted_mappings);
                move |_, _| {
                    persisted_mappings.fetch_add(1, Ordering::SeqCst);
                }
            },
        )
        .await
        .expect_err("store failure should be surfaced before local cleanup");

        assert!(err.contains("status update unavailable"));
        assert_eq!(voice.leaves.load(Ordering::SeqCst), 0);
        assert_eq!(
            saved_chunks.load(Ordering::SeqCst),
            0,
            "store failure must not flush tail audio before terminal DB state is durable"
        );
        assert_eq!(
            persisted_mappings.load(Ordering::SeqCst),
            0,
            "store failure must not persist SSRC mappings before terminal DB state is durable"
        );
        harness.assert_matching_state_present().await;
        let service = harness.service.lock().await;
        assert_eq!(
            service.store.meeting().status,
            crate::domain::MeetingStatus::Recording
        );
    }

    #[tokio::test]
    async fn async_teardown_exhaustion_tolerates_voice_leave_failure_after_terminalizing() {
        let store = FaultInjectedMeetingStore::with_recording_meeting();
        let saved_chunks = Arc::new(AtomicUsize::new(0));
        let local_state = RecordingLocalState::with_matching_session_and_saved_counter(
            0,
            Arc::clone(&saved_chunks),
        );
        let harness = async_lifecycle_harness(store, local_state);
        let voice = StubVoiceGateway::failing_once();
        let local = harness.local_state();
        let voice_leave = Some(&voice);
        let persisted_mappings = Arc::new(AtomicUsize::new(0));

        let leave_outcome = fail_recording_after_teardown_exhaustion_with_dependencies(
            &harness.service,
            &local,
            &voice_leave,
            TerminalAbsenceCleanupRequest {
                guild_id: GuildId::new(1),
                guild_key: "g1",
                expected_meeting_id: "m1",
                phase: "teardown exhaustion",
            },
            "recording stop failed after retries",
            {
                let persisted_mappings = Arc::clone(&persisted_mappings);
                move |_, _| {
                    persisted_mappings.fetch_add(1, Ordering::SeqCst);
                }
            },
        )
        .await
        .expect("voice leave failure should not undo terminal cleanup");

        assert_eq!(leave_outcome, Some(RecordingVoiceLeaveOutcome::Failed));
        assert_eq!(voice.leaves.load(Ordering::SeqCst), 1);
        assert_eq!(
            saved_chunks.load(Ordering::SeqCst),
            1,
            "teardown exhaustion must flush tail audio after terminalizing even when leave fails"
        );
        assert_eq!(
            persisted_mappings.load(Ordering::SeqCst),
            1,
            "teardown exhaustion must persist final SSRC mappings after terminalizing"
        );
        harness.assert_matching_state_cleared().await;
        let service = harness.service.lock().await;
        assert_eq!(
            service.store.meeting().status,
            crate::domain::MeetingStatus::Failed
        );
        assert_eq!(
            service.store.meeting().error_message.as_deref(),
            Some("recording stop failed after retries")
        );
    }

    #[tokio::test]
    async fn async_failed_start_cleanup_retries_store_failure_before_clearing_startup() {
        let store = FaultInjectedMeetingStore::with_recording_meeting().fail_status_updates(1);
        let saved_chunks = Arc::new(AtomicUsize::new(0));
        let local_state = RecordingLocalState::with_matching_session_and_saved_counter(
            0,
            Arc::clone(&saved_chunks),
        );
        let harness = async_lifecycle_harness(store, local_state);
        let local = harness.local_state();
        let persisted_mappings = Arc::new(AtomicUsize::new(0));

        let cleaned = try_cleanup_failed_recording_start_with_dependencies(
            &harness.service,
            &local,
            "g1",
            "m1",
            "voice join failed after setup",
            FailedRecordingStartLocalCleanup::FullRuntimeState,
            {
                let persisted_mappings = Arc::clone(&persisted_mappings);
                move |_, _| {
                    persisted_mappings.fetch_add(1, Ordering::SeqCst);
                }
            },
        )
        .await;

        assert!(
            !cleaned,
            "failed-start cleanup must retry while the store write is unavailable"
        );
        assert_eq!(
            saved_chunks.load(Ordering::SeqCst),
            0,
            "store failure must not flush failed-start tail audio before DB failure is durable"
        );
        assert_eq!(
            persisted_mappings.load(Ordering::SeqCst),
            0,
            "store failure must not persist failed-start SSRC mappings before DB failure is durable"
        );
        harness.assert_matching_state_present().await;
        {
            let service = harness.service.lock().await;
            assert_eq!(
                service.store.meeting().status,
                crate::domain::MeetingStatus::Recording
            );
        }

        let cleaned = try_cleanup_failed_recording_start_with_dependencies(
            &harness.service,
            &local,
            "g1",
            "m1",
            "voice join failed after setup",
            FailedRecordingStartLocalCleanup::FullRuntimeState,
            {
                let persisted_mappings = Arc::clone(&persisted_mappings);
                move |_, _| {
                    persisted_mappings.fetch_add(1, Ordering::SeqCst);
                }
            },
        )
        .await;

        assert!(cleaned);
        assert_eq!(
            saved_chunks.load(Ordering::SeqCst),
            1,
            "successful failed-start cleanup must flush tail audio from the removed session"
        );
        assert_eq!(
            persisted_mappings.load(Ordering::SeqCst),
            1,
            "successful failed-start cleanup must persist final SSRC mappings"
        );
        harness.assert_matching_state_cleared().await;
        let service = harness.service.lock().await;
        assert_eq!(
            service.store.meeting().status,
            crate::domain::MeetingStatus::Failed
        );
        assert_eq!(
            service.store.meeting().error_message.as_deref(),
            Some("voice join failed after setup")
        );
    }

    #[tokio::test]
    async fn async_failed_start_startup_only_retries_store_before_clearing_reservation() {
        let store = FaultInjectedMeetingStore::with_recording_meeting().fail_status_updates(1);
        let local_state = RecordingLocalState::with_startup_only();
        let harness = async_lifecycle_harness(store, local_state);
        let local = harness.local_state();
        let persisted_mappings = Arc::new(AtomicUsize::new(0));

        let cleaned = try_cleanup_failed_recording_start_with_dependencies(
            &harness.service,
            &local,
            "g1",
            "m1",
            "voice join failed before session setup",
            FailedRecordingStartLocalCleanup::StartupOnly,
            {
                let persisted_mappings = Arc::clone(&persisted_mappings);
                move |_, _| {
                    persisted_mappings.fetch_add(1, Ordering::SeqCst);
                }
            },
        )
        .await;

        assert!(!cleaned);
        assert!(
            harness.sessions.lock().await.is_empty(),
            "pre-session cleanup starts before any recording session exists"
        );
        assert!(
            harness.auto_stop_states.lock().await.is_empty(),
            "pre-session cleanup starts before auto-stop state exists"
        );
        assert!(
            harness.live_transcription_titles.lock().await.is_empty(),
            "pre-session cleanup starts before title cache insertion"
        );
        assert_eq!(
            harness
                .recording_startups
                .lock()
                .await
                .get("g1")
                .map(String::as_str),
            Some("m1"),
            "startup reservation should remain retryable while the store write is unavailable"
        );
        assert_eq!(
            persisted_mappings.load(Ordering::SeqCst),
            0,
            "StartupOnly cleanup must not persist SSRC mappings while the store write fails"
        );

        let cleaned = try_cleanup_failed_recording_start_with_dependencies(
            &harness.service,
            &local,
            "g1",
            "m1",
            "voice join failed before session setup",
            FailedRecordingStartLocalCleanup::StartupOnly,
            {
                let persisted_mappings = Arc::clone(&persisted_mappings);
                move |_, _| {
                    persisted_mappings.fetch_add(1, Ordering::SeqCst);
                }
            },
        )
        .await;

        assert!(cleaned);
        assert!(
            harness.sessions.lock().await.is_empty(),
            "pre-session cleanup scope runs before any recording session exists"
        );
        assert!(
            harness.auto_stop_states.lock().await.is_empty(),
            "pre-session cleanup scope runs before auto-stop state exists"
        );
        assert!(
            harness.live_transcription_titles.lock().await.is_empty(),
            "pre-session cleanup scope runs before title cache insertion"
        );
        assert!(!harness.recording_startups.lock().await.contains_key("g1"));
        assert_eq!(
            persisted_mappings.load(Ordering::SeqCst),
            0,
            "StartupOnly cleanup must not persist SSRC mappings"
        );
        let service = harness.service.lock().await;
        assert_eq!(
            service.store.meeting().status,
            crate::domain::MeetingStatus::Failed
        );
        assert_eq!(
            service.store.meeting().error_message.as_deref(),
            Some("voice join failed before session setup")
        );
    }

    #[tokio::test]
    async fn async_failed_start_retry_exhaustion_force_marks_failed_and_clears_full_state() {
        let store = FaultInjectedMeetingStore::with_recording_meeting();
        let saved_chunks = Arc::new(AtomicUsize::new(0));
        let local_state = RecordingLocalState::with_matching_session_and_saved_counter(
            0,
            Arc::clone(&saved_chunks),
        );
        let harness = async_lifecycle_harness(store, local_state);
        let local = harness.local_state();
        let persisted_mappings = Arc::new(AtomicUsize::new(0));

        finish_failed_recording_start_cleanup_retry_exhaustion_with_dependencies(
            &harness.service,
            &local,
            "g1",
            "m1",
            "voice join cleanup retries exhausted",
            FailedRecordingStartLocalCleanup::FullRuntimeState,
            {
                let persisted_mappings = Arc::clone(&persisted_mappings);
                move |_, _| {
                    persisted_mappings.fetch_add(1, Ordering::SeqCst);
                }
            },
        )
        .await;

        assert_eq!(
            saved_chunks.load(Ordering::SeqCst),
            1,
            "cleanup retry exhaustion must flush tail audio before clearing the session"
        );
        assert_eq!(
            persisted_mappings.load(Ordering::SeqCst),
            1,
            "cleanup retry exhaustion must persist final SSRC mappings"
        );
        harness.assert_matching_state_cleared().await;
        let service = harness.service.lock().await;
        assert_eq!(
            service.store.meeting().status,
            crate::domain::MeetingStatus::Failed
        );
        assert_eq!(
            service.store.meeting().error_message.as_deref(),
            Some("voice join cleanup retries exhausted")
        );
    }

    #[tokio::test]
    async fn async_stale_auto_stop_timer_clears_old_local_state_without_touching_successor_timer() {
        let store = FaultInjectedMeetingStore::with_recording_meeting();
        let saved_chunks = Arc::new(AtomicUsize::new(0));
        let mut local_state = RecordingLocalState::with_matching_session_and_saved_counter(
            0,
            Arc::clone(&saved_chunks),
        );
        local_state.auto_stop_states = HashMap::from([(
            "g1".to_owned(),
            AutoStopState::new_for_meeting(Duration::from_secs(0), Some("m2".to_owned())),
        )]);
        let harness = async_lifecycle_harness(store, local_state);
        let local = harness.local_state();
        let persisted_mappings = Arc::new(AtomicUsize::new(0));

        clear_failed_recording_start_local_state_with_dependencies(
            &local,
            "g1",
            "m1",
            FailedRecordingStartLocalCleanup::FullRuntimeState,
            {
                let persisted_mappings = Arc::clone(&persisted_mappings);
                move |_, _| {
                    persisted_mappings.fetch_add(1, Ordering::SeqCst);
                }
            },
        )
        .await;

        assert_eq!(
            saved_chunks.load(Ordering::SeqCst),
            1,
            "stale auto-stop cleanup should flush tail audio from the old session"
        );
        assert_eq!(
            persisted_mappings.load(Ordering::SeqCst),
            1,
            "stale auto-stop cleanup should persist final SSRC mappings"
        );
        assert!(!harness.sessions.lock().await.contains_key("g1"));
        assert!(!harness.recording_startups.lock().await.contains_key("g1"));
        assert!(
            !harness
                .live_transcription_titles
                .lock()
                .await
                .contains_key("m1")
        );
        assert!(
            harness
                .auto_stop_states
                .lock()
                .await
                .get("g1")
                .is_some_and(|state| state.belongs_to_meeting("m2")),
            "successor auto-stop state must survive stale timer cleanup"
        );
    }

    #[tokio::test]
    async fn async_terminal_absence_cleanup_does_not_leave_successor_recording() {
        let store = FaultInjectedMeetingStore::with_recording_meeting();
        let local_state = RecordingLocalState::with_newer_session_on_same_guild();
        let harness = async_lifecycle_harness(store, local_state);
        let voice = StubVoiceGateway::default();
        let local = harness.local_state();
        let voice_leave = Some(&voice);

        let removed_session =
            remove_local_recording_state_after_terminal_absence_with_dependencies(
                &local, "g1", "m1",
            )
            .await;

        assert!(
            removed_session.is_none(),
            "stale terminal cleanup must not remove a successor session"
        );
        let leave_outcome = finish_terminal_absence_cleanup_with_dependencies(
            &local,
            &voice_leave,
            TerminalAbsenceCleanupRequest {
                guild_id: GuildId::new(1),
                guild_key: "g1",
                expected_meeting_id: "m1",
                phase: "terminal absence",
            },
            removed_session,
            ignore_ssrc_mapping_persistence,
        )
        .await;

        assert_eq!(leave_outcome, None);
        assert_eq!(
            voice.leaves.load(Ordering::SeqCst),
            0,
            "successor recordings must not be disconnected by stale cleanup"
        );
        assert!(
            harness
                .sessions
                .lock()
                .await
                .get("g1")
                .is_some_and(|session| session.meeting_id == "m2")
        );
        assert!(
            harness
                .auto_stop_states
                .lock()
                .await
                .get("g1")
                .is_some_and(|state| state.belongs_to_meeting("m2"))
        );
        assert_eq!(
            harness
                .recording_startups
                .lock()
                .await
                .get("g1")
                .map(String::as_str),
            Some("m2")
        );
    }

    #[tokio::test]
    async fn async_terminal_absence_cleanup_leaves_flushes_and_persists_removed_session() {
        let store = FaultInjectedMeetingStore::with_recording_meeting();
        let saved_chunks = Arc::new(AtomicUsize::new(0));
        let local_state = RecordingLocalState::with_matching_session_and_saved_counter(
            0,
            Arc::clone(&saved_chunks),
        );
        let harness = async_lifecycle_harness(store, local_state);
        let voice = StubVoiceGateway::default();
        let local = harness.local_state();
        let voice_leave = Some(&voice);
        let persisted_mappings = Arc::new(AtomicUsize::new(0));

        let removed_session =
            remove_local_recording_state_after_terminal_absence_with_dependencies(
                &local, "g1", "m1",
            )
            .await;

        assert!(
            removed_session.is_some(),
            "matching target-absent cleanup should remove the stale session"
        );
        let leave_outcome = finish_terminal_absence_cleanup_with_dependencies(
            &local,
            &voice_leave,
            TerminalAbsenceCleanupRequest {
                guild_id: GuildId::new(1),
                guild_key: "g1",
                expected_meeting_id: "m1",
                phase: "terminal absence",
            },
            removed_session,
            {
                let persisted_mappings = Arc::clone(&persisted_mappings);
                move |_, _| {
                    persisted_mappings.fetch_add(1, Ordering::SeqCst);
                }
            },
        )
        .await;

        assert_eq!(leave_outcome, Some(RecordingVoiceLeaveOutcome::Succeeded));
        assert_eq!(voice.leaves.load(Ordering::SeqCst), 1);
        assert_eq!(
            saved_chunks.load(Ordering::SeqCst),
            1,
            "target-absent cleanup must flush tail audio from the removed session"
        );
        assert_eq!(
            persisted_mappings.load(Ordering::SeqCst),
            1,
            "target-absent cleanup must persist final SSRC mapping after flush"
        );
        harness.assert_matching_state_cleared().await;
    }

    #[test]
    fn auto_stop_and_driver_disconnect_cache_misses_keep_state_until_terminal_error() {
        for (context, max_cache_miss_checks, terminal_error_for_attempt) in [
            (
                "auto-stop",
                AUTO_STOP_GRACE_MAX_CACHE_MISS_CHECKS,
                auto_stop_cache_miss_terminal_error as fn(&mut u32) -> Option<String>,
            ),
            (
                "driver-disconnect",
                DRIVER_DISCONNECT_GRACE_MAX_CACHE_MISS_CHECKS,
                driver_disconnect_cache_miss_terminal_error as fn(&mut u32) -> Option<String>,
            ),
        ] {
            let mut store = FaultInjectedMeetingStore::with_recording_meeting();
            let mut local_state = RecordingLocalState::with_matching_session(0);
            let mut cache_misses = 0;

            for expected_misses in 1..max_cache_miss_checks {
                let terminal_error = terminal_error_for_attempt(&mut cache_misses);
                assert_eq!(terminal_error, None, "{context} should reschedule");
                assert_eq!(cache_misses, expected_misses);
                assert!(rearm_auto_stop_state_for_retry(
                    &mut local_state.auto_stop_states,
                    "g1",
                    "m1"
                ));
                local_state.assert_matching_state_present();
            }

            let terminal_error = terminal_error_for_attempt(&mut cache_misses);
            let terminal_error =
                terminal_error.expect("cache-miss retry limit should become terminal");
            mark_recording_failed_after_teardown_exhaustion(&mut store, "m1", &terminal_error)
                .expect("terminal cache-miss cleanup should mark recording failed");
            let removed = local_state.clear_expected_meeting();

            assert!(
                removed.is_some(),
                "{context} terminal cleanup should remove session"
            );
            local_state.assert_matching_state_cleared();
            assert_eq!(store.meeting().status, crate::domain::MeetingStatus::Failed);
            assert!(
                store
                    .meeting()
                    .error_message
                    .as_deref()
                    .is_some_and(|message| message.contains("voice state cache")),
                "{context} should persist the cache-miss terminal reason"
            );
        }
    }

    #[test]
    fn active_meeting_lookup_failures_terminalize_after_bounded_reschedules() {
        let mut store = FaultInjectedMeetingStore::with_recording_meeting()
            .fail_active_meeting_lookups(RECORDING_LOOKUP_MAX_RETRIES as usize);
        let mut lookup_failures = 0;

        for expected_attempts in 1..RECORDING_LOOKUP_MAX_RETRIES {
            let err = store
                .find_active_meeting_by_guild("g1")
                .expect_err("lookup should be fault-injected")
                .to_string();
            let terminal_error =
                recording_lookup_terminal_error(&mut lookup_failures, "auto-stop grace", &err);
            assert_eq!(terminal_error, None);
            assert_eq!(lookup_failures, expected_attempts);
        }

        let err = store
            .find_active_meeting_by_guild("g1")
            .expect_err("lookup should fail through terminal attempt")
            .to_string();
        let terminal_error =
            recording_lookup_terminal_error(&mut lookup_failures, "auto-stop grace", &err);
        let terminal_error = terminal_error.expect("lookup retry limit should terminalize");

        assert_eq!(lookup_failures, RECORDING_LOOKUP_MAX_RETRIES);
        assert!(terminal_error.contains("recording state lookup failed"));
        mark_recording_failed_after_teardown_exhaustion(&mut store, "m1", &terminal_error)
            .expect("lookup exhaustion should mark recording failed");
        assert_eq!(store.meeting().status, crate::domain::MeetingStatus::Failed);
    }

    #[test]
    fn stale_terminal_cleanup_preserves_newer_recording_state_on_same_guild() {
        let mut local_state = RecordingLocalState::with_newer_session_on_same_guild();

        let removed = local_state.clear_expected_meeting();

        assert!(
            removed.is_none(),
            "stale cleanup for m1 must not evict the newer m2 session"
        );
        assert!(
            local_state
                .sessions
                .get("g1")
                .is_some_and(|session| session.meeting_id == "m2")
        );
        assert!(
            local_state
                .auto_stop_states
                .get("g1")
                .is_some_and(|state| state.belongs_to_meeting("m2"))
        );
        assert_eq!(
            local_state.recording_startups.get("g1").map(String::as_str),
            Some("m2")
        );
        assert!(local_state.live_transcription_titles.contains_key("m2"));
    }

    #[test]
    fn teardown_exhaustion_marks_recording_failed() {
        let mut store = crate::infrastructure::storage::InMemoryMeetingStore::new();
        store.insert(recording_meeting());

        mark_recording_failed_after_teardown_exhaustion(
            &mut store,
            "m1",
            "final audio flush failed after retries",
        )
        .expect("recording should become failed");

        let meeting = store.get("m1").expect("meeting should remain");
        assert_eq!(meeting.status, crate::domain::MeetingStatus::Failed);
        assert_eq!(
            meeting.error_message.as_deref(),
            Some("final audio flush failed after retries")
        );
    }

    #[test]
    fn teardown_exhaustion_marks_stopping_failed() {
        let mut store = crate::infrastructure::storage::InMemoryMeetingStore::new();
        let mut meeting = recording_meeting();
        meeting.status = crate::domain::MeetingStatus::Stopping;
        store.insert(meeting);

        mark_recording_failed_after_teardown_exhaustion(
            &mut store,
            "m1",
            "recording stop failed after retries",
        )
        .expect("stopping recording should become failed");

        let meeting = store.get("m1").expect("meeting should remain");
        assert_eq!(meeting.status, crate::domain::MeetingStatus::Failed);
        assert_eq!(
            meeting.error_message.as_deref(),
            Some("recording stop failed after retries")
        );
    }

    #[test]
    fn auto_stop_rearm_does_not_mutate_newer_meeting_state() {
        let mut states = HashMap::new();
        states.insert(
            "g1".to_owned(),
            AutoStopState::new_for_meeting(Duration::from_secs(5), Some("m2".to_owned())),
        );

        assert!(!rearm_auto_stop_state_for_retry(&mut states, "g1", "m1"));
        assert!(
            states
                .get("g1")
                .expect("newer meeting state should remain")
                .belongs_to_meeting("m2")
        );
    }

    #[test]
    fn teardown_exhaustion_ignores_already_terminal_meeting() {
        let mut store = crate::infrastructure::storage::InMemoryMeetingStore::new();
        let mut meeting = recording_meeting();
        meeting.status = crate::domain::MeetingStatus::Posted;
        store.insert(meeting);

        mark_recording_failed_after_teardown_exhaustion(
            &mut store,
            "m1",
            "final audio flush failed after retries",
        )
        .expect("terminal rows should be treated as already handled");
        let meeting = store.get("m1").expect("meeting should remain");
        assert_eq!(meeting.status, crate::domain::MeetingStatus::Posted);
        assert_eq!(meeting.error_message, None);
    }

    #[test]
    fn teardown_exhaustion_ignores_missing_meeting() {
        let mut store = crate::infrastructure::storage::InMemoryMeetingStore::new();

        mark_recording_failed_after_teardown_exhaustion(
            &mut store,
            "missing",
            "final audio flush failed after retries",
        )
        .expect("missing rows should be treated as already handled");

        assert!(store.get("missing").is_none());
    }

    #[test]
    fn record_start_setup_cleanup_preserves_concurrent_stop() {
        let mut store = crate::infrastructure::storage::InMemoryMeetingStore::new();
        let mut meeting = recording_meeting();
        meeting.status = crate::domain::MeetingStatus::Stopping;
        store.insert(meeting);

        mark_recording_start_failed_after_setup_error(
            &mut store,
            "m1",
            "voice join failed after stop started",
        )
        .expect("concurrent stop should be treated as already handled");

        let meeting = store.get("m1").expect("meeting should remain");
        assert_eq!(meeting.status, crate::domain::MeetingStatus::Stopping);
        assert_eq!(meeting.error_message, None);
    }

    #[test]
    fn record_start_setup_cleanup_db_failure_preserves_local_state() {
        let mut store = FaultInjectedMeetingStore::with_recording_meeting().fail_status_updates(1);
        let mut local_state = RecordingLocalState::with_matching_session(0);

        let err = mark_recording_start_failed_after_setup_error(
            &mut store,
            "m1",
            "voice join failed after setup",
        )
        .expect_err("backend failure should stop local cleanup");

        assert!(err.to_string().contains("status update unavailable"));
        assert_eq!(
            store.meeting().status,
            crate::domain::MeetingStatus::Recording
        );
        local_state.assert_matching_state_present();

        mark_recording_start_failed_after_setup_error(
            &mut store,
            "m1",
            "voice join failed after setup",
        )
        .expect("retry should mark failed once store recovers");
        let removed = local_state.clear_expected_meeting();

        assert!(removed.is_some());
        local_state.assert_matching_state_cleared();
        let meeting = store.meeting();
        assert_eq!(meeting.status, crate::domain::MeetingStatus::Failed);
        assert_eq!(
            meeting.error_message.as_deref(),
            Some("voice join failed after setup")
        );
    }

    #[test]
    fn exhausted_start_cleanup_retry_force_marks_recording_failed() {
        let mut store = crate::infrastructure::storage::InMemoryMeetingStore::new();
        store.insert(recording_meeting());

        assert!(
            best_effort_mark_recording_start_failed_after_cleanup_retry_exhaustion(
                &mut store,
                "m1",
                "voice join failed before session setup",
            )
        );

        let meeting = store.get("m1").expect("meeting should remain");
        assert_eq!(meeting.status, crate::domain::MeetingStatus::Failed);
        assert_eq!(
            meeting.error_message.as_deref(),
            Some("voice join failed before session setup")
        );
    }

    #[test]
    fn pre_session_start_cleanup_preserves_legacy_unscoped_auto_stop_state() {
        let mut states = HashMap::from([(
            "g1".to_owned(),
            AutoStopState::new_for_meeting(Duration::from_secs(5), None),
        )]);

        remove_auto_stop_state_for_failed_recording_start_cleanup(
            &mut states,
            "g1",
            "m1",
            FailedRecordingStartLocalCleanup::StartupOnly,
        );

        assert!(states.contains_key("g1"));
    }

    #[test]
    fn full_start_cleanup_removes_matching_auto_stop_state() {
        let mut states = HashMap::from([(
            "g1".to_owned(),
            AutoStopState::new_for_meeting(Duration::from_secs(5), Some("m1".to_owned())),
        )]);

        remove_auto_stop_state_for_failed_recording_start_cleanup(
            &mut states,
            "g1",
            "m1",
            FailedRecordingStartLocalCleanup::FullRuntimeState,
        );

        assert!(!states.contains_key("g1"));
    }

    #[test]
    fn exhausted_start_cleanup_retry_can_release_startup_reservation() {
        let mut startups = HashMap::from([("g1".to_owned(), "m1".to_owned())]);

        clear_matching_recording_startup(&mut startups, "g1", "m1");

        assert!(!startups.contains_key("g1"));
    }

    #[test]
    fn voice_join_retry_cleanup_distinguishes_removed_and_successor_sessions() {
        assert_eq!(
            classify_voice_join_retry_cleanup(Some("m1"), "m1"),
            VoiceJoinRetryCleanup::RetryCurrentSession
        );
        assert_eq!(
            classify_voice_join_retry_cleanup(None, "m1"),
            VoiceJoinRetryCleanup::StopAfterSessionRemoved
        );
        assert_eq!(
            classify_voice_join_retry_cleanup(Some("successor"), "m1"),
            VoiceJoinRetryCleanup::StopAfterSessionReplaced
        );
        assert_eq!(
            voice_join_retry_cleanup_leave_phase(VoiceJoinRetryCleanup::RetryCurrentSession),
            Some("record-start retry cleanup")
        );
        assert_eq!(
            voice_join_retry_cleanup_leave_phase(VoiceJoinRetryCleanup::StopAfterSessionRemoved),
            Some("record-start retry cleanup after stop")
        );
        assert_eq!(
            voice_join_retry_cleanup_leave_phase(VoiceJoinRetryCleanup::StopAfterSessionReplaced),
            None,
            "successor session owns the guild voice state, so stale cleanup must not leave"
        );
    }

    #[test]
    fn recording_startup_reservation_blocks_concurrent_start() {
        let startups = HashMap::from([("g1".to_owned(), "m1".to_owned())]);

        assert_eq!(
            recording_startup_conflict(&startups, "g1"),
            Some(CommandError::ActiveMeetingExists {
                meeting_id: "m1".to_owned()
            })
        );
        assert_eq!(recording_startup_conflict(&startups, "g2"), None);
    }

    #[test]
    fn recording_startup_clear_preserves_newer_reservation() {
        let mut startups = HashMap::from([("g1".to_owned(), "newer".to_owned())]);

        clear_matching_recording_startup(&mut startups, "g1", "older");
        assert_eq!(startups.get("g1").map(String::as_str), Some("newer"));

        clear_matching_recording_startup(&mut startups, "g1", "newer");
        assert!(!startups.contains_key("g1"));
    }

    #[test]
    fn recording_start_join_completed_after_stop_accepts_downstream_terminal_statuses() {
        assert!(recording_start_join_completed_after_stop(
            crate::domain::MeetingStatus::Stopping
        ));
        assert!(recording_start_join_completed_after_stop(
            crate::domain::MeetingStatus::Transcribing
        ));
        assert!(recording_start_join_completed_after_stop(
            crate::domain::MeetingStatus::Summarizing
        ));
        assert!(recording_start_join_completed_after_stop(
            crate::domain::MeetingStatus::Posted
        ));
        assert!(recording_start_join_completed_after_stop(
            crate::domain::MeetingStatus::Failed
        ));

        assert!(!recording_start_join_completed_after_stop(
            crate::domain::MeetingStatus::Recording
        ));
        assert!(!recording_start_join_completed_after_stop(
            crate::domain::MeetingStatus::Aborted
        ));
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
            effective_settings: None,
        }
    }

    #[test]
    fn runtime_setup_completion_creates_recording_row() {
        let store = crate::infrastructure::storage::InMemoryMeetingStore::new();
        let mut service = BotCommandService::new(store);
        let input = start_input_for_runtime_setup_test();
        let preflight = validate_record_start_preconditions(
            &mut service.store,
            &RecordStartRequest {
                meeting_id: input.meeting_id.clone(),
                guild_id: input.guild_id.clone(),
                started_by_user_id: input.user_id.clone(),
                command_channel_id: input.command_channel_id.clone(),
                user_voice_channel_id: input.user_voice_channel_id.clone(),
                permissions: input.permissions,
                caller_role: input.caller_role,
                effective_settings: input.effective_settings.clone(),
            },
        )
        .expect("preflight should succeed");

        let result = complete_record_start_after_runtime_setup(&mut service, input, preflight)
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
            None,
        );
        assert!(message.contains("https://example.test/meetings/meeting-1"));
        assert!(message.contains("✅"));
    }

    #[test]
    fn live_status_messages_include_meeting_url() {
        let updates = [
            StatusMessageUpdate::RecordingStarted {
                voice_channel_id: 10,
                report_channel_id: 20,
            },
            StatusMessageUpdate::RecordingStopped,
            StatusMessageUpdate::SummaryStarted,
            StatusMessageUpdate::Failed {
                phase: "transcription",
                error: "failed",
            },
        ];

        for update in updates {
            let message = format_status_message_content(
                "meeting-1",
                &update,
                Some("https://example.test/meetings/meeting-1"),
            );
            assert!(message.contains("https://example.test/meetings/meeting-1"));
            assert!(message.contains("meeting_id=meeting-1"));
        }
    }
}
