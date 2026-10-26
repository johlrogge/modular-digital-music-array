// bases/beacon/src/provisioning/stage2_partition.rs
//! Stage 2: Partition NVMe drives
//!
//! Hard rule: provisioning must never destroy `/music` or `/metadata` by
//! accident. Unless the user explicitly chose `ExistingDataPolicy::WipeAll`,
//! the partition table is written ONLY to a drive that has no partitions at
//! all. A drive that already has partitions is reconciled against the plan:
//! either it matches (nothing to do) or provisioning aborts. The on-disk table
//! is the truth; it is never rewritten or "corrected".
//!
//! `WipeAll` is the deliberate exception: the planned target drives are
//! repartitioned from scratch and every old signature is erased. Data found on
//! a drive that is NOT being repartitioned still aborts.

use crate::actions::{Action, ActionId, PlannedAction};
use crate::error::Result;
use crate::provisioning::types::{
    CompletedPartitionedDrives, Partition, PartitionPlan, PartitionState, PartitionedDrives,
    ValidatedHardware,
};
use crate::types::{ExistingDataPolicy, MountPoint, ValidationError};
use serde::Deserialize;
use std::process::Command;

// ── Reading the on-disk table (`sfdisk --json`) ───────────────────────────────

#[derive(Debug, Deserialize)]
struct SfdiskOutput {
    partitiontable: SfdiskTable,
}

#[derive(Debug, Deserialize)]
struct SfdiskTable {
    label: String,
    /// Bytes per sector; sfdisk reports partition sizes in sectors.
    #[serde(default)]
    sectorsize: Option<u64>,
    #[serde(default)]
    partitions: Vec<SfdiskPartition>,
}

#[derive(Debug, Deserialize)]
struct SfdiskPartition {
    node: String,
    /// Size in sectors
    #[serde(default)]
    size: Option<u64>,
    /// GPT partition name (set by `name=` when we create the table)
    #[serde(default)]
    name: Option<String>,
}

/// A partition as found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OnDiskPartition {
    pub node: String,
    pub name: Option<String>,
    /// Size in bytes, when sfdisk reported it. Only used for display.
    pub size_bytes: Option<u64>,
}

impl OnDiskPartition {
    /// One-line description for logs and the confirmation page.
    pub(crate) fn describe(&self) -> String {
        let size = self
            .size_bytes
            .map(|b| format!("{} MB", b / (1024 * 1024)))
            .unwrap_or_else(|| "unknown size".to_string());
        format!(
            "{} (label: {}, size: {})",
            self.node,
            self.name.as_deref().unwrap_or("<unnamed>"),
            size
        )
    }
}

/// The partition table as found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OnDiskTable {
    /// The drive has no partition table and no signature of any kind.
    NoTable,
    /// No partition table, but the whole disk carries a signature (a
    /// filesystem, LUKS, LVM, ZFS ...). It is not blank.
    Unrecognised { signature: String },
    Table {
        /// Table type as reported by sfdisk (`gpt`, `dos`, ...)
        label: String,
        partitions: Vec<OnDiskPartition>,
    },
}

impl OnDiskTable {
    /// True when the drive holds nothing at all: no partitions, no signature.
    fn is_blank(&self) -> bool {
        match self {
            OnDiskTable::NoTable => true,
            OnDiskTable::Unrecognised { .. } => false,
            OnDiskTable::Table { partitions, .. } => partitions.is_empty(),
        }
    }

    /// What a wipe of this drive destroys, as pseudo-partitions for display
    /// and signature wiping. A whole-disk signature is listed as the disk.
    fn destroyed_by_wipe(&self, device: &str) -> Vec<OnDiskPartition> {
        match self {
            OnDiskTable::NoTable => Vec::new(),
            OnDiskTable::Unrecognised { signature } => vec![OnDiskPartition {
                node: device.to_string(),
                name: Some(format!("whole-disk signature: {}", signature)),
                size_bytes: None,
            }],
            OnDiskTable::Table { partitions, .. } => partitions.clone(),
        }
    }

    /// Names of the data partitions (music/metadata/cache) on this drive.
    fn data_labels(&self) -> Vec<String> {
        match self {
            OnDiskTable::NoTable | OnDiskTable::Unrecognised { .. } => Vec::new(),
            OnDiskTable::Table { partitions, .. } => partitions
                .iter()
                .filter_map(|p| p.name.as_deref())
                .filter(|n| MountPoint::is_data_label(n))
                .map(str::to_string)
                .collect(),
        }
    }
}

/// A drive together with the table found on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScannedDrive {
    pub device: String,
    pub table: OnDiskTable,
}

/// Data partitions found on a drive where the plan does not expect them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForeignData {
    pub device: String,
    pub labels: Vec<String>,
}

/// Why stage 2 refuses to continue. Every variant means: nothing was changed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LayoutError {
    #[error(
        "Cannot read the partition table of {device}: {reason}. Refusing to continue: \
         no data was changed, music and metadata were left untouched."
    )]
    Unreadable { device: String, reason: String },

    #[error(
        "Existing partition table on {device} is '{label}', not GPT, which is incompatible \
         with the MDMA layout. Refusing to continue: no data was changed, music and metadata \
         were left untouched."
    )]
    NotGpt { device: String, label: String },

    #[error(
        "Existing partition layout on {device} is incompatible with the current layout: \
         expected [{expected}], found [{found}]{hint}. Refusing to continue: no data was \
         changed, music and metadata were left untouched. To migrate an old unit, provision a \
         fresh disk with this version, then copy /music and /metadata over with rsync."
    )]
    Mismatch {
        device: String,
        expected: String,
        found: String,
        hint: String,
    },

    #[error(
        "Found data partition(s) [{labels}] on {other}, which is incompatible with the plan \
         for {device} (the NVMe drive order may have changed). Refusing to continue: no data \
         was changed, music and metadata were left untouched."
    )]
    DataOnOtherDrive {
        device: String,
        other: String,
        labels: String,
    },

    #[error(
        "Refusing to write the partition table of {device}: it now has partitions. \
         No data was changed; music and metadata were left untouched."
    )]
    ChangedSincePlanning { device: String },

    #[error(
        "{device} has no partition table but carries a signature ({signature}), so it is \
         not blank. Refusing to continue: no data was changed, music and metadata were left \
         untouched."
    )]
    Unrecognised { device: String, signature: String },

    #[error(
        "Refusing to write the partition table of {device}: some partitions already exist. \
         No data was changed."
    )]
    PartiallyExisting { device: String },
}

/// Interpret the result of `sfdisk --json <device>`.
///
/// Only the explicit "no partition table" answer counts as an empty drive. Any
/// other failure, or output we cannot parse, is an error: a failed read must
/// never be mistaken for an empty disk.
pub(crate) fn parse_sfdisk(
    device: &str,
    success: bool,
    stdout: &[u8],
    stderr: &str,
) -> std::result::Result<OnDiskTable, LayoutError> {
    if !success {
        if stderr.contains("does not contain a recognized partition table") {
            return Ok(OnDiskTable::NoTable);
        }
        return Err(LayoutError::Unreadable {
            device: device.to_string(),
            reason: format!("sfdisk failed: {}", stderr.trim()),
        });
    }

    let parsed: SfdiskOutput =
        serde_json::from_slice(stdout).map_err(|e| LayoutError::Unreadable {
            device: device.to_string(),
            reason: format!("cannot parse sfdisk output: {}", e),
        })?;

    let sector = parsed.partitiontable.sectorsize.unwrap_or(512);
    Ok(OnDiskTable::Table {
        label: parsed.partitiontable.label,
        partitions: parsed
            .partitiontable
            .partitions
            .into_iter()
            .map(|p| OnDiskPartition {
                node: p.node,
                name: p.name,
                size_bytes: p.size.map(|s| s.saturating_mul(sector)),
            })
            .collect(),
    })
}

/// Interpret `wipefs --noheadings --output TYPE <disk>` (list mode, changes
/// nothing). `Ok(None)` means the disk carries no signature at all; a failed
/// probe is an error, never "no signature".
pub(crate) fn interpret_signature_probe(
    device: &str,
    success: bool,
    stdout: &str,
    stderr: &str,
) -> std::result::Result<Option<String>, LayoutError> {
    if !success {
        return Err(LayoutError::Unreadable {
            device: device.to_string(),
            reason: format!("signature probe failed: {}", stderr.trim()),
        });
    }
    let mut types: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    types.dedup();
    Ok((!types.is_empty()).then(|| types.join(", ")))
}

/// Combine the sfdisk result with the whole-disk signature probe: "no
/// partition table" is a blank drive only if there is also no signature.
pub(crate) fn combine_with_signature(table: OnDiskTable, signature: Option<String>) -> OnDiskTable {
    match (table, signature) {
        (OnDiskTable::NoTable, Some(signature)) => OnDiskTable::Unrecognised { signature },
        (table, _) => table,
    }
}

/// Read the partition table of `device` with `sfdisk --json`. A drive without
/// a table is additionally probed for whole-disk signatures (ext4, LUKS, LVM,
/// ZFS ...).
fn read_table(device: &str) -> std::result::Result<OnDiskTable, LayoutError> {
    let output = Command::new("sfdisk")
        .args(["--json", device])
        .output()
        .map_err(|e| LayoutError::Unreadable {
            device: device.to_string(),
            reason: format!("failed to run sfdisk: {}", e),
        })?;

    let table = parse_sfdisk(
        device,
        output.status.success(),
        &output.stdout,
        &String::from_utf8_lossy(&output.stderr),
    )?;
    if table != OnDiskTable::NoTable {
        return Ok(table);
    }

    let probe = Command::new("wipefs")
        .args(["--noheadings", "--output", "TYPE", device])
        .output()
        .map_err(|e| LayoutError::Unreadable {
            device: device.to_string(),
            reason: format!("failed to run wipefs: {}", e),
        })?;
    let signature = interpret_signature_probe(
        device,
        probe.status.success(),
        &String::from_utf8_lossy(&probe.stdout),
        &String::from_utf8_lossy(&probe.stderr),
    )?;
    Ok(combine_with_signature(table, signature))
}

/// True for whole-disk NVMe names such as `nvme0n1` (not partitions like `nvme0n1p2`).
pub(crate) fn is_nvme_disk_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("nvme") else {
        return false;
    };
    let Some((controller, namespace)) = rest.split_once('n') else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    digits(controller) && digits(namespace)
}

/// Read the tables of every NVMe drive in the machine plus the given targets.
/// Aborts if any of them cannot be read.
fn scan_nvme_drives(targets: &[String]) -> std::result::Result<Vec<ScannedDrive>, LayoutError> {
    let entries = std::fs::read_dir("/dev").map_err(|e| LayoutError::Unreadable {
        device: "/dev".to_string(),
        reason: format!("cannot enumerate NVMe drives: {}", e),
    })?;

    let mut devices: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| is_nvme_disk_name(name))
        .map(|name| format!("/dev/{}", name))
        .chain(targets.iter().cloned())
        .collect();
    devices.sort();
    devices.dedup();

    devices
        .into_iter()
        .map(|device| {
            let table = read_table(&device)?;
            Ok(ScannedDrive { device, table })
        })
        .collect()
}

// ── Reconciling plan and disk (pure) ──────────────────────────────────────────

/// What stage 2 does to a drive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReconcileAction {
    /// The drive has no partitions: write the planned table.
    CreateAll,
    /// The drive already has the expected layout: change nothing.
    UseExisting,
    /// The user explicitly chose `WipeAll`: destroy these existing partitions
    /// and write the planned table.
    WipeAndCreate { destroyed: Vec<OnDiskPartition> },
}

/// Data partitions found on drives where the plan does not expect them.
///
/// `expected` maps each planned drive to the partition labels planned for it.
pub(crate) fn unexpected_data(
    scanned: &[ScannedDrive],
    expected: &[(String, Vec<String>)],
) -> Vec<ForeignData> {
    scanned
        .iter()
        .filter_map(|drive| {
            let planned_here: &[String] = expected
                .iter()
                .find(|(device, _)| *device == drive.device)
                .map(|(_, labels)| labels.as_slice())
                .unwrap_or(&[]);
            let labels: Vec<String> = drive
                .table
                .data_labels()
                .into_iter()
                .filter(|l| !planned_here.contains(l))
                .collect();
            (!labels.is_empty()).then(|| ForeignData {
                device: drive.device.clone(),
                labels,
            })
        })
        .collect()
}

/// Decide what to do with one drive. Never describes a write to a drive that
/// has partitions.
///
/// - data partitions where the plan does not expect them: error (also under
///   `WipeAll`; `foreign` must already exclude drives that are being wiped)
/// - no partitions on disk: `CreateAll`
/// - `WipeAll` and partitions on disk: `WipeAndCreate`, listing what is destroyed
/// - same labels, nodes and order as planned: `UseExisting`
/// - anything else: error (nothing is changed)
pub(crate) fn reconcile(
    device: &str,
    expected: &[PartitionState],
    on_disk: &OnDiskTable,
    foreign: &[ForeignData],
    policy: ExistingDataPolicy,
) -> std::result::Result<ReconcileAction, LayoutError> {
    if let Some(other) = foreign.first() {
        return Err(LayoutError::DataOnOtherDrive {
            device: device.to_string(),
            other: other.device.clone(),
            labels: other.labels.join(", "),
        });
    }

    if on_disk.is_blank() {
        return Ok(ReconcileAction::CreateAll);
    }

    if policy == ExistingDataPolicy::WipeAll {
        return Ok(ReconcileAction::WipeAndCreate {
            destroyed: on_disk.destroyed_by_wipe(device),
        });
    }

    let (label, partitions) = match on_disk {
        OnDiskTable::Table { label, partitions } => (label, partitions),
        OnDiskTable::Unrecognised { signature } => {
            return Err(LayoutError::Unrecognised {
                device: device.to_string(),
                signature: signature.clone(),
            })
        }
        // A table with no partitions is blank and returned `CreateAll` above.
        OnDiskTable::NoTable => unreachable!("blank table handled by the is_blank check"),
    };

    if label != "gpt" {
        return Err(LayoutError::NotGpt {
            device: device.to_string(),
            label: label.clone(),
        });
    }

    let expected_labels: Vec<String> = expected
        .iter()
        .map(|s| s.partition().label().as_str().to_string())
        .collect();
    let found_labels: Vec<String> = partitions
        .iter()
        .map(|p| p.name.clone().unwrap_or_else(|| "<unnamed>".to_string()))
        .collect();

    let labels_match = expected_labels == found_labels;
    let nodes_match = expected.len() == partitions.len()
        && expected
            .iter()
            .zip(partitions)
            .all(|(s, p)| s.partition().device.as_str() == p.node);

    if labels_match && nodes_match {
        return Ok(ReconcileAction::UseExisting);
    }

    let hint = if expected_labels.iter().any(|l| l == "boot")
        && !found_labels.iter().any(|l| l == "boot")
    {
        " (old layout without boot partition)"
    } else if labels_match {
        " (partition numbering differs)"
    } else {
        ""
    };

    Err(LayoutError::Mismatch {
        device: device.to_string(),
        expected: expected_labels.join(", "),
        found: found_labels.join(", "),
        hint: hint.to_string(),
    })
}

/// Reconcile one drive's planned partitions against what the scan found,
/// marking them `Exists` when the drive already has the layout.
fn reconcile_drive(
    device: &str,
    partitions: &mut [PartitionState],
    scanned: &[ScannedDrive],
    foreign: &[ForeignData],
    policy: ExistingDataPolicy,
) -> std::result::Result<(), LayoutError> {
    if partitions.is_empty() {
        // Nothing is planned for this drive, so it is never written.
        return Ok(());
    }

    let table = scanned
        .iter()
        .find(|d| d.device == device)
        .map(|d| &d.table)
        .ok_or_else(|| LayoutError::Unreadable {
            device: device.to_string(),
            reason: "drive was not found when scanning".to_string(),
        })?;

    match reconcile(device, partitions, table, foreign, policy)? {
        ReconcileAction::CreateAll => {
            tracing::info!("{} has no partitions, will create the full layout", device);
        }
        ReconcileAction::WipeAndCreate { destroyed } => {
            log_destroyed(device, &destroyed);
        }
        ReconcileAction::UseExisting => {
            tracing::info!(
                "{} already has the expected layout, leaving it as is",
                device
            );
            for state in partitions.iter_mut() {
                *state = PartitionState::Exists(state.partition().clone());
            }
        }
    }
    Ok(())
}

/// Log, loudly, everything a `WipeAll` is about to destroy on `device`.
fn log_destroyed(device: &str, destroyed: &[OnDiskPartition]) {
    tracing::warn!(
        "WIPE ALL requested by the user: {} will be repartitioned and ALL {} existing \
         partition(s) on it destroyed, music and metadata included:",
        device,
        destroyed.len()
    );
    for partition in destroyed {
        tracing::warn!("  WILL DESTROY {}", partition.describe());
    }
}

/// The partitions that will be destroyed by writing a new table to a drive
/// whose table was just re-read. Empty for a blank drive. A drive that has
/// partitions is only writable under `WipeAll`; otherwise this refuses.
pub(crate) fn partitions_to_destroy(
    device: &str,
    current: &OnDiskTable,
    policy: ExistingDataPolicy,
) -> std::result::Result<Vec<OnDiskPartition>, LayoutError> {
    if current.is_blank() {
        return Ok(Vec::new());
    }
    if policy == ExistingDataPolicy::WipeAll {
        Ok(current.destroyed_by_wipe(device))
    } else {
        Err(LayoutError::ChangedSincePlanning {
            device: device.to_string(),
        })
    }
}

/// A table is written all-or-nothing: either every planned partition is new,
/// or the plan is inconsistent and we refuse. Returns the number of partitions
/// to create (0 when everything already exists).
pub(crate) fn count_to_create(
    device: &str,
    partitions: &[PartitionState],
) -> std::result::Result<usize, LayoutError> {
    let planned = partitions
        .iter()
        .filter(|p| matches!(p, PartitionState::Planned(_)))
        .count();
    if planned != 0 && planned != partitions.len() {
        return Err(LayoutError::PartiallyExisting {
            device: device.to_string(),
        });
    }
    Ok(planned)
}

/// Erase the filesystem and partition-table signatures of the old partitions
/// and the disk itself (`wipefs -a`). Only called after the user's explicit
/// `WipeAll`. Any failure aborts before a new table is written.
async fn wipe_signatures(device: &str, destroyed: &[OnDiskPartition]) -> Result<()> {
    let targets = destroyed
        .iter()
        .map(|p| p.node.as_str())
        .chain(std::iter::once(device));
    for target in targets {
        tracing::warn!("wipefs -a {}", target);
        let output = tokio::process::Command::new("wipefs")
            .args(["-a", target])
            .output()
            .await
            .map_err(|e| {
                crate::error::BeaconError::Provisioning(format!("Failed to run wipefs: {}", e))
            })?;
        if !output.status.success() {
            return Err(crate::error::BeaconError::Provisioning(format!(
                "wipefs -a {} failed: {}",
                target,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
    }
    Ok(())
}

/// What an NVMe drive currently holds, for the confirmation page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingDrive {
    pub device: String,
    /// One description per partition (node, label, size).
    pub partitions: Vec<String>,
}

/// Read every NVMe drive and list those that have partitions. These are what
/// a `WipeAll` would destroy. Errors (unreadable table) are returned as text.
pub fn describe_existing_drives() -> std::result::Result<Vec<ExistingDrive>, String> {
    let scanned = scan_nvme_drives(&[]).map_err(|e| e.to_string())?;
    Ok(scanned
        .into_iter()
        .filter(|drive| !drive.table.is_blank())
        .map(|drive| ExistingDrive {
            partitions: drive
                .table
                .destroyed_by_wipe(&drive.device)
                .iter()
                .map(OnDiskPartition::describe)
                .collect(),
            device: drive.device,
        })
        .collect())
}

/// Devices and planned labels per drive of the plan.
fn expected_labels(plan: &PartitionPlan) -> Vec<(String, Vec<String>)> {
    let labels = |partitions: &[PartitionState]| -> Vec<String> {
        partitions
            .iter()
            .map(|s| s.partition().label().as_str().to_string())
            .collect()
    };
    match plan {
        PartitionPlan::SingleDrive { device, partitions } => {
            vec![(device.device.as_str().to_string(), labels(partitions))]
        }
        PartitionPlan::DualDrive {
            primary_device,
            primary_partitions,
            secondary_device,
            secondary_partitions,
        } => vec![
            (
                primary_device.device.as_str().to_string(),
                labels(primary_partitions),
            ),
            (
                secondary_device.device.as_str().to_string(),
                labels(secondary_partitions),
            ),
        ],
    }
}

/// Reconcile every drive of the plan against the scan. On success, drives that
/// already have the layout are marked `Exists`; the rest stay `Planned`.
///
/// Under `WipeAll`, data on a drive that the plan repartitions is the very data
/// the user chose to destroy, so it is not "foreign". Data on any drive the plan
/// does NOT repartition still aborts (enumeration-swap protection).
pub(crate) fn reconcile_plan(
    plan: &mut PartitionPlan,
    scanned: &[ScannedDrive],
    policy: ExistingDataPolicy,
) -> std::result::Result<(), LayoutError> {
    let expected = expected_labels(plan);
    let mut foreign = unexpected_data(scanned, &expected);
    if policy == ExistingDataPolicy::WipeAll {
        foreign.retain(|f| {
            !expected
                .iter()
                .any(|(device, labels)| *device == f.device && !labels.is_empty())
        });
    }
    match plan {
        PartitionPlan::SingleDrive { device, partitions } => reconcile_drive(
            device.device.as_str(),
            partitions,
            scanned,
            &foreign,
            policy,
        ),
        PartitionPlan::DualDrive {
            primary_device,
            primary_partitions,
            secondary_device,
            secondary_partitions,
        } => {
            reconcile_drive(
                primary_device.device.as_str(),
                primary_partitions,
                scanned,
                &foreign,
                policy,
            )?;
            reconcile_drive(
                secondary_device.device.as_str(),
                secondary_partitions,
                scanned,
                &foreign,
                policy,
            )
        }
    }
}

// ── Planning the layout (pure) ────────────────────────────────────────────────

/// Compute the partition layout wanted for these drives. Pure sizing; does not
/// look at the disks.
pub(crate) fn build_partition_plan(input: &ValidatedHardware) -> Result<PartitionPlan> {
    use crate::provisioning::types::{DevicePath, MountPoint, PartitionSize, UnitType};

    let primary_device = input.drives.primary().device.clone();
    let primary_size_bytes = input.drives.primary().size_bytes;

    // Constants
    const BOOT_SIZE_MB: u64 = 512;
    const ROOT_SIZE_GB: u64 = 16;
    const VAR_SIZE_GB: u64 = 8;
    const METADATA_SIZE_GB: u64 = 12; // ACID fact streams for music library metadata
    const MIN_MUSIC_SIZE_GB: u64 = 200; // Minimum viable music library size

    // Music assignment threshold: Primary must be 50% larger than secondary to justify shared drive
    const MUSIC_SIZE_THRESHOLD_RATIO: f64 = 1.5;

    let partitions = match input.config.unit_type {
        UnitType::Mdma909 | UnitType::Mdma101 => {
            if let Some(secondary) = input.drives.secondary() {
                // Two-drive config: Music goes on the larger drive (full remaining space)
                let os_overhead_gb = ROOT_SIZE_GB + VAR_SIZE_GB + METADATA_SIZE_GB;
                let primary_available_gb = primary_size_bytes
                    .gigabytes()
                    .saturating_sub(os_overhead_gb);
                let secondary_size_gb = secondary.size_bytes.gigabytes();
                let primary_total_gb = primary_size_bytes.gigabytes();

                // Check if primary is significantly larger (50%+) than secondary
                // Compare RAW drive sizes, not available space after OS overhead
                let primary_much_larger = (primary_total_gb as f64)
                    >= (secondary_size_gb as f64 * MUSIC_SIZE_THRESHOLD_RATIO);

                if primary_much_larger {
                    // Primary is 50%+ larger: Music on primary (all remaining space)
                    // Secondary left unpartitioned for future expansion
                    tracing::info!(
                        "Two-drive (primary larger): Music on primary ({}GB available after OS)",
                        primary_available_gb
                    );

                    let os_overhead = PartitionSize::from_gb(os_overhead_gb);
                    let remaining_bytes = primary_size_bytes.saturating_sub(os_overhead);

                    vec![
                        PartitionState::Planned(Partition {
                            device: DevicePath::new(format!("{}p1", primary_device))?,
                            mount_point: MountPoint::Boot,
                            size: PartitionSize::from_mb(BOOT_SIZE_MB),
                        }),
                        PartitionState::Planned(Partition {
                            device: DevicePath::new(format!("{}p2", primary_device))?,
                            mount_point: MountPoint::Root,
                            size: PartitionSize::from_gb(ROOT_SIZE_GB),
                        }),
                        PartitionState::Planned(Partition {
                            device: DevicePath::new(format!("{}p3", primary_device))?,
                            mount_point: MountPoint::Var,
                            size: PartitionSize::from_gb(VAR_SIZE_GB),
                        }),
                        PartitionState::Planned(Partition {
                            device: DevicePath::new(format!("{}p4", primary_device))?,
                            mount_point: MountPoint::Metadata,
                            size: PartitionSize::from_gb(METADATA_SIZE_GB),
                        }),
                        PartitionState::Planned(Partition {
                            device: DevicePath::new(format!("{}p5", primary_device))?,
                            mount_point: MountPoint::Music,
                            size: remaining_bytes, // ALL remaining on primary
                        }),
                    ]
                } else {
                    // Secondary is larger or equal: Music on secondary (full drive)
                    // Primary has OS + metadata only
                    tracing::info!(
                        "Two-drive (secondary larger/equal): Music on secondary ({}GB full drive)",
                        secondary_size_gb
                    );

                    vec![
                        PartitionState::Planned(Partition {
                            device: DevicePath::new(format!("{}p1", primary_device))?,
                            mount_point: MountPoint::Boot,
                            size: PartitionSize::from_mb(BOOT_SIZE_MB),
                        }),
                        PartitionState::Planned(Partition {
                            device: DevicePath::new(format!("{}p2", primary_device))?,
                            mount_point: MountPoint::Root,
                            size: PartitionSize::from_gb(ROOT_SIZE_GB),
                        }),
                        PartitionState::Planned(Partition {
                            device: DevicePath::new(format!("{}p3", primary_device))?,
                            mount_point: MountPoint::Var,
                            size: PartitionSize::from_gb(VAR_SIZE_GB),
                        }),
                        PartitionState::Planned(Partition {
                            device: DevicePath::new(format!("{}p4", primary_device))?,
                            mount_point: MountPoint::Metadata,
                            size: PartitionSize::from_gb(METADATA_SIZE_GB),
                        }),
                    ]
                }
            } else {
                // Single-drive config: Music gets ALL remaining space
                let os_overhead =
                    PartitionSize::from_gb(ROOT_SIZE_GB + VAR_SIZE_GB + METADATA_SIZE_GB);
                let remaining_bytes = primary_size_bytes.saturating_sub(os_overhead);
                let remaining_gb = remaining_bytes.gigabytes();

                // Check minimum
                if remaining_gb < MIN_MUSIC_SIZE_GB {
                    return Err(crate::error::BeaconError::Validation(
                        ValidationError::DriveToSmall(
                        format!(
                        "{} has only {}GB after OS partitions, need at least {}GB for music library",
                        primary_device,
                        remaining_gb,
                        MIN_MUSIC_SIZE_GB
                    ))));
                }

                tracing::info!(
                    "Single-drive partition sizing: {} total, {} OS overhead, {} music (remaining)",
                    primary_size_bytes,
                    os_overhead,
                    remaining_bytes
                );

                vec![
                    PartitionState::Planned(Partition {
                        device: DevicePath::new(format!("{}p1", primary_device))?,
                        mount_point: MountPoint::Boot,
                        size: PartitionSize::from_mb(BOOT_SIZE_MB),
                    }),
                    PartitionState::Planned(Partition {
                        device: DevicePath::new(format!("{}p2", primary_device))?,
                        mount_point: MountPoint::Root,
                        size: PartitionSize::from_gb(ROOT_SIZE_GB),
                    }),
                    PartitionState::Planned(Partition {
                        device: DevicePath::new(format!("{}p3", primary_device))?,
                        mount_point: MountPoint::Var,
                        size: PartitionSize::from_gb(VAR_SIZE_GB),
                    }),
                    PartitionState::Planned(Partition {
                        device: DevicePath::new(format!("{}p4", primary_device))?,
                        mount_point: MountPoint::Metadata,
                        size: PartitionSize::from_gb(METADATA_SIZE_GB),
                    }),
                    PartitionState::Planned(Partition {
                        device: DevicePath::new(format!("{}p5", primary_device))?,
                        mount_point: MountPoint::Music,
                        size: remaining_bytes, // ALL remaining space!
                    }),
                ]
            }
        }
        UnitType::Mdma303 => {
            // MDMA-303: Minimal + cache partition
            let os_overhead = PartitionSize::from_gb(ROOT_SIZE_GB + VAR_SIZE_GB);
            let remaining_bytes = primary_size_bytes.saturating_sub(os_overhead);
            let cache_size = remaining_bytes;

            vec![
                PartitionState::Planned(Partition {
                    device: DevicePath::new(format!("{}p1", primary_device))?,
                    mount_point: MountPoint::Root,
                    size: PartitionSize::from_gb(ROOT_SIZE_GB),
                }),
                PartitionState::Planned(Partition {
                    device: DevicePath::new(format!("{}p2", primary_device))?,
                    mount_point: MountPoint::Var,
                    size: PartitionSize::from_gb(VAR_SIZE_GB),
                }),
                PartitionState::Planned(Partition {
                    device: DevicePath::new(format!("{}p3", primary_device))?,
                    mount_point: MountPoint::Cache,
                    size: cache_size,
                }),
            ]
        }
    };

    // Check for secondary drive and assign based on size comparison
    let plan = if let Some(secondary) = input.drives.secondary() {
        let secondary_size_gb = secondary.size_bytes.gigabytes();
        let primary_total_gb = primary_size_bytes.gigabytes();

        // Use RAW drive size comparison (not available after OS overhead)
        let primary_much_larger =
            (primary_total_gb as f64) >= (secondary_size_gb as f64 * MUSIC_SIZE_THRESHOLD_RATIO);

        let secondary_partitions = if primary_much_larger {
            // Music on primary → secondary left unpartitioned for future expansion
            Vec::new()
        } else {
            // Music on secondary → secondary gets music (full drive, dedicated)
            vec![PartitionState::Planned(Partition {
                device: DevicePath::new(format!("{}p1", secondary.device))?,
                mount_point: MountPoint::Music,
                size: secondary.size_bytes,
            })]
        };

        PartitionPlan::DualDrive {
            primary_device: input.drives.primary().clone(),
            primary_partitions: partitions,
            secondary_device: secondary.clone(),
            secondary_partitions,
        }
    } else {
        // Single drive configuration
        PartitionPlan::SingleDrive {
            device: input.drives.primary().clone(),
            partitions,
        }
    };

    Ok(plan)
}

#[derive(Clone, Debug)]
pub struct PartitionDrivesAction;

impl Action<ValidatedHardware, PartitionedDrives, CompletedPartitionedDrives>
    for PartitionDrivesAction
{
    fn id(&self) -> ActionId {
        ActionId::new("partition-drives")
    }

    fn description(&self) -> String {
        "Partition NVMe drives".to_string()
    }

    async fn plan(
        &self,
        input: &ValidatedHardware,
    ) -> Result<PlannedAction<ValidatedHardware, PartitionedDrives, CompletedPartitionedDrives, Self>>
    {
        let mut plan = build_partition_plan(input)?;

        // Read every NVMe drive's partition table. If any read fails we abort:
        // a failed read is never treated as an empty disk.
        let targets: Vec<String> = expected_labels(&plan).into_iter().map(|(d, _)| d).collect();
        let scanned = scan_nvme_drives(&targets)?;
        for drive in &scanned {
            tracing::info!(
                "Found {} on {}",
                if drive.table.is_blank() {
                    "no partitions"
                } else {
                    "existing partitions or signatures"
                },
                drive.device
            );
        }

        // Existing drives are never rewritten: they either match the plan
        // (marked Exists, no-op) or provisioning aborts here, before anything
        // is changed.
        // Under an explicit `WipeAll` the target drives are instead marked for
        // a fresh layout and what they hold is logged as about to be destroyed.
        reconcile_plan(&mut plan, &scanned, input.config.existing_data)?;

        // Build the planned work WITH workflow state (PartitionState)
        let planned_work = PartitionedDrives {
            validated: input.clone(),
            plan,
        };

        // Build the assumed output WITHOUT workflow state (just Partition)
        let assumed_output = planned_work.clone().into_completed();

        Ok(PlannedAction {
            description: self.description(),
            action: self.clone(),
            input: input.clone(),
            planned_work,   // What apply() will use
            assumed_output, // What next stage receives
        })
    }

    async fn apply(
        &self,
        planned_output: &PartitionedDrives,
    ) -> Result<CompletedPartitionedDrives> {
        tracing::info!("Stage 2: Partition drives - executing plan");

        // Helper to write the partition table of an EMPTY drive with sfdisk
        async fn write_partition_table_sfdisk(
            device_path: &str,
            partitions: &[PartitionState],
            context: &str,
            policy: ExistingDataPolicy,
        ) -> Result<()> {
            use std::process::Stdio;
            use tokio::io::AsyncWriteExt;

            // Collect partitions that need to be created
            let mut planned_partitions = Vec::new();

            for (partition_num, partition_state) in (1..).zip(partitions.iter()) {
                match partition_state {
                    PartitionState::Planned(partition) => {
                        planned_partitions.push((partition_num, partition.clone()));
                    }
                    PartitionState::Exists(partition) => {
                        let log_msg = if context.is_empty() {
                            format!(
                                "Partition {} already exists: {} (label: {})",
                                partition_num,
                                partition.mount_point,
                                partition.label()
                            )
                        } else {
                            format!(
                                "{} partition {} already exists: {} (label: {})",
                                context,
                                partition_num,
                                partition.mount_point,
                                partition.label()
                            )
                        };
                        tracing::info!("{}", log_msg);
                    }
                }
            }

            // The table is written all-or-nothing. A mix of new and existing
            // partitions means the plan is inconsistent: refuse.
            if count_to_create(device_path, partitions)? == 0 {
                tracing::info!("All partitions already exist on {}", device_path);
                return Ok(());
            }

            // The disk may have changed since planning: re-read it right now
            // (aborting if unreadable). Without WipeAll only a verified empty
            // drive is written.
            let current = read_table(device_path)?;
            let destroyed = partitions_to_destroy(device_path, &current, policy)?;
            if !destroyed.is_empty() {
                log_destroyed(device_path, &destroyed);
                wipe_signatures(device_path, &destroyed).await?;
            }

            // Build sfdisk input - complete partition table
            let sfdisk_input = build_sfdisk_input(partitions);

            // Log what we're creating
            for (num, partition) in &planned_partitions {
                let log_msg = if context.is_empty() {
                    format!(
                        "Will create partition {}: {} ({} MB, label: {})",
                        num,
                        partition.mount_point,
                        partition.size.megabytes(),
                        partition.label()
                    )
                } else {
                    format!(
                        "Will create {} partition {}: {} ({} MB, label: {})",
                        context,
                        num,
                        partition.mount_point,
                        partition.size.megabytes(),
                        partition.label()
                    )
                };
                tracing::info!("{}", log_msg);
            }

            tracing::info!("Writing partition table to {} with sfdisk", device_path);
            tracing::debug!("sfdisk input:\n{}", sfdisk_input);

            // Run sfdisk with stdin
            // `--wipe*` always: erase stale filesystem signatures in the
            // areas the new partitions occupy, so nothing old survives a WipeAll.
            let mut child = tokio::process::Command::new("sfdisk")
                .args(["--wipe", "always", "--wipe-partitions", "always"])
                .arg(device_path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| {
                    crate::error::BeaconError::Provisioning(format!(
                        "Failed to spawn sfdisk: {}",
                        e
                    ))
                })?;

            // Write partition table to stdin
            if let Some(mut stdin) = child.stdin.take() {
                stdin
                    .write_all(sfdisk_input.as_bytes())
                    .await
                    .map_err(|e| {
                        crate::error::BeaconError::Provisioning(format!(
                            "Failed to write to sfdisk stdin: {}",
                            e
                        ))
                    })?;
                drop(stdin); // Close stdin to signal EOF
            }

            // Wait for sfdisk to complete
            let output = child.wait_with_output().await.map_err(|e| {
                crate::error::BeaconError::Provisioning(format!("Failed to wait for sfdisk: {}", e))
            })?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                return Err(crate::error::BeaconError::Provisioning(format!(
                    "sfdisk failed to create partitions on {}:\nstdout: {}\nstderr: {}",
                    device_path, stdout, stderr
                )));
            }

            tracing::info!(
                "Successfully created {} partition(s) on {}",
                planned_partitions.len(),
                device_path
            );

            // Wait for kernel to re-read partition table
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;

            Ok(())
        }

        // Process the plan based on drive configurtion
        let policy = planned_output.validated.config.existing_data;
        match &planned_output.plan {
            PartitionPlan::SingleDrive { device, partitions } => {
                write_partition_table_sfdisk(device.device.as_str(), partitions, "", policy)
                    .await?;
            }
            PartitionPlan::DualDrive {
                primary_device,
                primary_partitions,
                secondary_device,
                secondary_partitions,
            } => {
                write_partition_table_sfdisk(
                    primary_device.device.as_str(),
                    primary_partitions,
                    "primary",
                    policy,
                )
                .await?;

                write_partition_table_sfdisk(
                    secondary_device.device.as_str(),
                    secondary_partitions,
                    "secondary",
                    policy,
                )
                .await?;
            }
        }

        tracing::info!("Partition stage complete");
        Ok(planned_output.clone().into_completed())
    }
}

/// GPT type GUID for the boot partition (Microsoft Basic Data).
///
/// Pi 5 firmware refuses to consider a partition with the generic Linux
/// filesystem GUID (`0FC63DAF-...`) as a NVMe boot candidate, even if its
/// content is FAT32.  The Microsoft Basic Data GUID is required.
const BOOT_PARTITION_GPT_TYPE: &str = "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7";

/// Build the sfdisk input string for the given ordered list of partition states.
///
/// The boot partition receives the Microsoft Basic Data GPT type GUID so that
/// Pi 5 firmware will consider it as a NVMe boot candidate.  All other
/// partitions use the generic `linux` type alias.
pub(crate) fn build_sfdisk_input(partitions: &[PartitionState]) -> String {
    use crate::provisioning::types::MountPoint;
    let mut sfdisk_input = String::from("label: gpt\n");
    let mut start_mb = 1u64; // Start at 1MB for alignment

    for partition_state in partitions {
        let partition = partition_state.partition();
        let size_mb = partition.size.megabytes();

        let part_type = if partition.mount_point == MountPoint::Boot {
            BOOT_PARTITION_GPT_TYPE
        } else {
            "linux"
        };

        sfdisk_input.push_str(&format!(
            "start={}M, size={}M, type={}, name={}\n",
            start_mb,
            size_mb,
            part_type,
            partition.label()
        ));

        start_mb += size_mb;
    }

    sfdisk_input
}

#[cfg(test)]
mod tests {

    use crate::{
        provisioning::{DriveInfo, UnitType, ValidatedDrives},
        types::{DevicePath, ExistingDataPolicy, Hostname, SshPublicKey},
    };
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::provisioning::types::{MountPoint, PartitionSize};
    use rstest::rstest;
    use storage_primitives::ByteSize;

    // Helper to create a ValidatedDrives structure for testing
    fn create_validated_drives(primary_gb: u64, secondary_gb: Option<u64>) -> ValidatedHardware {
        let primary = DriveInfo {
            device: DevicePath::new("/dev/nvme0n1").expect("works"),
            size_bytes: ByteSize::from_gb(primary_gb),
            model: "Test Primary".to_string(),
        };

        let secondary = secondary_gb.map(|gb| DriveInfo {
            device: DevicePath::new("/dev/nvme1n1").expect("works"),
            size_bytes: ByteSize::from_gb(gb),
            model: "Test Secondary".to_string(),
        });

        ValidatedHardware {
            config: crate::provisioning::ProvisionConfig {
                hostname: Hostname::new("mdma-909".to_owned()).expect("works"),
                unit_type: UnitType::Mdma909,
                ssh_key: SshPublicKey::new("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIA96k1y1Y1326DtI4csBGXSqu57wjNuBYEkyjUQ3uS7x mdma-pi-access".to_owned()).expect("works"),
                existing_data: ExistingDataPolicy::Preserve,
            },
            drives: secondary
                .map(|s| ValidatedDrives::TwoDrives(primary.clone(), s))
                .unwrap_or_else(|| ValidatedDrives::OneDrive(primary.clone())),
        }
    }

    type PartitionSizes = Vec<(String, u64)>;

    // Helper to extract partition info for assertions
    fn get_partition_info(layout: &PartitionPlan) -> (PartitionSizes, Option<Vec<(String, u64)>>) {
        match layout {
            PartitionPlan::SingleDrive { partitions, .. } => {
                let primary_info: Vec<_> = partitions
                    .iter()
                    .map(|ps| {
                        let p = ps.partition();
                        (p.mount_point.to_string(), p.size.gigabytes())
                    })
                    .collect();
                (primary_info, None)
            }
            PartitionPlan::DualDrive {
                primary_partitions,
                secondary_partitions,
                ..
            } => {
                let primary_info: Vec<_> = primary_partitions
                    .iter()
                    .map(|ps| {
                        let p = ps.partition();
                        (p.mount_point.to_string(), p.size.gigabytes())
                    })
                    .collect();
                let secondary_info: Vec<_> = secondary_partitions
                    .iter()
                    .map(|ps| {
                        let p = ps.partition();
                        (p.mount_point.to_string(), p.size.gigabytes())
                    })
                    .collect();
                (primary_info, Some(secondary_info))
            }
        }
    }

    #[rstest]
    #[case::equal_drives_512gb(
        512, 512,
        "Equal drives → dedicated music on secondary (full drive)",
        vec![("/boot", 0), ("/", 16), ("/var", 8), ("/metadata", 12)],
        Some(vec![("/music", 512)])
    )]
    #[case::slightly_larger_primary_640gb(
        640, 512,
        "Primary 1.25× (640/512) → dedicated music on secondary (below 1.5× threshold)",
        vec![("/boot", 0), ("/", 16), ("/var", 8), ("/metadata", 12)],
        Some(vec![("/music", 512)])
    )]
    #[case::threshold_case_768gb(
        768, 512,
        "Primary 1.5× (768/512) → music on primary (exactly at threshold), secondary unpartitioned",
        vec![("/boot", 0), ("/", 16), ("/var", 8), ("/metadata", 12), ("/music", 732)],
        Some(Vec::new())  // Secondary left unpartitioned
    )]
    #[case::large_primary_1tb(
        1024, 512,
        "Primary 2.0× (1024/512) → music on primary (well above threshold), secondary unpartitioned",
        vec![("/boot", 0), ("/", 16), ("/var", 8), ("/metadata", 12), ("/music", 988)],
        Some(Vec::new())  // Secondary left unpartitioned
    )]
    #[case::huge_primary_2tb(
        2048, 512,
        "Primary 4.0× (2048/512) → music on primary (massive difference), secondary unpartitioned",
        vec![("/boot", 0), ("/", 16), ("/var", 8), ("/metadata", 12), ("/music", 2012)],
        Some(Vec::new())  // Secondary left unpartitioned
    )]
    #[case::just_below_threshold(
        730, 512,
        "Primary 1.43× (730/512) → dedicated music on secondary (just below 1.5×)",
        vec![("/boot", 0), ("/", 16), ("/var", 8), ("/metadata", 12)],
        Some(vec![("/music", 512)])
    )]
    #[case::just_above_threshold(
        800, 512,
        "Primary 1.56× (800/512) → music on primary (just above 1.5×), secondary unpartitioned",
        vec![("/boot", 0), ("/", 16), ("/var", 8), ("/metadata", 12), ("/music", 764)],
        Some(Vec::new())  // Secondary left unpartitioned
    )]
    fn two_drive_size_based_assignment(
        #[case] primary_gb: u64,
        #[case] secondary_gb: u64,
        #[case] description: &str,
        #[case] expected_primary: Vec<(&str, u64)>,
        #[case] expected_secondary: Option<Vec<(&str, u64)>>,
    ) {
        let input = create_validated_drives(primary_gb, Some(secondary_gb));

        let plan = build_partition_plan(&input).expect("Planning should succeed");
        let (primary_info, secondary_info) = get_partition_info(&plan);

        // Verify primary partitions
        assert_eq!(
            primary_info.len(),
            expected_primary.len(),
            "{}: Primary partition count mismatch",
            description
        );

        for (i, (mount, expected_gb)) in expected_primary.iter().enumerate() {
            let (actual_mount, actual_gb) = &primary_info[i];
            assert_eq!(
                actual_mount, mount,
                "{}: Primary partition {} mount point mismatch",
                description, i
            );
            assert_eq!(
                *actual_gb, *expected_gb,
                "{}: Primary partition {} size mismatch (expected {}GB, got {}GB)",
                description, i, expected_gb, actual_gb
            );
        }

        // Verify secondary partitions
        match (secondary_info, expected_secondary) {
            (Some(actual), Some(expected)) => {
                assert_eq!(
                    actual.len(),
                    expected.len(),
                    "{}: Secondary partition count mismatch",
                    description
                );

                for (i, (mount, expected_gb)) in expected.iter().enumerate() {
                    let (actual_mount, actual_gb) = &actual[i];
                    assert_eq!(
                        actual_mount, mount,
                        "{}: Secondary partition {} mount point mismatch",
                        description, i
                    );
                    assert_eq!(
                        *actual_gb, *expected_gb,
                        "{}: Secondary partition {} size mismatch (expected {}GB, got {}GB)",
                        description, i, expected_gb, actual_gb
                    );
                }
            }
            (None, None) => {}
            _ => panic!("{}: Secondary partition presence mismatch", description),
        }
    }

    #[rstest]
    #[case::standard_512gb(
        512,
        "Single 512GB drive - Music gets all remaining space",
        vec![("/boot", 0), ("/", 16), ("/var", 8), ("/metadata", 12), ("/music", 476)]
    )]
    #[case::large_1tb(
        1024,
        "Single 1TB drive - Music gets all remaining space",
        vec![("/boot", 0), ("/", 16), ("/var", 8), ("/metadata", 12), ("/music", 988)]
    )]
    fn single_drive_partitioning(
        #[case] drive_gb: u64,
        #[case] description: &str,
        #[case] expected: Vec<(&str, u64)>,
    ) {
        let input = create_validated_drives(drive_gb, None);

        let plan = build_partition_plan(&input).expect("Planning should succeed");
        let (primary_info, secondary_info) = get_partition_info(&plan);

        assert!(
            secondary_info.is_none(),
            "{}: Should be single drive",
            description
        );
        assert_eq!(
            primary_info.len(),
            expected.len(),
            "{}: Partition count mismatch",
            description
        );

        for (i, (mount, expected_gb)) in expected.iter().enumerate() {
            let (actual_mount, actual_gb) = &primary_info[i];
            assert_eq!(
                actual_mount, mount,
                "{}: PartitionState::Planned(Partition {} mount point mismatch",
                description, i
            );
            assert_eq!(
                *actual_gb, *expected_gb,
                "{}: PartitionState::Planned(Partition {} size mismatch (expected {}GB, got {}GB)",
                description, i, expected_gb, actual_gb
            );
        }
    }

    #[test]
    fn music_capacity_at_threshold() {
        // 768GB primary (exactly 1.5× after OS overhead threshold)
        let input = create_validated_drives(768, Some(512));

        let plan = build_partition_plan(&input).unwrap();
        let (primary_info, secondary_info) = get_partition_info(&plan);

        // Music should be on primary with all remaining space (656GB)
        let music_partition = primary_info.iter().find(|(mount, _)| mount == "/music");
        assert!(
            music_partition.is_some(),
            "Music partition should exist on primary"
        );
        assert_eq!(
            music_partition.unwrap().1,
            768 - 16 - 8 - 12,
            "Music partition should be 656GB (768 - 16 - 8 - 12)"
        );

        // Secondary should be unpartitioned (left for future expansion)
        let secondary = secondary_info.unwrap();
        assert_eq!(secondary.len(), 0, "Secondary should have no partitions");
    }

    #[test]
    fn just_below_threshold_keeps_dedicated() {
        // 730GB primary (1.43× after OS overhead) - just below 1.5×
        let input = create_validated_drives(730, Some(512));

        let plan = build_partition_plan(&input).unwrap();
        let (primary_info, secondary_info) = get_partition_info(&plan);

        // Music should be on secondary (dedicated)
        let music_on_primary = primary_info.iter().any(|(mount, _)| mount == "/music");
        assert!(
            !music_on_primary,
            "Music should NOT be on primary (below threshold)"
        );

        let secondary = secondary_info.unwrap();
        assert_eq!(secondary[0].0, "/music", "Secondary should have music");
        assert_eq!(secondary[0].1, 512, "Music should have full 512GB");
    }

    #[test]
    fn realistic_scenario_equal_drives() {
        // Most common case: 2× 512GB drives
        let input = create_validated_drives(512, Some(512));

        let plan = build_partition_plan(&input).unwrap();
        let (primary_info, secondary_info) = get_partition_info(&plan);

        // Should use clean separation pattern:
        // Primary: OS + metadata only (no music)
        // Secondary: Music (full drive)
        let music_on_primary = primary_info.iter().any(|(mount, _)| mount == "/music");
        assert!(
            !music_on_primary,
            "Music should NOT be on primary (equal drives → dedicated music on secondary)"
        );

        let music_on_secondary = secondary_info
            .as_ref()
            .unwrap()
            .iter()
            .any(|(mount, _)| mount == "/music");
        assert!(
            music_on_secondary,
            "Music should be on secondary (dedicated full drive)"
        );

        // Verify capacities
        let binding = secondary_info.unwrap();
        let music = binding.iter().find(|(m, _)| m == "/music").unwrap();
        assert_eq!(music.1, 512, "Music should get full 512GB dedicated");

        // Primary should have OS partitions only (boot, root, var, metadata)
        assert_eq!(
            primary_info.len(),
            4,
            "Primary should have 4 partitions (boot, root, var, metadata)"
        );
    }

    // ── Reconcile: helpers ────────────────────────────────────────────────────

    const FULL_LAYOUT: [MountPoint; 5] = [
        MountPoint::Boot,
        MountPoint::Root,
        MountPoint::Var,
        MountPoint::Metadata,
        MountPoint::Music,
    ];

    fn planned_on(device: &str, mounts: &[MountPoint]) -> Vec<PartitionState> {
        mounts
            .iter()
            .enumerate()
            .map(|(i, mount_point)| {
                PartitionState::Planned(Partition {
                    device: DevicePath::new(format!("{}p{}", device, i + 1)).unwrap(),
                    mount_point: *mount_point,
                    size: PartitionSize::from_gb(1),
                })
            })
            .collect()
    }

    fn planned(mounts: &[MountPoint]) -> Vec<PartitionState> {
        planned_on("/dev/nvme0n1", mounts)
    }

    /// `reconcile` under the default (`Preserve`) policy. The WipeAll tests call
    /// `super::reconcile` explicitly.
    fn reconcile(
        device: &str,
        expected: &[PartitionState],
        on_disk: &OnDiskTable,
        foreign: &[ForeignData],
    ) -> std::result::Result<ReconcileAction, LayoutError> {
        super::reconcile(
            device,
            expected,
            on_disk,
            foreign,
            ExistingDataPolicy::Preserve,
        )
    }

    /// `reconcile_plan` under the default (`Preserve`) policy.
    fn reconcile_plan(
        plan: &mut PartitionPlan,
        scanned: &[ScannedDrive],
    ) -> std::result::Result<(), LayoutError> {
        super::reconcile_plan(plan, scanned, ExistingDataPolicy::Preserve)
    }

    fn gpt_on(device: &str, names: &[&str]) -> OnDiskTable {
        OnDiskTable::Table {
            label: "gpt".to_string(),
            partitions: names
                .iter()
                .enumerate()
                .map(|(i, name)| OnDiskPartition {
                    node: format!("{}p{}", device, i + 1),
                    name: Some(name.to_string()),
                    size_bytes: None,
                })
                .collect(),
        }
    }

    fn gpt(names: &[&str]) -> OnDiskTable {
        gpt_on("/dev/nvme0n1", names)
    }

    // ── Reconcile: decisions ──────────────────────────────────────────────────

    #[test]
    fn reconcile_no_table_creates_all() {
        let action = reconcile(
            "/dev/nvme0n1",
            &planned(&FULL_LAYOUT),
            &OnDiskTable::NoTable,
            &[],
        );
        assert_eq!(action, Ok(ReconcileAction::CreateAll));
    }

    #[test]
    fn reconcile_empty_gpt_creates_all() {
        let action = reconcile("/dev/nvme0n1", &planned(&FULL_LAYOUT), &gpt(&[]), &[]);
        assert_eq!(action, Ok(ReconcileAction::CreateAll));
    }

    #[test]
    fn reconcile_identical_layout_uses_existing() {
        let on_disk = gpt(&["boot", "root", "var", "metadata", "music"]);
        let action = reconcile("/dev/nvme0n1", &planned(&FULL_LAYOUT), &on_disk, &[]);
        assert_eq!(action, Ok(ReconcileAction::UseExisting));
    }

    #[test]
    fn reconcile_ignores_sizes_on_disk_is_the_truth() {
        // Sizes are not even part of the on-disk model: a drive whose music
        // partition is a different size than recomputed is still accepted.
        let on_disk = gpt(&["boot", "root", "var", "metadata", "music"]);
        let mut expected = planned(&FULL_LAYOUT);
        if let Some(PartitionState::Planned(p)) = expected.last_mut() {
            p.size = PartitionSize::from_gb(999);
        }
        assert_eq!(
            reconcile("/dev/nvme0n1", &expected, &on_disk, &[]),
            Ok(ReconcileAction::UseExisting)
        );
    }

    #[test]
    fn reconcile_old_four_partition_layout_is_refused_and_says_music_untouched() {
        let on_disk = gpt(&["root", "var", "metadata", "music"]);
        let err = reconcile("/dev/nvme0n1", &planned(&FULL_LAYOUT), &on_disk, &[])
            .expect_err("old layout must be refused");
        let msg = err.to_string();
        assert!(msg.contains("incompatible"), "got: {msg}");
        assert!(msg.contains("without boot partition"), "got: {msg}");
        assert!(
            msg.contains("music and metadata were left untouched"),
            "got: {msg}"
        );
        assert!(msg.contains("no data was changed"), "got: {msg}");
    }

    #[test]
    fn reconcile_wrong_order_is_refused() {
        let on_disk = gpt(&["boot", "root", "var", "music", "metadata"]);
        assert!(matches!(
            reconcile("/dev/nvme0n1", &planned(&FULL_LAYOUT), &on_disk, &[]),
            Err(LayoutError::Mismatch { .. })
        ));
    }

    #[test]
    fn reconcile_unnamed_partitions_are_refused() {
        let on_disk = OnDiskTable::Table {
            label: "gpt".to_string(),
            partitions: vec![OnDiskPartition {
                node: "/dev/nvme0n1p1".to_string(),
                name: None,
                size_bytes: None,
            }],
        };
        assert!(matches!(
            reconcile("/dev/nvme0n1", &planned(&FULL_LAYOUT), &on_disk, &[]),
            Err(LayoutError::Mismatch { .. })
        ));
    }

    #[test]
    fn reconcile_dos_table_with_partitions_is_refused() {
        let on_disk = OnDiskTable::Table {
            label: "dos".to_string(),
            partitions: vec![OnDiskPartition {
                node: "/dev/nvme0n1p1".to_string(),
                name: None,
                size_bytes: None,
            }],
        };
        assert!(matches!(
            reconcile("/dev/nvme0n1", &planned(&FULL_LAYOUT), &on_disk, &[]),
            Err(LayoutError::NotGpt { .. })
        ));
    }

    #[test]
    fn reconcile_node_mismatch_is_refused() {
        let on_disk = gpt_on(
            "/dev/nvme1n1",
            &["boot", "root", "var", "metadata", "music"],
        );
        assert!(matches!(
            reconcile("/dev/nvme0n1", &planned(&FULL_LAYOUT), &on_disk, &[]),
            Err(LayoutError::Mismatch { .. })
        ));
    }

    #[test]
    fn reconcile_music_on_other_drive_is_refused_even_for_empty_target() {
        let foreign = vec![ForeignData {
            device: "/dev/nvme1n1".to_string(),
            labels: vec!["music".to_string()],
        }];
        let err = reconcile(
            "/dev/nvme0n1",
            &planned(&FULL_LAYOUT),
            &OnDiskTable::NoTable,
            &foreign,
        )
        .expect_err("music on another drive must abort");
        let msg = err.to_string();
        assert!(msg.contains("/dev/nvme1n1"), "got: {msg}");
        assert!(msg.contains("music"), "got: {msg}");
        assert!(msg.contains("left untouched"), "got: {msg}");
    }

    /// Property: without `WipeAll`, whatever is on the disk, `CreateAll` (the
    /// only action that writes a partition table) is returned only for a drive
    /// with no partitions at all, and `UseExisting` only for exactly the planned
    /// layout. `WipeAndCreate` is never returned. So no input can make stage 2
    /// write over an existing music/metadata partition by accident.
    ///
    /// With `WipeAll`, a drive with partitions yields exactly `WipeAndCreate`
    /// listing all of them, but only when no data was found on another drive.
    #[test]
    fn reconcile_never_writes_over_existing_partitions() {
        const NAMES: [Option<&str>; 8] = [
            Some("boot"),
            Some("root"),
            Some("var"),
            Some("metadata"),
            Some("music"),
            Some("cache"),
            Some("other"),
            None,
        ];

        // Every table of up to 5 partitions over the name alphabet.
        let mut tables: Vec<Vec<Option<&str>>> = vec![Vec::new()];
        let mut frontier: Vec<Vec<Option<&str>>> = vec![Vec::new()];
        for _ in 0..5 {
            frontier = frontier
                .iter()
                .flat_map(|prefix| {
                    NAMES.iter().map(move |n| {
                        let mut t = prefix.clone();
                        t.push(*n);
                        t
                    })
                })
                .collect();
            tables.extend(frontier.iter().cloned());
        }

        let expected = planned(&FULL_LAYOUT);
        let foreign_cases = [
            Vec::new(),
            vec![ForeignData {
                device: "/dev/nvme1n1".to_string(),
                labels: vec!["music".to_string()],
            }],
        ];
        let expected_names = ["boot", "root", "var", "metadata", "music"];

        for names in &tables {
            for label in ["gpt", "dos"] {
                for foreign in &foreign_cases {
                    let on_disk = OnDiskTable::Table {
                        label: label.to_string(),
                        partitions: names
                            .iter()
                            .enumerate()
                            .map(|(i, n)| OnDiskPartition {
                                node: format!("/dev/nvme0n1p{}", i + 1),
                                name: n.map(str::to_string),
                                size_bytes: None,
                            })
                            .collect(),
                    };

                    // WipeAll: foreign data always aborts; otherwise a drive
                    // with partitions is wiped, listing all of them.
                    let wipe = super::reconcile(
                        "/dev/nvme0n1",
                        &expected,
                        &on_disk,
                        foreign,
                        ExistingDataPolicy::WipeAll,
                    );
                    if !foreign.is_empty() {
                        assert!(wipe.is_err(), "WipeAll ignored foreign data: {names:?}");
                    } else if names.is_empty() {
                        assert_eq!(wipe, Ok(ReconcileAction::CreateAll));
                    } else {
                        let OnDiskTable::Table { partitions, .. } = &on_disk else {
                            unreachable!()
                        };
                        assert_eq!(
                            wipe,
                            Ok(ReconcileAction::WipeAndCreate {
                                destroyed: partitions.clone()
                            })
                        );
                    }

                    match reconcile("/dev/nvme0n1", &expected, &on_disk, foreign) {
                        Ok(ReconcileAction::WipeAndCreate { .. }) => {
                            panic!("WipeAndCreate without WipeAll: {names:?}")
                        }
                        Ok(ReconcileAction::CreateAll) => {
                            assert!(
                                names.is_empty() && foreign.is_empty(),
                                "CreateAll on non-empty disk: {names:?}"
                            );
                        }
                        Ok(ReconcileAction::UseExisting) => {
                            let found: Vec<&str> = names.iter().flatten().copied().collect();
                            assert!(
                                label == "gpt"
                                    && foreign.is_empty()
                                    && found == expected_names
                                    && names.len() == 5,
                                "UseExisting for unexpected disk: {label} {names:?}"
                            );
                        }
                        Err(_) => {}
                    }
                }
            }
        }
    }

    // ── Reconcile: whole plan and cross-drive scan ────────────────────────────

    fn scanned(device: &str, table: OnDiskTable) -> ScannedDrive {
        ScannedDrive {
            device: device.to_string(),
            table,
        }
    }

    fn dual_plan(secondary_music: bool) -> PartitionPlan {
        let primary_mounts: &[MountPoint] = if secondary_music {
            &[
                MountPoint::Boot,
                MountPoint::Root,
                MountPoint::Var,
                MountPoint::Metadata,
            ]
        } else {
            &FULL_LAYOUT
        };
        let drive = |dev: &str| DriveInfo {
            device: DevicePath::new(dev).unwrap(),
            size_bytes: ByteSize::from_gb(512),
            model: "t".to_string(),
        };
        PartitionPlan::DualDrive {
            primary_device: drive("/dev/nvme0n1"),
            primary_partitions: planned(primary_mounts),
            secondary_device: drive("/dev/nvme1n1"),
            secondary_partitions: if secondary_music {
                planned_on("/dev/nvme1n1", &[MountPoint::Music])
            } else {
                Vec::new()
            },
        }
    }

    #[test]
    fn reconcile_plan_fresh_drives_stay_planned() {
        let mut plan = dual_plan(true);
        let scan = vec![
            scanned("/dev/nvme0n1", OnDiskTable::NoTable),
            scanned("/dev/nvme1n1", OnDiskTable::NoTable),
        ];
        reconcile_plan(&mut plan, &scan).unwrap();
        assert_eq!(plan.planned_devices().len(), 5);
    }

    #[test]
    fn reconcile_plan_existing_layout_marks_everything_exists() {
        let mut plan = dual_plan(true);
        let scan = vec![
            scanned("/dev/nvme0n1", gpt(&["boot", "root", "var", "metadata"])),
            scanned("/dev/nvme1n1", gpt_on("/dev/nvme1n1", &["music"])),
        ];
        reconcile_plan(&mut plan, &scan).unwrap();
        assert!(plan.planned_devices().is_empty());
    }

    #[test]
    fn reconcile_plan_music_on_unplanned_drive_is_refused() {
        // Primary is planned to hold music; the secondary also carries music.
        // An enumeration swap must not partition the music drive as the OS drive.
        let mut plan = dual_plan(false);
        let scan = vec![
            scanned("/dev/nvme0n1", OnDiskTable::NoTable),
            scanned("/dev/nvme1n1", gpt_on("/dev/nvme1n1", &["music"])),
        ];
        let err = reconcile_plan(&mut plan, &scan).expect_err("must abort");
        assert!(matches!(err, LayoutError::DataOnOtherDrive { .. }));
    }

    #[test]
    fn reconcile_plan_swapped_drives_are_refused() {
        // The planned OS drive (nvme0n1) is blank, the planned music drive
        // (nvme1n1) is blank, but music lives on a third drive.
        let mut plan = dual_plan(true);
        let scan = vec![
            scanned("/dev/nvme0n1", OnDiskTable::NoTable),
            scanned("/dev/nvme1n1", OnDiskTable::NoTable),
            scanned(
                "/dev/nvme2n1",
                gpt_on("/dev/nvme2n1", &["metadata", "music"]),
            ),
        ];
        assert!(reconcile_plan(&mut plan, &scan).is_err());
    }

    #[test]
    fn reconcile_plan_unreadable_target_is_refused() {
        let mut plan = dual_plan(true);
        let scan = vec![scanned("/dev/nvme1n1", OnDiskTable::NoTable)];
        assert!(matches!(
            reconcile_plan(&mut plan, &scan),
            Err(LayoutError::Unreadable { .. })
        ));
    }

    #[test]
    fn unexpected_data_ignores_planned_labels_and_os_partitions() {
        let scan = vec![
            scanned(
                "/dev/nvme0n1",
                gpt(&["boot", "root", "var", "metadata", "music"]),
            ),
            scanned("/dev/nvme1n1", gpt_on("/dev/nvme1n1", &["root"])),
        ];
        let expected = vec![(
            "/dev/nvme0n1".to_string(),
            vec![
                "boot".to_string(),
                "root".to_string(),
                "var".to_string(),
                "metadata".to_string(),
                "music".to_string(),
            ],
        )];
        assert_eq!(unexpected_data(&scan, &expected), vec![]);
    }

    // ── Reading sfdisk output ─────────────────────────────────────────────────

    const SFDISK_JSON: &str = r#"{
        "partitiontable": {
            "label": "gpt",
            "id": "1B2C3D4E-0000-0000-0000-000000000000",
            "device": "/dev/nvme0n1",
            "unit": "sectors",
            "firstlba": 34,
            "lastlba": 1000000,
            "sectorsize": 512,
            "partitions": [
                {"node": "/dev/nvme0n1p1", "start": 2048, "size": 1048576,
                 "type": "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7", "name": "boot"},
                {"node": "/dev/nvme0n1p2", "start": 1050624, "size": 33554432,
                 "type": "0FC63DAF-8483-4772-8E79-3D69D8477DE4", "name": "root"}
            ]
        }
    }"#;

    #[test]
    fn parse_sfdisk_reads_gpt_names() {
        let table = parse_sfdisk("/dev/nvme0n1", true, SFDISK_JSON.as_bytes(), "").unwrap();
        let OnDiskTable::Table { label, partitions } = table else {
            panic!("expected a table");
        };
        assert_eq!(label, "gpt");
        let found: Vec<(&str, Option<&str>)> = partitions
            .iter()
            .map(|p| (p.node.as_str(), p.name.as_deref()))
            .collect();
        assert_eq!(
            found,
            vec![
                ("/dev/nvme0n1p1", Some("boot")),
                ("/dev/nvme0n1p2", Some("root"))
            ]
        );
        // 1048576 sectors of 512 bytes = 512 MiB
        assert_eq!(partitions[0].size_bytes, Some(512 * 1024 * 1024));
        assert!(partitions[0].describe().contains("512 MB"));
    }

    #[test]
    fn parse_sfdisk_gpt_without_partitions_is_empty() {
        let json = r#"{"partitiontable": {"label": "gpt", "device": "/dev/nvme0n1"}}"#;
        let table = parse_sfdisk("/dev/nvme0n1", true, json.as_bytes(), "").unwrap();
        assert!(table.is_blank());
    }

    // ── Whole-disk signatures (no table is not necessarily blank) ─────────────

    #[test]
    fn signature_probe_empty_output_is_no_signature() {
        assert_eq!(
            interpret_signature_probe("/dev/nvme0n1", true, "\n", ""),
            Ok(None)
        );
    }

    #[test]
    fn signature_probe_lists_signatures() {
        assert_eq!(
            interpret_signature_probe("/dev/nvme0n1", true, "crypto_LUKS\n", ""),
            Ok(Some("crypto_LUKS".to_string()))
        );
    }

    #[test]
    fn signature_probe_failure_is_an_error_not_blank() {
        assert!(matches!(
            interpret_signature_probe("/dev/nvme0n1", false, "", "wipefs: busy"),
            Err(LayoutError::Unreadable { .. })
        ));
    }

    #[test]
    fn no_table_with_signature_is_not_blank() {
        let t = combine_with_signature(OnDiskTable::NoTable, Some("ext4".to_string()));
        assert_eq!(
            t,
            OnDiskTable::Unrecognised {
                signature: "ext4".to_string()
            }
        );
        assert!(!t.is_blank());
        assert!(combine_with_signature(OnDiskTable::NoTable, None).is_blank());
        // A real table is never overridden by the probe.
        assert_eq!(
            combine_with_signature(gpt(&["boot"]), Some("gpt".into())),
            gpt(&["boot"])
        );
    }

    fn unrecognised() -> OnDiskTable {
        OnDiskTable::Unrecognised {
            signature: "ext4".to_string(),
        }
    }

    #[rstest]
    #[case(ExistingDataPolicy::Preserve)]
    #[case(ExistingDataPolicy::ReinstallOs)]
    fn reconcile_whole_disk_signature_is_refused(#[case] policy: ExistingDataPolicy) {
        let err = super::reconcile(
            "/dev/nvme0n1",
            &planned(&FULL_LAYOUT),
            &unrecognised(),
            &[],
            policy,
        )
        .expect_err("a disk with a signature is not blank");
        assert!(matches!(err, LayoutError::Unrecognised { .. }));
        assert!(err.to_string().contains("no data was changed"));
    }

    #[test]
    fn reconcile_whole_disk_signature_is_destroyed_by_wipe_all() {
        let action = super::reconcile(
            "/dev/nvme0n1",
            &planned(&FULL_LAYOUT),
            &unrecognised(),
            &[],
            ExistingDataPolicy::WipeAll,
        )
        .unwrap();
        let ReconcileAction::WipeAndCreate { destroyed } = action else {
            panic!("expected WipeAndCreate");
        };
        assert_eq!(destroyed.len(), 1);
        assert_eq!(destroyed[0].node, "/dev/nvme0n1");
        assert!(destroyed[0].describe().contains("ext4"));
    }

    // ── WipeAll ───────────────────────────────────────────────────────────────

    const WIPE: ExistingDataPolicy = ExistingDataPolicy::WipeAll;

    #[test]
    fn wipe_all_on_existing_layout_returns_wipe_and_create_listing_destroyed() {
        let on_disk = gpt(&["boot", "root", "var", "metadata", "music"]);
        let action =
            super::reconcile("/dev/nvme0n1", &planned(&FULL_LAYOUT), &on_disk, &[], WIPE).unwrap();
        let OnDiskTable::Table { partitions, .. } = &on_disk else {
            unreachable!()
        };
        assert_eq!(
            action,
            ReconcileAction::WipeAndCreate {
                destroyed: partitions.clone()
            }
        );
    }

    #[test]
    fn wipe_all_on_incompatible_and_dos_layouts_wipes() {
        for on_disk in [
            gpt(&["root", "var", "metadata", "music"]),
            OnDiskTable::Table {
                label: "dos".to_string(),
                partitions: vec![OnDiskPartition {
                    node: "/dev/nvme0n1p1".to_string(),
                    name: None,
                    size_bytes: None,
                }],
            },
        ] {
            assert!(matches!(
                super::reconcile("/dev/nvme0n1", &planned(&FULL_LAYOUT), &on_disk, &[], WIPE),
                Ok(ReconcileAction::WipeAndCreate { .. })
            ));
        }
    }

    #[test]
    fn wipe_all_on_blank_drive_just_creates() {
        assert_eq!(
            super::reconcile(
                "/dev/nvme0n1",
                &planned(&FULL_LAYOUT),
                &OnDiskTable::NoTable,
                &[],
                WIPE
            ),
            Ok(ReconcileAction::CreateAll)
        );
    }

    #[test]
    fn wipe_all_still_aborts_on_music_on_unexpected_other_drive() {
        let foreign = vec![ForeignData {
            device: "/dev/nvme1n1".to_string(),
            labels: vec!["music".to_string()],
        }];
        let on_disk = gpt(&["boot", "root"]);
        assert!(matches!(
            super::reconcile(
                "/dev/nvme0n1",
                &planned(&FULL_LAYOUT),
                &on_disk,
                &foreign,
                WIPE
            ),
            Err(LayoutError::DataOnOtherDrive { .. })
        ));
    }

    #[test]
    fn wipe_all_plan_with_unreadable_target_aborts() {
        let mut plan = dual_plan(true);
        let scan = vec![scanned("/dev/nvme1n1", OnDiskTable::NoTable)];
        assert!(matches!(
            super::reconcile_plan(&mut plan, &scan, WIPE),
            Err(LayoutError::Unreadable { .. })
        ));
    }

    #[test]
    fn wipe_all_plan_wipes_target_drives_data_but_not_other_drives() {
        // Primary holds old music (the user wants it gone); secondary is a
        // planned target too and also gets wiped.
        let mut plan = dual_plan(true);
        let scan = vec![
            scanned(
                "/dev/nvme0n1",
                gpt(&["boot", "root", "var", "metadata", "music"]),
            ),
            scanned("/dev/nvme1n1", gpt_on("/dev/nvme1n1", &["music"])),
        ];
        super::reconcile_plan(&mut plan, &scan, WIPE).unwrap();
        // Everything stays Planned: all of it will be created (and formatted).
        assert_eq!(plan.planned_devices().len(), 5);
    }

    #[test]
    fn wipe_all_plan_aborts_when_music_is_on_a_drive_not_being_repartitioned() {
        // dual_plan(false): music planned on primary, secondary left alone.
        // Secondary carries music: not a target, so it must abort even though
        // the user chose WipeAll.
        let mut plan = dual_plan(false);
        let scan = vec![
            scanned("/dev/nvme0n1", gpt(&["boot", "root"])),
            scanned("/dev/nvme1n1", gpt_on("/dev/nvme1n1", &["music"])),
        ];
        assert!(matches!(
            super::reconcile_plan(&mut plan, &scan, WIPE),
            Err(LayoutError::DataOnOtherDrive { .. })
        ));
    }

    #[test]
    fn preserve_plan_with_data_on_planned_target_but_wrong_drive_still_aborts() {
        // Without WipeAll, music sitting on a drive where the plan does not
        // expect it aborts, targets included.
        let mut plan = dual_plan(true); // primary: OS only
        let scan = vec![
            scanned(
                "/dev/nvme0n1",
                gpt(&["boot", "root", "var", "metadata", "music"]),
            ),
            scanned("/dev/nvme1n1", OnDiskTable::NoTable),
        ];
        assert!(super::reconcile_plan(&mut plan, &scan, ExistingDataPolicy::ReinstallOs).is_err());
    }

    #[rstest]
    #[case(ExistingDataPolicy::Preserve)]
    #[case(ExistingDataPolicy::ReinstallOs)]
    fn write_guard_refuses_drive_with_partitions_without_wipe_all(
        #[case] policy: ExistingDataPolicy,
    ) {
        assert!(matches!(
            partitions_to_destroy("/dev/nvme0n1", &gpt(&["music"]), policy),
            Err(LayoutError::ChangedSincePlanning { .. })
        ));
        assert!(matches!(
            partitions_to_destroy("/dev/nvme0n1", &unrecognised(), policy),
            Err(LayoutError::ChangedSincePlanning { .. })
        ));
        assert_eq!(
            partitions_to_destroy("/dev/nvme0n1", &OnDiskTable::NoTable, policy),
            Ok(Vec::new())
        );
    }

    #[test]
    fn write_guard_lists_everything_destroyed_under_wipe_all() {
        let destroyed =
            partitions_to_destroy("/dev/nvme0n1", &gpt(&["metadata", "music"]), WIPE).unwrap();
        assert_eq!(destroyed.len(), 2);
        assert_eq!(destroyed[1].name.as_deref(), Some("music"));
    }

    #[test]
    fn partition_description_names_label_and_size() {
        let p = OnDiskPartition {
            node: "/dev/nvme0n1p5".to_string(),
            name: Some("music".to_string()),
            size_bytes: Some(3 * 1024 * 1024 * 1024),
        };
        assert_eq!(p.describe(), "/dev/nvme0n1p5 (label: music, size: 3072 MB)");
    }

    // ── All-or-nothing table writes ───────────────────────────────────────────

    #[test]
    fn count_to_create_all_planned_or_all_existing() {
        let all_planned = planned(&FULL_LAYOUT);
        assert_eq!(count_to_create("/dev/nvme0n1", &all_planned), Ok(5));

        let all_exist: Vec<PartitionState> = all_planned
            .iter()
            .map(|s| PartitionState::Exists(s.partition().clone()))
            .collect();
        assert_eq!(count_to_create("/dev/nvme0n1", &all_exist), Ok(0));
        assert_eq!(count_to_create("/dev/nvme0n1", &[]), Ok(0));
    }

    #[test]
    fn count_to_create_refuses_a_mix() {
        let mut mixed = planned(&FULL_LAYOUT);
        mixed[0] = PartitionState::Exists(mixed[0].partition().clone());
        assert!(matches!(
            count_to_create("/dev/nvme0n1", &mixed),
            Err(LayoutError::PartiallyExisting { .. })
        ));
    }

    #[test]
    fn cache_label_counts_as_data() {
        let t = gpt(&["boot", "cache"]);
        assert_eq!(t.data_labels(), vec!["cache".to_string()]);
    }

    #[test]
    fn parse_sfdisk_no_partition_table_is_empty_disk() {
        let table = parse_sfdisk(
            "/dev/nvme0n1",
            false,
            b"",
            "sfdisk: /dev/nvme0n1: does not contain a recognized partition table",
        )
        .unwrap();
        assert_eq!(table, OnDiskTable::NoTable);
    }

    #[test]
    fn parse_sfdisk_other_failure_is_an_error_not_an_empty_disk() {
        let err = parse_sfdisk(
            "/dev/nvme0n1",
            false,
            b"",
            "sfdisk: cannot open /dev/nvme0n1: Device or resource busy",
        )
        .expect_err("a failed read must abort");
        assert!(matches!(err, LayoutError::Unreadable { .. }));
        assert!(err.to_string().contains("left untouched"));
    }

    #[test]
    fn parse_sfdisk_garbage_is_an_error() {
        assert!(parse_sfdisk("/dev/nvme0n1", true, b"not json", "").is_err());
        assert!(parse_sfdisk("/dev/nvme0n1", true, b"", "").is_err());
    }

    #[rstest]
    #[case("nvme0n1", true)]
    #[case("nvme1n1", true)]
    #[case("nvme10n2", true)]
    #[case("nvme0n1p2", false)]
    #[case("nvme", false)]
    #[case("nvme0", false)]
    #[case("nvmen1", false)]
    #[case("sda", false)]
    fn nvme_disk_name_detection(#[case] name: &str, #[case] expected: bool) {
        assert_eq!(is_nvme_disk_name(name), expected);
    }

    // ── sfdisk GPT type GUID tests ────────────────────────────────────────────

    /// Boot partition must use the Microsoft Basic Data GUID so Pi 5 firmware
    /// treats it as a NVMe boot candidate.
    #[test]
    fn sfdisk_boot_partition_uses_microsoft_basic_data_guid() {
        let partitions = planned(&[MountPoint::Boot, MountPoint::Root, MountPoint::Var]);

        let output = build_sfdisk_input(&partitions);

        // Boot partition must carry the Microsoft Basic Data GUID
        assert!(
            output.contains("EBD0A0A2-B9E5-4433-87C0-68B6B72699C7"),
            "boot partition must use Microsoft Basic Data GUID, got:\n{}",
            output
        );

        // Non-boot partitions must use plain linux type
        let non_boot_lines: Vec<&str> = output
            .lines()
            .filter(|l| l.contains("name=root") || l.contains("name=var"))
            .collect();
        assert!(
            !non_boot_lines.is_empty(),
            "expected root/var lines in sfdisk output"
        );
        for line in &non_boot_lines {
            assert!(
                line.contains("type=linux"),
                "non-boot partition should have type=linux, got: {}",
                line
            );
        }
    }

    /// The boot partition line must NOT contain the generic linux type.
    #[test]
    fn sfdisk_boot_partition_does_not_use_linux_type() {
        let partitions = planned(&[MountPoint::Boot]);

        let output = build_sfdisk_input(&partitions);

        let boot_line = output
            .lines()
            .find(|l| l.contains("name=boot"))
            .expect("expected a boot line");
        assert!(
            !boot_line.contains("type=linux"),
            "boot partition must not have type=linux, got: {}",
            boot_line
        );
    }
}
