// bases/beacon/src/provisioning/stage3_format.rs
//! Stage 3: Format partitions

use crate::actions::{Action, ActionId, PlannedAction};
use crate::error::Result;
use crate::provisioning::types::{CompletedPartitionedDrives, FormattedSystem};

#[derive(Clone, Debug)]
pub struct FormatPartitionsAction;

impl Action<CompletedPartitionedDrives, FormattedSystem, FormattedSystem>
    for FormatPartitionsAction
{
    fn id(&self) -> ActionId {
        ActionId::new("format-partitions")
    }

    fn description(&self) -> String {
        "Format partitions with ext4".to_string()
    }

    async fn plan(
        &self,
        input: &CompletedPartitionedDrives,
    ) -> Result<PlannedAction<CompletedPartitionedDrives, FormattedSystem, FormattedSystem, Self>>
    {
        let assumed_output = FormattedSystem {
            partitioned: input.clone(),
        };

        Ok(PlannedAction {
            description: self.description(),
            action: self.clone(),
            input: input.clone(),
            planned_work: assumed_output.clone(),
            assumed_output,
        })
    }

    async fn apply(&self, planned_output: &FormattedSystem) -> Result<FormattedSystem> {
        tracing::info!("Stage 3: Format partitions - executing plan");

        // Format all partitions from the plan
        match &planned_output.partitioned.plan {
            crate::provisioning::types::CompletedPartitionPlan::SingleDrive {
                partitions, ..
            } => {
                format_partitions(partitions);
            }
            crate::provisioning::types::CompletedPartitionPlan::DualDrive {
                primary_partitions,
                secondary_partitions,
                ..
            } => {
                format_partitions(primary_partitions);
                format_partitions(secondary_partitions);
            }
        }

        tracing::info!("Format stage complete (simulated)");
        Ok(planned_output.clone())
    }
}

fn format_partitions(partitions: &Vec<super::types::Partition>) {
    for partition in partitions {
        tracing::info!(
            "Would execute: mkfs.ext4 -L {} {}",
            partition.label,
            partition.device
        )
    }
}
