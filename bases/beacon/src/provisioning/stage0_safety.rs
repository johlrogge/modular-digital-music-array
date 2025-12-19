// bases/beacon/src/provisioning/stage0_safety.rs
//! Stage 0: Verify running on Raspberry Pi

use crate::actions::{Action, ActionId, ExecutionMode, PlannedAction};
use crate::error::{BeaconError, Result};
use crate::hardware::HardwareInfo;
use crate::provisioning::types::SafeHardware;

#[derive(Clone, Debug)]
pub struct CheckRaspberryPiAction {
    pub execution_mode: ExecutionMode,
}

impl Action<HardwareInfo, SafeHardware> for CheckRaspberryPiAction {
    fn id(&self) -> ActionId {
        ActionId::new("check-raspberry-pi")
    }

    fn description(&self) -> String {
        "Verify running on Raspberry Pi".to_string()
    }

    async fn plan(
        &self,
        input: &HardwareInfo,
    ) -> Result<PlannedAction<HardwareInfo, SafeHardware, Self>> {
        // In dry-run mode, skip the hardware check
        if self.execution_mode == ExecutionMode::DryRun {
            tracing::info!("🔍 DRY-RUN: Skipping Raspberry Pi check (dev mode)");
            
            let assumed_output = SafeHardware {
                info: input.clone(),
            };
            
            return Ok(PlannedAction {
                description: self.description(),
                action: self.clone(),
                input: input.clone(),
                assumed_output,
            });
        }
        
        // In apply mode, do the real check
        let cpuinfo = tokio::fs::read_to_string("/proc/cpuinfo").await?;
        if !cpuinfo.contains("Raspberry Pi") {
            return Err(BeaconError::Safety("Not running on Raspberry Pi - use --check flag for dry-run testing on dev machines".into()));
        }

        let assumed_output = SafeHardware {
            info: input.clone(),
        };

        Ok(PlannedAction {
            description: self.description(),
            action: self.clone(),
            input: input.clone(),
            assumed_output,
        })
    }

    async fn apply(&self, input: HardwareInfo) -> Result<SafeHardware> {
        // In dry-run mode, this is never called (plan-only)
        // But if it somehow is, be safe
        if self.execution_mode == ExecutionMode::DryRun {
            tracing::warn!("⚠️  apply() called in DRY-RUN mode - this shouldn't happen!");
            return Ok(SafeHardware { info: input });
        }
        
        // Re-verify during execution (paranoid + idempotent)
        let cpuinfo = tokio::fs::read_to_string("/proc/cpuinfo").await?;
        if !cpuinfo.contains("Raspberry Pi") {
            return Err(BeaconError::Safety("Not running on Raspberry Pi".into()));
        }
        Ok(SafeHardware { info: input })
    }
}
