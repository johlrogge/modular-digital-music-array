// bases/beacon/src/hardware.rs
use crate::error::{BeaconError, Result};
use crate::types::{DevicePath, StorageBytes};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::fs;
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NvmeDrive {
    pub device: DevicePath,
    pub capacity: StorageBytes,
    pub model: Option<String>,
    pub is_formatted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HardwareInfo {
    pub model: String,
    pub serial: Option<String>,
    pub memory_mb: Option<u64>,
    pub nvme_drives: Vec<NvmeDrive>,
}

/// Detect all NVMe drives in the system
pub async fn detect_nvme_drives() -> Result<Vec<NvmeDrive>> {
    let mut drives = Vec::new();
    
    // Try nvme0n1, nvme1n1, nvme2n1, etc. until we don't find any
    for i in 0..10 {  // Support up to 10 NVMe drives (plenty for our use case)
        let device_path = format!("/dev/nvme{}n1", i);
        
        // Check if device exists
        if !Path::new(&device_path).exists() {
            // No more drives found
            if i == 0 {
                tracing::warn!("No NVMe drives detected (this is normal on development machines)");
            }
            break;
        }
        
        tracing::info!("Found NVMe drive: {}", device_path);
        
        // Get capacity using lsblk (doesn't need sudo)
        let capacity = match get_device_capacity(&device_path).await {
            Ok(cap) => cap,
            Err(e) => {
                tracing::warn!("Could not get capacity for {}: {} - skipping", device_path, e);
                continue;
            }
        };
        
        // Check if formatted by looking for existing partitions
        let is_formatted = has_partitions(&device_path).await.unwrap_or(false);
        
        // Try to get model name
        let nvme_name = format!("nvme{}", i);
        let model = get_device_model(&nvme_name).await.ok();
        
        drives.push(NvmeDrive {
            device: DevicePath::new(device_path),
            capacity: StorageBytes::new(capacity),
            model,
            is_formatted,
        });
    }
    
    tracing::info!("Detected {} NVMe drive(s)", drives.len());
    
    Ok(drives)
}

async fn get_device_capacity(device: &str) -> Result<u64> {
    // Use lsblk instead of blockdev - doesn't require sudo
    let output = Command::new("lsblk")
        .args(["-b", "-d", "-n", "-o", "SIZE", device])
        .output()
        .await
        .map_err(|e| BeaconError::HardwareInfo(format!("lsblk failed: {}", e)))?;
    
    if !output.status.success() {
        return Err(BeaconError::HardwareInfo(
            format!("lsblk command failed: {}", String::from_utf8_lossy(&output.stderr))
        ));
    }
    
    let size_str = String::from_utf8_lossy(&output.stdout);
    size_str
        .trim()
        .parse()
        .map_err(|e| BeaconError::HardwareInfo(format!("invalid size: {}", e)))
}

async fn has_partitions(device: &str) -> Result<bool> {
    // Check if device has partition table by looking for partition entries
    let output = Command::new("blkid")
        .arg(device)
        .output()
        .await
        .map_err(|e| BeaconError::HardwareInfo(format!("blkid failed: {}", e)))?;
    
    // If blkid succeeds, device has filesystem/partition table
    Ok(output.status.success())
}

async fn get_device_model(nvme_name: &str) -> Result<String> {
    let model_path = format!("/sys/class/nvme/{}/model", nvme_name);
    let model = fs::read_to_string(&model_path)
        .await
        .map_err(|e| BeaconError::HardwareInfo(format!("read model failed: {}", e)))?;
    
    Ok(model.trim().to_string())
}

/// Get Raspberry Pi model information
async fn get_pi_model() -> Result<String> {
    let model = fs::read_to_string("/proc/device-tree/model")
        .await
        .unwrap_or_else(|_| {
            // Not a Raspberry Pi, try to get generic system info
            std::fs::read_to_string("/sys/devices/virtual/dmi/id/product_name")
                .unwrap_or_else(|_| "Unknown (Development Machine)".to_string())
        });
    
    Ok(model.trim_end_matches('\0').to_string())
}

/// Get Raspberry Pi serial number
async fn get_pi_serial() -> Result<Option<String>> {
    let serial = fs::read_to_string("/proc/device-tree/serial-number")
        .await
        .ok()
        .map(|s| s.trim_end_matches('\0').to_string());
    
    Ok(serial)
}

/// Get system memory in MB
async fn get_memory_mb() -> Result<Option<u64>> {
    let meminfo = fs::read_to_string("/proc/meminfo")
        .await
        .ok();
    
    if let Some(content) = meminfo {
        for line in content.lines() {
            if line.starts_with("MemTotal:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    if let Ok(kb) = parts[1].parse::<u64>() {
                        return Ok(Some(kb / 1024));
                    }
                }
            }
        }
    }
    
    Ok(None)
}

/// Detect all hardware information
pub async fn detect_hardware() -> Result<HardwareInfo> {
    let model = get_pi_model().await?;
    let serial = get_pi_serial().await?;
    let memory_mb = get_memory_mb().await?;
    let nvme_drives = detect_nvme_drives().await?;
    
    Ok(HardwareInfo {
        model,
        serial,
        memory_mb,
        nvme_drives,
    })
}
// Add these functions to bases/beacon/src/hardware.rs
// (append to the end of the file, before the closing brace if any)

/// Check if we're running on a Raspberry Pi
/// 
/// Returns true only if we can confirm we're on a Pi.
/// This is a SAFETY CHECK to prevent accidental execution on dev machines.
/// 
/// # Examples
/// 
/// ```no_run
/// use beacon::hardware::is_raspberry_pi;
/// 
/// #[tokio::main]
/// async fn main() {
///     if is_raspberry_pi().await {
///         println!("Running on Raspberry Pi - safe to proceed");
///     } else {
///         println!("NOT on Raspberry Pi - refusing destructive operations");
///     }
/// }
/// ```
pub async fn is_raspberry_pi() -> bool {
    // Check for Raspberry Pi device tree
    if tokio::fs::metadata("/proc/device-tree/model").await.is_err() {
        tracing::debug!("No /proc/device-tree/model - not a Raspberry Pi");
        return false;
    }
    
    // Read model string
    let model = match tokio::fs::read_to_string("/proc/device-tree/model").await {
        Ok(m) => m,
        Err(e) => {
            tracing::debug!("Could not read device tree model: {}", e);
            return false;
        }
    };
    
    // Must contain "Raspberry Pi"
    let is_pi = model.contains("Raspberry Pi");
    
    if is_pi {
        tracing::info!("✅ Confirmed running on Raspberry Pi: {}", model.trim_end_matches('\0'));
    } else {
        tracing::warn!("❌ Not running on Raspberry Pi (model: {})", model.trim_end_matches('\0'));
    }
    
    is_pi
}

/// Require that we're on a Raspberry Pi, or return error
/// 
/// This is a CRITICAL SAFETY CHECK for destructive operations.
/// Use this at the start of any action that modifies hardware.
/// 
/// # Examples
/// 
/// ```no_run
/// use beacon::hardware::require_raspberry_pi;
/// use beacon::error::Result;
/// 
/// async fn dangerous_operation() -> Result<()> {
///     // SAFETY: Only proceed if we're on a Pi
///     require_raspberry_pi().await?;
///     
///     // Safe to proceed with destructive operations
///     Ok(())
/// }
/// ```
/// 
/// # Errors
/// 
/// Returns `BeaconError::Safety` if not running on a Raspberry Pi.
pub async fn require_raspberry_pi() -> Result<()> {
    if !is_raspberry_pi().await {
        return Err(BeaconError::Safety(
            "Not running on a Raspberry Pi! Refusing to execute destructive operations. \
             This safety check prevents accidentally running provisioning on development machines."
                .to_string()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod pi_safety_tests {
    use super::*;

    #[tokio::test]
    async fn test_is_raspberry_pi_detection() {
        // This test will pass or fail depending on the machine
        // On a Pi: should return true
        // On dev machine: should return false
        let result = is_raspberry_pi().await;
        
        // Just verify it returns without panicking
        // The actual value depends on the hardware
        println!("is_raspberry_pi() returned: {}", result);
    }

    #[tokio::test]
    async fn test_require_raspberry_pi() {
        let result = require_raspberry_pi().await;
        
        // On a Pi: should succeed
        // On dev machine: should fail with Safety error
        match result {
            Ok(()) => println!("Running on Raspberry Pi"),
            Err(e) => println!("Not on Raspberry Pi: {}", e),
        }
    }
}
