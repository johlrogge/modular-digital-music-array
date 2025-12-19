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

    async fn apply(&self, input: PartitionedDrives) -> Result<FormattedSystem> {
        // Stub: Would use mkfs.ext4 here
        tracing::info!("Would format partitions here");

        Ok(FormattedSystem { partitioned: input })
    }
}
