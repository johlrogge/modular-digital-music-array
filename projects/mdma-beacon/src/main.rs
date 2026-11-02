// bases/beacon/src/main.rs
use clap::Parser;
use color_eyre::Result;

mod actions;
mod block_devices;
mod config;
mod error;
mod hardware;
mod log_archive;
mod log_tail;
mod mounts;
mod network_fallback;
mod provisioning;
mod routes;
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

    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(|s| s.as_str()))
            .unwrap_or("<non-string panic>");
        tracing::error!(location = %location, payload = %payload, "panic in beacon");
    }));

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

    // Network: on a wifi-only unit, reuse the installed system's wifi. This must
    // run before the server, which blocks on `chronyc waitsync` (needs network).
    // Check mode never mounts or touches services.
    let network = if config.is_check_mode() {
        network_fallback::NetworkFallback::Skipped
    } else {
        tokio::task::spawn_blocking(|| {
            network_fallback::bring_up_wifi_from_installed_system(
                &network_fallback::SystemHost,
                &network_fallback::Settings::default(),
            )
        })
        .await
        .unwrap_or_else(|e| {
            let reason = format!("network fallback task failed: {e}");
            tracing::warn!("{reason}");
            network_fallback::NetworkFallback::Failed(reason)
        })
    };

    // Start HTTP server
    server::run(hardware_info, network, config.clone(), config.log_file).await?;

    Ok(())
}
