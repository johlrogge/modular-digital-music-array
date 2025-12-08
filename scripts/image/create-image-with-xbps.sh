#!/usr/bin/env bash
# Prepare SD card image using xbps-install (the proper Void way!)

set -euo pipefail

VOID_IMAGE_URL="${VOID_IMAGE_URL:-https://repo-default.voidlinux.org/live/20250202/void-rpi-aarch64-20250202.img.xz}"
WORK_DIR="${HOME}/mdma-images"
OUTPUT_DIR="${WORK_DIR}/output"
STAGING_DIR="${WORK_DIR}/staging"
REPO_DIR="build/repository"

# Use centralized prerequisite checker
if ! ./scripts/utils/check-prerequisites.sh; then
    echo ""
    echo "❌ Prerequisites not met"
    exit 1
fi

# Check image-specific requirements
if ! command -v guestfish &> /dev/null; then
    echo "❌ guestfish not found"
    echo ""
    echo "Install with:"
    echo "  sudo pacman -S libguestfs"
    echo ""
    exit 1
fi

# Check beacon package exists
if [ ! -f "build/packages/beacon-0.1.0_1.aarch64.xbps" ]; then
    echo "❌ Beacon package not built"
    echo "   Run: just pkg-build-all"
    exit 1
fi

echo "🔧 Preparing SD card image with xbps-install..."
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
echo "📋 Copying base image..."
cp "$EXTRACTED_IMAGE" "$WORK_IMAGE"

# Mount image with guestfish
echo "📂 Mounting image..."
MOUNT_POINT="${STAGING_DIR}/mnt"
mkdir -p "$MOUNT_POINT"

# Use guestfish to get partition info and mount
ROOTFS_DEV=$(guestfish --ro -a "$WORK_IMAGE" <<GUESTEOF
run
list-filesystems
GUESTEOF
| grep ext4 | cut -d: -f1 | head -1)

echo "  Rootfs device: $ROOTFS_DEV"

# Mount using guestmount (safer than loop device)
guestmount -a "$WORK_IMAGE" -m "$ROOTFS_DEV" --rw "$MOUNT_POINT"

echo "✅ Image mounted at $MOUNT_POINT"
echo ""

# Set up local repository
echo "📚 Configuring package repository..."
LOCAL_REPO_PATH="file://$(realpath $REPO_DIR)/aarch64"

# Create xbps config for the installation
mkdir -p "$MOUNT_POINT/etc/xbps.d"
cat > "$MOUNT_POINT/etc/xbps.d/99-mdma-repo.conf" <<XBPSEOF
repository=$LOCAL_REPO_PATH
repository=https://repo-default.voidlinux.org/current/aarch64
XBPSEOF

# Install beacon and dependencies using xbps-install
echo "📦 Installing beacon package..."
echo ""

sudo xbps-install -r "$MOUNT_POINT" \
    --repository="$LOCAL_REPO_PATH" \
    --repository=https://repo-default.voidlinux.org/current/aarch64 \
    -Sy beacon

echo ""
echo "✅ Beacon installed!"

# Enable services
echo "🔧 Enabling services..."
sudo ln -sf /etc/sv/beacon "$MOUNT_POINT/var/service/beacon" 2>/dev/null || true
sudo ln -sf /etc/sv/dbus "$MOUNT_POINT/var/service/dbus" 2>/dev/null || true
sudo ln -sf /etc/sv/avahi-daemon "$MOUNT_POINT/var/service/avahi-daemon" 2>/dev/null || true

echo "✅ Services enabled"

# Unmount
echo "💾 Unmounting..."
guestunmount "$MOUNT_POINT"

# Move to output
FINAL_IMAGE="${OUTPUT_DIR}/mdma-beacon-${TIMESTAMP}-rpi5.img"
mv "$WORK_IMAGE" "$FINAL_IMAGE"

# Compress
echo "🗜️  Compressing..."
xz -z -9 -T 0 "$FINAL_IMAGE"

echo ""
echo "✅ SD card image ready!"
echo ""
echo "Image: ${FINAL_IMAGE}.xz"
ls -lh "${FINAL_IMAGE}.xz"
echo ""
echo "Size: $(du -h ${FINAL_IMAGE}.xz | cut -f1)"
echo ""
echo "Flash with:"
echo "  xz -dc ${FINAL_IMAGE}.xz | sudo dd of=/dev/sdX bs=4M status=progress conv=fsync"
