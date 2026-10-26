//! Pi 5 EEPROM configuration read/write.
//!
//! Provides idempotent config parse/rewrite logic and async wrappers around
//! `rpi-eeprom-config`.  Separates pure config manipulation (testable without
//! running on a Pi) from command invocation.

use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::process::Command;

/// BOOT_ORDER nibbles are read right to left: 1 = SD, 4 = USB, 6 = NVMe,
/// f = restart.  `0xf164` therefore boots USB → NVMe → SD.
/// See <https://www.raspberrypi.com/documentation/computers/raspberry-pi.html#BOOT_ORDER>
pub const BOOT_ORDER_NVME_FIRST: &str = "0xf164";

/// BOOT_ORDER nibbles are read right to left (1 = SD, 4 = USB, 6 = NVMe,
/// f = restart).  `0xf461` therefore boots SD → NVMe → USB.
/// Used for the service-mode recovery path so the Pi boots to beacon from SD.
pub const BOOT_ORDER_SD_FIRST: &str = "0xf461";

/// `PCIE_PROBE=1` is required for the bootloader to probe PCIe at all on Pi 5.
/// Without it the NVMe nibble in BOOT_ORDER is silently ignored.
pub const PCIE_PROBE: &str = "1";

/// Errors produced by this component.
#[derive(Debug, Error)]
pub enum EepromError {
    #[error("rpi-eeprom-config command failed to start: {0}")]
    CommandSpawn(#[source] std::io::Error),

    #[error("rpi-eeprom-config returned non-zero exit code: {0}")]
    CommandFailed(String),

    #[error("failed to write temporary EEPROM config file: {0}")]
    TmpWrite(#[source] std::io::Error),

    #[error(
        "rpi-eeprom-config --apply did not produce a staged file \
             (checked /boot/pieeprom.upd and /boot/firmware/pieeprom.upd)"
    )]
    NoStagedFile,

    #[error(
        "boot partition not mounted at /boot (or /boot/firmware) as vfat; \
         run migrate-bootfs first"
    )]
    BootNotMounted,

    #[error("cannot remove stale staged EEPROM file {}: {source}", path.display())]
    StaleStagedFile {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("EEPROM BOOT_ORDER verification failed: staged file {path} has unexpected config:\n{config}\nExpected BOOT_ORDER={expected}")]
    VerificationFailed {
        path: PathBuf,
        config: String,
        expected: String,
    },

    #[error("applying EEPROM config requires root or NOPASSWD sudo for rpi-eeprom-config (sudo -n failed)")]
    SudoRequired,
}

pub type Result<T> = std::result::Result<T, EepromError>;

/// Which device the Pi should try first at boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootTarget {
    /// SD → NVMe → USB (rescue: boots the beacon from SD).
    SdFirst,
    /// USB → NVMe → SD (normal operation).
    NvmeFirst,
}

impl BootTarget {
    /// The `BOOT_ORDER` value for this target.
    pub const fn boot_order(self) -> &'static str {
        match self {
            BootTarget::SdFirst => BOOT_ORDER_SD_FIRST,
            BootTarget::NvmeFirst => BOOT_ORDER_NVME_FIRST,
        }
    }

    /// Map a `BOOT_ORDER` value back to a known target (case-insensitive).
    pub fn from_boot_order(value: &str) -> Option<Self> {
        [BootTarget::SdFirst, BootTarget::NvmeFirst]
            .into_iter()
            .find(|t| t.boot_order().eq_ignore_ascii_case(value.trim()))
    }
}

/// Parsed representation of `rpi-eeprom-config` output.
///
/// Provides idempotency checks and rewrite logic separated from command
/// invocation so pure logic can be tested without a Pi.
pub struct EepromConfig {
    raw: String,
}

impl EepromConfig {
    /// Parse the raw text output of `rpi-eeprom-config`.
    pub fn parse(raw: &str) -> Self {
        Self {
            raw: raw.to_string(),
        }
    }

    /// Return the raw config text as a string slice.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Returns `true` if both `BOOT_ORDER` and `PCIE_PROBE` are already set to
    /// their NVMe-first values ([`BOOT_ORDER_NVME_FIRST`] and `PCIE_PROBE=1`).
    pub fn is_correct_for_nvme_first(&self) -> bool {
        self.is_correct_for(BootTarget::NvmeFirst)
    }

    /// Returns `true` if `BOOT_ORDER` equals the value for `target` and
    /// `PCIE_PROBE=1` is set.
    pub fn is_correct_for(&self, target: BootTarget) -> bool {
        let target_boot_order = format!("BOOT_ORDER={}", target.boot_order());
        let has_boot_order = self
            .raw
            .lines()
            .any(|line| line.trim() == target_boot_order);
        let has_pcie_probe = self
            .raw
            .lines()
            .any(|line| line.trim() == format!("PCIE_PROBE={PCIE_PROBE}"));
        has_boot_order && has_pcie_probe
    }

    /// The boot target the current `BOOT_ORDER` corresponds to, if it is one
    /// of the known values.
    pub fn boot_target(&self) -> Option<BootTarget> {
        self.get("BOOT_ORDER").and_then(BootTarget::from_boot_order)
    }

    /// Return a new config string with `BOOT_ORDER` for `target` and
    /// `PCIE_PROBE=1` set.  All other fields are preserved.
    pub fn with_boot_target(&self, target: BootTarget) -> String {
        let with_order = Self::parse(&self.with_key("BOOT_ORDER", target.boot_order()));
        with_order.with_key("PCIE_PROBE", PCIE_PROBE)
    }

    /// Return the trimmed value for `key` if it is present in the config.
    ///
    /// Looks for a line of the form `KEY=value` (leading/trailing whitespace on
    /// the line is ignored).  Returns `None` if no such line exists.
    ///
    /// # Examples
    ///
    /// ```
    /// # use rpi_eeprom::EepromConfig;
    /// let cfg = EepromConfig::parse("BOOT_ORDER=0xf164\nPCIE_PROBE=1\n");
    /// assert_eq!(cfg.get("BOOT_ORDER"), Some("0xf164"));
    /// assert_eq!(cfg.get("MISSING"), None);
    /// ```
    pub fn get<'a>(&'a self, key: &str) -> Option<&'a str> {
        let prefix = format!("{key}=");
        self.raw
            .lines()
            .find(|line| line.trim().starts_with(&prefix))
            .map(|line| line.trim().trim_start_matches(&prefix).trim_end())
    }

    /// Return a new config string with `KEY=value` set.
    ///
    /// Substitutes an existing `KEY=` line if present; appends one if the key
    /// is missing.  All other fields are preserved.
    fn with_key(&self, key: &str, value: &str) -> String {
        let prefix = format!("{key}=");
        let target_line = format!("{prefix}{value}");
        let mut found = false;
        let mut lines: Vec<String> = self
            .raw
            .lines()
            .map(|line| {
                if line.trim().starts_with(&prefix) {
                    found = true;
                    target_line.clone()
                } else {
                    line.to_string()
                }
            })
            .collect();
        if !found {
            lines.push(target_line);
        }
        let mut result = lines.join("\n");
        if !result.ends_with('\n') {
            result.push('\n');
        }
        result
    }

    /// Return a new config string with `BOOT_ORDER=`[`BOOT_ORDER_NVME_FIRST`]
    /// and `PCIE_PROBE=`[`PCIE_PROBE`] set.  Shorthand for
    /// `with_boot_target(BootTarget::NvmeFirst)`.
    ///
    /// `PCIE_PROBE=1` is required in addition to BOOT_ORDER because without it
    /// the Pi 5 bootloader does not probe PCIe at all, making the NVMe nibble
    /// in BOOT_ORDER a no-op on freshly provisioned hardware.
    pub fn with_correct_eeprom_config(&self) -> String {
        self.with_boot_target(BootTarget::NvmeFirst)
    }
}

/// Read the current Pi 5 EEPROM config by running `rpi-eeprom-config`.
pub async fn read_current_eeprom_config() -> Result<EepromConfig> {
    let output = Command::new("rpi-eeprom-config")
        .output()
        .await
        .map_err(EepromError::CommandSpawn)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(EepromError::CommandFailed(format!(
            "rpi-eeprom-config read failed: {stderr}"
        )));
    }

    let raw = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(EepromConfig::parse(&raw))
}

/// Apply a new EEPROM config string by writing it to a temp file and running
/// `rpi-eeprom-config --apply <file>`.
///
/// If the effective UID is not root the command is invoked via `sudo -n` so
/// that non-root callers (e.g. mdma-console) can apply config when the
/// NOPASSWD sudoers fragment for `rpi-eeprom-config` is installed (Phase 1B).
/// If `sudo -n` is not authorised [`EepromError::SudoRequired`] is returned
/// with a clear message.
pub async fn apply_eeprom_config(config: &str) -> Result<()> {
    // `--apply` writes into the bootfs; refuse before it can stage onto the
    // root filesystem where nothing would ever read it.
    if mounted_staged_dirs().is_empty() {
        return Err(EepromError::BootNotMounted);
    }

    // A stale pieeprom.upd from an earlier run must not be mistaken for the
    // result of this apply.
    remove_stale_staged_in(&staged_dirs())?;

    // O_EXCL, mode 0600, unpredictable name; deleted when dropped.
    let mut tmp = tempfile::NamedTempFile::new().map_err(EepromError::TmpWrite)?;
    std::io::Write::write_all(&mut tmp, config.as_bytes()).map_err(EepromError::TmpWrite)?;
    std::io::Write::flush(&mut tmp).map_err(EepromError::TmpWrite)?;
    let tmp_path = tmp.path();

    let output = if libc_euid() == 0 {
        Command::new("rpi-eeprom-config")
            .args(["--apply"])
            .arg(tmp_path)
            .output()
            .await
            .map_err(EepromError::CommandSpawn)?
    } else {
        let out = Command::new("sudo")
            .args(["-n", "rpi-eeprom-config", "--apply"])
            .arg(tmp_path)
            .output()
            .await
            .map_err(EepromError::CommandSpawn)?;
        // sudo -n exits 1 and writes "sudo: a password is required" to stderr
        // when authorization is missing — surface a clear error in that case.
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            if stderr.contains("password") || stderr.contains("not allowed") {
                return Err(EepromError::SudoRequired);
            }
            return Err(EepromError::CommandFailed(format!(
                "sudo rpi-eeprom-config --apply failed: {stderr}"
            )));
        }
        return Ok(());
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(EepromError::CommandFailed(format!(
            "rpi-eeprom-config --apply failed: {stderr}"
        )));
    }

    Ok(())
}

/// Locate the staged EEPROM update file written by `rpi-eeprom-config --apply`.
///
/// `--apply` writes `pieeprom.upd` to the bootfs.  `/boot` is checked first
/// (the boot partition mount point since #65), then `/boot/firmware`.
/// [`apply_eeprom_config`] deletes stale candidates before applying, so
/// whatever exists afterwards was produced by that apply.
///
/// Only a file in a directory that is a vfat mount point is accepted: a file
/// on the root filesystem under an unmounted `/boot` is never read by the
/// bootloader.
pub fn find_staged_eeprom_file() -> Option<PathBuf> {
    find_staged_in(&mounted_staged_dirs())
}

const STAGED_FILE_NAME: &str = "pieeprom.upd";

fn staged_dirs() -> [PathBuf; 2] {
    [PathBuf::from("/boot"), PathBuf::from("/boot/firmware")]
}

/// Mount points of vfat filesystems listed in `/proc/mounts` content.
///
/// Each line is `device mountpoint fstype options dump pass`.
fn vfat_mount_points(proc_mounts: &str) -> Vec<PathBuf> {
    proc_mounts
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let _device = fields.next()?;
            let mount_point = fields.next()?;
            let fs_type = fields.next()?;
            (fs_type == "vfat").then(|| PathBuf::from(mount_point))
        })
        .collect()
}

/// The candidate directories that are vfat mount points. An update staged in
/// any other directory sits on the root filesystem, where the bootloader never
/// reads it.
fn mounted_boot_dirs(candidates: &[PathBuf], proc_mounts: &str) -> Vec<PathBuf> {
    let mounts = vfat_mount_points(proc_mounts);
    candidates
        .iter()
        .filter(|dir| mounts.contains(dir))
        .cloned()
        .collect()
}

/// Staging directories that are currently mounted vfat. An unreadable
/// `/proc/mounts` yields none (fail closed).
fn mounted_staged_dirs() -> Vec<PathBuf> {
    let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
    mounted_boot_dirs(&staged_dirs(), &mounts)
}

fn find_staged_in(dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .map(|d| d.join(STAGED_FILE_NAME))
        .find(|p| p.exists())
}

fn remove_stale_staged_in(dirs: &[PathBuf]) -> Result<()> {
    for path in dirs.iter().map(|d| d.join(STAGED_FILE_NAME)) {
        match std::fs::remove_file(&path) {
            Ok(()) => tracing::info!("Removed stale staged EEPROM file {}", path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(EepromError::StaleStagedFile { path, source }),
        }
    }
    Ok(())
}

/// Verify the staged EEPROM config at `staged_path` by extracting its embedded
/// config with `rpi-eeprom-config <file>` and checking BOOT_ORDER.
///
/// This is the correct post-apply verification: the live EEPROM is not yet
/// re-flashed, so `rpi-eeprom-config` (no args) still returns the OLD value.
/// Only reading the staged file shows what will be applied after next reboot.
pub async fn verify_staged_eeprom_boot_order(staged_path: &Path) -> Result<()> {
    verify_staged_eeprom_boot_target(staged_path, BootTarget::NvmeFirst).await
}

/// Like [`verify_staged_eeprom_boot_order`] but checks the staged file against
/// an arbitrary [`BootTarget`].
pub async fn verify_staged_eeprom_boot_target(
    staged_path: &Path,
    target: BootTarget,
) -> Result<()> {
    let output = Command::new("rpi-eeprom-config")
        .arg(staged_path)
        .output()
        .await
        .map_err(EepromError::CommandSpawn)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(EepromError::CommandFailed(format!(
            "rpi-eeprom-config {} failed: {stderr}",
            staged_path.display()
        )));
    }

    let staged_raw = String::from_utf8_lossy(&output.stdout).to_string();
    let staged = EepromConfig::parse(&staged_raw);
    if !staged.is_correct_for(target) {
        return Err(EepromError::VerificationFailed {
            path: staged_path.to_path_buf(),
            config: staged_raw,
            expected: target.boot_order().to_string(),
        });
    }

    tracing::info!(
        "Staged EEPROM file {} verified: BOOT_ORDER={}",
        staged_path.display(),
        target.boot_order()
    );
    Ok(())
}

/// Stage an EEPROM update that sets the boot order for `target` (keeping
/// `PCIE_PROBE=1`), then verify the staged file.  Returns the staged file path.
///
/// The change only takes effect on the next reboot.  Always re-applies, even
/// if the live EEPROM already matches, because a previously staged update for
/// the other target may still be pending.
pub async fn stage_boot_target(target: BootTarget) -> Result<PathBuf> {
    let current = read_current_eeprom_config().await?;
    apply_eeprom_config(&current.with_boot_target(target)).await?;
    let staged_path = find_staged_eeprom_file().ok_or(EepromError::NoStagedFile)?;
    verify_staged_eeprom_boot_target(&staged_path, target).await?;
    Ok(staged_path)
}

/// Thin FFI shim — returns the effective UID without pulling in the `libc` crate.
///
/// Only used internally to decide whether to invoke via `sudo -n`.
#[cfg(unix)]
fn libc_euid() -> u32 {
    // SAFETY: geteuid() is always safe to call.
    unsafe { libc_geteuid() }
}

extern "C" {
    fn geteuid() -> u32;
}

#[inline(always)]
unsafe fn libc_geteuid() -> u32 {
    geteuid()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- EepromConfig::get ---------------------------------------------------

    #[test]
    fn get_returns_value_for_present_key() {
        let cfg = EepromConfig::parse("BOOT_ORDER=0xf164\nPCIE_PROBE=1\n");
        assert_eq!(cfg.get("BOOT_ORDER"), Some("0xf164"));
    }

    #[test]
    fn get_returns_none_for_missing_key() {
        let cfg = EepromConfig::parse("BOOT_ORDER=0xf164\nPCIE_PROBE=1\n");
        assert_eq!(cfg.get("MISSING_KEY"), None);
    }

    #[test]
    fn get_trims_trailing_whitespace_from_value() {
        let cfg = EepromConfig::parse("BOOT_ORDER=0xf164   \nPCIE_PROBE=1\n");
        assert_eq!(cfg.get("BOOT_ORDER"), Some("0xf164"));
    }

    #[test]
    fn get_works_in_multiline_config() {
        let cfg = EepromConfig::parse(
            "BOOT_UART=0\nPOWER_OFF_ON_HALT=0\nBOOT_ORDER=0xf164\nPCIE_PROBE=1\nNET_INSTALL_AT_POWER_ON=1\n",
        );
        assert_eq!(cfg.get("BOOT_ORDER"), Some("0xf164"));
        assert_eq!(cfg.get("PCIE_PROBE"), Some("1"));
        assert_eq!(cfg.get("BOOT_UART"), Some("0"));
    }

    #[test]
    fn eeprom_config_parse_detects_correct_when_both_keys_present() {
        let config = "BOOT_ORDER=0xf164\nPCIE_PROBE=1\nBOOT_UART=1\nNET_INSTALL_AT_POWER_ON=1\n";
        let parsed = EepromConfig::parse(config);
        assert!(
            parsed.is_correct_for_nvme_first(),
            "should detect config with correct BOOT_ORDER and PCIE_PROBE=1 as already correct"
        );
    }

    #[test]
    fn eeprom_config_parse_not_correct_when_pcie_probe_missing() {
        let config = "BOOT_ORDER=0xf164\nBOOT_UART=1\nNET_INSTALL_AT_POWER_ON=1\n";
        let parsed = EepromConfig::parse(config);
        assert!(
            !parsed.is_correct_for_nvme_first(),
            "should not be correct when PCIE_PROBE=1 is missing"
        );
    }

    #[test]
    fn eeprom_config_parse_detects_wrong_boot_order() {
        let config = "BOOT_ORDER=0xf461\nPCIE_PROBE=1\nBOOT_UART=1\n";
        let parsed = EepromConfig::parse(config);
        assert!(
            !parsed.is_correct_for_nvme_first(),
            "0xf461 should not be detected as correct for NVMe-first"
        );
    }

    #[test]
    fn eeprom_config_rewrite_substitutes_boot_order() {
        let config = "BOOT_ORDER=0xf461\nBOOT_UART=1\nNET_INSTALL_AT_POWER_ON=1\n";
        let parsed = EepromConfig::parse(config);
        let new_config = parsed.with_correct_eeprom_config();
        assert!(
            new_config.contains("BOOT_ORDER=0xf164"),
            "expected 0xf164 in rewritten config, got: {new_config}"
        );
        assert!(
            new_config.contains("BOOT_UART=1"),
            "BOOT_UART=1 should be preserved"
        );
        assert!(
            !new_config.contains("BOOT_ORDER=0xf461"),
            "old BOOT_ORDER should be gone"
        );
    }

    #[test]
    fn eeprom_config_appends_boot_order_when_missing() {
        let config = "BOOT_UART=1\nNET_INSTALL_AT_POWER_ON=1\n";
        let parsed = EepromConfig::parse(config);
        let new_config = parsed.with_correct_eeprom_config();
        assert!(
            new_config.contains("BOOT_ORDER=0xf164"),
            "expected BOOT_ORDER=0xf164 appended, got: {new_config}"
        );
        assert!(
            new_config.contains("BOOT_UART=1"),
            "BOOT_UART=1 should be preserved"
        );
    }

    #[test]
    fn eeprom_config_sets_pcie_probe_when_missing() {
        let config = "BOOT_ORDER=0xf164\nBOOT_UART=1\n";
        let parsed = EepromConfig::parse(config);
        let new_config = parsed.with_correct_eeprom_config();
        assert!(
            new_config.contains("PCIE_PROBE=1"),
            "expected PCIE_PROBE=1 appended when missing, got: {new_config}"
        );
    }

    #[test]
    fn eeprom_config_replaces_pcie_probe_wrong_value() {
        let config = "BOOT_ORDER=0xf164\nPCIE_PROBE=0\nBOOT_UART=1\n";
        let parsed = EepromConfig::parse(config);
        let new_config = parsed.with_correct_eeprom_config();
        assert!(
            new_config.contains("PCIE_PROBE=1"),
            "expected PCIE_PROBE=0 replaced with PCIE_PROBE=1, got: {new_config}"
        );
        assert!(
            !new_config.contains("PCIE_PROBE=0"),
            "old PCIE_PROBE=0 should be gone, got: {new_config}"
        );
    }

    #[test]
    fn eeprom_config_preserves_pcie_probe_already_correct() {
        let config = "BOOT_ORDER=0xf164\nPCIE_PROBE=1\nBOOT_UART=1\n";
        let parsed = EepromConfig::parse(config);
        let new_config = parsed.with_correct_eeprom_config();
        let pcie_count = new_config.matches("PCIE_PROBE=1").count();
        assert_eq!(
            pcie_count, 1,
            "PCIE_PROBE=1 should appear exactly once, got: {new_config}"
        );
    }

    #[test]
    fn eeprom_config_sets_both_boot_order_and_pcie_probe_when_both_missing() {
        let config = "BOOT_UART=1\n";
        let parsed = EepromConfig::parse(config);
        let new_config = parsed.with_correct_eeprom_config();
        assert!(
            new_config.contains("BOOT_ORDER=0xf164"),
            "expected BOOT_ORDER=0xf164 appended, got: {new_config}"
        );
        assert!(
            new_config.contains("PCIE_PROBE=1"),
            "expected PCIE_PROBE=1 appended, got: {new_config}"
        );
        assert!(
            new_config.contains("BOOT_UART=1"),
            "BOOT_UART=1 should be preserved, got: {new_config}"
        );
    }

    #[test]
    fn with_boot_order_substitutes_existing() {
        let config = "BOOT_ORDER=0xf164\nBOOT_UART=1\n";
        let parsed = EepromConfig::parse(config);
        let new_config = parsed.with_key("BOOT_ORDER", BOOT_ORDER_SD_FIRST);
        assert!(
            new_config.contains("BOOT_ORDER=0xf461"),
            "expected SD-first order in result, got: {new_config}"
        );
        assert!(
            !new_config.contains("BOOT_ORDER=0xf164"),
            "old NVMe-first order should be gone"
        );
    }

    #[test]
    fn with_boot_order_appends_when_missing() {
        let config = "BOOT_UART=1\n";
        let parsed = EepromConfig::parse(config);
        let new_config = parsed.with_key("BOOT_ORDER", BOOT_ORDER_SD_FIRST);
        assert!(
            new_config.contains("BOOT_ORDER=0xf461"),
            "expected SD-first order appended, got: {new_config}"
        );
    }

    #[test]
    fn with_pcie_probe_substitutes_existing() {
        let config = "PCIE_PROBE=0\nBOOT_UART=1\n";
        let parsed = EepromConfig::parse(config);
        let new_config = parsed.with_key("PCIE_PROBE", "1");
        assert!(new_config.contains("PCIE_PROBE=1"), "got: {new_config}");
        assert!(!new_config.contains("PCIE_PROBE=0"), "got: {new_config}");
    }

    #[test]
    fn boot_order_comments_match_constants() {
        assert_eq!(BootTarget::SdFirst.boot_order(), "0xf461");
        assert_eq!(BootTarget::NvmeFirst.boot_order(), "0xf164");
    }

    #[test]
    fn from_boot_order_maps_known_values() {
        assert_eq!(
            BootTarget::from_boot_order("0xf461"),
            Some(BootTarget::SdFirst)
        );
        assert_eq!(
            BootTarget::from_boot_order("0xF164"),
            Some(BootTarget::NvmeFirst)
        );
        assert_eq!(BootTarget::from_boot_order("0xf14"), None);
    }

    #[test]
    fn boot_target_reads_current_config() {
        let cfg = EepromConfig::parse("BOOT_UART=1\nBOOT_ORDER=0xf461\nPCIE_PROBE=1\n");
        assert_eq!(cfg.boot_target(), Some(BootTarget::SdFirst));
        let unknown = EepromConfig::parse("BOOT_ORDER=0xf41\n");
        assert_eq!(unknown.boot_target(), None);
        let missing = EepromConfig::parse("BOOT_UART=1\n");
        assert_eq!(missing.boot_target(), None);
    }

    #[test]
    fn with_boot_target_sd_first_sets_order_and_keeps_pcie_probe() {
        let cfg = EepromConfig::parse("BOOT_ORDER=0xf164\nPCIE_PROBE=0\nBOOT_UART=1\n");
        let new_config = cfg.with_boot_target(BootTarget::SdFirst);
        let parsed = EepromConfig::parse(&new_config);
        assert!(parsed.is_correct_for(BootTarget::SdFirst), "{new_config}");
        assert!(!parsed.is_correct_for(BootTarget::NvmeFirst));
        assert_eq!(parsed.get("BOOT_UART"), Some("1"));
        assert_eq!(new_config.matches("PCIE_PROBE=").count(), 1);
    }

    #[test]
    fn with_boot_target_nvme_first_matches_legacy_rewrite() {
        let config = "BOOT_ORDER=0xf461\nBOOT_UART=1\n";
        let cfg = EepromConfig::parse(config);
        assert_eq!(
            cfg.with_boot_target(BootTarget::NvmeFirst),
            cfg.with_correct_eeprom_config()
        );
    }

    #[test]
    fn with_pcie_probe_appends_when_missing() {
        let config = "BOOT_UART=1\n";
        let parsed = EepromConfig::parse(config);
        let new_config = parsed.with_key("PCIE_PROBE", "1");
        assert!(new_config.contains("PCIE_PROBE=1"), "got: {new_config}");
    }

    #[test]
    fn staged_file_search_prefers_boot_over_boot_firmware() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("boot");
        let second = dir.path().join("firmware");
        for d in [&first, &second] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("pieeprom.upd"), "x").unwrap();
        }
        assert_eq!(
            find_staged_in(&[first.clone(), second.clone()]),
            Some(first.join("pieeprom.upd"))
        );
        std::fs::remove_file(first.join("pieeprom.upd")).unwrap();
        assert_eq!(
            find_staged_in(&[first, second.clone()]),
            Some(second.join("pieeprom.upd"))
        );
    }

    const MOUNTS: &str = "\
/dev/mmcblk0p2 / ext4 rw,relatime 0 0
proc /proc proc rw,nosuid 0 0
/dev/nvme0n1p1 /boot vfat rw,relatime,fmask=0022 0 0
";

    #[test]
    fn vfat_mount_points_lists_only_vfat() {
        assert_eq!(vfat_mount_points(MOUNTS), vec![PathBuf::from("/boot")]);
        assert_eq!(vfat_mount_points(""), Vec::<PathBuf>::new());
        assert_eq!(vfat_mount_points("garbage\n"), Vec::<PathBuf>::new());
    }

    #[test]
    fn staged_dirs_must_be_vfat_mount_points() {
        let candidates = [PathBuf::from("/boot"), PathBuf::from("/boot/firmware")];
        assert_eq!(
            mounted_boot_dirs(&candidates, MOUNTS),
            vec![PathBuf::from("/boot")]
        );
        // /boot is just a directory on the root filesystem.
        let unmounted = "/dev/mmcblk0p2 / ext4 rw 0 0\n";
        assert_eq!(
            mounted_boot_dirs(&candidates, unmounted),
            Vec::<PathBuf>::new()
        );
        // Mounted, but not vfat.
        let wrong_fs = "/dev/nvme0n1p1 /boot ext4 rw 0 0\n";
        assert_eq!(
            mounted_boot_dirs(&candidates, wrong_fs),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn boot_not_mounted_error_points_at_migrate_bootfs() {
        assert!(EepromError::BootNotMounted
            .to_string()
            .contains("run migrate-bootfs first"));
    }

    #[test]
    fn remove_stale_staged_deletes_all_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("pieeprom.upd"), "stale").unwrap();
        remove_stale_staged_in(&[a.clone(), dir.path().join("missing")]).unwrap();
        assert_eq!(find_staged_in(&[a]), None);
    }
}
