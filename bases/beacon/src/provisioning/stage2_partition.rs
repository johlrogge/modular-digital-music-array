// bases/beacon/src/provisioning/stage2_partition.rs
//! Stage 2: Partition NVMe drives
//!
//! Creates the partition layout for the system.

use crate::actions::Action;
use crate::error::Result;
use crate::provisioning::types::{
    Partition, PartitionLayout, PartitionedDrives, ValidatedDrives, ValidatedHardware,
};

/// Action that partitions NVMe drives
///
/// This action:
/// - Creates GPT partition tables
/// - Creates partitions for /, /var, /music, /metadata
/// - Optionally creates /cdj-export on secondary drive
pub struct PartitionDrivesAction;

impl Action<ValidatedHardware, PartitionedDrives> for PartitionDrivesAction {
    fn description(&self) -> String {
        "Partition NVMe drives".to_string()
    }

    async fn check(&self, input: &ValidatedHardware) -> Result<bool> {
        // Check if drives are already partitioned correctly
        let primary = input.drives.primary();

        // Try to read partition table
        let output = tokio::process::Command::new("parted")
            .args([&primary.device, "print"])
            .output()
            .await?;

        // Simple check: if we can read partitions, assume already done
        // TODO: More sophisticated checking of partition layout
        let stdout = String::from_utf8_lossy(&output.stdout);
        let has_partitions = stdout.contains("Number") && stdout.contains("Start");

        // Return true if we NEED to partition (i.e., NOT already partitioned)
        Ok(!has_partitions)
    }

    async fn apply(&self, input: ValidatedHardware) -> Result<PartitionedDrives> {
        let primary = input.drives.primary();

        tracing::info!("Creating GPT partition table on {}", primary.device);

        // Create GPT partition table
        tokio::process::Command::new("parted")
            .args([&primary.device, "-s", "mklabel", "gpt"])
            .spawn()?
            .wait()
            .await?;

        tracing::info!("Creating partitions on {}", primary.device);

        // Create root partition (16GB)
        tokio::process::Command::new("parted")
            .args([
                &primary.device,
                "-s",
                "mkpart",
                "root",
                "ext4",
                "1MiB",
                "16GiB",
            ])
            .spawn()?
            .wait()
            .await?;

        // Create /var partition (8GB)
        tokio::process::Command::new("parted")
            .args([
                &primary.device,
                "-s",
                "mkpart",
                "var",
                "ext4",
                "16GiB",
                "24GiB",
            ])
            .spawn()?
            .wait()
            .await?;

        // Create /music partition (400GB)
        tokio::process::Command::new("parted")
            .args([
                &primary.device,
                "-s",
                "mkpart",
                "music",
                "ext4",
                "24GiB",
                "424GiB",
            ])
            .spawn()?
            .wait()
            .await?;

        // Create /metadata partition (rest)
        tokio::process::Command::new("parted")
            .args([
                &primary.device,
                "-s",
                "mkpart",
                "metadata",
                "ext4",
                "424GiB",
                "100%",
            ])
            .spawn()?
            .wait()
            .await?;

        // Build partition layout
        let layout = match &input.drives {
            ValidatedDrives::OneDrive(drive) => PartitionLayout::SingleDrive {
                device: drive.device.clone(),
                partitions: vec![
                    Partition {
                        device: format!("{}p1", drive.device),
                        mount_point: "/",
                        label: "root",
                        size_description: "16GB",
                    },
                    Partition {
                        device: format!("{}p2", drive.device),
                        mount_point: "/var",
                        label: "var",
                        size_description: "8GB",
                    },
                    Partition {
                        device: format!("{}p3", drive.device),
                        mount_point: "/music",
                        label: "music",
                        size_description: "400GB",
                    },
                    Partition {
                        device: format!("{}p4", drive.device),
                        mount_point: "/metadata",
                        label: "metadata",
                        size_description: "~88GB",
                    },
                ],
            },
            ValidatedDrives::TwoDrives(primary_drive, secondary_drive) => {
                // Partition secondary drive for CDJ export
                tracing::info!(
                    "Creating partition on secondary drive {}",
                    secondary_drive.device
                );

                tokio::process::Command::new("parted")
                    .args([&secondary_drive.device, "-s", "mklabel", "gpt"])
                    .spawn()?
                    .wait()
                    .await?;

                tokio::process::Command::new("parted")
                    .args([
                        &secondary_drive.device,
                        "-s",
                        "mkpart",
                        "cdj-export",
                        "ext4",
                        "1MiB",
                        "100%",
                    ])
                    .spawn()?
                    .wait()
                    .await?;

                PartitionLayout::DualDrive {
                    primary_device: primary_drive.device.clone(),
                    primary_partitions: vec![
                        Partition {
                            device: format!("{}p1", primary_drive.device),
                            mount_point: "/",
                            label: "root",
                            size_description: "16GB",
                        },
                        Partition {
                            device: format!("{}p2", primary_drive.device),
                            mount_point: "/var",
                            label: "var",
                            size_description: "8GB",
                        },
                        Partition {
                            device: format!("{}p3", primary_drive.device),
                            mount_point: "/music",
                            label: "music",
                            size_description: "400GB",
                        },
                        Partition {
                            device: format!("{}p4", primary_drive.device),
                            mount_point: "/metadata",
                            label: "metadata",
                            size_description: "~88GB",
                        },
                    ],
                    secondary_device: secondary_drive.device.clone(),
                    secondary_partitions: vec![Partition {
                        device: format!("{}p1", secondary_drive.device),
                        mount_point: "/cdj-export",
                        label: "cdj-export",
                        size_description: "512GB",
                    }],
                }
            }
        };

        tracing::info!("✅ Partitioning complete");

        Ok(PartitionedDrives {
            validated: input,
            layout,
        })
    }

    async fn preview(&self, input: ValidatedHardware) -> Result<PartitionedDrives> {
        tracing::info!("Would create partition layout:");

        // Build preview layout
        let layout = match &input.drives {
            ValidatedDrives::OneDrive(drive) => {
                tracing::info!("  Single drive: {}", drive.device);
                tracing::info!("    Partition 1: / (16GB)");
                tracing::info!("    Partition 2: /var (8GB)");
                tracing::info!("    Partition 3: /music (400GB)");
                tracing::info!("    Partition 4: /metadata (~88GB)");

                PartitionLayout::SingleDrive {
                    device: drive.device.clone(),
                    partitions: vec![
                        Partition {
                            device: format!("{}p1", drive.device),
                            mount_point: "/",
                            label: "root",
                            size_description: "16GB",
                        },
                        Partition {
                            device: format!("{}p2", drive.device),
                            mount_point: "/var",
                            label: "var",
                            size_description: "8GB",
                        },
                        Partition {
                            device: format!("{}p3", drive.device),
                            mount_point: "/music",
                            label: "music",
                            size_description: "400GB",
                        },
                        Partition {
                            device: format!("{}p4", drive.device),
                            mount_point: "/metadata",
                            label: "metadata",
                            size_description: "~88GB",
                        },
                    ],
                }
            }
            ValidatedDrives::TwoDrives(primary_drive, secondary_drive) => {
                tracing::info!("  Primary drive: {}", primary_drive.device);
                tracing::info!("    Partition 1: / (16GB)");
                tracing::info!("    Partition 2: /var (8GB)");
                tracing::info!("    Partition 3: /music (400GB)");
                tracing::info!("    Partition 4: /metadata (~88GB)");
                tracing::info!("  Secondary drive: {}", secondary_drive.device);
                tracing::info!("    Partition 1: /cdj-export (512GB)");

                PartitionLayout::DualDrive {
                    primary_device: primary_drive.device.clone(),
                    primary_partitions: vec![
                        Partition {
                            device: format!("{}p1", primary_drive.device),
                            mount_point: "/",
                            label: "root",
                            size_description: "16GB",
                        },
                        Partition {
                            device: format!("{}p2", primary_drive.device),
                            mount_point: "/var",
                            label: "var",
                            size_description: "8GB",
                        },
                        Partition {
                            device: format!("{}p3", primary_drive.device),
                            mount_point: "/music",
                            label: "music",
                            size_description: "400GB",
                        },
                        Partition {
                            device: format!("{}p4", primary_drive.device),
                            mount_point: "/metadata",
                            label: "metadata",
                            size_description: "~88GB",
                        },
                    ],
                    secondary_device: secondary_drive.device.clone(),
                    secondary_partitions: vec![Partition {
                        device: format!("{}p1", secondary_drive.device),
                        mount_point: "/cdj-export",
                        label: "cdj-export",
                        size_description: "512GB",
                    }],
                }
            }
        };

        Ok(PartitionedDrives {
            validated: input,
            layout,
        })
    }
}
