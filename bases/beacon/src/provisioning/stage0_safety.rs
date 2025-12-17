// bases/beacon/src/provisioning/stage0_safety.rs
//! Stage 0: Verify running on Raspberry Pi
//!
//! This is the most critical safety check. It ensures we're running
//! on actual Raspberry Pi hardware before making any system changes.

use crate::actions::Action;
use crate::error::{BeaconError, Result};
use crate::hardware::HardwareInfo;
use crate::provisioning::types::SafeHardware;

/// Action that verifies we're running on a Raspberry Pi
///
/// # Safety
///
/// This action MUST be the first stage in the pipeline.
/// In APPLY mode, it reads /proc/cpuinfo and verifies the CPU is
/// a Raspberry Pi. If not, it returns an error and prevents
/// any destructive operations.
///
/// In DRY RUN mode, it skips the check and allows preview to continue.
pub struct CheckRaspberryPiAction;

impl Action<HardwareInfo, SafeHardware> for CheckRaspberryPiAction {
    fn description(&self) -> String {
        "Verify running on Raspberry Pi".to_string()
    }

    async fn check(&self, _input: &HardwareInfo) -> Result<bool> {
        // Always need to perform the check in APPLY mode
        Ok(true)
    }

    async fn apply(&self, input: HardwareInfo) -> Result<SafeHardware> {
        // Read /proc/cpuinfo
        let cpuinfo = tokio::fs::read_to_string("/proc/cpuinfo")
            .await
            .map_err(|e| BeaconError::Safety(format!("Cannot read /proc/cpuinfo: {}", e)))?;

        // Check for Raspberry Pi
        if !cpuinfo.contains("Raspberry Pi") {
            return Err(BeaconError::Safety(
                "Not running on a Raspberry Pi! Refusing to provision.".to_string(),
            ));
        }

        tracing::info!("✅ Confirmed running on Raspberry Pi");

        Ok(SafeHardware { info: input })
    }

    async fn preview(&self, input: HardwareInfo) -> Result<SafeHardware> {
        // In DRY RUN mode, skip the actual check and build mock output
        tracing::info!("Would verify Raspberry Pi hardware");

        Ok(SafeHardware { info: input })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_hardware() -> HardwareInfo {
        HardwareInfo {
            nvme_drives: vec![],
            model: "model".to_string(),
            serial: None,
            memory_mb: None,
        }
    }

    #[tokio::test]
    async fn preview_always_succeeds() {
        let action = CheckRaspberryPiAction;
        let hardware = mock_hardware();

        let result = action.preview(hardware).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn check_always_returns_true() {
        let action = CheckRaspberryPiAction;
        let hardware = mock_hardware();

        let result = action.check(&hardware).await;
        assert!(result.is_ok());
        assert!(result.unwrap());
    }
}
