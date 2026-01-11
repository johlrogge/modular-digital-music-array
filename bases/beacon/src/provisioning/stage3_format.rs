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
        tracing::info!("Stage 3: Format partitions - checking current state");

        // Check and format partitions from the plan
        match &planned_output.partitioned.plan {
            crate::provisioning::types::CompletedPartitionPlan::SingleDrive {
                partitions, ..
            } => {
                check_and_format_partitions(partitions).await?;
            }
            crate::provisioning::types::CompletedPartitionPlan::DualDrive {
                primary_partitions,
                secondary_partitions,
                ..
            } => {
                check_and_format_partitions(primary_partitions).await?;
                check_and_format_partitions(secondary_partitions).await?;
            }
        }

        tracing::info!("Format stage complete - all partitions verified");
        Ok(planned_output.clone())
    }
}

async fn check_and_format_partitions(partitions: &[super::types::Partition]) -> Result<()> {
    // First, try to verify current state
    match verify_formatting(partitions).await {
        Ok(()) => {
            // All partitions already formatted correctly!
            tracing::info!(
                "✅ All {} partition(s) already formatted correctly - skipping format step",
                partitions.len()
            );
            Ok(())
        }
        Err(e) => {
            // Not formatted correctly - need to format
            tracing::info!(
                "Partitions not formatted correctly ({}), will format now...",
                e
            );
            
            // IMPORTANT: Unmount partitions before formatting
            // They might be mounted from a previous failed provisioning run
            unmount_if_needed(partitions).await?;
            
            // Now format
            format_partitions(partitions).await?;
            
            // Verify after formatting
            verify_formatting(partitions).await?;
            Ok(())
        }
    }
}

async fn unmount_if_needed(partitions: &[super::types::Partition]) -> Result<()> {
    use tokio::process::Command;
    
    tracing::debug!("Checking if any partitions are mounted...");
    
    for partition in partitions {
        let device = partition.device.as_str();
        
        // Check if mounted using findmnt
        let output = Command::new("findmnt")
            .arg("-n")  // No header
            .arg("-o")
            .arg("TARGET")  // Just show mount point
            .arg(device)
            .output()
            .await
            .map_err(|e| crate::error::BeaconError::command_failed("findmnt", e))?;
        
        if output.status.success() && !output.stdout.is_empty() {
            // Partition is mounted - unmount it
            let mount_point = String::from_utf8_lossy(&output.stdout).trim().to_string();
            tracing::warn!(
                "Partition {} is mounted at '{}', unmounting before formatting",
                device,
                mount_point
            );
            
            let unmount = Command::new("umount")
                .arg(device)
                .output()
                .await
                .map_err(|e| crate::error::BeaconError::command_failed("umount", e))?;
            
            if !unmount.status.success() {
                let stderr = String::from_utf8_lossy(&unmount.stderr);
                return Err(crate::error::BeaconError::Formatting {
                    partition: device.to_string(),
                    reason: format!("Failed to unmount before formatting: {}", stderr),
                });
            }
            
            tracing::info!("Successfully unmounted {} from {}", device, mount_point);
        } else {
            tracing::debug!("Partition {} is not mounted", device);
        }
    }
    
    Ok(())
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

    let fs_type = partition.filesystem_type();
    let device = partition.device.as_str();
    let label = partition.label();
    let label = label.as_str();

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

async fn verify_formatting(partitions: &[super::types::Partition]) -> Result<()> {
    use tokio::process::Command;

    tracing::info!("Verifying formatted partitions...");

    // Run lsblk with JSON output
    let output = Command::new("lsblk")
        .arg("-o")
        .arg("NAME,FSTYPE,LABEL,SIZE")
        .arg("--json")
        .output()
        .await
        .map_err(|e| crate::error::BeaconError::command_failed("lsblk", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(crate::error::BeaconError::Formatting {
            partition: "verification".to_string(),
            reason: format!("lsblk failed: {}", stderr),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Log full lsblk output for debugging
    tracing::info!("Current partition layout:\n{}", stdout);

    // Parse JSON and verify each partition
    let lsblk_data: serde_json::Value =
        serde_json::from_str(&stdout).map_err(|e| crate::error::BeaconError::Formatting {
            partition: "verification".to_string(),
            reason: format!("Failed to parse lsblk JSON: {}", e),
        })?;

    // Verify each partition
    for partition in partitions {
        verify_partition(&lsblk_data, partition)?;
    }

    tracing::info!(
        "✅ All {} partition(s) verified successfully",
        partitions.len()
    );
    Ok(())
}

fn verify_partition(
    lsblk_data: &serde_json::Value,
    partition: &super::types::Partition,
) -> Result<()> {
    use super::types::FilesystemType;

    let device_name = partition
        .device
        .as_str()
        .strip_prefix("/dev/")
        .unwrap_or(partition.device.as_str());

    let expected_fstype = match partition.filesystem_type() {
        FilesystemType::Fat32 => "vfat",
        FilesystemType::Ext4 => "ext4",
    };

    let expected_label = partition.label();
    let expected_label = expected_label.as_str();

    // Find the partition in lsblk output
    let blockdevices = lsblk_data["blockdevices"].as_array().ok_or_else(|| {
        crate::error::BeaconError::Formatting {
            partition: device_name.to_string(),
            reason: "lsblk JSON missing blockdevices array".to_string(),
        }
    })?;

    // Search through all devices and their children
    for device in blockdevices {
        // Check if this is the device directly
        if device["name"].as_str() == Some(device_name) {
            return verify_partition_fields(device, device_name, expected_fstype, expected_label);
        }

        // Check children (partitions)
        if let Some(children) = device["children"].as_array() {
            for child in children {
                if child["name"].as_str() == Some(device_name) {
                    return verify_partition_fields(
                        child,
                        device_name,
                        expected_fstype,
                        expected_label,
                    );
                }
            }
        }
    }

    Err(crate::error::BeaconError::Formatting {
        partition: device_name.to_string(),
        reason: "Partition not found in lsblk output".to_string(),
    })
}

fn verify_partition_fields(
    partition_data: &serde_json::Value,
    device_name: &str,
    expected_fstype: &str,
    expected_label: &str,
) -> Result<()> {
    let actual_fstype = partition_data["fstype"].as_str().unwrap_or("<none>");

    let actual_label = partition_data["label"].as_str().unwrap_or("<none>");

    // Verify filesystem type
    if actual_fstype != expected_fstype {
        return Err(crate::error::BeaconError::Formatting {
            partition: device_name.to_string(),
            reason: format!(
                "Filesystem type mismatch: expected '{}', found '{}'",
                expected_fstype, actual_fstype
            ),
        });
    }

    // Verify label
    if actual_label != expected_label {
        return Err(crate::error::BeaconError::Formatting {
            partition: device_name.to_string(),
            reason: format!(
                "Label mismatch: expected '{}', found '{}'",
                expected_label, actual_label
            ),
        });
    }

    tracing::debug!(
        "✅ {} verified: {} with label '{}'",
        device_name,
        actual_fstype,
        actual_label
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provisioning::types::{DevicePath, MountPoint, Partition, PartitionSize};

    #[test]
    fn test_verify_partition_with_valid_data() {
        let lsblk_json = r#"{
            "blockdevices": [
                {
                    "name": "nvme0n1",
                    "fstype": null,
                    "label": null,
                    "size": "512G",
                    "children": [
                        {
                            "name": "nvme0n1p1",
                            "fstype": "ext4",
                            "label": "music",
                            "size": "400G"
                        }
                    ]
                }
            ]
        }"#;

        let lsblk_data: serde_json::Value = serde_json::from_str(lsblk_json).unwrap();

        let partition = Partition {
            device: DevicePath::new("/dev/nvme0n1p1").unwrap(),
            mount_point: MountPoint::Music,
            size: PartitionSize::from_gb(400),
        };

        let result = verify_partition(&lsblk_data, &partition);
        assert!(result.is_ok(), "Expected verification to succeed");
    }

    #[test]
    fn test_verify_partition_fstype_mismatch() {
        let lsblk_json = r#"{
            "blockdevices": [
                {
                    "name": "nvme0n1",
                    "children": [
                        {
                            "name": "nvme0n1p1",
                            "fstype": "vfat",
                            "label": "music",
                            "size": "400G"
                        }
                    ]
                }
            ]
        }"#;

        let lsblk_data: serde_json::Value = serde_json::from_str(lsblk_json).unwrap();

        let partition = Partition {
            device: DevicePath::new("/dev/nvme0n1p1").unwrap(),
            mount_point: MountPoint::Music, // Expects ext4
            size: PartitionSize::from_gb(400),
        };

        let result = verify_partition(&lsblk_data, &partition);
        assert!(
            result.is_err(),
            "Expected verification to fail on fstype mismatch"
        );

        let err = result.unwrap_err();
        assert!(err.to_string().contains("Filesystem type mismatch"));
    }

    #[test]
    fn test_verify_partition_label_mismatch() {
        let lsblk_json = r#"{
            "blockdevices": [
                {
                    "name": "nvme0n1",
                    "children": [
                        {
                            "name": "nvme0n1p1",
                            "fstype": "ext4",
                            "label": "wrong-label",
                            "size": "400G"
                        }
                    ]
                }
            ]
        }"#;

        let lsblk_data: serde_json::Value = serde_json::from_str(lsblk_json).unwrap();

        let partition = Partition {
            device: DevicePath::new("/dev/nvme0n1p1").unwrap(),
            mount_point: MountPoint::Music, // Expects label "music"
            size: PartitionSize::from_gb(400),
        };

        let result = verify_partition(&lsblk_data, &partition);
        assert!(
            result.is_err(),
            "Expected verification to fail on label mismatch"
        );

        let err = result.unwrap_err();
        assert!(err.to_string().contains("Label mismatch"));
    }

    #[test]
    fn test_verify_partition_not_found() {
        let lsblk_json = r#"{
            "blockdevices": [
                {
                    "name": "nvme0n1",
                    "children": []
                }
            ]
        }"#;

        let lsblk_data: serde_json::Value = serde_json::from_str(lsblk_json).unwrap();

        let partition = Partition {
            device: DevicePath::new("/dev/nvme0n1p99").unwrap(),
            mount_point: MountPoint::Music,
            size: PartitionSize::from_gb(400),
        };

        let result = verify_partition(&lsblk_data, &partition);
        assert!(
            result.is_err(),
            "Expected verification to fail when partition not found"
        );

        let err = result.unwrap_err();
        assert!(err.to_string().contains("not found"));
    }
}
