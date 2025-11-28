// bases/beacon/src/provisioning.rs
use crate::actions::{execute_action, ExecutionMode};
use crate::error::{BeaconError, Result};
use crate::hardware::HardwareInfo;
use crate::types::{ProvisionConfig, UnitType};
use tokio::process::Command;
use tracing::{info, warn};

/// Main provisioning orchestrator
pub async fn provision_system(
    config: ProvisionConfig,
    hardware: HardwareInfo,
    mode: ExecutionMode,
) -> Result<()> {
    info!("Starting provisioning for {} ({})", config.hostname, config.unit_type);
    
    if mode == ExecutionMode::DryRun {
        info!("🚧 DRY RUN MODE - No actual changes will be made");
    }
    
    // Validate hardware meets requirements
    validate_hardware(&config, &hardware)?;
    
    // Step 1: Partition drives
    info!("Partitioning NVMe drives...");
    partition_drives(&config, &hardware, mode).await?;
    
    // Step 2: Format partitions
    info!("Formatting partitions...");
    format_partitions(&config, &hardware, mode).await?;
    
    // Step 3: Install base system
    info!("Installing Void Linux base system...");
    install_base_system(&config, &hardware, mode).await?;
    
    // Step 4: Configure system
    info!("Configuring system...");
    configure_system(&config, &hardware, mode).await?;
    
    // Step 5: Setup SSH
    info!("Setting up SSH access...");
    setup_ssh(&config, mode).await?;
    
    // Step 6: Modify boot configuration to boot from NVMe
    info!("Updating boot configuration...");
    update_boot_config(&config, mode).await?;
    
    if mode == ExecutionMode::Apply {
        info!("Provisioning complete! System will reboot in 10 seconds...");
    } else {
        info!("🚧 DRY RUN complete - see logs above for what would have been done");
    }
    
    Ok(())
}

fn validate_hardware(config: &ProvisionConfig, hardware: &HardwareInfo) -> Result<()> {
    // All unit types need at least one NVMe drive
    if hardware.nvme_drives.is_empty() {
        return Err(BeaconError::HardwareInfo(
            "No NVMe drives found - cannot provision".to_string()
        ));
    }
    
    // MDMA-909 prefers two drives but can work with one
    if config.unit_type.requires_dual_nvme() && hardware.nvme_drives.len() >= 2 {
        info!("MDMA-909 with dual NVMe setup - optimal configuration");
    } else if config.unit_type.requires_dual_nvme() && hardware.nvme_drives.len() == 1 {
        warn!("MDMA-909 with single NVMe - will use combined partition layout");
        warn!("For optimal performance, add a second NVMe drive");
    }
    
    // Check if any drive is already formatted
    if hardware.nvme_drives.iter().any(|d| d.is_formatted) {
        warn!("Some NVMe drives appear to be formatted - will overwrite!");
    }
    
    Ok(())
}

async fn partition_drives(config: &ProvisionConfig, hardware: &HardwareInfo, mode: ExecutionMode) -> Result<()> {
    let primary_device = &hardware.nvme_drives[0].device;
    
    match config.unit_type {
        UnitType::Mdma909 => {
            if hardware.nvme_drives.len() >= 2 {
                // Dual NVMe setup - optimal
                info!("Using dual NVMe configuration");
                partition_primary_drive(primary_device, mode).await?;
                
                let secondary_device = &hardware.nvme_drives[1].device;
                partition_secondary_drive(secondary_device, mode).await?;
            } else {
                // Single NVMe setup - combined partitions
                info!("Using single NVMe configuration (combined layout)");
                partition_combined_drive(primary_device, mode).await?;
            }
        }
        UnitType::Mdma101 | UnitType::Mdma303 => {
            // Single NVMe setup
            partition_primary_drive(primary_device, mode).await?;
        }
    }
    
    Ok(())
}

async fn partition_primary_drive(device: &crate::types::DevicePath, _mode: ExecutionMode) -> Result<()> {
    info!("Partitioning primary drive: {}", device);
    
    // TODO: Use parted to create:
    // p1: 16GB  / (root)
    // p2: 8GB   /var
    // p3: 400GB /music
    // p4: rest  /metadata
    
    // Placeholder command
    let output = Command::new("echo")
        .arg(format!("Would partition {}", device))
        .output()
        .await
        .map_err(|e| BeaconError::Partitioning {
            device: device.to_string(),
            reason: e.to_string(),
        })?;
    
    if !output.status.success() {
        return Err(BeaconError::Partitioning {
            device: device.to_string(),
            reason: "partitioning command failed".to_string(),
        });
    }
    
    Ok(())
}

async fn partition_secondary_drive(device: &crate::types::DevicePath, _mode: ExecutionMode) -> Result<()> {
    info!("Partitioning secondary drive: {}", device);
    
    // TODO: Use parted to create:
    // p1: full disk /cdj-export
    
    // Placeholder command
    let output = Command::new("echo")
        .arg(format!("Would partition {}", device))
        .output()
        .await
        .map_err(|e| BeaconError::Partitioning {
            device: device.to_string(),
            reason: e.to_string(),
        })?;
    
    if !output.status.success() {
        return Err(BeaconError::Partitioning {
            device: device.to_string(),
            reason: "partitioning command failed".to_string(),
        });
    }
    
    Ok(())
}

async fn partition_combined_drive(device: &crate::types::DevicePath, _mode: ExecutionMode) -> Result<()> {
    info!("Partitioning combined drive (MDMA-909 single NVMe): {}", device);
    
    // TODO: Use parted to create:
    // p1: 16GB  / (root)
    // p2: 8GB   /var
    // p3: 200GB /music (smaller to make room for CDJ export)
    // p4: 200GB /cdj-export (AIFF cache)
    // p5: rest  /metadata
    
    // Placeholder command
    let output = Command::new("echo")
        .arg(format!("Would partition {} with combined layout (music + cdj-export on one drive)", device))
        .output()
        .await
        .map_err(|e| BeaconError::Partitioning {
            device: device.to_string(),
            reason: e.to_string(),
        })?;
    
    if !output.status.success() {
        return Err(BeaconError::Partitioning {
            device: device.to_string(),
            reason: "partitioning command failed".to_string(),
        });
    }
    
    Ok(())
}

async fn format_partitions(_config: &ProvisionConfig, hardware: &HardwareInfo, _mode: ExecutionMode) -> Result<()> {
    let primary_device = &hardware.nvme_drives[0].device;
    
    // Format all partitions as ext4
    for i in 1..=4 {
        let partition = format!("{}p{}", primary_device, i);
        info!("Formatting partition: {}", partition);
        
        // TODO: Actually run mkfs.ext4
        let output = Command::new("echo")
            .arg(format!("Would format {}", partition))
            .output()
            .await
            .map_err(|e| BeaconError::Formatting {
                partition: partition.clone(),
                reason: e.to_string(),
            })?;
        
        if !output.status.success() {
            return Err(BeaconError::Formatting {
                partition,
                reason: "format command failed".to_string(),
            });
        }
    }
    
    Ok(())
}

async fn install_base_system(_config: &ProvisionConfig, _hardware: &HardwareInfo, _mode: ExecutionMode) -> Result<()> {
    // TODO: Implement actual Void Linux installation
    // This would involve:
    // 1. Mount root partition
    // 2. Extract base system tarball
    // 3. Chroot and configure
    // 4. Install bootloader
    
    info!("Installing Void Linux base system (placeholder)");
    Ok(())
}

async fn configure_system(config: &ProvisionConfig, _hardware: &HardwareInfo, _mode: ExecutionMode) -> Result<()> {
    // TODO: Configure hostname, locale, timezone, etc.
    info!("Configuring hostname: {}", config.hostname);
    
    // Write hostname
    // Configure avahi
    // Set timezone
    
    Ok(())
}

async fn setup_ssh(config: &ProvisionConfig, _mode: ExecutionMode) -> Result<()> {
    // TODO: Write SSH authorized_keys file
    info!("Setting up SSH key: {}", config.ssh_key);
    
    // mkdir -p /mnt/root/.ssh
    // echo key > /mnt/root/.ssh/authorized_keys
    // chmod 600 /mnt/root/.ssh/authorized_keys
    
    Ok(())
}

async fn update_boot_config(_config: &ProvisionConfig, _mode: ExecutionMode) -> Result<()> {
    // TODO: Modify cmdline.txt to boot from NVMe
    info!("Updating boot configuration to use NVMe root");
    
    // Modify /boot/cmdline.txt:
    // root=/dev/nvme0n1p1
    
    Ok(())
}
