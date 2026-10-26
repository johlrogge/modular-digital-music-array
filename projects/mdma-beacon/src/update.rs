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
        .args(["-n", "-u", "beacon"])
        .output()
        .await
        .map_err(|e| BeaconError::Installation(format!("Failed to check updates: {}", e)))?;

    let check_stdout = String::from_utf8_lossy(&check.stdout);
    let check_stderr = String::from_utf8_lossy(&check.stderr);
    info!("Update check output: {}", check_stdout);

    match classify_dry_run(check.status.success(), &check_stdout, &check_stderr) {
        DryRunOutcome::UpdateAvailable(line) => info!("Update available: {}", line),
        DryRunOutcome::UpToDate => {
            info!("Already at latest version");
            return Ok(());
        }
        DryRunOutcome::Failed(stderr) => {
            error!("Update check failed: {}", stderr);
            return Err(BeaconError::Installation(format!(
                "Update check failed: {}",
                stderr
            )));
        }
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

/// Result of interpreting a `xbps-install -n -u beacon` dry run
#[derive(Debug, PartialEq, Eq)]
pub enum DryRunOutcome {
    /// xbps wants to update beacon; carries the relevant output line
    UpdateAvailable(String),
    /// xbps succeeded and has nothing to do for beacon
    UpToDate,
    /// xbps failed (non-zero exit, package not found, unresolved deps); carries stderr
    Failed(String),
}

/// Classify the result of a `xbps-install -n -u beacon` dry run.
pub fn classify_dry_run(status_success: bool, stdout: &str, stderr: &str) -> DryRunOutcome {
    if !status_success || stderr.contains("not found") || stderr.contains("MISSING:") {
        let message = if !stderr.trim().is_empty() {
            stderr.to_string()
        } else if !stdout.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            "xbps-install exited with an error".to_string()
        };
        return DryRunOutcome::Failed(message);
    }

    stdout
        .lines()
        .find(|line| is_beacon_transaction_line(line))
        .map_or(DryRunOutcome::UpToDate, |line| {
            DryRunOutcome::UpdateAvailable(line.trim().to_string())
        })
}

/// True for a transaction line like `beacon-0.27.1_1 update aarch64 <url> 1 2`:
/// first field is `beacon-<digit>...` (pkgver) and second is `update` or `install`.
fn is_beacon_transaction_line(line: &str) -> bool {
    let mut fields = line.split_whitespace();
    let is_beacon_pkg = fields
        .next()
        .and_then(|pkg| pkg.strip_prefix("beacon-"))
        .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()));
    is_beacon_pkg && matches!(fields.next(), Some("update" | "install"))
}

#[cfg(test)]
mod tests {
    use super::{classify_dry_run, DryRunOutcome};
    use rstest::rstest;

    #[rstest]
    #[case::update_available(
        true,
        "beacon-0.27.0_1 update aarch64 https://example.org/aarch64 123 456\n",
        "",
        DryRunOutcome::UpdateAvailable(
            "beacon-0.27.0_1 update aarch64 https://example.org/aarch64 123 456".to_string()
        )
    )]
    #[case::empty_success(true, "", "", DryRunOutcome::UpToDate)]
    #[case::not_found(
        false,
        "",
        "ERROR: Package 'beacon' not found in repository pool.\n",
        DryRunOutcome::Failed("ERROR: Package 'beacon' not found in repository pool.\n".to_string())
    )]
    #[case::not_found_exit_zero(
        true,
        "",
        "ERROR: Package 'beacon' not found in repository pool.\n",
        DryRunOutcome::Failed("ERROR: Package 'beacon' not found in repository pool.\n".to_string())
    )]
    #[case::missing_dependency(
        false,
        "",
        "MISSING: avahi-daemon>=0\nERROR: Transaction aborted due to unresolved dependencies.\n",
        DryRunOutcome::Failed(
            "MISSING: avahi-daemon>=0\nERROR: Transaction aborted due to unresolved dependencies.\n"
                .to_string()
        )
    )]
    #[case::multi_line_beacon_after_other(
        true,
        "avahi-0.8_2 install aarch64 https://example.org/aarch64 1 2\nbeacon-0.27.1_1 update aarch64 https://johlrogge.github.io/modular-digital-music-array/aarch64 1 2\n",
        "",
        DryRunOutcome::UpdateAvailable(
            "beacon-0.27.1_1 update aarch64 https://johlrogge.github.io/modular-digital-music-array/aarch64 1 2"
                .to_string()
        )
    )]
    #[case::other_package_with_beacon_in_url(
        true,
        "avahi-0.8_2 update aarch64 https://example.org/beacon/aarch64 1 2\n",
        "",
        DryRunOutcome::UpToDate
    )]
    #[case::failure_message_from_stdout(
        false,
        "ERROR: something went wrong\n",
        "",
        DryRunOutcome::Failed("ERROR: something went wrong".to_string())
    )]
    #[case::failure_without_any_output(
        false,
        "",
        "  \n",
        DryRunOutcome::Failed("xbps-install exited with an error".to_string())
    )]
    fn classifies_dry_run(
        #[case] success: bool,
        #[case] stdout: &str,
        #[case] stderr: &str,
        #[case] expected: DryRunOutcome,
    ) {
        assert_eq!(classify_dry_run(success, stdout, stderr), expected);
    }

    #[test]
    fn current_version() {
        let version = super::current_version();
        assert!(!version.is_empty());
        println!("Current beacon version: {}", version);
    }
}
