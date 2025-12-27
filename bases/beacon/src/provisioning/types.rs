//! Provisioning pipeline stage types
//!
//! Each type represents a stage in the provisioning pipeline.
//! The type system ensures stages are executed in order.
//!
//! ## Type Safety Philosophy
//!
//! This module re-exports domain types from crate::types but NEVER redefines them.
//! All newtypes have private fields enforced at the crate::types level.

use std::path::PathBuf;

use crate::hardware::HardwareInfo;
use serde::{Deserialize, Serialize};

// ============================================================================
// Re-exports from crate::types (SINGLE SOURCE OF TRUTH)
// ============================================================================

pub use crate::types::{
    ByteSize, DevicePath, Hostname, MountPoint, PartitionLabel, PartitionSize, ProvisionConfig,
    SshPublicKey, StorageCapacity, UnitType, ValidationError,
};

// ============================================================================
// WiFi Configuration (provisioning-specific)
// ============================================================================

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
                write!(f, "1 drive: {}", drive)
            }
            ValidatedDrives::TwoDrives(primary, secondary) => {
                write!(f, "2 drives: {}, {}", primary, secondary)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DriveInfo {
    pub device: DevicePath,   // Using validated type from crate::types
    pub size_bytes: ByteSize, // Using shared type from storage_primitives
    pub model: String,
}

impl std::fmt::Display for DriveInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({}, model: {})",
            self.device, self.size_bytes, self.model
        )
    }
}

// ============================================================================
// Stage 2: Partitioning
// ============================================================================

/// Drives that have been partitioned
///
/// This type can only be constructed after creating partitions
#[derive(Debug, Clone, PartialEq)]
pub struct PartitionedDrives {
    pub validated: ValidatedHardware,
    pub plan: PartitionPlan,
}

impl std::fmt::Display for PartitionedDrives {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "✅ Partitioned drives:\n{}", self.plan)
    }
}

/// Partition plan for either single or dual drive configurations
#[derive(Debug, Clone, PartialEq)]
pub enum PartitionPlan {
    /// All partitions on a single drive
    SingleDrive {
        device: DriveInfo,
        partitions: Vec<Partition>,
    },
    /// Partitions split across primary and secondary drives
    DualDrive {
        primary_device: DriveInfo,
        primary_partitions: Vec<Partition>,
        secondary_device: DriveInfo,
        secondary_partitions: Vec<Partition>,
    },
}

impl std::fmt::Display for PartitionPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PartitionPlan::SingleDrive { device, partitions } => {
                writeln!(f, "Single Drive Configuration {}:", device)?;
                for partition in partitions {
                    writeln!(f, "  {}", partition)?;
                }
                Ok(())
            }
            PartitionPlan::DualDrive {
                primary_device,
                primary_partitions,
                secondary_device,
                secondary_partitions,
            } => {
                writeln!(f, "Dual Drive Configuration:")?;
                writeln!(f, "Primary Drive {}:", primary_device)?;
                for partition in primary_partitions {
                    writeln!(f, "  {}", partition)?;
                }
                writeln!(f, "Secondary Drive {}:", secondary_device)?;
                for partition in secondary_partitions {
                    writeln!(f, "  {}", partition)?;
                }
                Ok(())
            }
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
// Stage 4: Base Installation
// ============================================================================

/// System with base Void Linux installed
#[derive(Debug, Clone, PartialEq)]
pub struct InstalledSystem {
    pub formatted: FormattedSystem,
}
impl InstalledSystem {
    pub(crate) fn mount_point(&self) -> PathBuf {
        todo!()
    }
}

impl std::fmt::Display for InstalledSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "✅ Base system installed")
    }
}

// ============================================================================
// Stage 5: Configuration
// ============================================================================

/// System with configuration applied
#[derive(Debug, Clone, PartialEq)]
pub struct ConfiguredSystem {
    pub installed: InstalledSystem,
}

impl std::fmt::Display for ConfiguredSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "✅ System configured")
    }
}

// ============================================================================
// Stage 6: Finalization
// ============================================================================

/// Fully provisioned system ready to boot
#[derive(Debug, Clone, PartialEq)]
pub struct ProvisionedSystem {
    pub configured: ConfiguredSystem,
}

impl std::fmt::Display for ProvisionedSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "✅ System fully provisioned and ready to boot!")
    }
}
