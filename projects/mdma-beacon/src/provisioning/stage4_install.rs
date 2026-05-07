// bases/beacon/src/provisioning/stage4_install.rs
//! Stage 4: Install base system
//!
//! This stage is implemented as a composite of sub-actions:
//! 1. Mount partitions
//! 2. Install packages (xbps-install base-system)
//! 3. Configure fstab
//!
//! Note: Unmount is handled by stage 6 (finalize) so that stage 5 (configure)
//! can work with the mounted filesystem.
//!
//! Each sub-action checks current state in plan() and only acts if needed.

use crate::actions::{Action, ActionId, PlannedAction};
use crate::error::Result;
use crate::provisioning::types::{
    ConfiguredFstab, FormattedSystem, FstabPlan, FstabState, InstallPlan, InstallState,
    InstalledPackages, MountPlan, MountState, MountedPartitions, Partition,
};
use std::path::{Path, PathBuf};
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
            crate::provisioning::types::CompletedPartitionPlan::SingleDrive {
                partitions, ..
            } => partitions.clone(),
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
            let is_mounted = check_if_mounted(partition.device.as_str()).await?;

            if is_mounted {
                tracing::info!("{} is already mounted, will skip", partition.device);
                mount_states.push(MountState::AlreadyMounted(partition.clone()));
            } else {
                mount_states.push(MountState::NeedsMount(partition.clone()));
            }
        }

        let planned_work = MountPlan {
            formatted: input.clone(), // Store input in plan
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
async fn mount_partition(mount_root: &std::path::Path, partition: &Partition) -> Result<()> {
    use crate::provisioning::types::MountPoint;

    // Determine target mount path
    let target_path = match partition.mount_point {
        MountPoint::Root => mount_root.to_path_buf(),
        _ => mount_root.join(partition.mount_point.as_path().strip_prefix("/").unwrap()),
    };

    // Create mount point directory
    tracing::info!("Creating mount point: {}", target_path.display());
    tokio::fs::create_dir_all(&target_path).await.map_err(|e| {
        crate::error::BeaconError::Provisioning(format!(
            "Failed to create mount point {}: {}",
            target_path.display(),
            e
        ))
    })?;

    // Mount the partition
    tracing::info!("Mounting {} to {}", partition.device, target_path.display());

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
        let is_mounted = check_if_mounted(partition.device.as_str()).await?;

        if !is_mounted {
            return Err(crate::error::BeaconError::Provisioning(format!(
                "Verification failed: {} is not mounted",
                partition.device
            )));
        }

        tracing::debug!("✅ {} verified mounted", partition.device);
    }

    tracing::info!(
        "✅ All {} partition(s) verified mounted",
        mount_states.len()
    );
    Ok(())
}

// ============================================================================
// Sub-Action 2: Install Packages
// ============================================================================

#[derive(Clone, Debug)]
pub struct InstallPackagesAction;

impl Action<MountedPartitions, InstallPlan, InstalledPackages> for InstallPackagesAction {
    fn id(&self) -> ActionId {
        ActionId::new("install-packages")
    }

    fn description(&self) -> String {
        "Install base system and RPi kernel packages".to_string()
    }

    async fn plan(
        &self,
        input: &MountedPartitions,
    ) -> Result<PlannedAction<MountedPartitions, InstallPlan, InstalledPackages, Self>> {
        // Install both base-system (userland) and rpi-base (RPi kernel + firmware)
        let packages = vec![
            "base-system".to_string(),
            "rpi-base".to_string(),
            "rpi-eeprom".to_string(),
        ];

        // Check if base-system is already installed by looking for a key binary
        let install_state = check_if_base_system_installed(&input.mount_root).await;

        let planned_work = InstallPlan {
            mounted: input.clone(),
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
        match plan.install_state {
            InstallState::NeedsInstall => {
                install_base_system(&plan.mounted.mount_root, &plan.packages).await?;
            }
            InstallState::AlreadyInstalled => {
                tracing::info!("Base system already installed, skipping");
            }
        }

        Ok(InstalledPackages {
            mounted: plan.mounted.clone(),
            packages: plan.packages.clone(),
        })
    }
}

/// Check if base-system and rpi-base are already installed
async fn check_if_base_system_installed(mount_root: &std::path::Path) -> InstallState {
    // Check for /usr/bin/xbps-query (from base-system)
    let xbps_query_path = mount_root.join("usr/bin/xbps-query");
    // Check for /boot/kernel8.img (from rpi-base/rpi-kernel)
    let kernel_path = mount_root.join("boot/kernel8.img");

    let base_system_exists = tokio::fs::try_exists(&xbps_query_path)
        .await
        .unwrap_or(false);
    let rpi_kernel_exists = tokio::fs::try_exists(&kernel_path).await.unwrap_or(false);

    if base_system_exists && rpi_kernel_exists {
        tracing::info!(
            "Found {} and {} - system appears fully installed",
            xbps_query_path.display(),
            kernel_path.display()
        );
        InstallState::AlreadyInstalled
    } else {
        if !base_system_exists {
            tracing::info!(
                "{} not found - base-system needs to be installed",
                xbps_query_path.display()
            );
        }
        if !rpi_kernel_exists {
            tracing::info!(
                "{} not found - rpi-base needs to be installed",
                kernel_path.display()
            );
        }
        InstallState::NeedsInstall
    }
}

/// Copy xbps repository keys to the target root
///
/// This is required before running xbps-install to a fresh root directory,
/// otherwise xbps will prompt interactively for key import which fails
/// without a TTY.
async fn copy_xbps_keys(mount_root: &Path) -> Result<()> {
    let source_keys = std::path::Path::new("/var/db/xbps/keys");
    let target_keys = mount_root.join("var/db/xbps/keys");

    tracing::info!(
        "Copying xbps repository keys from {} to {}",
        source_keys.display(),
        target_keys.display()
    );

    // Create target directory
    tokio::fs::create_dir_all(&target_keys).await.map_err(|e| {
        crate::error::BeaconError::Provisioning(format!(
            "Failed to create {}: {}",
            target_keys.display(),
            e
        ))
    })?;

    // Copy all key files
    let mut entries = tokio::fs::read_dir(source_keys).await.map_err(|e| {
        crate::error::BeaconError::Provisioning(format!(
            "Failed to read {}: {}",
            source_keys.display(),
            e
        ))
    })?;

    let mut copied_count = 0;
    while let Some(entry) = entries.next_entry().await.map_err(|e| {
        crate::error::BeaconError::Provisioning(format!("Failed to read directory entry: {}", e))
    })? {
        let path = entry.path();
        if path.is_file() {
            let filename = path.file_name().unwrap();
            let target_path = target_keys.join(filename);

            tokio::fs::copy(&path, &target_path).await.map_err(|e| {
                crate::error::BeaconError::Provisioning(format!(
                    "Failed to copy {} to {}: {}",
                    path.display(),
                    target_path.display(),
                    e
                ))
            })?;

            tracing::info!("  Copied key: {:?}", filename);
            copied_count += 1;
        }
    }

    if copied_count == 0 {
        return Err(crate::error::BeaconError::Provisioning(format!(
            "No xbps keys found in {}",
            source_keys.display()
        )));
    }

    tracing::info!("✅ Copied {} xbps repository key(s)", copied_count);
    Ok(())
}

/// Install base system packages using xbps-install
async fn install_base_system(mount_root: &PathBuf, packages: &[String]) -> Result<()> {
    // Void Linux repo URL for aarch64 (Raspberry Pi 5)
    let repo_url = "https://repo-default.voidlinux.org/current/aarch64";

    tracing::info!(
        "Installing packages to {}: {}",
        mount_root.display(),
        packages.join(", ")
    );
    tracing::info!("Using repository: {}", repo_url);

    // Copy xbps repository keys to target root first
    // This prevents the interactive key import prompt
    copy_xbps_keys(mount_root).await?;

    // Build the command
    // xbps-install -Sy -R <repo> -r <root> <packages>
    // -S: sync repository index
    // -y: assume yes to all questions
    // -R: repository URL
    // -r: target root directory
    let mut cmd = Command::new("xbps-install");
    cmd.arg("-Sy")
        .arg("-R")
        .arg(repo_url)
        .arg("-r")
        .arg(mount_root)
        .args(packages);

    tracing::info!(
        "Executing: xbps-install -Sy -R {} -r {} {}",
        repo_url,
        mount_root.display(),
        packages.join(" ")
    );

    let output = cmd
        .output()
        .await
        .map_err(|e| crate::error::BeaconError::command_failed("xbps-install", e))?;

    // Log stdout for visibility
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.is_empty() {
        for line in stdout.lines() {
            tracing::info!("[xbps-install] {}", line);
        }
    }

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::error!("xbps-install failed with status: {}", output.status);
        for line in stderr.lines() {
            tracing::error!("[xbps-install stderr] {}", line);
        }
        return Err(crate::error::BeaconError::Provisioning(format!(
            "xbps-install failed: {}",
            stderr.trim()
        )));
    }

    // Verify installation succeeded - check for key files from both packages
    let xbps_query_path = mount_root.join("usr/bin/xbps-query");
    if !tokio::fs::try_exists(&xbps_query_path)
        .await
        .unwrap_or(false)
    {
        return Err(crate::error::BeaconError::Provisioning(format!(
            "xbps-install reported success but {} not found - base-system installation may have failed",
            xbps_query_path.display()
        )));
    }

    // Verify rpi-base installed (check for kernel image)
    let kernel_path = mount_root.join("boot/kernel8.img");
    if !tokio::fs::try_exists(&kernel_path).await.unwrap_or(false) {
        return Err(crate::error::BeaconError::Provisioning(format!(
            "xbps-install reported success but {} not found - rpi-base installation may have failed",
            kernel_path.display()
        )));
    }

    tracing::info!("✅ Base system and RPi kernel installed successfully");
    Ok(())
}

// ============================================================================
// Sub-Action 3: Configure fstab
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
        // Check if fstab already has the expected entries
        let config_state =
            check_if_fstab_configured(&input.mounted.mount_root, &input.mounted.partitions).await;

        let planned_work = FstabPlan {
            installed: input.clone(),
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
        match plan.config_state {
            FstabState::NeedsConfig => {
                write_fstab(
                    &plan.installed.mounted.mount_root,
                    &plan.installed.mounted.partitions,
                )
                .await?;
            }
            FstabState::AlreadyConfigured => {
                tracing::info!("fstab already configured, skipping");
            }
        }

        Ok(ConfiguredFstab {
            installed: plan.installed.clone(),
        })
    }
}

/// Check if fstab is already configured with expected partition entries
async fn check_if_fstab_configured(
    mount_root: &std::path::Path,
    partitions: &[Partition],
) -> FstabState {
    let fstab_path = mount_root.join("etc/fstab");

    // Try to read existing fstab
    let existing_content = match tokio::fs::read_to_string(&fstab_path).await {
        Ok(content) => content,
        Err(_) => {
            tracing::info!(
                "{} does not exist or is not readable - needs configuration",
                fstab_path.display()
            );
            return FstabState::NeedsConfig;
        }
    };

    // Check if all partitions have entries in fstab
    for partition in partitions {
        let device = partition.device.as_str();
        let mount_point = partition.mount_point.as_str();

        // Simple check: does fstab contain both the device and mount point?
        if !existing_content.contains(device) || !existing_content.contains(mount_point) {
            tracing::info!(
                "fstab missing entry for {} -> {} - needs configuration",
                device,
                mount_point
            );
            return FstabState::NeedsConfig;
        }
    }

    tracing::info!(
        "fstab at {} appears to have all {} partition entries",
        fstab_path.display(),
        partitions.len()
    );
    FstabState::AlreadyConfigured
}

/// Generate and write /etc/fstab
async fn write_fstab(mount_root: &std::path::Path, partitions: &[Partition]) -> Result<()> {
    use crate::provisioning::types::MountPoint;

    let fstab_path = mount_root.join("etc/fstab");

    tracing::info!("Writing fstab to {}", fstab_path.display());

    // Generate fstab content
    let mut content = String::new();
    content.push_str("# /etc/fstab - MDMA system partition table\n");
    content.push_str("# Generated by MDMA Beacon provisioning\n");
    content.push_str("#\n");
    content.push_str("# <device>  <mount point>  <type>  <options>  <dump>  <pass>\n\n");

    for partition in partitions {
        let device = partition.device.as_str();
        let mount_point = partition.mount_point.as_str();
        let fs_type = match partition.filesystem_type() {
            crate::provisioning::types::FilesystemType::Ext4 => "ext4",
            crate::provisioning::types::FilesystemType::Fat32 => "vfat",
        };

        // Root partition gets pass=1, others get pass=2
        let pass = if partition.mount_point == MountPoint::Root {
            1
        } else {
            2
        };

        let line = format!(
            "{}\t{}\t{}\tdefaults\t0\t{}\n",
            device, mount_point, fs_type, pass
        );

        tracing::info!("  {}", line.trim());
        content.push_str(&line);
    }

    // Ensure /etc directory exists
    let etc_dir = mount_root.join("etc");
    tokio::fs::create_dir_all(&etc_dir).await.map_err(|e| {
        crate::error::BeaconError::Provisioning(format!(
            "Failed to create {}: {}",
            etc_dir.display(),
            e
        ))
    })?;

    // Write fstab
    tokio::fs::write(&fstab_path, &content).await.map_err(|e| {
        crate::error::BeaconError::Provisioning(format!(
            "Failed to write {}: {}",
            fstab_path.display(),
            e
        ))
    })?;

    // Verify the write
    let verify_content = tokio::fs::read_to_string(&fstab_path).await.map_err(|e| {
        crate::error::BeaconError::Provisioning(format!(
            "Failed to verify {}: {}",
            fstab_path.display(),
            e
        ))
    })?;

    if verify_content != content {
        return Err(crate::error::BeaconError::Provisioning(
            "fstab verification failed - content mismatch after write".to_string(),
        ));
    }

    tracing::info!("✅ fstab configured with {} entries", partitions.len());
    Ok(())
}

// ============================================================================
// Composite Action: Install System
// ============================================================================

/// Planned work for the composite installation
///
/// Stores just the planned work from each sub-action (not the PlannedActions themselves)
/// since PlannedAction doesn't implement Clone.
///
/// Note: Unmount is handled by stage 6, not here.
#[derive(Debug, Clone, PartialEq)]
pub struct InstallationPlan {
    pub mount_plan: MountPlan,
    pub install_plan: InstallPlan,
    pub configure_plan: FstabPlan,
}

impl std::fmt::Display for InstallationPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "📝 Installation plan with 3 sub-stages:")?;
        writeln!(f, "  1. {}", self.mount_plan)?;
        writeln!(f, "  2. {}", self.install_plan)?;
        writeln!(f, "  3. {}", self.configure_plan)?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct InstallSystemAction;

impl Action<FormattedSystem, InstallationPlan, ConfiguredFstab> for InstallSystemAction {
    fn id(&self) -> ActionId {
        ActionId::new("install-system")
    }

    fn description(&self) -> String {
        "Install base operating system".to_string()
    }

    async fn plan(
        &self,
        input: &FormattedSystem,
    ) -> Result<PlannedAction<FormattedSystem, InstallationPlan, ConfiguredFstab, Self>> {
        tracing::info!("Planning installation with sub-actions...");

        // Plan each sub-action, chaining outputs to inputs
        let mount_planned = MountPartitionsAction.plan(input).await?;
        let install_planned = InstallPackagesAction
            .plan(&mount_planned.assumed_output)
            .await?;
        let configure_planned = ConfigureFstabAction
            .plan(&install_planned.assumed_output)
            .await?;

        // Extract just the planned work (the PlannedActions aren't Clone)
        let installation_plan = InstallationPlan {
            mount_plan: mount_planned.planned_work,
            install_plan: install_planned.planned_work,
            configure_plan: configure_planned.planned_work,
        };

        // Final output - filesystem remains mounted for stage 5
        let final_output = configure_planned.assumed_output;

        Ok(PlannedAction {
            description: self.description(),
            action: self.clone(),
            input: input.clone(),
            planned_work: installation_plan,
            assumed_output: final_output,
        })
    }

    async fn apply(&self, plan: &InstallationPlan) -> Result<ConfiguredFstab> {
        tracing::info!("Executing installation with 3 sub-stages");

        // Execute each sub-action in sequence (recreate actions - they're zero-sized)
        tracing::info!("Stage 1/3: Mount partitions");
        let _mounted = MountPartitionsAction.apply(&plan.mount_plan).await?;

        tracing::info!("Stage 2/3: Install packages");
        let _installed = InstallPackagesAction.apply(&plan.install_plan).await?;

        tracing::info!("Stage 3/3: Configure fstab");
        let final_output = ConfigureFstabAction.apply(&plan.configure_plan).await?;

        tracing::info!("✅ Installation complete (partitions remain mounted for stage 5)");
        Ok(final_output)
    }
}
