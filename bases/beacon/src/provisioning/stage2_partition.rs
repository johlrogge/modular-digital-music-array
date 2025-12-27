// bases/beacon/src/provisioning/stage2_partition.rs
//! Stage 2: Partition NVMe drives

use crate::actions::{Action, ActionId, PlannedAction};
use crate::error::Result;
use crate::provisioning::types::{Partition, PartitionPlan, PartitionedDrives, ValidatedHardware};
use crate::types::ValidationError;

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
        use crate::provisioning::types::{
            DevicePath, MountPoint, PartitionLabel, PartitionSize, UnitType,
        };

        let primary_device = input.drives.primary().device.clone();
        let primary_size_bytes = input.drives.primary().size_bytes;

        // Constants (in GB)
        const ROOT_SIZE_GB: u64 = 16;
        const VAR_SIZE_GB: u64 = 8;
        const METADATA_SIZE_GB: u64 = 12; // Sufficient for extensive collections + playback history
        const MIN_MUSIC_SIZE_GB: u64 = 300;
        const MIN_CDJ_SIZE_GB: u64 = 64;

        // Music assignment threshold: Primary must be 50% larger than secondary to justify shared drive
        const MUSIC_SIZE_THRESHOLD_RATIO: f64 = 1.5;

        let partitions = match input.config.unit_type {
            UnitType::Mdma909 | UnitType::Mdma101 => {
                if let Some(secondary) = input.drives.secondary() {
                    // Two-drive config: Decide music placement based on size
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
                        // Primary is 50%+ larger: Music on primary (worth sharing drive)
                        // Primary: OS + metadata + music (most of drive)
                        // Secondary: CDJ export (full drive, dedicated)
                        tracing::info!(
                            "Two-drive (primary larger): Music on primary ({}GB available > {}GB secondary × {:.1})",
                            primary_available_gb,
                            secondary_size_gb,
                            MUSIC_SIZE_THRESHOLD_RATIO
                        );

                        let os_overhead = PartitionSize::from_gb(os_overhead_gb);
                        let remaining_bytes = primary_size_bytes.saturating_sub(os_overhead);
                        let music_size = remaining_bytes;

                        vec![
                            Partition {
                                device: DevicePath::new(format!("{}p1", primary_device))?,
                                mount_point: MountPoint::new("/"),
                                label: PartitionLabel::new("root"),
                                size: PartitionSize::from_gb(ROOT_SIZE_GB),
                            },
                            Partition {
                                device: DevicePath::new(format!("{}p2", primary_device))?,
                                mount_point: MountPoint::new("/var"),
                                label: PartitionLabel::new("var"),
                                size: PartitionSize::from_gb(VAR_SIZE_GB),
                            },
                            Partition {
                                device: DevicePath::new(format!("{}p3", primary_device))?,
                                mount_point: MountPoint::new("/metadata"),
                                label: PartitionLabel::new("metadata"),
                                size: PartitionSize::from_gb(METADATA_SIZE_GB),
                            },
                            Partition {
                                device: DevicePath::new(format!("{}p4", primary_device))?,
                                mount_point: MountPoint::new("/music"),
                                label: PartitionLabel::new("music"),
                                size: music_size,
                            },
                        ]
                    } else {
                        // Secondary dedicated to music (clean separation worth preserving)
                        // Primary: OS + metadata + CDJ export
                        // Secondary: Music (full drive, dedicated)
                        tracing::info!(
                            "Two-drive (dedicated music): Music on secondary ({}GB available < {}GB secondary × {:.1})",
                            primary_available_gb,
                            secondary_size_gb,
                            MUSIC_SIZE_THRESHOLD_RATIO
                        );

                        let os_overhead = PartitionSize::from_gb(os_overhead_gb);
                        let remaining_bytes = primary_size_bytes.saturating_sub(os_overhead);
                        let cdj_size = remaining_bytes;

                        vec![
                            Partition {
                                device: DevicePath::new(format!("{}p1", primary_device))?,
                                mount_point: MountPoint::new("/"),
                                label: PartitionLabel::new("root"),
                                size: PartitionSize::from_gb(ROOT_SIZE_GB),
                            },
                            Partition {
                                device: DevicePath::new(format!("{}p2", primary_device))?,
                                mount_point: MountPoint::new("/var"),
                                label: PartitionLabel::new("var"),
                                size: PartitionSize::from_gb(VAR_SIZE_GB),
                            },
                            Partition {
                                device: DevicePath::new(format!("{}p3", primary_device))?,
                                mount_point: MountPoint::new("/metadata"),
                                label: PartitionLabel::new("metadata"),
                                size: PartitionSize::from_gb(METADATA_SIZE_GB),
                            },
                            Partition {
                                device: DevicePath::new(format!("{}p4", primary_device))?,
                                mount_point: MountPoint::new("/cdj-export"),
                                label: PartitionLabel::new("cdj-export"),
                                size: cdj_size,
                            },
                        ]
                    }
                } else {
                    // Single-drive config: Split between music and CDJ
                    let os_overhead =
                        PartitionSize::from_gb(ROOT_SIZE_GB + VAR_SIZE_GB + METADATA_SIZE_GB);
                    let remaining_bytes = primary_size_bytes.saturating_sub(os_overhead);
                    let remaining_gb = remaining_bytes.gigabytes();

                    // Check minimums
                    let min_required_gb = MIN_MUSIC_SIZE_GB + MIN_CDJ_SIZE_GB;
                    if remaining_gb < min_required_gb {
                        return Err(crate::error::BeaconError::Validation(
                            ValidationError::DriveToSmall(
                            format!(
                            "{} has only {}GB after OS partitions, need at least {}GB for music ({}GB) + CDJ export ({}GB)",
                            primary_device,
                            remaining_gb,
                            min_required_gb,
                            MIN_MUSIC_SIZE_GB,
                            MIN_CDJ_SIZE_GB
                        ))));
                    }

                    // Calculate proportional sizes
                    let extra_gb = remaining_gb - min_required_gb;
                    let music_weight =
                        MIN_MUSIC_SIZE_GB as f64 / (MIN_MUSIC_SIZE_GB + MIN_CDJ_SIZE_GB) as f64;
                    let cdj_weight =
                        MIN_CDJ_SIZE_GB as f64 / (MIN_MUSIC_SIZE_GB + MIN_CDJ_SIZE_GB) as f64;

                    let music_size_gb =
                        MIN_MUSIC_SIZE_GB + ((extra_gb as f64 * music_weight) as u64);
                    let cdj_size_gb = MIN_CDJ_SIZE_GB + ((extra_gb as f64 * cdj_weight) as u64);

                    tracing::info!(
                        "Single-drive partition sizing: {} total, {} music, {} CDJ export",
                        primary_size_bytes,
                        PartitionSize::from_gb(music_size_gb),
                        PartitionSize::from_gb(cdj_size_gb)
                    );

                    vec![
                        Partition {
                            device: DevicePath::new(format!("{}p1", primary_device))?,
                            mount_point: MountPoint::new("/"),
                            label: PartitionLabel::new("root"),
                            size: PartitionSize::from_gb(ROOT_SIZE_GB),
                        },
                        Partition {
                            device: DevicePath::new(format!("{}p2", primary_device))?,
                            mount_point: MountPoint::new("/var"),
                            label: PartitionLabel::new("var"),
                            size: PartitionSize::from_gb(VAR_SIZE_GB),
                        },
                        Partition {
                            device: DevicePath::new(format!("{}p3", primary_device))?,
                            mount_point: MountPoint::new("/music"),
                            label: PartitionLabel::new("music"),
                            size: PartitionSize::from_gb(music_size_gb),
                        },
                        Partition {
                            device: DevicePath::new(format!("{}p4", primary_device))?,
                            mount_point: MountPoint::new("/metadata"),
                            label: PartitionLabel::new("metadata"),
                            size: PartitionSize::from_gb(METADATA_SIZE_GB),
                        },
                        Partition {
                            device: DevicePath::new(format!("{}p5", primary_device))?,
                            mount_point: MountPoint::new("/cdj-export"),
                            label: PartitionLabel::new("cdj-export"),
                            size: PartitionSize::from_gb(cdj_size_gb),
                        },
                    ]
                }
            }
            UnitType::Mdma303 => {
                // MDMA-303: Minimal + cache partition
                let os_overhead = PartitionSize::from_gb(ROOT_SIZE_GB + VAR_SIZE_GB);
                let remaining_bytes = primary_size_bytes.saturating_sub(os_overhead);
                let cache_size = remaining_bytes;

                vec![
                    Partition {
                        device: DevicePath::new(format!("{}p1", primary_device))?,
                        mount_point: MountPoint::new("/"),
                        label: PartitionLabel::new("root"),
                        size: PartitionSize::from_gb(ROOT_SIZE_GB),
                    },
                    Partition {
                        device: DevicePath::new(format!("{}p2", primary_device))?,
                        mount_point: MountPoint::new("/var"),
                        label: PartitionLabel::new("var"),
                        size: PartitionSize::from_gb(VAR_SIZE_GB),
                    },
                    Partition {
                        device: DevicePath::new(format!("{}p3", primary_device))?,
                        mount_point: MountPoint::new("/cache"),
                        label: PartitionLabel::new("cache"),
                        size: cache_size,
                    },
                ]
            }
        };

        // Check for secondary drive and assign based on size comparison
        let plan = if let Some(secondary) = input.drives.secondary() {
            let secondary_size_gb = secondary.size_bytes.gigabytes();
            let primary_total_gb = primary_size_bytes.gigabytes();

            // Use RAW drive size comparison (not available after OS overhead)
            let primary_much_larger = (primary_total_gb as f64)
                >= (secondary_size_gb as f64 * MUSIC_SIZE_THRESHOLD_RATIO);

            let secondary_partitions = if primary_much_larger {
                // Music on primary → secondary gets CDJ export (full drive)
                vec![Partition {
                    device: DevicePath::new(format!("{}p1", secondary.device))?,
                    mount_point: MountPoint::new("/cdj-export"),
                    label: PartitionLabel::new("cdj-export"),
                    size: secondary.size_bytes,
                }]
            } else {
                // Music on secondary → secondary gets music (full drive, dedicated)
                vec![Partition {
                    device: DevicePath::new(format!("{}p1", secondary.device))?,
                    mount_point: MountPoint::new("/music"),
                    label: PartitionLabel::new("music"),
                    size: secondary.size_bytes,
                }]
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

        let assumed_output = PartitionedDrives {
            validated: input.clone(),
            plan,
        };

        Ok(PlannedAction {
            description: self.description(),
            action: self.clone(),
            input: input.clone(),
            assumed_output,
        })
    }

    async fn apply(&self, planned_output: &PartitionedDrives) -> Result<PartitionedDrives> {
        tracing::info!("Stage 2: Partition disks - executing plan");

        // Execute the plan that was already calculated in plan()
        match &planned_output.plan {
            PartitionPlan::SingleDrive { device, partitions } => {
                for partition in partitions {
                    tracing::info!(
                        "Would execute: parted {} mkpart primary {} {}B {}B",
                        device,
                        partition.label,
                        0, // TODO: Calculate actual start position
                        partition.size.bytes()
                    );
                    // TODO: Real implementation:
                    // Command::new("parted")
                    //     .arg(device.as_str())
                    //     .arg("mkpart")
                    //     .arg("primary")
                    //     .arg(partition.label.0)
                    //     .arg(format!("{}B", start_pos))
                    //     .arg(format!("{}B", end_pos))
                    //     .output()?;
                }
            }
            PartitionPlan::DualDrive {
                primary_device,
                primary_partitions,
                secondary_device,
                secondary_partitions,
            } => {
                tracing::info!("Partitioning primary drive: {}", primary_device);
                for partition in primary_partitions {
                    tracing::info!(
                        "Would execute: parted {} mkpart primary {} {}B {}B",
                        primary_device,
                        partition.label,
                        0,
                        partition.size.bytes()
                    );
                }

                tracing::info!("Partitioning secondary drive: {}", secondary_device);
                for partition in secondary_partitions {
                    tracing::info!(
                        "Would execute: parted {} mkpart primary {} {}B {}B",
                        secondary_device,
                        partition.label,
                        0,
                        partition.size.bytes()
                    );
                }
            }
        }

        tracing::info!("Partition stage complete (simulated)");

        // Return the executed plan (which matches what we planned)
        Ok(planned_output.clone())
    }
}

#[cfg(test)]
mod tests {

    use crate::{
        provisioning::{DriveInfo, UnitType, ValidatedDrives},
        types::{DevicePath, Hostname, SshPublicKey},
    };

    use super::*;
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
                ssh_key: SshPublicKey::new("public key".to_owned()).expect("works"),
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
                    .map(|p| (p.mount_point.to_string(), p.size.gigabytes()))
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
                    .map(|p| (p.mount_point.to_string(), p.size.gigabytes()))
                    .collect();
                let secondary_info: Vec<_> = secondary_partitions
                    .iter()
                    .map(|p| (p.mount_point.to_string(), p.size.gigabytes()))
                    .collect();
                (primary_info, Some(secondary_info))
            }
        }
    }

    #[rstest]
    #[case::equal_drives_512gb(
        512, 512,
        "Equal drives → dedicated music on secondary",
        vec![("/", 16), ("/var", 8), ("/metadata", 12), ("/cdj-export", 476)],
        Some(vec![("/music", 512)])
    )]
    #[case::slightly_larger_primary_640gb(
        640, 512,
        "Primary 1.25× (640/512) → dedicated music on secondary (below 1.5× threshold)",
        vec![("/", 16), ("/var", 8), ("/metadata", 12), ("/cdj-export", 604)],
        Some(vec![("/music", 512)])
    )]
    #[case::threshold_case_768gb(
        768, 512,
        "Primary 1.5× (768/512) → music on primary (exactly at threshold)",
        vec![("/", 16), ("/var", 8), ("/metadata", 12), ("/music", 732)],
        Some(vec![("/cdj-export", 512)])
    )]
    #[case::large_primary_1tb(
        1024, 512,
        "Primary 2.0× (1024/512) → music on primary (well above threshold)",
        vec![("/", 16), ("/var", 8), ("/metadata", 12), ("/music", 988)],
        Some(vec![("/cdj-export", 512)])
    )]
    #[case::huge_primary_2tb(
        2048, 512,
        "Primary 4.0× (2048/512) → music on primary (massive difference)",
        vec![("/", 16), ("/var", 8), ("/metadata", 12), ("/music", 2012)],
        Some(vec![("/cdj-export", 512)])
    )]
    #[case::just_below_threshold(
        730, 512,
        "Primary 1.43× (730/512) → dedicated music on secondary (just below 1.5×)",
        vec![("/", 16), ("/var", 8), ("/metadata", 12), ("/cdj-export", 694)],
        Some(vec![("/music", 512)])
    )]
    #[case::just_above_threshold(
        800, 512,
        "Primary 1.56× (800/512) → music on primary (just above 1.5×)",
        vec![("/", 16), ("/var", 8), ("/metadata", 12), ("/music", 764)],
        Some(vec![("/cdj-export", 512)])
    )]
    #[tokio::test]
    async fn test_two_drive_size_based_assignment(
        #[case] primary_gb: u64,
        #[case] secondary_gb: u64,
        #[case] description: &str,
        #[case] expected_primary: Vec<(&str, u64)>,
        #[case] expected_secondary: Option<Vec<(&str, u64)>>,
    ) {
        let input = create_validated_drives(primary_gb, Some(secondary_gb));
        let action = PartitionDrivesAction;

        let planned = action.plan(&input).await.expect("Planning should succeed");
        let (primary_info, secondary_info) = get_partition_info(&planned.assumed_output.plan);

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
        "Single 512GB drive",
        vec![("/", 16), ("/var", 8), ("/music", 392), ("/metadata", 12), ("/cdj-export", 83)]
    )]
    #[case::large_1tb(
        1024,
        "Single 1TB drive",
        vec![("/", 16), ("/var", 8), ("/music", 814), ("/metadata", 12), ("/cdj-export", 173)]
    )]
    #[tokio::test]
    async fn test_single_drive_partitioning(
        #[case] drive_gb: u64,
        #[case] description: &str,
        #[case] expected: Vec<(&str, u64)>,
    ) {
        let input = create_validated_drives(drive_gb, None);
        let action = PartitionDrivesAction;

        let planned = action.plan(&input).await.expect("Planning should succeed");
        let (primary_info, secondary_info) = get_partition_info(&planned.assumed_output.plan);

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
                "{}: Partition {} mount point mismatch",
                description, i
            );
            assert_eq!(
                *actual_gb, *expected_gb,
                "{}: Partition {} size mismatch (expected {}GB, got {}GB)",
                description, i, expected_gb, actual_gb
            );
        }
    }

    #[tokio::test]
    async fn test_music_capacity_at_threshold() {
        // 768GB primary (exactly 1.5× after OS overhead)
        let input = create_validated_drives(768, Some(512));
        let action = PartitionDrivesAction;

        let planned = action.plan(&input).await.unwrap();
        let (primary_info, secondary_info) = get_partition_info(&planned.assumed_output.plan);

        // Music should be on primary with 732 GB
        let music_partition = primary_info.iter().find(|(mount, _)| mount == "/music");
        assert!(
            music_partition.is_some(),
            "Music partition should exist on primary"
        );
        assert_eq!(
            music_partition.unwrap().1,
            732,
            "Music partition should be 732GB"
        );

        // Secondary should have CDJ export with full 512 GB
        let secondary = secondary_info.unwrap();
        assert_eq!(secondary.len(), 1, "Secondary should have one partition");
        assert_eq!(
            secondary[0].0, "/cdj-export",
            "Secondary should be CDJ export"
        );
        assert_eq!(secondary[0].1, 512, "CDJ export should be 512GB");

        // Verify capacity gain: 732GB vs 512GB = +220GB (+43%)
        let gain_gb = 732 - 512;
        let gain_percent = (gain_gb as f64 / 512.0) * 100.0;
        assert!(gain_percent >= 40.0, "Capacity gain should be at least 40%");
    }

    #[tokio::test]
    async fn test_just_below_threshold_keeps_dedicated() {
        // 730GB primary (1.43× after OS overhead) - just below 1.5×
        let input = create_validated_drives(730, Some(512));
        let action = PartitionDrivesAction;

        let planned = action.plan(&input).await.unwrap();
        let (primary_info, secondary_info) = get_partition_info(&planned.assumed_output.plan);

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

    #[tokio::test]
    async fn test_realistic_scenario_equal_drives() {
        // Most common case: 2× 512GB drives
        let input = create_validated_drives(512, Some(512));
        let action = PartitionDrivesAction;

        let planned = action.plan(&input).await.unwrap();
        let (primary_info, secondary_info) = get_partition_info(&planned.assumed_output.plan);

        // Should use clean separation pattern
        let cdj_on_primary = primary_info.iter().any(|(mount, _)| mount == "/cdj-export");
        let music_on_secondary = secondary_info
            .as_ref()
            .unwrap()
            .iter()
            .any(|(mount, _)| mount == "/music");

        assert!(cdj_on_primary, "CDJ export should be on primary");
        assert!(
            music_on_secondary,
            "Music should be on secondary (dedicated)"
        );

        // Verify capacities
        let binding = secondary_info.unwrap();
        let music = binding.iter().find(|(m, _)| m == "/music").unwrap();
        assert_eq!(music.1, 512, "Music should get full 512GB dedicated");
    }
}
