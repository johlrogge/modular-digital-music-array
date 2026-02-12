# Golden Image Workflow - Create bootable image from working SD card

# Copy setup script to Pi
golden-copy-script PI_IP:
    @echo "📤 Copying setup script to Pi..."
    scp scripts/setup-beacon-on-pi.sh root@{{PI_IP}}:/root/
    @echo "✅ Script copied!"
    @echo ""
    @echo "Now SSH in and run it:"
    @echo "  ssh root@{{PI_IP}}"
    @echo "  chmod +x setup-beacon-on-pi.sh"
    @echo "  ./setup-beacon-on-pi.sh"

# SSH to Pi for manual setup
golden-ssh PI_IP:
    ssh root@{{PI_IP}}

# Create image from SD card (after Pi is shutdown and SD card in dev machine)
golden-create-image DEVICE:
    #!/usr/bin/env bash
    set -euo pipefail
    
    # Verify device exists
    if [ ! -b "{{DEVICE}}" ]; then
        echo "❌ Device {{DEVICE}} not found"
        echo "   Use: lsblk to find your SD card"
        exit 1
    fi
    
    # Safety check
    echo "⚠️  About to read entire device {{DEVICE}}"
    echo "   This will create an image of the SD card"
    read -p "Continue? (yes/no): " confirm
    
    if [ "$confirm" != "yes" ]; then
        echo "Aborted"
        exit 1
    fi
    
    TIMESTAMP=$(date +%Y%m%d-%H%M%S)
    OUTPUT_DIR=~/mdma-images/golden
    mkdir -p "$OUTPUT_DIR"
    
    IMAGE_FILE="$OUTPUT_DIR/mdma-beacon-golden-$TIMESTAMP.img"
    
    echo ""
    echo "📀 Reading SD card..."
    sudo dd if={{DEVICE}} of="$IMAGE_FILE" bs=4M status=progress conv=fsync
    
    # Fix ownership so compression works
    sudo chown $(whoami):$(whoami) "$IMAGE_FILE"
    
    # Check if PiShrink is available
    PISHRINK=""
    if [ -f ~/mdma-images/pishrink.sh ]; then
        PISHRINK=~/mdma-images/pishrink.sh
    elif command -v pishrink.sh &> /dev/null; then
        PISHRINK=pishrink.sh
    fi
    
    if [ -n "$PISHRINK" ]; then
        echo ""
        echo "🔧 Shrinking image with PiShrink..."
        echo "   (This may take a few minutes)"
        sudo "$PISHRINK" "$IMAGE_FILE"
        echo "✅ Image shrunk"
    else
        echo ""
        echo "⚠️  PiShrink not found - image not shrunk"
        echo "   Image will be ~32GB when extracted"
        echo ""
        echo "   To install PiShrink:"
        echo "   curl -L https://raw.githubusercontent.com/Drewsif/PiShrink/master/pishrink.sh -o ~/mdma-images/pishrink.sh"
        echo "   chmod +x ~/mdma-images/pishrink.sh"
        echo ""
        read -p "Continue without shrinking? (yes/no): " continue_unshrunk
        if [ "$continue_unshrunk" != "yes" ]; then
            echo "Aborted - image saved at: $IMAGE_FILE"
            exit 1
        fi
    fi
    
    echo ""
    echo "🗜️  Compressing image..."
    xz -9 -T0 "$IMAGE_FILE"
    
    echo ""
    echo "✅ Golden image created!"
    echo "   Location: $IMAGE_FILE.xz"
    
    SIZE=$(du -h "$IMAGE_FILE.xz" | cut -f1)
    echo "   Compressed size: $SIZE"
    
    if [ -n "$PISHRINK" ]; then
        echo "   Extracted size: ~3-4GB (shrunk)"
    else
        echo "   Extracted size: ~32GB (not shrunk)"
    fi
    
    echo ""
    echo "🎯 To flash this image:"
    echo "   xz -dc $IMAGE_FILE.xz | sudo dd of=/dev/sdX bs=4M status=progress"

# Complete golden image workflow guide
golden-help:
    @echo "📋 Golden Image Workflow"
    @echo ""
    @echo "This creates a 'golden master' image from a working Pi."
    @echo ""
    @echo "Steps:"
    @echo "  1. Flash vanilla Void to SD card"
    @echo "     curl -LO https://repo-default.voidlinux.org/live/current/void-rpi-aarch64-20250202.img.xz"
    @echo "     xz -dc void-rpi-aarch64-20250202.img.xz | sudo dd of=/dev/sdX bs=4M status=progress"
    @echo ""
    @echo "  2. Boot Pi, find its IP (check router or use nmap)"
    @echo ""
    @echo "  3. Copy setup script to Pi"
    @echo "     just golden-copy-script 192.168.0.XXX"
    @echo ""
    @echo "  4. SSH to Pi and run setup"
    @echo "     just golden-ssh 192.168.0.XXX"
    @echo "     chmod +x setup-beacon-on-pi.sh"
    @echo "     ./setup-beacon-on-pi.sh"
    @echo ""
    @echo "  5. Test everything works"
    @echo "     ping welcome-to-mdma.local"
    @echo "     http://welcome-to-mdma.local/"
    @echo ""
    @echo "  6. Shutdown Pi"
    @echo "     shutdown -h now"
    @echo ""
    @echo "  7. Remove SD card, put in dev machine"
    @echo ""
    @echo "  8. Create golden image"
    @echo "     just golden-create-image /dev/sdX"
    @echo ""
    @echo "  9. Result: ~/mdma-images/golden/mdma-beacon-golden-TIMESTAMP.img.xz"
    @echo ""
    @echo "🎉 Now you have a working golden image to flash to all Pis!"
