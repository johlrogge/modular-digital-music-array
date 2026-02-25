# Provisioning Workflows

## Overview

MDMA provisioning follows a two-stage process:

1. **SD Card Provisioning** (local, via `just`): Creates bootable beacon mode
2. **NVMe Provisioning** (beacon): Full system setup via the beacon web UI

This separates local hardware preparation from remote system configuration.

## Stage 1: SD Card Provisioning

### Purpose

Create a minimal bootable SD card that:
- Boots the Raspberry Pi 5
- Broadcasts mDNS beacon (`${hostname}.local`)
- Accepts SSH connections with deployment keys
- Awaits Ansible provisioning

### Workflow

```bash
just provision-sd --hostname mdma-909-studio --role 909 --device /dev/sdX
```

Steps performed:

1. **Download base image** (if not cached)
   - Source: Official Void Linux ARM rootfs
   - Cache location: `~/.cache/mdma/void-arm-base.tar.xz`

2. **Partition SD card**
   ```
   /dev/sdX1: 512MB vfat (boot)
   /dev/sdX2: 512MB vfat (boot-spare)
   ```

3. **Extract base system** to `/dev/sdX1`

4. **Inject configuration**:
   - `/etc/hostname` ← hostname parameter
   - `/root/.ssh/authorized_keys` ← deployment keys from `~/.ssh/mdma_deploy_ed25519.pub`
   - `/etc/avahi/avahi-daemon.conf` ← mDNS configuration
   - `/etc/rc.local` ← beacon mode marker

5. **Sync and unmount**

6. **Display instructions**:
   ```
   SD card ready: mdma-909-studio
   1. Insert SD card into Raspberry Pi 5
   2. Connect NVMe drive(s)
   3. Power on
   4. Wait 60 seconds for network
   5. Run: just discover-units
   ```

### Requirements

- Void Linux ARM base rootfs (downloaded automatically)
- Deployment SSH keys in `~/.ssh/mdma_deploy_ed25519[.pub]`
- sudo privileges (for mount operations)
- Target SD card device path

### Beacon Mode Behavior

The SD card boots into "beacon mode":

**Active services:**
- `avahi-daemon`: Broadcasts `${hostname}.local`
- `sshd`: Accepts connections on port 22
- `beacon-http` (optional): HTTP endpoint at `:8080/status`

**Beacon HTTP response:**
```json
{
  "status": "beacon",
  "hostname": "mdma-909-studio",
  "role": "909",
  "needs_provisioning": true,
  "nvme_detected": true,
  "nvme_count": 2
}
```

## Stage 2: NVMe Provisioning (Beacon)

### Purpose

Transform beacon mode unit into fully operational MDMA system:
- Partition and format NVMe drive(s)
- Install complete Void Linux system
- Configure services (audio, NFS, runit)
- Deploy MDMA packages
- Set up music library and metadata directories

### Discovery

Before provisioning, discover available units:

```bash
just discover-units
```

Output:
```
🔍 Scanning for MDMA units...

Found 2 units:
  ✓ mdma-909-studio.local (needs provisioning)
    Role: 909, NVMe: 2 drives detected
  
  ✓ mdma-101-bedroom.local (needs provisioning)
    Role: 101, NVMe: 1 drive detected
```

### Provisioning Workflow

```bash
just provision mdma-909-studio.local
```

Provisioning executes:

#### 1. Pre-flight Checks
- Verify SSH connectivity
- Confirm hostname matches
- Detect NVMe drives (`lsblk`)
- Validate drive count matches role expectations

#### 2. Partition NVMe Drives

**For single NVMe (101, 303):**
```bash
parted /dev/nvme0n1 mklabel gpt
parted /dev/nvme0n1 mkpart primary ext4 0% 3%      # 16GB root
parted /dev/nvme0n1 mkpart primary ext4 3% 5%      # 8GB var
parted /dev/nvme0n1 mkpart primary ext4 5% 83%     # 400GB music
parted /dev/nvme0n1 mkpart primary ext4 83% 100%   # remaining metadata
```

**For dual NVMe (909):**
- Primary NVMe: Same as single NVMe
- Secondary NVMe: Single partition (full drive for `/cdj-export`)

#### 3. Format Filesystems
```bash
mkfs.ext4 -L mdma-root /dev/nvme0n1p1
mkfs.ext4 -L mdma-var /dev/nvme0n1p2
mkfs.ext4 -L mdma-music /dev/nvme0n1p3
mkfs.ext4 -L mdma-metadata /dev/nvme0n1p4

# For 909 secondary drive:
mkfs.ext4 -L mdma-cdj-export /dev/nvme0n2p1
```

#### 4. Mount Temporary Root
```bash
mount /dev/nvme0n1p1 /mnt
mkdir -p /mnt/{var,music,metadata,boot}
mount /dev/nvme0n1p2 /mnt/var
mount /dev/nvme0n1p3 /mnt/music
mount /dev/nvme0n1p4 /mnt/metadata
mount /dev/mmcblk0p1 /mnt/boot
```

#### 5. Install Base System
```bash
xbps-install -S -R https://repo.voidlinux.org/current/aarch64 \
  -r /mnt base-system void-repo-nonfree
```

#### 6. Configure fstab
Generate `/mnt/etc/fstab`:
```
LABEL=mdma-root      /           ext4  defaults              0 1
LABEL=mdma-var       /var        ext4  defaults              0 2
LABEL=mdma-music     /music      ext4  defaults,noatime      0 2
LABEL=mdma-metadata  /metadata   ext4  defaults,noatime      0 2
/dev/mmcblk0p1       /boot       vfat  defaults              0 2
```

For 909, add:
```
LABEL=mdma-cdj-export /cdj-export ext4  defaults,noatime      0 2
```

#### 7. Install System Packages
```bash
xbps-install -S -r /mnt \
  avahi-daemon nss-mdns \
  alsa-utils alsa-lib \
  runit-void-apparmor \
  nfs-utils     # 909 only
```

#### 8. Configure Hostname
```bash
echo "mdma-909-studio" > /mnt/etc/hostname
```

#### 9. Configure Avahi
Template `/mnt/etc/avahi/avahi-daemon.conf`:
```ini
[server]
host-name=mdma-909-studio
use-ipv4=yes
use-ipv6=no
enable-dbus=no

[publish]
publish-addresses=yes
publish-hinfo=yes
publish-workstation=yes
```

#### 10. Install MDMA Packages (from repository)
```bash
xbps-install -S -r /mnt \
  mdma-909       # or mdma-101, mdma-303
```

#### 11. Configure NFS Export (909 only)
Template `/mnt/etc/exports`:
```
/cdj-export 192.168.0.0/16(ro,sync,no_subtree_check,insecure)
```

Enable NFS service:
```bash
ln -s /etc/sv/nfs-server /mnt/etc/runit/runsvdir/default/
ln -s /etc/sv/rpcbind /mnt/etc/runit/runsvdir/default/
```

#### 12. Set Directory Permissions
```bash
chown -R mdma:mdma /mnt/music
chown -R mdma:mdma /mnt/metadata
chown -R mdma:mdma /mnt/cdj-export  # 909 only
chmod 755 /mnt/{music,metadata}
```

#### 13. Generate SSH Host Keys
```bash
ssh-keygen -A -f /mnt
```

#### 14. Unmount and Reboot
```bash
umount -R /mnt
ssh root@mdma-909-studio.local 'reboot'
```

#### 15. Post-Reboot Verification
Wait 60 seconds, then:
```bash
ssh mdma@mdma-909-studio.local 'mdma-health-check'
```

Expected output:
```
✓ Audio devices detected
✓ Music directory mounted
✓ Metadata directory mounted
✓ NFS export active (909 only)
✓ mDNS beacon broadcasting
✓ All services running
```

### Post-Provisioning State

Unit is now:
- Booting from NVMe root (SD card only for bootloader)
- Broadcasting mDNS hostname
- Running MDMA services via runit
- Ready for music library import
- Ready for package updates

## Batch Provisioning

To provision multiple units:

```bash
# Write all SD cards first
just provision-sd --hostname mdma-909-studio --role 909 --device /dev/sdX
just provision-sd --hostname mdma-101-bedroom --role 101 --device /dev/sdY
just provision-sd --hostname mdma-303-kitchen --role 303 --device /dev/sdZ

# Boot all units with NVMe drives installed
# Wait 60 seconds for network

# Discover all
just discover-units

# Provision each via beacon web UI or:
just provision mdma-909-studio.local
just provision mdma-101-bedroom.local
just provision mdma-303-kitchen.local
```

Batch provisioning of multiple units simultaneously will be supported via beacon fleet management (future).

## Troubleshooting

### Unit not found via mDNS
- Verify Avahi running: `systemctl status avahi-daemon`
- Check network connectivity
- Ensure `.local` domain resolution: `ping mdma-909-studio.local`
- Check firewall rules (port 5353 UDP for mDNS)

### NVMe not detected
- Verify NVMe physically installed
- Check kernel drivers: `lsmod | grep nvme`
- Check `dmesg | grep nvme` for errors

### Provisioning fails
- Check SSH connectivity: `ssh root@mdma-909-studio.local`
- Verify deployment keys accepted
- Check beacon provisioning logs

### Beacon mode stuck
- Unit may need manual intervention
- SSH in and check `journalctl -xe`
- Verify `/etc/rc.local` beacon script ran
