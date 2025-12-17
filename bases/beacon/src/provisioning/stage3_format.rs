// bases/beacon/src/provisioning/stage3_format.rs
//! Stage 3: Format partitions
//!
//! Creates filesystems on the partitions.

use crate::actions::Action;
use crate::error::Result;
use crate::provisioning::types::{
    FormattedSystem, PartitionLayout, PartitionedDrives,
};

/// Action that formats partitions with ext4
pub struct FormatPartitionsAction;

impl Action<PartitionedDrives, FormattedSystem> for FormatPartitionsAction {
    fn description(&self) -> String {
        "Format partitions with ext4".to_string()
    }

    async fn check(&self, input: &PartitionedDrives) -> Result<bool> {
        // Check if partitions are already formatted
        // For now, simplified: check first partition
        let first_partition = match &input.layout {
            PartitionLayout::SingleDrive { partitions, .. } => &partitions[0].device,
            PartitionLayout::DualDrive {
                primary_partitions, ..
            } => &primary_partitions[0].device,
        };

        let output = tokio::process::Command::new("blkid")
            .arg(first_partition)
            .output()
            .await?;

        // If blkid returns empty, partition needs formatting
        Ok(output.stdout.is_empty())
    }

    async fn apply(&self, input: PartitionedDrives) -> Result<FormattedSystem> {
        tracing::info!("Formatting partitions...");

        // Format all partitions
        match &input.layout {
            PartitionLayout::SingleDrive { partitions, .. } => {
                for partition in partitions {
                    tracing::info!("  Formatting {} as ext4...", partition.device);

                    tokio::process::Command::new("mkfs.ext4")
                        .args(&["-F", "-L", partition.label, &partition.device])
                        .spawn()?
                        .wait()
                        .await?;
                }
            }
            PartitionLayout::DualDrive {
                primary_partitions,
                secondary_partitions,
                ..
            } => {
                for partition in primary_partitions {
                    tracing::info!("  Formatting {} as ext4...", partition.device);

                    tokio::process::Command::new("mkfs.ext4")
                        .args(&["-F", "-L", partition.label, &partition.device])
                        .spawn()?
                        .wait()
                        .await?;
                }

                for partition in secondary_partitions {
                    tracing::info!("  Formatting {} as ext4...", partition.device);

                    tokio::process::Command::new("mkfs.ext4")
                        .args(&["-F", "-L", partition.label, &partition.device])
                        .spawn()?
                        .wait()
                        .await?;
                }
            }
        }

        tracing::info!("✅ Formatting complete");

        Ok(FormattedSystem { partitioned: input })
    }

    async fn preview(&self, input: PartitionedDrives) -> Result<FormattedSystem> {
        tracing::info!("Would format partitions:");

        match &input.layout {
            PartitionLayout::SingleDrive { partitions, .. } => {
                for partition in partitions {
                    tracing::info!("  {} -> ext4 (label: {})", partition.device, partition.label);
                }
            }
            PartitionLayout::DualDrive {
                primary_partitions,
                secondary_partitions,
                ..
            } => {
                tracing::info!("  Primary drive:");
                for partition in primary_partitions {
                    tracing::info!("    {} -> ext4 (label: {})", partition.device, partition.label);
                }
                tracing::info!("  Secondary drive:");
                for partition in secondary_partitions {
                    tracing::info!("    {} -> ext4 (label: {})", partition.device, partition.label);
                }
            }
        }

        Ok(FormattedSystem { partitioned: input })
    }
}
