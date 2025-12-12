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
    if [ -d "$MOUNT_POINT" ]; then
        echo ""
        echo "🧹 Cleaning up..."
        
        # Unmount chroot filesystems if they exist
        if mountpoint -q "$MOUNT_POINT/sys" 2>/dev/null; then
            echo "   Unmounting /sys..."
            sudo umount "$MOUNT_POINT/sys" || sudo umount -l "$MOUNT_POINT/sys" || true
        fi
        
        if mountpoint -q "$MOUNT_POINT/proc" 2>/dev/null; then
            echo "   Unmounting /proc..."
            sudo umount "$MOUNT_POINT/proc" || sudo umount -l "$MOUNT_POINT/proc" || true
        fi
        
        if mountpoint -q "$MOUNT_POINT/dev" 2>/dev/null; then
            echo "   Unmounting /dev..."
            sudo umount "$MOUNT_POINT/dev" || sudo umount -l "$MOUNT_POINT/dev" || true
        fi
        
        # Unmount main image
        if mountpoint -q "$MOUNT_POINT" 2>/dev/null; then
            echo "   Unmounting image..."
            sudo guestunmount "$MOUNT_POINT" || sudo umount -l "$MOUNT_POINT" || true
        fi
    fi
}

# Set trap to cleanup on exit (success or failure)
trap cleanup EXIT INT TERM

echo "🔧 Creating MDMA SD card image..."
echo ""

# Create directories
mkdir -p "$WORK_DIR" "$OUTPUT_DIR" "$MOUNT_POINT"

# Download base Void image (PLATFORMFS tarball for Raspberry Pi)
# Latest as of 2025-02-02 from https://repo-default.voidlinux.org/live/current/
# Using GLIBC version to match beacon binary (compiled with aarch64-linux-gnu-gcc)
VOID_IMAGE="void-rpi-aarch64-PLATFORMFS-20250202.tar.xz"
BASE_IMAGE_URL="https://repo-default.voidlinux.org/live/current/${VOID_IMAGE}"

echo "📥 Downloading base Void Linux PLATFORMFS (glibc)..."
echo "   Image: $VOID_IMAGE (116 MB)"

if [ ! -f "$WORK_DIR/$VOID_IMAGE" ]; then
    echo "   Downloading from Void Linux repository..."
    echo "   URL: $BASE_IMAGE_URL"
    echo ""
    
    # Download with progress bar
    if curl -L -f --progress-bar "$BASE_IMAGE_URL" -o "$WORK_DIR/$VOID_IMAGE"; then
        echo ""
        echo "✅ Downloaded successfully"
    else
        CURL_EXIT=$?
        echo ""
        echo "❌ Download failed (exit code: $CURL_EXIT)"
        echo ""
        echo "This could be due to:"
        echo "  1. Network restrictions in your environment"
        echo "  2. Repository temporarily unavailable"
        echo ""
        echo "Manual workaround:"
        echo "  1. Visit: https://voidlinux.org/download/"
        echo "  2. Download: Raspberry Pi (aarch64) PLATFORMFS (musl)"
        echo "  3. Or direct link: $BASE_IMAGE_URL"
        echo "  4. Save to: $WORK_DIR/$VOID_IMAGE"
        echo "  5. Re-run: just create-image"
        echo ""
        rm -f "$WORK_DIR/$VOID_IMAGE"
        exit 1
    fi
    
    # Verify it's actually an xz file
    if ! file "$WORK_DIR/$VOID_IMAGE" | grep -q "XZ compressed"; then
        echo "❌ Downloaded file is not a valid XZ archive!"
        echo ""
        file "$WORK_DIR/$VOID_IMAGE"
        echo ""
        echo "File appears to be:"
        head -c 100 "$WORK_DIR/$VOID_IMAGE"
        echo ""
        rm -f "$WORK_DIR/$VOID_IMAGE"
        exit 1
    fi
    
    FILE_SIZE=$(du -h "$WORK_DIR/$VOID_IMAGE" | cut -f1)
    echo "   Size: $FILE_SIZE"
else
    echo "✅ Using cached PLATFORMFS: $VOID_IMAGE"
    FILE_SIZE=$(du -h "$WORK_DIR/$VOID_IMAGE" | cut -f1)
    echo "   Size: $FILE_SIZE"
fi
echo ""

# Create work image
TIMESTAMP=$(date +%Y%m%d-%H%M%S)
WORK_IMAGE="${WORK_DIR}/mdma-beacon-${TIMESTAMP}-rpi5.img"
OUTPUT_IMAGE="${OUTPUT_DIR}/mdma-beacon-${TIMESTAMP}-rpi5.img.xz"

echo "💾 Creating disk image..."
# Create 32GB sparse image
dd if=/dev/zero of="$WORK_IMAGE" bs=1 count=0 seek=32G status=none
echo "✅ Image created"
echo ""

# Partition the image
echo "🗂️  Partitioning..."
sudo parted "$WORK_IMAGE" --script mklabel msdos
sudo parted "$WORK_IMAGE" --script mkpart primary fat32 1MiB 256MiB
sudo parted "$WORK_IMAGE" --script set 1 boot on
sudo parted "$WORK_IMAGE" --script mkpart primary ext4 256MiB 100%
echo "✅ Partitioned"
echo ""

# Setup loop device
LOOP_DEV=$(sudo losetup --show -fP "$WORK_IMAGE")
echo "📍 Loop device: $LOOP_DEV"

# Format partitions
echo "💿 Formatting partitions..."
sudo mkfs.vfat -F 32 "${LOOP_DEV}p1" > /dev/null 2>&1
sudo mkfs.ext4 -F "${LOOP_DEV}p2" > /dev/null 2>&1
echo "✅ Formatted"
echo ""

# Mount root filesystem
echo "📂 Mounting root filesystem..."
sudo guestmount -a "$WORK_IMAGE" -m /dev/sda2 --rw "$MOUNT_POINT"
echo "✅ Mounted at $MOUNT_POINT"
echo ""

# Extract base system
echo "📦 Extracting base Void Linux system..."
sudo tar -xJf "$WORK_DIR/$VOID_IMAGE" -C "$MOUNT_POINT" --strip-components=1
echo "✅ Base system extracted"
echo ""

# Configure repository
echo "🔧 Configuring package repositories..."
sudo bash -c "cat > '$MOUNT_POINT/etc/xbps.d/10-mdma-repo.conf'" <<EOF
repository=$ONLINE_REPO
EOF
echo "✅ Repository configured"
echo ""

# Check for QEMU user emulation
echo "🔍 Checking for QEMU user emulation..."
if [ ! -f /usr/bin/qemu-aarch64-static ]; then
    echo "❌ Error: QEMU user emulation not found!"
    echo ""
    echo "Install with:"
    echo "  sudo pacman -S qemu-user-static qemu-user-static-binfmt"
    echo "  sudo systemctl restart systemd-binfmt.service"
    exit 1
fi

if [ ! -f /proc/sys/fs/binfmt_misc/qemu-aarch64 ]; then
    echo "⚠️  Warning: binfmt_misc not registered for ARM64"
    echo "Run: sudo systemctl restart systemd-binfmt.service"
fi

echo "✅ QEMU user emulation available"
echo ""

# Copy QEMU into chroot
echo "📋 Setting up ARM64 emulation in chroot..."
sudo mkdir -p "$MOUNT_POINT/usr/bin"
sudo cp /usr/bin/qemu-aarch64-static "$MOUNT_POINT/usr/bin/"
echo "✅ QEMU copied to chroot"
echo ""

# Mount required filesystems for chroot
echo "📂 Mounting filesystems for chroot..."
sudo mount --bind /dev "$MOUNT_POINT/dev"
sudo mount --bind /proc "$MOUNT_POINT/proc"
sudo mount --bind /sys "$MOUNT_POINT/sys"

# Copy DNS configuration for network access in chroot
echo "🌐 Configuring DNS resolution..."
sudo cp /etc/resolv.conf "$MOUNT_POINT/etc/resolv.conf"

echo "✅ Filesystems mounted and DNS configured"
echo ""

# Note: Public key is embedded in repository metadata by xbps-rindex --sign
# xbps-install will download it automatically when syncing the repository
echo "📦 Installing packages via chroot (this runs post-install scripts)..."
echo "   This creates users, directories, and sets up everything properly"
echo ""

# Update xbps first
echo "Updating xbps..."
sudo XBPS_ARCH=aarch64 chroot "$MOUNT_POINT" xbps-install -Suy xbps

echo ""
echo "Installing beacon and dependencies..."
sudo XBPS_ARCH=aarch64 chroot "$MOUNT_POINT" xbps-install -Sy beacon

echo ""
echo "✅ Packages installed with post-install scripts executed!"

echo ""
echo "✅ Packages installed with post-install scripts executed!"
echo ""

# Enable dependency services (dbus and avahi)
# Note: beacon service is automatically enabled by its INSTALL script
echo "🔧 Enabling dependency services..."

echo "   Enabling dbus..."
sudo chroot "$MOUNT_POINT" ln -sf /etc/sv/dbus /var/service/dbus || true

echo "   Enabling avahi-daemon..."
sudo chroot "$MOUNT_POINT" ln -sf /etc/sv/avahi-daemon /var/service/avahi-daemon || true

echo "   beacon service enabled by package INSTALL script ✅"
echo "✅ Dependency services enabled"
echo ""

# Unmount chroot filesystems
echo "📂 Unmounting chroot filesystems..."
sudo umount "$MOUNT_POINT/sys" || true
sudo umount "$MOUNT_POINT/proc" || true
sudo umount "$MOUNT_POINT/dev" || true
echo "✅ Chroot filesystems unmounted"
echo ""

# Unmount and cleanup
echo "💾 Unmounting..."
sudo guestunmount "$MOUNT_POINT"
sudo losetup -d "$LOOP_DEV" 2>/dev/null || true
echo "✅ Unmounted"
echo ""

# Compress
echo "🗜️  Compressing..."
xz -9 -T0 "$WORK_IMAGE"
mv "${WORK_IMAGE}.xz" "$OUTPUT_IMAGE"
echo "✅ Compressed"
echo ""

# Summary
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "✅ SD card image ready!"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "Image: $OUTPUT_IMAGE"
ls -lh "$OUTPUT_IMAGE"
echo ""
echo "Flash with:"
echo "  xz -dc $OUTPUT_IMAGE | sudo dd of=/dev/sdX bs=4M status=progress conv=fsync"
echo ""
echo "🎉 That was MUCH simpler than before!"
