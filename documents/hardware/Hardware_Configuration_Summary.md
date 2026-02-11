# Hardware Configuration Summary

## Core Components

### Computing Platform
- Raspberry Pi 5 (Non-CM version)
- 16GB LPDDR5 RAM
  - Enables larger audio buffers
  - Better multi-room streaming capability
  - Reduced storage I/O needs
  - Room for sophisticated audio processing

### Storage Architecture
1. System Drive (SD Card)
   - 32GB High-Quality SD Card
   - Requirements:
     * A2 rating for random I/O
     * UHS-I Speed Class 3 or better
     * Reputable brand (Samsung PRO, SanDisk Extreme PRO)
   - Justification: 
     * Cost-effective with minimal write operations
     * Easy to backup and replace
     * Works well with NixOS immutable design

2. Data Storage (NVMe)
   - Capacity: 512GB minimum
   - Usage Distribution:
     * Music Library: ~300GB
     * Vinyl Rips Cache: ~100GB
     * Working Space: ~50GB
     * Network Cache: ~50GB
   - Requirements:
     * Good sustained write performance
     * High reliability rating
     * Temperature management capability

### Audio Output
- Primary: ifi zen 3 DAC over USB
- Secondary: HifiBerry DAC support
- Considerations:
  * Clean power supply
  * Short, high-quality USB cables
  * EMI shielding if needed

## System Optimization

### Storage Configuration
1. SD Card Optimization
   - Mount options: noatime, nodiratime
   - Minimized journaling
   - Logs redirected to NVMe
   - Regular automated backups

2. NVMe Configuration
   - Separate partitions for:
     * Music library
     * Cache
     * Backups
     * Working space
   - Optimized I/O scheduler for audio

### Memory Management
- No swap configuration (16GB RAM sufficient)
- Optimized cache pressure
- Large file system cache for music files
- Dedicated audio processing buffers

### Network Configuration
- Gigabit Ethernet
- Network Requirements:
  * ~1.5Mbps per FLAC stream
  * ~20Mbps total for 10-room setup
  * Low latency prioritization

## Power and Thermal Considerations

### Power Supply
- Requirements:
  * Clean 5V supply
  * Minimum 3A capacity
  * Low noise characteristics
  * Stable output under load

### Thermal Management
1. Active Cooling
   - Low-noise fan solution
   - Temperature-controlled operation
   - Good airflow design

2. Thermal Monitoring
   - CPU temperature monitoring
   - Thermal throttling prevention
   - Performance optimization

## Backup and Reliability

### System Backup
1. Regular SD Card Images
   - Stored on NVMe
   - Weekly automated backups
   - Documented restore procedure

2. Music Library Backup
   - Regular incremental backups
   - Checksums for file integrity
   - Version control for playlists

### Fault Tolerance
- Power loss protection
- Filesystem journaling
- Error detection and recovery
- Automated system health checks

## Future Expansion Considerations

### Hardware Upgrades
- Additional storage capacity
- External DAC options
- Network infrastructure
- Backup power systems

### Multi-Room Capability
- Network bandwidth reservation
- Synchronized playback
- Room-specific audio processing
- Distributed cache management