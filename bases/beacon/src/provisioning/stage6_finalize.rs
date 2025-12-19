// bases/beacon/src/provisioning/stage6_finalize.rs
//! Stage 6: Finalize provisioning

use crate::actions::{Action, ActionId, PlannedAction};
use crate::error::Result;
use crate::provisioning::types::{ConfiguredSystem, ProvisionedSystem, ProvisioningSummary};

#[derive(Clone, Debug)]
pub struct FinalizeProvisioningAction;

impl Action<ConfiguredSystem, ProvisionedSystem> for FinalizeProvisioningAction {
    fn id(&self) -> ActionId {
        ActionId::new("finalize-provisioning")
    }

    fn description(&self) -> String {
        "Finalize and verify provisioning".to_string()
    }

    async fn plan(
        &self,
        input: &ConfiguredSystem,
    ) -> Result<PlannedAction<ConfiguredSystem, ProvisionedSystem, Self>> {
        let config = &input.installed.formatted.partitioned.validated.config;
        let primary_drive = input
            .installed
            .formatted
            .partitioned
            .validated
            .drives
            .primary()
            .device
            .clone();

        let summary = ProvisioningSummary {
            hostname: config.hostname.clone(),
            unit_type: config.unit_type.clone(),
            primary_drive,
            secondary_drive: None,
            total_partitions: 2, // Stub value
        };

        let assumed_output = ProvisionedSystem {
            configured: input.clone(),
            summary,
        };

        Ok(PlannedAction {
            description: self.description(),
            action: self.clone(),
            input: input.clone(),
            assumed_output,
        })
    }

    async fn apply(&self, input: ConfiguredSystem) -> Result<ProvisionedSystem> {
        // Stub: Would verify everything and generate summary
        tracing::info!("Would finalize provisioning here");

        let config = &input.installed.formatted.partitioned.validated.config;
        let primary_drive = input
            .installed
            .formatted
            .partitioned
            .validated
            .drives
            .primary()
            .device
            .clone();

        let summary = ProvisioningSummary {
            hostname: config.hostname.clone(),
            unit_type: config.unit_type.clone(),
            primary_drive,
            secondary_drive: None,
            total_partitions: 2,
        };

        Ok(ProvisionedSystem {
            configured: input,
            summary,
        })
    }
}
