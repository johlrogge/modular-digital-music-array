// bases/beacon/src/provisioning/stage2_partition.rs
//! Stage 2: Partition NVMe drives

use crate::actions::{Action, ActionId, PlannedAction};
use crate::error::Result;
use crate::provisioning::types::{
    Partition, PartitionLayout, PartitionedDrives, ValidatedHardware,
};

#[derive(Clone, Debug)]
pub struct PartitionDrivesAction;

impl Action<ValidatedHardware, PartitionedDrives> for PartitionDrivesAction {
    fn id(&self) -> ActionId {
        ActionId::new("partition-drives")
    }

    fn description(&self) -> String {
        "Partition NVMe drives".to_string()
    }

    async fn plan(
        &self,
        input: &ValidatedHardware,
    ) -> Result<PlannedAction<ValidatedHardware, PartitionedDrives, Self>> {
        use crate::provisioning::types::{UnitType, DevicePath, MountPoint, PartitionLabel, PartitionSize};

        let primary_device = input.drives.primary().device.clone();
        let primary_size_bytes = input.drives.primary().size_bytes;
        
        // Constants (in GB)
        const ROOT_SIZE_GB: u64 = 16;
        const VAR_SIZE_GB: u64 = 8;
        const METADATA_SIZE_GB: u64 = 88;
        const MIN_MUSIC_SIZE_GB: u64 = 300;
        const MIN_CDJ_SIZE_GB: u64 = 64;
        
        let partitions = match input.config.unit_type {
            UnitType::Mdma909 | UnitType::Mdma101 => {
                if let Some(secondary) = input.drives.secondary() {
                    // Two-drive config: Primary gets music+metadata
                    let os_overhead = PartitionSize::from_gb(ROOT_SIZE_GB + VAR_SIZE_GB + METADATA_SIZE_GB);
                    let remaining_bytes = primary_size_bytes.saturating_sub(os_overhead.bytes());
                    let music_size = PartitionSize(remaining_bytes);
                    
                    vec![
                        Partition {
                            device: DevicePath(format!("{}p1", primary_device)),
                            mount_point: MountPoint("/"),
                            label: PartitionLabel("root"),
                            size: PartitionSize::from_gb(ROOT_SIZE_GB),
                        },
                        Partition {
                            device: DevicePath(format!("{}p2", primary_device)),
                            mount_point: MountPoint("/var"),
                            label: PartitionLabel("var"),
                            size: PartitionSize::from_gb(VAR_SIZE_GB),
                        },
                        Partition {
                            device: DevicePath(format!("{}p3", primary_device)),
                            mount_point: MountPoint("/music"),
                            label: PartitionLabel("music"),
                            size: music_size,
                        },
                        Partition {
                            device: DevicePath(format!("{}p4", primary_device)),
                            mount_point: MountPoint("/metadata"),
                            label: PartitionLabel("metadata"),
                            size: PartitionSize::from_gb(METADATA_SIZE_GB),
                        },
                    ]
                } else {
                    // Single-drive config: Split between music and CDJ
                    let os_overhead = PartitionSize::from_gb(ROOT_SIZE_GB + VAR_SIZE_GB + METADATA_SIZE_GB);
                    let remaining_bytes = primary_size_bytes.saturating_sub(os_overhead.bytes());
                    let remaining_gb = remaining_bytes / 1_000_000_000;
                    
                    // Check minimums
                    let min_required_gb = MIN_MUSIC_SIZE_GB + MIN_CDJ_SIZE_GB;
                    if remaining_gb < min_required_gb {
                        return Err(crate::error::BeaconError::Validation(format!(
                            "Drive too small: {} has only {}GB after OS partitions, need at least {}GB for music ({}GB) + CDJ export ({}GB)",
                            primary_device,
                            remaining_gb,
                            min_required_gb,
                            MIN_MUSIC_SIZE_GB,
                            MIN_CDJ_SIZE_GB
                        )));
                    }
                    
                    // Calculate proportional sizes
                    let extra_gb = remaining_gb - min_required_gb;
                    let music_weight = MIN_MUSIC_SIZE_GB as f64 / (MIN_MUSIC_SIZE_GB + MIN_CDJ_SIZE_GB) as f64;
                    let cdj_weight = MIN_CDJ_SIZE_GB as f64 / (MIN_MUSIC_SIZE_GB + MIN_CDJ_SIZE_GB) as f64;
                    
                    let music_size_gb = MIN_MUSIC_SIZE_GB + ((extra_gb as f64 * music_weight) as u64);
                    let cdj_size_gb = MIN_CDJ_SIZE_GB + ((extra_gb as f64 * cdj_weight) as u64);
                    
                    tracing::info!(
                        "Single-drive partition sizing: {} total, {} music, {} CDJ export",
                        PartitionSize(primary_size_bytes),
                        PartitionSize::from_gb(music_size_gb),
                        PartitionSize::from_gb(cdj_size_gb)
                    );
                    
                    vec![
                        Partition {
                            device: DevicePath(format!("{}p1", primary_device)),
                            mount_point: MountPoint("/"),
                            label: PartitionLabel("root"),
                            size: PartitionSize::from_gb(ROOT_SIZE_GB),
                        },
                        Partition {
                            device: DevicePath(format!("{}p2", primary_device)),
                            mount_point: MountPoint("/var"),
                            label: PartitionLabel("var"),
                            size: PartitionSize::from_gb(VAR_SIZE_GB),
                        },
                        Partition {
                            device: DevicePath(format!("{}p3", primary_device)),
                            mount_point: MountPoint("/music"),
                            label: PartitionLabel("music"),
                            size: PartitionSize::from_gb(music_size_gb),
                        },
                        Partition {
                            device: DevicePath(format!("{}p4", primary_device)),
                            mount_point: MountPoint("/metadata"),
                            label: PartitionLabel("metadata"),
                            size: PartitionSize::from_gb(METADATA_SIZE_GB),
                        },
                        Partition {
                            device: DevicePath(format!("{}p5", primary_device)),
                            mount_point: MountPoint("/cdj-export"),
                            label: PartitionLabel("cdj-export"),
                            size: PartitionSize::from_gb(cdj_size_gb),
                        },
                    ]
                }
            }
            UnitType::Mdma303 => {
                // MDMA-303: Minimal + cache partition
                let os_overhead = PartitionSize::from_gb(ROOT_SIZE_GB + VAR_SIZE_GB);
                let remaining_bytes = primary_size_bytes.saturating_sub(os_overhead.bytes());
                let cache_size = PartitionSize(remaining_bytes);
                
                vec![
                    Partition {
                        device: DevicePath(format!("{}p1", primary_device)),
                        mount_point: MountPoint("/"),
                        label: PartitionLabel("root"),
                        size: PartitionSize::from_gb(ROOT_SIZE_GB),
                    },
                    Partition {
                        device: DevicePath(format!("{}p2", primary_device)),
                        mount_point: MountPoint("/var"),
                        label: PartitionLabel("var"),
                        size: PartitionSize::from_gb(VAR_SIZE_GB),
                    },
                    Partition {
                        device: DevicePath(format!("{}p3", primary_device)),
                        mount_point: MountPoint("/cache"),
                        label: PartitionLabel("cache"),
                        size: cache_size,
                    },
                ]
            }
        };

        // Check for secondary drive (CDJ export on dedicated drive)
        let layout = if let Some(secondary) = input.drives.secondary() {
            // Two-drive MDMA-909: Secondary gets full-drive CDJ export
            let secondary_size = PartitionSize(secondary.size_bytes);
            
            let secondary_partitions = vec![Partition {
                device: DevicePath(format!("{}p1", secondary.device)),
                mount_point: MountPoint("/cdj-export"),
                label: PartitionLabel("cdj-export"),
                size: secondary_size,
            }];

            PartitionLayout::DualDrive {
                primary_device: primary_device.clone(),
                primary_partitions: partitions,
                secondary_device: secondary.device.clone(),
                secondary_partitions,
            }
        } else {
            // Single drive configuration
            PartitionLayout::SingleDrive {
                device: primary_device,
                partitions,
            }
        };

        let assumed_output = PartitionedDrives {
            validated: input.clone(),
            layout,
        };

        Ok(PlannedAction {
            description: self.description(),
            action: self.clone(),
            input: input.clone(),
            assumed_output,
        })
    }

    async fn apply(&self, input: ValidatedHardware) -> Result<PartitionedDrives> {
        use crate::provisioning::types::{UnitType, DevicePath, MountPoint, PartitionLabel, PartitionSize};

        let primary_device = input.drives.primary().device.clone();
        let primary_size_bytes = input.drives.primary().size_bytes;
        
        // Constants (in GB)
        const ROOT_SIZE_GB: u64 = 16;
        const VAR_SIZE_GB: u64 = 8;
        const METADATA_SIZE_GB: u64 = 88;
        const MIN_MUSIC_SIZE_GB: u64 = 300;
        const MIN_CDJ_SIZE_GB: u64 = 64;
        
        // Stub: In real implementation, would use parted/gdisk here
        tracing::info!("Would partition {} ({}) here", 
            primary_device, 
            PartitionSize(primary_size_bytes)
        );
        
        let partitions = match input.config.unit_type {
            UnitType::Mdma909 | UnitType::Mdma101 => {
                if let Some(secondary) = input.drives.secondary() {
                    // Two-drive config: Primary gets music+metadata
                    let os_overhead = PartitionSize::from_gb(ROOT_SIZE_GB + VAR_SIZE_GB + METADATA_SIZE_GB);
                    let remaining_bytes = primary_size_bytes.saturating_sub(os_overhead.bytes());
                    let music_size = PartitionSize(remaining_bytes);
                    
                    vec![
                        Partition {
                            device: DevicePath(format!("{}p1", primary_device)),
                            mount_point: MountPoint("/"),
                            label: PartitionLabel("root"),
                            size: PartitionSize::from_gb(ROOT_SIZE_GB),
                        },
                        Partition {
                            device: DevicePath(format!("{}p2", primary_device)),
                            mount_point: MountPoint("/var"),
                            label: PartitionLabel("var"),
                            size: PartitionSize::from_gb(VAR_SIZE_GB),
                        },
                        Partition {
                            device: DevicePath(format!("{}p3", primary_device)),
                            mount_point: MountPoint("/music"),
                            label: PartitionLabel("music"),
                            size: music_size,
                        },
                        Partition {
                            device: DevicePath(format!("{}p4", primary_device)),
                            mount_point: MountPoint("/metadata"),
                            label: PartitionLabel("metadata"),
                            size: PartitionSize::from_gb(METADATA_SIZE_GB),
                        },
                    ]
                } else {
                    // Single-drive config: Split between music and CDJ
                    let os_overhead = PartitionSize::from_gb(ROOT_SIZE_GB + VAR_SIZE_GB + METADATA_SIZE_GB);
                    let remaining_bytes = primary_size_bytes.saturating_sub(os_overhead.bytes());
                    let remaining_gb = remaining_bytes / 1_000_000_000;
                    
                    // Check minimums
                    let min_required_gb = MIN_MUSIC_SIZE_GB + MIN_CDJ_SIZE_GB;
                    if remaining_gb < min_required_gb {
                        return Err(crate::error::BeaconError::Validation(format!(
                            "Drive too small: {} has only {}GB after OS partitions",
                            primary_device,
                            remaining_gb
                        )));
                    }
                    
                    // Calculate proportional sizes
                    let extra_gb = remaining_gb - min_required_gb;
                    let music_weight = MIN_MUSIC_SIZE_GB as f64 / (MIN_MUSIC_SIZE_GB + MIN_CDJ_SIZE_GB) as f64;
                    let cdj_weight = MIN_CDJ_SIZE_GB as f64 / (MIN_MUSIC_SIZE_GB + MIN_CDJ_SIZE_GB) as f64;
                    
                    let music_size_gb = MIN_MUSIC_SIZE_GB + ((extra_gb as f64 * music_weight) as u64);
                    let cdj_size_gb = MIN_CDJ_SIZE_GB + ((extra_gb as f64 * cdj_weight) as u64);
                    
                    vec![
                        Partition {
                            device: DevicePath(format!("{}p1", primary_device)),
                            mount_point: MountPoint("/"),
                            label: PartitionLabel("root"),
                            size: PartitionSize::from_gb(ROOT_SIZE_GB),
                        },
                        Partition {
                            device: DevicePath(format!("{}p2", primary_device)),
                            mount_point: MountPoint("/var"),
                            label: PartitionLabel("var"),
                            size: PartitionSize::from_gb(VAR_SIZE_GB),
                        },
                        Partition {
                            device: DevicePath(format!("{}p3", primary_device)),
                            mount_point: MountPoint("/music"),
                            label: PartitionLabel("music"),
                            size: PartitionSize::from_gb(music_size_gb),
                        },
                        Partition {
                            device: DevicePath(format!("{}p4", primary_device)),
                            mount_point: MountPoint("/metadata"),
                            label: PartitionLabel("metadata"),
                            size: PartitionSize::from_gb(METADATA_SIZE_GB),
                        },
                        Partition {
                            device: DevicePath(format!("{}p5", primary_device)),
                            mount_point: MountPoint("/cdj-export"),
                            label: PartitionLabel("cdj-export"),
                            size: PartitionSize::from_gb(cdj_size_gb),
                        },
                    ]
                }
            }
            UnitType::Mdma303 => {
                // MDMA-303: Minimal + cache partition
                let os_overhead = PartitionSize::from_gb(ROOT_SIZE_GB + VAR_SIZE_GB);
                let remaining_bytes = primary_size_bytes.saturating_sub(os_overhead.bytes());
                let cache_size = PartitionSize(remaining_bytes);
                
                vec![
                    Partition {
                        device: DevicePath(format!("{}p1", primary_device)),
                        mount_point: MountPoint("/"),
                        label: PartitionLabel("root"),
                        size: PartitionSize::from_gb(ROOT_SIZE_GB),
                    },
                    Partition {
                        device: DevicePath(format!("{}p2", primary_device)),
                        mount_point: MountPoint("/var"),
                        label: PartitionLabel("var"),
                        size: PartitionSize::from_gb(VAR_SIZE_GB),
                    },
                    Partition {
                        device: DevicePath(format!("{}p3", primary_device)),
                        mount_point: MountPoint("/cache"),
                        label: PartitionLabel("cache"),
                        size: cache_size,
                    },
                ]
            }
        };

        // Check for secondary drive
        let layout = if let Some(secondary) = input.drives.secondary() {
            let secondary_size = PartitionSize(secondary.size_bytes);
            tracing::info!("Would partition secondary drive {} ({}) here", 
                secondary.device, 
                secondary_size
            );
            
            let secondary_partitions = vec![Partition {
                device: DevicePath(format!("{}p1", secondary.device)),
                mount_point: MountPoint("/cdj-export"),
                label: PartitionLabel("cdj-export"),
                size: secondary_size,
            }];

            PartitionLayout::DualDrive {
                primary_device: primary_device.clone(),
                primary_partitions: partitions,
                secondary_device: secondary.device.clone(),
                secondary_partitions,
            }
        } else {
            // Single drive configuration
            PartitionLayout::SingleDrive {
                device: primary_device,
                partitions,
            }
        };

        Ok(PartitionedDrives {
            validated: input,
            layout,
        })
    }
}
