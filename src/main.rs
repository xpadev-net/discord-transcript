use discord_transcript::config::AppConfig;
use discord_transcript::runtime::run_bot;
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
    run_bot(&config).await?;
    Ok(())
}
