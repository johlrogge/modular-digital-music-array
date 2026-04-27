// bases/beacon/src/update.rs
//! Beacon self-update functionality
//!
//! Provides the ability for beacon to update itself from the package repository
//! and restart the service. This enables rapid iteration during development.

use crate::error::{BeaconError, Result};
use tokio::process::Command;
use tracing::{error, info};

/// Update beacon from the package repository
///
/// This function:
/// 1. Syncs the package repository
/// 2. Updates the beacon package
/// 3. Restarts the beacon service
///
/// All output is emitted via tracing; svlogd writes it to disk and the SSE
/// `/stream` endpoint tails the file.
pub async fn update_beacon_from_repo() -> Result<()> {
    info!("Starting beacon update...");

    // Step 1: Sync repository
    info!("Syncing package repository...");

    let sync = Command::new("xbps-install")
        .arg("-S")
        .output()
        .await
        .map_err(|e| BeaconError::Installation(format!("Failed to run xbps-install: {}", e)))?;

    if !sync.status.success() {
        let stderr = String::from_utf8_lossy(&sync.stderr);
        let stdout = String::from_utf8_lossy(&sync.stdout);
        error!(
            "Repository sync failed. stdout: {}, stderr: {}",
            stdout, stderr
        );
        error!("Repository sync failed!");
        error!("   {}", stderr);
        return Err(BeaconError::Installation(format!(
            "Repo sync failed: {}",
            stderr
        )));
    }

    info!("Repository synced");

    // Step 2: Update xbps itself first (required before other updates)
    info!("Updating xbps package manager...");

    let xbps_update = Command::new("xbps-install")
        .args(["-uy", "xbps"])
        .output()
        .await
        .map_err(|e| BeaconError::Installation(format!("Failed to update xbps: {}", e)))?;

    let xbps_stdout = String::from_utf8_lossy(&xbps_update.stdout);
    let xbps_stderr = String::from_utf8_lossy(&xbps_update.stderr);

    if !xbps_update.status.success()
        && !xbps_stderr.contains("up to date")
        && !xbps_stdout.contains("up to date")
    {
        error!("xbps update issue: {}", xbps_stderr);
    }
    info!("xbps updated");

    // Step 3: Check what would be updated (dry run)
    info!("Checking for beacon updates...");

    let check = Command::new("xbps-install")
        .args(["-n", "beacon"])
        .output()
        .await
        .map_err(|e| BeaconError::Installation(format!("Failed to check updates: {}", e)))?;

    let check_output = String::from_utf8_lossy(&check.stdout);
    info!("Update check output: {}", check_output);

    if check_output.contains("beacon") {
        info!("Update available!");
    } else {
        info!("Already at latest version");
    }

    // Step 4: Update beacon package
    info!("Updating beacon package...");

    let update = Command::new("xbps-install")
        .args(["-uy", "beacon"])
        .output()
        .await
        .map_err(|e| BeaconError::Installation(format!("Failed to update beacon: {}", e)))?;

    let update_stdout = String::from_utf8_lossy(&update.stdout);
    let update_stderr = String::from_utf8_lossy(&update.stderr);

    info!("Update stdout: {}", update_stdout);
    if !update_stderr.is_empty() {
        info!("Update stderr: {}", update_stderr);
    }

    if !update.status.success() {
        error!("Beacon update failed");
        error!("Update failed: {}", update_stderr);
        return Err(BeaconError::Installation(format!(
            "Update failed: {}",
            update_stderr
        )));
    }

    info!("Beacon package updated");

    // Step 5: Restart beacon service
    info!("Restarting beacon service...");

    // Note: This will kill our own process, so we might not see the response
    let restart = Command::new("sv")
        .args(["restart", "beacon"])
        .output()
        .await
        .map_err(|e| BeaconError::Installation(format!("Failed to restart beacon: {}", e)))?;

    let restart_stdout = String::from_utf8_lossy(&restart.stdout);
    let restart_stderr = String::from_utf8_lossy(&restart.stderr);

    if !restart.status.success() {
        info!(
            "Restart command returned non-zero: stdout: {}, stderr: {}",
            restart_stdout, restart_stderr
        );
        info!("Restart command output: {}", restart_stderr);
    }

    info!("Beacon updated successfully!");
    info!("Service is restarting...");
    info!("Page will reload automatically in 3 seconds");

    info!("Beacon update completed successfully");

    Ok(())
}

/// Get current beacon version from Cargo package metadata
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Get git commit hash (with -dirty suffix if uncommitted changes)
pub fn git_hash() -> &'static str {
    env!("BUILD_GIT_HASH")
}

/// Get build timestamp
pub fn build_timestamp() -> &'static str {
    env!("BUILD_TIMESTAMP")
}

/// Get full version string including git hash
/// Format: "0.3.1 (abc1234)" or "0.3.1 (abc1234-dirty)"
pub fn full_version() -> String {
    format!("{} ({})", current_version(), git_hash())
}

#[cfg(test)]
mod tests {
    #[test]
    fn current_version() {
        let version = super::current_version();
        assert!(!version.is_empty());
        println!("Current beacon version: {}", version);
    }
}
