// bases/beacon/src/provisioning/stage5_configure.rs
//! Stage 5: Configure system

use crate::actions::{Action, ActionId, PlannedAction};
use crate::error::Result;
use crate::provisioning::types::{ConfiguredSystem, InstalledSystem};

#[derive(Clone, Debug)]
pub struct ConfigureSystemAction;

impl Action<InstalledSystem, ConfiguredSystem> for ConfigureSystemAction {
    fn id(&self) -> ActionId {
        ActionId::new("configure-system")
    }

    fn description(&self) -> String {
        "Configure hostname and network".to_string()
    }

    async fn plan(
        &self,
        input: &InstalledSystem,
    ) -> Result<PlannedAction<InstalledSystem, ConfiguredSystem, Self>> {
        let assumed_output = ConfiguredSystem {
            installed: input.clone(),
        };

        Ok(PlannedAction {
            description: self.description(),
            action: self.clone(),
            input: input.clone(),
            assumed_output,
        })
    }

    async fn apply(&self, planned_output: &ConfiguredSystem) -> Result<ConfiguredSystem> {
        tracing::info!("Stage 5: Configure system - executing plan");

        let mount_point = &planned_output.installed.mount_point();
        let unit_type = &planned_output
            .installed
            .formatted
            .partitioned
            .validated
            .config
            .unit_type;

        // Configure hostname based on unit type
        let hostname = format!("mdma-{}", unit_type.to_string().to_lowercase());
        tracing::info!(
            "Would execute: echo '{}' > {}/etc/hostname",
            hostname,
            mount_point.display()
        );
        // TODO: Real implementation:
        // std::fs::write(mount_point.join("etc/hostname"), hostname)?;

        // Configure network
        tracing::info!(
            "Would execute: configure DHCP in {}/etc/rc.conf",
            mount_point.display()
        );
        // TODO: Real implementation:
        // let rc_conf = mount_point.join("etc/rc.conf");
        // std::fs::write(&rc_conf, "NETWORKING=yes\n")?;

        // Create mdma user
        tracing::info!(
            "Would execute: chroot {} useradd -m -G audio,video mdma",
            mount_point.display()
        );
        // TODO: Real implementation:
        // Command::new("chroot")
        //     .arg(mount_point)
        //     .arg("useradd")
        //     .args(["-m", "-G", "audio,video", "mdma"])
        //     .output()?;

        // Install required packages
        tracing::info!(
            "Would execute: xbps-install -Sy -r {} dbus avahi void-repo-nonfree",
            mount_point.display()
        );
        // TODO: Real implementation:
        // Command::new("xbps-install")
        //     .args(["-Sy", "-r"])
        //     .arg(mount_point)
        //     .args(["dbus", "avahi", "void-repo-nonfree"])
        //     .output()?;

        tracing::info!("Configure stage complete (simulated)");
        Ok(planned_output.clone())
    }
}
