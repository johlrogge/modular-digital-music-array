// bases/beacon/src/provisioning/stage6_finalize.rs
//! Stage 6: Finalize provisioning
//!
//! Unmounts filesystems and prepares system for first boot.

use crate::actions::Action;
use crate::error::Result;
use crate::provisioning::types::{
    ConfiguredSystem, ProvisionedSystem, ProvisioningSummary,
};

/// Action that finalizes the provisioning
pub struct FinalizeProvisioningAction;

impl Action<ConfiguredSystem, ProvisionedSystem> for FinalizeProvisioningAction {
    fn description(&self) -> String {
        "Finalize provisioning".to_string()
    }

    async fn check(&self, _input: &ConfiguredSystem) -> Result<bool> {
        // Finalization always needs to happen
        Ok(true)
    }

    async fn apply(&self, input: ConfiguredSystem) -> Result<ProvisionedSystem> {
        tracing::info!("Finalizing provisioning...");
        
        // TODO: Implement actual finalization:
        // 1. Sync filesystems
        // 2. Unmount partitions
        // 3. Run any final checks

        let config = &input.installed.formatted.partitioned.validated.config;
        let drives = &input.installed.formatted.partitioned.validated.drives;

        let summary = ProvisioningSummary {
            hostname: config.hostname.clone(),
            unit_type: config.unit_type.clone(),
            primary_drive: drives.primary().device.clone(),
            secondary_drive: drives.secondary().map(|d| d.device.clone()),
            total_partitions: match &input.installed.formatted.partitioned.layout {
                crate::provisioning::types::PartitionLayout::SingleDrive { partitions, .. } => {
                    partitions.len()
                }
                crate::provisioning::types::PartitionLayout::DualDrive {
                    primary_partitions,
                    secondary_partitions,
                    ..
                } => primary_partitions.len() + secondary_partitions.len(),
            },
        };

        tracing::info!("✅ Provisioning complete (STUB - not implemented yet)");

        Ok(ProvisionedSystem {
            configured: input,
            summary,
        })
    }

    async fn preview(&self, input: ConfiguredSystem) -> Result<ProvisionedSystem> {
        tracing::info!("Would finalize provisioning:");
        tracing::info!("  1. Sync filesystems");
        tracing::info!("  2. Unmount partitions");
        tracing::info!("  3. Prepare for first boot");

        let config = &input.installed.formatted.partitioned.validated.config;
        let drives = &input.installed.formatted.partitioned.validated.drives;

        let summary = ProvisioningSummary {
            hostname: config.hostname.clone(),
            unit_type: config.unit_type.clone(),
            primary_drive: drives.primary().device.clone(),
            secondary_drive: drives.secondary().map(|d| d.device.clone()),
            total_partitions: match &input.installed.formatted.partitioned.layout {
                crate::provisioning::types::PartitionLayout::SingleDrive { partitions, .. } => {
                    partitions.len()
                }
                crate::provisioning::types::PartitionLayout::DualDrive {
                    primary_partitions,
                    secondary_partitions,
                    ..
                } => primary_partitions.len() + secondary_partitions.len(),
            },
        };

        Ok(ProvisionedSystem {
            configured: input,
            summary,
        })
    }
}
