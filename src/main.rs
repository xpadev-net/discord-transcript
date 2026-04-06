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

    // Establish async DB connection for the web server
    let db_url = if config.database_url.contains("sslmode=") {
        config.database_url.clone()
    } else {
        let sep = if config.database_url.contains('?') {
            '&'
        } else {
            '?'
        };
        format!("{}{}sslmode={}", config.database_url, sep, config.database_ssl_mode)
    };
    let (db_client, db_connection) = tokio_postgres::connect(&db_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(err) = db_connection.await {
            tracing::error!(error = %err, "web db connection error");
        }
    });

    let web_state = web::WebState {
        db: Arc::new(db_client),
        chunk_storage_dir: config.chunk_storage_dir.clone(),
    };
    let router = web::create_router(web_state);

    let web_port = config.web_port;
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", web_port)).await?;
    tracing::info!(port = web_port, "web server listening");
    tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, router).await {
            tracing::error!(error = %err, "web server error");
        }
    });

    run_bot(&config).await?;
    Ok(())
}
