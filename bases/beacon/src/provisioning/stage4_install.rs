// bases/beacon/src/provisioning/stage4_install.rs
//! Stage 4: Install base system

use crate::actions::{Action, ActionId, PlannedAction};
use crate::error::Result;
use crate::provisioning::types::{FormattedSystem, InstalledSystem};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct InstallSystemAction;

impl Action<FormattedSystem, InstalledSystem> for InstallSystemAction {
    fn id(&self) -> ActionId {
        ActionId::new("install-system")
    }

    fn description(&self) -> String {
        "Install base operating system".to_string()
    }

    async fn plan(
        &self,
        input: &FormattedSystem,
    ) -> Result<PlannedAction<FormattedSystem, InstalledSystem, Self>> {
        let assumed_output = InstalledSystem {
            formatted: input.clone(),
        };

        Ok(PlannedAction {
            description: self.description(),
            action: self.clone(),
            input: input.clone(),
            assumed_output,
        })
    }

    async fn apply(&self, planned_output: &InstalledSystem) -> Result<InstalledSystem> {
        tracing::info!("Stage 4: Install base system - executing plan");

        // Mount filesystems from the plan
        let mount_point = match &planned_output.formatted.partitioned.plan {
            crate::provisioning::types::PartitionPlan::SingleDrive { device, partitions } => {
                for partition in partitions {
                    let target_path = partition.device.join(partition.mount_point);
                    tracing::info!(
                        "Would execute: mkdir -p {} && mount {} {}",
                        target_path.display(),
                        partition.device,
                        target_path.display()
                    );
                    // TODO: Real implementation:
                    // std::fs::create_dir_all(&target_path)?;
                    // Command::new("mount")
                    //     .arg(&partition.device.0)
                    //     .arg(&target_path)
                    //     .output()?;
                }
                device.device.clone()
            }
            crate::provisioning::types::PartitionPlan::DualDrive {
                primary_device,
                primary_partitions,
                secondary_partitions,
                ..
            } => {
                for partition in primary_partitions {
                    let target_path = partition.device.join(partition.mount_point);
                    tracing::info!(
                        "Would execute: mkdir -p {} && mount {} {}",
                        target_path.display(),
                        partition.device,
                        target_path.display()
                    );
                }
                for partition in secondary_partitions {
                    let target_path = partition.device.join(partition.mount_point);
                    tracing::info!(
                        "Would execute: mkdir -p {} && mount {} {}",
                        target_path.display(),
                        partition.device,
                        target_path.display()
                    );
                }
                primary_device.device.clone()
            }
        };

        tracing::info!("Would execute: xbps-install -Sy -R https://repo-default.voidlinux.org/current -r {} base-system",
            mount_point
        );
        // TODO: Real implementation:
        // Command::new("xbps-install")
        //     .arg("-Sy")
        //     .arg("-R").arg("https://repo-default.voidlinux.org/current")
        //     .arg("-r").arg(mount_point)
        //     .arg("base-system")
        //     .output()?;

        tracing::info!("Install stage complete (simulated)");
        Ok(planned_output.clone())
    }
}
