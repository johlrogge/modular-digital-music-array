// bases/beacon/src/provisioning/stage5_configure.rs
//! Stage 5: Configure system
//!
//! This stage configures the installed system:
//! 1. Set hostname
//! 2. Configure networking (DHCP)
//! 3. Create users (admin, mdma)
//! 4. Set up SSH for admin user
//! 5. Configure sshd (disable root login)
//! 6. Install additional packages
//! 7. Enable services (runit)
//! 8. Sync kernel from NVMe to SD card boot partition
//! 9. Configure boot to use NVMe root

use crate::actions::{Action, ActionId, PlannedAction};
use crate::error::{BeaconError, Result};
use crate::provisioning::types::{ConfiguredFstab, ConfiguredSystem};
use std::path::Path;
use tokio::process::Command;

#[derive(Clone, Debug)]
pub struct ConfigureSystemAction;

impl Action<ConfiguredFstab, ConfiguredSystem, ConfiguredSystem> for ConfigureSystemAction {
    fn id(&self) -> ActionId {
        ActionId::new("configure-system")
    }

    fn description(&self) -> String {
        "Configure system (hostname, users, SSH, services, boot)".to_string()
    }

    async fn plan(
        &self,
        input: &ConfiguredFstab,
    ) -> Result<PlannedAction<ConfiguredFstab, ConfiguredSystem, ConfiguredSystem, Self>> {
        let assumed_output = ConfiguredSystem {
            fstab_configured: input.clone(),
        };

        Ok(PlannedAction {
            description: self.description(),
            action: self.clone(),
            input: input.clone(),
            planned_work: assumed_output.clone(),
            assumed_output,
        })
    }

    async fn apply(&self, planned_output: &ConfiguredSystem) -> Result<ConfiguredSystem> {
        tracing::info!("Stage 5: Configure system - executing plan");

        let mount_root = planned_output.mount_root();
        let config = planned_output.config();

        // 0. Update SD card system FIRST (ensures matching kernel versions)
        update_sd_card_system().await?;

        // 1. Set hostname
        configure_hostname(mount_root, config.hostname.as_str()).await?;

        // 2. Configure networking (DHCP)
        configure_networking(mount_root).await?;

        // 3. Create users
        create_users(mount_root).await?;

        // 4. Set up SSH for admin user
        setup_ssh_for_admin(mount_root, config.ssh_key.as_str()).await?;

        // 5. Configure sshd (disable root login)
        configure_sshd(mount_root).await?;

        // 6. Configure MDMA package repository
        configure_mdma_repository(mount_root).await?;

        // 7. Install Void Linux packages
        install_packages(mount_root).await?;

        // 8. Install MDMA packages (beacon, mdma-console, mdma-library)
        install_mdma_packages(mount_root).await?;

        // 9. Enable services
        enable_services(mount_root).await?;

        // 10. Sync kernel from NVMe to SD card boot partition
        // (kept as safety measure even though both should have matching versions)
        sync_kernel_to_sd_boot(mount_root).await?;

        // 11. Configure boot to use NVMe root
        configure_boot().await?;

        tracing::info!("✅ Stage 5: System configuration complete");
        Ok(planned_output.clone())
    }
}

/// Update the SD card (running system) before configuring NVMe
///
/// This ensures both SD and NVMe have matching kernel versions,
/// allowing easy switching between boot sources via cmdline.txt.
async fn update_sd_card_system() -> Result<()> {
    tracing::info!("Updating SD card system packages (for kernel matching)");

    // Sync and update all packages on the running system
    let output = Command::new("xbps-install")
        .args(["-Syu"])
        .output()
        .await
        .map_err(|e| BeaconError::command_failed("xbps-install -Syu", e))?;

    // Log output
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        tracing::info!("  [xbps] {}", line);
    }

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Don't fail if already up to date
        if !stderr.contains("up to date") {
            tracing::warn!("xbps-install -Syu returned non-zero: {}", stderr);
            // Continue anyway - kernel sync will handle mismatches
        }
    }

    tracing::info!("✅ SD card system updated");
    Ok(())
}

/// Set hostname from ProvisionConfig
async fn configure_hostname(mount_root: &Path, hostname: &str) -> Result<()> {
    tracing::info!("Setting hostname to '{}'", hostname);

    let hostname_path = mount_root.join("etc/hostname");
    tokio::fs::write(&hostname_path, format!("{}\n", hostname))
        .await
        .map_err(|e| {
            BeaconError::Provisioning(format!(
                "Failed to write {}: {}",
                hostname_path.display(),
                e
            ))
        })?;

    tracing::info!("✅ Hostname configured");
    Ok(())
}

/// Configure networking with dhcpcd
async fn configure_networking(mount_root: &Path) -> Result<()> {
    tracing::info!("Configuring networking (DHCP)");

    // For Void Linux, dhcpcd service will be enabled via symlink
    // The default dhcpcd.conf should work for basic DHCP on all interfaces

    // Ensure /etc/dhcpcd.conf exists with sensible defaults
    let dhcpcd_conf = mount_root.join("etc/dhcpcd.conf");
    if !dhcpcd_conf.exists() {
        let content = r#"# MDMA dhcpcd configuration
# Use DHCP on all interfaces
hostname
clientid
persistent
option rapid_commit
option domain_name_servers, domain_name, domain_search, host_name
option classless_static_routes
option interface_mtu
require dhcp_server_identifier
slaac private
"#;
        tokio::fs::write(&dhcpcd_conf, content).await.map_err(|e| {
            BeaconError::Provisioning(format!("Failed to write {}: {}", dhcpcd_conf.display(), e))
        })?;
    }

    tracing::info!("✅ Networking configured");
    Ok(())
}

/// Create admin and mdma users
async fn create_users(mount_root: &Path) -> Result<()> {
    tracing::info!("Creating users");

    // Create admin user with wheel group (for sudo)
    tracing::info!("  Creating admin user...");
    let output = Command::new("chroot")
        .arg(mount_root)
        .args(["useradd", "-m", "-G", "wheel", "-s", "/bin/bash", "admin"])
        .output()
        .await
        .map_err(|e| BeaconError::command_failed("chroot useradd admin", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Ignore "user already exists" error
        if !stderr.contains("already exists") {
            return Err(BeaconError::Provisioning(format!(
                "Failed to create admin user: {}",
                stderr
            )));
        }
        tracing::info!("  admin user already exists");
    } else {
        tracing::info!("  ✅ admin user created");
    }

    // Create mdma user with wheel, audio, video groups
    tracing::info!("  Creating mdma user...");
    let output = Command::new("chroot")
        .arg(mount_root)
        .args([
            "useradd",
            "-m",
            "-G",
            "wheel,audio,video",
            "-s",
            "/bin/bash",
            "mdma",
        ])
        .output()
        .await
        .map_err(|e| BeaconError::command_failed("chroot useradd mdma", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("already exists") {
            return Err(BeaconError::Provisioning(format!(
                "Failed to create mdma user: {}",
                stderr
            )));
        }
        tracing::info!("  mdma user already exists");
    } else {
        tracing::info!("  ✅ mdma user created");
    }

    // Enable passwordless sudo for wheel group
    // Admin user has no password (SSH key only), so NOPASSWD is required
    let sudoers_path = mount_root.join("etc/sudoers");
    if sudoers_path.exists() {
        let content = tokio::fs::read_to_string(&sudoers_path)
            .await
            .map_err(|e| {
                BeaconError::Provisioning(format!(
                    "Failed to read {}: {}",
                    sudoers_path.display(),
                    e
                ))
            })?;

        // Enable wheel group for passwordless sudo
        // Replace the commented NOPASSWD line, or the regular wheel line
        let new_content = content
            .replace(
                "# %wheel ALL=(ALL:ALL) NOPASSWD: ALL",
                "%wheel ALL=(ALL:ALL) NOPASSWD: ALL",
            )
            .replace(
                "# %wheel ALL=(ALL:ALL) ALL",
                "%wheel ALL=(ALL:ALL) NOPASSWD: ALL",
            );
        if new_content != content {
            tokio::fs::write(&sudoers_path, new_content)
                .await
                .map_err(|e| {
                    BeaconError::Provisioning(format!(
                        "Failed to write {}: {}",
                        sudoers_path.display(),
                        e
                    ))
                })?;
            tracing::info!("  ✅ passwordless sudo enabled for wheel group");
        }
    }

    tracing::info!("✅ Users created");
    Ok(())
}

/// Set up SSH authorized_keys for admin user
async fn setup_ssh_for_admin(mount_root: &Path, ssh_key: &str) -> Result<()> {
    tracing::info!("Setting up SSH for admin user");

    let ssh_dir = mount_root.join("home/admin/.ssh");
    tokio::fs::create_dir_all(&ssh_dir).await.map_err(|e| {
        BeaconError::Provisioning(format!("Failed to create {}: {}", ssh_dir.display(), e))
    })?;

    // Write authorized_keys
    let auth_keys_path = ssh_dir.join("authorized_keys");
    tokio::fs::write(&auth_keys_path, format!("{}\n", ssh_key))
        .await
        .map_err(|e| {
            BeaconError::Provisioning(format!(
                "Failed to write {}: {}",
                auth_keys_path.display(),
                e
            ))
        })?;

    // Set ownership (chown admin:admin)
    let output = Command::new("chroot")
        .arg(mount_root)
        .args(["chown", "-R", "admin:admin", "/home/admin/.ssh"])
        .output()
        .await
        .map_err(|e| BeaconError::command_failed("chroot chown", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BeaconError::Provisioning(format!(
            "Failed to chown .ssh: {}",
            stderr
        )));
    }

    // Set permissions (chmod 700 .ssh, chmod 600 authorized_keys)
    let output = Command::new("chroot")
        .arg(mount_root)
        .args(["chmod", "700", "/home/admin/.ssh"])
        .output()
        .await
        .map_err(|e| BeaconError::command_failed("chroot chmod .ssh", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BeaconError::Provisioning(format!(
            "Failed to chmod .ssh: {}",
            stderr
        )));
    }

    let output = Command::new("chroot")
        .arg(mount_root)
        .args(["chmod", "600", "/home/admin/.ssh/authorized_keys"])
        .output()
        .await
        .map_err(|e| BeaconError::command_failed("chroot chmod authorized_keys", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BeaconError::Provisioning(format!(
            "Failed to chmod authorized_keys: {}",
            stderr
        )));
    }

    tracing::info!("✅ SSH configured for admin user");
    Ok(())
}

/// Configure sshd to disable root login
async fn configure_sshd(mount_root: &Path) -> Result<()> {
    tracing::info!("Configuring sshd (disable root login)");

    let sshd_config_path = mount_root.join("etc/ssh/sshd_config");

    // Read existing config or create new
    let content = if sshd_config_path.exists() {
        tokio::fs::read_to_string(&sshd_config_path)
            .await
            .map_err(|e| {
                BeaconError::Provisioning(format!(
                    "Failed to read {}: {}",
                    sshd_config_path.display(),
                    e
                ))
            })?
    } else {
        String::new()
    };

    // Check if PermitRootLogin is already configured
    let mut new_content = content.clone();
    if content.contains("PermitRootLogin") {
        // Replace existing setting
        let lines: Vec<&str> = content.lines().collect();
        let mut new_lines: Vec<String> = Vec::new();
        for line in lines {
            if line.trim().starts_with("PermitRootLogin")
                || line.trim().starts_with("#PermitRootLogin")
            {
                new_lines.push("PermitRootLogin no".to_string());
            } else {
                new_lines.push(line.to_string());
            }
        }
        new_content = new_lines.join("\n") + "\n";
    } else {
        // Append setting
        new_content.push_str("\n# MDMA: Disable root SSH login\nPermitRootLogin no\n");
    }

    if new_content != content {
        tokio::fs::write(&sshd_config_path, new_content)
            .await
            .map_err(|e| {
                BeaconError::Provisioning(format!(
                    "Failed to write {}: {}",
                    sshd_config_path.display(),
                    e
                ))
            })?;
    }

    tracing::info!("✅ sshd configured (root login disabled)");
    Ok(())
}

/// Configure MDMA package repository
///
/// Adds the MDMA GitHub Pages repository to xbps.d so we can install
/// beacon, mdma-console, mdma-library packages.
async fn configure_mdma_repository(mount_root: &Path) -> Result<()> {
    tracing::info!("Configuring MDMA package repository");

    // Create xbps.d directory if needed
    let xbps_d = mount_root.join("etc/xbps.d");
    tokio::fs::create_dir_all(&xbps_d).await.map_err(|e| {
        BeaconError::Provisioning(format!("Failed to create {}: {}", xbps_d.display(), e))
    })?;

    // Write repository configuration
    let repo_conf = xbps_d.join("10-mdma.conf");
    let repo_url = "repository=https://johlrogge.github.io/modular-digital-music-array/aarch64";

    tokio::fs::write(&repo_conf, format!("{}\n", repo_url))
        .await
        .map_err(|e| {
            BeaconError::Provisioning(format!("Failed to write {}: {}", repo_conf.display(), e))
        })?;

    tracing::info!("  Added MDMA repository: {}", repo_url);
    tracing::info!("✅ MDMA repository configured");
    Ok(())
}

/// Install additional packages (Void Linux base packages)
async fn install_packages(mount_root: &Path) -> Result<()> {
    tracing::info!("Installing additional Void Linux packages");

    let packages = [
        "openssh",
        "dhcpcd",
        "dbus",
        "avahi",
        "nss-mdns",
        "sudo",
        "void-repo-nonfree",
    ];

    tracing::info!("  Packages: {}", packages.join(", "));

    let output = Command::new("xbps-install")
        .args(["-Sy", "-r"])
        .arg(mount_root)
        .args(&packages)
        .output()
        .await
        .map_err(|e| BeaconError::command_failed("xbps-install", e))?;

    // Log output
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        tracing::info!("  [xbps] {}", line);
    }

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::error!("xbps-install failed: {}", stderr);
        return Err(BeaconError::Provisioning(format!(
            "xbps-install failed: {}",
            stderr
        )));
    }

    tracing::info!("✅ Void Linux packages installed");
    Ok(())
}

/// Install MDMA packages from MDMA repository
///
/// Installs beacon, mdma-console, and mdma-library services.
async fn install_mdma_packages(mount_root: &Path) -> Result<()> {
    tracing::info!("Installing MDMA packages from MDMA repository");

    let packages = [
        "beacon",
        "mdma-console",
        "mdma-library",
        "mdma-playback",
        "mdma-gateway",
        "mdma-bandcamp",
    ];

    tracing::info!("  Packages: {}", packages.join(", "));

    // First sync the repository index to pick up the new MDMA repo
    let output = Command::new("xbps-install")
        .args(["-Sy", "-r"])
        .arg(mount_root)
        .args(&packages)
        .output()
        .await
        .map_err(|e| BeaconError::command_failed("xbps-install mdma packages", e))?;

    // Log output
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        tracing::info!("  [xbps] {}", line);
    }

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Don't fail provisioning if MDMA packages aren't available yet
        // This allows provisioning to work before packages are published
        tracing::warn!(
            "MDMA packages not installed (may not be published yet): {}",
            stderr
        );
        tracing::warn!("You can install them manually later with: xbps-install beacon mdma-console mdma-library mdma-playback mdma-gateway mdma-bandcamp");
        return Ok(());
    }

    tracing::info!("✅ MDMA packages installed");
    Ok(())
}

/// Enable runit services via symlinks
async fn enable_services(mount_root: &Path) -> Result<()> {
    tracing::info!("Enabling services (runit)");

    let services = [
        "sshd",
        "dhcpcd",
        "dbus",
        "avahi-daemon",
        "mdma-console",
        "mdma-library",
        "mdma-playback",
        "mdma-gateway",
        "mdma-bandcamp",
    ];

    // Create log directories for MDMA services
    for log_dir in [
        "mdma-console",
        "mdma-library",
        "mdma-playback",
        "mdma-gateway",
        "mdma-bandcamp",
    ] {
        let log_path = mount_root.join(format!("var/log/{}", log_dir));
        tokio::fs::create_dir_all(&log_path).await.map_err(|e| {
            BeaconError::Provisioning(format!("Failed to create {}: {}", log_path.display(), e))
        })?;
        tracing::info!("  Created log directory: /var/log/{}", log_dir);
    }

    for service in &services {
        tracing::info!("  Enabling {}...", service);

        // Create symlink: ln -s /etc/sv/<service> /etc/runit/runsvdir/default/
        let output = Command::new("chroot")
            .arg(mount_root)
            .args([
                "ln",
                "-sf",
                &format!("/etc/sv/{}", service),
                "/etc/runit/runsvdir/default/",
            ])
            .output()
            .await
            .map_err(|e| BeaconError::command_failed(&format!("ln -s {} service", service), e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Don't fail if symlink already exists
            if !stderr.contains("File exists") {
                return Err(BeaconError::Provisioning(format!(
                    "Failed to enable {}: {}",
                    service, stderr
                )));
            }
        }

        tracing::info!("  ✅ {} enabled", service);
    }

    tracing::info!("✅ Services enabled");
    Ok(())
}

/// Sync kernel and device tree files from NVMe to SD card boot partition
///
/// The Raspberry Pi bootloader loads the kernel from the SD card's /boot partition.
/// After installing rpi-kernel on NVMe, we need to copy the kernel and DTB files
/// to the SD card so the bootloader loads the correct kernel that matches the
/// NVMe's modules.
async fn sync_kernel_to_sd_boot(nvme_mount_root: &Path) -> Result<()> {
    tracing::info!("Syncing kernel from NVMe to SD card boot partition");

    let nvme_boot = nvme_mount_root.join("boot");
    let sd_boot = Path::new("/boot");

    // Verify NVMe boot has kernel
    let nvme_kernel = nvme_boot.join("kernel8.img");
    if !nvme_kernel.exists() {
        return Err(BeaconError::Provisioning(format!(
            "NVMe kernel not found at {}",
            nvme_kernel.display()
        )));
    }

    // Backup current SD card kernel
    let sd_kernel = sd_boot.join("kernel8.img");
    let sd_kernel_backup = sd_boot.join("kernel8.img.sd-original");
    if sd_kernel.exists() && !sd_kernel_backup.exists() {
        tracing::info!("  Backing up SD kernel to kernel8.img.sd-original");
        tokio::fs::copy(&sd_kernel, &sd_kernel_backup)
            .await
            .map_err(|e| BeaconError::Provisioning(format!("Failed to backup SD kernel: {}", e)))?;
    }

    // Copy kernel
    tracing::info!("  Copying kernel8.img");
    tokio::fs::copy(&nvme_kernel, &sd_kernel)
        .await
        .map_err(|e| BeaconError::Provisioning(format!("Failed to copy kernel: {}", e)))?;

    // Copy all DTB files (device tree blobs)
    tracing::info!("  Copying device tree files (*.dtb)");
    let mut dtb_count = 0;
    let mut entries = tokio::fs::read_dir(&nvme_boot)
        .await
        .map_err(|e| BeaconError::Provisioning(format!("Failed to read NVMe boot dir: {}", e)))?;

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| BeaconError::Provisioning(format!("Failed to read dir entry: {}", e)))?
    {
        let path = entry.path();
        if let Some(ext) = path.extension() {
            if ext == "dtb" {
                let filename = path.file_name().unwrap();
                let dest = sd_boot.join(filename);
                tokio::fs::copy(&path, &dest).await.map_err(|e| {
                    BeaconError::Provisioning(format!("Failed to copy {}: {}", path.display(), e))
                })?;
                dtb_count += 1;
            }
        }
    }

    tracing::info!("  Copied {} device tree files", dtb_count);
    tracing::info!("✅ Kernel synced to SD card boot partition");
    Ok(())
}

/// Configure SD card boot to use NVMe root
async fn configure_boot() -> Result<()> {
    tracing::info!("Configuring boot to use NVMe root");

    // The cmdline.txt is on the SD card (current boot device)
    // Location varies by distro:
    // - Void Linux: /boot/cmdline.txt
    // - Raspberry Pi OS: /boot/firmware/cmdline.txt
    let cmdline_path = if Path::new("/boot/cmdline.txt").exists() {
        Path::new("/boot/cmdline.txt")
    } else if Path::new("/boot/firmware/cmdline.txt").exists() {
        Path::new("/boot/firmware/cmdline.txt")
    } else {
        tracing::warn!("cmdline.txt not found - boot configuration skipped");
        tracing::warn!("You may need to manually configure boot parameters");
        return Ok(());
    };

    // Read current cmdline
    let current = if cmdline_path.exists() {
        tokio::fs::read_to_string(cmdline_path).await.map_err(|e| {
            BeaconError::Provisioning(format!("Failed to read {}: {}", cmdline_path.display(), e))
        })?
    } else {
        String::new()
    };

    tracing::info!("  Current cmdline: {}", current.trim());

    // Build new cmdline with NVMe root
    // Keep console settings, update root= to NVMe
    let new_cmdline =
        "console=serial0,115200 console=tty1 root=/dev/nvme0n1p1 rootfstype=ext4 rootwait\n";

    if current.trim() != new_cmdline.trim() {
        // Backup original
        let backup_path = cmdline_path.with_extension("txt.bak");
        if !backup_path.exists() {
            tokio::fs::write(backup_path, &current).await.map_err(|e| {
                BeaconError::Provisioning(format!(
                    "Failed to backup {}: {}",
                    cmdline_path.display(),
                    e
                ))
            })?;
            tracing::info!("  Backed up original cmdline.txt");
        }

        // Write new cmdline
        tokio::fs::write(cmdline_path, new_cmdline)
            .await
            .map_err(|e| {
                BeaconError::Provisioning(format!(
                    "Failed to write {}: {}",
                    cmdline_path.display(),
                    e
                ))
            })?;

        tracing::info!("  New cmdline: {}", new_cmdline.trim());
    } else {
        tracing::info!("  cmdline.txt already configured for NVMe boot");
    }

    tracing::info!("✅ Boot configured for NVMe root");
    Ok(())
}
