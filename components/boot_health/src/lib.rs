//! Boot health: can this unit safely reboot into its installed kernel?
//!
//! Issue #65: the NVMe boot partition (p1) used to be mounted at
//! `/boot/firmware` while Void's `rpi-kernel` installs into `/boot`. After a
//! package upgrade the firmware booted a stale kernel from p1 whose modules
//! were gone. `rpi-kernel` ships no xbps hooks, so a runtime check is the guard.
//!
//! The verdict ([`assess`]) is pure and fixture-tested; [`gather`] is the thin
//! I/O layer that collects the [`BootFacts`] from the live system.

pub mod fstab;

use serde::{Deserialize, Serialize};
#[cfg(feature = "io")]
use std::path::Path;

pub use fstab::{migrate_fstab, FstabError};

pub const BOOT_MOUNT: &str = "/boot";
pub const BOOT_FSTYPE: &str = "vfat";
pub const KERNEL_PKG: &str = "rpi-kernel";
/// File name of the kernel image on the boot partition.
pub const KERNEL_IMAGE_NAME: &str = "kernel8.img";
pub const KERNEL_IMAGE: &str = "/boot/kernel8.img";
pub const MODULES_DIR: &str = "/usr/lib/modules";

/// Verdict on whether the unit can reboot safely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state")]
pub enum BootHealth {
    /// Boot partition, kernel image and modules agree with the installed kernel.
    Healthy,
    /// A newer kernel is installed and consistent; rebooting will pick it up.
    RebootPending { running: String, installed: String },
    /// `/boot` is not the vfat boot partition.
    BootfsNotMounted,
    /// `/boot/kernel8.img` does not match the installed `rpi-kernel` package.
    KernelImageStale { pkg: String },
    /// The installed kernel's modules are missing from `/usr/lib/modules`.
    ModulesMissing { version: String },
}

impl BootHealth {
    /// A reboot is only safe when healthy or merely waiting for the reboot.
    pub fn allows_reboot(&self) -> bool {
        matches!(self, Self::Healthy | Self::RebootPending { .. })
    }
}

impl std::fmt::Display for BootHealth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Healthy => write!(f, "healthy"),
            Self::RebootPending { running, installed } => {
                write!(
                    f,
                    "reboot pending (running {running}, installed {installed})"
                )
            }
            Self::BootfsNotMounted => {
                write!(f, "{BOOT_MOUNT} is not the {BOOT_FSTYPE} boot partition")
            }
            Self::KernelImageStale { pkg } => {
                write!(f, "{KERNEL_IMAGE} does not match installed {pkg}")
            }
            Self::ModulesMissing { version } => {
                write!(f, "{MODULES_DIR}/{version} is missing")
            }
        }
    }
}

/// Plain data describing the system, gathered by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootFacts {
    /// Filesystem type mounted at `/boot`, if anything is.
    pub boot_fstype: Option<String>,
    /// `xbps-pkgdb rpi-kernel` reported a problem with `/boot/kernel8.img`.
    pub kernel_image_stale: bool,
    /// Installed `rpi-kernel` version, e.g. `6.12.95_1`.
    pub installed_kernel: String,
    /// Entries of `/usr/lib/modules`.
    pub modules: Vec<String>,
    /// `uname -r`.
    pub running_kernel: String,
}

/// Pure verdict. Checks are ordered from most to least severe.
pub fn assess(facts: &BootFacts) -> BootHealth {
    if facts.boot_fstype.as_deref() != Some(BOOT_FSTYPE) {
        return BootHealth::BootfsNotMounted;
    }
    if facts.kernel_image_stale {
        return BootHealth::KernelImageStale {
            pkg: format!("{KERNEL_PKG}-{}", facts.installed_kernel),
        };
    }
    if !facts.modules.contains(&facts.installed_kernel) {
        return BootHealth::ModulesMissing {
            version: facts.installed_kernel.clone(),
        };
    }
    if facts.running_kernel != facts.installed_kernel {
        return BootHealth::RebootPending {
            running: facts.running_kernel.clone(),
            installed: facts.installed_kernel.clone(),
        };
    }
    BootHealth::Healthy
}

/// Filesystem type mounted at `mountpoint` in `/proc/mounts`-formatted text.
/// The last matching entry wins (later mounts shadow earlier ones).
pub fn fstype_at(mounts: &str, mountpoint: &str) -> Option<String> {
    mounts
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (_dev, target, fstype) = (f.next()?, f.next()?, f.next()?);
            (target == mountpoint).then(|| fstype.to_string())
        })
        .next_back()
}

/// Version part of `xbps-query -p pkgver rpi-kernel` output
/// (`rpi-kernel-6.12.95_1` -> `6.12.95_1`).
pub fn parse_installed_version(pkgver_output: &str) -> Option<String> {
    pkgver_output
        .trim()
        .strip_prefix(KERNEL_PKG)
        .and_then(|rest| rest.strip_prefix('-'))
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// Whether `xbps-pkgdb rpi-kernel` output complains about the kernel image
/// (hash mismatch, missing file, ...). Any mention counts.
pub fn pkgdb_reports_stale_image(pkgdb_output: &str) -> bool {
    pkgdb_output.lines().any(|l| l.contains(KERNEL_IMAGE))
}

/// Interpret an `xbps-pkgdb rpi-kernel` run. A kernel image complaint means
/// stale; a failure that does not mention the image is unknown, so it is an
/// error rather than a silent "fine".
pub fn interpret_pkgdb(success: bool, output: &str) -> Result<bool, GatherError> {
    if pkgdb_reports_stale_image(output) {
        Ok(true)
    } else if success {
        Ok(false)
    } else {
        Err(GatherError::PkgdbFailed(output.trim().to_string()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GatherError {
    #[error("xbps-pkgdb failed without naming {KERNEL_IMAGE_NAME}: {0:?}")]
    PkgdbFailed(String),
    #[error("failed to run {command}: {source}")]
    Command {
        command: &'static str,
        source: std::io::Error,
    },
    #[error("failed to read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("could not determine installed {KERNEL_PKG} version from {0:?}")]
    UnparsableVersion(String),
}

#[cfg(feature = "io")]
async fn run(command: &'static str, args: &[&str]) -> Result<(bool, String, String), GatherError> {
    let out = tokio::process::Command::new(command)
        .args(args)
        .output()
        .await
        .map_err(|source| GatherError::Command { command, source })?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    Ok((out.status.success(), stdout, stderr))
}

/// Collect [`BootFacts`] from the live system.
#[cfg(feature = "io")]
pub async fn gather() -> Result<BootFacts, GatherError> {
    let mounts = tokio::fs::read_to_string("/proc/mounts")
        .await
        .map_err(|source| GatherError::Read {
            path: "/proc/mounts".to_string(),
            source,
        })?;

    let (_, pkgver, _) = run("xbps-query", &["-p", "pkgver", KERNEL_PKG]).await?;
    let installed_kernel =
        parse_installed_version(&pkgver).ok_or(GatherError::UnparsableVersion(pkgver))?;

    // xbps-pkgdb exits non-zero on problems; a failure that does not name the
    // kernel image is unknown, not healthy.
    let (pkgdb_ok, pkgdb_out, pkgdb_err) = run("xbps-pkgdb", &[KERNEL_PKG]).await?;
    let kernel_image_stale = interpret_pkgdb(pkgdb_ok, &format!("{pkgdb_out}\n{pkgdb_err}"))?;

    let (_, uname, _) = run("uname", &["-r"]).await?;

    Ok(BootFacts {
        boot_fstype: fstype_at(&mounts, BOOT_MOUNT),
        kernel_image_stale,
        installed_kernel,
        modules: list_modules(Path::new(MODULES_DIR)).await?,
        running_kernel: uname.trim().to_string(),
    })
}

/// Names of the entries in a modules directory; empty if it does not exist.
#[cfg(feature = "io")]
pub async fn list_modules(dir: &Path) -> Result<Vec<String>, GatherError> {
    let read_err = |source| GatherError::Read {
        path: dir.display().to_string(),
        source,
    };
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(read_err(e)),
    };
    let mut names = Vec::new();
    while let Some(entry) = entries.next_entry().await.map_err(read_err)? {
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn healthy_facts() -> BootFacts {
        BootFacts {
            boot_fstype: Some("vfat".into()),
            kernel_image_stale: false,
            installed_kernel: "6.12.95_1".into(),
            modules: vec!["6.12.95_1".into()],
            running_kernel: "6.12.95_1".into(),
        }
    }

    #[test]
    fn healthy() {
        assert_eq!(assess(&healthy_facts()), BootHealth::Healthy);
    }

    #[test]
    fn reboot_pending_when_running_differs_from_installed() {
        let facts = BootFacts {
            running_kernel: "6.12.90_1".into(),
            ..healthy_facts()
        };
        assert_eq!(
            assess(&facts),
            BootHealth::RebootPending {
                running: "6.12.90_1".into(),
                installed: "6.12.95_1".into()
            }
        );
    }

    #[test]
    fn bootfs_not_mounted_when_nothing_at_boot() {
        let facts = BootFacts {
            boot_fstype: None,
            ..healthy_facts()
        };
        assert_eq!(assess(&facts), BootHealth::BootfsNotMounted);
    }

    #[test]
    fn bootfs_not_mounted_when_boot_is_not_vfat() {
        let facts = BootFacts {
            boot_fstype: Some("ext4".into()),
            ..healthy_facts()
        };
        assert_eq!(assess(&facts), BootHealth::BootfsNotMounted);
    }

    #[test]
    fn kernel_image_stale() {
        let facts = BootFacts {
            kernel_image_stale: true,
            ..healthy_facts()
        };
        assert_eq!(
            assess(&facts),
            BootHealth::KernelImageStale {
                pkg: "rpi-kernel-6.12.95_1".into()
            }
        );
    }

    #[test]
    fn modules_missing() {
        let facts = BootFacts {
            modules: vec!["6.12.90_1".into()],
            ..healthy_facts()
        };
        assert_eq!(
            assess(&facts),
            BootHealth::ModulesMissing {
                version: "6.12.95_1".into()
            }
        );
    }

    #[test]
    fn unmounted_bootfs_outranks_everything_else() {
        let facts = BootFacts {
            boot_fstype: None,
            kernel_image_stale: true,
            modules: vec![],
            running_kernel: "other".into(),
            ..healthy_facts()
        };
        assert_eq!(assess(&facts), BootHealth::BootfsNotMounted);
    }

    #[test]
    fn stale_image_outranks_missing_modules() {
        let facts = BootFacts {
            kernel_image_stale: true,
            modules: vec![],
            ..healthy_facts()
        };
        assert!(matches!(
            assess(&facts),
            BootHealth::KernelImageStale { .. }
        ));
    }

    #[test]
    fn only_healthy_and_reboot_pending_allow_reboot() {
        assert!(BootHealth::Healthy.allows_reboot());
        assert!(BootHealth::RebootPending {
            running: "a".into(),
            installed: "b".into()
        }
        .allows_reboot());
        assert!(!BootHealth::BootfsNotMounted.allows_reboot());
        assert!(!BootHealth::KernelImageStale { pkg: "p".into() }.allows_reboot());
        assert!(!BootHealth::ModulesMissing {
            version: "v".into()
        }
        .allows_reboot());
    }

    #[test]
    fn verdict_serde_roundtrip() {
        for v in [
            BootHealth::Healthy,
            BootHealth::BootfsNotMounted,
            BootHealth::RebootPending {
                running: "a".into(),
                installed: "b".into(),
            },
            BootHealth::KernelImageStale { pkg: "p".into() },
            BootHealth::ModulesMissing {
                version: "v".into(),
            },
        ] {
            let json = serde_json::to_string(&v).unwrap();
            assert_eq!(serde_json::from_str::<BootHealth>(&json).unwrap(), v);
        }
    }

    const MOUNTS: &str = "\
/dev/nvme0n1p2 / ext4 rw,noatime 0 0
tmpfs /tmp tmpfs rw 0 0
/dev/nvme0n1p1 /boot/firmware vfat rw 0 0
/dev/nvme0n1p1 /boot vfat rw 0 0
";

    #[test]
    fn fstype_at_finds_exact_mountpoint() {
        assert_eq!(fstype_at(MOUNTS, "/boot"), Some("vfat".into()));
        assert_eq!(fstype_at(MOUNTS, "/"), Some("ext4".into()));
    }

    #[test]
    fn fstype_at_does_not_match_prefixes_or_missing() {
        let only_firmware = "/dev/nvme0n1p1 /boot/firmware vfat rw 0 0\n";
        assert_eq!(fstype_at(only_firmware, "/boot"), None);
    }

    #[test]
    fn fstype_at_last_mount_wins() {
        let shadowed = "/dev/a /boot ext4 rw 0 0\n/dev/b /boot vfat rw 0 0\n";
        assert_eq!(fstype_at(shadowed, "/boot"), Some("vfat".into()));
    }

    #[test]
    fn parses_installed_version() {
        assert_eq!(
            parse_installed_version("rpi-kernel-6.12.95_1\n"),
            Some("6.12.95_1".into())
        );
        assert_eq!(parse_installed_version(""), None);
        assert_eq!(parse_installed_version("rpi-kernel-"), None);
        assert_eq!(parse_installed_version("other-1.0_1"), None);
    }

    #[test]
    fn pkgdb_mismatch_on_kernel_image_is_stale() {
        let out = "rpi-kernel-6.12.95_1: file `/boot/kernel8.img' hash mismatch.\n";
        assert!(pkgdb_reports_stale_image(out));
    }

    #[test]
    fn pkgdb_failure_without_image_mention_is_an_error() {
        assert!(matches!(
            interpret_pkgdb(false, "xbps-pkgdb: database locked"),
            Err(GatherError::PkgdbFailed(_))
        ));
    }

    #[test]
    fn pkgdb_failure_naming_the_image_is_stale() {
        let out = "file `/boot/kernel8.img' hash mismatch.";
        assert!(interpret_pkgdb(false, out).unwrap());
    }

    #[test]
    fn pkgdb_success_is_not_stale() {
        assert!(!interpret_pkgdb(true, "").unwrap());
    }

    #[test]
    fn pkgdb_clean_or_unrelated_output_is_not_stale() {
        assert!(!pkgdb_reports_stale_image(""));
        assert!(!pkgdb_reports_stale_image(
            "rpi-kernel-6.12.95_1: file `/boot/overlays/x.dtbo' hash mismatch.\n"
        ));
    }
}
