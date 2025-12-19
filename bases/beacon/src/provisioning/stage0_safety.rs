// bases/beacon/src/provisioning/stage0_safety.rs
//! Stage 0: Verify running on Raspberry Pi

use crate::actions::{Action, ActionId, PlannedAction};
use crate::error::{BeaconError, Result};
use crate::hardware::HardwareInfo;
use crate::provisioning::types::SafeHardware;

#[derive(Clone, Debug)]
pub struct CheckRaspberryPiAction;

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
        let cpuinfo = tokio::fs::read_to_string("/proc/cpuinfo").await?;
        if !cpuinfo.contains("Raspberry Pi") {
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

    async fn apply(&self, input: HardwareInfo) -> Result<SafeHardware> {
        // Re-verify during execution (paranoid + idempotent)
        let cpuinfo = tokio::fs::read_to_string("/proc/cpuinfo").await?;
        if !cpuinfo.contains("Raspberry Pi") {
            return Err(BeaconError::Safety("Not running on Raspberry Pi".into()));
        }
        Ok(SafeHardware { info: input })
    }
}
