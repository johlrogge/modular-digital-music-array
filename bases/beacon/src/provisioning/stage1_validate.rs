// bases/beacon/src/provisioning/stage1_validate.rs
//! Stage 1: Validate hardware configuration

use crate::actions::{Action, ActionId, PlannedAction};
use crate::error::{BeaconError, Result};
use crate::provisioning::types::{
    DriveInfo, ProvisionConfig, SafeHardware, UnitType, ValidatedDrives, ValidatedHardware,
};

#[derive(Clone, Debug)]
pub struct ValidateHardwareAction {
    pub config: ProvisionConfig,
}

impl ValidateHardwareAction {
    fn validate_drives(&self, drives: &[DriveInfo]) -> Result<ValidatedDrives> {
        match self.config.unit_type {
            UnitType::Mdma909 => {
                // 909 requires 1-2 drives
                match drives.len() {
                    1 => Ok(ValidatedDrives::OneDrive(drives[0].clone())),
                    2 => Ok(ValidatedDrives::TwoDrives(
                        drives[0].clone(),
                        drives[1].clone(),
                    )),
                    n => Err(BeaconError::Hardware(format!(
                        "MDMA-909 requires 1-2 NVMe drives, found {}",
                        n
                    ))),
                }
            }
            UnitType::Mdma101 | UnitType::Mdma303 => {
                // 101/303 require exactly 1 drive
                if drives.len() != 1 {
                    Err(BeaconError::Hardware(format!(
                        "{} requires exactly 1 NVMe drive, found {}",
                        self.config.unit_type,
                        drives.len()
                    )))
                } else {
                    Ok(ValidatedDrives::OneDrive(drives[0].clone()))
                }
            }
        }
    }

    fn build_drive_infos(safe: &SafeHardware) -> Vec<DriveInfo> {
        safe.info
            .nvme_drives
            .iter()
            .map(|nvme| DriveInfo {
                device: nvme.device.clone(),
                size_bytes: nvme.capacity,
                model: nvme.model.clone().unwrap_or_else(|| "Unknown".to_string()),
            })
            .collect()
    }
}

impl Action<SafeHardware, ValidatedHardware> for ValidateHardwareAction {
    fn id(&self) -> ActionId {
        ActionId::new("validate-hardware")
    }

    fn description(&self) -> String {
        format!("Validate hardware for {}", self.config.unit_type)
    }

    async fn plan(
        &self,
        input: &SafeHardware,
    ) -> Result<PlannedAction<SafeHardware, ValidatedHardware, Self>> {
        let drive_infos = Self::build_drive_infos(input);
        let drives = self.validate_drives(&drive_infos)?;

        let assumed_output = ValidatedHardware {
            config: self.config.clone(),
            drives,
        };

        Ok(PlannedAction {
            description: self.description(),
            action: self.clone(),
            input: input.clone(),
            assumed_output,
        })
    }

    async fn apply(&self, planned_output: &ValidatedHardware) -> Result<ValidatedHardware> {
        tracing::info!("Stage 1: Validate hardware - executing plan");

        // The validation already happened in plan()
        // Just log what was validated
        match &planned_output.drives {
            ValidatedDrives::OneDrive(drive) => {
                tracing::info!("Validated 1 drive: {} ({})", drive.device, drive.size_bytes);
            }
            ValidatedDrives::TwoDrives(primary, secondary) => {
                tracing::info!("Validated 2 drives:");
                tracing::info!("  Primary: {} ({})", primary.device, primary.size_bytes);
                tracing::info!(
                    "  Secondary: {} ({})",
                    secondary.device,
                    secondary.size_bytes
                );
            }
        }

        tracing::info!("Validate stage complete");
        Ok(planned_output.clone())
    }
}
