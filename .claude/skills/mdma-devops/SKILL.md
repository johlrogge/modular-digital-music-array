---
name: mdma-devops
description: Complete DevOps workflow for MDMA (Modular Distributed Music Architecture) deployment on Raspberry Pi 5 with Void Linux. Covers beacon provisioning, golden image creation, SD card setup, NVMe drive provisioning, Ansible automation, package building and distribution, system updates, and recovery procedures. Implements the "deploy first" philosophy where code runs on live units within minutes.
---

# MDMA DevOps

Production deployment infrastructure for MDMA music system on Raspberry Pi 5 hardware.

## Core Principles

**Deploy First Philosophy:** Code should run on live units within minutes (even seconds) of being written. This requires automated builds, minimal manual intervention, and safe rollback capabilities.

**Iterative Development:** Small steps, test on real hardware, validate before scaling. Don't pursue golden images or automation until the system is proven stable on actual Raspberry Pi hardware.

**Separation of Concerns:**
- **SD Card:** Minimal bootloader (beacon mode) or OS recovery
- **NVMe:** All runtime operations, OS, data, high-write activities  
- **Music/Metadata:** Separate partitions, survive OS reinstalls

**Ansible vs Packages:**
- **Void Packages:** Application code, binaries, services (changes with git commits)
- **Ansible:** System topology, configuration, provisioning (defines system shape)

## Quick Start - Beacon Provisioning

### Method 1: Setup Script (Current Recommended)

**Fastest and most reliable for initial development:**

```bash
# 1. Flash vanilla Void Linux
xz -dc ~/mdma-images/void-rpi-aarch64-20250202.img.xz | \
  sudo dd of=/dev/sdX bs=4M status=progress conv=fsync

# 2. Boot Pi, find it on network
just pi-scan

# 3. Copy and run setup script
scp setup-beacon-on-pi.sh root@<IP>:/root/
ssh root@<IP>
./setup-beacon-on-pi.sh

# 4. Test
ping welcome-to-mdma.local
http://welcome-to-mdma.local/

# Total time: ~5 minutes
```

**When to use:** Development, testing, iterating on beacon functionality

**Benefits:**
- ✅ Runs on real hardware (no chroot issues)
- ✅ Services actually start and can be tested
- ✅ Fast iteration (5 minutes per Pi)
- ✅ Easy to debug
- ✅ Supervise symlinks created correctly

### Method 2: Golden Image (Future)

**For production deployment when beacon is stable:**

```bash
# 1. Create working beacon on Pi (Method 1)
# 2. Test thoroughly
# 3. Shutdown Pi
# 4. Create image from SD card
just golden-create-image /dev/sdX

# 5. Distribute to unlimited Pis
xz -dc golden-image.img.xz | sudo dd of=/dev/sdX bs=4M status=progress
```

**When to use:** Production, distributing to users, scaling

**Benefits:**
- ✅ No configuration needed per Pi
- ✅ Guaranteed working (tested before imaging)
- ✅ Fast deployment (10-15 minutes)
- ✅ Consistent across all units

**Important:** Don't create golden images until beacon is feature-complete and thoroughly tested!

## Network Discovery

### Find Raspberry Pi Devices

```bash
# Full scan (recommended, ~30 seconds)
just pi-scan

# Quick scan (requires arp-scan)
just pi-scan-quick

# Find and auto-connect
just pi-connect

# Wait for Pi to appear
just pi-wait

# Check specific IP
just pi-check 192.168.0.164
```

**Requires:** `nmap` (and optionally `arp-scan`)

```bash
sudo pacman -S nmap arp-scan
```

### mDNS Resolution

**On Arch Linux development machine:**

```bash
# Install nss-mdns
sudo pacman -S nss-mdns

# Configure NSS
sudo sed -i 's/^hosts:.*/hosts: files mymachines mdns_minimal [NOTFOUND=return] resolve [!UNAVAIL=return] dns/' /etc/nsswitch.conf

# Restart avahi
sudo systemctl restart avahi-daemon

# Now .local domains work
ping welcome-to-mdma.local
```

## Beacon Setup Script

### Script Requirements

The `setup-beacon-on-pi.sh` script must:

1. **Update system**
   ```bash
   xbps-install -Suy xbps
   xbps-install -Suy
   ```

2. **Configure MDMA repository**
   ```bash
   echo "repository=https://johlrogge.github.io/modular-digital-music-array/aarch64" \
     > /etc/xbps.d/10-mdma-repo.conf
   xbps-install -S
   ```

3. **Install packages explicitly** (don't rely on dependency resolution in chroot)
   ```bash
   xbps-install -y dbus
   xbps-install -y avahi
   xbps-install -y beacon
   ```

4. **Set hostname**
   ```bash
   echo "welcome-to-mdma" > /etc/hostname
   hostname welcome-to-mdma
   ```

5. **Enable services** (create symlinks)
   ```bash
   ln -sf /etc/sv/dbus /var/service/
   ln -sf /etc/sv/avahi-daemon /var/service/
   ln -sf /etc/sv/beacon /var/service/
   ```

6. **Wait for runsvdir** (critical!)
   ```bash
   sleep 5  # Let runsvdir create supervise directories
   ```

7. **Restart services**
   ```bash
   sv restart dbus avahi-daemon beacon
   ```

8. **Verify everything**
   ```bash
   sv status dbus avahi-daemon beacon
   ss -tulpn | grep :80
   hostname
   ```

### Critical Timing Issue

**Problem:** If you restart services immediately after enabling them, supervise directories don't exist yet.

**Solution:** Sleep 5 seconds between enabling and restarting.

```bash
# Enable
ln -sf /etc/sv/beacon /var/service/

# Wait for runsvdir to pick it up
sleep 5

# Now restart works
sv restart beacon
```

## Runit Service Requirements

### Every Service Must Have

1. **Run script** - `/etc/sv/servicename/run`
   ```bash
   #!/bin/sh
   exec 2>&1
   exec chpst -u servicename /usr/bin/servicename
   ```

2. **Supervise symlink** - `/etc/sv/servicename/supervise`
   ```bash
   ln -sf /run/runit/supervise.servicename supervise
   ```

**Without the supervise symlink:**
- `sv status` fails: "unable to change to service directory"
- Service may run but supervision breaks
- Critical for beacon, avahi, dbus, all MDMA services

### Package INSTALL Scripts

Every MDMA package should create supervise symlink:

```bash
# In package INSTALL script
case "${ACTION}" in
post)
    # Create supervise symlink
    ln -sf /run/runit/supervise.beacon /etc/sv/beacon/supervise
    
    # Enable service
    ln -sf /etc/sv/beacon /var/service/beacon
    ;;
esac
```

## Golden Image Creation

### Prerequisites

```bash
# Install PiShrink (one-time)
curl -L https://raw.githubusercontent.com/Drewsif/PiShrink/master/pishrink.sh \
  -o ~/mdma-images/pishrink.sh
chmod +x ~/mdma-images/pishrink.sh
```

### Workflow

**Step 1: Create Working Beacon**

```bash
# Use setup script method (Method 1)
# Test thoroughly on real hardware
# Verify everything works:
# - ping welcome-to-mdma.local
# - http://welcome-to-mdma.local/
# - Web UI functional
# - Hardware detection working
# - Services stable
```

**Step 2: Prepare for Imaging**

```bash
# On the Pi, final verification
sv status beacon dbus avahi-daemon  # All running
ss -tulpn | grep :80                # Beacon listening
hostname                            # welcome-to-mdma

# Clean shutdown (important!)
shutdown -h now

# Wait for complete shutdown:
# - Green LED stops blinking
# - Only red power LED remains
# - Wait additional 10 seconds
```

**Step 3: Create Image**

```bash
# Remove SD card, insert in dev machine
lsblk  # Verify device

# Create golden image (auto-shrinks if PiShrink installed)
just golden-create-image /dev/sdX

# Result: ~/mdma-images/golden/mdma-beacon-golden-TIMESTAMP.img.xz
```

**Step 4: Test on Separate SD Card**

```bash
# CRITICAL: Test on different SD card first!
# Don't overwrite your working card until tested

# Flash to test card
xz -dc ~/mdma-images/golden/mdma-beacon-golden-*.img.xz | \
  sudo dd of=/dev/sdY bs=4M status=progress

# Boot and verify
ping welcome-to-mdma.local
http://welcome-to-mdma.local/

# If it works: golden image is good!
# If it fails: debug, fix, recreate image
```

### Image Shrinking

**Without PiShrink:**
- Compressed: ~150MB
- Extracted: 32GB (mostly empty)
- Flash time: 10-15 minutes

**With PiShrink:**
- Compressed: ~120MB
- Extracted: 3.7GB (minimal + headroom)
- Flash time: 3-5 minutes
- Auto-expands on first boot

**Always use PiShrink for production images!**

### Common Image Creation Issues

**Symptom:** Pi won't boot from golden image (solid green LED, 100% fan)

**Causes:**
1. **Filesystem corruption** - Image captured while writes pending
2. **Improper shutdown** - Didn't wait for complete shutdown
3. **Bad SD card** - Target card has issues
4. **Boot partition corruption** - Bootloader didn't transfer correctly

**Prevention:**
- Proper shutdown sequence
- Wait 10+ seconds after LED stops
- Test on spare card first
- Keep working SD card safe
- Use multiple cards for testing

**If it happens:**
- Start over with setup script (5 minutes)
- Test more thoroughly before next imaging
- Consider deferring golden images until beacon is stable

## Bootable Image vs PLATFORMFS

### Use Bootable Image (Recommended)

**Download:**
```bash
curl -LO https://repo-default.voidlinux.org/live/current/void-rpi-aarch64-20250202.img.xz
```

**Characteristics:**
- ✅ Complete disk image with bootloader
- ✅ Pre-configured partitions
- ✅ Ready to boot on Pi
- ✅ Just modify and repackage

**Use for:** All golden image workflows

### Don't Use PLATFORMFS

**What it is:**
```bash
void-rpi-aarch64-PLATFORMFS-20250202.tar.xz  # Wrong!
```

**Characteristics:**
- ❌ Root filesystem only
- ❌ No bootloader included
- ❌ Requires manual firmware setup
- ❌ Complex to make bootable

**Only for:** Advanced users who setup bootloaders manually

## Local Image Exploration

### Mount Image Locally

```bash
# Mount for inspection/modification
just image-mount

# Check contents
just image-check

# Auto-fix common issues
just image-fix

# Open shell inside image
just image-shell

# Unmount when done
just image-unmount
```

### Debug Workflow

**Before flashing, verify locally:**

```bash
# 1. Create image
just create-image

# 2. Mount and check
just image-mount
just image-check

# Expected output:
# ✅ beacon installed
# ✅ avahi installed
# ✅ dbus installed
# ✅ Services enabled
# ✅ Supervise symlinks exist
# ✅ Hostname correct

# 3. Fix any issues
just image-fix

# 4. Verify fixes
just image-check

# 5. Unmount and flash
just image-unmount
```

**Time savings:** 10x faster iteration (2 min vs 20 min per cycle)

## Hardware Configurations

### MDMA-101 and MDMA-303 (Single NVMe)

**SD Card:**
- `/boot` (512MB): Bootloader and kernel
- `/boot-spare` (512MB): Backup boot partition

**NVMe (512GB):**
- `/` (16GB): Operating system
- `/var` (8GB): Logs, journals, temp files
- `/music` (400GB): FLAC library (source of truth)
- `/metadata` (88GB): ACID fact streams

### MDMA-909 (Dual NVMe with CDJ Export)

**Primary NVMe:** Same as above

**Secondary NVMe (512GB):**
- `/cdj-export` (512GB): AIFF/WAV cache for CDJ network access via NFS

The secondary drive serves transcoded audio to CDJ-900/2000 players over the network.

## Package Building and Distribution

### Two Approaches

**Option 1: GitHub Pages Repository (Current)**
- Free hosting with HTTPS
- Git-based updates via GitHub Actions
- Units configured with `repository=https://yourusername.github.io/mdma/aarch64`

**Option 2: Local xbps Repository**
- Fast on local network
- Host on any HTTP server
- Units configured with `repository=http://repo.mdma.local/aarch64`

### Build Pipeline

```
Git push → GitHub Actions → xbps-src builds packages →
Upload to repository → Units update with xbps-install -Su
```

**Time from push to live: 2-5 minutes**

### Package Requirements

**Every MDMA package must:**

1. Include supervise symlink creation
2. Enable service automatically
3. Create necessary users/groups
4. Set proper permissions
5. Include health check support

**Example INSTALL script:**
```bash
#!/bin/sh
case "${ACTION}" in
post)
    # Create supervise symlink
    ln -sf /run/runit/supervise.beacon /etc/sv/beacon/supervise
    
    # Enable service
    if [ ! -e /var/service/beacon ]; then
        ln -sf /etc/sv/beacon /var/service/beacon
    fi
    ;;
esac
```

## NVMe Provisioning (Future)

### Phase 1: Beacon Mode (SD Card)

Current focus - beacon runs from SD card:
- Boots as `welcome-to-mdma.local`
- Shows provisioning web UI
- User selects unit type (909/101/303)
- Detects hardware (NVMe, RAM, etc.)

### Phase 2: NVMe Provisioning (Beacon Executes)

Future - beacon will:
1. Partition NVMe drives
2. Format filesystems
3. Install Void Linux base
4. Create users (mdma-audio, mdma-library, etc.)
5. Install MDMA packages
6. Configure services
7. Set hostname
8. Reboot into NVMe

**Not implemented yet - deferred until beacon is solid!**

## Updates and Rollback

### Safe Update Workflow

```bash
mdma-update  # Wrapper script that:
  1. Checks audio not active
  2. Snapshots package versions
  3. Updates packages
  4. Runs health check
  5. Auto-rollback on failure
```

### Manual Rollback

```bash
# View snapshots
ls /var/lib/mdma/snapshots/

# Downgrade specific package
xbps-install -f mdma-909-0.0.9_1
```

## Common Tasks

### Provision Fresh Beacon

```bash
# 1. Flash vanilla Void
xz -dc void-rpi-aarch64-20250202.img.xz | \
  sudo dd of=/dev/sdX bs=4M status=progress

# 2. Boot, find it
just pi-scan

# 3. Run setup
scp setup-beacon-on-pi.sh root@<IP>:/root/
ssh root@<IP>
./setup-beacon-on-pi.sh

# 4. Test
ping welcome-to-mdma.local
http://welcome-to-mdma.local/
```

### Create Golden Image (When Ready)

```bash
# 1. Thoroughly test working beacon
# 2. Clean shutdown: shutdown -h now
# 3. Wait 10+ seconds after LED stops
# 4. Create image
just golden-create-image /dev/sdX

# 5. Test on spare SD card
xz -dc golden-image.img.xz | sudo dd of=/dev/sdY bs=4M status=progress

# 6. Boot and verify
# 7. If works: distribute!
```

### Recover from Bad Image

```bash
# Start over - takes only 5 minutes
just pi-scan                          # Find Pi
scp setup-beacon-on-pi.sh root@<IP>:/root/
ssh root@<IP>
./setup-beacon-on-pi.sh
```

### Update Beacon Package

```bash
# On development machine
git commit -m "feat: Update beacon"
git push

# Wait for GitHub Actions (~3 min)

# On Pi
ssh root@welcome-to-mdma.local
xbps-install -Suy
sv restart beacon
```

## Best Practices

1. **Test on real hardware first** - Never assume chroot/QEMU works the same
2. **Small iterative steps** - Get one thing working, then iterate
3. **Use setup script during development** - Faster than debugging golden images
4. **Only create golden images when stable** - Don't prematurely optimize
5. **Test golden images on spare cards** - Don't overwrite working SD cards
6. **Wait for proper shutdown** - 10+ seconds after LED stops
7. **Install PiShrink** - Faster flash times, smaller images
8. **Keep working cards safe** - Use multiple SD cards for testing
9. **Document what works** - Update this skill with learnings
10. **Never update during playback** - Audio check is first step

## Troubleshooting Quick Reference

### Unit Not Found on Network

```bash
# Verify Pi is powered and booted (60 seconds)
# Check ethernet connected
# Scan again
just pi-scan

# Try direct IP if known
ssh root@192.168.0.164  # password: voidlinux
```

### Can't Resolve .local Domains

```bash
# On Arch development machine
sudo pacman -S nss-mdns
sudo sed -i 's/^hosts:.*/hosts: files mymachines mdns_minimal [NOTFOUND=return] resolve [!UNAVAIL=return] dns/' /etc/nsswitch.conf
sudo systemctl restart avahi-daemon
```

### Service Won't Start

```bash
# Check supervise symlink exists
ls -la /etc/sv/beacon/supervise
# Should show: supervise -> /run/runit/supervise.beacon

# If missing, create it
ln -sf /run/runit/supervise.beacon /etc/sv/beacon/supervise

# Enable service
ln -sf /etc/sv/beacon /var/service/

# Wait for runsvdir
sleep 5

# Restart
sv restart beacon

# Check status
sv status beacon
```

### Image Won't Boot

**Symptoms:** Solid green LED, 100% fan speed

**Causes:**
- Filesystem corruption during imaging
- Improper shutdown before imaging
- Bad target SD card
- Boot partition issues

**Solution:**
```bash
# Use setup script method instead
# Takes 5 minutes, always works
just pi-scan
scp setup-beacon-on-pi.sh root@<IP>:/root/
ssh root@<IP>
./setup-beacon-on-pi.sh
```

### Beacon Not Listening on Port 80

```bash
# Check service running
sv status beacon

# Check if listening
ss -tulpn | grep :80

# Check logs
tail -50 /var/log/socklog/current | grep beacon

# Restart beacon
sv restart beacon
```

## Integration Points

### With GitHub Repository

- **Repository:** https://github.com/johlrogge/modular-digital-music-array
- **Clean git history:** 604KB total (audio test files removed)
- **CI/CD:** GitHub Actions builds beacon on every push
- **Beacon binary:** 4.6MB stripped, builds automatically
- **Packaged artifacts:** 1.8MB compressed tar.gz

### With MDMA Application Code

- Packages built from Rust workspace
- Service definitions in package templates
- Configuration files templated by Ansible (future)
- Health checks verify application state

### With Music Library Management (Future)

- `/music` partition for FLAC source files
- `/metadata` partition for ACID fact streams
- `/cdj-export` (909) for transcoded AIFF/WAV
- NFS export enables CDJ network access

### With Development Workflow

- Git push triggers package build
- Packages auto-published to repository
- Units check for updates (manual or cron)
- Deploy time: minutes from commit to live

## Time Estimates

| Task | Time |
|------|------|
| Flash vanilla Void to SD | 10-15 minutes |
| Boot and network init | 60 seconds |
| Run setup script on Pi | 3-5 minutes |
| Test beacon works | 2 minutes |
| **Total: Working beacon** | **~20 minutes** |
| Create golden image (with PiShrink) | 10-15 minutes |
| Flash golden image | 3-5 minutes (with PiShrink) |
| Package build (GitHub Actions) | 3-5 minutes |
| Update unit | 2-5 minutes |
| Local image exploration | 2 minutes per check |
| Recover from bad image | 5 minutes (use setup script) |

**Current recommended workflow: Setup script = 5 minutes per Pi**

## Asset Files

### Justfile Recipes

Copy into your project's `justfile`:

**Pi Network Discovery:**
- `pi-scan` - Find Pis on network
- `pi-connect` - Find and auto-connect
- `pi-wait` - Monitor until Pi appears
- `pi-check <IP>` - Check specific IP

**Golden Image Creation:**
- `golden-copy-script <IP>` - Copy setup script to Pi
- `golden-ssh <IP>` - SSH to Pi
- `golden-create-image <DEVICE>` - Create golden image with auto-shrinking
- `golden-help` - Show complete workflow

**Local Image Exploration:**
- `image-mount` - Mount image locally
- `image-check` - Verify packages and services
- `image-fix` - Auto-fix common issues
- `image-shell` - Open shell in image
- `image-unmount` - Unmount image
- `image-debug` - Interactive guided workflow

**Package Management:**
- `pkg-build-all` - Build all packages
- `pkg-clean` - Clean build artifacts
- `create-image` - Full bootable image pipeline (old method)

### Setup Script

**setup-beacon-on-pi.sh** - Run on live Pi to configure beacon:
- Updates system
- Configures repository
- Installs packages (dbus, avahi, beacon)
- Sets hostname
- Enables services
- Verifies everything

## Development Notes

This skill assumes:
- Raspberry Pi 5 hardware
- Void Linux operating system
- NVMe drives via M.2 HAT (for future provisioning)
- Local network with mDNS support
- Development machine runs Linux (Arch Linux tested)
- SD card reader on development machine

For alternative configurations, adapt accordingly.

## Current Development Phase

**Phase: Beacon Provisioning (In Progress)**

**Working:**
- ✅ Setup script creates working beacon
- ✅ Network discovery with nmap
- ✅ mDNS resolution (.local domains)
- ✅ Beacon detects hardware
- ✅ Web UI accessible
- ✅ Services run properly
- ✅ Package distribution via GitHub Pages

**Not Yet Implemented:**
- ⏳ Golden image workflow (works but needs more testing)
- ⏳ NVMe provisioning (deferred)
- ⏳ Multi-user security (deferred)
- ⏳ Auto-updates (deferred)
- ⏳ Multiple unit types (deferred)

**Focus:** Get beacon rock-solid on real hardware before pursuing automation.

## Key Learnings

### From Real Hardware Testing

1. **Chroot limitations** - QEMU/chroot doesn't match real Pi behavior
2. **Supervise symlinks critical** - Required for runit services
3. **Timing matters** - Wait between enabling and restarting services
4. **Test before imaging** - Golden images require thorough validation
5. **Keep it simple** - Setup script faster and more reliable than golden images during development
6. **Real hardware wins** - 5 minutes on real Pi > hours debugging chroot
7. **Image creation fragile** - Many ways for it to fail, always test on spare card
8. **PiShrink essential** - 10x smaller images, 3x faster flashing

### What Works Reliably

- ✅ Setup script on live Pi
- ✅ Network scanning with nmap
- ✅ Package distribution via GitHub Pages
- ✅ Service management with runit
- ✅ mDNS with avahi

### What Needs More Work

- ⚠️ Golden image creation (works but fragile)
- ⚠️ Chroot package installation (dependency issues)
- ⚠️ Image boot reliability (needs proper shutdown)

**Recommendation:** Use setup script method until beacon is feature-complete and stable, then pursue golden images for production distribution.

## GitHub Repository

**Repository:** https://github.com/johlrogge/modular-digital-music-array

**Branches:**
- `main` - Stable releases
- `dev` - Development branch

**CI/CD:**
- GitHub Actions builds beacon binary on every push
- Workflow uses justfile recipes for consistency
- Artifacts available for download from Actions tab

**Package Repository:**
- Hosted on GitHub Pages
- URL: `https://johlrogge.github.io/modular-digital-music-array/aarch64`
- Units can update with: `xbps-install -Su`

**Current Status:** Beacon builds and deploys successfully, golden image workflow under development.
