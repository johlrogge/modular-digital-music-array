# MDMA Beacon - Complete Structure

## Overview

The beacon is a self-contained HTTP server that runs on a minimal Void Linux SD card. It detects hardware, presents a web interface for configuration, and provisions the NVMe drives to create a fully bootable MDMA system.

## File Structure

```
mdma/
├── Cargo.toml                           # Updated with beacon member and dependencies
└── bases/
    └── beacon/
        ├── Cargo.toml                   # Beacon package manifest
        ├── src/
        │   ├── main.rs                  # Entry point with color_eyre setup
        │   ├── error.rs                 # BeaconError with thiserror
        │   ├── types.rs                 # Newtypes (Hostname, SshPublicKey, etc.)
        │   ├── hardware.rs              # NVMe detection and Pi info
        │   ├── provisioning.rs          # Partition, format, install logic
        │   └── server.rs                # Axum HTTP server
        └── templates/
            └── index.html               # Askama template for web UI
```

## Type Safety with Newtypes

All primitives are wrapped in newtypes for type safety:

- `Hostname` - Validates hostname format
- `SshPublicKey` - Validates SSH key format
- `UnitType` - Enum for MDMA-909/101/303
- `DevicePath` - Device paths like /dev/nvme0n1
- `StorageBytes` - Storage capacity with GB display

Invalid values are rejected at construction time, making illegal states unrepresentable.

## Error Handling Strategy

Following the rust-architect guidelines:

- **thiserror** for structured errors in library code (error.rs)
- **color_eyre** for rich error reports in main.rs
- Explicit error contexts with field interpolation
- `#[from]` conversions for common error types

## Module Responsibilities

### main.rs
- Installs color_eyre and tracing
- Detects hardware
- Starts HTTP server

### error.rs
- Defines BeaconError enum
- Specific variants for each failure mode
- Custom Result type alias

### types.rs
- Newtypes for all domain primitives
- Validation at construction
- Display implementations
- Serde support for form handling

### hardware.rs
- Scans /sys/class/nvme for drives
- Uses blockdev for capacity
- Reads Raspberry Pi model/serial
- Returns HardwareInfo struct

### provisioning.rs
- Orchestrates full provisioning flow
- Validates hardware requirements
- Partitions drives (parted)
- Formats filesystems (mkfs.ext4)
- Installs base system
- Configures hostname and SSH
- Updates boot config for NVMe root

### server.rs
- Axum HTTP server on port 80
- Index route renders Askama template
- Provision route validates and spawns background task
- AppState holds detected hardware
- Custom AppError for HTTP error responses

## Current State

✅ Complete type-safe structure
✅ Hardware detection
✅ Web UI with gradient styling
✅ Form validation with newtypes
⚠️  Provisioning logic is placeholder (TODO comments)

## Next Steps

1. **Implement Actual Provisioning**
   - Real parted commands for partitioning
   - Real mkfs.ext4 for formatting
   - Base system extraction from tarball
   - Chroot configuration
   - Boot config modification

2. **Create Askama Template Directory**
   ```bash
   mkdir -p bases/beacon/templates
   mv index.html bases/beacon/templates/
   ```

3. **GitHub Actions Workflow**
   - Build beacon package with xbps-src
   - Publish to GitHub Pages
   - Serve as Void Linux repository

4. **Justfile Integration**
   - `just bootstrap-sdcard` - Creates bootable image
   - `just flash-sdcard /dev/sdX` - Writes to physical media

5. **Testing Strategy**
   - Unit tests for newtype validation
   - Integration tests for hardware detection
   - Mock provisioning for CI

## Usage Flow

1. User flashes SD card with beacon image
2. Boots Raspberry Pi 5 with NVMe drives
3. Navigates to http://welcome-to-mdma.local
4. Fills form: unit type, hostname, SSH key
5. Clicks "Provision System"
6. Beacon partitions, formats, installs to NVMe
7. Updates boot config to use NVMe root
8. Reboots
9. System comes up on NVMe as configured hostname
10. User SSH in and runs Ansible for MDMA software

## Design Principles Applied

- **Type-Driven Design**: Newtypes prevent value mixing
- **Error Handling**: Structured errors with context
- **Incremental Development**: Placeholders for complex logic
- **Separation of Concerns**: Each module has clear responsibility
- **Zero-Cost Abstractions**: Newtypes compile to primitives
- **Fail-Fast**: Validation at construction time

## Dependencies Added to Workspace

- `axum = "0.7"` - HTTP server framework
- `askama = "0.12"` - Type-safe HTML templates
- `tower-http = { version = "0.5", features = ["fs"] }` - Static file serving
- `thiserror = "1.0"` - Structured error types (already in workspace)

## Port and Hostname

- **Port**: 80 (no need for :8080 suffix)
- **Hostname**: `welcome-to-mdma.local` (via Avahi)
- Accessible on local network immediately after boot

---

**Rusty McRustface**: This structure is ready to integrate into your MDMA workspace! 
All the type safety, error handling, and architecture patterns are in place. 
The provisioning logic just needs the actual command implementations.
