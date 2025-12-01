// bases/beacon/src/provisioning/pipeline.rs
//! Type-safe provisioning pipeline
//! 
//! Each stage can only be called with the output from the previous stage.
//! This makes illegal state transitions impossible at compile time.

use crate::error::{BeaconError, Result};
use crate::hardware::HardwareInfo;
use crate::types::{DevicePath, ProvisionConfig, UnitType};
use tokio::sync::broadcast;
use tracing::{info, warn};

/// Send a log message to both tracing and the broadcast channel
macro_rules! send_log {
    ($tx:expr, $($arg:tt)*) => {{
        let msg = format!($($arg)*);
        tracing::info!("{}", msg);
        let _ = $tx.send(msg);
    }};
}

// ============================================================================
// Stage 1: Validated Hardware
// ============================================================================

/// Validated NVMe drives after hardware detection
/// 
/// The type makes it impossible to have invalid drive configurations
#[derive(Debug, Clone)]
pub enum ValidatedDrives {
    /// Single NVMe drive
    /// 
    /// Used for MDMA-101, MDMA-303, or MDMA-909 with combined layout
    OneDrive(crate::hardware::NvmeDrive),
    
    /// Two NVMe drives (optimal for MDMA-909)
    /// 
    /// First drive is primary (OS + music), second is CDJ export
    TwoDrives(crate::hardware::NvmeDrive, crate::hardware::NvmeDrive),
}

impl ValidatedDrives {
    /// Get number of drives
    pub fn count(&self) -> usize {
        match self {
            ValidatedDrives::OneDrive(_) => 1,
            ValidatedDrives::TwoDrives(_, _) => 2,
        }
    }
    
    /// Get primary drive (always exists)
    pub fn primary(&self) -> &crate::hardware::NvmeDrive {
        match self {
            ValidatedDrives::OneDrive(drive) => drive,
            ValidatedDrives::TwoDrives(primary, _) => primary,
        }
    }
    
    /// Get secondary drive (only if it exists)
    pub fn secondary(&self) -> Option<&crate::hardware::NvmeDrive> {
        match self {
            ValidatedDrives::OneDrive(_) => None,
            ValidatedDrives::TwoDrives(_, secondary) => Some(secondary),
        }
    }
}

/// Hardware that has been validated for provisioning
/// 
/// Can only be constructed after validation passes
pub struct ValidatedHardware {
    pub config: ProvisionConfig,
    pub drives: ValidatedDrives,
}

impl ValidatedHardware {
    /// Validate hardware meets requirements
    /// 
    /// Returns ValidatedHardware only if all checks pass
    pub fn validate(config: ProvisionConfig, hardware: HardwareInfo, log_tx: &broadcast::Sender<String>) -> Result<Self> {
        // Build ValidatedDrives enum based on what we found
        let drives = match hardware.nvme_drives.len() {
            0 => {
                return Err(BeaconError::HardwareInfo(
                    "No NVMe drives found - cannot provision".to_string()
                ));
            }
            1 => {
                let drive = hardware.nvme_drives.into_iter().next().unwrap();
                
                if config.unit_type.requires_dual_nvme() {
                    send_log!(log_tx, "  ⚠️  MDMA-909 with single NVMe - will use combined partition layout");
                    send_log!(log_tx, "  ⚠️  For optimal performance, add a second NVMe drive");
                }
                
                if drive.is_formatted {
                    send_log!(log_tx, "  ⚠️  NVMe drive appears to be formatted - will overwrite!");
                }
                
                ValidatedDrives::OneDrive(drive)
            }
            _ => {
                // 2 or more drives - use first two
                let mut iter = hardware.nvme_drives.into_iter();
                let primary = iter.next().unwrap();
                let secondary = iter.next().unwrap();
                
                if config.unit_type.requires_dual_nvme() {
                    send_log!(log_tx, "  ✓ MDMA-909 with dual NVMe setup - optimal configuration");
                }
                
                if primary.is_formatted || secondary.is_formatted {
                    send_log!(log_tx, "  ⚠️  Some NVMe drives appear to be formatted - will overwrite!");
                }
                
                ValidatedDrives::TwoDrives(primary, secondary)
            }
        };
        
        Ok(ValidatedHardware { config, drives })
    }
}

// ============================================================================
// Stage 2: Partitioned Drives
// ============================================================================

/// Drives that have been partitioned
/// 
/// Can only be constructed by partitioning ValidatedHardware
pub struct PartitionedDrives {
    /// The validated hardware (carried forward)
    validated: ValidatedHardware,
    
    /// Partition layout that was created
    pub layout: PartitionLayout,
}

#[derive(Debug, Clone)]
pub enum PartitionLayout {
    /// Single NVMe with standard partitions (101, 303, or 909-single)
    SingleDrive {
        device: DevicePath,
        partitions: Vec<Partition>,
    },
    /// Dual NVMe with music + CDJ export (909-dual)
    DualDrive {
        primary: DevicePath,
        primary_partitions: Vec<Partition>,
        secondary: DevicePath,
        secondary_partitions: Vec<Partition>,
    },
}

#[derive(Debug, Clone)]
pub struct Partition {
    pub device: String,  // e.g., "/dev/nvme0n1p1"
    pub mount_point: &'static str,  // e.g., "/"
    pub label: &'static str,  // e.g., "root"
    pub size_description: &'static str,  // e.g., "16GB"
}

impl PartitionedDrives {
    /// Partition drives based on unit type and available hardware
    /// 
    /// This is the ONLY way to get a PartitionedDrives instance
    pub async fn partition(validated: ValidatedHardware, log_tx: &broadcast::Sender<String>) -> Result<Self> {
        let layout = match (&validated.config.unit_type, &validated.drives) {
            // MDMA-909 with dual drives - optimal layout
            (UnitType::Mdma909, ValidatedDrives::TwoDrives(primary, secondary)) => {
                send_log!(log_tx, "  Using dual NVMe configuration");
                Self::partition_dual_drive(&primary.device, &secondary.device, log_tx).await?
            }
            // MDMA-909 with single drive - combined layout
            (UnitType::Mdma909, ValidatedDrives::OneDrive(drive)) => {
                send_log!(log_tx, "  Using single NVMe configuration (combined layout)");
                Self::partition_combined_drive(&drive.device, log_tx).await?
            }
            // MDMA-101 or MDMA-303 - standard single drive
            (_, ValidatedDrives::OneDrive(drive)) => {
                send_log!(log_tx, "  Using standard single NVMe configuration");
                Self::partition_standard_drive(&drive.device, log_tx).await?
            }
            // MDMA-101 or MDMA-303 with two drives - just use first one
            (_, ValidatedDrives::TwoDrives(primary, _)) => {
                send_log!(log_tx, "  Using standard single NVMe configuration (ignoring second drive)");
                Self::partition_standard_drive(&primary.device, log_tx).await?
            }
        };
        
        Ok(PartitionedDrives { validated, layout })
    }
    
    async fn partition_standard_drive(device: &DevicePath, log_tx: &broadcast::Sender<String>) -> Result<PartitionLayout> {
        send_log!(log_tx, "    Partitioning drive: {}", device);
        
        // TODO: Actually run parted commands
        // For now, just describe the layout
        
        let partitions = vec![
            Partition {
                device: format!("{}p1", device),
                mount_point: "/",
                label: "root",
                size_description: "16GB",
            },
            Partition {
                device: format!("{}p2", device),
                mount_point: "/var",
                label: "var",
                size_description: "8GB",
            },
            Partition {
                device: format!("{}p3", device),
                mount_point: "/music",
                label: "music",
                size_description: "400GB",
            },
            Partition {
                device: format!("{}p4", device),
                mount_point: "/metadata",
                label: "metadata",
                size_description: "rest",
            },
        ];
        
        Ok(PartitionLayout::SingleDrive {
            device: device.clone(),
            partitions,
        })
    }
    
    async fn partition_combined_drive(device: &DevicePath, log_tx: &broadcast::Sender<String>) -> Result<PartitionLayout> {
        send_log!(log_tx, "    Partitioning combined drive: {}", device);
        
        let partitions = vec![
            Partition {
                device: format!("{}p1", device),
                mount_point: "/",
                label: "root",
                size_description: "16GB",
            },
            Partition {
                device: format!("{}p2", device),
                mount_point: "/var",
                label: "var",
                size_description: "8GB",
            },
            Partition {
                device: format!("{}p3", device),
                mount_point: "/music",
                label: "music",
                size_description: "200GB",
            },
            Partition {
                device: format!("{}p4", device),
                mount_point: "/cdj-export",
                label: "cdj-export",
                size_description: "200GB",
            },
            Partition {
                device: format!("{}p5", device),
                mount_point: "/metadata",
                label: "metadata",
                size_description: "rest",
            },
        ];
        
        Ok(PartitionLayout::SingleDrive {
            device: device.clone(),
            partitions,
        })
    }
    
    async fn partition_dual_drive(
        primary: &DevicePath,
        secondary: &DevicePath,
        log_tx: &broadcast::Sender<String>,
    ) -> Result<PartitionLayout> {
        send_log!(log_tx, "    Partitioning primary drive: {}", primary);
        send_log!(log_tx, "    Partitioning secondary drive: {}", secondary);
        
        let primary_partitions = vec![
            Partition {
                device: format!("{}p1", primary),
                mount_point: "/",
                label: "root",
                size_description: "16GB",
            },
            Partition {
                device: format!("{}p2", primary),
                mount_point: "/var",
                label: "var",
                size_description: "8GB",
            },
            Partition {
                device: format!("{}p3", primary),
                mount_point: "/music",
                label: "music",
                size_description: "400GB",
            },
            Partition {
                device: format!("{}p4", primary),
                mount_point: "/metadata",
                label: "metadata",
                size_description: "rest",
            },
        ];
        
        let secondary_partitions = vec![
            Partition {
                device: format!("{}p1", secondary),
                mount_point: "/cdj-export",
                label: "cdj-export",
                size_description: "full disk",
            },
        ];
        
        Ok(PartitionLayout::DualDrive {
            primary: primary.clone(),
            primary_partitions,
            secondary: secondary.clone(),
            secondary_partitions,
        })
    }
}

// ============================================================================
// Stage 3: Formatted System
// ============================================================================

/// Partitions that have been formatted
/// 
/// Can only be constructed by formatting PartitionedDrives
pub struct FormattedSystem {
    /// The partitioned drives (carried forward)
    partitioned: PartitionedDrives,
}

impl FormattedSystem {
    /// Format all partitions with ext4
    /// 
    /// This is the ONLY way to get a FormattedSystem instance
    pub async fn format(partitioned: PartitionedDrives, log_tx: &broadcast::Sender<String>) -> Result<Self> {
        match &partitioned.layout {
            PartitionLayout::SingleDrive { partitions, .. } => {
                for partition in partitions {
                    send_log!(log_tx, "    Formatting {}: {}", partition.device, partition.label);
                    // TODO: Actually run mkfs.ext4
                }
            }
            PartitionLayout::DualDrive {
                primary_partitions,
                secondary_partitions,
                ..
            } => {
                for partition in primary_partitions.iter().chain(secondary_partitions.iter()) {
                    send_log!(log_tx, "    Formatting {}: {}", partition.device, partition.label);
                    // TODO: Actually run mkfs.ext4
                }
            }
        }
        
        Ok(FormattedSystem { partitioned })
    }
}

// ============================================================================
// Stage 4: Installed System
// ============================================================================

/// System with Void Linux installed
/// 
/// Can only be constructed by installing to FormattedSystem
pub struct InstalledSystem {
    /// The formatted system (carried forward)
    formatted: FormattedSystem,
}

impl InstalledSystem {
    /// Install Void Linux base system
    /// 
    /// This is the ONLY way to get an InstalledSystem instance
    pub async fn install(formatted: FormattedSystem, log_tx: &broadcast::Sender<String>) -> Result<Self> {
        // TODO: Mount partitions, extract tarball, chroot, install bootloader
        send_log!(log_tx, "    Installing base system (placeholder)");
        
        Ok(InstalledSystem { formatted })
    }
}

// ============================================================================
// Stage 5: Configured System
// ============================================================================

/// System that has been configured
/// 
/// Can only be constructed by configuring InstalledSystem
pub struct ConfiguredSystem {
    /// The installed system (carried forward)
    installed: InstalledSystem,
}

impl ConfiguredSystem {
    /// Configure hostname, SSH, and boot settings
    /// 
    /// This is the ONLY way to get a ConfiguredSystem instance
    pub async fn configure(installed: InstalledSystem, log_tx: &broadcast::Sender<String>) -> Result<Self> {
        let config = &installed.formatted.partitioned.validated.config;
        
        // Configure hostname
        send_log!(log_tx, "    Setting hostname: {}", config.hostname);
        // TODO: Write /etc/hostname, configure avahi
        
        // Setup SSH
        send_log!(log_tx, "    Setting up SSH key");
        // TODO: Write authorized_keys
        
        // Update boot config
        send_log!(log_tx, "    Updating boot configuration");
        // TODO: Modify /boot/cmdline.txt
        
        Ok(ConfiguredSystem { installed })
    }
}

// ============================================================================
// Stage 6: Provisioned System (Final!)
// ============================================================================

/// Fully provisioned system ready to reboot
/// 
/// This is the final state - represents a successful provisioning
pub struct ProvisionedSystem {
    /// The configured system (carried forward)
    _configured: ConfiguredSystem,
}

impl ProvisionedSystem {
    /// Finalize provisioning
    /// 
    /// This is the ONLY way to get a ProvisionedSystem instance
    pub async fn finalize(configured: ConfiguredSystem, _log_tx: &broadcast::Sender<String>) -> Result<Self> {
        Ok(ProvisionedSystem {
            _configured: configured,
        })
    }
    
    /// Trigger system reboot
    /// 
    /// Only available after successful provisioning
    pub async fn reboot(self) -> Result<()> {
        // TODO: Trigger actual reboot
        Ok(())
    }
}
