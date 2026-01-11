// bases/beacon/src/provisioning/stage4_install.rs
//! Stage 4: Install base system
//!
//! This stage is implemented as a composite of sub-actions:
//! 1. Mount partitions
//! 2. Install packages (xbps-install base-system)
//! 3. Configure fstab
//! 4. Unmount partitions
//!
//! Each sub-action checks current state in plan() and only acts if needed.

use crate::actions::{Action, ActionId, PlannedAction};
use crate::error::Result;
use crate::provisioning::types::{
    ConfiguredFstab, FstabPlan, FstabState, FormattedSystem, InstallPlan, InstallState,
    InstalledPackages, InstalledSystem, MountPlan, MountState, MountedPartitions, Partition,
    UnmountPlan,
};
use std::path::PathBuf;
use tokio::process::Command;

// ============================================================================
// Sub-Action 1: Mount Partitions
// ============================================================================

#[derive(Clone, Debug)]
pub struct MountPartitionsAction;

impl Action<FormattedSystem, MountPlan, MountedPartitions> for MountPartitionsAction {
    fn id(&self) -> ActionId {
        ActionId::new("mount-partitions")
    }

    fn description(&self) -> String {
        "Mount formatted partitions".to_string()
    }

    async fn plan(
        &self,
        input: &FormattedSystem,
    ) -> Result<PlannedAction<FormattedSystem, MountPlan, MountedPartitions, Self>> {
        let mount_root = PathBuf::from("/mnt/mdma-install");

        // Collect all partitions from the partition plan
        let partitions: Vec<Partition> = match &input.partitioned.plan {
            crate::provisioning::types::CompletedPartitionPlan::SingleDrive { partitions, .. } => {
                partitions.clone()
            }
            crate::provisioning::types::CompletedPartitionPlan::DualDrive {
                primary_partitions,
                secondary_partitions,
                ..
            } => {
                let mut all = primary_partitions.clone();
                all.extend(secondary_partitions.clone());
                all
            }
        };

        // Check which partitions are already mounted
        let mut mount_states = Vec::new();
        for partition in &partitions {
            let is_mounted = check_if_mounted(&partition.device.as_str()).await?;

            if is_mounted {
                tracing::info!(
                    "{} is already mounted, will skip",
                    partition.device
                );
                mount_states.push(MountState::AlreadyMounted(partition.clone()));
            } else {
                mount_states.push(MountState::NeedsMount(partition.clone()));
            }
        }

        let planned_work = MountPlan {
            formatted: input.clone(),  // Store input in plan
            mount_root: mount_root.clone(),
            partitions: mount_states,
        };

        let assumed_output = MountedPartitions {
            formatted: input.clone(),
            mount_root,
            partitions,
        };

        Ok(PlannedAction {
            description: self.description(),
            action: self.clone(),
            input: input.clone(),
            planned_work,
            assumed_output,
        })
    }

    async fn apply(&self, plan: &MountPlan) -> Result<MountedPartitions> {
        tracing::info!("Mounting partitions to {}", plan.mount_root.display());

        for mount_state in &plan.partitions {
            match mount_state {
                MountState::NeedsMount(partition) => {
                    mount_partition(&plan.mount_root, partition).await?;
                }
                MountState::AlreadyMounted(partition) => {
                    tracing::info!("{} already mounted, skipping", partition.device);
                }
            }
        }

        // Verify all partitions are mounted
        verify_all_mounted(&plan.mount_root, &plan.partitions).await?;

        // Extract partitions from mount states
        let partitions = plan
            .partitions
            .iter()
            .map(|state| state.partition().clone())
            .collect();

        // Use the formatted system from the plan
        Ok(MountedPartitions {
            formatted: plan.formatted.clone(),
            mount_root: plan.mount_root.clone(),
            partitions,
        })
    }
}

/// Check if a device is currently mounted
async fn check_if_mounted(device: &str) -> Result<bool> {
    let output = Command::new("mount")
        .output()
        .await
        .map_err(|e| crate::error::BeaconError::command_failed("mount", e))?;

    if !output.status.success() {
        return Ok(false);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.contains(device))
}

/// Mount a single partition
async fn mount_partition(mount_root: &PathBuf, partition: &Partition) -> Result<()> {
    use crate::provisioning::types::MountPoint;

    // Determine target mount path
    let target_path = match partition.mount_point {
        MountPoint::Root => mount_root.clone(),
        _ => mount_root.join(partition.mount_point.as_path().strip_prefix("/").unwrap()),
    };

    // Create mount point directory
    tracing::info!("Creating mount point: {}", target_path.display());
    tokio::fs::create_dir_all(&target_path)
        .await
        .map_err(|e| crate::error::BeaconError::Provisioning(
            format!("Failed to create mount point {}: {}", target_path.display(), e)
        ))?;

    // Mount the partition
    tracing::info!(
        "Mounting {} to {}",
        partition.device,
        target_path.display()
    );

    let output = Command::new("mount")
        .arg(partition.device.as_str())
        .arg(&target_path)
        .output()
        .await
        .map_err(|e| crate::error::BeaconError::command_failed("mount", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(crate::error::BeaconError::Provisioning(format!(
            "Failed to mount {} to {}: {}",
            partition.device,
            target_path.display(),
            stderr
        )));
    }

    tracing::info!("✅ Mounted {} successfully", partition.device);
    Ok(())
}

/// Verify all partitions are mounted correctly
async fn verify_all_mounted(_mount_root: &PathBuf, mount_states: &[MountState]) -> Result<()> {
    tracing::info!("Verifying all partitions are mounted...");

    for mount_state in mount_states {
        let partition = mount_state.partition();
        let is_mounted = check_if_mounted(&partition.device.as_str()).await?;

        if !is_mounted {
            return Err(crate::error::BeaconError::Provisioning(format!(
                "Verification failed: {} is not mounted",
                partition.device
            )));
        }

        tracing::debug!("✅ {} verified mounted", partition.device);
    }

    tracing::info!("✅ All {} partition(s) verified mounted", mount_states.len());
    Ok(())
}

// ============================================================================
// Sub-Action 2: Install Packages (STUBBED)
// ============================================================================

#[derive(Clone, Debug)]
pub struct InstallPackagesAction;

impl Action<MountedPartitions, InstallPlan, InstalledPackages> for InstallPackagesAction {
    fn id(&self) -> ActionId {
        ActionId::new("install-packages")
    }

    fn description(&self) -> String {
        "Install base system packages".to_string()
    }

    async fn plan(
        &self,
        input: &MountedPartitions,
    ) -> Result<PlannedAction<MountedPartitions, InstallPlan, InstalledPackages, Self>> {
        // TODO: Check if base-system is already installed
        let install_state = InstallState::NeedsInstall;

        let packages = vec!["base-system".to_string()];

        let planned_work = InstallPlan {
            mount_root: input.mount_root.clone(),
            packages: packages.clone(),
            install_state,
        };

        let assumed_output = InstalledPackages {
            mounted: input.clone(),
            packages,
        };

        Ok(PlannedAction {
            description: self.description(),
            action: self.clone(),
            input: input.clone(),
            planned_work,
            assumed_output,
        })
    }

    async fn apply(&self, plan: &InstallPlan) -> Result<InstalledPackages> {
        tracing::info!("TODO: Install packages to {}", plan.mount_root.display());
        tracing::info!("  Packages: {}", plan.packages.join(", "));
        tracing::info!("  Would execute: xbps-install -Sy -R https://repo-default.voidlinux.org/current -r {} {}",
            plan.mount_root.display(),
            plan.packages.join(" ")
        );

        // Stub: This will be implemented in a future session
        // For now, we just return what we planned - the input MountedPartitions is in the PlannedAction
        // We can't access it from here, so we'll need to construct it
        // Actually, we should store the input in the plan!
        
        // FIXME: Need to add `mounted: MountedPartitions` to InstallPlan
        // For now, create a minimal stub that won't panic
        Err(crate::error::BeaconError::Provisioning(
            "InstallPackagesAction not yet implemented - coming in next session!".to_string()
        ))
    }
}

// ============================================================================
// Sub-Action 3: Configure fstab (STUBBED)
// ============================================================================

#[derive(Clone, Debug)]
pub struct ConfigureFstabAction;

impl Action<InstalledPackages, FstabPlan, ConfiguredFstab> for ConfigureFstabAction {
    fn id(&self) -> ActionId {
        ActionId::new("configure-fstab")
    }

    fn description(&self) -> String {
        "Configure /etc/fstab".to_string()
    }

    async fn plan(
        &self,
        input: &InstalledPackages,
    ) -> Result<PlannedAction<InstalledPackages, FstabPlan, ConfiguredFstab, Self>> {
        // TODO: Check if fstab is already configured
        let config_state = FstabState::NeedsConfig;

        let planned_work = FstabPlan {
            mount_root: input.mounted.mount_root.clone(),
            partitions: input.mounted.partitions.clone(),
            config_state,
        };

        let assumed_output = ConfiguredFstab {
            installed: input.clone(),
        };

        Ok(PlannedAction {
            description: self.description(),
            action: self.clone(),
            input: input.clone(),
            planned_work,
            assumed_output,
        })
    }

    async fn apply(&self, plan: &FstabPlan) -> Result<ConfiguredFstab> {
        tracing::info!("TODO: Configure fstab at {}/etc/fstab", plan.mount_root.display());
        tracing::info!("  Partitions: {}", plan.partitions.len());

        // Stub: This will be implemented in a future session
        Err(crate::error::BeaconError::Provisioning(
            "ConfigureFstabAction not yet implemented - coming in a future session!".to_string()
        ))
    }
}

// ============================================================================
// Sub-Action 4: Unmount Partitions (STUBBED - Keep last for inspection)
// ============================================================================

#[derive(Clone, Debug)]
pub struct UnmountPartitionsAction;

impl Action<ConfiguredFstab, UnmountPlan, InstalledSystem> for UnmountPartitionsAction {
    fn id(&self) -> ActionId {
        ActionId::new("unmount-partitions")
    }

    fn description(&self) -> String {
        "Unmount all partitions".to_string()
    }

    async fn plan(
        &self,
        input: &ConfiguredFstab,
    ) -> Result<PlannedAction<ConfiguredFstab, UnmountPlan, InstalledSystem, Self>> {
        // TODO: Check which mount points need unmounting
        let mount_points = vec![]; // Stub

        let planned_work = UnmountPlan { mount_points };

        let assumed_output = InstalledSystem {
            formatted: input.installed.mounted.formatted.clone(),
        };

        Ok(PlannedAction {
            description: self.description(),
            action: self.clone(),
            input: input.clone(),
            planned_work,
            assumed_output,
        })
    }

    async fn apply(&self, _plan: &UnmountPlan) -> Result<InstalledSystem> {
        tracing::info!("TODO: Unmount partitions");
        
        // Stub: Keep this stubbed intentionally so we can inspect mounted filesystems
        // This will be the LAST thing we implement
        Err(crate::error::BeaconError::Provisioning(
            "UnmountPartitionsAction not yet implemented - keeping partitions mounted for inspection!".to_string()
        ))
    }
}

// ============================================================================
// Composite Action: Install System
// ============================================================================

/// Planned work for the composite installation
/// 
/// Stores just the planned work from each sub-action (not the PlannedActions themselves)
/// since PlannedAction doesn't implement Clone.
#[derive(Debug, Clone, PartialEq)]
pub struct InstallationPlan {
    pub mount_plan: MountPlan,
    pub install_plan: InstallPlan,
    pub configure_plan: FstabPlan,
    pub unmount_plan: UnmountPlan,
}

impl std::fmt::Display for InstallationPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "📝 Installation plan with 4 sub-stages:")?;
        writeln!(f, "  1. {}", self.mount_plan)?;
        writeln!(f, "  2. {}", self.install_plan)?;
        writeln!(f, "  3. {}", self.configure_plan)?;
        writeln!(f, "  4. {}", self.unmount_plan)?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct InstallSystemAction;

impl Action<FormattedSystem, InstallationPlan, InstalledSystem> for InstallSystemAction {
    fn id(&self) -> ActionId {
        ActionId::new("install-system")
    }

    fn description(&self) -> String {
        "Install base operating system".to_string()
    }

    async fn plan(
        &self,
        input: &FormattedSystem,
    ) -> Result<PlannedAction<FormattedSystem, InstallationPlan, InstalledSystem, Self>> {
        tracing::info!("Planning installation with sub-actions...");

        // Plan each sub-action, chaining outputs to inputs
        let mount_planned = MountPartitionsAction.plan(input).await?;
        let install_planned = InstallPackagesAction.plan(&mount_planned.assumed_output).await?;
        let configure_planned = ConfigureFstabAction.plan(&install_planned.assumed_output).await?;
        let unmount_planned = UnmountPartitionsAction.plan(&configure_planned.assumed_output).await?;

        // Extract just the planned work (the PlannedActions aren't Clone)
        let installation_plan = InstallationPlan {
            mount_plan: mount_planned.planned_work,
            install_plan: install_planned.planned_work,
            configure_plan: configure_planned.planned_work,
            unmount_plan: unmount_planned.planned_work,
        };

        // Final output after all sub-actions complete
        let final_output = unmount_planned.assumed_output;

        Ok(PlannedAction {
            description: self.description(),
            action: self.clone(),
            input: input.clone(),
            planned_work: installation_plan,
            assumed_output: final_output,
        })
    }

    async fn apply(&self, plan: &InstallationPlan) -> Result<InstalledSystem> {
        tracing::info!("Executing installation with 4 sub-stages");

        // Execute each sub-action in sequence (recreate actions - they're zero-sized)
        tracing::info!("Stage 1/4: Mount partitions");
        let _mounted = MountPartitionsAction.apply(&plan.mount_plan).await?;
        
        tracing::info!("Stage 2/4: Install packages");
        let _installed = InstallPackagesAction.apply(&plan.install_plan).await?;
        
        tracing::info!("Stage 3/4: Configure fstab");
        let _configured = ConfigureFstabAction.apply(&plan.configure_plan).await?;
        
        tracing::info!("Stage 4/4: Unmount partitions");
        let final_output = UnmountPartitionsAction.apply(&plan.unmount_plan).await?;

        tracing::info!("✅ Installation complete");
        Ok(final_output)
    }
}
