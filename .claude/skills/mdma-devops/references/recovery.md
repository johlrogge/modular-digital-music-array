# Recovery and Troubleshooting

## Recovery Philosophy

MDMA systems follow the "scorched earth" recovery principle:
- Music and metadata are sacred (separate partitions)
- OS is disposable (can reinstall in minutes)
- Updates can break things (rollback must be trivial)
- Hardware can fail (recovery procedures are documented)

## Common Recovery Scenarios

### Scenario 1: Corrupted OS / System Won't Boot

**Symptoms:**
- Unit doesn't respond to network
- Boot loops or kernel panic
- Filesystem corruption on root partition

**Recovery:**

1. **Prepare fresh SD card**:
```bash
just provision-sd --hostname mdma-909-studio --role 909 --device /dev/sdX
```

2. **Remove SD card from unit, insert fresh one**

3. **Boot unit** (enters beacon mode)

4. **Discover unit**:
```bash
just discover-units
# Should find: mdma-909-studio.local (needs provisioning)
```

5. **Reprovision via beacon**:
```bash
just provision mdma-909-studio.local
```

This reformats OS partition but preserves `/music` and `/metadata`.

6. **Verify music intact**:
```bash
ssh mdma@mdma-909-studio.local 'ls -lh /music'
```

**Recovery time:** 10-15 minutes

### Scenario 2: Failed Package Update

**Symptoms:**
- Services won't start after update
- Health check fails
- Unit functional but MDMA not working

**Recovery:**

1. **Check snapshot history**:
```bash
ssh mdma@mdma-909-studio.local
ls -lh /var/lib/mdma/snapshots/
```

2. **View what changed**:
```bash
LATEST=$(ls -t /var/lib/mdma/snapshots/pre-update-*.txt | head -1)
PREVIOUS=$(ls -t /var/lib/mdma/snapshots/pre-update-*.txt | head -2 | tail -1)
diff "$PREVIOUS" "$LATEST"
```

3. **Downgrade problematic package**:
```bash
# If mdma-909 is broken, find previous version
xbps-query -R mdma-909

# Force install previous version
sudo xbps-install -f mdma-909-0.0.9_1
```

4. **Verify health**:
```bash
mdma-health-check
```

5. **Restart services**:
```bash
sudo sv restart mdma-909
```

**Recovery time:** 2-5 minutes

### Scenario 3: NVMe Drive Failure

**Symptoms:**
- System boots but mounts fail
- I/O errors in dmesg
- Unit accessible but music unavailable

**Recovery:**

#### If Primary NVMe Failed (OS + Music + Metadata)

**Critical:** This requires restoring music from backup. Ensure backups exist.

1. **Replace NVMe drive**

2. **Boot from SD card** (will enter beacon mode - no root partition)

3. **Provision new drive**:
```bash
just provision mdma-909-studio.local
```

4. **Restore music library**:
```bash
# From backup drive or network location
rsync -avP /mnt/backup/music/ mdma@mdma-909-studio.local:/music/
rsync -avP /mnt/backup/metadata/ mdma@mdma-909-studio.local:/metadata/
```

**Recovery time:** 15-30 minutes + music transfer time

#### If Secondary NVMe Failed (CDJ Export Only - MDMA-909)

**Good news:** This is a cache, can be regenerated.

1. **Replace NVMe drive**

2. **SSH to unit**:
```bash
ssh mdma@mdma-909-studio.local
```

3. **Partition and format new drive**:
```bash
sudo parted /dev/nvme0n2 mklabel gpt
sudo parted /dev/nvme0n2 mkpart primary ext4 0% 100%
sudo mkfs.ext4 -L mdma-cdj-export /dev/nvme0n2p1
```

4. **Mount and update fstab**:
```bash
sudo mount /dev/nvme0n2p1 /cdj-export
# fstab should auto-mount on reboot (already configured)
```

5. **Regenerate CDJ export cache**:
```bash
mdma-cdj-export-rebuild
# This transcodes FLAC → AIFF for entire library
```

**Recovery time:** 5 minutes + transcode time (runs in background)

### Scenario 4: SD Card Failure

**Symptoms:**
- Unit won't boot at all
- Red LED pattern indicates boot failure

**Recovery:**

1. **Write fresh SD card** (keep old one for data forensics if needed):
```bash
just provision-sd --hostname mdma-909-studio --role 909 --device /dev/sdX
```

2. **Insert in unit and boot**

3. **System should boot normally** from NVMe root
   - SD card only contains bootloader
   - All OS and data on NVMe

**Recovery time:** 5 minutes

### Scenario 5: Complete Hardware Failure

**Symptoms:**
- Raspberry Pi 5 hardware failure
- Power supply failure
- Multiple components failed

**Recovery:**

1. **Acquire replacement Raspberry Pi 5**

2. **Remove NVMe drives from failed unit**

3. **Write fresh SD card**:
```bash
just provision-sd --hostname mdma-909-studio --role 909 --device /dev/sdX
```

4. **Install NVMe drives in new unit**

5. **Boot - system should work immediately**
   - SD card has bootloader
   - NVMe has complete system
   - All data intact

6. **Verify**:
```bash
ssh mdma@mdma-909-studio.local 'mdma-health-check'
```

**Recovery time:** 10 minutes

### Scenario 6: Music Library Corruption

**Symptoms:**
- Database errors
- Missing tracks
- Corrupted ACID fact streams

**Recovery:**

#### Option A: Restore from Backup
```bash
rsync -avP /mnt/backup/metadata/ mdma@mdma-909-studio.local:/metadata/
```

#### Option B: Rebuild from Source Files
```bash
ssh mdma@mdma-909-studio.local

# Backup corrupted metadata
sudo mv /metadata /metadata.corrupted.$(date +%s)
sudo mkdir /metadata
sudo chown mdma:mdma /metadata

# Rebuild ACID database from music files
mdma-library-rebuild --scan /music
```

**Recovery time:** 5 minutes + scan time (depends on library size)

## Disaster Recovery: Complete Rebuild

**Worst case:** Everything is lost except music files on external backup.

1. **Provision SD card**:
```bash
just provision-sd --hostname mdma-909-studio --role 909 --device /dev/sdX
```

2. **Install fresh NVMe drive(s)**

3. **Boot and provision**:
```bash
just discover-units
just provision mdma-909-studio.local
```

4. **Restore music**:
```bash
rsync -avP /mnt/backup/music/ mdma@mdma-909-studio.local:/music/
```

5. **Rebuild metadata**:
```bash
ssh mdma@mdma-909-studio.local 'mdma-library-rebuild --scan /music'
```

6. **Verify**:
```bash
ssh mdma@mdma-909-studio.local 'mdma-health-check'
```

**Recovery time:** 20-30 minutes + music transfer time

## Troubleshooting Guide

### Unit Not Responding to Network

**Check:**
1. Physical network connection (cable plugged in?)
2. Router DHCP logs (did unit get IP?)
3. mDNS resolution: `ping mdma-909-studio.local`

**Fix:**
```bash
# Connect monitor and keyboard to unit
# Check network status
ip addr show
systemctl status NetworkManager  # or dhcpcd

# Restart networking
sudo sv restart dhcpcd
```

### Audio Device Not Detected

**Check:**
```bash
ssh mdma@mdma-909-studio.local
aplay -l    # List audio devices
lsusb       # Check USB audio interfaces
dmesg | grep -i audio
```

**Fix:**
```bash
# Reload ALSA
sudo alsa reload

# Check runit service
sudo sv restart mdma-909
```

### NFS Export Not Working (MDMA-909)

**Check:**
```bash
ssh mdma@mdma-909-studio.local

# Is NFS server running?
sudo sv status nfs-server

# Is export configured?
cat /etc/exports

# Test export locally
showmount -e localhost
```

**Fix:**
```bash
# Restart NFS
sudo sv restart nfs-server
sudo sv restart rpcbind

# Re-export
sudo exportfs -ra
```

### mDNS Not Broadcasting

**Check:**
```bash
ssh mdma@mdma-909-studio.local

# Is Avahi running?
sudo sv status avahi-daemon

# Check configuration
cat /etc/avahi/avahi-daemon.conf

# Test broadcast
avahi-browse -at
```

**Fix:**
```bash
# Restart Avahi
sudo sv restart avahi-daemon

# Verify hostname
cat /etc/hostname
```

### Partition Won't Mount

**Check:**
```bash
ssh mdma@mdma-909-studio.local

# List all block devices
lsblk

# Check fstab
cat /etc/fstab

# Try manual mount
sudo mount /dev/nvme0n1p3 /music
```

**Fix:**
```bash
# Check filesystem
sudo fsck.ext4 /dev/nvme0n1p3

# If corrupted beyond repair, reformat
sudo mkfs.ext4 -L mdma-music /dev/nvme0n1p3

# Restore from backup
rsync -avP /mnt/backup/music/ /music/
```

### Package Database Corrupted

**Symptoms:**
- `xbps-install` fails with database errors
- Package queries return garbage

**Fix:**
```bash
ssh mdma@mdma-909-studio.local

# Rebuild package database
sudo xbps-install -S
sudo xbps-pkgdb -a

# If still broken, remove cache
sudo rm -rf /var/cache/xbps/*
sudo xbps-install -S
```

## Backup Best Practices

### What to Backup

**Critical (must backup):**
- `/music` - Music library (source of truth)
- `/metadata` - ACID fact streams, database
- SSH keys (`~/.ssh/mdma_deploy_ed25519`)

**Important (should backup):**
- Package snapshots (`/var/lib/mdma/snapshots/`)
- Configuration files (`/etc/mdma/`)

**Not needed (can regenerate):**
- `/cdj-export` - Cache, rebuilt from `/music`
- `/` - OS, reinstalled via beacon provisioning
- Package cache

### Backup Strategies

#### Strategy 1: External Drive
```bash
# Weekly backup via cron
rsync -avP --delete /music/ /mnt/backup/music/
rsync -avP --delete /metadata/ /mnt/backup/metadata/
```

#### Strategy 2: Network Backup to NAS
```bash
# Daily incremental backup
rsync -avP --delete /music/ nas.local:/mdma-backups/music/
rsync -avP --delete /metadata/ nas.local:/mdma-backups/metadata/
```

#### Strategy 3: ZFS Snapshots (Advanced)
If using ZFS on NVMe:
```bash
# Daily snapshots
zfs snapshot tank/music@$(date +%Y%m%d)
zfs snapshot tank/metadata@$(date +%Y%m%d)

# Restore from snapshot
zfs rollback tank/music@20240315
```

### Testing Recovery Procedures

**Quarterly drill:**
1. Write fresh SD card
2. Install spare NVMe
3. Provision from scratch
4. Restore from backup
5. Verify music playback
6. Time the process
7. Document any issues

This ensures recovery procedures actually work when needed.

## Emergency Contact Information

**When all else fails:**

1. Review this documentation
2. Check project knowledge in Claude.ai
3. Review MDMA GitHub issues
4. SSH to unit and examine logs: `journalctl -xe`
5. Take deep breath, you've got this

Remember: The music is safe on separate partitions. Everything else is just code and configuration, which can be rebuilt.
