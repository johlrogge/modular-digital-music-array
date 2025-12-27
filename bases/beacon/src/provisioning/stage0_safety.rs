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
        let cpuinfo = tokio::fs::read_to_string("/proc/cpuinfo")
            .await
            .map_err(|source| BeaconError::io(self.id().as_str(), source))?;
        if self.execution_mode == ExecutionMode::Apply && !cpuinfo.contains("Raspberry Pi") {
            return Err(BeaconError::Safety("Not running on Raspberry Pi".into()));
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

    async fn apply(&self, planned_output: &SafeHardware) -> Result<SafeHardware> {
        tracing::info!("Stage 0: Safety check - executing plan");

        // Re-verify during execution (safety-critical: paranoid + idempotent)
        let cpuinfo = tokio::fs::read_to_string("/proc/cpuinfo")
            .await
            .map_err(|source| BeaconError::io(self.id().as_str(), source))?;
        if !cpuinfo.contains("Raspberry Pi") {
            return Err(BeaconError::Safety(
                "Safety check failed: Not running on Raspberry Pi".into(),
            ));
        }

        tracing::info!("Safety check passed: Running on Raspberry Pi");
        Ok(planned_output.clone())
    }
}
