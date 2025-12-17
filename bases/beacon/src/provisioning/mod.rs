// bases/beacon/src/provisioning/mod.rs
//! Provisioning system for MDMA units
//!
//! This module implements a type-safe, action-based provisioning pipeline.
//! Each stage is an Action that transforms input to output, and the type
//! system enforces correct ordering.

use crate::actions::{execute_action, ExecutionMode};
use crate::error::Result;
use crate::hardware::HardwareInfo;
use tokio::sync::broadcast;

pub mod types;

mod stage0_safety;
mod stage1_validate;
mod stage2_partition;
mod stage3_format;
mod stage4_install;
mod stage5_configure;
mod stage6_finalize;

pub use stage0_safety::CheckRaspberryPiAction;
pub use stage1_validate::ValidateHardwareAction;
pub use stage2_partition::PartitionDrivesAction;
pub use stage3_format::FormatPartitionsAction;
pub use stage4_install::InstallSystemAction;
pub use stage5_configure::ConfigureSystemAction;
pub use stage6_finalize::FinalizeProvisioningAction;

pub use types::{ProvisionConfig, ProvisionedSystem, UnitType, WifiConfig};

/// Provision a system through all stages
///
/// This is the main entry point for provisioning. It executes all stages
/// in order, with the type system enforcing that stages cannot be skipped
/// or executed out of order.
pub async fn provision_system(
    config: ProvisionConfig,
    hardware: HardwareInfo,
    mode: ExecutionMode,
    log_tx: broadcast::Sender<String>,
) -> Result<ProvisionedSystem> {
    let mode_str = match mode {
        ExecutionMode::DryRun => "DRY RUN",
        ExecutionMode::Apply => "APPLY",
    };

    tracing::info!(
        "🚀 Starting provisioning for {} [{}]",
        config.hostname,
        mode_str
    );

    // Stage 0: Safety check
    tracing::info!("📍 Stage 0: Safety Check");
    let safe = execute_action(&CheckRaspberryPiAction, hardware, mode, &log_tx).await?;

    // Stage 1: Validate hardware
    tracing::info!("📍 Stage 1: Hardware Validation");
    let validated = execute_action(
        &ValidateHardwareAction {
            config: config.clone(),
        },
        safe,
        mode,
        &log_tx,
    )
    .await?;

    // Stage 2: Partition drives
    tracing::info!("📍 Stage 2: Partition Drives");
    let partitioned = execute_action(&PartitionDrivesAction, validated, mode, &log_tx).await?;

    // Stage 3: Format partitions
    tracing::info!("📍 Stage 3: Format Partitions");
    let formatted = execute_action(&FormatPartitionsAction, partitioned, mode, &log_tx).await?;

    // Stage 4: Install system
    tracing::info!("📍 Stage 4: Install System");
    let installed = execute_action(&InstallSystemAction, formatted, mode, &log_tx).await?;

    // Stage 5: Configure system
    tracing::info!("📍 Stage 5: Configure System");
    let configured = execute_action(&ConfigureSystemAction, installed, mode, &log_tx).await?;

    // Stage 6: Finalize
    tracing::info!("📍 Stage 6: Finalize Provisioning");
    let provisioned =
        execute_action(&FinalizeProvisioningAction, configured, mode, &log_tx).await?;

    tracing::info!("✅ Provisioning pipeline complete!");
    tracing::info!("Summary:");
    tracing::info!("  Hostname: {}", provisioned.summary.hostname);
    tracing::info!("  Unit Type: {}", provisioned.summary.unit_type);
    tracing::info!("  Primary Drive: {}", provisioned.summary.primary_drive);
    if let Some(secondary) = &provisioned.summary.secondary_drive {
        tracing::info!("  Secondary Drive: {}", secondary);
    }
    tracing::info!(
        "  Total Partitions: {}",
        provisioned.summary.total_partitions
    );

    Ok(provisioned)
}
