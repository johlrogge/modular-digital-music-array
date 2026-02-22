// bases/beacon/src/update.rs
//! Beacon self-update functionality
//!
//! Provides the ability for beacon to update itself from the package repository
//! and restart the service. This enables rapid iteration during development.

use crate::error::{BeaconError, Result};
use tokio::process::Command;
use tokio::sync::broadcast;
use tracing::{error, info};

/// Send a log message to both tracing and the broadcast channel
macro_rules! send_log {
    ($tx:expr, $($arg:tt)*) => {{
        let msg = format!($($arg)*);
        tracing::info!("{}", msg);
        let _ = $tx.send(msg);
    }};
}

/// Update beacon from the package repository
///
/// This function:
/// 1. Syncs the package repository
/// 2. Updates the beacon package
/// 3. Restarts the beacon service
///
/// All output is streamed to the provided broadcast channel for real-time
/// display in the web UI.
pub async fn update_beacon_from_repo(log_tx: broadcast::Sender<String>) -> Result<()> {
    send_log!(log_tx, "🔄 Starting beacon update...");
    send_log!(log_tx, "");

    // Step 1: Sync repository
    send_log!(log_tx, "📦 Syncing package repository...");

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
        send_log!(log_tx, "❌ Repository sync failed!");
        send_log!(log_tx, "   {}", stderr);
        return Err(BeaconError::Installation(format!(
            "Repo sync failed: {}",
            stderr
        )));
    }

    send_log!(log_tx, "✅ Repository synced");
    send_log!(log_tx, "");

    // Step 2: Update xbps itself first (required before other updates)
    send_log!(log_tx, "📦 Updating xbps package manager...");

    let xbps_update = Command::new("xbps-install")
        .args(["-uy", "xbps"])
        .output()
        .await
        .map_err(|e| BeaconError::Installation(format!("Failed to update xbps: {}", e)))?;

    let xbps_stdout = String::from_utf8_lossy(&xbps_update.stdout);
    let xbps_stderr = String::from_utf8_lossy(&xbps_update.stderr);

    if !xbps_update.status.success() && !xbps_stderr.contains("up to date") {
        // Only fail if it's not already up to date
        if !xbps_stdout.contains("up to date") {
            error!("xbps update failed: {}", xbps_stderr);
            send_log!(log_tx, "⚠️  xbps update issue: {}", xbps_stderr);
        }
    }
    send_log!(log_tx, "✅ xbps updated");
    send_log!(log_tx, "");

    // Step 3: Check what would be updated (dry run)
    send_log!(log_tx, "🔍 Checking for beacon updates...");

    let check = Command::new("xbps-install")
        .args(["-n", "beacon"])
        .output()
        .await
        .map_err(|e| BeaconError::Installation(format!("Failed to check updates: {}", e)))?;

    let check_output = String::from_utf8_lossy(&check.stdout);
    info!("Update check output: {}", check_output);

    if check_output.contains("beacon") {
        send_log!(log_tx, "   Update available!");
    } else {
        send_log!(log_tx, "   Already at latest version");
    }
    send_log!(log_tx, "");

    // Step 4: Update beacon package
    send_log!(log_tx, "⬇️  Updating beacon package...");

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
        send_log!(log_tx, "❌ Update failed!");
        send_log!(log_tx, "   {}", update_stderr);
        return Err(BeaconError::Installation(format!(
            "Update failed: {}",
            update_stderr
        )));
    }

    send_log!(log_tx, "✅ Beacon package updated");
    send_log!(log_tx, "");

    // Step 5: Restart beacon service
    send_log!(log_tx, "🔄 Restarting beacon service...");

    // Note: This will kill our own process, so we might not see the response
    let restart = Command::new("sv")
        .args(["restart", "beacon"])
        .output()
        .await
        .map_err(|e| BeaconError::Installation(format!("Failed to restart beacon: {}", e)))?;

    let restart_stdout = String::from_utf8_lossy(&restart.stdout);
    let restart_stderr = String::from_utf8_lossy(&restart.stderr);

    if !restart.status.success() {
        // sv restart might return non-zero even on success, check output
        info!(
            "Restart command returned non-zero: stdout: {}, stderr: {}",
            restart_stdout, restart_stderr
        );
        send_log!(log_tx, "⚠️  Restart command output: {}", restart_stderr);
    }

    send_log!(log_tx, "");
    send_log!(log_tx, "✅ Beacon updated successfully!");
    send_log!(log_tx, "🔄 Service is restarting...");
    send_log!(log_tx, "");
    send_log!(log_tx, "🌟 Page will reload automatically in 3 seconds");

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
    use super::*;

    #[test]
    fn test_current_version() {
        let version = current_version();
        assert!(!version.is_empty());
        println!("Current beacon version: {}", version);
    }
}
