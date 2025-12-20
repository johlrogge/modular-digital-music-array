// bases/beacon/src/provisioning/types.rs
//! Pipeline stage types
//!
//! Each type represents a stage in the provisioning pipeline.
//! The type system ensures stages are executed in order.

use crate::hardware::HardwareInfo;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Configuration for provisioning
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvisionConfig {
    pub hostname: String,
    pub unit_type: UnitType,
    pub wifi_config: Option<WifiConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum UnitType {
    Mdma909,
    Mdma101,
    Mdma303,
}

impl std::fmt::Display for UnitType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnitType::Mdma909 => write!(f, "MDMA-909"),
            UnitType::Mdma101 => write!(f, "MDMA-101"),
            UnitType::Mdma303 => write!(f, "MDMA-303"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WifiConfig {
    pub ssid: String,
    pub password: String,
}

// ============================================================================
// Stage 0: Safety Check
// ============================================================================

/// Hardware that has been verified to be a Raspberry Pi
///
/// This is the output of the safety check. The only way to construct this
/// type is through CheckRaspberryPiAction in APPLY mode, which verifies
/// /proc/cpuinfo contains "Raspberry Pi".
#[derive(Debug, Clone, PartialEq)]
pub struct SafeHardware {
    pub info: HardwareInfo,
}

impl std::fmt::Display for SafeHardware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "✅ Raspberry Pi verified: {}", self.info.model)
    }
}

// ============================================================================
// Stage 1: Validation
// ============================================================================

/// Hardware that has been validated for provisioning
///
/// This type can only be constructed after validating:
/// - NVMe drives exist
/// - Drive configuration matches unit requirements
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedHardware {
    pub config: ProvisionConfig,
    pub drives: ValidatedDrives,
}

impl std::fmt::Display for ValidatedHardware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "✅ Validated {} with {}",
            self.config.unit_type, self.drives
        )
    }
}

/// Validated drive configuration
#[derive(Debug, Clone, PartialEq)]
pub enum ValidatedDrives {
    /// Single NVMe drive (MDMA-101, MDMA-303)
    OneDrive(DriveInfo),
    /// Two NVMe drives (MDMA-909)
    TwoDrives(DriveInfo, DriveInfo),
}

impl ValidatedDrives {
    pub fn primary(&self) -> &DriveInfo {
        match self {
            ValidatedDrives::OneDrive(drive) => drive,
            ValidatedDrives::TwoDrives(primary, _) => primary,
        }
    }

    pub fn secondary(&self) -> Option<&DriveInfo> {
        match self {
            ValidatedDrives::OneDrive(_) => None,
            ValidatedDrives::TwoDrives(_, secondary) => Some(secondary),
        }
    }
}

impl std::fmt::Display for ValidatedDrives {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidatedDrives::OneDrive(drive) => {
                write!(f, "Validated 1 drive: {}", drive)
            }
            ValidatedDrives::TwoDrives(primary, secondary) => {
                write!(f, "Validated 2 drives: {}, {}", primary, secondary)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DriveInfo {
    pub device: String,
    pub size_bytes: u64,
    pub model: String,
}

impl std::fmt::Display for DriveInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({} GB, model: {})",
            self.device,
            self.size_bytes / 1_000_000_000,
            self.model
        )
    }
}

// ============================================================================
// Stage 2: Partitioning
// ============================================================================

/// Drives that have been partitioned
///
/// This type can only be constructed after creating partitions
/// using parted/gdisk.
#[derive(Debug, Clone, PartialEq)]
pub struct PartitionedDrives {
    pub validated: ValidatedHardware,
    pub layout: PartitionLayout,
}

impl std::fmt::Display for PartitionedDrives {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "✅ Partitioned drives:\n{}", self.layout)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PartitionLayout {
    SingleDrive {
        device: String,
        partitions: Vec<Partition>,
    },
    DualDrive {
        primary_device: String,
        primary_partitions: Vec<Partition>,
        secondary_device: String,
        secondary_partitions: Vec<Partition>,
    },
}

impl std::fmt::Display for PartitionLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PartitionLayout::SingleDrive { device, partitions } => {
                writeln!(f, "Single drive: {}", device)?;
                for partition in partitions {
                    writeln!(f, "  {}", partition)?;
                }
                Ok(())
            }
            PartitionLayout::DualDrive {
                primary_device,
                primary_partitions,
                secondary_device,
                secondary_partitions,
            } => {
                writeln!(f, "Primary drive: {}", primary_device)?;
                for partition in primary_partitions {
                    writeln!(f, "  {}", partition)?;
                }
                writeln!(f, "Secondary drive: {}", secondary_device)?;
                for partition in secondary_partitions {
                    writeln!(f, "  {}", partition)?;
                }
                Ok(())
            }
        }
    }
}

/// Device path newtype
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevicePath(pub String);

impl std::fmt::Display for DevicePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Mount point newtype
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountPoint(pub &'static str);

impl std::fmt::Display for MountPoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Partition label newtype
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionLabel(pub &'static str);

impl std::fmt::Display for PartitionLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Partition size in bytes with smart display formatting
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartitionSize(pub u64);

impl PartitionSize {
    pub const fn from_gb(gb: u64) -> Self {
        Self(gb * 1_000_000_000)
    }

    pub const fn from_mb(mb: u64) -> Self {
        Self(mb * 1_000_000)
    }

    pub fn bytes(&self) -> u64 {
        self.0
    }

    pub fn gigabytes(&self) -> u64 {
        self.0 / 1_000_000_000
    }
}

impl std::fmt::Display for PartitionSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        const MB: u64 = 1_000_000;
        const GB: u64 = 1_000_000_000;
        const TB: u64 = 1_000_000_000_000;

        if self.0 >= TB {
            let tb = self.0 as f64 / TB as f64;
            write!(f, "{:.1} TB", tb)
        } else if self.0 >= GB {
            write!(f, "{} GB", self.0 / GB)
        } else if self.0 >= MB {
            write!(f, "{} MB", self.0 / MB)
        } else {
            write!(f, "{} bytes", self.0)
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Partition {
    pub device: DevicePath,
    pub mount_point: MountPoint,
    pub label: PartitionLabel,
    pub size: PartitionSize,
}

impl std::fmt::Display for Partition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} → {} ({}, label: {})",
            self.device, self.mount_point, self.size, self.label
        )
    }
}

impl Partition {
    /// Format this partition for formatting preview (as opposed to partitioning)
    pub fn format_display(&self) -> String {
        format!("{} -> ext4 (label: {})", self.device, self.label)
    }
}

// ============================================================================
// Stage 3: Formatting
// ============================================================================

/// Partitions that have been formatted with filesystems
///
/// This type can only be constructed after formatting partitions
/// with mkfs.ext4.
#[derive(Debug, Clone, PartialEq)]
pub struct FormattedSystem {
    pub partitioned: PartitionedDrives,
}

impl std::fmt::Display for FormattedSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "✅ Formatted filesystems on all partitions")
    }
}

// ============================================================================
// Stage 4: Installation
// ============================================================================

/// System with OS installed
///
/// This type can only be constructed after:
/// - Mounting partitions
/// - Installing base system
/// - Installing bootloader
#[derive(Debug, Clone, PartialEq)]
pub struct InstalledSystem {
    pub formatted: FormattedSystem,
    pub mount_point: PathBuf,
}

impl std::fmt::Display for InstalledSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "✅ OS installed at {:?}", self.mount_point)
    }
}

// ============================================================================
// Stage 5: Configuration
// ============================================================================

/// System that has been configured
///
/// This type can only be constructed after:
/// - Setting hostname
/// - Configuring network
/// - Setting up users
/// - Installing packages
#[derive(Debug, Clone, PartialEq)]
pub struct ConfiguredSystem {
    pub installed: InstalledSystem,
}

impl std::fmt::Display for ConfiguredSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "✅ System configured and ready")
    }
}

// ============================================================================
// Stage 6: Finalization
// ============================================================================

/// Fully provisioned system ready to boot
///
/// This is the final stage output.
#[derive(Debug, Clone, PartialEq)]
pub struct ProvisionedSystem {
    pub configured: ConfiguredSystem,
    pub summary: ProvisioningSummary,
}

impl std::fmt::Display for ProvisionedSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "✅ {} ({}) fully provisioned on {}",
            self.summary.hostname, self.summary.unit_type, self.summary.primary_drive
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProvisioningSummary {
    pub hostname: String,
    pub unit_type: UnitType,
    pub primary_drive: String,
    pub secondary_drive: Option<String>,
    pub total_partitions: usize,
}
