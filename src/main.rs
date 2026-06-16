use discord_transcript::application::runtime::{BotRunExit, SummaryJobWakeups, run_bot};
use discord_transcript::application::worker::{
    ProcessJobResult, SummaryJobOptions, SummaryNotificationReceipt, SummaryStatusNotification,
    SummaryUrlNotification, WorkerError, complete_summary_job_after_notification,
    process_next_summary_job, record_summary_completion_usage_observe_only,
};
use discord_transcript::bootstrap::config::{AppConfig, AppRole};
use discord_transcript::infrastructure::bot_token::{
    BotTokenCipher, BotTokenResolveError, resolve_effective_bot_token,
};
use discord_transcript::infrastructure::integrations::{
    CommandWhisperClient, DEFAULT_COMMAND_TIMEOUT, HarnessCliSummaryClient,
};
use discord_transcript::infrastructure::queue::JobQueue;
use discord_transcript::infrastructure::retry::RetryPolicy;
use discord_transcript::infrastructure::sql::{
    CREATE_SCHEMA_MIGRATIONS_SQL, LOCK_SCHEMA_MIGRATIONS_SQL, MIGRATIONS,
    SELECT_SCHEMA_MIGRATION_SQL, UNLOCK_SCHEMA_MIGRATIONS_SQL, migration_transaction_sql,
};
use discord_transcript::infrastructure::sql_store::{PgSqlExecutor, SqlJobQueue, SqlMeetingStore};
use discord_transcript::infrastructure::storage::{MeetingStore, StoredMeeting};
use discord_transcript::interfaces::web;
use serenity::all::{ChannelId, EditMessage};
use serenity::http::Http;
use std::env;
use std::sync::Arc;
use std::time::Duration;
use tokio_postgres::NoTls;
use tracing_subscriber::{EnvFilter, fmt};

#[tokio::main]
async fn main() {
    let _ = fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,serenity=warn,songbird=warn")),
        )
        .try_init();

    let result = match env::args().nth(1).as_deref() {
        Some("migrate") => run_migrations_from_env().await,
        _ => run().await,
    };

    if let Err(err) = result {
        tracing::error!(error = %err, "fatal");
        std::process::exit(1);
    }
}

async fn run_migrations_from_env() -> Result<(), Box<dyn std::error::Error>> {
    let database_url =
        env::var("DATABASE_URL").map_err(|_| "missing required env var: DATABASE_URL")?;
    let database_ssl_mode = env::var("DATABASE_SSL_MODE").unwrap_or_else(|_| "disable".to_owned());
    let db_url = database_url_with_ssl_mode(&database_url, &database_ssl_mode)?;
    let (db_client, db_connection) = tokio_postgres::connect(&db_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(err) = db_connection.await {
            tracing::error!(error = %err, "migration db connection lost");
        }
    });
    apply_pending_migrations(&db_client).await?;
    Ok(())
}

fn database_url_with_ssl_mode(
    database_url: &str,
    database_ssl_mode: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    if database_ssl_mode != "disable" {
        return Err(format!(
            "DATABASE_SSL_MODE={database_ssl_mode} is not supported for this database connection (only \"disable\" is supported with NoTls)"
        )
        .into());
    }
    if let Some(url_ssl_mode) = database_url_ssl_mode(database_url) {
        if url_ssl_mode != "disable" {
            return Err(format!(
                "DATABASE_URL sslmode={url_ssl_mode} is not supported for this database connection (only \"disable\" is supported with NoTls)"
            )
            .into());
        }
        Ok(database_url.to_owned())
    } else {
        let sep = if database_url.contains('?') { '&' } else { '?' };
        Ok(format!("{database_url}{sep}sslmode={database_ssl_mode}"))
    }
}

fn database_url_ssl_mode(database_url: &str) -> Option<&str> {
    database_url
        .split_once('?')?
        .1
        .split('&')
        .filter_map(|param| param.split_once('='))
        .find_map(|(key, value)| (key == "sslmode").then_some(value))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GatewayBotTokenStartDecision {
    Start(String),
    WaitForRepair,
}

fn gateway_bot_token_start_decision(
    guild_id: &str,
    resolved: Result<String, BotTokenResolveError>,
) -> Result<GatewayBotTokenStartDecision, BotTokenResolveError> {
    match resolved {
        Ok(token) => Ok(GatewayBotTokenStartDecision::Start(token)),
        Err(BotTokenResolveError::Database(err)) => Err(BotTokenResolveError::Database(err)),
        Err(err) => {
            tracing::warn!(
                error = %err,
                guild_id = %guild_id,
                "stored guild bot token could not be resolved; Discord gateway will stay stopped until token settings are repaired"
            );
            Ok(GatewayBotTokenStartDecision::WaitForRepair)
        }
    }
}

async fn wait_for_gateway_bot_token_repair(revision: &mut tokio::sync::watch::Receiver<u64>) {
    if revision.changed().await.is_ok() {
        tracing::info!("retrying Discord gateway startup after guild bot token settings changed");
    } else {
        tracing::error!(
            "guild bot token revision channel closed; Discord gateway will remain stopped until process restart"
        );
        std::future::pending::<()>().await;
    }
}

async fn apply_pending_migrations(
    db_client: &tokio_postgres::Client,
) -> Result<(), Box<dyn std::error::Error>> {
    db_client.batch_execute(LOCK_SCHEMA_MIGRATIONS_SQL).await?;
    let result = apply_pending_migrations_locked(db_client).await;
    let unlock_result = db_client.batch_execute(UNLOCK_SCHEMA_MIGRATIONS_SQL).await;
    match (result, unlock_result) {
        (Err(err), _) => Err(err),
        (Ok(()), Err(err)) => Err(err.into()),
        (Ok(()), Ok(())) => Ok(()),
    }
}

async fn apply_pending_migrations_locked(
    db_client: &tokio_postgres::Client,
) -> Result<(), Box<dyn std::error::Error>> {
    db_client
        .batch_execute(CREATE_SCHEMA_MIGRATIONS_SQL)
        .await?;
    for migration in MIGRATIONS {
        let already_applied = db_client
            .query_opt(SELECT_SCHEMA_MIGRATION_SQL, &[&migration.version])
            .await?
            .is_some();
        if already_applied {
            continue;
        }
        tracing::info!(version = migration.version, "applying database migration");
        db_client
            .batch_execute(&migration_transaction_sql(*migration))
            .await?;
    }
    Ok(())
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = AppConfig::from_env()?;
    match runtime_entrypoints_for_role(config.app_role) {
        RuntimeEntrypoints::WebBot => run_web_and_gateway(config).await,
        RuntimeEntrypoints::Worker => run_standalone_worker(config).await,
    }
}

async fn run_web_and_gateway(config: AppConfig) -> Result<(), Box<dyn std::error::Error>> {
    // Establish async DB connection for the web server
    let db_url = database_url_with_ssl_mode(&config.database_url, &config.database_ssl_mode)?;
    let (db_client, db_connection) = tokio_postgres::connect(&db_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(err) = db_connection.await {
            tracing::error!(error = %err, "web db connection lost, exiting");
            std::process::exit(1);
        }
    });

    // Run migrations on the web DB connection before serving requests.
    apply_pending_migrations(&db_client).await?;

    let db_client = Arc::new(db_client);
    let guild_bot_token_cipher = config
        .guild_bot_token_encryption_key
        .as_deref()
        .map(BotTokenCipher::new)
        .transpose()?
        .map(Arc::new);
    let (bot_token_revision_tx, bot_token_revision_rx) = tokio::sync::watch::channel(0u64);

    // Build OAuth config if all required fields are present
    let auth = match (
        &config.discord_client_id,
        &config.discord_client_secret,
        &config.web_session_secret,
        &config.public_base_url,
    ) {
        (Some(client_id), Some(client_secret), Some(session_secret), Some(base_url)) => {
            let redirect_uri = format!("{}/auth/callback", base_url.trim_end_matches('/'));
            tracing::info!("Discord OAuth enabled (redirect_uri: {redirect_uri})");
            let secure_cookie = base_url.starts_with("https://");
            Some(Arc::new(web::AuthConfig {
                client_id: client_id.clone(),
                client_secret: client_secret.clone(),
                session_secret: session_secret.clone(),
                redirect_uri,
                guild_id: config.discord_guild_id.clone(),
                bot_token: config.discord_token.clone(),
                secure_cookie,
            }))
        }
        _ => {
            tracing::warn!(
                "Discord OAuth disabled: set DISCORD_CLIENT_ID, DISCORD_CLIENT_SECRET, \
                 WEB_SESSION_SECRET, and PUBLIC_BASE_URL to enable authentication"
            );
            None
        }
    };

    let summary_job_wakeups = SummaryJobWakeups::new();
    let web_state = web::WebState::new(
        Arc::clone(&db_client),
        config.chunk_storage_dir.clone(),
        auth,
        reqwest::Client::builder()
            .use_rustls_tls()
            .timeout(std::time::Duration::from_secs(10))
            .connect_timeout(std::time::Duration::from_secs(5))
            .build()?,
        web::GuildBotTokenRuntimeConfig {
            cipher: guild_bot_token_cipher.clone(),
            revision_tx: Some(bot_token_revision_tx),
            operational_metrics_bearer_token: config.operational_metrics_bearer_token.clone(),
            summary_job_wakeups: Some(summary_job_wakeups.clone()),
        },
        config.static_files_dir.clone(),
        web::GuildSettingsDefaults {
            whisper_language: config.whisper_language.clone(),
            whisper_vad: config.whisper_vad,
            auto_stop_grace_seconds: i64::try_from(config.auto_stop_grace_seconds)
                .expect("auto_stop_grace_seconds exceeds i64::MAX"),
            retention_raw_audio_ttl_days: i32::try_from(
                config.retention_policy.raw_audio_ttl_days.get(),
            )
            .expect("retention_raw_audio_ttl_days exceeds i32::MAX"),
            retention_transcript_ttl_days: i32::try_from(
                config.retention_policy.transcript_ttl_days.get(),
            )
            .expect("retention_transcript_ttl_days exceeds i32::MAX"),
            summary_enabled: config.summary_enabled,
        },
    );
    let router = web::create_router(web_state);

    let web_bind_host = config.web_bind_host.clone();
    let web_port = config.web_port;
    let listener = tokio::net::TcpListener::bind((&*web_bind_host, web_port)).await?;
    tracing::info!(host = %web_bind_host, port = web_port, "web server listening");
    tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, router).await {
            tracing::error!(error = %err, "web server fatal error, exiting");
            std::process::exit(1);
        }
    });

    loop {
        let mut runtime_bot_token_revision_rx = bot_token_revision_rx.clone();
        runtime_bot_token_revision_rx.borrow_and_update();
        // If a token update lands between this mark and the DB resolve below,
        // this run may start once with the token read by this iteration, then
        // observes the revision change and restarts on the next loop.
        let effective_discord_token = match gateway_bot_token_start_decision(
            &config.discord_guild_id,
            resolve_effective_bot_token(
                &db_client,
                &config.discord_guild_id,
                &config.discord_token,
                guild_bot_token_cipher.as_deref(),
            )
            .await,
        )? {
            GatewayBotTokenStartDecision::Start(token) => token,
            GatewayBotTokenStartDecision::WaitForRepair => {
                wait_for_gateway_bot_token_repair(&mut runtime_bot_token_revision_rx).await;
                continue;
            }
        };
        let mut runtime_config = config.clone();
        runtime_config.discord_token = effective_discord_token;

        match run_bot(
            &runtime_config,
            runtime_bot_token_revision_rx,
            summary_job_wakeups.clone(),
        )
        .await?
        {
            BotRunExit::Shutdown => break,
            BotRunExit::TokenChanged => {
                tracing::info!("restarting Discord gateway after guild bot token update");
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeEntrypoints {
    WebBot,
    Worker,
}

fn runtime_entrypoints_for_role(role: AppRole) -> RuntimeEntrypoints {
    match role {
        AppRole::All | AppRole::WebBot => RuntimeEntrypoints::WebBot,
        AppRole::Worker => RuntimeEntrypoints::Worker,
    }
}

async fn run_standalone_worker(config: AppConfig) -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!(app_role = %config.app_role, "starting standalone summary worker");

    let migration_executor =
        PgSqlExecutor::connect_with_ssl_mode(&config.database_url, &config.database_ssl_mode)?;
    let mut migration_store = SqlMeetingStore::new(migration_executor);
    migration_store.apply_pending_migrations()?;

    let mut store = SqlMeetingStore::new(PgSqlExecutor::connect_with_ssl_mode(
        &config.database_url,
        &config.database_ssl_mode,
    )?);
    let mut queue = SqlJobQueue::new(PgSqlExecutor::connect_with_ssl_mode(
        &config.database_url,
        &config.database_ssl_mode,
    )?);
    let token_db_url = database_url_with_ssl_mode(&config.database_url, &config.database_ssl_mode)?;
    let (token_db_client, token_db_connection) =
        tokio_postgres::connect(&token_db_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(err) = token_db_connection.await {
            tracing::error!(error = %err, "worker bot token db connection lost");
        }
    });
    let guild_bot_token_cipher = config
        .guild_bot_token_encryption_key
        .as_deref()
        .map(BotTokenCipher::new)
        .transpose()?;
    let retry_policy = RetryPolicy {
        max_attempts: config.integration_retry_max_attempts,
        initial_delay: Duration::from_millis(config.integration_retry_initial_delay_ms),
        backoff_multiplier: config.integration_retry_backoff_multiplier,
        max_delay: Duration::from_millis(config.integration_retry_max_delay_ms),
    };
    let whisper = CommandWhisperClient {
        endpoint: config.whisper_endpoint.clone(),
        curl_bin: "curl".to_owned(),
        retry_policy,
        beam_size: config.whisper_beam_size,
        suppress_non_speech: config.whisper_suppress_non_speech,
        prompt: config.whisper_prompt.clone(),
        vad: config.whisper_vad,
        temperature: config.whisper_temperature,
        command_timeout: DEFAULT_COMMAND_TIMEOUT,
    };
    let summary_client = HarnessCliSummaryClient {
        harness: config.summary_harness,
        command_path: config.summary_command.clone(),
        model: config.summary_model.clone(),
        allow_unsafe_agent_harness: config.summary_allow_unsafe_agent_harness,
        retry_policy,
        command_timeout: DEFAULT_COMMAND_TIMEOUT,
    };
    let options = SummaryJobOptions {
        max_retries: config.summary_max_retries,
        audio_base_dir: config.chunk_storage_dir.clone(),
        language: config.whisper_language.clone(),
        resample_to_16k: config.whisper_resample_to_16k,
    };
    let mut idle_sleep = Box::pin(tokio::time::sleep(Duration::ZERO));

    loop {
        tokio::select! {
            () = shutdown_signal() => {
                tracing::info!("shutdown signal received");
                break;
            }
            () = &mut idle_sleep => {
                if let Err(err) = queue.ready_summary_meeting_ids() {
                    tracing::warn!(
                        error = %err,
                        "standalone worker failed to recover stale running summary jobs"
                    );
                }
                match process_next_summary_job(
                    &mut store,
                    &mut queue,
                    &whisper,
                    &summary_client,
                    &options,
                ) {
                    Ok(Some(result)) => {
                        let chunk_count = result.output.chunks.len();
                        let completion_result =
                            match notify_standalone_worker_summary(
                                &config,
                                &token_db_client,
                                guild_bot_token_cipher.as_ref(),
                                &mut store,
                                &result,
                            )
                            .await
                            {
                                Ok(receipt) => complete_summary_job_after_notification(
                                    &mut store,
                                    &mut queue,
                                    &result.job,
                                    receipt,
                                ),
                                Err(err) => Err(err),
                            };
                        match completion_result {
                            Ok(true) => {
                                record_summary_completion_usage_observe_only(
                                    &mut store,
                                    &result.output.meeting_id,
                                    &result.job_id,
                                    chunk_count,
                                );
                            }
                            Ok(false) => match queue.mark_done(&result.job) {
                                Ok(()) => {
                                    record_summary_completion_usage_observe_only(
                                        &mut store,
                                        &result.output.meeting_id,
                                        &result.job_id,
                                        chunk_count,
                                    );
                                }
                                Err(err) => {
                                    tracing::warn!(
                                        job_id = %result.job_id,
                                        meeting_id = %result.output.meeting_id,
                                        error = %err,
                                        "standalone worker could not mark already-posted summary job done"
                                    );
                                }
                            },
                            Err(err) => {
                                tracing::warn!(
                                    job_id = %result.job_id,
                                    meeting_id = %result.output.meeting_id,
                                    error = %err,
                                    "standalone worker could not complete generated summary job"
                                );
                            }
                        }
                        idle_sleep.as_mut().reset(tokio::time::Instant::now());
                    }
                    Ok(None) => {
                        idle_sleep.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(5));
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "standalone worker summary job attempt failed");
                        idle_sleep.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(5));
                    }
                }
            }
        }
    }

    Ok(())
}

async fn notify_standalone_worker_summary(
    config: &AppConfig,
    token_db_client: &tokio_postgres::Client,
    guild_bot_token_cipher: Option<&BotTokenCipher>,
    store: &mut SqlMeetingStore<PgSqlExecutor>,
    result: &ProcessJobResult,
) -> Result<SummaryNotificationReceipt, WorkerError> {
    let meeting = store
        .get_meeting(&result.output.meeting_id)
        .map_err(WorkerError::from)?
        .ok_or_else(|| {
            WorkerError::Completion(format!(
                "meeting not found while notifying summary: {}",
                result.output.meeting_id
            ))
        })?;
    let token = resolve_effective_bot_token(
        token_db_client,
        &meeting.guild_id,
        &config.discord_token,
        guild_bot_token_cipher,
    )
    .await
    .map_err(|err| {
        WorkerError::Completion(format!(
            "failed to resolve bot token for summary notification: {err}"
        ))
    })?;
    if token.trim().is_empty() {
        return Err(WorkerError::Completion(
            "no global or guild bot token is configured; generated summary is waiting for Discord notification"
                .to_owned(),
        ));
    }
    let report_channel_id = meeting.report_channel_id.parse::<u64>().map_err(|err| {
        WorkerError::Completion(format!(
            "invalid report channel id for meeting {}: {}",
            result.output.meeting_id, err
        ))
    })?;
    let http = Http::new(&token);

    let chunks = summary_chunks_with_voice_channel_metadata(&meeting, result.output.chunks.clone());
    post_summary_to_report_channel(&http, report_channel_id, &chunks)
        .await
        .map_err(WorkerError::Completion)?;

    let url_notification =
        if let Some(url) = meeting_url(config.public_base_url.as_deref(), &meeting.id) {
            let url_msg = format!("詳細はこちら: {url}");
            match post_summary_to_report_channel(&http, report_channel_id, &[url_msg]).await {
                Ok(()) => SummaryUrlNotification::Posted,
                Err(err) => {
                    tracing::warn!(
                        meeting_id = %meeting.id,
                        error = %err,
                        "failed to post meeting URL from standalone worker"
                    );
                    SummaryUrlNotification::FailedBestEffort
                }
            }
        } else {
            SummaryUrlNotification::NotConfigured
        };

    let status_notification =
        match upsert_summary_completed_status_message(&http, store, &meeting.id, config).await {
            Ok(()) => SummaryStatusNotification::Updated,
            Err(err) => {
                tracing::warn!(
                    meeting_id = %meeting.id,
                    error = %err,
                    "failed to update summary status message from standalone worker"
                );
                SummaryStatusNotification::FailedBestEffort
            }
        };

    SummaryNotificationReceipt::new(chunks.len(), url_notification, status_notification)
}

fn meeting_url(public_base_url: Option<&str>, meeting_id: &str) -> Option<String> {
    public_base_url
        .map(str::trim)
        .filter(|base_url| !base_url.is_empty())
        .map(|base_url| format!("{}/meetings/{}", base_url.trim_end_matches('/'), meeting_id))
}

fn meeting_voice_channel_display(meeting: &StoredMeeting) -> String {
    meeting
        .voice_channel_name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("VC ID: {}", meeting.voice_channel_id))
}

fn summary_chunks_with_voice_channel_metadata(
    meeting: &StoredMeeting,
    chunks: Vec<String>,
) -> Vec<String> {
    let display = meeting_voice_channel_display(meeting);
    let has_name = meeting
        .voice_channel_name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .is_some();
    let header = if has_name {
        format!("VC: {display} ({})", meeting.voice_channel_id)
    } else {
        display
    };
    let mut with_metadata = Vec::with_capacity(chunks.len().saturating_add(1));
    with_metadata.push(header);
    with_metadata.extend(chunks);
    with_metadata
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

async fn upsert_summary_completed_status_message(
    http: &Http,
    store: &mut SqlMeetingStore<PgSqlExecutor>,
    meeting_id: &str,
    config: &AppConfig,
) -> Result<(), String> {
    let metadata = store
        .get_status_message_metadata(meeting_id)
        .map_err(|err| err.to_string())?;
    let channel_id_str = metadata
        .status_message_channel_id
        .as_deref()
        .unwrap_or(&metadata.report_channel_id);
    let channel_id = channel_id_str.parse::<u64>().map_err(|err| {
        format!(
            "invalid status message channel id: meeting_id={meeting_id}, value={channel_id_str}, error={err}"
        )
    })?;
    let content = format_summary_completed_status_message(
        meeting_id,
        meeting_url(config.public_base_url.as_deref(), meeting_id).as_deref(),
    );
    let channel = ChannelId::new(channel_id);

    let message_id = match metadata.status_message_id.as_deref() {
        Some(message_id_str) => match message_id_str.parse::<u64>() {
            Ok(message_id) => {
                channel
                    .edit_message(http, message_id, EditMessage::new().content(&content))
                    .await
                    .map_err(|err| err.to_string())?;
                message_id
            }
            Err(err) => {
                tracing::warn!(
                    meeting_id,
                    message_id = message_id_str,
                    error = %err,
                    "invalid status message id, recreating status message from standalone worker"
                );
                channel
                    .say(http, &content)
                    .await
                    .map_err(|err| err.to_string())?
                    .id
                    .get()
            }
        },
        None => channel
            .say(http, &content)
            .await
            .map_err(|err| err.to_string())?
            .id
            .get(),
    };

    store
        .set_status_message(meeting_id, channel_id.to_string(), message_id.to_string())
        .map_err(|err| err.to_string())
}

fn format_summary_completed_status_message(meeting_id: &str, summary_url: Option<&str>) -> String {
    let base = format!("要約が完了しました\nmeeting_id={meeting_id}");
    summary_url.map_or(base.clone(), |url| format!("{base}\n詳細ページ: {url}"))
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BotTokenResolveError, GatewayBotTokenStartDecision, RuntimeEntrypoints,
        database_url_with_ssl_mode, gateway_bot_token_start_decision, runtime_entrypoints_for_role,
        wait_for_gateway_bot_token_repair,
    };
    use discord_transcript::bootstrap::config::AppRole;
    use discord_transcript::infrastructure::bot_token::{
        BOT_TOKEN_KEY_VERSION, BotTokenCipher, EncryptedBotToken, GuildBotTokenMetadata,
        StoredGuildBotToken, resolve_bot_token_from_record,
    };

    fn stored(encrypted: EncryptedBotToken) -> StoredGuildBotToken {
        StoredGuildBotToken {
            encrypted,
            metadata: GuildBotTokenMetadata {
                updated_at: None,
                last_validated_at: None,
                bot_user_id: None,
                bot_username: None,
            },
        }
    }

    #[test]
    fn database_url_with_ssl_mode_appends_query_param_separator() {
        assert_eq!(
            database_url_with_ssl_mode("postgresql://user:pass@localhost/db", "disable")
                .expect("url should build"),
            "postgresql://user:pass@localhost/db?sslmode=disable"
        );
        assert_eq!(
            database_url_with_ssl_mode(
                "postgresql://user:pass@localhost/db?connect_timeout=10",
                "disable",
            )
            .expect("url should build"),
            "postgresql://user:pass@localhost/db?connect_timeout=10&sslmode=disable"
        );
    }

    #[test]
    fn database_url_with_ssl_mode_preserves_existing_sslmode() {
        assert_eq!(
            database_url_with_ssl_mode(
                "postgresql://user:pass@localhost/db?sslmode=disable",
                "disable",
            )
            .expect("url should build"),
            "postgresql://user:pass@localhost/db?sslmode=disable"
        );
    }

    #[test]
    fn database_url_with_ssl_mode_rejects_unsupported_configured_sslmode() {
        let err = database_url_with_ssl_mode("postgresql://user:pass@localhost/db", "require")
            .expect_err("unsupported sslmode should fail");

        assert!(err.to_string().contains("DATABASE_SSL_MODE=require"));
    }

    #[test]
    fn database_url_with_ssl_mode_rejects_unsupported_embedded_sslmode() {
        let err = database_url_with_ssl_mode(
            "postgresql://user:pass@localhost/db?sslmode=require",
            "disable",
        )
        .expect_err("unsupported sslmode should fail");

        assert!(err.to_string().contains("sslmode=require"));
    }

    #[test]
    fn gateway_waits_for_repair_when_stored_token_has_missing_cipher() {
        let cipher = BotTokenCipher::new("secret key material").expect("cipher");
        let encrypted = cipher
            .encrypt_for_guild("guild-1", "guild-token")
            .expect("encrypt");
        let resolved = resolve_bot_token_from_record(
            "guild-1",
            "global-token",
            Some(&stored(encrypted)),
            None,
        );

        let decision = gateway_bot_token_start_decision("guild-1", resolved)
            .expect("missing cipher should wait, not fail fatal");

        assert_eq!(decision, GatewayBotTokenStartDecision::WaitForRepair);
    }

    #[test]
    fn gateway_waits_for_repair_when_stored_token_ciphertext_is_bad() {
        let cipher = BotTokenCipher::new("secret key material").expect("cipher");
        let encrypted = EncryptedBotToken {
            ciphertext: "not valid base64!".to_owned(),
            nonce: "AAAAAAAAAAAAAAAA".to_owned(),
            key_version: BOT_TOKEN_KEY_VERSION.to_owned(),
        };
        let stored = stored(encrypted);
        let resolved =
            resolve_bot_token_from_record("guild-1", "global-token", Some(&stored), Some(&cipher));

        let decision = gateway_bot_token_start_decision("guild-1", resolved)
            .expect("crypto errors should wait, not fail fatal");

        assert_eq!(decision, GatewayBotTokenStartDecision::WaitForRepair);
    }

    #[test]
    fn gateway_starts_with_global_token_when_stored_token_absent() {
        let resolved = resolve_bot_token_from_record("guild-1", "global-token", None, None);

        let decision = gateway_bot_token_start_decision("guild-1", resolved)
            .expect("absent stored token should use global fallback");

        assert_eq!(
            decision,
            GatewayBotTokenStartDecision::Start("global-token".to_owned())
        );
    }

    #[test]
    fn gateway_propagates_database_resolve_errors() {
        let err = gateway_bot_token_start_decision(
            "guild-1",
            Err(BotTokenResolveError::Database("db down".to_owned())),
        )
        .expect_err("database errors should remain fatal to gateway startup");

        match err {
            BotTokenResolveError::Database(message) => assert_eq!(message, "db down"),
            other => panic!("expected database error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn gateway_wait_for_repair_returns_after_token_revision_changes() {
        let (sender, mut receiver) = tokio::sync::watch::channel(0u64);
        receiver.borrow_and_update();

        sender.send_replace(1);

        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            wait_for_gateway_bot_token_repair(&mut receiver),
        )
        .await
        .expect("token revision change should wake gateway repair wait");
    }

    #[test]
    fn runtime_entrypoints_select_role_specific_process_surfaces() {
        assert_eq!(
            runtime_entrypoints_for_role(AppRole::All),
            RuntimeEntrypoints::WebBot
        );
        assert_eq!(
            runtime_entrypoints_for_role(AppRole::WebBot),
            RuntimeEntrypoints::WebBot
        );
        assert_eq!(
            runtime_entrypoints_for_role(AppRole::Worker),
            RuntimeEntrypoints::Worker
        );
    }
}
