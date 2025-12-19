// bases/beacon/src/provisioning/stage2_partition.rs
//! Stage 2: Partition NVMe drives

use crate::actions::{Action, ActionId, PlannedAction};
use crate::error::Result;
use crate::provisioning::types::{
    Partition, PartitionLayout, PartitionedDrives, ValidatedHardware,
};

#[derive(Clone, Debug)]
pub struct PartitionDrivesAction;

impl Action<ValidatedHardware, PartitionedDrives> for PartitionDrivesAction {
    fn id(&self) -> ActionId {
        ActionId::new("partition-drives")
    }

    fn description(&self) -> String {
        "Partition NVMe drives".to_string()
    }

    async fn plan(
        &self,
        input: &ValidatedHardware,
    ) -> Result<PlannedAction<ValidatedHardware, PartitionedDrives, Self>> {
        // Build partition layout
        let primary_device = input.drives.primary().device.clone();
        let partitions = vec![
            Partition {
                device: format!("{}p1", primary_device),
                mount_point: "/",
                label: "root",
                size_description: "16GB",
            },
            Partition {
                device: format!("{}p2", primary_device),
                mount_point: "/var",
                label: "var",
                size_description: "8GB",
            },
        ];

        let layout = PartitionLayout::SingleDrive {
            device: primary_device,
            partitions,
        };

        let assumed_output = PartitionedDrives {
            validated: input.clone(),
            layout,
        };

        Ok(PlannedAction {
            description: self.description(),
            action: self.clone(),
            input: input.clone(),
            assumed_output,
        })
    }

    async fn apply(&self, input: ValidatedHardware) -> Result<PartitionedDrives> {
        // Stub: In real implementation, would use parted/gdisk
        tracing::info!("Would partition {} here", input.drives.primary().device);

        let primary_device = input.drives.primary().device.clone();
        let partitions = vec![
            Partition {
                device: format!("{}p1", primary_device),
                mount_point: "/",
                label: "root",
                size_description: "16GB",
            },
            Partition {
                device: format!("{}p2", primary_device),
                mount_point: "/var",
                label: "var",
                size_description: "8GB",
            },
        ];

        let layout = PartitionLayout::SingleDrive {
            device: primary_device,
            partitions,
        };

        Ok(PartitionedDrives {
            validated: input,
            layout,
        })
    }
}
