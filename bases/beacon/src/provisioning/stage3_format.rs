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
        "Format partitions (FAT32 for boot, ext4 for others)".to_string()
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
                format_partitions(partitions).await?;
            }
            crate::provisioning::types::CompletedPartitionPlan::DualDrive {
                primary_partitions,
                secondary_partitions,
                ..
            } => {
                format_partitions(primary_partitions).await?;
                format_partitions(secondary_partitions).await?;
            }
        }

        tracing::info!("Format stage complete");
        Ok(planned_output.clone())
    }
}

async fn format_partitions(partitions: &[super::types::Partition]) -> Result<()> {
    for partition in partitions {
        format_partition(partition).await?;
    }
    Ok(())
}

async fn format_partition(partition: &super::types::Partition) -> Result<()> {
    use super::types::FilesystemType;
    use tokio::process::Command;

    let fs_type = partition.filesystem_type;
    let device = partition.device.as_str();
    let label = partition.label.as_str();

    tracing::info!(
        "Formatting {} as {} with label '{}'",
        device,
        fs_type,
        label
    );

    let output = match fs_type {
        FilesystemType::Fat32 => {
            // mkfs.vfat -F 32 -n LABEL /dev/device
            Command::new("mkfs.vfat")
                .arg("-F")
                .arg("32")
                .arg("-n")
                .arg(label)
                .arg(device)
                .output()
                .await
                .map_err(|e| crate::error::BeaconError::command_failed("mkfs.vfat", e))?
        }
        FilesystemType::Ext4 => {
            // mkfs.ext4 -L LABEL /dev/device
            Command::new("mkfs.ext4")
                .arg("-L")
                .arg(label)
                .arg(device)
                .output()
                .await
                .map_err(|e| crate::error::BeaconError::command_failed("mkfs.ext4", e))?
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(crate::error::BeaconError::Formatting {
            partition: device.to_string(),
            reason: stderr.to_string(),
        });
    }

    tracing::info!("Successfully formatted {} as {}", device, fs_type);
    Ok(())
}
