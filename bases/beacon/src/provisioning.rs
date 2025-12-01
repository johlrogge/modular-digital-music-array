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
use tracing::info;

mod pipeline;
pub use pipeline::*;

/// Main provisioning orchestrator
/// 
/// Uses the type-safe pipeline to ensure correct ordering
pub async fn provision_system(
    config: ProvisionConfig,
    hardware: HardwareInfo,
    mode: ExecutionMode,
) -> Result<()> {
    info!("Starting provisioning for {} ({})", config.hostname, config.unit_type);
    
    if mode == ExecutionMode::DryRun {
        info!("🚧 DRY RUN MODE - No actual changes will be made");
    }
    
    // Stage 1: Validate hardware
    let validated = ValidatedHardware::validate(config, hardware)?;
    
    // Stage 2: Partition drives
    // Can ONLY be called with ValidatedHardware!
    let partitioned = PartitionedDrives::partition(validated).await?;
    
    // Stage 3: Format partitions
    // Can ONLY be called with PartitionedDrives!
    let formatted = FormattedSystem::format(partitioned).await?;
    
    // Stage 4: Install base system
    // Can ONLY be called with FormattedSystem!
    let installed = InstalledSystem::install(formatted).await?;
    
    // Stage 5: Configure system
    // Can ONLY be called with InstalledSystem!
    let configured = ConfiguredSystem::configure(installed).await?;
    
    // Stage 6: Finalize provisioning
    // Can ONLY be called with ConfiguredSystem!
    let provisioned = ProvisionedSystem::finalize(configured).await?;
    
    // Reboot (only available after successful provisioning!)
    if mode == ExecutionMode::Apply {
        provisioned.reboot().await?;
    } else {
        info!("🚧 DRY RUN complete - see logs above for what would have been done");
        // In dry run, we drop the ProvisionedSystem without rebooting
    }
    
    Ok(())
}
