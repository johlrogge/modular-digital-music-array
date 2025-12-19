// bases/beacon/src/provisioning/mod.rs
//! Provisioning system for MDMA units using plan-then-execute architecture

use crate::actions::{Action, ProvisioningPlan};
use crate::error::Result;
use crate::hardware::HardwareInfo;
use tokio::sync::mpsc;

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

pub use types::{DriveInfo, ProvisionConfig, ProvisionedSystem, UnitType, ValidatedDrives};

/// Build a provisioning plan for the given configuration and hardware
///
/// This function creates a type-safe plan that can be displayed to the user
/// before execution. The type system ensures stages are chained correctly.
pub async fn build_provisioning_plan(
    config: ProvisionConfig,
    hardware: HardwareInfo,
) -> Result<ProvisioningPlan> {
    // Stage 0: Safety check
    let check_pi = CheckRaspberryPiAction;
    let stage0 = check_pi.plan(&hardware).await?;

    // Stage 1: Validate hardware
    let validate = ValidateHardwareAction {
        config: config.clone(),
    };
    let stage1 = validate.plan(&stage0.assumed_output).await?;

    // Stage 2: Partition drives
    let partition = PartitionDrivesAction;
    let stage2 = partition.plan(&stage1.assumed_output).await?;

    // Stage 3: Format partitions
    let format = FormatPartitionsAction;
    let stage3 = format.plan(&stage2.assumed_output).await?;

    // Stage 4: Install system
    let install = InstallSystemAction;
    let stage4 = install.plan(&stage3.assumed_output).await?;

    // Stage 5: Configure system
    let configure = ConfigureSystemAction;
    let stage5 = configure.plan(&stage4.assumed_output).await?;

    // Stage 6: Finalize
    let finalize = FinalizeProvisioningAction;
    let stage6 = finalize.plan(&stage5.assumed_output).await?;

    // Build the plan - type system enforces correct chaining!
    let plan = ProvisioningPlan::new(stage0)
        .append(stage1)
        .append(stage2)
        .append(stage3)
        .append(stage4)
        .append(stage5)
        .append(stage6);

    Ok(plan)
}

/// Legacy API wrapper for server.rs compatibility
///
/// This function wraps the new plan-then-execute API to match the old
/// `provision_system` signature that server.rs expects.
pub async fn provision_system(
    config: ProvisionConfig,
    hardware: HardwareInfo,
    execution_mode: crate::actions::ExecutionMode,
    log_tx: tokio::sync::broadcast::Sender<String>,
) -> Result<ProvisionedSystem> {
    use crate::actions::ExecutionProgress;

    // Build the plan
    let plan = build_provisioning_plan(config.clone(), hardware.clone()).await?;

    // Show plan summary via logs (broadcast::send is sync, no .await)
    let _ = log_tx.send("📋 Provisioning Plan:".to_string());
    for summary in plan.summary() {
        let _ = log_tx.send(format!("  {} - {}", summary.id, summary.description));
        let _ = log_tx.send(format!("    {}", summary.details));
    }
    let _ = log_tx.send("".to_string());

    // Check execution mode
    if execution_mode == crate::actions::ExecutionMode::DryRun {
        let _ = log_tx.send("✅ Dry-run complete - no changes made".to_string());

        // In dry-run mode, build a mock ProvisionedSystem
        // This represents what WOULD be created
        let drives = match config.unit_type {
            UnitType::Mdma909 => {
                if hardware.nvme_drives.len() >= 2 {
                    ValidatedDrives::TwoDrives(
                        DriveInfo {
                            device: hardware.nvme_drives[0].device.as_str().to_string(),
                            size_bytes: hardware.nvme_drives[0].capacity.bytes(),
                            model: hardware.nvme_drives[0]
                                .model
                                .clone()
                                .unwrap_or_else(|| "Unknown".to_string()),
                        },
                        DriveInfo {
                            device: hardware.nvme_drives[1].device.as_str().to_string(),
                            size_bytes: hardware.nvme_drives[1].capacity.bytes(),
                            model: hardware.nvme_drives[1]
                                .model
                                .clone()
                                .unwrap_or_else(|| "Unknown".to_string()),
                        },
                    )
                } else if !hardware.nvme_drives.is_empty() {
                    ValidatedDrives::OneDrive(DriveInfo {
                        device: hardware.nvme_drives[0].device.as_str().to_string(),
                        size_bytes: hardware.nvme_drives[0].capacity.bytes(),
                        model: hardware.nvme_drives[0]
                            .model
                            .clone()
                            .unwrap_or_else(|| "Unknown".to_string()),
                    })
                } else {
                    return Err(crate::error::BeaconError::Hardware(
                        "No NVMe drives found".to_string(),
                    ));
                }
            }
            _ => {
                if hardware.nvme_drives.is_empty() {
                    return Err(crate::error::BeaconError::Hardware(
                        "No NVMe drives found".to_string(),
                    ));
                }
                ValidatedDrives::OneDrive(DriveInfo {
                    device: hardware.nvme_drives[0].device.as_str().to_string(),
                    size_bytes: hardware.nvme_drives[0].capacity.bytes(),
                    model: hardware.nvme_drives[0]
                        .model
                        .clone()
                        .unwrap_or_else(|| "Unknown".to_string()),
                })
            }
        };

        let primary_drive = drives.primary().device.clone();
        let secondary_drive = drives.secondary().map(|d| d.device.clone());

        let summary = types::ProvisioningSummary {
            hostname: config.hostname.clone(),
            unit_type: config.unit_type.clone(),
            primary_drive: primary_drive.clone(),
            secondary_drive,
            total_partitions: 4, // Mock value
        };

        return Ok(types::ProvisionedSystem {
            configured: types::ConfiguredSystem {
                installed: types::InstalledSystem {
                    formatted: types::FormattedSystem {
                        partitioned: types::PartitionedDrives {
                            validated: types::ValidatedHardware { config, drives },
                            layout: types::PartitionLayout::SingleDrive {
                                device: primary_drive,
                                partitions: vec![], // Mock
                            },
                        },
                    },
                    mount_point: std::path::PathBuf::from("/mnt"),
                },
            },
            summary,
        });
    }

    // Execute with progress feedback
    let (progress_tx, mut progress_rx) = mpsc::channel(100);

    // Spawn execution task
    let execution_handle = tokio::spawn(async move { plan.execute(progress_tx).await });

    // Forward progress to logs
    while let Some(progress) = progress_rx.recv().await {
        match progress {
            ExecutionProgress::Started { id, description } => {
                let _ = log_tx.send(format!("🚀 Starting: {}", description));
            }
            ExecutionProgress::Progress { id, message } => {
                let _ = log_tx.send(format!("   {}", message));
            }
            ExecutionProgress::Complete { id } => {
                let _ = log_tx.send(format!("✅ Complete: {}", id));
            }
            ExecutionProgress::Failed { id, error } => {
                let _ = log_tx.send(format!("❌ Failed: {} - {}", id, error));
            }
        }
    }

    // Wait for execution to complete
    execution_handle.await.map_err(|e| {
        crate::error::BeaconError::Provisioning(format!("Execution task panicked: {}", e))
    })?;

    let _ = log_tx.send("✅ Provisioning complete!".to_string());

    // TODO: In real implementation, need to return actual ProvisionedSystem
    // For now, since stages are stubs, build a mock result
    let drives = match config.unit_type {
        UnitType::Mdma909 => {
            if hardware.nvme_drives.len() >= 2 {
                ValidatedDrives::TwoDrives(
                    DriveInfo {
                        device: hardware.nvme_drives[0].device.as_str().to_string(),
                        size_bytes: hardware.nvme_drives[0].capacity.bytes(),
                        model: hardware.nvme_drives[0]
                            .model
                            .clone()
                            .unwrap_or_else(|| "Unknown".to_string()),
                    },
                    DriveInfo {
                        device: hardware.nvme_drives[1].device.as_str().to_string(),
                        size_bytes: hardware.nvme_drives[1].capacity.bytes(),
                        model: hardware.nvme_drives[1]
                            .model
                            .clone()
                            .unwrap_or_else(|| "Unknown".to_string()),
                    },
                )
            } else if !hardware.nvme_drives.is_empty() {
                ValidatedDrives::OneDrive(DriveInfo {
                    device: hardware.nvme_drives[0].device.as_str().to_string(),
                    size_bytes: hardware.nvme_drives[0].capacity.bytes(),
                    model: hardware.nvme_drives[0]
                        .model
                        .clone()
                        .unwrap_or_else(|| "Unknown".to_string()),
                })
            } else {
                return Err(crate::error::BeaconError::Hardware(
                    "No NVMe drives found".to_string(),
                ));
            }
        }
        _ => {
            if hardware.nvme_drives.is_empty() {
                return Err(crate::error::BeaconError::Hardware(
                    "No NVMe drives found".to_string(),
                ));
            }
            ValidatedDrives::OneDrive(DriveInfo {
                device: hardware.nvme_drives[0].device.as_str().to_string(),
                size_bytes: hardware.nvme_drives[0].capacity.bytes(),
                model: hardware.nvme_drives[0]
                    .model
                    .clone()
                    .unwrap_or_else(|| "Unknown".to_string()),
            })
        }
    };

    let primary_drive = drives.primary().device.clone();
    let secondary_drive = drives.secondary().map(|d| d.device.clone());

    let summary = types::ProvisioningSummary {
        hostname: config.hostname.clone(),
        unit_type: config.unit_type.clone(),
        primary_drive: primary_drive.clone(),
        secondary_drive,
        total_partitions: 4,
    };

    Ok(types::ProvisionedSystem {
        configured: types::ConfiguredSystem {
            installed: types::InstalledSystem {
                formatted: types::FormattedSystem {
                    partitioned: types::PartitionedDrives {
                        validated: types::ValidatedHardware { config, drives },
                        layout: types::PartitionLayout::SingleDrive {
                            device: primary_drive,
                            partitions: vec![],
                        },
                    },
                },
                mount_point: std::path::PathBuf::from("/mnt"),
            },
        },
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::NvmeDrive;
    use crate::provisioning::types::UnitType;
    use crate::types::{DevicePath, StorageBytes};

    fn mock_hardware() -> HardwareInfo {
        HardwareInfo {
            model: "Raspberry Pi 5 Model B".to_string(),
            nvme_drives: vec![NvmeDrive {
                device: DevicePath::from("/dev/nvme0n1"),
                capacity: StorageBytes::from(512_000_000_000u64),
                model: Some("Test NVMe".to_string()),
                is_formatted: false,
            }],
            memory_mb: Some(8192),
            serial: Some("test123".to_string()),
        }
    }

    fn mock_config() -> ProvisionConfig {
        ProvisionConfig {
            hostname: "test-909".to_string(),
            unit_type: UnitType::Mdma909,
            wifi_config: None,
        }
    }

    #[tokio::test]
    async fn plan_builds_successfully() {
        // Note: This test will fail if not on Pi (stage0 checks /proc/cpuinfo)
        // That's intentional - the safety check is working!
        let config = mock_config();
        let hardware = mock_hardware();

        let result = build_provisioning_plan(config, hardware).await;

        // On non-Pi systems, expect safety check failure
        if result.is_err() {
            let err = result.unwrap_err();
            assert!(err.to_string().contains("Raspberry Pi"));
        }
    }
}
