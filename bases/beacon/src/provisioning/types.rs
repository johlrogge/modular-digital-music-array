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

// ============================================================================
// Re-exports from crate::types (SINGLE SOURCE OF TRUTH)
// ============================================================================

pub use crate::types::{
    ByteSize, DevicePath, FilesystemType, MountPoint, PartitionLabel, PartitionSize,
    ProvisionConfig, UnitType, ValidationError,
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
    pub size: PartitionSize,
}

impl Partition {
    /// Get the filesystem type for this partition
    ///
    /// Currently determined by mount point, but may become
    /// customizable in future versions without changing call sites.
    pub fn filesystem_type(&self) -> FilesystemType {
        self.mount_point.filesystem_type()
    }

    /// Get the partition label
    ///
    /// Derived from the mount point using kebab-case naming.
    /// All MDMA hosts use the same labels for the same purpose
    /// partitions, making lsblk output consistent and recognizable.
    pub fn label(&self) -> PartitionLabel {
        self.mount_point.label()
    }
}

impl std::fmt::Display for Partition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} → {} ({}, label: {})",
            self.device,
            self.mount_point,
            self.size,
            self.label()
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
        // Neutral description - doesn't imply formatting already happened
        match &self.partitioned.plan {
            CompletedPartitionPlan::SingleDrive { partitions, .. } => {
                write!(
                    f,
                    "Filesystems verified on {} partition(s)",
                    partitions.len()
                )
            }
            CompletedPartitionPlan::DualDrive {
                primary_partitions,
                secondary_partitions,
                ..
            } => {
                write!(
                    f,
                    "Filesystems verified on {} partition(s)",
                    primary_partitions.len() + secondary_partitions.len()
                )
            }
        }
    }
}

// ============================================================================
// Stage 4: Base Installation (Sub-stages)
// ============================================================================

// ----------------------------------------------------------------------------
// Stage 4.1: Mount Partitions
// ----------------------------------------------------------------------------

/// Workflow state for a partition during mounting
#[derive(Debug, Clone, PartialEq)]
pub enum MountState {
    /// Partition needs to be mounted
    NeedsMount(Partition),
    /// Partition is already mounted
    AlreadyMounted(Partition),
}

impl MountState {
    pub fn partition(&self) -> &Partition {
        match self {
            MountState::NeedsMount(p) => p,
            MountState::AlreadyMounted(p) => p,
        }
    }
}

impl std::fmt::Display for MountState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MountState::NeedsMount(p) => write!(f, "✨ Mount {} to /mnt/mdma-install{}", 
                p.device, p.mount_point.as_path().display()),
            MountState::AlreadyMounted(p) => write!(f, "⏭️  {} already mounted", p.device),
        }
    }
}

/// Planned work for mounting partitions (WITH workflow state)
#[derive(Debug, Clone, PartialEq)]
pub struct MountPlan {
    pub formatted: FormattedSystem,  // Thread the input through
    pub mount_root: PathBuf,
    pub partitions: Vec<MountState>,
}

impl std::fmt::Display for MountPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let needs_mount = self.partitions.iter()
            .filter(|p| matches!(p, MountState::NeedsMount(_)))
            .count();
        let already_mounted = self.partitions.iter()
            .filter(|p| matches!(p, MountState::AlreadyMounted(_)))
            .count();
        
        if needs_mount > 0 && already_mounted > 0 {
            writeln!(f, "📝 Mount {} partition(s), skip {} (already mounted)", 
                needs_mount, already_mounted)?;
        } else if needs_mount > 0 {
            writeln!(f, "📝 Mount {} partition(s)", needs_mount)?;
        } else {
            writeln!(f, "📝 All {} partition(s) already mounted", already_mounted)?;
        }
        
        writeln!(f, "\nMount root: {}", self.mount_root.display())?;
        for partition_state in &self.partitions {
            writeln!(f, "  {}", partition_state)?;
        }
        Ok(())
    }
}

/// Partitions that have been mounted (WITHOUT workflow state)
#[derive(Debug, Clone, PartialEq)]
pub struct MountedPartitions {
    pub formatted: FormattedSystem,
    pub mount_root: PathBuf,
    pub partitions: Vec<Partition>,
}

impl std::fmt::Display for MountedPartitions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "✅ {} partition(s) mounted at {}", 
            self.partitions.len(), 
            self.mount_root.display())
    }
}

// ----------------------------------------------------------------------------
// Stage 4.2: Install Packages
// ----------------------------------------------------------------------------

/// Workflow state for package installation
#[derive(Debug, Clone, PartialEq)]
pub enum InstallState {
    NeedsInstall,
    AlreadyInstalled,
}

impl std::fmt::Display for InstallState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallState::NeedsInstall => write!(f, "✨ Install base-system package"),
            InstallState::AlreadyInstalled => write!(f, "⏭️  base-system already installed"),
        }
    }
}

/// Planned work for installing packages (WITH workflow state)
#[derive(Debug, Clone, PartialEq)]
pub struct InstallPlan {
    pub mount_root: PathBuf,
    pub packages: Vec<String>,
    pub install_state: InstallState,
}

impl std::fmt::Display for InstallPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{}", self.install_state)?;
        writeln!(f, "Target: {}", self.mount_root.display())?;
        writeln!(f, "Packages: {}", self.packages.join(", "))
    }
}

/// System with packages installed (WITHOUT workflow state)
#[derive(Debug, Clone, PartialEq)]
pub struct InstalledPackages {
    pub mounted: MountedPartitions,
    pub packages: Vec<String>,
}

impl std::fmt::Display for InstalledPackages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "✅ {} package(s) installed", self.packages.len())
    }
}

// ----------------------------------------------------------------------------
// Stage 4.3: Configure fstab
// ----------------------------------------------------------------------------

/// Workflow state for fstab configuration
#[derive(Debug, Clone, PartialEq)]
pub enum FstabState {
    NeedsConfig,
    AlreadyConfigured,
}

impl std::fmt::Display for FstabState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FstabState::NeedsConfig => write!(f, "✨ Configure /etc/fstab"),
            FstabState::AlreadyConfigured => write!(f, "⏭️  /etc/fstab already configured"),
        }
    }
}

/// Planned work for configuring fstab (WITH workflow state)
#[derive(Debug, Clone, PartialEq)]
pub struct FstabPlan {
    pub mount_root: PathBuf,
    pub partitions: Vec<Partition>,
    pub config_state: FstabState,
}

impl std::fmt::Display for FstabPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{}", self.config_state)?;
        writeln!(f, "Target: {}/etc/fstab", self.mount_root.display())?;
        writeln!(f, "Partitions: {}", self.partitions.len())
    }
}

/// System with fstab configured (WITHOUT workflow state)
#[derive(Debug, Clone, PartialEq)]
pub struct ConfiguredFstab {
    pub installed: InstalledPackages,
}

impl std::fmt::Display for ConfiguredFstab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "✅ fstab configured")
    }
}

// ----------------------------------------------------------------------------
// Stage 4.4: Unmount Partitions
// ----------------------------------------------------------------------------

/// Workflow state for unmounting
#[derive(Debug, Clone, PartialEq)]
pub enum UnmountState {
    NeedsUnmount(PathBuf),
    AlreadyUnmounted,
}

impl std::fmt::Display for UnmountState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnmountState::NeedsUnmount(path) => write!(f, "✨ Unmount {}", path.display()),
            UnmountState::AlreadyUnmounted => write!(f, "⏭️  Already unmounted"),
        }
    }
}

/// Planned work for unmounting (WITH workflow state)
#[derive(Debug, Clone, PartialEq)]
pub struct UnmountPlan {
    pub mount_points: Vec<(PathBuf, UnmountState)>,
}

impl std::fmt::Display for UnmountPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let needs_unmount = self.mount_points.iter()
            .filter(|(_, state)| matches!(state, UnmountState::NeedsUnmount(_)))
            .count();
        
        if needs_unmount > 0 {
            writeln!(f, "📝 Unmount {} mount point(s)", needs_unmount)?;
            for (_path, state) in &self.mount_points {
                if matches!(state, UnmountState::NeedsUnmount(_)) {
                    writeln!(f, "  {}", state)?;
                }
            }
        } else {
            writeln!(f, "📝 All mount points already unmounted")?;
        }
        Ok(())
    }
}

// ----------------------------------------------------------------------------
// Stage 4: Final Output (Composite)
// ----------------------------------------------------------------------------

/// System with base Void Linux installed
#[derive(Debug, Clone, PartialEq)]
pub struct InstalledSystem {
    pub formatted: FormattedSystem,
}

impl std::fmt::Display for InstalledSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "✅ Base system installed")
    }
}

impl InstalledSystem {
    /// Get the mount point where the system was installed
    /// 
    /// NOTE: This is a temporary stub for backwards compatibility with stages 5 and 6.
    /// These stages should be updated to use a better approach once we finalize
    /// the installation architecture.
    pub fn mount_point(&self) -> PathBuf {
        // For now, return the standard mount point
        // In the future, this should come from the installation plan
        PathBuf::from("/mnt/mdma-install")
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
