// bases/beacon/src/provisioning.rs
//! Main provisioning orchestrator
//!
//! This file contains the provision_system function that chains all
//! the provisioning actions together in a type-safe way.

use crate::actions::{execute_action, ExecutionMode};
use crate::error::Result;
use crate::hardware::HardwareInfo;
use crate::provisioning::{
    CheckRaspberryPiAction, ConfigureSystemAction, FinalizeProvisioningAction,
    FormatPartitionsAction, InstallSystemAction, PartitionDrivesAction, ProvisionConfig,
    ProvisionedSystem, ValidateHardwareAction,
};
use tokio::sync::broadcast;

/// Provision a system through all stages
///
/// This is the main entry point for provisioning. It executes all stages
/// in order, with the type system enforcing that stages cannot be skipped
/// or executed out of order.
///
/// # Type-Safe Pipeline
///
/// ```text
/// HardwareInfo
///     |
///     | CheckRaspberryPiAction
///     ↓
/// SafeHardware
///     |
///     | ValidateHardwareAction
///     ↓
/// ValidatedHardware
///     |
///     | PartitionDrivesAction
///     ↓
/// PartitionedDrives
///     |
///     | FormatPartitionsAction
///     ↓
/// FormattedSystem
///     |
///     | InstallSystemAction
///     ↓
/// InstalledSystem
///     |
///     | ConfigureSystemAction
///     ↓
/// ConfiguredSystem
///     |
///     | FinalizeProvisioningAction
///     ↓
/// ProvisionedSystem
/// ```
///
/// # Arguments
///
/// * `config` - Provisioning configuration (hostname, unit type, etc.)
/// * `hardware` - Detected hardware information
/// * `mode` - Execution mode (DRY RUN or APPLY)
/// * `log_tx` - Broadcast channel for sending log messages
///
/// # Returns
///
/// Returns `Ok(ProvisionedSystem)` on success.
///
/// # Errors
///
/// Returns an error if:
/// - Not running on Raspberry Pi (in APPLY mode)
/// - Hardware validation fails
/// - Any stage fails to execute
///
/// # Example
///
/// ```ignore
/// use beacon::actions::ExecutionMode;
/// use beacon::hardware::detect_hardware;
/// use beacon::provisioning::{provision_system, ProvisionConfig, UnitType};
/// use tokio::sync::broadcast;
///
/// let config = ProvisionConfig {
///     hostname: "mdma-909-living-room".to_string(),
///     unit_type: UnitType::Mdma909,
///     wifi_config: None,
/// };
///
/// let hardware = detect_hardware().await?;
/// let (log_tx, _) = broadcast::channel(100);
///
/// let result = provision_system(
///     config,
///     hardware,
///     ExecutionMode::DryRun,
///     log_tx,
/// ).await?;
///
/// println!("Provisioning complete! Hostname: {}", result.summary.hostname);
/// ```
pub async fn provision_system(
    config: ProvisionConfig,
    hardware: HardwareInfo,
    mode: ExecutionMode,
    log_tx: broadcast::Sender<String>,
) -> Result<ProvisionedSystem> {
    // Log what we're doing
    let mode_str = match mode {
        ExecutionMode::DryRun => "DRY RUN",
        ExecutionMode::Apply => "APPLY",
    };

    tracing::info!(
        "🚀 Starting provisioning for {} [{}]",
        config.hostname,
        mode_str
    );

    // ========================================================================
    // The Beautiful Type-Safe Chain
    // ========================================================================
    //
    // Each action consumes the previous stage's output as its input.
    // The compiler ensures we can't skip stages or call them out of order!
    // ========================================================================

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

    // ========================================================================
    // Done!
    // ========================================================================

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
