// bases/beacon/src/provisioning/stage4_install.rs
//! Stage 4: Install base system
//!
//! Mounts partitions and installs Void Linux base system.

use crate::actions::Action;
use crate::error::Result;
use crate::provisioning::types::{FormattedSystem, InstalledSystem};
use std::path::PathBuf;

/// Action that installs the base system
pub struct InstallSystemAction;

impl Action<FormattedSystem, InstalledSystem> for InstallSystemAction {
    fn description(&self) -> String {
        "Install base system".to_string()
    }

    async fn check(&self, _input: &FormattedSystem) -> Result<bool> {
        // TODO: Check if system is already installed
        Ok(true)
    }

    async fn apply(&self, input: FormattedSystem) -> Result<InstalledSystem> {
        tracing::info!("Installing base system...");
        
        // TODO: Implement actual installation:
        // 1. Mount partitions to /mnt/mdma-provision
        // 2. Install Void Linux base system
        // 3. Install bootloader
        // 4. Create fstab

        tracing::info!("✅ System installation complete (STUB - not implemented yet)");

        Ok(InstalledSystem {
            formatted: input,
            mount_point: PathBuf::from("/mnt/mdma-provision"),
        })
    }

    async fn preview(&self, input: FormattedSystem) -> Result<InstalledSystem> {
        tracing::info!("Would install base system:");
        tracing::info!("  1. Mount partitions to /mnt/mdma-provision");
        tracing::info!("  2. Install Void Linux base packages");
        tracing::info!("  3. Install bootloader");
        tracing::info!("  4. Generate fstab");

        Ok(InstalledSystem {
            formatted: input,
            mount_point: PathBuf::from("/mnt/mdma-provision"),
        })
    }
}
