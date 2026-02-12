# MDMA System Architecture

## Boot Strategy: SD Card + NVMe

MDMA units use a two-device boot architecture that separates boot firmware from the working system:

- **SD Card**: Minimal, read-mostly boot firmware
- **NVMe Drive(s)**: All runtime operations, OS, data, and writes

This separation provides:
- Extended SD card lifespan (no write wear)
- Fast NVMe performance for all operations
- Easy recovery (swap SD card, reinstall OS to NVMe)
- Data persistence across OS reinstalls

## Boot Flow

1. Raspberry Pi 5 boots from SD card `/boot`
2. Bootloader loads kernel and initramfs
3. Kernel mounts NVMe root filesystem at `/`
4. System continues boot from NVMe
5. Additional NVMe partitions mount (`/var`, `/music`, `/metadata`, `/cdj-export`)

## Disk Layouts by Unit Type

### MDMA-101 and MDMA-303 (Single NVMe)

**SD Card:**
```
/dev/mmcblk0p1  → /boot       (512MB, vfat, bootloader + kernel)
/dev/mmcblk0p2  → /boot-spare (512MB, vfat, backup boot partition)
```

**NVMe Drive (512GB):**
```
/dev/nvme0n1p1  → /           (16GB, ext4, operating system)
/dev/nvme0n1p2  → /var        (8GB, ext4, logs/journals/temp)
/dev/nvme0n1p3  → /music      (400GB, ext4, FLAC library)
/dev/nvme0n1p4  → /metadata   (88GB, ext4, ACID fact streams)
```

### MDMA-909 (Dual NVMe with CDJ Export)

**SD Card:** Same as above

**Primary NVMe (512GB):**
```
/dev/nvme0n1p1  → /           (16GB, ext4, operating system)
/dev/nvme0n1p2  → /var        (8GB, ext4, logs/journals/temp)
/dev/nvme0n1p3  → /music      (400GB, ext4, FLAC library - source of truth)
/dev/nvme0n1p4  → /metadata   (88GB, ext4, ACID fact streams)
```

**Secondary NVMe (512GB):**
```
/dev/nvme0n2p1  → /cdj-export (512GB, ext4, AIFF/WAV cache for CDJs)
```

The secondary drive serves transcoded audio for CDJ consumption via NFS export.

## Partition Rationale

### `/` (16GB)
Base Void Linux installation, MDMA binaries, system packages. Sized for full OS with headroom.

### `/var` (8GB)
All write-heavy operations:
- System logs (`/var/log`)
- Journal files
- Temporary files
- Package cache

Separate partition prevents log accumulation from filling root.

### `/music` (400GB)
FLAC music library. Immutable source of truth. Survives OS reinstalls.

Storage estimate:
- Average FLAC album: 300-400MB
- 400GB ≈ 1,000-1,300 albums
- Sufficient for typical home DJ library

### `/metadata` (88GB)
ACID fact streams, database files, search indices. Survives OS reinstalls.

### `/cdj-export` (512GB, MDMA-909 only)
Transcoded AIFF/WAV cache for CDJ network shares.

Capacity planning:
- FLAC album: ~350MB
- AIFF album: ~700MB (2x FLAC size)
- 512GB allows full library export with headroom
- Can be regenerated from `/music` if corrupted

## Write Endurance Strategy

All high-frequency writes target NVMe, not SD card:

**SD Card** (read-mostly):
- Bootloader updates (rare)
- Kernel updates (occasional)
- No runtime writes

**NVMe** (write-heavy):
- System logs
- Temporary files
- Database transactions
- Audio transcoding

This extends SD card lifespan from months to years.

## Recovery Scenarios

### Corrupted OS
1. Boot fails or system unstable
2. Write fresh SD card (2 minutes)
3. Insert blank NVMe or reformat existing
4. Boot, provision via Ansible
5. Music and metadata intact on original NVMe partitions

### Failed NVMe
1. Replace NVMe drive
2. Boot from existing SD card (enters beacon mode)
3. Provision via Ansible
4. Restore music library from backup

### Dual-NVMe Recovery (MDMA-909)
1. Primary NVMe failure: Replace, reprovision, library intact on secondary
2. Secondary NVMe failure: Replace, regenerate `/cdj-export` from primary
3. Both failed: Sequential recovery, restore music from external backup

## Network Topology

All units announce via mDNS (Avahi):
- `mdma-909-studio.local`
- `mdma-101-bedroom.local`
- `mdma-303-kitchen.local`

MDMA-909 additionally exports NFS share:
- Share: `/cdj-export`
- Protocol: NFSv3 (CDJ-compatible)
- Access: Read-only, local network only
- Clients: CDJ-900, CDJ-2000, or other MDMA units
