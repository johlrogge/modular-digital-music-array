// bases/beacon/src/main.rs
use clap::Parser;
use color_eyre::Result;

mod actions;
mod config;
mod error;
mod hardware;
mod provisioning;
mod server;
mod types;
mod update;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "beacon=info,tower_http=info".into()),
        )
        .init();

    // Log version info
    tracing::info!(
        "Beacon {} (built: {})",
        update::full_version(),
        update::build_timestamp()
    );

    // Parse CLI arguments
    let args = config::CliArgs::parse();
    let config = config::Config::from_args(args);

    if config.is_check_mode() {
        tracing::warn!(
            "🔍 CHECK MODE: Running on port {}, DRY RUN only (no changes will be made)",
            config.port
        );
        tracing::warn!("   Use --apply to actually provision the system");
    } else {
        tracing::warn!("⚠️  APPLY MODE: Changes WILL be made to your system!");
        tracing::info!("   Starting MDMA Beacon in production mode...");
    }

    // Detect hardware
    let hardware_info = hardware::detect_hardware().await?;
    tracing::info!("Detected hardware: {:?}", hardware_info);

    // Start HTTP server
    server::run(hardware_info, config).await?;

    Ok(())
}
