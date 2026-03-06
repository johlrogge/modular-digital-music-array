#!/usr/bin/env bash
# MDMA SD Card Image Builder
# Uses void-mklive (Void Linux's official image builder) to create
# a bootable Raspberry Pi 5 SD card image with beacon pre-installed.
#
# void-mklive runs xbps-reconfigure -a after package installation,
# which executes post-install scripts (creating users, enabling services, etc.)
# correctly — unlike a manual chroot approach.
#
# Usage: sudo ./create-sd-card-simple.sh
# Output: ~/mdma-images/output/mdma-beacon-YYYYMMDD-rpi5.img.xz (under invoking user's home)

set -euo pipefail

# Must run as root
if [ "$(id -u)" -ne 0 ]; then
    echo "Error: must run as root (sudo)"
    exit 1
fi

# Configuration
MDMA_REPO="https://johlrogge.github.io/modular-digital-music-array/aarch64"
INVOKING_HOME=$(eval echo "~${SUDO_USER:-$(whoami)}")
WORK_DIR="${WORK_DIR:-${INVOKING_HOME}/mdma-images}"
MKLIVE_DIR="${WORK_DIR}/void-mklive"
OUTPUT_DIR="${WORK_DIR}/output"

# Step 0: Clone/update void-mklive
if [ ! -d "$MKLIVE_DIR" ]; then
    echo "Cloning void-mklive..."
    git clone --depth 1 https://github.com/void-linux/void-mklive.git "$MKLIVE_DIR"
else
    echo "Updating void-mklive..."
    git -C "$MKLIVE_DIR" pull --ff-only || true
fi

# Patch: skip cloud-guest-utils install in mkimage.sh
# It tries to chroot+xbps-install in an aarch64 rootfs on x86_64 host, which fails
# without qemu-user-static. We pre-install it via mkplatformfs -p instead.
sed -i 's/run_cmd_target "xbps-install -Syr $ROOTFS cloud-guest-utils"/# [MDMA] skipped: cloud-guest-utils pre-installed via mkplatformfs/' "$MKLIVE_DIR/mkimage.sh"

mkdir -p "$OUTPUT_DIR"
cd "$MKLIVE_DIR"

# Step 1: Create base rootfs (architecture-generic)
# Skip if already exists from a previous run
ROOTFS_TAR=$(ls -t void-aarch64-ROOTFS-*.tar.xz 2>/dev/null | head -1 || true)
if [ -z "$ROOTFS_TAR" ]; then
    echo "Step 1/3: Building aarch64 base rootfs..."
    ./mkrootfs.sh aarch64
    ROOTFS_TAR=$(ls -t void-aarch64-ROOTFS-*.tar.xz | head -1)
else
    echo "Step 1/3: Using cached rootfs: $ROOTFS_TAR"
fi

# Step 2: Create platform-specific rootfs with beacon
# -p adds extra packages, -r adds our custom repo
# -k runs a post-install hook script after xbps-reconfigure -a
PLATFORMFS_TAR=$(ls -t void-rpi-aarch64-PLATFORMFS-*.tar.xz 2>/dev/null | head -1 || true)

# Create post-install hook that configures beacon for first boot.
# void-mklive calls this hook with the rootfs path as $1.
HOOK_SCRIPT="${WORK_DIR}/mdma-hook.sh"
cat > "$HOOK_SCRIPT" << 'HOOKEOF'
#!/bin/bash
# Post-install hook for void-mklive mkplatformfs.sh
# Called after xbps-reconfigure -a has run inside the rootfs.
# $1 = path to the rootfs being built
ROOTFS="$1"

# Set hostname for beacon discovery
echo "welcome-to-mdma" > "${ROOTFS}/etc/hostname"

# Enable services in runit by linking into the default runsvdir.
# void-mklive creates /etc/runit/runsvdir/default for enabled services.
mkdir -p "${ROOTFS}/etc/runit/runsvdir/default"
ln -sf /etc/sv/beacon "${ROOTFS}/etc/runit/runsvdir/default/beacon"
ln -sf /etc/sv/dbus "${ROOTFS}/etc/runit/runsvdir/default/dbus"
ln -sf /etc/sv/avahi-daemon "${ROOTFS}/etc/runit/runsvdir/default/avahi-daemon"

echo "MDMA beacon configured for first boot"
HOOKEOF
chmod +x "$HOOK_SCRIPT"

if [ -z "$PLATFORMFS_TAR" ]; then
    echo "Step 2/3: Building platform rootfs with beacon..."
    ./mkplatformfs.sh \
        -p "beacon dbus avahi cloud-guest-utils" \
        -r "$MDMA_REPO" \
        -k "$HOOK_SCRIPT" \
        rpi-aarch64 \
        "$ROOTFS_TAR"
    PLATFORMFS_TAR=$(ls -t void-rpi-aarch64-PLATFORMFS-*.tar.xz | head -1)
else
    echo "Step 2/3: Using cached platformfs: $PLATFORMFS_TAR"
fi

# Step 3: Create bootable disk image
echo "Step 3/3: Creating bootable SD card image..."
./mkimage.sh "$PLATFORMFS_TAR"

# Move and rename output
MKLIVE_OUTPUT=$(ls -t void-rpi-aarch64-*.img.xz 2>/dev/null | head -1 || true)
FINAL_OUTPUT="${OUTPUT_DIR}/mdma-beacon-$(date +%Y%m%d)-rpi5.img.xz"

if [ -n "$MKLIVE_OUTPUT" ]; then
    mv "$MKLIVE_OUTPUT" "$FINAL_OUTPUT"
    chmod 644 "$FINAL_OUTPUT"
else
    echo "Error: could not find output image from mkimage.sh"
    exit 1
fi

echo ""
echo "========================================"
echo "  MDMA SD Card Image Ready!"
echo "========================================"
echo "Image: $FINAL_OUTPUT"
ls -lh "$FINAL_OUTPUT"
echo ""
echo "Flash with:"
echo "  xzcat $FINAL_OUTPUT | sudo dd of=/dev/sdX bs=4M status=progress conv=fsync"
echo ""
echo "Then:"
echo "  1. Insert SD card into Raspberry Pi 5"
echo "  2. Boot the Pi"
echo "  3. Browse to http://welcome-to-mdma.local"
