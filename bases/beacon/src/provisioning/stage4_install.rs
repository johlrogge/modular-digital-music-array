// bases/beacon/src/provisioning/stage4_install.rs
//! Stage 4: Install base system

use crate::actions::{Action, ActionId, PlannedAction};
use crate::error::Result;
use crate::provisioning::types::{FormattedSystem, InstalledSystem};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct InstallSystemAction;

impl Action<FormattedSystem, InstalledSystem> for InstallSystemAction {
    fn id(&self) -> ActionId {
        ActionId::new("install-system")
    }

    fn description(&self) -> String {
        "Install base operating system".to_string()
    }

    async fn plan(
        &self,
        input: &FormattedSystem,
    ) -> Result<PlannedAction<FormattedSystem, InstalledSystem, Self>> {
        let assumed_output = InstalledSystem {
            formatted: input.clone(),
            mount_point: PathBuf::from("/mnt"),
        };

        Ok(PlannedAction {
            description: self.description(),
            action: self.clone(),
            input: input.clone(),
            assumed_output,
        })
    }

    async fn apply(&self, input: FormattedSystem) -> Result<InstalledSystem> {
        // Stub: Would mount and install base system here
        tracing::info!("Would install base system here");

        Ok(InstalledSystem {
            formatted: input,
            mount_point: PathBuf::from("/mnt"),
        })
    }
}
