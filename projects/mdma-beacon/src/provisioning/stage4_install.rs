// bases/beacon/src/provisioning/stage4_install.rs
//! Stage 4: Install base system
//!
//! This stage is implemented as a composite of sub-actions:
//! 1. Mount partitions (root first, then `/boot` and the rest)
//! 2. Install packages (xbps-install base-system). The boot partition is
//!    already mounted at `<root>/boot`, so the Void kernel/firmware packages
//!    write straight to it; if it is empty (reprovision), `PopulateBootfs`
//!    force re-extracts the packages that own `/boot`.
//! 3. Configure fstab (always regenerated, PARTUUID based)
//!
//! Note: Unmount is handled by stage 6 (finalize) so that stage 5 (configure)
//! can work with the mounted filesystem.
//!
//! Each sub-action checks current state in plan() and only acts if needed.

use crate::actions::{Action, ActionId, PlannedAction};
use crate::error::Result;
use crate::provisioning::stage5_configure::get_partuuid;
use crate::provisioning::types::{
    ConfiguredFstab, FilesystemType, FormattedSystem, FstabPlan, InstallPlan, InstallState,
    InstalledPackages, MountPlan, MountPoint, MountState, MountedPartitions, Partition,
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

        // Collect all partitions from the partition plan, ordered so that parent
        // mounts are always established before child mounts (e.g. "/" before "/boot").
        let mut partitions: Vec<Partition> = match &input.partitioned.plan {
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
        sort_for_mounting(&mut partitions);

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

/// Order partitions so parents are mounted before children: shallower mount
/// paths first ("/" before "/boot" and "/var"). The sort is stable.
fn sort_for_mounting(partitions: &mut [Partition]) {
    partitions.sort_by_key(|p| p.mount_point.as_path().components().count());
}

/// Whether `device` appears as the source of a mount in `mount` output
/// (`<source> on <target> type <fs> (<opts>)`). Exact match on the source
/// field, so `/dev/nvme0n1p1` does not match `/dev/nvme0n1p10`.
fn is_device_mounted(mount_output: &str, device: &str) -> bool {
    mount_output
        .lines()
        .any(|line| line.split_whitespace().next() == Some(device))
}

/// Check if a device is currently mounted
async fn check_if_mounted(device: &str) -> Result<bool> {
    let output = Command::new("mount")
        .output()
        .await
        .map_err(|e| crate::error::BeaconError::command_failed("mount", e))?;

    if !output.status.success() {
        return Err(crate::error::BeaconError::Provisioning(format!(
            "`mount` failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(is_device_mounted(&stdout, device))
}

/// Mount a single partition
async fn mount_partition(mount_root: &std::path::Path, partition: &Partition) -> Result<()> {
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

        // The boot partition is mounted at <root>/boot. On a reprovision the
        // base system may already exist while the boot partition is empty.
        populate_bootfs(&plan.mounted.mount_root).await?;

        Ok(InstalledPackages {
            mounted: plan.mounted.clone(),
            packages: plan.packages.clone(),
        })
    }
}

/// Check if the base system is already installed in the root filesystem.
///
/// The kernel is deliberately not checked here: `/boot` lives on its own
/// partition, which may be empty after a reformat (see [`populate_bootfs`]).
async fn check_if_base_system_installed(mount_root: &std::path::Path) -> InstallState {
    // Check for /usr/bin/xbps-query (from base-system)
    let xbps_query_path = mount_root.join("usr/bin/xbps-query");

    if tokio::fs::try_exists(&xbps_query_path)
        .await
        .unwrap_or(false)
    {
        tracing::info!(
            "Found {} - base system already installed",
            xbps_query_path.display()
        );
        InstallState::AlreadyInstalled
    } else {
        tracing::info!(
            "{} not found - base-system needs to be installed",
            xbps_query_path.display()
        );
        InstallState::NeedsInstall
    }
}

/// Packages known to own files in `/boot` (fallback when xbps-query yields none).
const BOOT_PACKAGES: [&str; 2] = [boot_health::KERNEL_PKG, "rpi-firmware"];

/// Void repositories (aarch64). `nonfree` is required for `rpi-eeprom`.
const REPO_URL: &str = "https://repo-default.voidlinux.org/current/aarch64";
const NONFREE_REPO_URL: &str = "https://repo-default.voidlinux.org/current/aarch64/nonfree";

/// Parse `xbps-query -o '/boot/*'` output into unique package names.
///
/// Each line looks like `rpi-kernel-6.12.20_1: /boot/kernel8.img (regular file)`;
/// the version is stripped from the `pkgver` before the colon. Order of first
/// appearance is preserved.
fn parse_boot_owners(xbps_query_output: &str) -> Vec<String> {
    let mut owners: Vec<String> = Vec::new();
    for line in xbps_query_output.lines() {
        let Some((pkgver, _path)) = line.split_once(':') else {
            continue;
        };
        let Some((name, _version)) = pkgver.trim().rsplit_once('-') else {
            continue;
        };
        if !name.is_empty() && !owners.iter().any(|o| o == name) {
            owners.push(name.to_string());
        }
    }
    owners
}

/// Arguments for force re-extracting `packages` into `mount_root`.
fn force_reinstall_args(mount_root: &Path, packages: &[String]) -> Vec<String> {
    let mut args = vec![
        "-r".to_string(),
        mount_root.display().to_string(),
        "-R".to_string(),
        REPO_URL.to_string(),
        "-R".to_string(),
        NONFREE_REPO_URL.to_string(),
        "-Syf".to_string(),
    ];
    args.extend(packages.iter().cloned());
    args
}

/// Find the packages that own files in `<root>/boot`, falling back to
/// [`BOOT_PACKAGES`] when the query fails or finds nothing.
async fn discover_boot_owners(mount_root: &Path) -> Vec<String> {
    let queried = Command::new("xbps-query")
        .arg("-r")
        .arg(mount_root)
        .args(["-o", "/boot/*"])
        .output()
        .await
        .ok()
        .filter(|o| o.status.success())
        .map(|o| parse_boot_owners(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default();

    if queried.is_empty() {
        BOOT_PACKAGES.iter().map(|s| s.to_string()).collect()
    } else {
        queried
    }
}

/// Sub-step `PopulateBootfs`: make sure the boot partition (mounted at
/// `<root>/boot`) contains the kernel.
///
/// On a reprovision the root may already hold the base system while the boot
/// partition is empty or reformatted; xbps would consider the packages
/// installed, so they are force re-extracted.
async fn populate_bootfs(mount_root: &Path) -> Result<()> {
    let kernel_path = mount_root.join("boot").join(boot_health::KERNEL_IMAGE_NAME);
    let kernel_exists = tokio::fs::try_exists(&kernel_path).await.unwrap_or(false);

    if !kernel_exists {
        let owners = discover_boot_owners(mount_root).await;
        tracing::info!(
            "{} missing - re-extracting boot packages: {}",
            kernel_path.display(),
            owners.join(", ")
        );

        copy_xbps_keys(mount_root).await?;
        let output = Command::new("xbps-install")
            .args(force_reinstall_args(mount_root, &owners))
            .output()
            .await
            .map_err(|e| crate::error::BeaconError::command_failed("xbps-install -f", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(crate::error::BeaconError::Provisioning(format!(
                "Failed to populate boot partition ({}): {}",
                owners.join(" "),
                stderr.trim()
            )));
        }
    }

    if !tokio::fs::try_exists(&kernel_path).await.unwrap_or(false) {
        return Err(crate::error::BeaconError::Provisioning(format!(
            "{} not found after install - boot partition not populated (rpi-kernel missing?)",
            kernel_path.display()
        )));
    }

    tracing::info!("✅ Boot partition populated ({})", kernel_path.display());
    Ok(())
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
    let repo_url = REPO_URL;
    let nonfree_repo_url = NONFREE_REPO_URL;

    tracing::info!(
        "Installing packages to {}: {}",
        mount_root.display(),
        packages.join(", ")
    );
    tracing::info!("Using repositories: {}, {}", repo_url, nonfree_repo_url);

    // Copy xbps repository keys to target root first
    // This prevents the interactive key import prompt
    copy_xbps_keys(mount_root).await?;

    // Build the command
    // xbps-install -Sy -R <repo> -R <nonfree-repo> -r <root> <packages>
    // -S: sync repository index
    // -y: assume yes to all questions
    // -R: repository URL (may be specified multiple times)
    // -r: target root directory
    let mut cmd = Command::new("xbps-install");
    cmd.arg("-Sy")
        .arg("-R")
        .arg(repo_url)
        .arg("-R")
        .arg(nonfree_repo_url)
        .arg("-r")
        .arg(mount_root)
        .args(packages);

    tracing::info!(
        "Executing: xbps-install -Sy -R {} -R {} -r {} {}",
        repo_url,
        nonfree_repo_url,
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

    // The kernel image on the boot partition is verified by `populate_bootfs`.
    tracing::info!("✅ Base system installed successfully");
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
        // fstab is always regenerated
        let planned_work = FstabPlan {
            installed: input.clone(),
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
        write_fstab(
            &plan.installed.mounted.mount_root,
            &plan.installed.mounted.partitions,
        )
        .await?;

        Ok(ConfiguredFstab {
            installed: plan.installed.clone(),
        })
    }
}

/// One fstab line's inputs: a partition identified by PARTUUID.
#[derive(Debug, Clone, PartialEq)]
struct FstabEntry {
    partuuid: String,
    mount_point: MountPoint,
}

/// fstab mount options: data partitions and the boot partition are `nofail`
/// so a missing disk never blocks boot; root and /var must mount.
fn fstab_options(mount_point: MountPoint) -> &'static str {
    if mount_point == MountPoint::Boot || mount_point.is_data() {
        "defaults,nofail"
    } else {
        "defaults"
    }
}

/// fsck pass: root is checked first, everything else second.
fn fstab_pass(mount_point: MountPoint) -> u8 {
    if mount_point == MountPoint::Root {
        1
    } else {
        2
    }
}

/// Render /etc/fstab content from PARTUUID-identified entries (pure).
fn render_fstab(entries: &[FstabEntry]) -> String {
    let mut content = String::new();
    content.push_str("# /etc/fstab - MDMA system partition table\n");
    content.push_str("# Generated by MDMA Beacon provisioning\n");
    content.push_str("#\n");
    content.push_str("# <device>  <mount point>  <type>  <options>  <dump>  <pass>\n\n");

    for entry in entries {
        let fs_type = match entry.mount_point.filesystem_type() {
            FilesystemType::Ext4 => "ext4",
            FilesystemType::Fat32 => "vfat",
        };
        content.push_str(&format!(
            "PARTUUID={}\t{}\t{}\t{}\t0\t{}\n",
            entry.partuuid,
            entry.mount_point.as_str(),
            fs_type,
            fstab_options(entry.mount_point),
            fstab_pass(entry.mount_point)
        ));
    }
    content
}

/// Generate and write /etc/fstab
async fn write_fstab(mount_root: &std::path::Path, partitions: &[Partition]) -> Result<()> {
    let fstab_path = mount_root.join("etc/fstab");

    tracing::info!("Writing fstab to {}", fstab_path.display());

    let mut ordered = partitions.to_vec();
    sort_for_mounting(&mut ordered);

    let mut entries = Vec::with_capacity(ordered.len());
    for partition in &ordered {
        entries.push(FstabEntry {
            partuuid: get_partuuid(partition.device.as_str()).await?,
            mount_point: partition.mount_point,
        });
    }

    let content = render_fstab(&entries);
    for line in content
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        tracing::info!("  {}", line);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provisioning::types::{DevicePath, PartitionSize};

    fn part(mount_point: MountPoint, device: &str) -> Partition {
        Partition {
            device: DevicePath::new(device).expect("device"),
            mount_point,
            size: PartitionSize::from_mb(512),
        }
    }

    fn entry(partuuid: &str, mount_point: MountPoint) -> FstabEntry {
        FstabEntry {
            partuuid: partuuid.to_string(),
            mount_point,
        }
    }

    #[test]
    fn root_is_mounted_before_boot() {
        let mut parts = vec![
            part(MountPoint::Boot, "/dev/nvme0n1p1"),
            part(MountPoint::Music, "/dev/nvme1n1p1"),
            part(MountPoint::Root, "/dev/nvme0n1p2"),
        ];
        sort_for_mounting(&mut parts);
        assert_eq!(parts[0].mount_point, MountPoint::Root);
    }

    #[test]
    fn is_device_mounted_matches_exactly() {
        let out = "/dev/nvme0n1p10 on /mnt/x type ext4 (rw)\n\
                   /dev/nvme0n1p2 on /mnt/mdma-install type ext4 (rw)\n";
        assert!(!is_device_mounted(out, "/dev/nvme0n1p1"));
        assert!(is_device_mounted(out, "/dev/nvme0n1p10"));
        assert!(is_device_mounted(out, "/dev/nvme0n1p2"));
    }

    #[test]
    fn render_fstab_golden() {
        let content = render_fstab(&[
            entry("aaaa-02", MountPoint::Root),
            entry("aaaa-01", MountPoint::Boot),
            entry("aaaa-03", MountPoint::Var),
            entry("aaaa-04", MountPoint::Metadata),
            entry("bbbb-01", MountPoint::Music),
        ]);
        let expected = "\
# /etc/fstab - MDMA system partition table
# Generated by MDMA Beacon provisioning
#
# <device>  <mount point>  <type>  <options>  <dump>  <pass>

PARTUUID=aaaa-02\t/\text4\tdefaults\t0\t1
PARTUUID=aaaa-01\t/boot\tvfat\tdefaults,nofail\t0\t2
PARTUUID=aaaa-03\t/var\text4\tdefaults\t0\t2
PARTUUID=aaaa-04\t/metadata\text4\tdefaults,nofail\t0\t2
PARTUUID=bbbb-01\t/music\text4\tdefaults,nofail\t0\t2
";
        assert_eq!(content, expected);
    }

    #[test]
    fn parse_boot_owners_strips_versions_and_dedupes() {
        let out = "rpi-kernel-6.12.20_1: /boot/kernel8.img (regular file)\n\
                   rpi-kernel-6.12.20_1: /boot/overlays/a.dtbo (regular file)\n\
                   rpi-firmware-20250101_1: /boot/config.txt (regular file)\n\
                   garbage line\n";
        assert_eq!(parse_boot_owners(out), vec!["rpi-kernel", "rpi-firmware"]);
        assert!(parse_boot_owners("").is_empty());
    }

    #[test]
    fn force_reinstall_args_force_extract_into_root() {
        let args = force_reinstall_args(
            Path::new("/mnt/mdma-install"),
            &["rpi-kernel".to_string(), "rpi-firmware".to_string()],
        );
        assert_eq!(&args[..2], ["-r", "/mnt/mdma-install"]);
        assert!(args.contains(&"-Syf".to_string()));
        assert_eq!(&args[args.len() - 2..], ["rpi-kernel", "rpi-firmware"]);
    }
}
