//! Package management via xbps

use crate::types::{PackageName, ServiceName};
use std::process::Stdio;
use thiserror::Error;
use tokio::process::Command;

#[derive(Debug, Error)]
pub enum PackageError {
    #[error("command failed: {0}")]
    CommandFailed(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Information about an installed package
#[derive(Debug, Clone, serde::Serialize)]
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
}

/// Information about an available update
#[derive(Debug, Clone, serde::Serialize)]
pub struct AvailableUpdate {
    pub name: String,
    pub current_version: String,
    pub new_version: String,
}

/// List all installed MDMA packages
pub async fn list_installed() -> Result<Vec<InstalledPackage>, PackageError> {
    let output = Command::new("xbps-query")
        .args(["-l"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(PackageError::CommandFailed(format!(
            "xbps-query -l failed: {}",
            stderr
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let packages: Vec<InstalledPackage> = stdout
        .lines()
        .filter_map(|line| {
            // Format: "ii package-name-version description"
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let name_version = parts[1];
                // Split "name-version" at last hyphen before version number
                if let Some(idx) = find_version_split(name_version) {
                    let name = &name_version[..idx];
                    let version = &name_version[idx + 1..];
                    // Filter to only MDMA packages
                    if name.starts_with("mdma-") || name == "beacon" {
                        return Some(InstalledPackage {
                            name: name.to_string(),
                            version: version.to_string(),
                        });
                    }
                }
            }
            None
        })
        .collect();

    Ok(packages)
}

/// Find the split point between package name and version
/// Returns the index of the hyphen before the version
fn find_version_split(s: &str) -> Option<usize> {
    // Version typically starts with a digit after a hyphen
    // Find the last hyphen followed by a digit
    let mut last_split = None;
    let chars: Vec<char> = s.chars().collect();
    for i in 0..chars.len().saturating_sub(1) {
        if chars[i] == '-' && chars[i + 1].is_ascii_digit() {
            last_split = Some(i);
        }
    }
    last_split
}

/// Check for available updates for MDMA packages
pub async fn check_updates() -> Result<Vec<AvailableUpdate>, PackageError> {
    // First sync the repository
    let sync = Command::new("xbps-install")
        .args(["-S"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await?;

    if !sync.status.success() {
        let stderr = String::from_utf8_lossy(&sync.stderr);
        tracing::warn!("xbps-install -S warning: {}", stderr);
        // Continue anyway - might work with cached data
    }

    // Check for updates (dry run)
    let output = Command::new("xbps-install")
        .args(["-Sun"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;

    // Exit code 17 means "nothing to do" which is fine
    if !output.status.success() && output.status.code() != Some(17) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(PackageError::CommandFailed(format!(
            "xbps-install -Sun failed: {}",
            stderr
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let updates: Vec<AvailableUpdate> = stdout
        .lines()
        .filter_map(|line| {
            // Format: "name-newversion update arch (current-version -> new-version)"
            // or: "name-version download ..."
            if !line.contains("update") {
                return None;
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 {
                let name_version = parts[0];
                if let Some(idx) = find_version_split(name_version) {
                    let name = &name_version[..idx];
                    let new_version = &name_version[idx + 1..];
                    // Filter to only MDMA packages
                    if name.starts_with("mdma-") || name == "beacon" {
                        // Try to extract current version from parentheses
                        let current = line
                            .find('(')
                            .and_then(|start| {
                                line[start..]
                                    .find(" ->")
                                    .map(|end| line[start + 1..start + end].trim().to_string())
                            })
                            .unwrap_or_else(|| "?".to_string());

                        return Some(AvailableUpdate {
                            name: name.to_string(),
                            current_version: current,
                            new_version: new_version.to_string(),
                        });
                    }
                }
            }
            None
        })
        .collect();

    Ok(updates)
}

/// Update a specific package
pub async fn update_package(pkg: &PackageName) -> Result<(), PackageError> {
    let output = Command::new("xbps-install")
        .args(["-uy", pkg.as_str()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(PackageError::CommandFailed(format!(
            "xbps-install -uy {} failed: {}",
            pkg, stderr
        )));
    }

    tracing::info!(package = %pkg, "Package updated");
    Ok(())
}

/// Restart a service
pub async fn restart_service(svc: &ServiceName) -> Result<(), PackageError> {
    let output = Command::new("sv")
        .args(["restart", svc.as_str()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(PackageError::CommandFailed(format!(
            "sv restart {} failed: {}",
            svc, stderr
        )));
    }

    tracing::info!(service = %svc, "Service restarted");
    Ok(())
}

/// Map package name to service name
pub fn package_to_service(pkg: &PackageName) -> Option<ServiceName> {
    // Most packages have matching service names
    ServiceName::new(pkg.as_str()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    #[rstest]
    #[case("mdma-console-0.1.3", Some(12))]
    #[case("beacon-0.5.3", Some(6))]
    #[case("mdma-library-0.1.0_1", Some(12))]
    fn version_split(#[case] input: &str, #[case] expected: Option<usize>) {
        assert_eq!(find_version_split(input), expected);
    }
}
