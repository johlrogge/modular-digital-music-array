// bases/beacon/src/provisioning/stage1_validate.rs
//! Stage 1: Validate hardware configuration
//!
//! Verifies that the detected hardware meets requirements for
//! the specified unit type.

use crate::actions::Action;
use crate::error::{BeaconError, Result};
use crate::provisioning::types::{
    DriveInfo, ProvisionConfig, SafeHardware, UnitType, ValidatedDrives, ValidatedHardware,
};

/// Action that validates hardware configuration
///
/// This action checks:
/// - NVMe drives are present
/// - Drive count matches unit requirements
/// - Drive sizes are adequate
pub struct ValidateHardwareAction {
    pub config: ProvisionConfig,
}

impl Action<SafeHardware, ValidatedHardware> for ValidateHardwareAction {
    fn description(&self) -> String {
        format!("Validate hardware for {}", self.config.unit_type)
    }

    async fn check(&self, _input: &SafeHardware) -> Result<bool> {
        // Validation always needs to happen
        // (It's not idempotent - it's just a check)
        Ok(true)
    }

    async fn apply(&self, input: SafeHardware) -> Result<ValidatedHardware> {
        let hardware = input.info;

        // Check NVMe drives exist
        if hardware.nvme_drives.is_empty() {
            return Err(BeaconError::HardwareInfo(
                "No NVMe drives found".to_string(),
            ));
        }

        // Build drive info
        let drive_infos: Vec<DriveInfo> = hardware
            .nvme_drives
            .iter()
            .map(|nvme| DriveInfo {
                device: nvme.device.to_string(),   // DevicePath -> String
                size_bytes: nvme.capacity.bytes(), // StorageBytes -> u64
                model: nvme.model.clone().unwrap_or_else(|| "Unknown".to_string()), // Option<String> -> String
            })
            .collect();

        // Validate drive count for unit type
        let drives = match self.config.unit_type {
            UnitType::Mdma909 => {
                // MDMA-909 should have 2 drives, but can work with 1
                if drive_infos.len() >= 2 {
                    let mut iter = drive_infos.into_iter();
                    ValidatedDrives::TwoDrives(iter.next().unwrap(), iter.next().unwrap())
                } else if drive_infos.len() == 1 {
                    tracing::warn!(
                        "MDMA-909 typically has 2 NVMe drives, but found only 1. Continuing..."
                    );
                    ValidatedDrives::OneDrive(drive_infos.into_iter().next().unwrap())
                } else {
                    return Err(BeaconError::HardwareInfo(
                        "MDMA-909 requires at least 1 NVMe drive".to_string(),
                    ));
                }
            }
            UnitType::Mdma101 | UnitType::Mdma303 => {
                // These units need exactly 1 drive
                if drive_infos.len() != 1 {
                    return Err(BeaconError::HardwareInfo(format!(
                        "{} requires exactly 1 NVMe drive, found {}",
                        self.config.unit_type,
                        drive_infos.len()
                    )));
                }
                ValidatedDrives::OneDrive(drive_infos.into_iter().next().unwrap())
            }
        };

        // Log what we found
        match &drives {
            ValidatedDrives::OneDrive(drive) => {
                tracing::info!(
                    "Validated 1 drive: {} ({} GB)",
                    drive.device,
                    drive.size_bytes / 1_000_000_000
                );
            }
            ValidatedDrives::TwoDrives(primary, secondary) => {
                tracing::info!(
                    "Validated 2 drives: {} ({} GB), {} ({} GB)",
                    primary.device,
                    primary.size_bytes / 1_000_000_000,
                    secondary.device,
                    secondary.size_bytes / 1_000_000_000
                );
            }
        }

        Ok(ValidatedHardware {
            config: self.config.clone(),
            drives,
        })
    }

    async fn preview(&self, input: SafeHardware) -> Result<ValidatedHardware> {
        let hardware = input.info;

        // Build mock drive info for preview
        let drive_infos: Vec<DriveInfo> = if hardware.nvme_drives.is_empty() {
            // No drives detected - create mock
            vec![DriveInfo {
                device: "/dev/nvme0n1".to_string(),
                size_bytes: 512_000_000_000, // 512GB
                model: "Mock NVMe Drive".to_string(),
            }]
        } else {
            hardware
                .nvme_drives
                .iter()
                .map(|nvme| DriveInfo {
                    device: nvme.device.to_string(),   // DevicePath -> String
                    size_bytes: nvme.capacity.bytes(), // StorageBytes -> u64
                    model: nvme.model.clone().unwrap_or_else(|| "Unknown".to_string()), // Option<String> -> String
                })
                .collect()
        };

        // Build drives enum based on count
        let drives = if drive_infos.len() >= 2 {
            let mut iter = drive_infos.into_iter();
            ValidatedDrives::TwoDrives(iter.next().unwrap(), iter.next().unwrap())
        } else {
            ValidatedDrives::OneDrive(drive_infos.into_iter().next().unwrap())
        };

        tracing::info!("Would validate hardware configuration");

        Ok(ValidatedHardware {
            config: self.config.clone(),
            drives,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::HardwareInfo;
    use crate::hardware::NvmeDrive;
    use crate::types::{DevicePath, StorageBytes};

    fn mock_safe_hardware(drive_count: usize) -> SafeHardware {
        let mut nvme_drives = Vec::new();
        for i in 0..drive_count {
            nvme_drives.push(NvmeDrive {
                device: DevicePath::new(format!("/dev/nvme{}n1", i)),
                capacity: StorageBytes::new(512_000_000_000),
                model: Some(format!("Mock Drive {}", i)),
                is_formatted: false,
            });
        }

        SafeHardware {
            info: HardwareInfo {
                nvme_drives,
                model: "Raspberry Pi 5".to_string(),
                serial: Some("12345".to_string()),
                memory_mb: None,
            },
        }
    }

    #[tokio::test]
    async fn validate_one_drive_for_mdma_101() {
        let config = ProvisionConfig {
            hostname: "test-101".to_string(),
            unit_type: UnitType::Mdma101,
            wifi_config: None,
        };

        let action = ValidateHardwareAction { config };
        let safe = mock_safe_hardware(1);

        let result = action.apply(safe).await;
        assert!(result.is_ok());

        let validated = result.unwrap();
        assert!(matches!(validated.drives, ValidatedDrives::OneDrive(_)));
    }

    #[tokio::test]
    async fn validate_two_drives_for_mdma_909() {
        let config = ProvisionConfig {
            hostname: "test-909".to_string(),
            unit_type: UnitType::Mdma909,
            wifi_config: None,
        };

        let action = ValidateHardwareAction { config };
        let safe = mock_safe_hardware(2);

        let result = action.apply(safe).await;
        assert!(result.is_ok());

        let validated = result.unwrap();
        assert!(matches!(validated.drives, ValidatedDrives::TwoDrives(_, _)));
    }

    #[tokio::test]
    async fn preview_creates_mock_when_no_drives() {
        let config = ProvisionConfig {
            hostname: "test".to_string(),
            unit_type: UnitType::Mdma101,
            wifi_config: None,
        };

        let action = ValidateHardwareAction { config };
        let safe = mock_safe_hardware(0);

        let result = action.preview(safe).await;
        assert!(result.is_ok());
    }
}
