// bases/beacon/src/provisioning.rs
//! Provisioning orchestration using type-safe pipeline
//!
//! The pipeline enforces correct ordering at compile time:
//! 1. ValidatedHardware (from validation)
//! 2. PartitionedDrives (from partitioning ValidatedHardware)
//! 3. FormattedSystem (from formatting PartitionedDrives)
//! 4. InstalledSystem (from installing to FormattedSystem)
//! 5. ConfiguredSystem (from configuring InstalledSystem)
//! 6. ProvisionedSystem (from finalizing ConfiguredSystem)

use crate::actions::ExecutionMode;
use crate::error::Result;
use crate::hardware::HardwareInfo;
use crate::types::ProvisionConfig;
use tokio::sync::broadcast;
use tracing::info;

/// Send a log message to both tracing and the broadcast channel
macro_rules! send_log {
    ($tx:expr, $($arg:tt)*) => {{
        let msg = format!($($arg)*);
        tracing::info!("{}", msg);
        let _ = $tx.send(msg);
    }};
}

mod pipeline;
pub use pipeline::*;

/// Main provisioning orchestrator
/// 
/// Uses the type-safe pipeline to ensure correct ordering
pub async fn provision_system(
    config: ProvisionConfig,
    hardware: HardwareInfo,
    mode: ExecutionMode,
    log_tx: broadcast::Sender<String>,
) -> Result<()> {
    send_log!(log_tx, "Starting provisioning for {} ({})", config.hostname, config.unit_type);
    
    if mode == ExecutionMode::DryRun {
        send_log!(log_tx, "🚧 DRY RUN MODE - No actual changes will be made");
    }
    
    // Stage 1: Validate hardware
    send_log!(log_tx, "🔍 Validating hardware...");
    let validated = ValidatedHardware::validate(config, hardware, &log_tx)?;
    
    // Stage 2: Partition drives
    // Can ONLY be called with ValidatedHardware!
    send_log!(log_tx, "📦 Partitioning NVMe drives...");
    let partitioned = PartitionedDrives::partition(validated, &log_tx).await?;
    
    // Stage 3: Format partitions
    // Can ONLY be called with PartitionedDrives!
    send_log!(log_tx, "💾 Formatting partitions...");
    let formatted = FormattedSystem::format(partitioned, &log_tx).await?;
    
    // Stage 4: Install base system
    // Can ONLY be called with FormattedSystem!
    send_log!(log_tx, "📥 Installing Void Linux base system...");
    let installed = InstalledSystem::install(formatted, &log_tx).await?;
    
    // Stage 5: Configure system
    // Can ONLY be called with InstalledSystem!
    send_log!(log_tx, "⚙️  Configuring system...");
    let configured = ConfiguredSystem::configure(installed, &log_tx).await?;
    
    // Stage 6: Finalize provisioning
    // Can ONLY be called with ConfiguredSystem!
    let provisioned = ProvisionedSystem::finalize(configured, &log_tx).await?;
    
    // Reboot (only available after successful provisioning!)
    if mode == ExecutionMode::Apply {
        send_log!(log_tx, "🔄 Rebooting system in 10 seconds...");
        provisioned.reboot().await?;
    } else {
        send_log!(log_tx, "🚧 DRY RUN complete - see logs above for what would have been done");
        // In dry run, we drop the ProvisionedSystem without rebooting
    }
    
    send_log!(log_tx, "✅ Provisioning completed successfully!");
    
    Ok(())
}
