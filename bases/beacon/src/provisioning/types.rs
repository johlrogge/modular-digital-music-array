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
    ByteSize, DevicePath, MountPoint, PartitionLabel, PartitionSize, ProvisionConfig, UnitType,
    ValidationError,
};

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

impl PartitionedDrives {
    /// Convert to completed partitions (strips workflow state)
    ///
    /// After partitioning is complete, downstream stages don't need
    /// to know about Planned vs Exists state. This method converts
    /// the plan to contain just Partition data.
    pub fn into_completed(self) -> CompletedPartitionedDrives {
        CompletedPartitionedDrives {
            validated: self.validated,
            plan: self.plan.into_completed(),
        }
    }
}

impl std::fmt::Display for PartitionedDrives {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "✅ Partitioned drives:\n{}", self.plan)
    }
}

/// Drives with completed partitioning (no workflow state)
///
/// Used by stages after partitioning is complete
#[derive(Debug, Clone, PartialEq)]
pub struct CompletedPartitionedDrives {
    pub validated: ValidatedHardware,
    pub plan: CompletedPartitionPlan,
}

impl std::fmt::Display for CompletedPartitionedDrives {
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
        partitions: Vec<PartitionState>,
    },
    /// Partitions split across primary and secondary drives
    DualDrive {
        primary_device: DriveInfo,
        primary_partitions: Vec<PartitionState>,
        secondary_device: DriveInfo,
        secondary_partitions: Vec<PartitionState>,
    },
}

impl std::fmt::Display for PartitionPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn count_planned_and_exists(partitions: &[PartitionState]) -> (usize, usize) {
            let planned = partitions
                .iter()
                .filter(|p| matches!(p, PartitionState::Planned(_)))
                .count();
            let exists = partitions
                .iter()
                .filter(|p| matches!(p, PartitionState::Exists(_)))
                .count();
            (planned, exists)
        }

        match self {
            PartitionPlan::SingleDrive { device, partitions } => {
                let (planned, exists) = count_planned_and_exists(partitions);

                if planned > 0 && exists > 0 {
                    writeln!(
                        f,
                        "📝 Plan: Create {} partition(s), skip {} (already exist)",
                        planned, exists
                    )?;
                } else if planned > 0 {
                    writeln!(f, "📝 Plan: Create {} partition(s)", planned)?;
                } else {
                    writeln!(f, "📝 Plan: All {} partition(s) already exist", exists)?;
                }
                writeln!(f)?;
                writeln!(f, "{}:", device)?;
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
                let (primary_planned, primary_exists) =
                    count_planned_and_exists(primary_partitions);
                let (secondary_planned, secondary_exists) =
                    count_planned_and_exists(secondary_partitions);
                let total_planned = primary_planned + secondary_planned;
                let total_exists = primary_exists + secondary_exists;

                if total_planned > 0 && total_exists > 0 {
                    writeln!(
                        f,
                        "📝 Plan: Create {} partition(s), skip {} (already exist)",
                        total_planned, total_exists
                    )?;
                } else if total_planned > 0 {
                    writeln!(f, "📝 Plan: Create {} partition(s)", total_planned)?;
                } else {
                    writeln!(
                        f,
                        "📝 Plan: All {} partition(s) already exist",
                        total_exists
                    )?;
                }
                writeln!(f)?;
                writeln!(f, "Primary {}:", primary_device)?;
                for partition in primary_partitions {
                    writeln!(f, "  {}", partition)?;
                }
                writeln!(f)?;
                writeln!(f, "Secondary {}:", secondary_device)?;
                for partition in secondary_partitions {
                    writeln!(f, "  {}", partition)?;
                }
                Ok(())
            }
        }
    }
}

impl PartitionPlan {
    /// Convert from workflow state (PartitionState) to completed partitions
    ///
    /// After stage 2 completes, all partitions exist on disk. This method
    /// strips the workflow state wrapper, giving downstream stages clean
    /// Partition data without caring about Planned vs Exists.
    pub fn into_completed(self) -> CompletedPartitionPlan {
        match self {
            PartitionPlan::SingleDrive { device, partitions } => {
                CompletedPartitionPlan::SingleDrive {
                    device,
                    partitions: partitions
                        .into_iter()
                        .map(|state| state.into_partition())
                        .collect(),
                }
            }
            PartitionPlan::DualDrive {
                primary_device,
                primary_partitions,
                secondary_device,
                secondary_partitions,
            } => CompletedPartitionPlan::DualDrive {
                primary_device,
                primary_partitions: primary_partitions
                    .into_iter()
                    .map(|state| state.into_partition())
                    .collect(),
                secondary_device,
                secondary_partitions: secondary_partitions
                    .into_iter()
                    .map(|state| state.into_partition())
                    .collect(),
            },
        }
    }
}

/// Partition plan after all partitions have been created
///
/// This structure tracks what actually happened during partitioning:
/// - Which partitions were created
/// - Which partitions already existed and were skipped
#[derive(Debug, Clone, PartialEq)]
pub enum CompletedPartitionPlan {
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

impl std::fmt::Display for CompletedPartitionPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompletedPartitionPlan::SingleDrive { device, partitions } => {
                write!(
                    f,
                    "{} partition(s) verified on {}",
                    partitions.len(),
                    device.device.as_path().display()
                )
            }
            CompletedPartitionPlan::DualDrive {
                primary_device,
                primary_partitions,
                secondary_device,
                secondary_partitions,
            } => {
                write!(
                    f,
                    "{} partition(s) verified on {} (primary), {} on {} (secondary)",
                    primary_partitions.len(),
                    primary_device.device.as_path().display(),
                    secondary_partitions.len(),
                    secondary_device.device.as_path().display()
                )
            }
        }
    }
}

/// Represents a partition's state during provisioning
#[derive(Debug, Clone, PartialEq)]
pub enum PartitionState {
    /// Partition needs to be created
    Planned(Partition),
    /// Partition already exists on disk
    Exists(Partition),
}

impl PartitionState {
    /// Get the underlying partition regardless of state
    pub fn partition(&self) -> &Partition {
        match self {
            PartitionState::Planned(p) => p,
            PartitionState::Exists(p) => p,
        }
    }

    /// Extract the underlying partition, consuming self
    pub fn into_partition(self) -> Partition {
        match self {
            PartitionState::Planned(p) => p,
            PartitionState::Exists(p) => p,
        }
    }
}

impl std::fmt::Display for PartitionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PartitionState::Planned(p) => write!(f, "✨ {} [will create]", p),
            PartitionState::Exists(p) => write!(f, "⏭️  {} [will skip - already exists]", p),
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

// ============================================================================
// Stage 3: Formatting
// ============================================================================

/// Partitions that have been formatted with filesystems
///
/// This type can only be constructed after formatting partitions
/// with mkfs.ext4.
#[derive(Debug, Clone, PartialEq)]
pub struct FormattedSystem {
    pub partitioned: CompletedPartitionedDrives,
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
