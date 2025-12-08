#!/usr/bin/env bash
# Prepare SD card image using guestmount + xbps-install

set -euo pipefail

# Configure libguestfs to use home directory for temp files
# This avoids filling up /var/tmp on small root partitions
export TMPDIR="${HOME}/tmp"
mkdir -p "$TMPDIR"

VOID_IMAGE_URL="${VOID_IMAGE_URL:-https://repo-default.voidlinux.org/live/20250202/void-rpi-aarch64-20250202.img.xz}"
WORK_DIR="${HOME}/mdma-images"
OUTPUT_DIR="${WORK_DIR}/output"
STAGING_DIR="${WORK_DIR}/staging"
REPO_DIR="build/repository"
MOUNT_POINT="${STAGING_DIR}/mnt"

# Cleanup function to ensure mount is unmounted on exit
cleanup() {
    if [ -d "$MOUNT_POINT" ]; then
        echo ""
        echo "🧹 Cleaning up..."
        sudo umount "$MOUNT_POINT" 2>/dev/null || true
    fi
}

# Register cleanup on exit
trap cleanup EXIT

# Prerequisites already checked by justfile dependency
# This script assumes all prerequisites are met

# Check beacon package exists
if [ ! -f "build/packages/beacon-0.1.0_1.aarch64.xbps" ]; then
    echo "❌ Beacon package not built"
    echo "   Run: just pkg-build-all"
    exit 1
fi

echo "🔧 Preparing SD card image..."
echo ""
echo "📁 Using temp directory: $TMPDIR"
echo ""

# Create directories
mkdir -p "${WORK_DIR}" "${OUTPUT_DIR}" "${STAGING_DIR}"

# Download Void Linux image if needed
VOID_IMAGE="${WORK_DIR}/void-rpi-aarch64.img.xz"
if [ ! -f "$VOID_IMAGE" ]; then
    echo "📥 Downloading Void Linux image..."
    wget -O "$VOID_IMAGE" "$VOID_IMAGE_URL"
else
    echo "✅ Using cached Void Linux image"
fi

# Extract image
EXTRACTED_IMAGE="${WORK_DIR}/void-rpi-aarch64.img"
if [ ! -f "$EXTRACTED_IMAGE" ]; then
    echo "📦 Extracting image..."
    xz -dc "$VOID_IMAGE" > "$EXTRACTED_IMAGE"
else
    echo "✅ Using extracted image"
fi

# Copy base image for modification
TIMESTAMP=$(date +%Y%m%d-%H%M%S)
WORK_IMAGE="${STAGING_DIR}/mdma-beacon-${TIMESTAMP}.img"

# Clean up any stale mount points BEFORE copying work image
echo "🧹 Cleaning up any previous state..."
if [ -d "$STAGING_DIR" ]; then
    # Unmount if there's a stale FUSE mount
    if mountpoint -q "$MOUNT_POINT" 2>/dev/null; then
        sudo umount "$MOUNT_POINT" 2>/dev/null || sudo umount -l "$MOUNT_POINT" 2>/dev/null || true
    fi
    # Remove old work images and mount point
    sudo rm -rf "$STAGING_DIR"
fi

# Recreate directories
mkdir -p "$STAGING_DIR"
sudo mkdir -p "$MOUNT_POINT"

echo "📋 Copying base image..."
cp "$EXTRACTED_IMAGE" "$WORK_IMAGE"

# Find root filesystem using guestfish (part of libguestfs)
echo "🔍 Detecting root filesystem..."
ROOTFS_DEV=$(guestfish --ro -a "$WORK_IMAGE" run : list-filesystems | grep -E 'ext4|xfs' | head -1 | cut -d: -f1)

if [ -z "$ROOTFS_DEV" ]; then
    echo "❌ Could not find root filesystem in image"
    exit 1
fi

echo "  → Found: $ROOTFS_DEV"

# Explain why sudo is needed BEFORE prompting
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "🔐 Sudo access required for image modification"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "Why sudo is needed:"
echo "  • Create mount point directory (proper permissions)"
echo "  • Mount the disk image (guestmount requires root for FUSE mounts)"
echo "  • Install packages into the image (write to mounted filesystem)"
echo "  • Create service symlinks (modify system directories)"
echo "  • Unmount the image cleanly"
echo ""
echo "This is standard for cross-platform image building (x86_64 → ARM64)."
echo "You'll be prompted for your password for the privileged operations."
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
read -p "Press Enter to continue or Ctrl+C to cancel..."
echo ""

# Mount image
echo "📂 Mounting image..."
sudo guestmount -a "$WORK_IMAGE" -m "$ROOTFS_DEV" --rw "$MOUNT_POINT"

echo "✅ Image mounted at $MOUNT_POINT"
echo ""

# From here on, we're running as root via sudo, so group operations
# to avoid multiple password prompts

echo "📚 Configuring package repository..."

# Copy local repository into mounted filesystem
sudo mkdir -p "$MOUNT_POINT/tmp/mdma-repo"
sudo cp -r "$(realpath $REPO_DIR)"/aarch64/*.xbps "$MOUNT_POINT/tmp/mdma-repo/"
sudo cp "$(realpath $REPO_DIR)"/aarch64/aarch64-repodata "$MOUNT_POINT/tmp/mdma-repo/"

# Create xbps config pointing to the copied repository
sudo bash -c "cat > '$MOUNT_POINT/etc/xbps.d/99-mdma-repo.conf'" <<EOF
repository=/tmp/mdma-repo
repository=https://repo-default.voidlinux.org/current/aarch64
EOF

echo "📦 Installing beacon package..."
echo ""
echo "  Note: Installing into ARM64 image from x86_64 host."
echo "  This works because xbps-install -r just copies files,"
echo "  it doesn't run any ARM64 code!"
echo ""

# First, update xbps itself in the target root
echo "  → Updating xbps package manager..."
sudo xbps-install -r "$MOUNT_POINT" \
    --repository=https://repo-default.voidlinux.org/current/aarch64 \
    -Su xbps

echo ""
echo "  → Installing beacon and dependencies..."
# Install beacon and dependencies
# Using -r (rootdir) means xbps extracts files but doesn't execute them
# This works cross-platform: x86_64 host can install into ARM64 image!
# Repository is configured in /etc/xbps.d/99-mdma-repo.conf inside the mounted root
sudo xbps-install -r "$MOUNT_POINT" \
    -Sy beacon

echo ""
echo "✅ Beacon installed!"

# Enable services
echo "🔧 Enabling services..."
sudo ln -sf /etc/sv/beacon "$MOUNT_POINT/var/service/beacon" 2>/dev/null || true
sudo ln -sf /etc/sv/dbus "$MOUNT_POINT/var/service/dbus" 2>/dev/null || true
sudo ln -sf /etc/sv/avahi-daemon "$MOUNT_POINT/var/service/avahi-daemon" 2>/dev/null || true

echo "✅ Services enabled"

# Unmount (cleanup trap will also handle this if script exits early)
echo "💾 Unmounting..."
sudo guestunmount "$MOUNT_POINT"

echo "✅ Image unmounted"

# Move to output
FINAL_IMAGE="${OUTPUT_DIR}/mdma-beacon-${TIMESTAMP}-rpi5.img"
mv "$WORK_IMAGE" "$FINAL_IMAGE"

# Compress
echo "🗜️  Compressing..."
xz -z -9 -T 0 "$FINAL_IMAGE"

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "✅ SD card image ready!"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "Image: ${FINAL_IMAGE}.xz"
ls -lh "${FINAL_IMAGE}.xz"
echo ""
echo "Size: $(du -h ${FINAL_IMAGE}.xz | cut -f1)"
echo ""
echo "Flash with:"
echo "  xz -dc ${FINAL_IMAGE}.xz | sudo dd of=/dev/sdX bs=4M status=progress conv=fsync"
echo ""
