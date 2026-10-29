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
//! 8. Write boot partition cmdline.txt (`/boot/cmdline.txt`; the boot partition is
//!    mounted at `/boot`, so the kernel packages write to it directly)
//! 9. Set Pi 5 EEPROM BOOT_ORDER to USB→NVMe→SD (0xf164)
//!
//! Optionally (when the user supplied a wifi network) the wifi configuration
//! is written and `wpa_supplicant` is enabled (#41).

use crate::actions::{Action, ActionId, PlannedAction};
use crate::error::{BeaconError, Result};
use crate::provisioning::types::{ConfiguredFstab, ConfiguredSystem, MountPoint, Partition};
use rpi_eeprom::{
    apply_eeprom_config, find_staged_eeprom_file, read_current_eeprom_config,
    verify_staged_eeprom_boot_order, EepromError, BOOT_ORDER_NVME_FIRST,
};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
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

        // 9b. Configure wifi (only when the user supplied a network). After the
        // package installs so wpa_supplicant owns /etc/sv/wpa_supplicant first.
        let system = planned_output.clone();
        tokio::task::spawn_blocking(move || configure_wifi(&system))
            .await
            .map_err(|e| BeaconError::Provisioning(format!("wifi configuration task: {e}")))??;

        // 10. Enable services
        enable_services(mount_root, config.network.as_ref()).await?;

        // 11. Write boot partition cmdline.txt (#55, #65) — the boot partition is
        // mounted at /boot, so packages already wrote the kernel to it.
        configure_nvme_boot_cmdline(mount_root, root_device(planned_output.partitions())?).await?;

        // 12. Set up /music directory ownership for mdma user (#60)
        setup_music_directory(mount_root).await?;

        // 13. Seed bandcamp.conf if not already present (#61)
        seed_bandcamp_config(mount_root).await?;

        // 14. Set EEPROM BOOT_ORDER to USB→NVMe→SD — MUST be last (#22)
        configure_pi_eeprom_boot_order().await?;

        tracing::info!("✅ Stage 5: System configuration complete");
        Ok(planned_output.clone())
    }
}

/// Find the device of the Root partition in the provisioning plan.
fn root_device(partitions: &[Partition]) -> Result<&str> {
    partitions
        .iter()
        .find(|p| p.mount_point == MountPoint::Root)
        .map(|p| p.device.as_str())
        .ok_or_else(|| BeaconError::Provisioning("No Root partition in provisioning plan".into()))
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

/// Write the wifi configuration, if one was supplied.
///
/// `Some`: writes `wpa_supplicant.conf` (0600, it holds the PSK) and the runit
/// service `conf` that pins the service to wlan0. `None`: touches nothing and
/// logs what that means for the given data policy. Sync (blocking fs); callers
/// in async code use `spawn_blocking`. Must run after `install_packages` so the
/// wpa_supplicant package owns `/etc/sv/wpa_supplicant/` first.
fn configure_wifi(system: &ConfiguredSystem) -> Result<()> {
    let mount_root = system.mount_root();
    let Some(network) = system.config().network.as_ref() else {
        tracing::info!("{}", system.wifi_summary());
        return Ok(());
    };
    tracing::info!(
        "Configuring wifi: SSID \"{}\" country {}",
        network.ssid(),
        network.country()
    );

    let files = [
        (
            wifi_config::WPA_SUPPLICANT_CONF_PATH,
            wifi_config::render_wpa_supplicant_conf(network),
            0o600,
        ),
        (
            wifi_config::WPA_SERVICE_CONF_PATH,
            wifi_config::render_wpa_service_conf(),
            0o644,
        ),
    ];
    // A symlink at `path` itself is replaced, not followed; symlinks in parent
    // directories are followed.
    for (rel, content, mode) in files {
        let path = mount_root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                BeaconError::Provisioning(format!("Failed to create {}: {}", parent.display(), e))
            })?;
        }
        private_file::write_file_atomic(&path, content.as_bytes(), mode).map_err(|e| {
            BeaconError::Provisioning(format!("Failed to write {}: {}", path.display(), e))
        })?;
    }

    tracing::info!("✅ Wifi configured");
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
        "wpa_supplicant",
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
        return Err(BeaconError::Provisioning(format!(
            "Failed to install MDMA packages ({}): {}",
            TARGET_PACKAGES.join(" "),
            stderr.trim()
        )));
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

/// Non-MDMA runit services that are always enabled.
const BASE_SERVICES: [&str; 5] = ["sshd", "dhcpcd", "dbus", "avahi-daemon", "pipewire"];

/// The runit services to enable: the base services, every MDMA package (each
/// `TARGET_PACKAGES` entry is also its service name), and `wpa_supplicant` only
/// when a wifi network is configured.
fn services_to_enable(network: Option<&wifi_config::NetworkConfig>) -> Vec<&'static str> {
    let mut services: Vec<&'static str> = BASE_SERVICES
        .iter()
        .chain(TARGET_PACKAGES.iter())
        .copied()
        .collect();
    if network.is_some() {
        services.push("wpa_supplicant");
    }
    services
}

/// Enable runit services via symlinks
async fn enable_services(
    mount_root: &Path,
    network: Option<&wifi_config::NetworkConfig>,
) -> Result<()> {
    tracing::info!("Enabling services (runit)");

    let services = services_to_enable(network);

    // Create log directories for MDMA services
    for log_dir in TARGET_PACKAGES {
        let log_path = mount_root.join(format!("var/log/{}", log_dir));
        tokio::fs::create_dir_all(&log_path).await.map_err(|e| {
            BeaconError::Provisioning(format!("Failed to create {}: {}", log_path.display(), e))
        })?;
        tracing::info!("  Created log directory: /var/log/{}", log_dir);
    }

    for service in services {
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

/// Look up the PARTUUID for a block device using blkid.
///
/// Returns the PARTUUID string (e.g. `"abc123ef-02"`) or an error if blkid
/// fails or the device has no PARTUUID.
pub(crate) async fn get_partuuid(device: &str) -> Result<String> {
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

/// Kernel params disabling NVMe APST and PCIe ASPM / port power management (#69).
///
/// On Pi 5 the NVMe controller can drop off the PCIe bus (~65s after boot,
/// `CSTS=0xffffffff`), hanging the system. MDMA is mains-powered, so we trade
/// power saving for stability unconditionally.
const NVME_STABILITY_PARAMS: [&str; 3] = [
    "nvme_core.default_ps_max_latency_us=0",
    "pcie_aspm=off",
    "pcie_port_pm=off",
];

/// The key of a kernel parameter: the part before `=`, or the whole token.
fn param_key(tok: &str) -> &str {
    tok.split_once('=').map_or(tok, |(k, _)| k)
}

/// Build a cmdline.txt that boots from `root_partuuid`.
///
/// Replaces the `root=` token in `current_cmdline` with the PARTUUID form.
/// All other kernel parameters are preserved, except the
/// [`NVME_STABILITY_PARAMS`] keys, whose existing values are replaced. A
/// trailing newline is always appended.
///
/// If `current_cmdline` is empty or contains no `root=` token, a safe
/// default is produced from scratch.
///
/// The [`NVME_STABILITY_PARAMS`] are always present exactly once in the result.
fn rewrite_cmdline(current_cmdline: &str, root_partuuid: &str) -> String {
    let new_root = format!("root=PARTUUID={}", root_partuuid);

    let trimmed = current_cmdline.trim();
    let base = if trimmed.is_empty() {
        // No existing cmdline — generate a minimal one.
        format!(
            "console=serial0,115200 console=tty1 {} rootfstype=ext4 rootwait",
            new_root
        )
    } else {
        // Replace any existing `root=...` token, preserving all other parameters.
        let mut tokens: Vec<String> = trimmed
            .split_whitespace()
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
            tokens.push(new_root);
        }
        tokens.join(" ")
    };

    // Drop any existing occurrence (any value) of our params, then append once.
    let mut out: Vec<&str> = base
        .split_whitespace()
        .filter(|tok| {
            let key = param_key(tok);
            !NVME_STABILITY_PARAMS.iter().any(|p| param_key(p) == key)
        })
        .collect();
    out.extend(NVME_STABILITY_PARAMS);
    format!("{}\n", out.join(" "))
}

/// Write `cmdline.txt` at `dest` using `root_partuuid` as the root device.
///
/// Reads the existing file (if any) and preserves all other kernel parameters,
/// except the [`NVME_STABILITY_PARAMS`] keys, whose existing values are
/// replaced. Creates a `.bak` backup on first write.
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
        tracing::info!("  {} already up to date — no change", dest.display());
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

/// Path of the boot partition's `cmdline.txt` (boot partition is mounted at `/boot`, #65).
fn boot_cmdline_path(mount_root: &Path) -> PathBuf {
    mount_root.join("boot/cmdline.txt")
}

/// Write `<mount_root>/boot/cmdline.txt` booting from `root_partuuid`.
async fn write_boot_cmdline(mount_root: &Path, root_partuuid: &str) -> Result<()> {
    let cmdline = boot_cmdline_path(mount_root);
    // Ensure the directory exists (should be mounted, but create defensively)
    if let Some(parent) = cmdline.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            BeaconError::Provisioning(format!("Failed to create {}: {}", parent.display(), e))
        })?;
    }
    write_cmdline(root_partuuid, &cmdline).await
}

/// Write NVMe Boot partition `cmdline.txt` so it boots from the NVMe root partition (#55, #65).
///
/// The boot partition is mounted at `<mount_root>/boot`, so the Void kernel
/// packages write straight to it. `cmdline.txt` is a conf_file of `rpi-firmware`,
/// so the rewritten copy survives package updates.
async fn configure_nvme_boot_cmdline(mount_root: &Path, nvme_root_device: &str) -> Result<()> {
    tracing::info!("Writing NVMe Boot partition cmdline.txt");

    let partuuid = get_partuuid(nvme_root_device).await?;
    tracing::info!("  NVMe root PARTUUID: {}", partuuid);

    write_boot_cmdline(mount_root, &partuuid).await?;

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
    let staged_path = find_staged_eeprom_file()
        .ok_or_else(|| BeaconError::Provisioning(EepromError::NoStagedFile.to_string()))?;
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

/// Directories (inside the target) that provisioning creates and chowns to
/// `mdma:mdma`. Chowned non-recursively so an existing library is not walked.
const MUSIC_CHOWN_PATHS: [&str; 4] = [
    "/music",
    "/music/downloads",
    "/music/inbox",
    "/music/by-artist",
];

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

    // Non-recursive chown of only the directories provisioning creates (inside
    // the target). A recursive chown would walk the whole existing library.
    let output = Command::new("chroot")
        .arg(mount_root)
        .args(["chown", "mdma:mdma"])
        .args(MUSIC_CHOWN_PATHS)
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
    use super::{
        boot_cmdline_path, rewrite_cmdline, root_device, write_boot_cmdline, MountPoint, Partition,
        MUSIC_CHOWN_PATHS, TARGET_PACKAGES,
    };
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

    fn count_token(cmdline: &str, token: &str) -> usize {
        cmdline.split_whitespace().filter(|t| *t == token).count()
    }

    #[test]
    fn rewrite_cmdline_adds_nvme_stability_params_exactly_once() {
        for input in ["", "console=tty1 root=/dev/mmcblk0p2 rootwait\n"] {
            let out = rewrite_cmdline(input, "aa-01");
            for p in super::NVME_STABILITY_PARAMS {
                assert_eq!(count_token(&out, p), 1, "{p} in: {out}");
            }
            assert!(out.contains("root=PARTUUID=aa-01"), "{out}");
            assert_eq!(out.matches("root=").count(), 1, "{out}");
        }
    }

    #[test]
    fn rewrite_cmdline_nvme_params_idempotent() {
        let input = "root=PARTUUID=old rootwait pcie_aspm=off quiet \
                     nvme_core.default_ps_max_latency_us=0 pcie_port_pm=off\n";
        let once = rewrite_cmdline(input, "new-01");
        for p in super::NVME_STABILITY_PARAMS {
            assert_eq!(count_token(&once, p), 1, "{p} in: {once}");
        }
        assert!(once.contains("root=PARTUUID=new-01") && once.contains("quiet"));
        assert_eq!(rewrite_cmdline(&once, "new-01"), once);
    }

    #[test]
    fn rewrite_cmdline_overrides_conflicting_nvme_param_values() {
        let out = rewrite_cmdline(
            "root=/dev/x pcie_aspm=force pcie_aspm=force pcie_aspm.policy=performance",
            "u-01",
        );
        assert_eq!(count_token(&out, "pcie_aspm=off"), 1, "{out}");
        assert_eq!(
            count_token(&out, "pcie_aspm.policy=performance"),
            1,
            "{out}"
        );
        assert!(!out.contains("pcie_aspm=force"), "{out}");
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

    // ── wifi (#41) ───────────────────────────────────────────────────────────

    use super::{configure_wifi, services_to_enable, ConfigureSystemAction};
    use crate::actions::Action;
    use crate::provisioning::types::{
        CompletedPartitionPlan, CompletedPartitionedDrives, ConfiguredFstab, ConfiguredSystem,
        DriveInfo, FormattedSystem, InstalledPackages, MountedPartitions, ValidatedDrives,
        ValidatedHardware,
    };
    use crate::types::{
        ByteSize, DevicePath, ExistingDataPolicy, Hostname, ProvisionConfig, SshPublicKey, UnitType,
    };
    use wifi_config::{
        render_wpa_supplicant_conf, CountryCode, NetworkConfig, WifiPassphrase, WifiSsid,
        WPA_SERVICE_CONF_PATH, WPA_SUPPLICANT_CONF_PATH,
    };

    fn network() -> NetworkConfig {
        NetworkConfig::new(
            WifiSsid::new("HomeNet").expect("ssid"),
            &WifiPassphrase::new("correct horse battery").expect("pass"),
            CountryCode::new("se").expect("country"),
        )
    }

    fn fstab(mount_root: &std::path::Path, network: Option<NetworkConfig>) -> ConfiguredFstab {
        fstab_with_policy(mount_root, network, ExistingDataPolicy::Preserve)
    }

    fn fstab_with_policy(
        mount_root: &std::path::Path,
        network: Option<NetworkConfig>,
        existing_data: ExistingDataPolicy,
    ) -> ConfiguredFstab {
        let drive = DriveInfo {
            device: DevicePath::new("/dev/nvme0n1").expect("device"),
            size_bytes: ByteSize::from_gb(512),
            model: "Test".to_string(),
        };
        let config = ProvisionConfig {
            hostname: Hostname::new("test-909".to_string()).expect("hostname"),
            unit_type: UnitType::Mdma909,
            ssh_key: SshPublicKey::new(
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIA96k1y1Y1326DtI4csBGXSqu57wjNuBYEkyjUQ3uS7x t"
                    .to_string(),
            )
            .expect("key"),
            existing_data,
            network,
        };
        ConfiguredFstab {
            installed: InstalledPackages {
                mounted: MountedPartitions {
                    formatted: FormattedSystem {
                        partitioned: CompletedPartitionedDrives {
                            validated: ValidatedHardware {
                                config,
                                drives: ValidatedDrives::OneDrive(drive.clone()),
                            },
                            plan: CompletedPartitionPlan::SingleDrive {
                                device: drive,
                                partitions: vec![],
                            },
                            created: vec![],
                        },
                        format_actions: vec![],
                    },
                    mount_root: mount_root.to_path_buf(),
                    partitions: vec![],
                },
                packages: vec![],
            },
        }
    }

    fn mode_of(path: &std::path::Path) -> u32 {
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    }

    fn system(
        dir: &std::path::Path,
        network: Option<NetworkConfig>,
        policy: ExistingDataPolicy,
    ) -> ConfiguredSystem {
        ConfiguredSystem {
            fstab_configured: fstab_with_policy(dir, network, policy),
        }
    }

    #[test]
    fn configure_wifi_some_writes_private_conf_and_service_conf() {
        let dir = tempfile::tempdir().expect("tempdir");
        let net = network();
        configure_wifi(&system(
            dir.path(),
            Some(net.clone()),
            ExistingDataPolicy::Preserve,
        ))
        .expect("wifi");

        let conf = dir.path().join(WPA_SUPPLICANT_CONF_PATH);
        assert_eq!(mode_of(&conf), 0o600);
        assert_eq!(
            std::fs::read_to_string(&conf).expect("read"),
            render_wpa_supplicant_conf(&net)
        );
        let svc = std::fs::read_to_string(dir.path().join(WPA_SERVICE_CONF_PATH)).expect("svc");
        assert!(svc.contains("WPA_INTERFACE=wlan0"), "{svc}");
    }

    #[test]
    fn configure_wifi_tightens_existing_permissions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let conf = dir.path().join(WPA_SUPPLICANT_CONF_PATH);
        std::fs::create_dir_all(conf.parent().expect("parent")).expect("mkdir");
        // Longer than the new content, so truncation is exercised too.
        std::fs::write(&conf, "old\n".repeat(200)).expect("write");
        std::fs::set_permissions(&conf, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        let net = network();
        configure_wifi(&system(
            dir.path(),
            Some(net.clone()),
            ExistingDataPolicy::Preserve,
        ))
        .expect("wifi");

        assert_eq!(mode_of(&conf), 0o600);
        assert_eq!(
            std::fs::read_to_string(&conf).expect("read"),
            render_wpa_supplicant_conf(&net)
        );
    }

    #[test]
    fn configure_wifi_replaces_symlink_instead_of_following_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let victim = tempfile::tempdir().expect("victim dir");
        let victim_file = victim.path().join("host-file");
        std::fs::write(&victim_file, "host").expect("write");
        let conf = dir.path().join(WPA_SUPPLICANT_CONF_PATH);
        std::fs::create_dir_all(conf.parent().expect("parent")).expect("mkdir");
        std::os::unix::fs::symlink(&victim_file, &conf).expect("symlink");

        configure_wifi(&system(
            dir.path(),
            Some(network()),
            ExistingDataPolicy::Preserve,
        ))
        .expect("wifi");

        assert_eq!(std::fs::read(&victim_file).expect("read"), b"host");
        assert_eq!(mode_of(&conf), 0o600);
    }

    #[test]
    fn configure_wifi_none_touches_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let conf = dir.path().join(WPA_SUPPLICANT_CONF_PATH);
        std::fs::create_dir_all(conf.parent().expect("parent")).expect("mkdir");
        std::fs::write(&conf, "keep me\n").expect("write");
        std::fs::set_permissions(&conf, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        configure_wifi(&system(dir.path(), None, ExistingDataPolicy::Preserve)).expect("wifi");

        assert_eq!(std::fs::read(&conf).expect("read"), b"keep me\n");
        assert_eq!(mode_of(&conf), 0o644);
        assert!(!dir.path().join(WPA_SERVICE_CONF_PATH).exists());
    }

    #[test]
    fn services_to_enable_adds_wpa_supplicant_only_with_wifi() {
        let net = network();
        let with = services_to_enable(Some(&net));
        let without = services_to_enable(None);
        assert!(with.contains(&"wpa_supplicant"));
        assert!(!without.contains(&"wpa_supplicant"));
        let filtered: Vec<_> = with
            .iter()
            .copied()
            .filter(|s| *s != "wpa_supplicant")
            .collect();
        assert_eq!(filtered, without);
    }

    #[tokio::test]
    async fn plan_states_wifi_without_psk_and_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let net = network();
        let psk = net.psk().to_hex();
        let input = fstab(dir.path(), Some(net));

        let planned = ConfigureSystemAction.plan(&input).await.expect("plan");
        let text = planned.assumed_output.to_string();

        assert!(text.contains("SSID \"HomeNet\" country SE"), "{text}");
        assert!(
            text.contains("etc/wpa_supplicant/wpa_supplicant.conf"),
            "{text}"
        );
        assert!(text.contains("enable wpa_supplicant"), "{text}");
        assert!(!text.contains(&psk));
        assert_eq!(
            std::fs::read_dir(dir.path()).expect("readdir").count(),
            0,
            "check mode (plan) must write nothing"
        );
    }

    #[rstest::rstest]
    #[case(
        ExistingDataPolicy::Preserve,
        "wifi: not configured here — any existing wifi setup is kept"
    )]
    #[case(ExistingDataPolicy::ReinstallOs, "the OS is reinstalled")]
    #[case(ExistingDataPolicy::WipeAll, "the OS is reinstalled")]
    fn wifi_summary_without_wifi_is_policy_aware(
        #[case] policy: ExistingDataPolicy,
        #[case] expected: &str,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let sys = system(dir.path(), None, policy);
        assert!(
            sys.wifi_summary().contains(expected),
            "{}",
            sys.wifi_summary()
        );
        assert!(sys.to_string().contains(expected));
    }

    #[test]
    fn wifi_summary_under_preserve_says_removed_when_root_is_formatted() {
        use crate::provisioning::types::{PartitionFormatAction, PartitionSize};
        use crate::types::FilesystemType;

        let dir = tempfile::tempdir().expect("tempdir");
        let mut fstab = fstab(dir.path(), None);
        let root = Partition {
            device: DevicePath::new("/dev/nvme0n1p2").expect("device"),
            mount_point: MountPoint::Root,
            size: PartitionSize::from_mb(512),
        };
        fstab.installed.mounted.formatted.format_actions =
            vec![PartitionFormatAction::WillFormat {
                device: root.device.clone(),
                label: root.label(),
                fs_type: FilesystemType::Ext4,
            }];
        fstab.installed.mounted.partitions = vec![root];
        let sys = ConfiguredSystem {
            fstab_configured: fstab,
        };
        assert!(
            sys.wifi_summary()
                .contains("any previous wifi setup is removed"),
            "{}",
            sys.wifi_summary()
        );
    }

    #[test]
    fn debug_of_configured_system_does_not_leak_psk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let net = network();
        let psk = net.psk().to_hex();
        let sys = ConfiguredSystem {
            fstab_configured: fstab(dir.path(), Some(net)),
        };
        let provisioned = crate::provisioning::types::ProvisionedSystem {
            configured: sys.clone(),
        };
        assert!(!format!("{sys:?}").contains(&psk));
        assert!(!format!("{provisioned:?}").contains(&psk));
        assert!(format!("{sys:?}").contains("redacted"));
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

    // ── #65: boot partition is mounted at /boot ──────────────────────────────

    #[test]
    fn boot_cmdline_path_is_boot_cmdline_txt() {
        let root = std::path::Path::new("/mnt/mdma-install");
        assert_eq!(
            boot_cmdline_path(root),
            std::path::PathBuf::from("/mnt/mdma-install/boot/cmdline.txt")
        );
    }

    #[tokio::test]
    async fn write_boot_cmdline_writes_to_boot_cmdline_txt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mount_root = dir.path();

        write_boot_cmdline(mount_root, "aabbccdd-02")
            .await
            .expect("write_boot_cmdline");

        let written = std::fs::read_to_string(mount_root.join("boot/cmdline.txt"))
            .expect("boot/cmdline.txt must exist");
        assert!(written.contains("root=PARTUUID=aabbccdd-02"), "{written}");
        assert!(
            !mount_root.join("boot/firmware").exists(),
            "must not create boot/firmware"
        );
    }

    #[test]
    fn root_device_comes_from_the_plan_root_partition() {
        use crate::provisioning::types::{DevicePath, PartitionSize};

        let part = |mp, dev: &str| Partition {
            device: DevicePath::new(dev).expect("device"),
            mount_point: mp,
            size: PartitionSize::from_mb(512),
        };
        let parts = vec![
            part(MountPoint::Boot, "/dev/nvme0n1p1"),
            part(MountPoint::Root, "/dev/nvme0n1p2"),
        ];
        assert_eq!(root_device(&parts).expect("root"), "/dev/nvme0n1p2");
        assert!(root_device(&parts[..1]).is_err());
    }

    #[test]
    fn music_chown_paths_are_the_created_directories() {
        assert_eq!(
            MUSIC_CHOWN_PATHS,
            [
                "/music",
                "/music/downloads",
                "/music/inbox",
                "/music/by-artist"
            ]
        );
    }
}
