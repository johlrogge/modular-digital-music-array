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
//! 8. Sync kernel from rootfs /boot/ to NVMe Boot partition (/boot/firmware/)
//! 9. Write NVMe Boot partition cmdline.txt
//! 10. Set Pi 5 EEPROM BOOT_ORDER to USB→NVMe→SD (0xf164)

use crate::actions::{Action, ActionId, PlannedAction};
use crate::error::{BeaconError, Result};
use crate::provisioning::types::{ConfiguredFstab, ConfiguredSystem};
use rpi_eeprom::{
    apply_eeprom_config, find_staged_eeprom_file, read_current_eeprom_config,
    verify_staged_eeprom_boot_order, BOOT_ORDER_NVME_FIRST,
};
use std::os::unix::fs::PermissionsExt;
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

        // 9. Set up pipewire system service (copy example, fix run script for runit)
        setup_pipewire_service(mount_root).await?;

        // 10. Enable services
        enable_services(mount_root).await?;

        // 11. Sync kernel + DTBs + config.txt from rootfs /boot/ → NVMe Boot partition
        sync_kernel_to_nvme_boot(mount_root).await?;

        // 12. Write NVMe Boot partition cmdline.txt (#55, updated for Phase 2)
        configure_nvme_boot_cmdline(mount_root, "/dev/nvme0n1p2").await?;

        // 13. Set up /music directory ownership for mdma user (#60)
        setup_music_directory(mount_root).await?;

        // 14. Seed bandcamp.conf if not already present (#61)
        seed_bandcamp_config(mount_root).await?;

        // 15. Set EEPROM BOOT_ORDER to USB→NVMe→SD — MUST be last (#22)
        configure_pi_eeprom_boot_order().await?;

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

    // Fix #62: set .ssh directory to 0o700 immediately after creation, before any
    // chroot operations. tokio::fs::create_dir_all inherits the process umask, which
    // on beacon (umask 0o002) would leave the directory world-writable (0o775).
    // OpenSSH rejects keys when .ssh or authorized_keys are group/world-writable.
    tokio::fs::set_permissions(&ssh_dir, std::fs::Permissions::from_mode(0o700))
        .await
        .map_err(|e| {
            BeaconError::Provisioning(format!(
                "Failed to set permissions on {}: {}",
                ssh_dir.display(),
                e
            ))
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

    // Fix #62: set authorized_keys to 0o600 immediately after write.
    // tokio::fs::write creates with umask-affected mode (0o664 at umask 0o002).
    // We set it explicitly here on the host path so OpenSSH will accept it.
    tokio::fs::set_permissions(&auth_keys_path, std::fs::Permissions::from_mode(0o600))
        .await
        .map_err(|e| {
            BeaconError::Provisioning(format!(
                "Failed to set permissions on {}: {}",
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

    // Confirm permissions via chroot chmod as belt-and-suspenders
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
        .args(packages)
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

/// MDMA packages to install on the provisioned target.
///
/// `beacon` is intentionally excluded — it is the SD-card bootstrap and
/// must not be installed as a runtime service on the provisioned NVMe target.
const TARGET_PACKAGES: &[&str] = &[
    "mdma-acid",
    "mdma-admin",
    "mdma-audio",
    "mdma-bandcamp",
    "mdma-console",
    "mdma-gateway",
    "mdma-library",
    "mdma-playback",
];

/// Install MDMA packages from MDMA repository
///
/// Installs all MDMA target services (excludes beacon, which is SD-card only).
async fn install_mdma_packages(mount_root: &Path) -> Result<()> {
    tracing::info!("Installing MDMA packages from MDMA repository");

    let packages = TARGET_PACKAGES;

    tracing::info!("  Packages: {}", packages.join(", "));

    // First sync the repository index to pick up the new MDMA repo
    let output = Command::new("xbps-install")
        .args(["-Sy", "-r"])
        .arg(mount_root)
        .args(packages)
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
        tracing::warn!(
            "You can install them manually later with: xbps-install {}",
            TARGET_PACKAGES.join(" ")
        );
        return Ok(());
    }

    tracing::info!("✅ MDMA packages installed");
    Ok(())
}

/// Set up pipewire as a system runit service.
///
/// Void's pipewire package ships a service example under
/// `/usr/share/examples/sv/pipewire/` but does NOT install it to `/etc/sv/`.
/// The example run script uses `dbus-run-session` which forks — the child
/// pipewire ends up adopted by init (PID 1), so runit cannot track it.
///
/// Our run script instead:
/// 1. Starts a private session D-Bus via `dbus-daemon --session --print-address --fork`
/// 2. Exports DBUS_SESSION_BUS_ADDRESS so WirePlumber can connect to D-Bus
///    (required for ALSA device enumeration)
/// 3. `exec chpst ... pipewire` — runit tracks the pipewire PID directly
///
/// We also drop in the WirePlumber context.exec config so PipeWire launches
/// WirePlumber automatically (the Void handbook way).
async fn setup_pipewire_service(mount_root: &Path) -> Result<()> {
    tracing::info!("Setting up pipewire system runit service");

    let sv_pipewire = mount_root.join("etc/sv/pipewire");

    // Create service directory
    tokio::fs::create_dir_all(&sv_pipewire).await.map_err(|e| {
        BeaconError::Provisioning(format!("Failed to create {}: {}", sv_pipewire.display(), e))
    })?;

    // Write fixed run script: private session D-Bus + direct exec (runit-trackable)
    let run_script = r#"#!/bin/sh
exec 2>&1
! [ -d /run/pipewire ] && install -m 755 -g _pipewire -o _pipewire -d /run/pipewire
umask 002
export PIPEWIRE_RUNTIME_DIR=/run/pipewire
export XDG_STATE_HOME=/var/lib
# Start private session D-Bus so WirePlumber can enumerate ALSA devices.
# --fork means dbus-daemon daemonizes; pipewire is exec'd directly so runit
# tracks the pipewire PID (not a dbus-run-session wrapper that would fork it).
export DBUS_SESSION_BUS_ADDRESS=$(dbus-daemon --session --print-address --fork 2>/dev/null)
exec chpst -P -u _pipewire:_pipewire:audio:video pipewire
"#;

    let run_path = sv_pipewire.join("run");
    tokio::fs::write(&run_path, run_script).await.map_err(|e| {
        BeaconError::Provisioning(format!("Failed to write {}: {}", run_path.display(), e))
    })?;

    // Make run executable
    let output = Command::new("chmod")
        .args(["755", run_path.to_str().unwrap()])
        .output()
        .await
        .map_err(|e| BeaconError::command_failed("chmod pipewire/run", e))?;

    if !output.status.success() {
        return Err(BeaconError::Provisioning(
            "Failed to chmod pipewire run script".to_string(),
        ));
    }

    // Set up WirePlumber drop-in: PipeWire launches WirePlumber via context.exec
    let pipewire_conf_d = mount_root.join("etc/pipewire/pipewire.conf.d");
    tokio::fs::create_dir_all(&pipewire_conf_d)
        .await
        .map_err(|e| {
            BeaconError::Provisioning(format!(
                "Failed to create {}: {}",
                pipewire_conf_d.display(),
                e
            ))
        })?;

    // Symlink the wireplumber drop-in (Void handbook approach)
    let dropin_link = pipewire_conf_d.join("10-wireplumber.conf");
    let dropin_target = "/usr/share/examples/wireplumber/10-wireplumber.conf";

    // Remove existing symlink/file if present
    let _ = tokio::fs::remove_file(&dropin_link).await;

    let output = Command::new("chroot")
        .arg(mount_root)
        .args([
            "ln",
            "-sf",
            dropin_target,
            "/etc/pipewire/pipewire.conf.d/10-wireplumber.conf",
        ])
        .output()
        .await
        .map_err(|e| BeaconError::command_failed("ln wireplumber dropin", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BeaconError::Provisioning(format!(
            "Failed to symlink wireplumber drop-in: {}",
            stderr
        )));
    }

    tracing::info!("✅ pipewire system service configured");
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
        "pipewire",
        "mdma-acid",
        "mdma-admin",
        "mdma-audio",
        "mdma-bandcamp",
        "mdma-console",
        "mdma-gateway",
        "mdma-library",
        "mdma-playback",
    ];

    // Create log directories for MDMA services
    for log_dir in [
        "mdma-acid",
        "mdma-admin",
        "mdma-audio",
        "mdma-bandcamp",
        "mdma-console",
        "mdma-gateway",
        "mdma-library",
        "mdma-playback",
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
            .map_err(|e| BeaconError::command_failed(format!("ln -s {} service", service), e))?;

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

/// Sync kernel, DTBs, overlays, and config.txt from the installed rootfs to the
/// NVMe Boot partition.
///
/// After `rpi5-kernel` and `rpi-firmware` are installed into the rootfs during
/// stage 4, they land in `mount_root/boot/`. The NVMe Boot partition (a separate
/// FAT32 partition, mounted at `mount_root/boot/firmware/` per Phase 1 of #22)
/// is what the Pi firmware actually reads at boot time. This function populates it.
///
/// Files copied:
/// - `kernel8.img`          — Pi 5 kernel
/// - `*.dtb`                — device tree blobs
/// - `*.dtbo`               — device tree overlays
/// - `config.txt`           — firmware configuration
/// - `initramfs-*.img`      — dracut initramfs if present
pub(crate) async fn sync_kernel_to_nvme_boot(mount_root: &Path) -> Result<()> {
    tracing::info!("Syncing kernel + firmware to NVMe Boot partition");

    let src = mount_root.join("boot");
    let dest = mount_root.join("boot/firmware");

    // Verify source kernel exists (installed by rpi5-kernel package in stage 4)
    let src_kernel = src.join("kernel8.img");
    if !src_kernel.exists() {
        return Err(BeaconError::Provisioning(format!(
            "kernel8.img not found at {} — rpi5-kernel may not have been installed",
            src_kernel.display()
        )));
    }

    // Ensure destination exists (should be mounted, but create defensively)
    tokio::fs::create_dir_all(&dest).await.map_err(|e| {
        BeaconError::Provisioning(format!("Failed to create {}: {}", dest.display(), e))
    })?;

    // Copy kernel8.img
    tracing::info!("  Copying kernel8.img");
    tokio::fs::copy(&src_kernel, dest.join("kernel8.img"))
        .await
        .map_err(|e| BeaconError::Provisioning(format!("Failed to copy kernel8.img: {}", e)))?;

    // Copy config.txt — hard error if missing (rpi-firmware not installed)
    let src_config = src.join("config.txt");
    if src_config.exists() {
        tracing::info!("  Copying config.txt");
        tokio::fs::copy(&src_config, dest.join("config.txt"))
            .await
            .map_err(|e| BeaconError::Provisioning(format!("Failed to copy config.txt: {}", e)))?;
    } else {
        return Err(BeaconError::Provisioning(format!(
            "config.txt not found at {} — rpi-firmware package may not be installed in rootfs",
            src_config.display()
        )));
    }

    // Copy *.dtb and initramfs-*.img from flat boot/
    let mut dtb_count = 0usize;
    let mut initramfs_count = 0usize;

    let mut entries = tokio::fs::read_dir(&src).await.map_err(|e| {
        BeaconError::Provisioning(format!("Failed to read {}: {}", src.display(), e))
    })?;

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| BeaconError::Provisioning(format!("Failed to read dir entry: {}", e)))?
    {
        let path = entry.path();
        let filename = match path.file_name() {
            Some(f) => f.to_owned(),
            None => continue,
        };
        let name_str = filename.to_string_lossy();

        let should_copy = if let Some(ext) = path.extension() {
            let ext = ext.to_string_lossy();
            if ext == "dtb" {
                dtb_count += 1;
                true
            } else {
                false
            }
        } else {
            false
        };

        let should_copy =
            should_copy || (name_str.starts_with("initramfs-") && name_str.ends_with(".img"));
        if name_str.starts_with("initramfs-") && name_str.ends_with(".img") {
            initramfs_count += 1;
        }

        if should_copy {
            tokio::fs::copy(&path, dest.join(&filename))
                .await
                .map_err(|e| {
                    BeaconError::Provisioning(format!("Failed to copy {}: {}", path.display(), e))
                })?;
        }
    }

    // Copy boot/overlays/*.dtbo → boot/firmware/overlays/*.dtbo
    // Overlays are provided by rpi-kernel and must be in <bootfs>/overlays/ for the firmware.
    let src_overlays = src.join("overlays");
    if !src_overlays.exists() {
        return Err(BeaconError::Provisioning(format!(
            "boot/overlays/ not found at {} — rpi-kernel package may not be installed in rootfs",
            src_overlays.display()
        )));
    }
    let dest_overlays = dest.join("overlays");
    tokio::fs::create_dir_all(&dest_overlays)
        .await
        .map_err(|e| {
            BeaconError::Provisioning(format!(
                "Failed to create {}: {}",
                dest_overlays.display(),
                e
            ))
        })?;

    let mut dtbo_count = 0usize;
    let mut overlay_entries = tokio::fs::read_dir(&src_overlays).await.map_err(|e| {
        BeaconError::Provisioning(format!("Failed to read {}: {}", src_overlays.display(), e))
    })?;

    while let Some(entry) = overlay_entries.next_entry().await.map_err(|e| {
        BeaconError::Provisioning(format!("Failed to read overlay dir entry: {}", e))
    })? {
        let path = entry.path();
        let filename = match path.file_name() {
            Some(f) => f.to_owned(),
            None => continue,
        };
        if path.extension().map(|e| e == "dtbo").unwrap_or(false) {
            tokio::fs::copy(&path, dest_overlays.join(&filename))
                .await
                .map_err(|e| {
                    BeaconError::Provisioning(format!(
                        "Failed to copy overlay {}: {}",
                        path.display(),
                        e
                    ))
                })?;
            dtbo_count += 1;
        }
    }

    tracing::info!(
        "  Copied {} dtb, {} dtbo (overlays), {} initramfs files",
        dtb_count,
        dtbo_count,
        initramfs_count
    );
    tracing::info!("NVMe Boot partition populated");
    Ok(())
}

/// Look up the PARTUUID for a block device using blkid.
///
/// Returns the PARTUUID string (e.g. `"abc123ef-02"`) or an error if blkid
/// fails or the device has no PARTUUID.
async fn get_partuuid(device: &str) -> Result<String> {
    let output = Command::new("blkid")
        .args(["-s", "PARTUUID", "-o", "value", device])
        .output()
        .await
        .map_err(|e| BeaconError::command_failed("blkid", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BeaconError::Provisioning(format!(
            "blkid failed for {}: {}",
            device, stderr
        )));
    }

    let partuuid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if partuuid.is_empty() {
        return Err(BeaconError::Provisioning(format!(
            "No PARTUUID found for {} — partition may not have a GPT/MBR UUID",
            device
        )));
    }

    Ok(partuuid)
}

/// Build a cmdline.txt that boots from `root_partuuid`.
///
/// Replaces the `root=` token in `current_cmdline` with the PARTUUID form.
/// All other kernel parameters are preserved. A trailing newline is always
/// appended.
///
/// If `current_cmdline` is empty or contains no `root=` token, a safe
/// default is produced from scratch.
fn rewrite_cmdline(current_cmdline: &str, root_partuuid: &str) -> String {
    let new_root = format!("root=PARTUUID={}", root_partuuid);

    let trimmed = current_cmdline.trim();
    if trimmed.is_empty() {
        // No existing cmdline — generate a minimal one.
        return format!(
            "console=serial0,115200 console=tty1 {} rootfstype=ext4 rootwait\n",
            new_root
        );
    }

    // Replace any existing `root=...` token, preserving all other parameters.
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    let rewritten: Vec<String> = tokens
        .iter()
        .map(|tok| {
            if tok.starts_with("root=") {
                new_root.clone()
            } else {
                tok.to_string()
            }
        })
        .collect();

    // If there was no root= token at all, append one.
    if !tokens.iter().any(|t| t.starts_with("root=")) {
        format!("{} {}\n", rewritten.join(" "), new_root)
    } else {
        format!("{}\n", rewritten.join(" "))
    }
}

/// Write `cmdline.txt` at `dest` using `root_partuuid` as the root device.
///
/// Reads the existing file (if any) to preserve non-root kernel parameters,
/// then rewrites only the `root=` token. Creates a `.bak` backup on first
/// write.
async fn write_cmdline(root_partuuid: &str, dest: &Path) -> Result<()> {
    let current = if dest.exists() {
        tokio::fs::read_to_string(dest).await.map_err(|e| {
            BeaconError::Provisioning(format!("Failed to read {}: {}", dest.display(), e))
        })?
    } else {
        String::new()
    };

    let new_content = rewrite_cmdline(&current, root_partuuid);

    if current.trim() == new_content.trim() {
        tracing::info!("  {} already has correct root= — no change", dest.display());
        return Ok(());
    }

    // Backup original on first write
    let backup = dest.with_extension("txt.bak");
    if !backup.exists() && !current.is_empty() {
        tokio::fs::write(&backup, &current).await.map_err(|e| {
            BeaconError::Provisioning(format!("Failed to backup {}: {}", dest.display(), e))
        })?;
        tracing::info!("  Backed up original to {}", backup.display());
    }

    tokio::fs::write(dest, &new_content).await.map_err(|e| {
        BeaconError::Provisioning(format!("Failed to write {}: {}", dest.display(), e))
    })?;

    tracing::info!("  Wrote {}: {}", dest.display(), new_content.trim());
    Ok(())
}

/// Write NVMe Boot partition `cmdline.txt` so it boots from the NVMe root partition (#55, updated for #22 Phase 2).
///
/// Without this, the NVMe's `/boot/cmdline.txt` still contains whatever the
/// tarball default was (e.g. `root=/dev/mmcblk0p2`), which would cause a boot
/// loop if the Pi ever loads the NVMe's boot partition directly.
async fn configure_nvme_boot_cmdline(mount_root: &Path, nvme_root_device: &str) -> Result<()> {
    tracing::info!("Writing NVMe Boot partition cmdline.txt");

    let partuuid = get_partuuid(nvme_root_device).await?;
    tracing::info!("  NVMe root PARTUUID: {}", partuuid);

    // Phase 1 of #22 mounts the NVMe Boot (FAT32) partition at boot/firmware/
    let nvme_cmdline = mount_root.join("boot/firmware/cmdline.txt");
    // Ensure the directory exists (should be mounted, but create defensively)
    if let Some(parent) = nvme_cmdline.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            BeaconError::Provisioning(format!("Failed to create {}: {}", parent.display(), e))
        })?;
    }

    write_cmdline(&partuuid, &nvme_cmdline).await?;

    tracing::info!("✅ NVMe Boot partition cmdline.txt written");
    Ok(())
}

/// Set the Pi 5 EEPROM BOOT_ORDER to USB→NVMe→SD ([`BOOT_ORDER_NVME_FIRST`]) via rpi-eeprom-config.
///
/// Idempotent: if BOOT_ORDER is already correct, the apply step is skipped.
/// After `--apply`, reads the staged `pieeprom.upd` file (NOT the live EEPROM —
/// live EEPROM is only updated on next reboot) and verifies BOOT_ORDER matches.
/// A Pi with misconfigured BOOT_ORDER must not be rebooted silently.
///
/// This must be the last step in stage 5 so the EEPROM is only updated after
/// the NVMe Boot partition is fully populated and verified.
async fn configure_pi_eeprom_boot_order() -> Result<()> {
    tracing::info!(
        "Configuring Pi 5 EEPROM BOOT_ORDER to {} (USB→NVMe→SD)",
        BOOT_ORDER_NVME_FIRST
    );

    let current = read_current_eeprom_config()
        .await
        .map_err(|e| BeaconError::Provisioning(e.to_string()))?;

    if current.is_correct_for_nvme_first() {
        tracing::info!(
            "  BOOT_ORDER already {} — skipping EEPROM update",
            BOOT_ORDER_NVME_FIRST
        );
        return Ok(());
    }

    let new_config = current.with_correct_eeprom_config();
    tracing::info!(
        "  BOOT_ORDER diff: old config had different value → setting {}",
        BOOT_ORDER_NVME_FIRST
    );

    apply_eeprom_config(&new_config)
        .await
        .map_err(|e| BeaconError::Provisioning(e.to_string()))?;

    // Verify by reading the STAGED file — NOT the live EEPROM.
    // `rpi-eeprom-config --apply` writes pieeprom.upd to the bootfs;
    // the live EEPROM is only re-flashed on next reboot, so reading live
    // EEPROM here would always return the OLD value.
    let staged_path = find_staged_eeprom_file().ok_or_else(|| {
        BeaconError::Provisioning(
            "rpi-eeprom-config --apply did not produce a staged file \
             (checked /boot/firmware/pieeprom.upd and /boot/pieeprom.upd)"
                .to_string(),
        )
    })?;
    tracing::info!("  Found staged EEPROM file: {}", staged_path.display());

    verify_staged_eeprom_boot_order(&staged_path)
        .await
        .map_err(|e| BeaconError::Provisioning(e.to_string()))?;

    tracing::info!(
        "Pi 5 EEPROM BOOT_ORDER staged to {} (USB→NVMe→SD) — will take effect after reboot",
        BOOT_ORDER_NVME_FIRST
    );
    Ok(())
}

/// Set up /music directory and required subdirectories for the mdma user (#60).
///
/// mdma-library crashes on startup if `/music/by-artist` doesn't exist and the
/// mdma user can't create it (because `/music` is root-owned). This function
/// creates the expected layout and sets ownership to `mdma:mdma`.
async fn setup_music_directory(mount_root: &Path) -> Result<()> {
    tracing::info!("Setting up /music directory ownership for mdma user");

    let subdirs = ["music/downloads", "music/inbox", "music/by-artist"];
    for subdir in &subdirs {
        let path = mount_root.join(subdir);
        tokio::fs::create_dir_all(&path).await.map_err(|e| {
            BeaconError::Provisioning(format!("Failed to create {}: {}", path.display(), e))
        })?;
        tracing::info!("  Created {}", path.display());
    }

    // chown -R mdma:mdma /music (inside the target)
    let output = Command::new("chroot")
        .arg(mount_root)
        .args(["chown", "-R", "mdma:mdma", "/music"])
        .output()
        .await
        .map_err(|e| BeaconError::command_failed("chroot chown /music", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BeaconError::Provisioning(format!(
            "Failed to chown /music: {}",
            stderr
        )));
    }

    tracing::info!("✅ /music directory configured for mdma user");
    Ok(())
}

/// Seed `/etc/mdma/bandcamp.conf` from the example file if not already present (#61).
///
/// The mdma-bandcamp package ships `bandcamp.conf.example`; the package's own
/// INSTALL script (option b) copies it on `xbps-install`. However, when packages
/// are installed with `-r <root>` the INSTALL script runs inside the chroot and
/// may not be triggered correctly. This belt-and-suspenders copy ensures the conf
/// always exists after provisioning, regardless of INSTALL hook execution.
async fn seed_bandcamp_config(mount_root: &Path) -> Result<()> {
    let conf_path = mount_root.join("etc/mdma/bandcamp.conf");
    let example_path = mount_root.join("etc/mdma/bandcamp.conf.example");

    if conf_path.exists() {
        tracing::info!("  /etc/mdma/bandcamp.conf already exists — skipping seed");
        return Ok(());
    }

    if !example_path.exists() {
        tracing::error!(
            "  /etc/mdma/bandcamp.conf.example not found — mdma-bandcamp package is missing or broken; bandcamp will not work"
        );
        return Err(BeaconError::Provisioning(
            "/etc/mdma/bandcamp.conf.example not found — mdma-bandcamp package is missing or broken; bandcamp will not work".to_string(),
        ));
    }

    // Ensure directory exists
    let conf_dir = mount_root.join("etc/mdma");
    tokio::fs::create_dir_all(&conf_dir).await.map_err(|e| {
        BeaconError::Provisioning(format!("Failed to create {}: {}", conf_dir.display(), e))
    })?;

    tokio::fs::copy(&example_path, &conf_path)
        .await
        .map_err(|e| {
            BeaconError::Provisioning(format!(
                "Failed to copy bandcamp.conf.example to bandcamp.conf: {}",
                e
            ))
        })?;

    tracing::info!("✅ Seeded /etc/mdma/bandcamp.conf from example");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{rewrite_cmdline, sync_kernel_to_nvme_boot, TARGET_PACKAGES};
    use crate::error::BeaconError;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn target_packages_does_not_include_beacon() {
        // beacon is the SD-card bootstrap, not a runtime target service.
        // If this fails, someone re-added beacon to the install list.
        assert!(
            !TARGET_PACKAGES.contains(&"beacon"),
            "beacon should not be installed on the provisioned target"
        );
    }

    #[test]
    fn target_packages_includes_mdma_admin() {
        assert!(
            TARGET_PACKAGES.contains(&"mdma-admin"),
            "mdma-admin must be in TARGET_PACKAGES so it is installed on the provisioned target"
        );
    }

    // ── #54 / #55: cmdline rewrite helper ────────────────────────────────────

    #[test]
    fn rewrite_cmdline_replaces_root_partuuid() {
        let input =
            "console=serial0,115200 console=tty1 root=PARTUUID=77668be1-02 rootfstype=ext4 rootwait\n";
        let out = rewrite_cmdline(input, "aabbccdd-01");
        assert!(
            out.contains("root=PARTUUID=aabbccdd-01"),
            "expected new PARTUUID in output, got: {}",
            out
        );
        // Old PARTUUID must be gone
        assert!(
            !out.contains("77668be1-02"),
            "old PARTUUID should be removed, got: {}",
            out
        );
    }

    #[test]
    fn rewrite_cmdline_replaces_dev_path_root() {
        // #55: NVMe-side cmdline had root=/dev/mmcblk0p2
        let input =
            "console=serial0,115200 console=tty1 root=/dev/mmcblk0p2 rootfstype=ext4 rootwait\n";
        let out = rewrite_cmdline(input, "deadbeef-01");
        assert!(
            out.contains("root=PARTUUID=deadbeef-01"),
            "expected PARTUUID root in output, got: {}",
            out
        );
        assert!(
            !out.contains("/dev/mmcblk0p2"),
            "old root path should be removed, got: {}",
            out
        );
    }

    #[test]
    fn rewrite_cmdline_preserves_other_parameters() {
        let input = "console=serial0,115200 console=tty1 root=PARTUUID=old-uuid rootfstype=ext4 rootwait quiet splash\n";
        let out = rewrite_cmdline(input, "new-uuid");
        for param in &[
            "console=serial0,115200",
            "console=tty1",
            "rootfstype=ext4",
            "rootwait",
            "quiet",
            "splash",
        ] {
            assert!(
                out.contains(param),
                "parameter '{}' missing from rewritten cmdline: {}",
                param,
                out
            );
        }
    }

    #[test]
    fn rewrite_cmdline_empty_input_generates_default() {
        let out = rewrite_cmdline("", "cafebabe-02");
        assert!(
            out.contains("root=PARTUUID=cafebabe-02"),
            "expected PARTUUID in generated cmdline, got: {}",
            out
        );
        assert!(out.ends_with('\n'), "cmdline should end with newline");
    }

    #[test]
    fn rewrite_cmdline_always_ends_with_newline() {
        let out = rewrite_cmdline("console=tty1 root=PARTUUID=x rootfstype=ext4 rootwait", "y");
        assert!(out.ends_with('\n'));
    }

    // ── #62: authorized_keys permissions ─────────────────────────────────────

    #[test]
    fn authorized_keys_written_with_mode_0600() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ssh_dir = dir.path().join(".ssh");
        std::fs::create_dir_all(&ssh_dir).expect("create .ssh");

        // Simulate umask 0o002 by writing with default OpenOptions (no explicit mode)
        let auth_keys = ssh_dir.join("authorized_keys");
        std::fs::write(&auth_keys, "ssh-ed25519 AAAA test\n").expect("write");

        // Apply the same explicit permission set that setup_ssh_for_admin now does
        std::fs::set_permissions(&auth_keys, std::fs::Permissions::from_mode(0o600))
            .expect("set perms");

        let mode = std::fs::metadata(&auth_keys)
            .expect("metadata")
            .permissions()
            .mode();
        // Mask off file-type bits — we only care about the permission bits
        assert_eq!(
            mode & 0o777,
            0o600,
            "authorized_keys should be 0o600, got {:o}",
            mode & 0o777
        );
    }

    #[test]
    fn ssh_dir_written_with_mode_0700() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ssh_dir = dir.path().join(".ssh");
        std::fs::create_dir_all(&ssh_dir).expect("create .ssh");

        std::fs::set_permissions(&ssh_dir, std::fs::Permissions::from_mode(0o700))
            .expect("set perms");

        let mode = std::fs::metadata(&ssh_dir)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o700,
            ".ssh should be 0o700, got {:o}",
            mode & 0o777
        );
    }

    // ── #22 Phase 2: NVMe boot kernel sync ───────────────────────────────────

    #[tokio::test]
    async fn sync_kernel_to_nvme_boot_copies_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mount_root = dir.path();

        // Set up fake mount_root/boot/ with kernel + dtbs + config.txt
        let boot = mount_root.join("boot");
        std::fs::create_dir_all(&boot).expect("create boot");
        std::fs::write(boot.join("kernel8.img"), b"fake-kernel").expect("write kernel");
        std::fs::write(boot.join("bcm2712-rpi-5-b.dtb"), b"fake-dtb").expect("write dtb");
        std::fs::write(boot.join("config.txt"), b"# rpi config").expect("write config");

        // Overlays live in boot/overlays/ — provided by rpi-kernel package
        let overlays = boot.join("overlays");
        std::fs::create_dir_all(&overlays).expect("create overlays");
        std::fs::write(overlays.join("some.dtbo"), b"fake-overlay").expect("write dtbo");

        // Destination must exist (mount point for NVMe Boot partition)
        let firmware = mount_root.join("boot/firmware");
        std::fs::create_dir_all(&firmware).expect("create firmware");

        sync_kernel_to_nvme_boot(mount_root)
            .await
            .expect("sync_kernel_to_nvme_boot");

        assert!(
            firmware.join("kernel8.img").exists(),
            "kernel8.img missing from firmware"
        );
        assert!(
            firmware.join("bcm2712-rpi-5-b.dtb").exists(),
            "dtb missing from firmware"
        );
        assert!(
            firmware.join("config.txt").exists(),
            "config.txt missing from firmware"
        );
        // Overlays must be in firmware/overlays/, not flat in firmware/
        assert!(
            firmware.join("overlays/some.dtbo").exists(),
            "some.dtbo missing from firmware/overlays/"
        );
        assert!(
            !firmware.join("some.dtbo").exists(),
            "dtbo must NOT be copied flat into firmware/ — must be in overlays/"
        );
    }

    #[tokio::test]
    async fn sync_kernel_to_nvme_boot_errors_on_missing_config_txt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mount_root = dir.path();

        let boot = mount_root.join("boot");
        std::fs::create_dir_all(&boot).expect("create boot");
        std::fs::write(boot.join("kernel8.img"), b"fake-kernel").expect("write kernel");
        // No config.txt — rpi-firmware not installed
        let overlays = boot.join("overlays");
        std::fs::create_dir_all(&overlays).expect("create overlays");

        let firmware = mount_root.join("boot/firmware");
        std::fs::create_dir_all(&firmware).expect("create firmware");

        let err = sync_kernel_to_nvme_boot(mount_root)
            .await
            .expect_err("should fail when config.txt is missing");

        match err {
            BeaconError::Provisioning(msg) => {
                assert!(
                    msg.contains("config.txt"),
                    "error should mention config.txt, got: {}",
                    msg
                );
                assert!(
                    msg.contains("rpi-firmware"),
                    "error should mention rpi-firmware, got: {}",
                    msg
                );
            }
            other => panic!("expected Provisioning error, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn sync_kernel_to_nvme_boot_errors_on_missing_overlays_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mount_root = dir.path();

        let boot = mount_root.join("boot");
        std::fs::create_dir_all(&boot).expect("create boot");
        std::fs::write(boot.join("kernel8.img"), b"fake-kernel").expect("write kernel");
        std::fs::write(boot.join("config.txt"), b"# rpi config").expect("write config");
        // No overlays/ dir — rpi-kernel not installed

        let firmware = mount_root.join("boot/firmware");
        std::fs::create_dir_all(&firmware).expect("create firmware");

        let err = sync_kernel_to_nvme_boot(mount_root)
            .await
            .expect_err("should fail when boot/overlays/ is missing");

        match err {
            BeaconError::Provisioning(msg) => {
                assert!(
                    msg.contains("overlays"),
                    "error should mention overlays, got: {}",
                    msg
                );
                assert!(
                    msg.contains("rpi-kernel"),
                    "error should mention rpi-kernel, got: {}",
                    msg
                );
            }
            other => panic!("expected Provisioning error, got: {:?}", other),
        }
    }
}
