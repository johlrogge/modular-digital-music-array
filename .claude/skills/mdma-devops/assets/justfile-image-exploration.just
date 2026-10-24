# MDMA Image Exploration Justfile Recipes
# Add these to your main justfile

# Mount the latest created image for exploration
image-mount:
    #!/usr/bin/env bash
    set -euo pipefail
    
    LATEST_IMAGE=$(ls -t ~/mdma-images/output/*.img.xz 2>/dev/null | head -1)
    
    if [ -z "$LATEST_IMAGE" ]; then
        echo "❌ No image found in ~/mdma-images/output/"
        exit 1
    fi
    
    echo "📦 Found image: $LATEST_IMAGE"
    
    # Extract if not already extracted
    EXTRACTED="${LATEST_IMAGE%.xz}"
    if [ ! -f "$EXTRACTED" ]; then
        echo "📦 Extracting image..."
        xz -d -k "$LATEST_IMAGE"
    fi
    
    # Create mount point (fix permissions if needed)
    MOUNT_POINT=~/mdma-images/explore
    if [ ! -d "$MOUNT_POINT" ]; then
        sudo mkdir -p "$MOUNT_POINT"
        sudo chown $(whoami):$(whoami) "$MOUNT_POINT"
    fi
    
    # Check if already mounted
    if sudo mountpoint -q "$MOUNT_POINT" 2>/dev/null; then
        echo "✅ Already mounted at $MOUNT_POINT"
    else
        echo "📂 Mounting image..."
        sudo guestmount -a "$EXTRACTED" -m /dev/sda2 --rw "$MOUNT_POINT"
        echo "✅ Mounted at $MOUNT_POINT"
    fi
    
    echo ""
    echo "🎯 Image mounted! You can now:"
    echo "   just image-shell      # Open shell in image"
    echo "   just image-check      # Verify packages and services"
    echo "   just image-fix        # Fix common issues"
    echo "   just image-unmount    # Unmount when done"

# Unmount the explored image
image-unmount:
    #!/usr/bin/env bash
    set -euo pipefail
    
    MOUNT_POINT=~/mdma-images/explore
    
    if mountpoint -q "$MOUNT_POINT" 2>/dev/null; then
        echo "💾 Unmounting..."
        sudo guestunmount "$MOUNT_POINT"
        echo "✅ Unmounted"
    else
        echo "ℹ️  Not currently mounted"
    fi

# Open a shell in the mounted image (with QEMU chroot)
image-shell:
    #!/usr/bin/env bash
    set -euo pipefail
    
    MOUNT_POINT=~/mdma-images/explore
    
    if ! sudo mountpoint -q "$MOUNT_POINT" 2>/dev/null; then
        echo "❌ Image not mounted. Run: just image-mount"
        exit 1
    fi
    
    echo "🐚 Opening shell in image..."
    echo "   (Type 'exit' to leave)"
    echo ""
    
    # Setup QEMU for ARM64 chroot
    sudo cp /usr/bin/qemu-aarch64-static "$MOUNT_POINT/usr/bin/" 2>/dev/null || true
    sudo mount --bind /dev "$MOUNT_POINT/dev" 2>/dev/null || true
    sudo mount --bind /proc "$MOUNT_POINT/proc" 2>/dev/null || true
    sudo mount --bind /sys "$MOUNT_POINT/sys" 2>/dev/null || true
    sudo cp /etc/resolv.conf "$MOUNT_POINT/etc/resolv.conf" 2>/dev/null || true
    
    # Open shell
    sudo XBPS_ARCH=aarch64 chroot "$MOUNT_POINT" /bin/bash
    
    # Cleanup mounts when done
    sudo umount "$MOUNT_POINT/sys" 2>/dev/null || true
    sudo umount "$MOUNT_POINT/proc" 2>/dev/null || true
    sudo umount "$MOUNT_POINT/dev" 2>/dev/null || true

# Check image state (packages, services, config)
image-check:
    #!/usr/bin/env bash
    set -euo pipefail
    
    MOUNT_POINT=~/mdma-images/explore
    
    if ! sudo mountpoint -q "$MOUNT_POINT" 2>/dev/null; then
        echo "❌ Image not mounted. Run: just image-mount"
        exit 1
    fi
    
    echo "🔍 Checking image state..."
    echo ""
    
    # Setup QEMU
    sudo cp /usr/bin/qemu-aarch64-static "$MOUNT_POINT/usr/bin/" 2>/dev/null || true
    sudo mount --bind /dev "$MOUNT_POINT/dev" 2>/dev/null || true
    sudo mount --bind /proc "$MOUNT_POINT/proc" 2>/dev/null || true
    sudo mount --bind /sys "$MOUNT_POINT/sys" 2>/dev/null || true
    
    # Run checks
    sudo chroot "$MOUNT_POINT" bash -c '
    echo "=== Installed Packages ==="
    xbps-query -l | grep -E "beacon|avahi|dbus" || echo "None found"
    
    echo ""
    echo "=== Service Symlinks in /var/service ==="
    ls -la /var/service/ 2>/dev/null || echo "/var/service does not exist"
    
    echo ""
    echo "=== Service Definitions in /etc/sv ==="
    ls -la /etc/sv/ | grep -E "beacon|avahi|dbus" || echo "None found"
    
    echo ""
    echo "=== Hostname ==="
    cat /etc/hostname 2>/dev/null || echo "No hostname file"
    
    echo ""
    echo "=== Repository Configuration ==="
    cat /etc/xbps.d/*.conf 2>/dev/null || echo "No repo config"
    
    echo ""
    echo "=== /var/service Type ==="
    if [ -d /var/service ]; then
        echo "/var/service is a directory ✅"
    elif [ -L /var/service ]; then
        echo "/var/service is a symlink: $(readlink /var/service)"
    elif [ -e /var/service ]; then
        echo "/var/service exists but is not a directory ❌"
        file /var/service
    else
        echo "/var/service does not exist ❌"
    fi
    
    echo ""
    echo "=== Beacon Binary ==="
    if [ -f /usr/bin/beacon ]; then
        ls -lh /usr/bin/beacon
        file /usr/bin/beacon
    else
        echo "Beacon binary not found ❌"
    fi
    '
    
    # Cleanup
    sudo umount "$MOUNT_POINT/sys" 2>/dev/null || true
    sudo umount "$MOUNT_POINT/proc" 2>/dev/null || true
    sudo umount "$MOUNT_POINT/dev" 2>/dev/null || true
    
    echo ""
    echo "✅ Check complete!"

# Fix common issues in the mounted image
image-fix:
    #!/usr/bin/env bash
    set -euo pipefail
    
    MOUNT_POINT=~/mdma-images/explore
    
    if ! sudo mountpoint -q "$MOUNT_POINT" 2>/dev/null; then
        echo "❌ Image not mounted. Run: just image-mount"
        exit 1
    fi
    
    echo "🔧 Fixing common issues..."
    echo ""
    
    # Setup QEMU
    sudo cp /usr/bin/qemu-aarch64-static "$MOUNT_POINT/usr/bin/" 2>/dev/null || true
    sudo mount --bind /dev "$MOUNT_POINT/dev" 2>/dev/null || true
    sudo mount --bind /proc "$MOUNT_POINT/proc" 2>/dev/null || true
    sudo mount --bind /sys "$MOUNT_POINT/sys" 2>/dev/null || true
    sudo cp /etc/resolv.conf "$MOUNT_POINT/etc/resolv.conf" 2>/dev/null || true
    
    # Run fixes
    sudo chroot "$MOUNT_POINT" bash -c '
    echo "=== Fixing /var/service ==="
    if [ ! -d /var/service ]; then
        rm -f /var/service
        mkdir -p /var/service
        echo "✅ Created /var/service directory"
    else
        echo "✅ /var/service already a directory"
    fi
    
    echo ""
    echo "=== Installing missing packages ==="
    
    # Check and install avahi
    if ! xbps-query avahi >/dev/null 2>&1; then
        echo "Installing avahi..."
        xbps-install -y avahi
    else
        echo "✅ avahi already installed"
    fi
    
    # Check and install dbus
    if ! xbps-query dbus >/dev/null 2>&1; then
        echo "Installing dbus..."
        xbps-install -y dbus
    else
        echo "✅ dbus already installed"
    fi
    
    # Check and install beacon
    if ! xbps-query beacon >/dev/null 2>&1; then
        echo "Installing beacon..."
        xbps-install -y beacon
    else
        echo "✅ beacon already installed"
    fi
    
    echo ""
    echo "=== Enabling services ==="
    cd /var/service
    
    if [ ! -L beacon ]; then
        ln -sf /etc/sv/beacon .
        echo "✅ Enabled beacon"
    else
        echo "✅ beacon already enabled"
    fi
    
    if [ ! -L dbus ]; then
        ln -sf /etc/sv/dbus .
        echo "✅ Enabled dbus"
    else
        echo "✅ dbus already enabled"
    fi
    
    if [ ! -L avahi-daemon ]; then
        ln -sf /etc/sv/avahi-daemon .
        echo "✅ Enabled avahi-daemon"
    else
        echo "✅ avahi-daemon already enabled"
    fi
    
    echo ""
    echo "=== Fixing supervise symlinks ==="
    
    # Check and fix beacon supervise symlink
    if [ ! -e /etc/sv/beacon/supervise ]; then
        ln -sf /run/runit/supervise.beacon /etc/sv/beacon/supervise
        echo "✅ Created beacon supervise symlink"
    else
        echo "✅ beacon supervise symlink exists"
    fi
    
    # Verify others (should exist from packages)
    if [ ! -e /etc/sv/dbus/supervise ]; then
        ln -sf /run/runit/supervise.dbus /etc/sv/dbus/supervise
        echo "⚠️  Created missing dbus supervise symlink"
    fi
    
    if [ ! -e /etc/sv/avahi-daemon/supervise ]; then
        ln -sf /run/runit/supervise.avahi-daemon /etc/sv/avahi-daemon/supervise
        echo "⚠️  Created missing avahi-daemon supervise symlink"
    fi
    
    echo ""
    echo "=== Setting hostname ==="
    echo "welcome-to-mdma" > /etc/hostname
    echo "✅ Hostname set to welcome-to-mdma"
    '
    
    # Cleanup
    sudo umount "$MOUNT_POINT/sys" 2>/dev/null || true
    sudo umount "$MOUNT_POINT/proc" 2>/dev/null || true
    sudo umount "$MOUNT_POINT/dev" 2>/dev/null || true
    
    echo ""
    echo "✅ Fixes applied!"
    echo ""
    echo "Run 'just image-check' to verify"

# Complete workflow: mount, check, fix if needed, recompress
image-debug:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "🔍 Starting image debug workflow..."
    echo ""
    
    # Mount
    just image-mount
    
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    read -p "Press Enter to check image state..."
    echo ""
    
    # Check
    just image-check
    
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    read -p "Press Enter to apply fixes (or Ctrl+C to skip)..."
    echo ""
    
    # Fix
    just image-fix
    
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    read -p "Press Enter to verify fixes..."
    echo ""
    
    # Check again
    just image-check
    
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "Would you like to:"
    echo "  1. Recompress the fixed image"
    echo "  2. Open a shell for manual fixes"
    echo "  3. Unmount and exit"
    read -p "Choice (1/2/3): " choice
    
    case $choice in
        1)
            just image-unmount
            echo ""
            echo "🗜️  Recompressing image..."
            LATEST_IMAGE=$(ls -t ~/mdma-images/output/*.img 2>/dev/null | head -1)
            if [ -f "$LATEST_IMAGE" ]; then
                rm -f "${LATEST_IMAGE}.xz"
                xz -9 -T0 "$LATEST_IMAGE"
                echo "✅ Recompressed: ${LATEST_IMAGE}.xz"
            fi
            ;;
        2)
            echo ""
            just image-shell
            echo ""
            just image-unmount
            ;;
        3)
            just image-unmount
            ;;
    esac
    
    echo ""
    echo "✅ Debug complete!"

# List all available image exploration commands
image-help:
    @echo "📋 Image Exploration Commands:"
    @echo ""
    @echo "  just image-mount           - Mount latest image for exploration"
    @echo "  just image-unmount         - Unmount the image"
    @echo "  just image-shell           - Open shell in mounted image"
    @echo "  just image-check           - Verify packages, services, config"
    @echo "  just image-fix             - Fix common issues automatically"
    @echo "  just image-fix-permissions - Fix ~/mdma-images directory permissions"
    @echo "  just image-debug           - Complete debug workflow (recommended)"
    @echo "  just image-help            - Show this help"
    @echo ""
    @echo "📖 Typical Workflow:"
    @echo ""
    @echo "  1. just create-image        # Build the image"
    @echo "  2. just image-debug         # Debug interactively"
    @echo "     OR"
    @echo "  2. just image-mount         # Mount manually"
    @echo "  3. just image-check         # Check state"
    @echo "  4. just image-fix           # Auto-fix issues"
    @echo "  5. just image-check         # Verify fixes"
    @echo "  6. just image-shell         # Manual fixes if needed"
    @echo "  7. just image-unmount       # Unmount"
    @echo "  8. Flash and test!"
    @echo ""
    @echo "💡 Pro tip: Use 'just image-debug' for guided workflow!"

# Fix permissions on ~/mdma-images directory (if created by sudo)
image-fix-permissions:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "🔧 Fixing ~/mdma-images permissions..."
    
    if [ -d ~/mdma-images ]; then
        sudo chown -R $(whoami):$(whoami) ~/mdma-images
        echo "✅ Permissions fixed"
    else
        echo "ℹ️  ~/mdma-images doesn't exist yet"
    fi
