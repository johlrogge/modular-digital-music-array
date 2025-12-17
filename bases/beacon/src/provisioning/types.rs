// bases/beacon/src/provisioning/types.rs
//! Pipeline stage types
//!
//! Each type represents a stage in the provisioning pipeline.
//! The type system ensures stages are executed in order.

use crate::hardware::HardwareInfo;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Configuration for provisioning
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisionConfig {
    pub hostname: String,
    pub unit_type: UnitType,
    pub wifi_config: Option<WifiConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone)]
pub struct SafeHardware {
    pub info: HardwareInfo,
}

// ============================================================================
// Stage 1: Validation
// ============================================================================

/// Hardware that has been validated for provisioning
///
/// This type can only be constructed after validating:
/// - NVMe drives exist
/// - Drive configuration matches unit requirements
#[derive(Debug, Clone)]
pub struct ValidatedHardware {
    pub config: ProvisionConfig,
    pub drives: ValidatedDrives,
}

/// Validated drive configuration
#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
pub struct DriveInfo {
    pub device: String,
    pub size_bytes: u64,
    pub model: String,
}

// ============================================================================
// Stage 2: Partitioning
// ============================================================================

/// Drives that have been partitioned
///
/// This type can only be constructed after creating partitions
/// using parted/gdisk.
#[derive(Debug, Clone)]
pub struct PartitionedDrives {
    pub validated: ValidatedHardware,
    pub layout: PartitionLayout,
}

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
pub struct Partition {
    pub device: String,
    pub mount_point: &'static str,
    pub label: &'static str,
    pub size_description: &'static str,
}

// ============================================================================
// Stage 3: Formatting
// ============================================================================

/// Partitions that have been formatted with filesystems
///
/// This type can only be constructed after formatting partitions
/// with mkfs.ext4.
#[derive(Debug, Clone)]
pub struct FormattedSystem {
    pub partitioned: PartitionedDrives,
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
#[derive(Debug, Clone)]
pub struct InstalledSystem {
    pub formatted: FormattedSystem,
    pub mount_point: PathBuf,
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
#[derive(Debug, Clone)]
pub struct ConfiguredSystem {
    pub installed: InstalledSystem,
}

// ============================================================================
// Stage 6: Finalization
// ============================================================================

/// Fully provisioned system ready to boot
///
/// This is the final stage output.
#[derive(Debug, Clone)]
pub struct ProvisionedSystem {
    pub configured: ConfiguredSystem,
    pub summary: ProvisioningSummary,
}

#[derive(Debug, Clone)]
pub struct ProvisioningSummary {
    pub hostname: String,
    pub unit_type: UnitType,
    pub primary_drive: String,
    pub secondary_drive: Option<String>,
    pub total_partitions: usize,
}
