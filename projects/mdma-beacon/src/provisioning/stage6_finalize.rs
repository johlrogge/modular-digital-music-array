// bases/beacon/src/provisioning/stage6_finalize.rs
//! Stage 6: Finalize provisioning
//!
//! This stage:
//! 1. Verifies the target would boot its installed kernel (boot health, #65)
//! 2. Unmounts all partitions in reverse depth order
//! 3. Verifies everything is unmounted

use crate::actions::{Action, ActionId, PlannedAction};
use crate::error::{BeaconError, Result};
use crate::provisioning::types::{ConfiguredSystem, MountPoint, Partition, ProvisionedSystem};
use boot_health::{
    assess, list_modules, parse_installed_version, BootFacts, BootHealth, KERNEL_PKG,
};
use std::path::PathBuf;
use tokio::process::Command;

#[derive(Clone, Debug)]
pub struct FinalizeProvisioningAction;

impl Action<ConfiguredSystem, ProvisionedSystem, ProvisionedSystem> for FinalizeProvisioningAction {
    fn id(&self) -> ActionId {
        ActionId::new("finalize-provisioning")
    }

    fn description(&self) -> String {
        "Unmount partitions and finalize".to_string()
    }

    async fn plan(
        &self,
        input: &ConfiguredSystem,
    ) -> Result<PlannedAction<ConfiguredSystem, ProvisionedSystem, ProvisionedSystem, Self>> {
        let assumed_output = ProvisionedSystem {
            configured: input.clone(),
        };

        Ok(PlannedAction {
            description: self.description(),
            action: self.clone(),
            input: input.clone(),
            planned_work: assumed_output.clone(),
            assumed_output,
        })
    }

    async fn apply(&self, planned_output: &ProvisionedSystem) -> Result<ProvisionedSystem> {
        tracing::info!("Stage 6: Finalize provisioning - executing plan");

        let mount_root = planned_output.configured.mount_root();
        let partitions = planned_output.configured.partitions();

        // Refuse to finish a target that would not boot its installed kernel (#65)
        verify_target_boot_health(mount_root).await?;

        // Unmount all partitions
        unmount_all(mount_root, partitions).await?;

        tracing::info!("✅ Stage 6: Provisioning complete!");
        tracing::info!("   System is ready to boot from NVMe");
        tracing::info!("   Reboot to start the new system");

        Ok(planned_output.clone())
    }
}

/// Pure verdict for a freshly provisioned target: the "running" kernel is by
/// definition the installed one, so only the on-disk state is judged.
fn assess_target(
    boot_fstype: Option<String>,
    kernel_image_exists: bool,
    installed_kernel: String,
    modules: Vec<String>,
) -> BootHealth {
    assess(&BootFacts {
        boot_fstype,
        kernel_image_stale: !kernel_image_exists,
        running_kernel: installed_kernel.clone(),
        installed_kernel,
        modules,
    })
}

/// Verify the mounted target has p1 (vfat) at `<root>/boot`, the kernel image
/// on it, and the modules for the installed `rpi-kernel`. Aborts otherwise.
async fn verify_target_boot_health(mount_root: &std::path::Path) -> Result<()> {
    tracing::info!("Verifying target boot health");

    let boot_dir = mount_root.join("boot");
    let findmnt = Command::new("findmnt")
        .args(["-n", "-o", "FSTYPE", "-M"])
        .arg(&boot_dir)
        .output()
        .await
        .map_err(|e| BeaconError::command_failed("findmnt", e))?;
    let boot_fstype = Some(String::from_utf8_lossy(&findmnt.stdout).trim().to_string())
        .filter(|s| findmnt.status.success() && !s.is_empty());

    let query = Command::new("xbps-query")
        .arg("-r")
        .arg(mount_root)
        .args(["-p", "pkgver", KERNEL_PKG])
        .output()
        .await
        .map_err(|e| BeaconError::command_failed("xbps-query", e))?;
    let pkgver = String::from_utf8_lossy(&query.stdout).to_string();
    let installed_kernel = parse_installed_version(&pkgver).ok_or_else(|| {
        BeaconError::Provisioning(format!(
            "Boot health check failed: cannot determine installed {KERNEL_PKG} version in {} (xbps-query said {:?})",
            mount_root.display(),
            pkgver.trim()
        ))
    })?;

    let kernel_image_exists = boot_dir.join(boot_health::KERNEL_IMAGE_NAME).exists();
    let modules = list_modules(&mount_root.join("usr/lib/modules"))
        .await
        .map_err(|e| BeaconError::Provisioning(format!("Boot health check failed: {e}")))?;

    match assess_target(boot_fstype, kernel_image_exists, installed_kernel, modules) {
        BootHealth::Healthy => {
            tracing::info!("✅ Target boot health OK");
            Ok(())
        }
        verdict => Err(BeaconError::Provisioning(format!(
            "Boot health check failed for target {}: {verdict}. \
             The unit would not boot its installed kernel; aborting before unmount.",
            mount_root.display()
        ))),
    }
}

/// Check if a path is currently a mount point
async fn check_if_path_mounted(path: &std::path::Path) -> Result<bool> {
    let output = Command::new("findmnt")
        .arg("-n")
        .arg("-M")
        .arg(path)
        .output()
        .await
        .map_err(|e| BeaconError::command_failed("findmnt", e))?;

    Ok(output.status.success())
}

/// Unmount all mount points in reverse depth order (deepest first)
async fn unmount_all(mount_root: &std::path::Path, partitions: &[Partition]) -> Result<()> {
    tracing::info!("Unmounting partitions");

    // Build list of mount paths
    let mut mount_paths: Vec<PathBuf> = partitions
        .iter()
        .map(|p| match p.mount_point {
            MountPoint::Root => mount_root.to_path_buf(),
            _ => mount_root.join(
                p.mount_point
                    .as_path()
                    .strip_prefix("/")
                    .unwrap_or(p.mount_point.as_path()),
            ),
        })
        .collect();

    // Sort by path length descending (deepest paths first)
    mount_paths.sort_by(|a, b| {
        let a_depth = a.components().count();
        let b_depth = b.components().count();
        b_depth.cmp(&a_depth)
    });

    // Filter to only those actually mounted
    let mut paths_to_unmount = Vec::new();
    for path in &mount_paths {
        if check_if_path_mounted(path).await? {
            paths_to_unmount.push(path.clone());
        }
    }

    if paths_to_unmount.is_empty() {
        tracing::info!("  No mount points to unmount");
        return Ok(());
    }

    tracing::info!(
        "  Unmounting {} mount point(s) in reverse depth order",
        paths_to_unmount.len()
    );

    for path in &paths_to_unmount {
        tracing::info!("  Unmounting {}", path.display());

        let output = Command::new("umount")
            .arg(path)
            .output()
            .await
            .map_err(|e| BeaconError::command_failed("umount", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(BeaconError::Provisioning(format!(
                "Failed to unmount {}: {}",
                path.display(),
                stderr.trim()
            )));
        }

        tracing::info!("  ✅ Unmounted {}", path.display());
    }

    // Verify all are unmounted
    for path in &paths_to_unmount {
        if check_if_path_mounted(path).await? {
            return Err(BeaconError::Provisioning(format!(
                "Verification failed: {} is still mounted after unmount",
                path.display()
            )));
        }
    }

    tracing::info!(
        "✅ All {} mount point(s) unmounted successfully",
        paths_to_unmount.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(fstype: Option<&str>, image: bool, modules: &[&str]) -> BootHealth {
        assess_target(
            fstype.map(str::to_string),
            image,
            "6.12.95_1".to_string(),
            modules.iter().map(|m| m.to_string()).collect(),
        )
    }

    #[test]
    fn healthy_target_passes() {
        assert_eq!(
            target(Some("vfat"), true, &["6.12.95_1"]),
            BootHealth::Healthy
        );
    }

    #[test]
    fn boot_not_vfat_fails() {
        assert_eq!(
            target(None, true, &["6.12.95_1"]),
            BootHealth::BootfsNotMounted
        );
        assert_eq!(
            target(Some("ext4"), true, &["6.12.95_1"]),
            BootHealth::BootfsNotMounted
        );
    }

    #[test]
    fn missing_kernel_image_fails() {
        assert!(matches!(
            target(Some("vfat"), false, &["6.12.95_1"]),
            BootHealth::KernelImageStale { .. }
        ));
    }

    #[test]
    fn missing_modules_fails() {
        assert_eq!(
            target(Some("vfat"), true, &["6.1.0_1"]),
            BootHealth::ModulesMissing {
                version: "6.12.95_1".to_string()
            }
        );
    }
}
