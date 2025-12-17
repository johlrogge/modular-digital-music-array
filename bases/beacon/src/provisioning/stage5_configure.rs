// bases/beacon/src/provisioning/stage5_configure.rs
//! Stage 5: Configure system
//!
//! Sets up hostname, network, users, and packages.

use crate::actions::Action;
use crate::error::Result;
use crate::provisioning::types::{ConfiguredSystem, InstalledSystem};

/// Action that configures the installed system
pub struct ConfigureSystemAction;

impl Action<InstalledSystem, ConfiguredSystem> for ConfigureSystemAction {
    fn description(&self) -> String {
        "Configure system".to_string()
    }

    async fn check(&self, _input: &InstalledSystem) -> Result<bool> {
        // TODO: Check if system is already configured
        Ok(true)
    }

    async fn apply(&self, input: InstalledSystem) -> Result<ConfiguredSystem> {
        tracing::info!("Configuring system...");
        
        // TODO: Implement actual configuration:
        // 1. Set hostname
        // 2. Configure network
        // 3. Create users (mdma-audio, mdma-library, etc.)
        // 4. Install MDMA packages
        // 5. Configure services

        tracing::info!("✅ System configuration complete (STUB - not implemented yet)");

        Ok(ConfiguredSystem { installed: input })
    }

    async fn preview(&self, input: InstalledSystem) -> Result<ConfiguredSystem> {
        let hostname = &input.formatted.partitioned.validated.config.hostname;
        
        tracing::info!("Would configure system:");
        tracing::info!("  1. Set hostname to {}", hostname);
        tracing::info!("  2. Configure network");
        tracing::info!("  3. Create system users");
        tracing::info!("  4. Install MDMA packages");
        tracing::info!("  5. Enable services");

        Ok(ConfiguredSystem { installed: input })
    }
}
