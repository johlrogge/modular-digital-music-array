// bases/beacon/src/hardware.rs
use crate::error::{BeaconError, Result};
use crate::types::{DevicePath, StorageBytes};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::fs;
use tokio::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NvmeDrive {
    pub device: DevicePath,
    pub capacity: StorageBytes,
    pub model: Option<String>,
    pub is_formatted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
