use discord_transcript::application::runtime::{BotRunExit, SummaryJobWakeups, run_bot};
use discord_transcript::bootstrap::config::AppConfig;
use discord_transcript::infrastructure::bot_token::{
    BotTokenCipher, BotTokenResolveError, resolve_effective_bot_token,
};
use discord_transcript::infrastructure::sql::{
    CREATE_SCHEMA_MIGRATIONS_SQL, LOCK_SCHEMA_MIGRATIONS_SQL, MIGRATIONS,
    SELECT_SCHEMA_MIGRATION_SQL, UNLOCK_SCHEMA_MIGRATIONS_SQL, migration_transaction_sql,
};
use discord_transcript::interfaces::web;
use std::env;
use std::sync::Arc;
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

#[cfg(test)]
mod tests {
    use super::{
        BotTokenResolveError, GatewayBotTokenStartDecision, database_url_with_ssl_mode,
        gateway_bot_token_start_decision, wait_for_gateway_bot_token_repair,
    };
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
}
