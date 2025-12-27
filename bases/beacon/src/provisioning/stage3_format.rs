// bases/beacon/src/provisioning/stage3_format.rs
//! Stage 3: Format partitions

use crate::actions::{Action, ActionId, PlannedAction};
use crate::error::Result;
use crate::provisioning::types::{FormattedSystem, PartitionedDrives};

#[derive(Clone, Debug)]
pub struct FormatPartitionsAction;

impl Action<PartitionedDrives, FormattedSystem> for FormatPartitionsAction {
    fn id(&self) -> ActionId {
        ActionId::new("format-partitions")
    }

    fn description(&self) -> String {
        "Format partitions with ext4".to_string()
    }

    async fn plan(
        &self,
        input: &PartitionedDrives,
    ) -> Result<PlannedAction<PartitionedDrives, FormattedSystem, Self>> {
        let assumed_output = FormattedSystem {
            partitioned: input.clone(),
        };

        Ok(PlannedAction {
            description: self.description(),
            action: self.clone(),
            input: input.clone(),
            assumed_output,
        })
    }

    async fn apply(&self, planned_output: &FormattedSystem) -> Result<FormattedSystem> {
        tracing::info!("Stage 3: Format partitions - executing plan");

        // Format all partitions from the plan
        match &planned_output.partitioned.plan {
            crate::provisioning::types::PartitionPlan::SingleDrive { partitions, .. } => {
                for partition in partitions {
                    tracing::info!(
                        "Would execute: mkfs.ext4 -L {} {}",
                        partition.label,
                        partition.device
                    );
                    // TODO: Real implementation:
                    // Command::new("mkfs.ext4")
                    //     .arg("-L")
                    //     .arg(partition.label.0)
                    //     .arg(&partition.device.0)
                    //     .output()?;
                }
            }
            crate::provisioning::types::PartitionPlan::DualDrive {
                primary_partitions,
                secondary_partitions,
                ..
            } => {
                for partition in primary_partitions {
                    tracing::info!(
                        "Would execute: mkfs.ext4 -L {} {}",
                        partition.label,
                        partition.device
                    );
                }
                for partition in secondary_partitions {
                    tracing::info!(
                        "Would execute: mkfs.ext4 -L {} {}",
                        partition.label,
                        partition.device
                    );
                }
            }
        }

        tracing::info!("Format stage complete (simulated)");
        Ok(planned_output.clone())
    }
}
