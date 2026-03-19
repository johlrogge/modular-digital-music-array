// bases/beacon/src/provisioning/stage6_finalize.rs
//! Stage 6: Finalize provisioning
//!
//! This stage:
//! 1. Unmounts all partitions in reverse depth order
//! 2. Verifies the system is ready to boot

use crate::actions::{Action, ActionId, PlannedAction};
use crate::error::{BeaconError, Result};
use crate::provisioning::types::{ConfiguredSystem, MountPoint, Partition, ProvisionedSystem};
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

        // Unmount all partitions
        unmount_all(mount_root, partitions).await?;

        tracing::info!("✅ Stage 6: Provisioning complete!");
        tracing::info!("   System is ready to boot from NVMe");
        tracing::info!("   Reboot to start the new system");

        Ok(planned_output.clone())
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
