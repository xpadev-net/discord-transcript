use discord_transcript::config::AppConfig;
use discord_transcript::runtime::run_bot;
use discord_transcript::web;
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
            tracing::error!(error = %err, "web db connection error");
        }
    });

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

    let web_state = web::WebState {
        db: Arc::new(db_client),
        chunk_storage_dir: config.chunk_storage_dir.clone(),
        auth,
        http_client: reqwest::Client::new(),
    };
    let router = web::create_router(web_state);

    let web_port = config.web_port;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", web_port)).await?;
    tracing::info!(host = "127.0.0.1", port = web_port, "web server listening");
    tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, router).await {
            tracing::error!(error = %err, "web server error");
        }
    });

    run_bot(&config).await?;
    Ok(())
}
