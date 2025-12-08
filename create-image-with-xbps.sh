#!/usr/bin/env bash
# Prepare SD card image using xbps-install (the proper Void way!)

set -euo pipefail

VOID_IMAGE_URL="${VOID_IMAGE_URL:-https://repo-default.voidlinux.org/live/20250202/void-rpi-aarch64-20250202.img.xz}"
WORK_DIR="${HOME}/mdma-images"
OUTPUT_DIR="${WORK_DIR}/output"
STAGING_DIR="${WORK_DIR}/staging"
REPO_DIR="build/repository"

# Check prerequisites
if ! command -v xbps-install &> /dev/null; then
    echo "❌ xbps-install not found"
    echo "   Install: sudo pacman -S xbps"
    exit 1
fi

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

# Mount image
echo "📂 Mounting image..."
MOUNT_DIR="${STAGING_DIR}/mnt"
mkdir -p "$MOUNT_DIR"

# Find partition offset (usually partition 2 is the rootfs)
OFFSET=$(fdisk -l "$EXTRACTED_IMAGE" | grep "Linux" | tail -1 | awk '{print $2 * 512}')

sudo mount -o loop,offset=$OFFSET "$EXTRACTED_IMAGE" "$MOUNT_DIR"

echo "✅ Image mounted at $MOUNT_DIR"
echo ""

# Set up local repository for xbps-install
echo "📚 Setting up local package repository..."
LOCAL_REPO_PATH="file://$(realpath $REPO_DIR)/aarch64"

# Install packages using xbps-install with rootdir
echo "📦 Installing packages into image..."
echo ""

# Install beacon and its dependencies
sudo xbps-install -r "$MOUNT_DIR" \
    --repository="$LOCAL_REPO_PATH" \
    --repository=https://repo-default.voidlinux.org/current/aarch64 \
    -y beacon

echo ""
echo "✅ Packages installed!"

# Configure hostname
echo "⚙️  Configuring system..."
echo "welcome-to-mdma" | sudo tee "$MOUNT_DIR/etc/hostname" > /dev/null

# Configure hosts file
sudo tee "$MOUNT_DIR/etc/hosts" > /dev/null <<EOF
127.0.0.1   localhost
127.0.1.1   welcome-to-mdma.local welcome-to-mdma

::1         localhost ip6-localhost ip6-loopback
ff02::1     ip6-allnodes
ff02::2     ip6-allrouters
EOF

# Enable services
echo "🔧 Enabling services..."
sudo ln -sf /etc/sv/beacon "$MOUNT_DIR/var/service/" 2>/dev/null || true
sudo ln -sf /etc/sv/dbus "$MOUNT_DIR/var/service/" 2>/dev/null || true
sudo ln -sf /etc/sv/avahi-daemon "$MOUNT_DIR/var/service/" 2>/dev/null || true

# Configure SSH
echo "🔐 Configuring SSH..."
sudo sed -i 's/#PermitRootLogin.*/PermitRootLogin yes/' "$MOUNT_DIR/etc/ssh/sshd_config"

# Unmount
echo "💾 Unmounting..."
sudo umount "$MOUNT_DIR"

# Compress final image
echo "🗜️  Compressing..."
TIMESTAMP=$(date +%Y%m%d)
FINAL_IMAGE="${OUTPUT_DIR}/mdma-beacon-${TIMESTAMP}-rpi5.img"

cp "$EXTRACTED_IMAGE" "$FINAL_IMAGE"
xz -z -9 -T 0 "$FINAL_IMAGE"

echo ""
echo "✅ SD card image ready!"
echo ""
echo "Image: ${FINAL_IMAGE}.xz"
ls -lh "${FINAL_IMAGE}.xz"
echo ""
echo "Flash with:"
echo "  xz -dc ${FINAL_IMAGE}.xz | sudo dd of=/dev/sdX bs=4M status=progress conv=fsync"
