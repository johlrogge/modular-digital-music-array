//! Pi 5 EEPROM configuration read/write.
//!
//! Provides idempotent config parse/rewrite logic and async wrappers around
//! `rpi-eeprom-config`.  Separates pure config manipulation (testable without
//! running on a Pi) from command invocation.

use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::process::Command;

/// BOOT_ORDER nibbles: USB (1) → NVMe (6) → SD (4) → restart (f).
/// See <https://www.raspberrypi.com/documentation/computers/raspberry-pi.html#BOOT_ORDER>
pub const BOOT_ORDER_NVME_FIRST: &str = "0xf164";

/// BOOT_ORDER nibbles: SD (4) → USB (1) → NVMe (6) → restart (f).
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
             (checked /boot/firmware/pieeprom.upd and /boot/pieeprom.upd)"
    )]
    NoStagedFile,

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
        let target_boot_order = format!("BOOT_ORDER={BOOT_ORDER_NVME_FIRST}");
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

    /// Return a new config string with `BOOT_ORDER` set to `value`.
    ///
    /// Substitutes an existing `BOOT_ORDER=` line if present; appends one
    /// if the key is missing.  All other fields are preserved.
    pub fn with_boot_order(&self, value: &str) -> String {
        let target_line = format!("BOOT_ORDER={value}");
        let mut found = false;
        let mut lines: Vec<String> = self
            .raw
            .lines()
            .map(|line| {
                if line.trim().starts_with("BOOT_ORDER=") {
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

    /// Return a new config string with `PCIE_PROBE` set to `value`.
    ///
    /// Substitutes an existing `PCIE_PROBE=` line if present; appends one
    /// if the key is missing.  All other fields are preserved.
    pub fn with_pcie_probe(&self, value: &str) -> String {
        let target_line = format!("PCIE_PROBE={value}");
        let mut found = false;
        let mut lines: Vec<String> = self
            .raw
            .lines()
            .map(|line| {
                if line.trim().starts_with("PCIE_PROBE=") {
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

    /// Return a new config string with both `BOOT_ORDER=`[`BOOT_ORDER_NVME_FIRST`]
    /// and `PCIE_PROBE=`[`PCIE_PROBE`] set.
    ///
    /// For each key: substitutes an existing `KEY=` line if present; appends
    /// one if the key is missing entirely.  All other fields are preserved.
    ///
    /// `PCIE_PROBE=1` is required in addition to BOOT_ORDER because without it
    /// the Pi 5 bootloader does not probe PCIe at all, making the NVMe nibble
    /// in BOOT_ORDER a no-op on freshly provisioned hardware.
    pub fn with_correct_eeprom_config(&self) -> String {
        let boot_order_line = format!("BOOT_ORDER={BOOT_ORDER_NVME_FIRST}");
        let pcie_probe_line = format!("PCIE_PROBE={PCIE_PROBE}");

        let mut found_boot_order = false;
        let mut found_pcie_probe = false;

        let mut lines: Vec<String> = self
            .raw
            .lines()
            .map(|line| {
                if line.trim().starts_with("BOOT_ORDER=") {
                    found_boot_order = true;
                    boot_order_line.clone()
                } else if line.trim().starts_with("PCIE_PROBE=") {
                    found_pcie_probe = true;
                    pcie_probe_line.clone()
                } else {
                    line.to_string()
                }
            })
            .collect();

        if !found_boot_order {
            lines.push(boot_order_line);
        }
        if !found_pcie_probe {
            lines.push(pcie_probe_line);
        }

        let mut result = lines.join("\n");
        if !result.ends_with('\n') {
            result.push('\n');
        }
        result
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
    let tmp_path = "/tmp/mdma-eeprom-config.txt";
    tokio::fs::write(tmp_path, config)
        .await
        .map_err(EepromError::TmpWrite)?;

    let output = if libc_euid() == 0 {
        Command::new("rpi-eeprom-config")
            .args(["--apply", tmp_path])
            .output()
            .await
            .map_err(EepromError::CommandSpawn)?
    } else {
        let out = Command::new("sudo")
            .args(["-n", "rpi-eeprom-config", "--apply", tmp_path])
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
/// `--apply` writes `pieeprom.upd` to the bootfs (typically `/boot/firmware/`
/// or `/boot/`).  Returns the path of the first candidate that exists, or
/// `None` if neither exists.
pub fn find_staged_eeprom_file() -> Option<PathBuf> {
    let candidates = [
        PathBuf::from("/boot/firmware/pieeprom.upd"),
        PathBuf::from("/boot/pieeprom.upd"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

/// Verify the staged EEPROM config at `staged_path` by extracting its embedded
/// config with `rpi-eeprom-config <file>` and checking BOOT_ORDER.
///
/// This is the correct post-apply verification: the live EEPROM is not yet
/// re-flashed, so `rpi-eeprom-config` (no args) still returns the OLD value.
/// Only reading the staged file shows what will be applied after next reboot.
pub async fn verify_staged_eeprom_boot_order(staged_path: &Path) -> Result<()> {
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
    if !staged.is_correct_for_nvme_first() {
        return Err(EepromError::VerificationFailed {
            path: staged_path.to_path_buf(),
            config: staged_raw,
            expected: BOOT_ORDER_NVME_FIRST.to_string(),
        });
    }

    tracing::info!(
        "Staged EEPROM file {} verified: BOOT_ORDER={}",
        staged_path.display(),
        BOOT_ORDER_NVME_FIRST
    );
    Ok(())
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
        let new_config = parsed.with_boot_order(BOOT_ORDER_SD_FIRST);
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
        let new_config = parsed.with_boot_order(BOOT_ORDER_SD_FIRST);
        assert!(
            new_config.contains("BOOT_ORDER=0xf461"),
            "expected SD-first order appended, got: {new_config}"
        );
    }

    #[test]
    fn with_pcie_probe_substitutes_existing() {
        let config = "PCIE_PROBE=0\nBOOT_UART=1\n";
        let parsed = EepromConfig::parse(config);
        let new_config = parsed.with_pcie_probe("1");
        assert!(new_config.contains("PCIE_PROBE=1"), "got: {new_config}");
        assert!(!new_config.contains("PCIE_PROBE=0"), "got: {new_config}");
    }

    #[test]
    fn with_pcie_probe_appends_when_missing() {
        let config = "BOOT_UART=1\n";
        let parsed = EepromConfig::parse(config);
        let new_config = parsed.with_pcie_probe("1");
        assert!(new_config.contains("PCIE_PROBE=1"), "got: {new_config}");
    }
}
