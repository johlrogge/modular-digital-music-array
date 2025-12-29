// bases/beacon/src/provisioning/stage6_finalize.rs
//! Stage 6: Finalize provisioning

use crate::actions::{Action, ActionId, PlannedAction};
use crate::error::Result;
use crate::provisioning::types::{ConfiguredSystem, ProvisionedSystem};

#[derive(Clone, Debug)]
pub struct FinalizeProvisioningAction;

impl Action<ConfiguredSystem, ProvisionedSystem, ProvisionedSystem> for FinalizeProvisioningAction {
    fn id(&self) -> ActionId {
        ActionId::new("finalize-provisioning")
    }

    fn description(&self) -> String {
        "Finalize and verify provisioning".to_string()
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

        let mount_point = &planned_output.configured.installed.mount_point();

        // Install bootloader
        tracing::info!(
            "Would execute: grub-install --root-directory={} /dev/nvme0n1",
            mount_point.display()
        );
        // TODO: Real implementation:
        // let boot_device = &planned_output.summary.primary_drive;
        // Command::new("grub-install")
        //     .arg(format!("--root-directory={}", mount_point.display()))
        //     .arg(boot_device)
        //     .output()?;

        // Generate fstab
        tracing::info!(
            "Would execute: genfstab -U {} > {}/etc/fstab",
            mount_point.display(),
            mount_point.display()
        );
        // TODO: Real implementation:
        // Command::new("genfstab")
        //     .args(["-U", mount_point.to_str().unwrap()])
        //     .output()
        //     .and_then(|output| {
        //         std::fs::write(
        //             mount_point.join("etc/fstab"),
        //             output.stdout
        //         )
        //     })?;

        // Unmount all filesystems
        tracing::info!("Would execute: umount -R {}", mount_point.display());
        // TODO: Real implementation:
        // Command::new("umount")
        //     .args(["-R", mount_point.to_str().unwrap()])
        //     .output()?;

        // Verify summary
        tracing::info!("Provisioning complete!");

        tracing::info!("Finalize stage complete (simulated)");
        Ok(planned_output.clone())
    }
}
