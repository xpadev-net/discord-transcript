use crate::auto_stop::{AutoStopSignal, AutoStopState};
use crate::bot::{BotCommandService, StartCommandInput, StopCommandInput};
use crate::command::{CommandError, PermissionSet};
use crate::config::AppConfig;
use crate::domain::{MeetingStatus, StopReason};
use crate::integrations::{ClaudeCliSummaryClient, CommandWhisperClient};
use crate::summary::ClaudeSummaryClient;
use crate::posting::{DISCORD_MESSAGE_LIMIT, split_discord_message};
use crate::queue::JobQueue;
use crate::receiver::ReceiverConfig;
use crate::recording_session::RecordingSession;
use crate::recovery::RecoveryCandidate;
use crate::recovery_runner::{RecoveryEffect, run_recovery};
use crate::retry::RetryPolicy;
use crate::songbird_adapter::{AdaptedVoiceFrames, SsrcTracker, adapt_voice_tick};
use crate::sql::{INITIAL_SCHEMA_SQL, RECOVERY_SCAN_SQL};
use crate::sql_store::{PgSqlExecutor, SqlExecutor, SqlJobQueue, SqlMeetingStore};
use crate::stop::StopOutcome;
use crate::storage::MeetingStore;
use crate::storage_fs::LocalChunkStorage;
use crate::worker::enqueue_summary_job;
use serenity::all::{
    ChannelId, CommandDataOptionValue, CommandInteraction, CommandOptionType, CreateCommand,
    CreateCommandOption, CreateInteractionResponse, CreateInteractionResponseMessage,
    GatewayIntents, GuildId, Interaction, Ready, UserId, VoiceState,
};
use serenity::async_trait;
use serenity::http::Http;
use serenity::prelude::{Client, Context, EventHandler};
use songbird::{
    CoreEvent, Event, EventContext, EventHandler as SongbirdEventHandler, SerenityInit,
};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
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
            RECORD_STOP_COMMAND => CreateCommand::new(spec.name)
                .description(spec.description)
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::String,
                        "reason",
                        "manual | auto_empty | client_disconnect | error",
                    )
                    .add_string_choice("manual", "manual")
                    .add_string_choice("auto_empty", "auto_empty")
                    .add_string_choice("client_disconnect", "client_disconnect")
                    .add_string_choice("error", "error"),
                ),
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

pub fn meeting_audio_path(base_dir: &str, meeting_id: &str) -> String {
    PathBuf::from(base_dir)
        .join(meeting_id)
        .join("mixdown.wav")
        .to_string_lossy()
        .to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecoverySnapshot {
    meeting_id: String,
    status: MeetingStatus,
    voice_channel_id: Option<u64>,
}

fn parse_meeting_status(value: &str) -> MeetingStatus {
    MeetingStatus::from_str(value).unwrap_or(MeetingStatus::Aborted)
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
        summary_max_retries: config.summary_max_retries,
        integration_retry_policy: RetryPolicy {
            max_attempts: config.integration_retry_max_attempts,
            initial_delay: std::time::Duration::from_millis(
                config.integration_retry_initial_delay_ms,
            ),
            backoff_multiplier: config.integration_retry_backoff_multiplier,
            max_delay: std::time::Duration::from_millis(config.integration_retry_max_delay_ms),
        },
    };

    let intents = GatewayIntents::GUILDS | GatewayIntents::GUILD_VOICE_STATES;
    let mut client = Client::builder(&config.discord_token, intents)
        .event_handler(handler)
        .register_songbird()
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
    summary_max_retries: u32,
    integration_retry_policy: RetryPolicy,
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

        if let Err(err) = self.run_startup_recovery(&ctx).await {
            error!(error = %err, "startup recovery failed");
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::Command(command) = interaction {
            let message = self.handle_command(&ctx, &command).await;
            if let Err(err) = command
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content(message)
                            .ephemeral(true),
                    ),
                )
                .await
            {
                error!(error = %err, "failed to send interaction response");
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

        if signal == AutoStopSignal::Pending && non_bot == 0 {
            let handler = self.clone();
            let ctx_for_task = ctx.clone();
            let guild_for_task = guild_key;
            let expected_meeting_id = self.active_meeting_id().await;
            tokio::spawn(async move {
                sleep(Duration::from_secs(15)).await;
                // Verify the same meeting is still active (not a new recording)
                let current_meeting_id = handler.active_meeting_id().await;
                if current_meeting_id != expected_meeting_id || expected_meeting_id.is_none() {
                    return;
                }
                let trigger = {
                    let mut states = handler.auto_stop_states.lock().await;
                    let Some(state) = states.get_mut(&guild_for_task) else {
                        return;
                    };
                    state.tick(now_ms()) == AutoStopSignal::Trigger
                };
                if !trigger {
                    return;
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
                if let Some(manager) = songbird::get(&ctx_for_task).await {
                    let _ = manager.leave(handler.guild_id).await;
                }
                {
                    let mut sessions = handler.sessions.lock().await;
                    sessions.remove(&guild_for_task);
                }
                {
                    let mut states = handler.auto_stop_states.lock().await;
                    states.remove(&guild_for_task);
                }
                match stop_result {
                    Ok(result) => {
                        info!(
                            guild_id = %guild_for_task,
                            meeting_id = %result.meeting_id,
                            "auto stop triggered due to empty voice channel"
                        );
                        if result.outcome == StopOutcome::Owner {
                            if let Err(err) = handler
                                .run_summary_and_notify(&ctx_for_task.http, &result.meeting_id)
                                .await
                            {
                                warn!(
                                    guild_id = %guild_for_task,
                                    meeting_id = %result.meeting_id,
                                    error = %err,
                                    "failed to process summary after auto stop"
                                );
                                if let Err(post_err) = handler
                                    .post_failure_for_meeting(
                                        &ctx_for_task.http,
                                        &result.meeting_id,
                                        &format!("自動停止後の要約処理に失敗しました: {err}"),
                                    )
                                    .await
                                {
                                    warn!(
                                        guild_id = %guild_for_task,
                                        meeting_id = %result.meeting_id,
                                        error = %post_err,
                                        "failed to notify auto-stop summary failure"
                                    );
                                }
                            }
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
            let rows = service
                .store
                .executor
                .query_rows(RECOVERY_SCAN_SQL, &[])?;
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
            let has_recording_file = fs::metadata(meeting_audio_path(
                &self.chunk_storage_dir,
                &snapshot.meeting_id,
            ))
            .is_ok();
            let voice_connected = snapshot
                .voice_channel_id
                .and_then(|voice_channel_id| {
                    is_bot_connected_to_voice_channel(ctx, self.guild_id, voice_channel_id)
                })
                .unwrap_or(false);
            let summary_job_already_queued = self
                .summary_job_already_queued(&snapshot.meeting_id)
                .await
                .unwrap_or(false);
            let candidate = RecoveryCandidate {
                meeting_id: snapshot.meeting_id.clone(),
                status: snapshot.status,
                voice_connected,
                has_recording_file,
                summary_job_already_queued,
            };

            let effect = {
                let mut service = self.service.lock().await;
                run_recovery(&mut service.store, &candidate).map_err(|err| err.to_string())?
            };

            match effect {
                RecoveryEffect::SummaryRequeued { .. }
                | RecoveryEffect::StopConfirmedClientDisconnect { .. } => {
                    let job_id = format!("summary-{}", snapshot.meeting_id);
                    {
                        let mut queue = self.queue.lock().await;
                        // Reset any previously failed job to queued so it can be re-claimed
                        let _ = queue.executor.execute(
                            "UPDATE jobs SET status='queued', error_message=NULL, updated_at=NOW() WHERE id=$1 AND status='failed'",
                            &[job_id.clone()],
                        );
                        let _ = enqueue_summary_job(&mut *queue, &job_id, &snapshot.meeting_id);
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
                        if let Err(post_err) = self
                            .post_failure_for_meeting(
                                &ctx.http,
                                &snapshot.meeting_id,
                                &format!("復旧後の要約処理に失敗しました: {err}"),
                            )
                            .await
                        {
                            warn!(
                                meeting_id = %snapshot.meeting_id,
                                error = %post_err,
                                "failed to post recovery summary failure notification"
                            );
                        }
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

    async fn summary_job_already_queued(&self, meeting_id: &str) -> Result<bool, String> {
        let mut queue = self.queue.lock().await;
        let rows = queue.executor.query_rows(
            "SELECT id FROM jobs WHERE meeting_id=$1 AND job_type='summarize' AND status IN ('queued','running') LIMIT 1",
            &[meeting_id.to_owned()],
        )?;
        Ok(!rows.is_empty())
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
        let voice_channel_id_u64 =
            resolve_user_voice_channel_id(ctx, guild_id, command.user.id);

        let meeting_id = format!("{}-{}", guild_id.get(), command.id.get());
        let mut service = self.service.lock().await;
        let response = service
            .handle_record_start(StartCommandInput {
                meeting_id: meeting_id.clone(),
                guild_id: guild_id.get().to_string(),
                user_id: command.user.id.get().to_string(),
                command_channel_id: command.channel_id.get().to_string(),
                user_voice_channel_id: voice_channel_id_u64.map(|v| v.to_string()),
                permissions: PermissionSet {
                    can_connect_voice: true,
                    can_send_messages: true,
                },
            })
            .map_err(|err| err.to_string())?;
        drop(service);

        let voice_channel_id_u64 =
            voice_channel_id_u64.ok_or_else(|| "user is not in a voice channel".to_owned())?;

        let manager = songbird::get(ctx)
            .await
            .ok_or_else(|| "songbird not initialized".to_owned())?;
        let call_lock = match manager
            .join(guild_id, ChannelId::new(voice_channel_id_u64))
            .await
        {
            Ok(call) => call,
            Err(err) => {
                let mut service = self.service.lock().await;
                let _ = service
                    .store
                    .set_meeting_status(&meeting_id, MeetingStatus::Failed);
                let _ = service.store.set_error_message(
                    &meeting_id,
                    Some(format!("failed to join voice channel: {err}")),
                );
                return Err(format!("failed to join voice channel: {err}"));
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

        let mut sessions = self.sessions.lock().await;
        sessions.insert(
            guild_id.get().to_string(),
            RecordingSession::new(
                meeting_id,
                LocalChunkStorage::new(&self.chunk_storage_dir),
                ReceiverConfig::default(),
                48_000,
            ),
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
        let reason = stop_reason_from_interaction(command)?;
        let guild_key = guild_id.get().to_string();
        let stop_result = {
            let mut service = self.service.lock().await;
            service
                .handle_record_stop_result(StopCommandInput {
                    guild_id: guild_key.clone(),
                    reason,
                })
                .map_err(|err| err.to_string())?
        };
        let meeting_id = stop_result.meeting_id.clone();
        let outcome = stop_result.outcome;
        let response = stop_result.message;

        {
            let mut sessions = self.sessions.lock().await;
            sessions.remove(&guild_key);
        }
        {
            let mut states = self.auto_stop_states.lock().await;
            states.remove(&guild_key);
        }

        if let Some(manager) = songbird::get(ctx).await {
            let _ = manager.leave(guild_id).await;
        }

        if outcome == StopOutcome::Owner {
            {
                let mut queue = self.queue.lock().await;
                let job_id = format!("summary-{meeting_id}");
                enqueue_summary_job(&mut *queue, &job_id, &meeting_id)
                    .map_err(|err| err.to_string())?;
                info!(meeting_id = %meeting_id, job_id = %job_id, "summary job enqueued");
            }
            if let Err(err) = self.run_summary_and_notify(&ctx.http, &meeting_id).await {
                warn!(
                    meeting_id = %meeting_id,
                    error = %err,
                    "failed to enqueue/process summary job"
                );
                if let Err(post_err) = self
                    .post_failure_for_meeting(
                        &ctx.http,
                        &meeting_id,
                        &format!("手動停止後の要約処理に失敗しました: {err}"),
                    )
                    .await
                {
                    warn!(
                        meeting_id = %meeting_id,
                        error = %post_err,
                        "failed to notify manual-stop summary failure"
                    );
                }
            }
        }

        info!(
            guild_id = %guild_key,
            meeting_id = %meeting_id,
            outcome = ?outcome,
            "recording stop handled"
        );
        Ok(response)
    }

    async fn run_summary_and_notify(&self, http: &Http, meeting_id: &str) -> Result<(), String> {
        let report_channel_id = match self.report_channel_id_for_meeting(meeting_id).await {
            Ok(value) => value,
            Err(err) => {
                let mut service = self.service.lock().await;
                let _ = service
                    .store
                    .set_meeting_status(meeting_id, MeetingStatus::Failed);
                let _ = service
                    .store
                    .set_error_message(meeting_id, Some(err.clone()));
                return Err(err);
            }
        };
        match self.process_enqueued_summary_job(meeting_id).await {
            Ok(output) => {
                if let Err(err) =
                    post_summary_to_report_channel(http, report_channel_id, &output.chunks).await
                {
                    {
                        let mut service = self.service.lock().await;
                        let _ = service
                            .store
                            .set_meeting_status(meeting_id, MeetingStatus::Failed);
                        let _ = service.store.set_error_message(
                            meeting_id,
                            Some(format!("summary posted failed: {err}")),
                        );
                    }
                    let _ =
                        post_failure_to_report_channel(http, report_channel_id, meeting_id, &err)
                            .await;
                    return Err(err);
                }
                let mut service = self.service.lock().await;
                service
                    .store
                    .set_meeting_status(meeting_id, MeetingStatus::Posted)
                    .map_err(|err| err.to_string())?;
                service
                    .store
                    .set_error_message(meeting_id, None)
                    .map_err(|err| err.to_string())?;
                Ok(())
            }
            Err(err) => {
                {
                    let mut service = self.service.lock().await;
                    let _ = service
                        .store
                        .set_meeting_status(meeting_id, MeetingStatus::Failed);
                    let _ = service
                        .store
                        .set_error_message(meeting_id, Some(err.clone()));
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
        let audio_path = meeting_audio_path(&self.chunk_storage_dir, meeting_id);

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
            title: None,
            audio_path,
            language: None,
        };

        // Phase 1: Transcription (mutex held only for status update)
        {
            let mut service = self.service.lock().await;
            service
                .store
                .set_meeting_status(&claimed_job.meeting_id, MeetingStatus::Transcribing)
                .map_err(|e| e.to_string())?;
        }

        let transcription = tokio::task::block_in_place(|| {
            crate::summary::run_transcription(&whisper, &request)
        });
        let transcription = match transcription {
            Ok(t) => t,
            Err(err) => {
                let mut service = self.service.lock().await;
                let _ = service
                    .store
                    .set_meeting_status(&claimed_job.meeting_id, MeetingStatus::Failed);
                let _ = service
                    .store
                    .set_error_message(&claimed_job.meeting_id, Some(err.to_string()));
                drop(service);
                let mut queue = self.queue.lock().await;
                let _ = queue.retry(&claimed_job.id, err.to_string(), self.summary_max_retries);
                return Err(err.to_string());
            }
        };

        // Phase 2: Summarization (mutex held only for status update)
        {
            let mut service = self.service.lock().await;
            service
                .store
                .set_meeting_status(&claimed_job.meeting_id, MeetingStatus::Summarizing)
                .map_err(|e| e.to_string())?;
        }

        let markdown = tokio::task::block_in_place(|| {
            let prompt =
                crate::summary::build_summary_prompt(&request, &transcription.transcript_for_summary);
            claude.summarize(&prompt)
        });
        let markdown = match markdown {
            Ok(m) => m,
            Err(err) => {
                let mut service = self.service.lock().await;
                let _ = service
                    .store
                    .set_meeting_status(&claimed_job.meeting_id, MeetingStatus::Failed);
                let _ = service
                    .store
                    .set_error_message(&claimed_job.meeting_id, Some(err.to_string()));
                drop(service);
                let mut queue = self.queue.lock().await;
                let _ = queue.retry(&claimed_job.id, err.to_string(), self.summary_max_retries);
                return Err(err.to_string());
            }
        };

        let chunks = split_discord_message(&markdown, DISCORD_MESSAGE_LIMIT);

        // Mark job done
        {
            let mut queue = self.queue.lock().await;
            queue
                .mark_done(&claimed_job.id)
                .map_err(|err| err.to_string())?;
        }

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
                if let Some(session) = sessions.get_mut(&self.guild_id) {
                    if let Err(err) = ingest_voice_frames_into_session(session, &adapted, ts) {
                        warn!(guild_id = %self.guild_id, error = %err, "failed to ingest voice tick");
                    }
                }
            }
            EventContext::ClientDisconnect(evt) => {
                let user_id_u64 = evt.user_id.0;
                warn!(user_id = user_id_u64, "voice client disconnected");
                if user_id_u64 == self.bot_user_id {
                    let runtime = self.runtime.clone();
                    let guild_key = self.guild_id.clone();
                    let http = Arc::clone(&self.http);
                    tokio::spawn(async move {
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
                                if result.outcome == StopOutcome::Owner {
                                    if let Err(err) = runtime
                                        .run_summary_and_notify(&http, &result.meeting_id)
                                        .await
                                    {
                                        warn!(
                                            guild_id = %guild_key,
                                            meeting_id = %result.meeting_id,
                                            error = %err,
                                            "failed to process summary after client disconnect"
                                        );
                                        if let Err(post_err) = runtime
                                            .post_failure_for_meeting(
                                                &http,
                                                &result.meeting_id,
                                                &format!(
                                                    "ClientDisconnect 後の要約処理に失敗しました: {err}"
                                                ),
                                            )
                                            .await
                                        {
                                            warn!(
                                                guild_id = %guild_key,
                                                meeting_id = %result.meeting_id,
                                                error = %post_err,
                                                "failed to notify client-disconnect summary failure"
                                            );
                                        }
                                    }
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
    now_ms: u64,
) -> Result<usize, String> {
    for (user_id, frame) in &adapted.per_user {
        session.ingest_frame(user_id, frame.clone());
    }

    session
        .flush_due(now_ms)
        .map(|chunks| chunks.len())
        .map_err(|err| err.to_string())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_millis() as u64
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
    StopReason::from_str(value).ok_or_else(|| format!("invalid stop reason: {value}"))
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
