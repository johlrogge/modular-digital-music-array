//! Reading block devices: partition tables (`sfdisk`, `wipefs`) and filesystem
//! signatures (`blkid`). Read-only; shared by the provisioning stages and the
//! network fallback, so neither depends on the other.

use crate::types::MountPoint;
use serde::Deserialize;
use std::process::Command;

// ── Errors ────────────────────────────────────────────────────────────────────

/// A device could not be read. A failed read is never mistaken for an empty disk.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Cannot read the partition table of {device}: {reason}")]
pub(crate) struct ReadError {
    pub device: String,
    pub reason: String,
}

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
    pub(crate) fn is_blank(&self) -> bool {
        match self {
            OnDiskTable::NoTable => true,
            OnDiskTable::Unrecognised { .. } => false,
            OnDiskTable::Table { partitions, .. } => partitions.is_empty(),
        }
    }

    /// What a wipe of this drive destroys, as pseudo-partitions for display
    /// and signature wiping. A whole-disk signature is listed as the disk.
    pub(crate) fn destroyed_by_wipe(&self, device: &str) -> Vec<OnDiskPartition> {
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
    pub(crate) fn data_labels(&self) -> Vec<String> {
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
) -> Result<OnDiskTable, ReadError> {
    if !success {
        if stderr.contains("does not contain a recognized partition table") {
            return Ok(OnDiskTable::NoTable);
        }
        return Err(ReadError {
            device: device.to_string(),
            reason: format!("sfdisk failed: {}", stderr.trim()),
        });
    }

    let parsed: SfdiskOutput = serde_json::from_slice(stdout).map_err(|e| ReadError {
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
) -> Result<Option<String>, ReadError> {
    if !success {
        return Err(ReadError {
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
pub(crate) fn read_table(device: &str) -> Result<OnDiskTable, ReadError> {
    let output = Command::new("sfdisk")
        .args(["--json", device])
        .output()
        .map_err(|e| ReadError {
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
        .map_err(|e| ReadError {
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
pub(crate) fn scan_nvme_drives(targets: &[String]) -> Result<Vec<ScannedDrive>, ReadError> {
    let entries = std::fs::read_dir("/dev").map_err(|e| ReadError {
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

// ── Filesystem signatures (`blkid`) ───────────────────────────────────────────

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

/// Arguments for the blkid probe that [`interpret_blkid`] understands.
pub(crate) fn blkid_args(device: &str) -> [&str; 8] {
    ["-p", "-o", "export", "-s", "TYPE", "-s", "LABEL", device]
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

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

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

    #[test]
    fn signature_probe_failure_is_an_error_not_blank() {
        assert!(interpret_signature_probe("/dev/nvme0n1", false, "", "wipefs: busy").is_err());
    }

    #[test]
    fn read_error_names_the_device() {
        let err = parse_sfdisk("/dev/nvme0n1", false, b"", "sfdisk: busy").unwrap_err();
        assert!(err.to_string().contains("/dev/nvme0n1"));
    }
}
