#!/usr/bin/env bash
# Simple SD card image creation using online package repository
# Now that beacon is available online, this is MUCH simpler!

set -euo pipefail

# Configuration
ONLINE_REPO="https://johlrogge.github.io/modular-digital-music-array/aarch64"
VOID_BASE_URL="https://repo-default.voidlinux.org/live/current"
WORK_DIR="${HOME}/mdma-images"
OUTPUT_DIR="${WORK_DIR}/output"
MOUNT_POINT="${WORK_DIR}/mnt"

# Cleanup function
cleanup() {
    if [ -d "$MOUNT_POINT" ] && mountpoint -q "$MOUNT_POINT"; then
        echo ""
        echo "🧹 Cleaning up: unmounting image..."
        sudo guestunmount "$MOUNT_POINT" || sudo umount -l "$MOUNT_POINT" || true
    fi
}

# Set trap to cleanup on exit (success or failure)
trap cleanup EXIT INT TERM

echo "🔧 Creating MDMA SD card image..."
echo ""

# Create directories
mkdir -p "${WORK_DIR}" "${OUTPUT_DIR}"

# Find latest Void Linux image
echo "🔍 Finding latest Void Linux image..."
LATEST_IMAGE=$(curl -s "$VOID_BASE_URL/" | grep -o 'void-rpi-aarch64-[0-9]*.img.xz' | sort -u | tail -1)

if [ -z "$LATEST_IMAGE" ]; then
    echo "❌ Could not find Void Linux image"
    echo "Check: $VOID_BASE_URL/"
    exit 1
fi

echo "  → Found: $LATEST_IMAGE"
echo "  (This image works for all Raspberry Pi models including Pi 5)"

VOID_IMAGE_URL="${VOID_BASE_URL}/${LATEST_IMAGE}"
VOID_IMAGE="${WORK_DIR}/${LATEST_IMAGE}"
EXTRACTED_IMAGE="${VOID_IMAGE%.xz}"

if [ ! -f "$VOID_IMAGE" ]; then
    echo "📥 Downloading Void Linux base image..."
    wget -O "$VOID_IMAGE" "$VOID_IMAGE_URL"
else
    echo "✅ Using cached base image"
fi

# Extract if needed
if [ ! -f "$EXTRACTED_IMAGE" ]; then
    echo "📦 Extracting base image..."
    xz -dc "$VOID_IMAGE" > "$EXTRACTED_IMAGE"
else
    echo "✅ Using extracted base image"
fi

# Create working copy
TIMESTAMP=$(date +%Y%m%d-%H%M%S)
WORK_IMAGE="${WORK_DIR}/mdma-beacon-${TIMESTAMP}.img"
echo "📋 Copying base image..."
cp "$EXTRACTED_IMAGE" "$WORK_IMAGE"

# Find root partition
echo "🔍 Detecting root filesystem..."
ROOTFS_DEV=$(guestfish --ro -a "$WORK_IMAGE" run : list-filesystems | grep -E 'ext4|xfs' | head -1 | cut -d: -f1)

if [ -z "$ROOTFS_DEV" ]; then
    echo "❌ Could not find root filesystem"
    exit 1
fi

echo "  → Found: $ROOTFS_DEV"
echo ""

# Mount point
MOUNT_POINT="${WORK_DIR}/mnt"
mkdir -p "$MOUNT_POINT"

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "🔐 Sudo required to mount image and install packages"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "This is the SIMPLE way - no copying repos, no complex mounting!"
echo "Just configure the online repository and install beacon."
echo ""
read -p "Press Enter to continue..."
echo ""

# Mount
echo "📂 Mounting image..."
sudo guestmount -a "$WORK_IMAGE" -m "$ROOTFS_DEV" --rw "$MOUNT_POINT"

# Configure online repository
echo "🌐 Configuring online package repository..."
# Our custom repository (unsigned)
sudo bash -c "cat > '$MOUNT_POINT/etc/xbps.d/10-mdma-repo.conf'" <<EOF
repository=$ONLINE_REPO
EOF

# Official Void repositories (signed)
sudo bash -c "cat > '$MOUNT_POINT/etc/xbps.d/00-repository-main.conf'" <<EOF
repository=https://repo-default.voidlinux.org/current/aarch64
EOF

echo "✅ Repository configured"
echo ""

# Update xbps first (if needed)
echo "📦 Installing beacon from online repository..."
echo ""
echo "  This proves the whole pipeline works:"
echo "  1. Package built in GitHub Actions ✅"
echo "  2. Deployed to GitHub Pages ✅"
echo "  3. Accessible online ✅"
echo "  4. Installable with xbps ✅"
echo ""

# Update xbps
echo "📦 Updating xbps..."
sudo XBPS_TARGET_ARCH=aarch64 xbps-install -r "$MOUNT_POINT" -Suy xbps

# Note: Public key is embedded in repository metadata by xbps-rindex --sign
# xbps-install will download it automatically when syncing the repository
echo "📦 Installing beacon from online repository..."
echo "   (Public key is embedded in repository metadata)"
echo ""

# Install beacon from online repo with VERBOSE output
echo "Installing beacon (verbose output enabled)..."
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
sudo XBPS_TARGET_ARCH=aarch64 xbps-install -r "$MOUNT_POINT" -Sy -vvv beacon
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

echo ""
echo "✅ Beacon installed from online repository!"

# Enable services
echo "🔧 Enabling services..."
sudo ln -sf /etc/sv/beacon "$MOUNT_POINT/var/service/beacon" 2>/dev/null || true
sudo ln -sf /etc/sv/dbus "$MOUNT_POINT/var/service/dbus" 2>/dev/null || true
sudo ln -sf /etc/sv/avahi-daemon "$MOUNT_POINT/var/service/avahi-daemon" 2>/dev/null || true

# Unmount
echo "💾 Unmounting..."
sudo guestunmount "$MOUNT_POINT"

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
echo "🎉 That was MUCH simpler than before!"
echo ""
