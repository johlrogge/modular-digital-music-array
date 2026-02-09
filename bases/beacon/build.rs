// bases/beacon/build.rs
//! Build script to capture git and build metadata at compile time

use std::process::Command;

fn main() {
    // Re-run if git HEAD changes (new commits)
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");

    // Get git commit hash (short)
    let git_hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Check if working directory is dirty
    let git_dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);

    let dirty_suffix = if git_dirty { "-dirty" } else { "" };

    // Get build timestamp (UTC)
    let build_time = chrono_lite_build_time();

    // Set environment variables for the build
    println!(
        "cargo:rustc-env=BUILD_GIT_HASH={}{}",
        git_hash, dirty_suffix
    );
    println!("cargo:rustc-env=BUILD_TIMESTAMP={}", build_time);
}

/// Simple build timestamp without external dependencies
fn chrono_lite_build_time() -> String {
    // Try to get timestamp from date command
    Command::new("date")
        .args(["-u", "+%Y-%m-%d %H:%M:%S UTC"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
