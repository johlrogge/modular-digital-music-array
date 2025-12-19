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

    async fn apply(&self, input: InstalledSystem) -> Result<ConfiguredSystem> {
        // Stub: Would configure hostname, network, users here
        tracing::info!("Would configure system here");

        Ok(ConfiguredSystem { installed: input })
    }
}
