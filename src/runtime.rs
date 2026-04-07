use crate::auto_stop::{AutoStopSignal, AutoStopState};
use crate::bot::{BotCommandService, StartCommandInput, StopCommandInput};
use crate::command::{CommandError, PermissionSet};
use crate::config::AppConfig;
use crate::domain::{MeetingStatus, StopReason};
use crate::integrations::{ClaudeCliSummaryClient, CommandWhisperClient};
use crate::posting::{DISCORD_MESSAGE_LIMIT, split_discord_message};
use crate::queue::JobQueue;
use crate::receiver::ReceiverConfig;
use crate::recording_session::RecordingSession;
use crate::recovery::RecoveryCandidate;
use crate::recovery_runner::{RecoveryEffect, run_recovery};
use crate::retry::RetryPolicy;
use crate::songbird_adapter::{AdaptedVoiceFrames, SsrcTracker, adapt_voice_tick};
use crate::sql::{INCREMENTAL_MIGRATIONS_SQL, INITIAL_SCHEMA_SQL, RECOVERY_SCAN_SQL};
use crate::sql_store::{PgSqlExecutor, SqlExecutor, SqlJobQueue, SqlMeetingStore};
use crate::stop::StopOutcome;
use crate::storage::{MeetingStore, StoredMeeting};
use crate::storage_fs::LocalChunkStorage;
use crate::summary::ClaudeSummaryClient;
use crate::worker::enqueue_summary_job;
use serenity::all::{
    ChannelId, CommandDataOptionValue, CommandInteraction, CreateCommand,
    CreateInteractionResponse, CreateInteractionResponseMessage, EditInteractionResponse,
    GatewayIntents, GuildId, Interaction, Ready, UserId, VoiceState,
};
use serenity::async_trait;
use serenity::http::Http;
use serenity::prelude::{Client, Context, EventHandler};
use songbird::driver::{DecodeConfig, DecodeMode};
use songbird::{
    Config as SongbirdConfig, CoreEvent, Event, EventContext, EventHandler as SongbirdEventHandler,
    SerenityInit,
};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::time::sleep;
use tracing::{error, info, warn};

pub const RECORD_START_COMMAND: &str = "record-start";
pub const RECORD_STOP_COMMAND: &str = "record-stop";

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeCommandInput {
    RecordStart(StartCommandInput),
    RecordStop {
        guild_id: String,
        reason: StopReason,
    },
}

pub fn dispatch_runtime_command<S: MeetingStore>(
    service: &mut BotCommandService<S>,
    input: RuntimeCommandInput,
) -> Result<String, CommandError> {
    match input {
        RuntimeCommandInput::RecordStart(value) => service.handle_record_start(value),
        RuntimeCommandInput::RecordStop { guild_id, reason } => {
            service.handle_record_stop(StopCommandInput { guild_id, reason })
        }
    }
}

pub fn stop_and_enqueue_summary_job<S, Q>(
    service: &mut BotCommandService<S>,
    queue: &mut Q,
    guild_id: &str,
    reason: StopReason,
) -> Result<crate::bot::StopCommandResult, String>
where
    S: MeetingStore,
    Q: crate::queue::JobQueue,
{
    let stop_result = service
        .handle_record_stop_result(StopCommandInput {
            guild_id: guild_id.to_owned(),
            reason,
        })
        .map_err(|err| err.to_string())?;

    if stop_result.outcome == StopOutcome::Owner {
        let job_id = format!("summary-{}", stop_result.meeting_id);
        enqueue_summary_job(queue, &job_id, &stop_result.meeting_id)
            .map_err(|err| err.to_string())?;
        info!(
            meeting_id = %stop_result.meeting_id,
            job_id = %job_id,
            "summary job enqueued after stop"
        );
    }

    Ok(stop_result)
}

pub fn meeting_audio_dir(
    base_dir: &str,
    guild_id: &str,
    voice_channel_id: &str,
    meeting_id: &str,
) -> PathBuf {
    crate::workspace::MeetingWorkspaceLayout::new(base_dir)
        .for_meeting(guild_id, voice_channel_id, meeting_id)
        .audio_dir()
}

pub fn meeting_audio_path(
    base_dir: &str,
    guild_id: &str,
    voice_channel_id: &str,
    meeting_id: &str,
) -> String {
    crate::workspace::MeetingWorkspaceLayout::new(base_dir)
        .for_meeting(guild_id, voice_channel_id, meeting_id)
        .mixdown_path()
        .to_string_lossy()
        .to_string()
}

type UserPcmChunk = (String, Vec<u8>);
type SequenceGroup = (u64, Vec<UserPcmChunk>);

pub fn merge_user_chunks_to_mixdown(audio_dir: &std::path::Path) -> Result<String, String> {
    use crate::audio::build_wav_bytes_raw;

    let mixdown_path = audio_dir.join("mixdown.wav");

    let mut chunk_files: Vec<PathBuf> = Vec::new();
    let entries =
        fs::read_dir(audio_dir).map_err(|err| format!("failed to read audio dir: {err}"))?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("failed to read dir entry: {err}"))?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("wav")
            && path.file_name() != Some(std::ffi::OsStr::new("mixdown.wav"))
        {
            chunk_files.push(path);
        }
    }

    if chunk_files.is_empty() {
        return Err("no audio chunks found for meeting".to_owned());
    }

    // Sort by (sequence, user_id) to interleave users within each time window.
    // Filenames follow the pattern {user_id}_{sequence}.wav
    chunk_files.sort_by(|a, b| {
        let parse = |p: &PathBuf| -> (u64, String) {
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if let Some(pos) = stem.rfind('_') {
                let seq = stem[pos + 1..].parse::<u64>().unwrap_or(0);
                let user = stem[..pos].to_owned();
                (seq, user)
            } else {
                (0, stem.to_owned())
            }
        };
        let (seq_a, user_a) = parse(a);
        let (seq_b, user_b) = parse(b);
        seq_a.cmp(&seq_b).then(user_a.cmp(&user_b))
    });

    // Read PCM from each chunk and mix same-sequence chunks by summing samples
    let mut sequence_groups: Vec<SequenceGroup> = Vec::new();
    for chunk_path in &chunk_files {
        let stem = chunk_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let seq = stem
            .rfind('_')
            .and_then(|pos| stem[pos + 1..].parse::<u64>().ok())
            .unwrap_or(0);

        let data = fs::read(chunk_path)
            .map_err(|err| format!("failed to read chunk {}: {err}", chunk_path.display()))?;
        // Validate WAV header before stripping: must start with RIFF and have
        // a "data" subchunk at offset 36 for standard PCM WAV (44-byte header).
        // This is coupled to `build_wav_bytes` which always writes a minimal
        // 44-byte header. If WAV generation changes, update this check.
        let pcm = if data.len() >= 44 && data[0..4] == *b"RIFF" && data[36..40] == *b"data" {
            data[44..].to_vec()
        } else {
            warn!(
                path = %chunk_path.display(),
                "skipping chunk: invalid WAV header or too small"
            );
            continue;
        };

        match sequence_groups.last_mut() {
            Some((last_seq, group)) if *last_seq == seq => {
                group.push((stem.to_owned(), pcm));
            }
            _ => {
                sequence_groups.push((seq, vec![(stem.to_owned(), pcm)]));
            }
        }
    }

    // Mix each sequence group: sum i16 samples with clipping
    let mut all_pcm = Vec::new();
    for (_seq, group) in &sequence_groups {
        if group.len() == 1 {
            all_pcm.extend_from_slice(&group[0].1);
            continue;
        }
        // Find max length, then sum samples
        let max_len = group.iter().map(|(_, pcm)| pcm.len()).max().unwrap_or(0);
        let sample_count = max_len / 2;
        let mut mixed = vec![0i32; sample_count];
        for (_, pcm) in group {
            for i in 0..pcm.len() / 2 {
                let sample = i16::from_le_bytes([pcm[i * 2], pcm[i * 2 + 1]]) as i32;
                mixed[i] += sample;
            }
        }
        for sample in &mixed {
            let clamped = (*sample).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            all_pcm.extend_from_slice(&clamped.to_le_bytes());
        }
    }

    let wav_bytes = build_wav_bytes_raw(&all_pcm, 48_000, 1, 16)
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

fn parse_meeting_status(value: &str) -> MeetingStatus {
    MeetingStatus::parse_str(value).unwrap_or(MeetingStatus::Aborted)
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
        whisper_endpoint: config.whisper_endpoint.clone(),
        claude_command: config.claude_command.clone(),
        whisper_language: config.whisper_language.clone(),
        summary_max_retries: config.summary_max_retries,
        integration_retry_policy: RetryPolicy {
            max_attempts: config.integration_retry_max_attempts,
            initial_delay: std::time::Duration::from_millis(
                config.integration_retry_initial_delay_ms,
            ),
            backoff_multiplier: config.integration_retry_backoff_multiplier,
            max_delay: std::time::Duration::from_millis(config.integration_retry_max_delay_ms),
        },
        public_base_url: config.public_base_url.clone(),
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
    whisper_endpoint: String,
    claude_command: String,
    whisper_language: Option<String>,
    summary_max_retries: u32,
    integration_retry_policy: RetryPolicy,
    public_base_url: Option<String>,
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

            let message = self.handle_command(&ctx, &command).await;

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
        let non_bot =
            count_non_bot_members_in_target_voice(&ctx, self.guild_id, target_voice_channel_id)
                .unwrap_or(0);
        let signal = {
            let mut states = self.auto_stop_states.lock().await;
            let state = states
                .entry(guild_key.clone())
                .or_insert_with(|| AutoStopState::new(Duration::from_secs(15)));
            state.on_non_bot_member_count_changed(non_bot, now_ms())
        };

        if signal == AutoStopSignal::StartTimer {
            // timer_active was already set atomically inside
            // on_non_bot_member_count_changed — no separate reservation needed.
            let handler = self.clone();
            let ctx_for_task = ctx.clone();
            let guild_for_task = guild_key;
            let expected_meeting_id = self.active_meeting_id().await;
            tokio::spawn(async move {
                sleep(Duration::from_secs(15)).await;
                // Verify the same meeting is still active (not a new recording)
                let current_meeting_id = handler.active_meeting_id().await;
                if current_meeting_id != expected_meeting_id || expected_meeting_id.is_none() {
                    // Clear timer flag before returning.
                    let mut states = handler.auto_stop_states.lock().await;
                    if let Some(state) = states.get_mut(&guild_for_task) {
                        state.clear_timer_active();
                    }
                    return;
                }
                let trigger = {
                    let mut states = handler.auto_stop_states.lock().await;
                    let Some(state) = states.get_mut(&guild_for_task) else {
                        return;
                    };
                    let result = state.tick(now_ms()) == AutoStopSignal::Trigger;
                    if !result {
                        state.clear_timer_active();
                    }
                    result
                };
                if !trigger {
                    return;
                }
                // Flush remaining audio before stopping
                {
                    let mut sessions = handler.sessions.lock().await;
                    if let Some(session) = sessions.get_mut(&guild_for_task) {
                        match session.flush_all() {
                            Ok(result) if !result.failed.is_empty() => {
                                warn!(guild_id = %guild_for_task, failed = result.failed.len(), "some chunks failed to persist on auto-stop");
                            }
                            Err(err) => {
                                warn!(guild_id = %guild_for_task, error = %err, "failed to flush remaining audio on auto-stop");
                            }
                            _ => {}
                        }
                    }
                    sessions.remove(&guild_for_task);
                }
                {
                    let mut states = handler.auto_stop_states.lock().await;
                    states.remove(&guild_for_task);
                }
                if let Some(manager) = songbird::get(&ctx_for_task).await {
                    let _ = manager.leave(handler.guild_id).await;
                }
                let stop_result = {
                    let mut service = handler.service.lock().await;
                    let mut queue = handler.queue.lock().await;
                    stop_and_enqueue_summary_job(
                        &mut service,
                        &mut *queue,
                        &guild_for_task,
                        StopReason::AutoEmpty,
                    )
                };
                match stop_result {
                    Ok(result) => {
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
            });
        }
    }
}

impl ScaffoldHandler {
    async fn run_startup_recovery(&self, ctx: &Context) -> Result<(), String> {
        let snapshots: Vec<RecoverySnapshot> = {
            let mut service = self.service.lock().await;
            let rows = service.store.executor.query_rows(RECOVERY_SCAN_SQL, &[])?;
            rows.into_iter()
                .filter_map(|row| {
                    if row.len() < 3 {
                        return None;
                    }
                    Some(RecoverySnapshot {
                        meeting_id: row[0].clone(),
                        status: parse_meeting_status(&row[1]),
                        voice_channel_id: parse_u64_with_warning(
                            &row[0],
                            "voice_channel_id",
                            &row[2],
                        ),
                    })
                })
                .collect()
        };

        for snapshot in snapshots {
            let meeting = self.load_meeting(&snapshot.meeting_id).await.ok();
            let workspace = meeting.as_ref().map(|m| self.workspace_for_meeting(m));
            let audio_dir = workspace
                .as_ref()
                .map(|w| w.audio_dir())
                .filter(|dir| dir.is_dir())
                .unwrap_or_else(|| {
                    crate::workspace::MeetingWorkspaceLayout::new(&self.chunk_storage_dir)
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
                    // Merge per-user chunks into mixdown before ASR
                    if let Err(err) = merge_user_chunks_to_mixdown(&audio_dir) {
                        warn!(meeting_id = %snapshot.meeting_id, error = %err, "failed to merge audio chunks during recovery");
                        let _ = self.post_failure_for_meeting(
                            &ctx.http,
                            &snapshot.meeting_id,
                            &format!("音声チャンクのマージに失敗しました (recovery): {err}"),
                        ).await;
                        let mut service = self.service.lock().await;
                        let _ = service.store.set_meeting_status(&snapshot.meeting_id, MeetingStatus::Failed, None);
                        let _ = service.store.set_error_message(&snapshot.meeting_id, Some(format!("merge failed during recovery: {err}")));
                        continue;
                    }
                    let job_id = format!("summary-{}", snapshot.meeting_id);
                    let job_available = {
                        let mut queue = self.queue.lock().await;
                        // Reset any previously failed job to queued so it can be re-claimed.
                        // If the reset itself fails we cannot know whether a claimable job
                        // exists, so abort recovery for this meeting.
                        if let Err(err) = queue.executor.execute(
                            "UPDATE jobs SET status='queued', error_message=NULL, updated_at=NOW() WHERE id=$1 AND status IN ('failed', 'running')",
                            std::slice::from_ref(&job_id),
                        ) {
                            warn!(meeting_id = %snapshot.meeting_id, error = %err, "failed to reset previously failed summary job during recovery");
                            false
                        } else {
                            match enqueue_summary_job(&mut *queue, &job_id, &snapshot.meeting_id) {
                                // Job freshly inserted — claimable.
                                Ok(()) => true,
                                // Job was already in the queue — also claimable.
                                Err(crate::worker::WorkerError::AlreadyExists) => true,
                                // Genuine failure — no job to claim.
                                Err(err) => {
                                    warn!(meeting_id = %snapshot.meeting_id, error = %err, "failed to enqueue summary job during recovery");
                                    false
                                }
                            }
                        }
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

    async fn handle_command(&self, ctx: &Context, command: &CommandInteraction) -> String {
        let result = match command.data.name.as_str() {
            RECORD_START_COMMAND => self.handle_record_start(ctx, command).await,
            RECORD_STOP_COMMAND => self.handle_record_stop(ctx, command).await,
            _ => Err("unsupported command".to_owned()),
        };

        match result {
            Ok(message) => message,
            Err(err) => format!("error: {err}"),
        }
    }

    async fn handle_record_start(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
    ) -> Result<String, String> {
        let guild_id = command
            .guild_id
            .ok_or_else(|| "guild_id is required for this command".to_owned())?;
        let voice_channel_id_u64 = resolve_user_voice_channel_id(ctx, guild_id, command.user.id);

        let meeting_id = format!("{}-{}", guild_id.get(), command.id.get());
        let permissions = resolve_bot_permissions(
            ctx,
            guild_id,
            voice_channel_id_u64,
            Some(command.channel_id.get()),
        );
        let mut service = self.service.lock().await;
        let response = service
            .handle_record_start(StartCommandInput {
                meeting_id: meeting_id.clone(),
                guild_id: guild_id.get().to_string(),
                user_id: command.user.id.get().to_string(),
                command_channel_id: command.channel_id.get().to_string(),
                user_voice_channel_id: voice_channel_id_u64.map(|v| v.to_string()),
                permissions,
            })
            .map_err(|err| err.to_string())?;
        drop(service);

        let voice_channel_id_u64 = voice_channel_id_u64
            .ok_or_else(|| "voice_channel_id unexpectedly None after record_start".to_owned())?;

        let manager = songbird::get(ctx)
            .await
            .ok_or_else(|| "songbird not initialized".to_owned())?;
        let layout = crate::workspace::MeetingWorkspaceLayout::new(&self.chunk_storage_dir);
        let workspace = layout.for_meeting(
            &guild_id.get().to_string(),
            &voice_channel_id_u64.to_string(),
            &meeting_id,
        );
        workspace
            .ensure_base_dirs()
            .map_err(|err| format!("failed to prepare workspace: {err}"))?;
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
                bot_user_id: ctx.cache.current_user().id.get(),
            };
            call.add_global_event(
                Event::Core(CoreEvent::SpeakingStateUpdate),
                voice_handler.clone(),
            );
            call.add_global_event(Event::Core(CoreEvent::VoiceTick), voice_handler.clone());
            call.add_global_event(Event::Core(CoreEvent::ClientDisconnect), voice_handler);
        }

        info!(
            guild_id = %guild_id.get(),
            meeting_id = %meeting_id,
            "recording started"
        );

        Ok(response)
    }

    async fn handle_record_stop(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
    ) -> Result<String, String> {
        let guild_id = command
            .guild_id
            .ok_or_else(|| "guild_id is required for this command".to_owned())?;
        let guild_key = guild_id.get().to_string();

        // Flush remaining audio before stopping
        {
            let mut sessions = self.sessions.lock().await;
            if let Some(session) = sessions.get_mut(&guild_key) {
                match session.flush_all() {
                    Ok(result) if !result.failed.is_empty() => {
                        warn!(guild_id = %guild_key, failed = result.failed.len(), "some chunks failed to persist on stop");
                    }
                    Err(err) => {
                        warn!(guild_id = %guild_key, error = %err, "failed to flush remaining audio on stop");
                    }
                    _ => {}
                }
            }
            sessions.remove(&guild_key);
        }
        {
            let mut states = self.auto_stop_states.lock().await;
            states.remove(&guild_key);
        }

        if let Some(manager) = songbird::get(ctx).await {
            let _ = manager.leave(guild_id).await;
        }

        let stop_result = {
            let mut service = self.service.lock().await;
            let mut queue = self.queue.lock().await;
            stop_and_enqueue_summary_job(&mut service, &mut *queue, &guild_key, StopReason::Manual)
        };

        match stop_result {
            Ok(result) => {
                let meeting_id = result.meeting_id.clone();
                let outcome = result.outcome;

                if outcome == StopOutcome::Owner {
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
            Err(err) => Err(err),
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
        match self.process_enqueued_summary_job(meeting_id).await {
            Ok(output) => {
                let chunks = if output.chunks.iter().all(|c| c.trim().is_empty()) {
                    vec!["会議が終了しました。要約内容がありません。".to_owned()]
                } else {
                    output.chunks
                };
                if let Err(err) =
                    post_summary_to_report_channel(http, report_channel_id, &chunks).await
                {
                    {
                        let mut service = self.service.lock().await;
                        let _ = service.store.set_meeting_status(
                            meeting_id,
                            MeetingStatus::Failed,
                            None,
                        );
                        let _ = service.store.set_error_message(
                            meeting_id,
                            Some(format!("summary posting failed: {err}")),
                        );
                    }
                    let _ =
                        post_failure_to_report_channel(http, report_channel_id, meeting_id, &err)
                            .await;
                    return Err(err);
                }
                // Post meeting URL if PUBLIC_BASE_URL is configured
                if let Some(ref base_url) = self.public_base_url {
                    let url = format!("{}/meetings/{}", base_url.trim_end_matches('/'), meeting_id);
                    let url_msg = format!("詳細はこちら: {url}");
                    if let Err(err) =
                        post_summary_to_report_channel(http, report_channel_id, &[url_msg]).await
                    {
                        warn!(meeting_id = %meeting_id, error = %err, "failed to post meeting URL");
                    }
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
            Err(err) => {
                // process_enqueued_summary_job already handles Failed/retry status.
                // Only post failure notification here.
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
        post_failure_to_report_channel(http, report_channel_id, meeting_id, error_message).await
    }

    async fn report_channel_id_for_meeting(&self, meeting_id: &str) -> Result<u64, String> {
        let mut service = self.service.lock().await;
        let rows = service.store.executor.query_rows(
            "SELECT report_channel_id FROM meetings WHERE id=$1 LIMIT 1",
            &[meeting_id.to_owned()],
        )?;
        let Some(row) = rows.into_iter().next() else {
            return Err(format!(
                "meeting not found while loading report channel: meeting_id={meeting_id}"
            ));
        };
        let Some(value) = row.first() else {
            return Err(format!(
                "report channel row missing value: meeting_id={meeting_id}"
            ));
        };
        value.parse::<u64>().map_err(|err| {
            format!(
                "invalid report channel id: meeting_id={meeting_id}, value={value}, error={err}"
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
    ) -> crate::workspace::MeetingWorkspacePaths {
        crate::workspace::MeetingWorkspaceLayout::new(&self.chunk_storage_dir).for_meeting(
            &meeting.guild_id,
            &meeting.voice_channel_id,
            &meeting.id,
        )
    }

    fn legacy_mixdown_path(&self, meeting_id: &str) -> PathBuf {
        crate::workspace::MeetingWorkspaceLayout::new(&self.chunk_storage_dir)
            .legacy_meeting_dir(meeting_id)
            .join("mixdown.wav")
    }

    async fn process_enqueued_summary_job(
        &self,
        meeting_id: &str,
    ) -> Result<crate::worker::ProcessMeetingOutput, String> {
        let whisper = CommandWhisperClient {
            endpoint: self.whisper_endpoint.clone(),
            curl_bin: "curl".to_owned(),
            retry_policy: self.integration_retry_policy,
        };
        let claude = ClaudeCliSummaryClient {
            command_path: self.claude_command.clone(),
            retry_policy: self.integration_retry_policy,
        };
        let job_id = format!("summary-{meeting_id}");
        let meeting = self.load_meeting(meeting_id).await?;
        let workspace = self.workspace_for_meeting(&meeting);
        workspace
            .ensure_base_dirs()
            .map_err(|err| format!("failed to prepare workspace: {err}"))?;
        let (audio_path, using_legacy_audio) = {
            let primary = workspace.mixdown_path();
            if primary.exists() {
                (primary, false)
            } else {
                (self.legacy_mixdown_path(&meeting.id), true)
            }
        };
        if using_legacy_audio {
            warn!(
                meeting_id = %meeting.id,
                path = %audio_path.display(),
                "falling back to legacy mixdown path"
            );
        }

        let claimed_job = {
            let mut queue = self.queue.lock().await;
            queue.claim_by_id(&job_id).map_err(|err| err.to_string())?
        };
        let Some(claimed_job) = claimed_job else {
            return Err(format!("summary job was not available for job_id={job_id}"));
        };
        if claimed_job.meeting_id != meeting_id {
            warn!(
                expected_meeting_id = %meeting_id,
                processed_meeting_id = %claimed_job.meeting_id,
                job_id = %claimed_job.id,
                "processed summary job for different meeting"
            );
        }

        let request = crate::summary::SummaryRequest {
            meeting_id: claimed_job.meeting_id.clone(),
            guild_id: meeting.guild_id.clone(),
            voice_channel_id: meeting.voice_channel_id.clone(),
            title: meeting.title.clone(),
            audio_path: audio_path.to_string_lossy().to_string(),
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
            // CAS failed — another process may own this meeting.  Mark the
            // job failed so it does not stay Running forever.
            warn!(meeting_id = %claimed_job.meeting_id, error = %cas_err, "CAS Stopping→Transcribing failed; marking job failed");
            let mut queue = self.queue.lock().await;
            let _ = queue.mark_failed(&claimed_job.id, cas_err.to_string());
            return Err(cas_err.to_string());
        }

        let transcription =
            tokio::task::block_in_place(|| crate::summary::run_transcription(&whisper, &request));
        let transcription = match transcription {
            Ok(t) => t,
            Err(err) => {
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
                    let retry_status =
                        queue.retry(&claimed_job.id, err.to_string(), self.summary_max_retries);
                    drop(queue);
                    if retry_status.map_or(true, |s| s == crate::domain::JobStatus::Failed) {
                        let mut service = self.service.lock().await;
                        let _ = service.store.set_meeting_status(
                            &claimed_job.meeting_id,
                            MeetingStatus::Failed,
                            None,
                        );
                        let _ = service
                            .store
                            .set_error_message(&claimed_job.meeting_id, Some(err.to_string()));
                    }
                } else {
                    // Revert failed — another process may have progressed the
                    // state.  Mark the job failed so it does not stay Running.
                    warn!(
                        meeting_id = %claimed_job.meeting_id,
                        "CAS revert to Stopping failed; marking job failed"
                    );
                    let mut queue = self.queue.lock().await;
                    let _ = queue.mark_failed(&claimed_job.id, err.to_string());
                }
                return Err(err.to_string());
            }
        };

        // Persist transcript segments to DB (best-effort)
        if !transcription.segments.is_empty() {
            let sql = crate::sql::build_insert_transcripts_sql(transcription.segments.len());
            let mut params = Vec::with_capacity(transcription.segments.len() * 8);
            for (i, seg) in transcription.segments.iter().enumerate() {
                params.push(format!("{}-t-{i}", claimed_job.meeting_id));
                params.push(claimed_job.meeting_id.clone());
                params.push(seg.speaker_id.clone());
                params.push(seg.start_ms.to_string());
                params.push(seg.end_ms.to_string());
                params.push(seg.text.clone());
                params.push(seg.confidence.map(|c| c.to_string()).unwrap_or_default());
                params.push(seg.is_noisy.to_string());
            }
            let mut service = self.service.lock().await;
            if let Err(err) = service.store.executor.execute(&sql, &params) {
                warn!(
                    meeting_id = %claimed_job.meeting_id,
                    error = %err,
                    "failed to persist transcript segments"
                );
            }
        }

        // Phase 2: Summarization (mutex held only for status update)
        if let Err(cas_err) = {
            let mut service = self.service.lock().await;
            service.store.set_meeting_status(
                &claimed_job.meeting_id,
                MeetingStatus::Summarizing,
                Some(MeetingStatus::Transcribing),
            )
        } {
            warn!(meeting_id = %claimed_job.meeting_id, error = %cas_err, "CAS Transcribing→Summarizing failed; marking job failed");
            let mut queue = self.queue.lock().await;
            let _ = queue.mark_failed(&claimed_job.id, cas_err.to_string());
            return Err(cas_err.to_string());
        }

        let markdown = tokio::task::block_in_place(|| {
            let manifest = crate::summary::write_transcript_files(&request, &transcription)?;
            let prompt = crate::summary::build_summary_prompt(&request, &manifest);
            claude.summarize(&prompt, Some(request.workspace.root()))
        });
        let markdown = match markdown {
            Ok(m) => m,
            Err(err) => {
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
                    let retry_status =
                        queue.retry(&claimed_job.id, err.to_string(), self.summary_max_retries);
                    drop(queue);
                    if retry_status.map_or(true, |s| s == crate::domain::JobStatus::Failed) {
                        let mut service = self.service.lock().await;
                        let _ = service.store.set_meeting_status(
                            &claimed_job.meeting_id,
                            MeetingStatus::Failed,
                            None,
                        );
                        let _ = service
                            .store
                            .set_error_message(&claimed_job.meeting_id, Some(err.to_string()));
                    }
                } else {
                    warn!(
                        meeting_id = %claimed_job.meeting_id,
                        "CAS revert to Stopping failed; marking job failed"
                    );
                    let mut queue = self.queue.lock().await;
                    let _ = queue.mark_failed(&claimed_job.id, err.to_string());
                }
                return Err(err.to_string());
            }
        };

        // Persist summary markdown to DB (best-effort)
        {
            let summary_id = format!("{}-s-1", claimed_job.meeting_id);
            let mut service = self.service.lock().await;
            if let Err(err) = service.store.executor.execute(
                crate::sql::INSERT_SUMMARY_SQL,
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

        Ok(crate::worker::ProcessMeetingOutput {
            meeting_id: claimed_job.meeting_id,
            markdown,
            chunks,
        })
    }
}

#[derive(Clone)]
struct VoiceReceiveHandler {
    tracker: Arc<Mutex<SsrcTracker>>,
    sessions: Arc<Mutex<HashMap<String, RecordingSession<LocalChunkStorage>>>>,
    guild_id: String,
    runtime: ScaffoldHandler,
    http: Arc<Http>,
    bot_user_id: u64,
}

#[serenity::async_trait]
impl SongbirdEventHandler for VoiceReceiveHandler {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        match ctx {
            EventContext::SpeakingStateUpdate(evt) => {
                if let Some(user_id) = evt.user_id {
                    let mut tracker = self.tracker.lock().await;
                    let user_id_u64 = user_id.0;
                    tracker.update_mapping(evt.ssrc, user_id_u64);
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
            EventContext::ClientDisconnect(evt) => {
                let user_id_u64 = evt.user_id.0;
                if user_id_u64 != self.bot_user_id {
                    return None;
                }
                warn!(user_id = user_id_u64, "bot voice client disconnected");
                {
                    let runtime = self.runtime.clone();
                    let guild_key = self.guild_id.clone();
                    let http = Arc::clone(&self.http);
                    tokio::spawn(async move {
                        // Flush remaining audio and clean up session
                        {
                            let mut sessions = runtime.sessions.lock().await;
                            if let Some(session) = sessions.get_mut(&guild_key) {
                                match session.flush_all() {
                                    Ok(result) if !result.failed.is_empty() => {
                                        warn!(guild_id = %guild_key, failed = result.failed.len(), "some chunks failed to persist on client disconnect");
                                    }
                                    Err(err) => {
                                        warn!(guild_id = %guild_key, error = %err, "failed to flush audio on client disconnect");
                                    }
                                    _ => {}
                                }
                            }
                            sessions.remove(&guild_key);
                        }
                        {
                            let mut states = runtime.auto_stop_states.lock().await;
                            states.remove(&guild_key);
                        }
                        let stop_result = {
                            let mut service = runtime.service.lock().await;
                            let mut queue = runtime.queue.lock().await;
                            stop_and_enqueue_summary_job(
                                &mut service,
                                &mut *queue,
                                &guild_key,
                                StopReason::ClientDisconnect,
                            )
                        };
                        match stop_result {
                            Ok(result) => {
                                if result.outcome == StopOutcome::Owner
                                    && let Err(err) =
                                        run_summary_background(&runtime, &http, &result.meeting_id)
                                            .await
                                {
                                    warn!(
                                        guild_id = %guild_key,
                                        meeting_id = %result.meeting_id,
                                        error = %err,
                                        "failed to process summary after client disconnect"
                                    );
                                }
                            }
                            Err(err) => {
                                warn!(
                                    guild_id = %guild_key,
                                    error = %err,
                                    "failed to stop recording on client disconnect"
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
            if !result.failed.is_empty() {
                tracing::warn!(
                    failed_count = result.failed.len(),
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
        warn!(guild_id = %guild_id, "guild not found in cache, assuming permissive permissions");
        return PermissionSet {
            can_connect_voice: true,
            can_send_messages: true,
        };
    };
    let bot_id = ctx.cache.current_user().id;
    let Some(member) = guild.members.get(&bot_id) else {
        warn!(guild_id = %guild_id, bot_id = %bot_id, "bot member not found in cache, assuming permissive permissions");
        return PermissionSet {
            can_connect_voice: true,
            can_send_messages: true,
        };
    };

    let can_connect_voice = voice_channel_id
        .and_then(|vc_id| {
            let channel = guild.channels.get(&ChannelId::new(vc_id))?;
            let perms = guild.user_permissions_in(channel, member);
            Some(perms.contains(Permissions::CONNECT))
        })
        .unwrap_or(true);

    let can_send_messages = text_channel_id
        .and_then(|tc_id| {
            let channel = guild.channels.get(&ChannelId::new(tc_id))?;
            let perms = guild.user_permissions_in(channel, member);
            Some(perms.contains(Permissions::SEND_MESSAGES))
        })
        .unwrap_or(true);

    PermissionSet {
        can_connect_voice,
        can_send_messages,
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

/// Runs merge + summary + notify in a background context.
/// All errors are handled internally (failure notification + status update).
async fn run_summary_background(
    handler: &ScaffoldHandler,
    http: &Http,
    meeting_id: &str,
) -> Result<(), String> {
    let meeting = handler.load_meeting(meeting_id).await.ok();
    let workspace = meeting.as_ref().map(|m| handler.workspace_for_meeting(m));
    let primary_audio_dir = workspace.as_ref().map(|w| w.audio_dir());
    let audio_dir = primary_audio_dir
        .as_ref()
        .filter(|dir| dir.exists())
        .cloned()
        .unwrap_or_else(|| {
            crate::workspace::MeetingWorkspaceLayout::new(&handler.chunk_storage_dir)
                .legacy_meeting_dir(meeting_id)
        });
    if let Some(primary) = primary_audio_dir
        && !primary.exists()
    {
        warn!(
            meeting_id = %meeting_id,
            primary = %primary.display(),
            fallback = %audio_dir.display(),
            "workspace audio dir missing; falling back to legacy mixdown path"
        );
    }
    if workspace.is_none() || !audio_dir.starts_with(&handler.chunk_storage_dir) {
        warn!(
            meeting_id = %meeting_id,
            path = %audio_dir.display(),
            "using legacy audio directory while merging chunks"
        );
    }

    if let Err(err) = merge_user_chunks_to_mixdown(&audio_dir) {
        warn!(meeting_id = %meeting_id, error = %err, "failed to merge audio chunks");
        let _ = handler
            .post_failure_for_meeting(
                http,
                meeting_id,
                &format!("音声チャンクのマージに失敗しました: {err}"),
            )
            .await;
        let mut service = handler.service.lock().await;
        let _ = service
            .store
            .set_meeting_status(meeting_id, MeetingStatus::Failed, None);
        let _ = service
            .store
            .set_error_message(meeting_id, Some(format!("merge failed: {err}")));
        return Err(err);
    }

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
