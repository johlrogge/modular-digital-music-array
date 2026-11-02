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
//! 9. Force the PCIe link to Gen1 in `/boot/config.txt` (`dtparam=pciex1_gen=1`,
//!    the actual fix for NVMe dropping off the bus, #69)
//! 10. Set up `/music` ownership and seed `bandcamp.conf`
//! 11. Set Pi 5 EEPROM BOOT_ORDER to USB→NVMe→SD (0xf164)
//!
//! `cmdline.txt` and `config.txt` are rewritten atomically; a `.bak` copy of the
//! original is kept the first time a file changes.
//!
//! Optionally (when the user supplied a wifi network, or the beacon runs on the
//! installed system's wifi and the form's wifi fields were blank, in which case
//! that configuration is carried over verbatim) the wifi configuration is
//! written and `wpa_supplicant` is enabled (#41, #72).

use crate::actions::{Action, ActionId, PlannedAction};
use crate::error::{BeaconError, Result};
use crate::provisioning::types::{
    ConfiguredFstab, ConfiguredSystem, MountPoint, Partition, WifiAction, WifiWrite,
};
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
        enable_services(mount_root, &planned_output.wifi_action()).await?;

        // 11. Write boot partition cmdline.txt (#55, #65) — the boot partition is
        // mounted at /boot, so packages already wrote the kernel to it.
        configure_nvme_boot_cmdline(mount_root, root_device(planned_output.partitions())?).await?;

        // 12. config.txt PCIe Gen1 (#69)
        ensure_boot_config_pcie_gen1(mount_root).await?;

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

/// The `wpa_supplicant.conf` text to write. A carried-over configuration is
/// used verbatim.
fn wpa_supplicant_conf_text(wifi: WifiWrite<'_>) -> String {
    match wifi {
        WifiWrite::FromForm(network) => wifi_config::render_wpa_supplicant_conf(network),
        WifiWrite::CarriedOver(conf) => conf.as_str().to_owned(),
    }
}

/// Write the wifi configuration, as decided by [`ConfiguredSystem::wifi_action`].
///
/// `Write`: writes `wpa_supplicant.conf` (0600, it holds the PSK; a
/// carried-over one verbatim) and the runit service `conf` that pins the service
/// to wlan0. `Nothing` and `KeepExisting`: touch nothing and log what that
/// means. Sync (blocking fs); callers in async code use `spawn_blocking`. Must
/// run after `install_packages` so the wpa_supplicant package owns
/// `/etc/sv/wpa_supplicant/` first.
fn configure_wifi(system: &ConfiguredSystem) -> Result<()> {
    let mount_root = system.mount_root();
    let conf_text = match system.wifi_action() {
        WifiAction::Nothing | WifiAction::KeepExisting => {
            tracing::info!("{}", system.wifi_summary());
            return Ok(());
        }
        WifiAction::Write(write) => {
            match write {
                WifiWrite::FromForm(network) => tracing::info!(
                    "Configuring wifi: SSID \"{}\" country {}",
                    network.ssid(),
                    network.country()
                ),
                WifiWrite::CarriedOver(_) => tracing::info!("{}", system.wifi_summary()),
            }
            wpa_supplicant_conf_text(write)
        }
    };

    let files = [
        (wifi_config::WPA_SUPPLICANT_CONF_PATH, conf_text, 0o600),
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
///
/// `acpid` is installed via mdma-admin's `depends` and `pipewire` via
/// mdma-audio's. They are enabled here but deliberately not listed in
/// `install_packages`, so there is one source of truth for packages.
const BASE_SERVICES: [&str; 6] = [
    "sshd",
    "dhcpcd",
    "dbus",
    "avahi-daemon",
    "pipewire",
    "acpid",
];

/// Extra services shipped inside `TARGET_PACKAGES` (`mdma-klog` comes from
/// `mdma-admin`). These are service names, not package names.
const EXTRA_PACKAGE_SERVICES: [&str; 1] = ["mdma-klog"];

/// The runit services to enable: the base services, every MDMA package (each
/// `TARGET_PACKAGES` entry is also its service name), the extra services
/// shipped inside those packages, and `wpa_supplicant` only
/// when wifi is written. `KeepExisting` and `Nothing` neither enable nor
/// disable it.
fn services_to_enable(wifi: &WifiAction<'_>) -> Vec<&'static str> {
    let mut services: Vec<&'static str> = BASE_SERVICES
        .iter()
        .chain(TARGET_PACKAGES.iter())
        .chain(EXTRA_PACKAGE_SERVICES.iter())
        .copied()
        .collect();
    if matches!(wifi, WifiAction::Write(_)) {
        services.push("wpa_supplicant");
    }
    services
}

/// Enable runit services via symlinks
async fn enable_services(mount_root: &Path, wifi: &WifiAction<'_>) -> Result<()> {
    tracing::info!("Enabling services (runit)");

    let services = services_to_enable(wifi);

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
/// Defensive only: on Pi 5 the NVMe controller can drop off the PCIe bus
/// (~65s after boot, `CSTS=0xffffffff`), hanging the system, but these params
/// turned out NOT to be the fix. The actual fix for #69 is forcing the PCIe
/// link to Gen1 via [`PCIE_GEN1_LINE`] in config.txt. MDMA is mains-powered,
/// so we keep these as harmless extra stability measures.
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

/// Outcome of [`rewrite_boot_file`].
#[derive(Debug, PartialEq, Eq)]
enum Rewritten {
    Unchanged,
    Written,
}

/// What [`rewrite_boot_file`] does when the file does not exist.
#[derive(Clone, Copy)]
enum IfMissing {
    /// Treat as empty and create the file (the parent directory must exist).
    StartEmpty,
    /// Fail: the boot partition is unpopulated or not mounted.
    Error,
}

/// Rewrite a boot partition file through `transform`, atomically.
///
/// Skips the write when the output equals the input exactly. Before the first
/// change a `<name>.bak` copy of a non-empty original is kept (never
/// overwritten). The parent directory is never created.
fn rewrite_boot_file(
    path: &Path,
    if_missing: IfMissing,
    transform: impl FnOnce(&str) -> String,
) -> Result<Rewritten> {
    let current = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => match if_missing {
            IfMissing::StartEmpty => String::new(),
            IfMissing::Error => {
                return Err(BeaconError::Provisioning(format!(
                    "{} does not exist — boot partition is unpopulated or not mounted",
                    path.display()
                )))
            }
        },
        Err(e) => {
            return Err(BeaconError::Provisioning(format!(
                "Failed to read {}: {}",
                path.display(),
                e
            )))
        }
    };

    let new_content = transform(&current);
    if new_content == current {
        tracing::info!("  {} already up to date — no change", path.display());
        return Ok(Rewritten::Unchanged);
    }

    let mut backup = path.as_os_str().to_owned();
    backup.push(".bak");
    let backup = PathBuf::from(backup);
    if !current.is_empty() && !backup.exists() {
        private_file::write_file_atomic(&backup, current.as_bytes(), 0o644).map_err(|e| {
            BeaconError::Provisioning(format!("Failed to backup {}: {}", path.display(), e))
        })?;
        tracing::info!("  Backed up original to {}", backup.display());
    }

    private_file::write_file_atomic(path, new_content.as_bytes(), 0o644).map_err(|e| {
        BeaconError::Provisioning(format!("Failed to write {}: {}", path.display(), e))
    })?;
    tracing::info!("  Wrote {}", path.display());
    Ok(Rewritten::Written)
}

/// Run [`rewrite_boot_file`] on the blocking pool.
async fn rewrite_boot_file_blocking(
    path: PathBuf,
    if_missing: IfMissing,
    transform: impl FnOnce(&str) -> String + Send + 'static,
) -> Result<Rewritten> {
    tokio::task::spawn_blocking(move || rewrite_boot_file(&path, if_missing, transform))
        .await
        .map_err(|e| BeaconError::Provisioning(format!("boot file rewrite task: {e}")))?
}

/// Path of the boot partition's `cmdline.txt` (boot partition is mounted at `/boot`, #65).
fn boot_cmdline_path(mount_root: &Path) -> PathBuf {
    mount_root.join("boot/cmdline.txt")
}

/// Write `<mount_root>/boot/cmdline.txt` booting from `root_partuuid`.
///
/// A missing cmdline.txt is created from a default; the `boot` directory must
/// already exist (it is the mounted boot partition).
async fn write_boot_cmdline(mount_root: &Path, root_partuuid: &str) -> Result<()> {
    let uuid = root_partuuid.to_string();
    rewrite_boot_file_blocking(
        boot_cmdline_path(mount_root),
        IfMissing::StartEmpty,
        move |c| rewrite_cmdline(c, &uuid),
    )
    .await?;
    Ok(())
}

/// The config.txt line forcing the external PCIe link to Gen1 (#69).
///
/// Verified on hardware to keep the NVMe stable with wifi active. Gen1
/// (~250 MB/s) is ample for audio, so it is applied unconditionally. The
/// kernel-side companions are [`NVME_STABILITY_PARAMS`].
const PCIE_GEN1_LINE: &str = "dtparam=pciex1_gen=1";

/// Key of the PCIe generation dtparam.
const PCIE_GEN_KEY: &str = "pciex1_gen=";

/// One config.txt line, as seen by all passes of [`ensure_pcie_gen1`].
#[derive(Debug, PartialEq, Eq)]
enum Line<'a> {
    /// A `[...]` section header (trimmed).
    Header(&'a str),
    /// A standalone `dtparam=pciex1_gen=<v>` line.
    PcieGen {
        gen1: bool,
    },
    /// A `dtparam=a=b,pciex1_gen=<v>,...` comma-combined line.
    PcieGenCombined,
    Other,
}

/// Classify one config.txt line.
///
/// Assumption: the firmware treats `#` as the start of a comment, so
/// `[pi4]  # note` is still a section header. Stripping is the conservative
/// direction: a commented header is always reported as a header (so a gen line
/// below it is never trusted as top-level), and it is returned with its comment
/// intact so it can never equal `[all]` and is never applicable, even if the
/// firmware turns out not to parse it as a header.
fn classify(line: &str) -> Line<'_> {
    let t = line.trim();
    let code = t.split('#').next().unwrap_or(t).trim_end();
    if code.starts_with('[') && code.ends_with(']') {
        return Line::Header(if code.len() == t.len() { code } else { t });
    }
    let Some(rest) = t.strip_prefix("dtparam=") else {
        return Line::Other;
    };
    if !rest.contains(',') {
        return match rest.strip_prefix(PCIE_GEN_KEY) {
            Some(v) => Line::PcieGen { gen1: v == "1" },
            None => Line::Other,
        };
    }
    if rest.split(',').any(|p| p.trim().starts_with(PCIE_GEN_KEY)) {
        Line::PcieGenCombined
    } else {
        Line::Other
    }
}

/// Whether a header resets to the unconditional region. Only top level and
/// `[all]` apply; filters like `[pi5]` AND together with earlier ones, so no
/// other header is trusted.
fn header_is_applicable(header: &str) -> bool {
    header == "[all]"
}

/// Ensure config.txt contains exactly one applicable `dtparam=pciex1_gen=1`
/// that is also the effective one (#69).
///
/// A line is applicable when it is at top level or under `[all]` with no other
/// header since. If exactly one applicable standalone gen line exists, it is
/// `=1`, and no later gen line of any kind (including ones in conditional
/// sections) could override it, the content is returned unchanged (apart from
/// a guaranteed trailing newline). Otherwise every applicable standalone
/// `dtparam=pciex1_gen=` line is removed and ours is appended (later wins),
/// preceded by `[all]` unless the file has no headers or the last header is
/// `[all]`. Gen lines in non-applicable sections (`[pi4]`, `[cm4]`, ...) are
/// left byte-for-byte.
///
/// Comma-combined lines (`dtparam=audio=on,pciex1_gen=3`) are never rewritten;
/// if one follows our line (or there is no line of ours), ours is re-appended
/// at EOF so it wins.
///
/// `include` directives in config.txt are not followed; gen lines in included
/// files are neither seen nor removed.
fn ensure_pcie_gen1(current: &str) -> String {
    let mut base = current.to_string();
    if !base.is_empty() && !base.ends_with('\n') {
        base.push('\n');
    }

    // Per line: (raw, classification, applicable). Applicability is the state
    // *at* that line.
    let mut applicable = true;
    let mut lines: Vec<(&str, Line<'_>, bool)> = Vec::new();
    for raw in base.split_inclusive('\n') {
        let c = classify(raw);
        if let Line::Header(h) = c {
            applicable = header_is_applicable(h);
        }
        lines.push((raw, c, applicable));
    }

    let applicable_gen: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, (_, c, a))| *a && matches!(c, Line::PcieGen { .. }))
        .map(|(i, _)| i)
        .collect();
    if let [i] = applicable_gen[..] {
        let correct = matches!(lines[i].1, Line::PcieGen { gen1: true });
        let overridden = lines[i + 1..]
            .iter()
            .any(|(_, c, _)| matches!(c, Line::PcieGen { .. } | Line::PcieGenCombined));
        if correct && !overridden {
            return base;
        }
    }

    let mut out = String::with_capacity(base.len() + 32);
    let mut last_header: Option<&str> = None;
    for (raw, c, a) in &lines {
        match c {
            Line::PcieGen { .. } if *a => continue,
            Line::Header(h) => last_header = Some(h),
            _ => {}
        }
        out.push_str(raw);
    }
    if last_header.is_some_and(|h| !header_is_applicable(h)) {
        out.push_str("[all]\n");
    }
    out.push_str(PCIE_GEN1_LINE);
    out.push('\n');
    out
}

/// Path of the boot partition's `config.txt` (boot partition is mounted at `/boot`, #65).
fn boot_config_path(mount_root: &Path) -> PathBuf {
    mount_root.join("boot/config.txt")
}

/// Ensure `<mount_root>/boot/config.txt` forces PCIe Gen1 (#69).
///
/// config.txt is owned by `rpi-firmware` (ADR-006), so a missing file is an
/// error rather than something to create.
async fn ensure_boot_config_pcie_gen1(mount_root: &Path) -> Result<()> {
    tracing::info!("Ensuring NVMe Boot partition config.txt forces PCIe Gen1");
    rewrite_boot_file_blocking(boot_config_path(mount_root), IfMissing::Error, |c| {
        ensure_pcie_gen1(c)
    })
    .await?;
    tracing::info!("✅ NVMe Boot partition config.txt updated");
    Ok(())
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
        boot_cmdline_path, classify, ensure_boot_config_pcie_gen1, ensure_pcie_gen1,
        header_is_applicable, rewrite_boot_file, rewrite_cmdline, root_device, write_boot_cmdline,
        IfMissing, Line, MountPoint, Partition, Rewritten, MUSIC_CHOWN_PATHS, TARGET_PACKAGES,
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

    use super::{
        configure_wifi, services_to_enable, wpa_supplicant_conf_text, ConfigureSystemAction,
        WifiAction, WifiWrite,
    };
    use crate::actions::Action;
    use crate::provisioning::types::{
        CompletedPartitionPlan, CompletedPartitionedDrives, ConfiguredFstab, ConfiguredSystem,
        DriveInfo, FormattedSystem, InstalledPackages, MountedPartitions, ValidatedDrives,
        ValidatedHardware,
    };
    use crate::types::{
        ByteSize, DevicePath, ExistingDataPolicy, Hostname, ProvisionConfig, SshPublicKey,
        UnitType, WifiSetup,
    };
    use wifi_config::{
        render_wpa_supplicant_conf, CountryCode, InstalledWifiConf, NetworkConfig, WifiPassphrase,
        WifiSsid, WPA_SERVICE_CONF_PATH, WPA_SUPPLICANT_CONF_PATH,
    };

    fn network() -> NetworkConfig {
        NetworkConfig::new(
            WifiSsid::new("HomeNet").expect("ssid"),
            &WifiPassphrase::new("correct horse battery").expect("pass"),
            CountryCode::new("se").expect("country"),
        )
    }

    fn form_wifi(network: Option<NetworkConfig>) -> WifiSetup {
        network.map_or(WifiSetup::None, WifiSetup::FromForm)
    }

    fn fstab(mount_root: &std::path::Path, network: Option<NetworkConfig>) -> ConfiguredFstab {
        fstab_with_policy(mount_root, form_wifi(network), ExistingDataPolicy::Preserve)
    }

    fn fstab_with_policy(
        mount_root: &std::path::Path,
        wifi: WifiSetup,
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
            wifi,
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
        system_with_wifi(dir, form_wifi(network), policy)
    }

    fn system_with_wifi(
        dir: &std::path::Path,
        wifi: WifiSetup,
        policy: ExistingDataPolicy,
    ) -> ConfiguredSystem {
        ConfiguredSystem {
            fstab_configured: fstab_with_policy(dir, wifi, policy),
        }
    }

    /// A staged conf with a hand-added `freq_list`, as the beacon would carry over.
    const CARRIED_CONF: &str = "country=SE\nctrl_interface=/run/wpa_supplicant\n\
        network={\n    ssid=\"HomeNet\"\n    psk=\"hunter2-secret\"\n    freq_list=5180 5200\n}\n";

    fn carried_over() -> WifiSetup {
        WifiSetup::CarriedOver(InstalledWifiConf::parse(CARRIED_CONF).expect("usable conf"))
    }

    /// Mark the root partition as formatted by stage 3.
    fn with_root_formatted(mut sys: ConfiguredSystem) -> ConfiguredSystem {
        use crate::provisioning::types::{PartitionFormatAction, PartitionSize};
        use crate::types::FilesystemType;
        let root = Partition {
            device: DevicePath::new("/dev/nvme0n1p2").expect("device"),
            mount_point: MountPoint::Root,
            size: PartitionSize::from_mb(512),
        };
        let mounted = &mut sys.fstab_configured.installed.mounted;
        mounted.formatted.format_actions = vec![PartitionFormatAction::WillFormat {
            device: root.device.clone(),
            label: root.label(),
            fs_type: FilesystemType::Ext4,
        }];
        mounted.partitions = vec![root];
        sys
    }

    #[test]
    fn carried_over_conf_is_written_verbatim_when_the_os_is_reinstalled() {
        let dir = tempfile::tempdir().expect("tempdir");
        configure_wifi(&system_with_wifi(
            dir.path(),
            carried_over(),
            ExistingDataPolicy::ReinstallOs,
        ))
        .expect("wifi");

        let conf = dir.path().join(WPA_SUPPLICANT_CONF_PATH);
        assert_eq!(std::fs::read_to_string(&conf).expect("read"), CARRIED_CONF);
        assert_eq!(mode_of(&conf), 0o600);
        let svc = std::fs::read_to_string(dir.path().join(WPA_SERVICE_CONF_PATH)).expect("svc");
        assert!(svc.contains("WPA_INTERFACE=wlan0"), "{svc}");
    }

    #[test]
    fn carried_over_conf_is_written_under_preserve_when_root_is_formatted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sys = with_root_formatted(system_with_wifi(
            dir.path(),
            carried_over(),
            ExistingDataPolicy::Preserve,
        ));
        configure_wifi(&sys).expect("wifi");
        assert_eq!(
            std::fs::read_to_string(dir.path().join(WPA_SUPPLICANT_CONF_PATH)).expect("read"),
            CARRIED_CONF
        );
    }

    #[test]
    fn carried_over_conf_leaves_a_kept_root_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let conf = dir.path().join(WPA_SUPPLICANT_CONF_PATH);
        std::fs::create_dir_all(conf.parent().expect("parent")).expect("mkdir");
        std::fs::write(&conf, "existing\n").expect("write");

        configure_wifi(&system_with_wifi(
            dir.path(),
            carried_over(),
            ExistingDataPolicy::Preserve,
        ))
        .expect("wifi");

        assert_eq!(std::fs::read(&conf).expect("read"), b"existing\n");
        assert!(!dir.path().join(WPA_SERVICE_CONF_PATH).exists());
    }

    #[test]
    fn form_wifi_wins_over_the_carried_over_conf_text() {
        let net = network();
        assert_eq!(
            wpa_supplicant_conf_text(WifiWrite::FromForm(&net)),
            render_wpa_supplicant_conf(&net)
        );
        let WifiSetup::CarriedOver(conf) = carried_over() else {
            unreachable!()
        };
        assert_eq!(
            wpa_supplicant_conf_text(WifiWrite::CarriedOver(&conf)),
            CARRIED_CONF
        );
    }

    #[test]
    fn carried_over_wifi_enables_wpa_supplicant_when_written() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sys = system_with_wifi(dir.path(), carried_over(), ExistingDataPolicy::ReinstallOs);
        assert!(matches!(sys.wifi_action(), WifiAction::Write(_)));
        assert!(services_to_enable(&sys.wifi_action()).contains(&"wpa_supplicant"));
    }

    #[test]
    fn kept_root_neither_writes_nor_enables_wifi() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sys = system_with_wifi(dir.path(), carried_over(), ExistingDataPolicy::Preserve);
        assert!(matches!(sys.wifi_action(), WifiAction::KeepExisting));
        assert!(!services_to_enable(&sys.wifi_action()).contains(&"wpa_supplicant"));
    }

    #[test]
    fn kept_root_with_carried_over_is_a_reformatted_write() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sys = with_root_formatted(system_with_wifi(
            dir.path(),
            carried_over(),
            ExistingDataPolicy::Preserve,
        ));
        assert!(matches!(sys.wifi_action(), WifiAction::Write(_)));
    }

    #[test]
    fn no_wifi_is_nothing_and_not_enabled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sys = system(dir.path(), None, ExistingDataPolicy::ReinstallOs);
        assert!(matches!(sys.wifi_action(), WifiAction::Nothing));
        assert!(!services_to_enable(&sys.wifi_action()).contains(&"wpa_supplicant"));
    }

    #[test]
    fn carried_over_summary_and_debug_never_leak_the_psk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sys = system_with_wifi(dir.path(), carried_over(), ExistingDataPolicy::ReinstallOs);
        let summary = sys.wifi_summary();
        assert!(summary.contains("carried over verbatim"), "{summary}");
        assert!(summary.contains("HomeNet"), "{summary}");
        assert!(!summary.contains("hunter2-secret"));
        assert!(!format!("{sys:?}").contains("hunter2-secret"));

        let kept = system_with_wifi(dir.path(), carried_over(), ExistingDataPolicy::Preserve);
        let text = kept.wifi_summary();
        assert!(text.contains("nothing is changed"), "{text}");
        assert!(text.contains("neither enabled nor disabled"), "{text}");
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
        let with = services_to_enable(&WifiAction::Write(WifiWrite::FromForm(&net)));
        let without = services_to_enable(&WifiAction::Nothing);
        assert!(with.contains(&"wpa_supplicant"));
        assert!(!without.contains(&"wpa_supplicant"));
        let filtered: Vec<_> = with
            .iter()
            .copied()
            .filter(|s| *s != "wpa_supplicant")
            .collect();
        assert_eq!(filtered, without);
    }

    #[test]
    fn services_to_enable_always_includes_acpid_and_klog() {
        let services = services_to_enable(&WifiAction::Nothing);
        assert!(services.contains(&"acpid"));
        assert!(services.contains(&"mdma-klog"));
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
        std::fs::create_dir_all(mount_root.join("boot")).expect("mkdir boot");

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

    // ── #69: PCIe Gen1 in config.txt ─────────────────────────────────────────

    fn gen_lines(s: &str) -> Vec<&str> {
        s.lines()
            .filter(|l| l.trim().starts_with("dtparam=pciex1_gen="))
            .collect()
    }

    #[test]
    fn ensure_pcie_gen1_adds_when_absent() {
        let out = ensure_pcie_gen1("dtparam=audio=on\n");
        assert_eq!(out, "dtparam=audio=on\ndtparam=pciex1_gen=1\n");
    }

    #[test]
    fn ensure_pcie_gen1_empty_input() {
        assert_eq!(ensure_pcie_gen1(""), "dtparam=pciex1_gen=1\n");
    }

    #[test]
    fn ensure_pcie_gen1_adds_missing_trailing_newline() {
        let out = ensure_pcie_gen1("dtparam=audio=on");
        assert_eq!(out, "dtparam=audio=on\ndtparam=pciex1_gen=1\n");
    }

    #[test]
    fn ensure_pcie_gen1_is_idempotent() {
        for input in [
            "",
            "dtparam=audio=on\n",
            "[pi4]\narm_boost=1\n",
            "[all]\ndtoverlay=x\n[pi4]\nfoo=1\n",
        ] {
            let once = ensure_pcie_gen1(input);
            assert_eq!(ensure_pcie_gen1(&once), once, "input: {input:?}");
            assert_eq!(gen_lines(&once), vec!["dtparam=pciex1_gen=1"]);
        }
    }

    #[test]
    fn ensure_pcie_gen1_replaces_other_values() {
        for v in ["2", "3"] {
            let input = format!("a=1\ndtparam=pciex1_gen={v}\nb=2\n");
            assert_eq!(ensure_pcie_gen1(&input), "a=1\nb=2\ndtparam=pciex1_gen=1\n");
        }
    }

    #[test]
    fn ensure_pcie_gen1_does_not_duplicate() {
        let input = "dtparam=pciex1_gen=1\nx=1\ndtparam=pciex1_gen=1\n";
        assert_eq!(ensure_pcie_gen1(input), "x=1\ndtparam=pciex1_gen=1\n");
    }

    #[test]
    fn ensure_pcie_gen1_pi5_after_other_filter_is_not_trusted() {
        assert_eq!(
            ensure_pcie_gen1("[gpio4=1]\n[pi5]\nx=1\n"),
            "[gpio4=1]\n[pi5]\nx=1\n[all]\ndtparam=pciex1_gen=1\n"
        );
    }

    #[test]
    fn ensure_pcie_gen1_keeps_correct_line_under_all_after_other_sections() {
        let input = "[pi4]\na=1\n[all]\ndtparam=pciex1_gen=1\nb=2\n";
        assert_eq!(ensure_pcie_gen1(input), input);
    }

    #[test]
    fn ensure_pcie_gen1_correct_and_wrong_in_different_regions_leaves_one() {
        let input = "dtparam=pciex1_gen=3\n[pi4]\nx=1\n[all]\ndtparam=pciex1_gen=1\n";
        assert_eq!(
            ensure_pcie_gen1(input),
            "[pi4]\nx=1\n[all]\ndtparam=pciex1_gen=1\n"
        );
    }

    #[test]
    fn ensure_pcie_gen1_none_and_board_type_sections_are_not_applicable() {
        for h in ["[none]", "[board-type=0x17]"] {
            let input = format!("{h}\ndtparam=pciex1_gen=1\n");
            assert_eq!(
                ensure_pcie_gen1(&input),
                format!("{h}\ndtparam=pciex1_gen=1\n[all]\ndtparam=pciex1_gen=1\n")
            );
        }
    }

    #[test]
    fn ensure_pcie_gen1_comma_combined_line_is_kept_and_ours_wins_after_it() {
        assert_eq!(
            ensure_pcie_gen1("dtparam=audio=on,pciex1_gen=3\n"),
            "dtparam=audio=on,pciex1_gen=3\ndtparam=pciex1_gen=1\n"
        );
        let settled = "dtparam=audio=on,pciex1_gen=3\ndtparam=pciex1_gen=1\n";
        assert_eq!(ensure_pcie_gen1(settled), settled);
        assert_eq!(
            ensure_pcie_gen1("dtparam=pciex1_gen=1\ndtparam=audio=on,pciex1_gen=3\n"),
            settled
        );
    }

    #[test]
    fn classify_lines() {
        assert_eq!(classify(" [pi5] \n"), Line::Header("[pi5]"));
        assert_eq!(
            classify("dtparam=pciex1_gen=1\n"),
            Line::PcieGen { gen1: true }
        );
        assert_eq!(
            classify("dtparam=pciex1_gen=2"),
            Line::PcieGen { gen1: false }
        );
        assert_eq!(classify("dtparam=a=on,pciex1_gen=1"), Line::PcieGenCombined);
        assert_eq!(classify("#dtparam=pciex1_gen=1"), Line::Other);
        assert_eq!(classify("dtparam=audio=on,i2c=on"), Line::Other);
        assert!(header_is_applicable("[all]"));
        assert!(!header_is_applicable("[pi5]"));
    }

    mod prop {
        use super::super::ensure_pcie_gen1;
        use proptest::prelude::*;

        const LINES: [&str; 14] = [
            "[all]",
            "[pi4]",
            "[pi5]",
            "[none]",
            "[gpio4=1]",
            "[pi4]  # comment",
            "[all] # comment",
            "dtparam=pciex1_gen=1",
            "dtparam=pciex1_gen=2",
            "dtparam=pciex1_gen=3",
            "dtparam=audio=on,pciex1_gen=3",
            "foo=1",
            "#dtparam=pciex1_gen=3",
            "",
        ];

        const GEN1: &str = "dtparam=pciex1_gen=1";

        fn arb_config() -> impl Strategy<Value = String> {
            (
                prop::collection::vec(prop::sample::select(LINES.to_vec()), 0..14),
                any::<bool>(),
            )
                .prop_map(|(v, trailing_newline)| {
                    let mut s = v.join("\n");
                    if trailing_newline {
                        s.push('\n');
                    }
                    s
                })
        }

        // ── Independent oracle: literal string matching only, valid because
        // every generated line comes from LINES (no surrounding whitespace). ──

        fn body(raw: &str) -> &str {
            raw.trim_end_matches('\n')
        }

        fn is_standalone_gen(l: &str) -> bool {
            l.starts_with("dtparam=pciex1_gen=")
        }

        fn is_any_gen(l: &str) -> bool {
            l.starts_with("dtparam=") && l.contains("pciex1_gen=")
        }

        /// (raw line, applicable at that line): applicable = top level or under
        /// `[all]` exactly, any other `[` line makes the region non-applicable.
        fn with_applicability(text: &str) -> Vec<(&str, bool)> {
            let mut applicable = true;
            text.split_inclusive('\n')
                .map(|raw| {
                    let l = body(raw);
                    if l.starts_with('[') {
                        applicable = l == "[all]";
                    }
                    (raw, applicable)
                })
                .collect()
        }

        /// The postcondition: exactly one applicable standalone gen line, it is
        /// `=1`, and it is the last gen line of any kind in the file.
        fn satisfied(text: &str) -> bool {
            let lines = with_applicability(text);
            let applicable_gen: Vec<&str> = lines
                .iter()
                .filter(|(raw, a)| *a && is_standalone_gen(body(raw)))
                .map(|(raw, _)| body(raw))
                .collect();
            let last_gen = lines
                .iter()
                .rev()
                .find(|(raw, _)| is_any_gen(body(raw)))
                .map(|(raw, a)| (*a, body(raw)));
            applicable_gen == [GEN1] && last_gen == Some((true, GEN1))
        }

        fn normalized(input: &str) -> String {
            let mut n = input.to_string();
            if !n.is_empty() && !n.ends_with('\n') {
                n.push('\n');
            }
            n
        }

        proptest! {
            #[test]
            fn result_satisfies_postcondition_and_is_idempotent(input in arb_config()) {
                let once = ensure_pcie_gen1(&input);
                prop_assert!(satisfied(&once), "not satisfied: {:?}", once);
                prop_assert_eq!(ensure_pcie_gen1(&once), once.clone());
            }

            #[test]
            fn preserves_everything_except_applicable_standalone_gen_lines(input in arb_config()) {
                let norm = normalized(&input);
                let out = ensure_pcie_gen1(&input);

                let kept: Vec<&str> = with_applicability(&norm)
                    .into_iter()
                    .filter(|(raw, a)| !(*a && is_standalone_gen(body(raw))))
                    .map(|(raw, _)| raw)
                    .collect();
                let last_header = kept.iter().rev().map(|r| body(r)).find(|l| l.starts_with('['));
                let suffix = if last_header.is_some_and(|h| h != "[all]") {
                    format!("[all]\n{GEN1}\n")
                } else {
                    format!("{GEN1}\n")
                };
                let filtered = kept.concat();

                prop_assert!(
                    out == norm || out == format!("{filtered}{suffix}"),
                    "norm: {:?}\nout: {:?}",
                    norm,
                    out
                );
            }

            #[test]
            fn already_satisfied_input_is_a_noop(input in arb_config()) {
                let norm = normalized(&input);
                if satisfied(&norm) {
                    prop_assert_eq!(ensure_pcie_gen1(&input), norm);
                }
            }
        }
    }

    #[test]
    fn classify_strips_trailing_comment_on_headers() {
        assert_eq!(
            classify("[pi4]  # comment\n"),
            Line::Header("[pi4]  # comment")
        );
        // A commented header is never equal to [all]: conservative either way.
        assert!(!header_is_applicable("[all] # c"));
        assert_eq!(classify("#[pi4]"), Line::Other);
    }

    #[test]
    fn ensure_pcie_gen1_commented_header_ends_applicable_region() {
        assert_eq!(
            ensure_pcie_gen1("[pi4]  # comment\ndtparam=pciex1_gen=3\n"),
            "[pi4]  # comment\ndtparam=pciex1_gen=3\n[all]\ndtparam=pciex1_gen=1\n"
        );
    }

    #[tokio::test]
    async fn write_boot_cmdline_fails_without_boot_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_boot_cmdline(dir.path(), "aa-01")
            .await
            .expect_err("missing boot/ must fail");
        assert!(!dir.path().join("boot").exists());
    }

    #[test]
    fn rewrite_boot_file_makes_no_backup_for_missing_or_empty_original() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("a.txt");
        let r = rewrite_boot_file(&missing, IfMissing::StartEmpty, |_| "x\n".into()).expect("a");
        assert_eq!(r, Rewritten::Written);
        assert!(!dir.path().join("a.txt.bak").exists());

        let empty = dir.path().join("b.txt");
        std::fs::write(&empty, "").expect("seed");
        let r = rewrite_boot_file(&empty, IfMissing::Error, |_| "x\n".into()).expect("b");
        assert_eq!(r, Rewritten::Written);
        assert!(!dir.path().join("b.txt.bak").exists());
    }

    #[tokio::test]
    async fn cmdline_reprovision_is_unchanged_and_backup_keeps_original() {
        let dir = tempfile::tempdir().expect("tempdir");
        let boot = dir.path().join("boot");
        std::fs::create_dir_all(&boot).expect("mkdir");
        let original = "console=tty1 root=/dev/mmcblk0p2 rootwait\n";
        std::fs::write(boot.join("cmdline.txt"), original).expect("seed");

        write_boot_cmdline(dir.path(), "aa-01")
            .await
            .expect("first");
        let first = std::fs::read_to_string(boot.join("cmdline.txt")).expect("read");
        assert_ne!(first, original);

        let r = rewrite_boot_file(&boot.join("cmdline.txt"), IfMissing::StartEmpty, |c| {
            rewrite_cmdline(c, "aa-01")
        })
        .expect("second");
        assert_eq!(r, Rewritten::Unchanged);

        write_boot_cmdline(dir.path(), "aa-01")
            .await
            .expect("third");
        assert_eq!(
            std::fs::read_to_string(boot.join("cmdline.txt")).expect("read"),
            first
        );
        assert_eq!(
            std::fs::read_to_string(boot.join("cmdline.txt.bak")).expect("bak"),
            original
        );
    }

    #[test]
    fn ensure_pcie_gen1_existing_line_unchanged_in_place() {
        let input = "dtparam=pciex1_gen=1\n[all]\nfoo=1\n";
        assert_eq!(ensure_pcie_gen1(input), input);
    }

    #[test]
    fn ensure_pcie_gen1_appends_all_section_after_pi4_section() {
        let input = "dtparam=audio=on\n[pi4]\narm_boost=1\n";
        let out = ensure_pcie_gen1(input);
        assert_eq!(
            out,
            "dtparam=audio=on\n[pi4]\narm_boost=1\n[all]\ndtparam=pciex1_gen=1\n"
        );
    }

    #[test]
    fn ensure_pcie_gen1_preserves_pi4_line_and_appends_all() {
        let input = "[pi4]\ndtparam=pciex1_gen=1\n";
        let out = ensure_pcie_gen1(input);
        assert_eq!(
            out,
            "[pi4]\ndtparam=pciex1_gen=1\n[all]\ndtparam=pciex1_gen=1\n"
        );
    }

    #[test]
    fn ensure_pcie_gen1_no_extra_header_when_last_section_is_all() {
        assert_eq!(
            ensure_pcie_gen1("[pi4]\na=1\n[all]\nb=2\n"),
            "[pi4]\na=1\n[all]\nb=2\ndtparam=pciex1_gen=1\n"
        );
        assert_eq!(
            ensure_pcie_gen1("[pi5]\nb=2\n"),
            "[pi5]\nb=2\n[all]\ndtparam=pciex1_gen=1\n"
        );
    }

    #[test]
    fn ensure_pcie_gen1_preserves_other_lines_and_comments() {
        let input = "# comment\n\ndtparam=audio=on\n#dtparam=pciex1_gen=3\ndtoverlay=vc4-kms-v3d\n[cm4]\notg_mode=1\n";
        let out = ensure_pcie_gen1(input);
        assert!(out.starts_with(input), "{out}");
        assert!(out.contains("#dtparam=pciex1_gen=3\n"));
        assert_eq!(gen_lines(&out), vec!["dtparam=pciex1_gen=1"]);
    }

    #[tokio::test]
    async fn ensure_boot_config_writes_and_is_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join("boot")).expect("mkdir");
        std::fs::write(root.join("boot/config.txt"), "[pi4]\nx=1\n").expect("seed");

        ensure_boot_config_pcie_gen1(root).await.expect("first");
        let first = std::fs::read_to_string(root.join("boot/config.txt")).expect("read");
        assert_eq!(first, "[pi4]\nx=1\n[all]\ndtparam=pciex1_gen=1\n");

        let mut names: Vec<String> = std::fs::read_dir(root.join("boot"))
            .expect("readdir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["config.txt", "config.txt.bak"]);

        ensure_boot_config_pcie_gen1(root).await.expect("second");
        let second = std::fs::read_to_string(root.join("boot/config.txt")).expect("read");
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn ensure_boot_config_missing_file_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("boot")).expect("mkdir");
        let err = ensure_boot_config_pcie_gen1(dir.path())
            .await
            .expect_err("missing config.txt must fail");
        assert!(err.to_string().contains("config.txt"), "{err}");
        assert!(!dir.path().join("boot/config.txt").exists());
    }

    #[test]
    fn rewrite_boot_file_backup_is_original_and_never_overwritten() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.txt");
        let bak = dir.path().join("config.txt.bak");
        std::fs::write(&path, "[pi4]\nx=1\n").expect("seed");

        let r = rewrite_boot_file(&path, IfMissing::Error, ensure_pcie_gen1).expect("first");
        assert_eq!(r, Rewritten::Written);
        assert_eq!(std::fs::read_to_string(&bak).expect("bak"), "[pi4]\nx=1\n");

        std::fs::write(&path, "[all]\ndtparam=pciex1_gen=3\n").expect("modify");
        let r = rewrite_boot_file(&path, IfMissing::Error, ensure_pcie_gen1).expect("second");
        assert_eq!(r, Rewritten::Written);
        assert_eq!(std::fs::read_to_string(&bak).expect("bak"), "[pi4]\nx=1\n");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "[all]\ndtparam=pciex1_gen=1\n"
        );

        let r = rewrite_boot_file(&path, IfMissing::Error, ensure_pcie_gen1).expect("third");
        assert_eq!(r, Rewritten::Unchanged);
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
