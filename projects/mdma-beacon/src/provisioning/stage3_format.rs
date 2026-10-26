// bases/beacon/src/provisioning/stage3_format.rs
//! Stage 3: Format partitions
//!
//! Hard rule: provisioning must never format `/music` or `/metadata` (or the
//! cache partition) once it holds a filesystem. Only OS partitions
//! (boot/root/var) and data partitions created in this very run can be
//! formatted; the type system enforces this through [`target::FormatTarget`].

use crate::actions::{Action, ActionId, PlannedAction};
use crate::error::Result;
use crate::provisioning::types::{
    CompletedPartitionPlan, CompletedPartitionedDrives, DevicePath, ExistingDataPolicy,
    FormattedSystem, MountPoint, Partition, PartitionFormatAction,
};

// ── Decisions (pure) ──────────────────────────────────────────────────────────

/// How a partition is treated. Decided from the mount point only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PartitionRole {
    /// Boot, root, var: reproducible, may be reformatted.
    Os,
    /// Music, metadata, cache: user data, formatted only when brand new.
    Data,
}

pub(crate) fn role_of(mount_point: MountPoint) -> PartitionRole {
    match mount_point {
        MountPoint::Boot | MountPoint::Root | MountPoint::Var => PartitionRole::Os,
        MountPoint::Music | MountPoint::Metadata | MountPoint::Cache => PartitionRole::Data,
    }
}

/// What `blkid` told us about a partition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BlkidResult {
    /// A filesystem signature was found.
    Signature {
        fs_type: String,
        label: Option<String>,
    },
    /// blkid ran fine and found no signature (exit status 2).
    NoSignature,
    /// blkid could not tell us. Never treated as "empty".
    Error(String),
}

/// Interpret a finished `blkid -o export -s TYPE -s LABEL` run.
pub(crate) fn interpret_blkid(code: Option<i32>, stdout: &str, stderr: &str) -> BlkidResult {
    match code {
        Some(0) => {
            let value = |key: &str| {
                stdout
                    .lines()
                    .find_map(|l| l.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
            };
            match value("TYPE") {
                Some(fs_type) => BlkidResult::Signature {
                    fs_type,
                    label: value("LABEL"),
                },
                None => BlkidResult::NoSignature,
            }
        }
        // blkid: 2 = "nothing found"
        Some(2) => BlkidResult::NoSignature,
        other => BlkidResult::Error(format!("blkid exited with {:?}: {}", other, stderr.trim())),
    }
}

/// The filesystem and label a partition should carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Expected<'a> {
    pub fs_type: &'a str,
    pub label: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FormatDecision {
    Format,
    Preserve,
    Abort(String),
}

/// Decide whether a partition gets formatted.
///
/// - A blkid error aborts (for any role), except under `WipeAll` for a
///   partition created this run (see the last rule).
/// - Data partition with any filesystem signature: preserved, even with a
///   wrong label or type.
/// - Data partition without a signature: formatted only if it was created in
///   this run; otherwise abort (an old, empty-looking data partition may hold
///   data blkid cannot recognize).
/// - OS partition carrying a data label (music/metadata/cache): abort, even
///   under `ReinstallOs`; the label says it holds user data.
/// - OS partition: formatted when missing or wrong; when correct, preserved
///   unless the policy asks for a reinstall.
/// - `WipeAll` and the partition was created this run (stage 2 repartitioned
///   the drive after the user's confirmation): always formatted. Old
///   signatures seen at plan time belong to the partitions that were destroyed.
pub(crate) fn decide_format(
    role: PartitionRole,
    expected: Expected<'_>,
    blkid: &BlkidResult,
    created_this_run: bool,
    policy: ExistingDataPolicy,
) -> FormatDecision {
    match (role, blkid) {
        // The plan-time probe describes partitions stage 2 destroyed, so even a
        // probe error is moot: `ensure_blank` re-checks new data partitions
        // after the wipe, right before mkfs.
        _ if policy == ExistingDataPolicy::WipeAll && created_this_run => FormatDecision::Format,
        (_, BlkidResult::Error(reason)) => {
            FormatDecision::Abort(format!("cannot determine filesystem: {}", reason))
        }
        (
            PartitionRole::Os,
            BlkidResult::Signature {
                label: Some(label), ..
            },
        ) if MountPoint::is_data_label(label) => FormatDecision::Abort(format!(
            "OS partition carries the data label '{}'; refusing to format it",
            label
        )),
        (PartitionRole::Data, BlkidResult::Signature { fs_type, label }) => {
            if fs_type != expected.fs_type || label.as_deref() != Some(expected.label) {
                tracing::warn!(
                    "data partition has {} with label {:?}, expected {} '{}' - preserving it as is",
                    fs_type,
                    label,
                    expected.fs_type,
                    expected.label
                );
            }
            FormatDecision::Preserve
        }
        (PartitionRole::Data, BlkidResult::NoSignature) => {
            if created_this_run {
                FormatDecision::Format
            } else {
                FormatDecision::Abort(
                    "data partition existed before this run and has no recognizable filesystem; \
                     refusing to format it"
                        .to_string(),
                )
            }
        }
        (PartitionRole::Os, BlkidResult::NoSignature) => FormatDecision::Format,
        (PartitionRole::Os, BlkidResult::Signature { fs_type, label }) => {
            let correct = fs_type == expected.fs_type && label.as_deref() == Some(expected.label);
            if correct && policy == ExistingDataPolicy::Preserve {
                FormatDecision::Preserve
            } else {
                FormatDecision::Format
            }
        }
    }
}

// ── What may be formatted (type-level) ────────────────────────────────────────

mod target {
    use super::{role_of, PartitionRole};
    use crate::error::{BeaconError, Result};
    use crate::provisioning::types::{DevicePath, Partition};

    /// A boot/root/var partition.
    pub(crate) struct OsPartition<'a>(&'a Partition);

    /// A data partition that was created in this run and has never held data.
    pub(crate) struct NewDataPartition<'a>(&'a Partition);

    /// The only things `format_partition` accepts. An existing data partition
    /// cannot be turned into one: the constructor refuses it.
    pub(crate) enum FormatTarget<'a> {
        Os(OsPartition<'a>),
        NewData(NewDataPartition<'a>),
    }

    impl<'a> FormatTarget<'a> {
        pub(crate) fn new(partition: &'a Partition, created: &[DevicePath]) -> Result<Self> {
            match role_of(partition.mount_point) {
                PartitionRole::Os => Ok(FormatTarget::Os(OsPartition(partition))),
                PartitionRole::Data if created.contains(&partition.device) => {
                    Ok(FormatTarget::NewData(NewDataPartition(partition)))
                }
                PartitionRole::Data => Err(BeaconError::Formatting {
                    partition: partition.device.to_string(),
                    reason: "refusing to format a data partition that existed before this run"
                        .to_string(),
                }),
            }
        }

        pub(crate) fn partition(&self) -> &'a Partition {
            match self {
                FormatTarget::Os(OsPartition(p)) | FormatTarget::NewData(NewDataPartition(p)) => p,
            }
        }

        pub(crate) fn is_new_data(&self) -> bool {
            matches!(self, FormatTarget::NewData(_))
        }
    }
}

use target::FormatTarget;

// ── The action ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct FormatPartitionsAction;

/// All partitions of the plan, primary drive first. Same order as
/// `FormattedSystem::format_actions`.
fn all_partitions(plan: &CompletedPartitionPlan) -> Vec<&Partition> {
    match plan {
        CompletedPartitionPlan::SingleDrive { partitions, .. } => partitions.iter().collect(),
        CompletedPartitionPlan::DualDrive {
            primary_partitions,
            secondary_partitions,
            ..
        } => primary_partitions
            .iter()
            .chain(secondary_partitions.iter())
            .collect(),
    }
}

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
        tracing::info!("Planning format stage - checking current partition state");

        let policy = input.validated.config.existing_data;
        let format_actions = check_partitions_formatting_status(
            &all_partitions(&input.plan),
            &input.created,
            policy,
        )
        .await?;

        let assumed_output = FormattedSystem {
            partitioned: input.clone(),
            format_actions,
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

        let partitions = all_partitions(&planned_output.partitioned.plan);
        let created = &planned_output.partitioned.created;

        // Collect partitions that need formatting. Building a FormatTarget
        // fails for a data partition that existed before this run.
        let mut targets = Vec::new();
        for (partition, action) in partitions.iter().zip(&planned_output.format_actions) {
            if matches!(action, PartitionFormatAction::WillFormat { .. }) {
                targets.push(FormatTarget::new(partition, created)?);
            }
        }

        if targets.is_empty() {
            tracing::info!("✅ All partitions already formatted correctly - skipping format step");
        } else {
            tracing::info!("Formatting {} partition(s)...", targets.len());

            // Unmount any that are mounted before formatting
            for target in &targets {
                unmount_if_mounted(target.partition()).await?;
            }

            // Last look before mkfs: a brand-new data partition must still be
            // blank. A leftover signature means there is data we must not erase.
            for target in targets.iter().filter(|t| t.is_new_data()) {
                ensure_blank(target.partition()).await?;
            }

            // Format the partitions
            for target in &targets {
                format_partition(target).await?;
            }

            // Sync and wait for udev to update device info
            // This ensures lsblk sees the new filesystems, especially for large partitions
            tracing::info!("Syncing filesystems and waiting for udev...");
            let _ = tokio::process::Command::new("sync").output().await;
            let _ = tokio::process::Command::new("udevadm")
                .args(["settle", "--timeout=10"])
                .output()
                .await;
            // Small additional delay for safety
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

            // Verify the partitions we own: OS partitions and anything we just
            // formatted. Preserved data partitions are not ours to judge.
            let to_verify: Vec<Partition> = partitions
                .iter()
                .zip(&planned_output.format_actions)
                .filter(|(p, action)| {
                    role_of(p.mount_point) == PartitionRole::Os
                        || matches!(action, PartitionFormatAction::WillFormat { .. })
                })
                .map(|(p, _)| (*p).clone())
                .collect();
            verify_formatting(&to_verify).await?;
        }

        tracing::info!("Format stage complete - all partitions verified");
        Ok(planned_output.clone())
    }
}

// ── I/O ───────────────────────────────────────────────────────────────────────

/// Ask `blkid` what is on a partition. `blkid -p` reads the on-disk superblock
/// directly and is not fooled by stale kernel caches after `wipefs -a` (#53).
async fn run_blkid(device: &str) -> BlkidResult {
    let output = tokio::process::Command::new("blkid")
        .args(["-p", "-o", "export", "-s", "TYPE", "-s", "LABEL", device])
        .output()
        .await;

    match output {
        Ok(out) => interpret_blkid(
            out.status.code(),
            &String::from_utf8_lossy(&out.stdout),
            &String::from_utf8_lossy(&out.stderr),
        ),
        Err(e) => BlkidResult::Error(format!("failed to run blkid: {}", e)),
    }
}

/// Probe a partition for a filesystem. A device node that does not exist yet is
/// blank if stage 2 is going to create it, and an error otherwise.
async fn probe_partition(partition: &Partition, created_this_run: bool) -> BlkidResult {
    let device = partition.device.as_str();
    if !std::path::Path::new(device).exists() {
        return if created_this_run {
            BlkidResult::NoSignature
        } else {
            BlkidResult::Error(format!("{} does not exist", device))
        };
    }
    run_blkid(device).await
}

async fn ensure_blank(partition: &Partition) -> Result<()> {
    let device = partition.device.as_str();
    match run_blkid(device).await {
        BlkidResult::NoSignature => Ok(()),
        BlkidResult::Signature { fs_type, label } => Err(crate::error::BeaconError::Formatting {
            partition: device.to_string(),
            reason: format!(
                "new data partition already carries a {} filesystem (label {:?}); refusing to \
                 overwrite it. If this is really stale, wipe it by hand with wipefs.",
                fs_type, label
            ),
        }),
        BlkidResult::Error(reason) => Err(crate::error::BeaconError::Formatting {
            partition: device.to_string(),
            reason,
        }),
    }
}

async fn check_partitions_formatting_status(
    partitions: &[&Partition],
    created: &[DevicePath],
    policy: ExistingDataPolicy,
) -> Result<Vec<PartitionFormatAction>> {
    let mut actions = Vec::new();

    for partition in partitions {
        let device = partition.device.clone();
        let label = partition.label();
        let fs_type = partition.filesystem_type();
        let created_this_run = created.contains(&device);

        let blkid = probe_partition(partition, created_this_run).await;
        let decision = decide_format(
            role_of(partition.mount_point),
            Expected {
                fs_type: fs_type.as_str(),
                label: label.as_str(),
            },
            &blkid,
            created_this_run,
            policy,
        );

        match decision {
            FormatDecision::Format => actions.push(PartitionFormatAction::WillFormat {
                device,
                label,
                fs_type,
            }),
            FormatDecision::Preserve => {
                tracing::info!("  {} already holds a filesystem - keeping it", device);
                actions.push(PartitionFormatAction::AlreadyFormatted {
                    device,
                    label,
                    fs_type,
                });
            }
            FormatDecision::Abort(reason) => {
                return Err(crate::error::BeaconError::Formatting {
                    partition: device.to_string(),
                    reason: format!("{}. No data was changed.", reason),
                });
            }
        }
    }

    Ok(actions)
}

async fn unmount_if_mounted(partition: &Partition) -> Result<()> {
    use tokio::process::Command;

    let device = partition.device.as_str();

    // Check if mounted using findmnt
    let output = Command::new("findmnt")
        .arg("-n") // No header
        .arg("-o")
        .arg("TARGET") // Just show mount point
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

    Ok(())
}

/// Format a partition. Accepts only an OS partition or a data partition created
/// in this run; see [`FormatTarget`].
async fn format_partition(target: &FormatTarget<'_>) -> Result<()> {
    use super::types::FilesystemType;
    use tokio::process::Command;

    let partition = target.partition();
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
    fn verify_partition_with_valid_data() {
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
    fn verify_partition_fstype_mismatch() {
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
    fn verify_partition_label_mismatch() {
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
    fn verify_partition_not_found() {
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

    // ── #53: wiped partition must not be classified as already-formatted ──────

    /// Regression test for #53.
    ///
    /// `verify_partition_fields` is the leaf function that decides whether a
    /// partition is correctly formatted. When `wipefs -a` erases the filesystem
    /// signature, `blkid` returns an empty TYPE. Simulate that by passing JSON
    /// with `"fstype": null` (what lsblk reported before the fix) and confirm
    /// the function returns an error — meaning stage 3 would proceed to format.
    #[test]
    fn verify_partition_fields_null_fstype_triggers_format() {
        // Simulate what lsblk reported for a wiped partition (fstype == null)
        let partition_data = serde_json::json!({
            "name": "nvme0n1p1",
            "fstype": null,
            "label": null,
            "size": "400G"
        });

        let result = verify_partition_fields(&partition_data, "nvme0n1p1", "ext4", "music");

        assert!(
            result.is_err(),
            "A wiped partition (null fstype) should NOT be classified as already-formatted"
        );

        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("Filesystem type mismatch"),
            "Expected fstype mismatch error, got: {}",
            err
        );
    }

    #[test]
    fn verify_partition_fields_empty_fstype_triggers_format() {
        // blkid returns empty string when partition has no FS signature
        let partition_data = serde_json::json!({
            "name": "nvme0n1p1",
            "fstype": "",
            "label": "",
            "size": "400G"
        });

        let result = verify_partition_fields(&partition_data, "nvme0n1p1", "ext4", "music");

        assert!(
            result.is_err(),
            "A partition with empty fstype should NOT be classified as already-formatted"
        );
    }

    // ── decide_format ─────────────────────────────────────────────────────────

    fn signature(fs: &str, label: Option<&str>) -> BlkidResult {
        BlkidResult::Signature {
            fs_type: fs.to_string(),
            label: label.map(str::to_string),
        }
    }

    const MUSIC: Expected<'static> = Expected {
        fs_type: "ext4",
        label: "music",
    };
    const ROOT: Expected<'static> = Expected {
        fs_type: "ext4",
        label: "root",
    };

    fn is_abort(d: &FormatDecision) -> bool {
        matches!(d, FormatDecision::Abort(_))
    }

    #[test]
    fn decide_format_table() {
        use ExistingDataPolicy::{Preserve, ReinstallOs};
        use PartitionRole::{Data, Os};

        // (case, role, expected, blkid, created_this_run, policy, wanted)
        let cases: Vec<(
            &str,
            PartitionRole,
            Expected,
            BlkidResult,
            bool,
            ExistingDataPolicy,
            FormatDecision,
        )> = vec![
            (
                "music, ext4, wrong label -> preserve",
                Data,
                MUSIC,
                signature("ext4", Some("oops")),
                false,
                Preserve,
                FormatDecision::Preserve,
            ),
            (
                "music, ext4, no label -> preserve",
                Data,
                MUSIC,
                signature("ext4", None),
                false,
                Preserve,
                FormatDecision::Preserve,
            ),
            (
                "music, other fs -> preserve",
                Data,
                MUSIC,
                signature("xfs", Some("music")),
                false,
                Preserve,
                FormatDecision::Preserve,
            ),
            (
                "music, correct -> preserve",
                Data,
                MUSIC,
                signature("ext4", Some("music")),
                false,
                Preserve,
                FormatDecision::Preserve,
            ),
            (
                "music, correct, reinstall os -> still preserve",
                Data,
                MUSIC,
                signature("ext4", Some("music")),
                false,
                ReinstallOs,
                FormatDecision::Preserve,
            ),
            (
                "music, wrong label, reinstall os -> still preserve",
                Data,
                MUSIC,
                signature("ext4", Some("oops")),
                false,
                ReinstallOs,
                FormatDecision::Preserve,
            ),
            (
                "music, blkid error -> abort",
                Data,
                MUSIC,
                BlkidResult::Error("boom".to_string()),
                false,
                Preserve,
                FormatDecision::Abort(String::new()),
            ),
            (
                "music, blkid error even if created -> abort",
                Data,
                MUSIC,
                BlkidResult::Error("boom".to_string()),
                true,
                Preserve,
                FormatDecision::Abort(String::new()),
            ),
            (
                "music, no signature, pre-existing -> abort",
                Data,
                MUSIC,
                BlkidResult::NoSignature,
                false,
                Preserve,
                FormatDecision::Abort(String::new()),
            ),
            (
                "music, no signature, pre-existing, reinstall os -> abort",
                Data,
                MUSIC,
                BlkidResult::NoSignature,
                false,
                ReinstallOs,
                FormatDecision::Abort(String::new()),
            ),
            (
                "music, created this run -> format",
                Data,
                MUSIC,
                BlkidResult::NoSignature,
                true,
                Preserve,
                FormatDecision::Format,
            ),
            (
                "root, mismatch -> format",
                Os,
                ROOT,
                signature("ext4", Some("oops")),
                false,
                Preserve,
                FormatDecision::Format,
            ),
            (
                "root, other fs -> format",
                Os,
                ROOT,
                signature("vfat", Some("root")),
                false,
                Preserve,
                FormatDecision::Format,
            ),
            (
                "root, no signature -> format",
                Os,
                ROOT,
                BlkidResult::NoSignature,
                false,
                Preserve,
                FormatDecision::Format,
            ),
            (
                "root, correct -> preserve",
                Os,
                ROOT,
                signature("ext4", Some("root")),
                false,
                Preserve,
                FormatDecision::Preserve,
            ),
            (
                "root, correct, reinstall os -> format",
                Os,
                ROOT,
                signature("ext4", Some("root")),
                false,
                ReinstallOs,
                FormatDecision::Format,
            ),
            (
                "root, blkid error -> abort",
                Os,
                ROOT,
                BlkidResult::Error("boom".to_string()),
                false,
                Preserve,
                FormatDecision::Abort(String::new()),
            ),
            (
                "root partition labelled music -> abort",
                Os,
                ROOT,
                signature("ext4", Some("music")),
                false,
                Preserve,
                FormatDecision::Abort(String::new()),
            ),
            (
                "root partition labelled music, reinstall os -> abort",
                Os,
                ROOT,
                signature("ext4", Some("music")),
                false,
                ReinstallOs,
                FormatDecision::Abort(String::new()),
            ),
            (
                "root partition labelled metadata, reinstall os -> abort",
                Os,
                ROOT,
                signature("ext4", Some("metadata")),
                false,
                ReinstallOs,
                FormatDecision::Abort(String::new()),
            ),
            (
                "root partition labelled cache, reinstall os -> abort",
                Os,
                ROOT,
                signature("ext4", Some("cache")),
                false,
                ReinstallOs,
                FormatDecision::Abort(String::new()),
            ),
        ];

        for (case, role, expected, blkid, created, policy, wanted) in cases {
            let got = decide_format(role, expected, &blkid, created, policy);
            if is_abort(&wanted) {
                assert!(is_abort(&got), "{case}: got {got:?}");
            } else {
                assert_eq!(got, wanted, "{case}");
            }
        }
    }

    /// Property: a data partition that already holds a filesystem is never
    /// formatted, whatever the label, filesystem, flags or policy.
    #[test]
    fn decide_format_never_formats_data_with_a_signature() {
        for fs in ["ext4", "xfs", "vfat", "btrfs"] {
            for label in [None, Some("music"), Some("oops")] {
                for created in [false, true] {
                    for policy in [
                        ExistingDataPolicy::Preserve,
                        ExistingDataPolicy::ReinstallOs,
                    ] {
                        let decision = decide_format(
                            PartitionRole::Data,
                            MUSIC,
                            &signature(fs, label),
                            created,
                            policy,
                        );
                        assert_eq!(decision, FormatDecision::Preserve);
                    }
                }
            }
        }
    }

    /// After a WipeAll repartition every partition was created this run, so
    /// stale signatures seen at plan time (even a data label on an OS node) do
    /// not stop the format, and neither does a plan-time blkid error (messy
    /// disks must be wipeable; `ensure_blank` re-checks after the wipe).
    #[test]
    fn wipe_all_formats_partitions_created_this_run() {
        let stale = [
            signature("ext4", Some("music")),
            signature("ext4", Some("root")),
            signature("xfs", None),
            BlkidResult::NoSignature,
            BlkidResult::Error("boom".to_string()),
        ];
        for blkid in &stale {
            for (role, expected) in [(PartitionRole::Data, MUSIC), (PartitionRole::Os, ROOT)] {
                assert_eq!(
                    decide_format(role, expected, blkid, true, ExistingDataPolicy::WipeAll),
                    FormatDecision::Format,
                    "{role:?} {blkid:?}"
                );
            }
        }
        // A probe error on a partition NOT created this run still aborts, and
        // so does one created this run under Preserve / ReinstallOs.
        for (created, policy) in [
            (false, ExistingDataPolicy::WipeAll),
            (true, ExistingDataPolicy::Preserve),
            (true, ExistingDataPolicy::ReinstallOs),
        ] {
            for (role, expected) in [(PartitionRole::Data, MUSIC), (PartitionRole::Os, ROOT)] {
                assert!(
                    is_abort(&decide_format(
                        role,
                        expected,
                        &BlkidResult::Error("boom".to_string()),
                        created,
                        policy
                    )),
                    "{role:?} created={created} {policy:?}"
                );
            }
        }
    }

    /// WipeAll does not authorise formatting a data partition that was NOT
    /// created this run.
    #[test]
    fn wipe_all_does_not_format_pre_existing_data_partitions() {
        assert_eq!(
            decide_format(
                PartitionRole::Data,
                MUSIC,
                &signature("ext4", Some("music")),
                false,
                ExistingDataPolicy::WipeAll
            ),
            FormatDecision::Preserve
        );
    }

    #[test]
    fn roles_follow_mount_point() {
        assert_eq!(role_of(MountPoint::Boot), PartitionRole::Os);
        assert_eq!(role_of(MountPoint::Root), PartitionRole::Os);
        assert_eq!(role_of(MountPoint::Var), PartitionRole::Os);
        assert_eq!(role_of(MountPoint::Music), PartitionRole::Data);
        assert_eq!(role_of(MountPoint::Metadata), PartitionRole::Data);
        assert_eq!(role_of(MountPoint::Cache), PartitionRole::Data);
    }

    // ── interpret_blkid ───────────────────────────────────────────────────────

    #[test]
    fn blkid_exit_zero_with_type_is_a_signature() {
        let out = "DEVNAME=/dev/nvme0n1p5\nLABEL=music\nTYPE=ext4\n";
        assert_eq!(
            interpret_blkid(Some(0), out, ""),
            signature("ext4", Some("music"))
        );
    }

    #[test]
    fn blkid_signature_without_label() {
        assert_eq!(
            interpret_blkid(Some(0), "TYPE=ext4\n", ""),
            signature("ext4", None)
        );
    }

    #[test]
    fn blkid_exit_two_is_no_signature() {
        assert_eq!(interpret_blkid(Some(2), "", ""), BlkidResult::NoSignature);
    }

    #[test]
    fn blkid_exit_zero_without_type_is_no_signature() {
        assert_eq!(
            interpret_blkid(Some(0), "DEVNAME=/dev/x\n", ""),
            BlkidResult::NoSignature
        );
    }

    #[test]
    fn blkid_other_exit_codes_and_signals_are_errors() {
        assert!(matches!(
            interpret_blkid(Some(4), "", "usage error"),
            BlkidResult::Error(_)
        ));
        assert!(matches!(
            interpret_blkid(Some(1), "", ""),
            BlkidResult::Error(_)
        ));
        assert!(matches!(
            interpret_blkid(None, "", ""),
            BlkidResult::Error(_)
        ));
    }

    // ── FormatTarget ──────────────────────────────────────────────────────────

    fn partition(device: &str, mount_point: MountPoint) -> Partition {
        Partition {
            device: DevicePath::new(device).unwrap(),
            mount_point,
            size: PartitionSize::from_gb(1),
        }
    }

    #[test]
    fn format_target_refuses_existing_data_partitions() {
        for mount_point in [MountPoint::Music, MountPoint::Metadata, MountPoint::Cache] {
            let p = partition("/dev/nvme0n1p5", mount_point);
            assert!(
                FormatTarget::new(&p, &[]).is_err(),
                "{mount_point:?} that existed before must not be formattable"
            );
        }
    }

    #[test]
    fn format_target_accepts_new_data_and_os_partitions() {
        let music = partition("/dev/nvme0n1p5", MountPoint::Music);
        let created = vec![music.device.clone()];
        assert!(FormatTarget::new(&music, &created).unwrap().is_new_data());

        let root = partition("/dev/nvme0n1p2", MountPoint::Root);
        let target = FormatTarget::new(&root, &[]).unwrap();
        assert!(!target.is_new_data());
    }
}
