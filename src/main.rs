use discord_transcript::application::runtime::{BotRunExit, run_bot};
use discord_transcript::bootstrap::config::AppConfig;
use discord_transcript::infrastructure::bot_token::{
    BotTokenCipher, BotTokenResolveError, resolve_effective_bot_token,
};
use discord_transcript::interfaces::web;
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

    if let Err(err) = run().await {
        tracing::error!(error = %err, "fatal");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = AppConfig::from_env()?;

    // The web server's async tokio_postgres connection uses NoTls,
    // so reject non-"disable" SSL modes to avoid silent downgrade.
    if config.database_ssl_mode != "disable" {
        return Err(format!(
            "DATABASE_SSL_MODE={} is not supported for the web server connection (only \"disable\" is supported with NoTls)",
            config.database_ssl_mode,
        ).into());
    }

    // Establish async DB connection for the web server
    let db_url = if config.database_url.contains("sslmode=") {
        config.database_url.clone()
    } else {
        format!(
            "{}?sslmode={}",
            config.database_url, config.database_ssl_mode
        )
    };
    let (db_client, db_connection) = tokio_postgres::connect(&db_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(err) = db_connection.await {
            tracing::error!(error = %err, "web db connection lost, exiting");
            std::process::exit(1);
        }
    });

    // Run migrations on the web DB connection before serving requests.
    // These are idempotent (IF NOT EXISTS), so running them here AND in
    // run_bot() is safe — but this ensures the web server never faces
    // a pre-migration schema.
    db_client
        .batch_execute(discord_transcript::infrastructure::sql::INITIAL_SCHEMA_SQL)
        .await?;
    db_client
        .batch_execute(discord_transcript::infrastructure::sql::INCREMENTAL_MIGRATIONS_SQL)
        .await?;

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

    let web_state = web::WebState::new(
        Arc::clone(&db_client),
        config.chunk_storage_dir.clone(),
        auth,
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .connect_timeout(std::time::Duration::from_secs(5))
            .build()?,
        web::GuildBotTokenRuntimeConfig {
            cipher: guild_bot_token_cipher.clone(),
            revision_tx: Some(bot_token_revision_tx),
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
        let effective_discord_token = match resolve_effective_bot_token(
            &db_client,
            &config.discord_guild_id,
            &config.discord_token,
            guild_bot_token_cipher.as_deref(),
        )
        .await
        {
            Ok(token) => token,
            Err(BotTokenResolveError::Database(err)) => {
                return Err(BotTokenResolveError::Database(err).into());
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    guild_id = %config.discord_guild_id,
                    "failed to resolve stored guild bot token for gateway; using global token so settings recovery stays online"
                );
                config.discord_token.clone()
            }
        };
        let mut runtime_config = config.clone();
        runtime_config.discord_token = effective_discord_token;

        match run_bot(&runtime_config, runtime_bot_token_revision_rx).await? {
            BotRunExit::Shutdown => break,
            BotRunExit::TokenChanged => {
                tracing::info!("restarting Discord gateway after guild bot token update");
            }
        }
    }
    Ok(())
}
