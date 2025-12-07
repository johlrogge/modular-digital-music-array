#!/usr/bin/env bash
# Create MDMA Bootable Image - Using guestfish (Arch Compatible)
# Auto-discovers latest Void Linux image

set -euo pipefail

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

echo "🔧 MDMA Bootable Image Creator (guestfish version)"
echo "===================================================="
echo ""

# Configuration
WORK_DIR="${HOME}/mdma-images"
OUTPUT_DIR="${WORK_DIR}/output"
TIMESTAMP=$(date +%Y%m%d)
OUTPUT_IMAGE="mdma-beacon-${TIMESTAMP}-rpi5.img"

# Verify we're NOT running as root
if [ "$EUID" -eq 0 ]; then
    echo -e "${RED}❌ Do NOT run this script as root!${NC}"
    echo "This script uses libguestfs which doesn't require root."
    exit 1
fi

# Find workspace root
if [ -f "Cargo.toml" ]; then
    WORKSPACE_ROOT="$(pwd)"
elif [ -f "../Cargo.toml" ]; then
    WORKSPACE_ROOT="$(cd .. && pwd)"
else
    echo -e "${RED}❌ Cannot find workspace root (no Cargo.toml)${NC}"
    echo "Please run from your MDMA workspace directory"
    exit 1
fi

BEACON_BINARY="${WORKSPACE_ROOT}/target/aarch64-unknown-linux-gnu/release/beacon"

echo "Workspace: $WORKSPACE_ROOT"
echo "Output directory: $OUTPUT_DIR"
echo ""

# Check dependencies
echo "Checking dependencies..."
if ! command -v guestfish &> /dev/null; then
    echo -e "${RED}❌ guestfish not installed${NC}"
    echo ""
    echo "Install libguestfs:"
    echo "  sudo pacman -S libguestfs"
    exit 1
fi
echo -e "${GREEN}✅ guestfish found (no AUR needed!)${NC}"

if ! command -v xz &> /dev/null; then
    echo -e "${RED}❌ xz not installed${NC}"
    exit 1
fi
echo -e "${GREEN}✅ xz found${NC}"
echo ""

# Step 1: Verify beacon
echo "Step 1: Verifying beacon binary..."
if [ ! -f "$BEACON_BINARY" ]; then
    echo -e "${RED}❌ Beacon binary not found!${NC}"
    echo "Expected: $BEACON_BINARY"
    echo ""
    echo "Build it:"
    echo "  just beacon-native && just beacon-strip"
    exit 1
fi

if ! file "$BEACON_BINARY" | grep -q "ARM aarch64"; then
    echo -e "${RED}❌ Not an ARM64 binary!${NC}"
    file "$BEACON_BINARY"
    exit 1
fi

BEACON_SIZE=$(du -h "$BEACON_BINARY" | cut -f1)
echo -e "${GREEN}✅ Beacon binary ($BEACON_SIZE)${NC}"
echo ""

# Step 2: Setup directories
echo "Step 2: Creating directories..."
mkdir -p "$WORK_DIR" "$OUTPUT_DIR" "${WORK_DIR}/staging"
cd "$WORK_DIR"
echo -e "${GREEN}✅ Directories ready${NC}"
echo ""

# Step 3: Find and download Void Linux
echo "Step 3: Finding Void Linux image..."
echo ""

VOID_IMAGE="void-rpi-aarch64.img.xz"

# Check if already have it (and it's valid)
if [ -f "$VOID_IMAGE" ]; then
    FILE_SIZE=$(stat -c%s "$VOID_IMAGE" 2>/dev/null || stat -f%z "$VOID_IMAGE" 2>/dev/null || echo "0")
    if [ "$FILE_SIZE" -lt 10000000 ]; then
        echo -e "${YELLOW}⚠️  Cached file corrupted (only $FILE_SIZE bytes)${NC}"
        echo "Removing and re-downloading..."
        rm "$VOID_IMAGE"
        VOID_URL=""  # Will trigger download below
    else
        echo -e "${GREEN}✅ Using cached: $VOID_IMAGE${NC}"
        echo "(Delete to download fresh)"
        VOID_URL=""
    fi
# Check for custom URL
elif [ -n "${VOID_IMAGE_URL:-}" ]; then
    echo -e "${BLUE}Using VOID_IMAGE_URL: $VOID_IMAGE_URL${NC}"
    VOID_URL="$VOID_IMAGE_URL"
else
    echo "Searching for latest image..."
    
    # Try ARM platform images (correct type for Raspberry Pi!)
    URLS=(
        # 2025 images first (from ARM platforms)
        "https://repo-default.voidlinux.org/live/20250202/void-rpi-aarch64-20250202.img.xz"
        "https://repo-default.voidlinux.org/live/20241201/void-rpi-aarch64-20241201.img.xz"
        "https://alpha.de.repo.voidlinux.org/live/20250202/void-rpi-aarch64-20250202.img.xz"
        # Fallback to older if needed
        "https://repo-default.voidlinux.org/live/20240314/void-rpi-aarch64-20240314.img.xz"
    )
    
    VOID_URL=""
    for url in "${URLS[@]}"; do
        echo "  Trying: $(basename "$url")..."
        if wget --spider -q "$url" 2>/dev/null; then
            VOID_URL="$url"
            echo -e "  ${GREEN}✓ Available!${NC}"
            break
        fi
        echo "    Not available"
    done
    
    if [ -z "$VOID_URL" ]; then
        echo ""
        echo -e "${RED}❌ Could not find working Void image URL${NC}"
        echo ""
        echo "MANUAL DOWNLOAD:"
        echo "  1. Visit: https://voidlinux.org/download/"
        echo "  2. Get: Raspberry Pi aarch64 image"
        echo "  3. Save as: $WORK_DIR/void-rpi-aarch64.img.xz"
        echo "  4. Re-run script"
        echo ""
        echo "Or use custom URL:"
        echo "  VOID_IMAGE_URL='https://...' $0"
        exit 1
    fi
fi

# Download if needed
if [ -n "$VOID_URL" ]; then
    echo ""
    echo "Downloading: $(basename "$VOID_URL")"
    echo "Size: ~150-200MB"
    echo ""
    
    if ! wget --progress=bar:force "$VOID_URL" -O "$VOID_IMAGE"; then
        echo ""
        echo -e "${RED}❌ Download failed${NC}"
        rm -f "$VOID_IMAGE"  # Remove partial download
        exit 1
    fi
    echo ""
    echo -e "${GREEN}✅ Downloaded${NC}"
fi

ls -lh "$VOID_IMAGE"
echo ""

# Step 4: Extract
echo "Step 4: Extracting image..."
EXTRACTED_IMG="void-rpi-aarch64.img"
[ -f "$EXTRACTED_IMG" ] && rm "$EXTRACTED_IMG"

xz -dc "$VOID_IMAGE" > "$EXTRACTED_IMG"
echo -e "${GREEN}✅ Extracted${NC}"
ls -lh "$EXTRACTED_IMG"
echo ""

# Step 5: Preparing staging files...
echo "Step 5: Preparing staging files..."

# Copy beacon
cp "$BEACON_BINARY" "${WORK_DIR}/staging/beacon"
chmod +x "${WORK_DIR}/staging/beacon"

# Create beacon service script
cat > "${WORK_DIR}/staging/beacon-run" <<'EOF'
#!/bin/sh
exec 2>&1
exec chpst -u root /usr/local/bin/beacon --apply --port 80
EOF
chmod +x "${WORK_DIR}/staging/beacon-run"

# Create avahi-daemon service script (missing in base image!)
cat > "${WORK_DIR}/staging/avahi-daemon-run" <<'EOF'
#!/bin/sh
exec 2>&1
[ -r ./conf ] && . ./conf
exec avahi-daemon -s ${OPTS:=--no-chroot}
EOF
chmod +x "${WORK_DIR}/staging/avahi-daemon-run"

# Create hostname file
echo "welcome-to-mdma" > "${WORK_DIR}/staging/hostname"

# Create hosts file
cat > "${WORK_DIR}/staging/hosts" <<'EOF'
127.0.0.1   localhost
127.0.1.1   welcome-to-mdma.local welcome-to-mdma

::1         localhost ip6-localhost ip6-loopback
ff02::1     ip6-allnodes
ff02::2     ip6-allrouters
EOF

# Create network config
echo "interface eth0" > "${WORK_DIR}/staging/eth0.conf"

echo -e "${GREEN}✅ Files staged${NC}"
echo ""

# Step 6: Copy to output
echo "Step 6: Preparing output image..."
cp "$EXTRACTED_IMG" "${OUTPUT_DIR}/${OUTPUT_IMAGE}"
echo -e "${GREEN}✅ Ready for modification${NC}"
echo ""

# Step 7: Modify with guestfish
echo "Step 7: Modifying image (takes 2-3 min)..."
echo ""

# First, download sshd_config to modify it
guestfish --rw -a "${OUTPUT_DIR}/${OUTPUT_IMAGE}" -m /dev/sda2 <<EOF
download /etc/ssh/sshd_config ${WORK_DIR}/staging/sshd_config
EOF

# Modify SSH config on host (no architecture issues!)
sed -i 's/#PermitRootLogin.*/PermitRootLogin yes/' "${WORK_DIR}/staging/sshd_config" || true

# Now build the main guestfish commands
cat > "${WORK_DIR}/guestfish-commands.txt" <<EOF
run
mount /dev/sda2 /

# Install beacon binary
mkdir-p /usr/local/bin
upload ${WORK_DIR}/staging/beacon /usr/local/bin/beacon
chmod 0755 /usr/local/bin/beacon

# Create beacon service
mkdir-p /etc/sv/beacon
upload ${WORK_DIR}/staging/beacon-run /etc/sv/beacon/run
chmod 0755 /etc/sv/beacon/run

# Create avahi-daemon service (missing in base image but binary exists!)
mkdir-p /etc/sv/avahi-daemon
upload ${WORK_DIR}/staging/avahi-daemon-run /etc/sv/avahi-daemon/run
chmod 0755 /etc/sv/avahi-daemon/run

# Enable services (Void runit way!)
ln-sf /etc/sv/beacon /etc/runit/runsvdir/default/beacon
ln-sf /etc/sv/avahi-daemon /etc/runit/runsvdir/default/avahi-daemon

# Upload config files (all created/modified on host!)
upload ${WORK_DIR}/staging/hostname /etc/hostname
upload ${WORK_DIR}/staging/hosts /etc/hosts
upload ${WORK_DIR}/staging/sshd_config /etc/ssh/sshd_config

# Configure network
mkdir-p /etc/dhcpcd.conf.d
upload ${WORK_DIR}/staging/eth0.conf /etc/dhcpcd.conf.d/eth0.conf
EOF

# Run guestfish with commands file
guestfish --rw -a "${OUTPUT_DIR}/${OUTPUT_IMAGE}" -f "${WORK_DIR}/guestfish-commands.txt"
rm "${WORK_DIR}/guestfish-commands.txt"

echo -e "${GREEN}✅ Image modified${NC}"
echo ""

# Step 8: Compress
echo "Step 8: Compressing (takes 2-3 min)..."
cd "$OUTPUT_DIR"
[ -f "${OUTPUT_IMAGE}.xz" ] && rm "${OUTPUT_IMAGE}.xz"

xz -z -9 -T 0 "$OUTPUT_IMAGE"
FINAL_IMAGE="${OUTPUT_IMAGE}.xz"
echo -e "${GREEN}✅ Compressed${NC}"
echo ""

# Step 9: Checksum
echo "Step 9: Creating checksum..."
sha256sum "$FINAL_IMAGE" > "${FINAL_IMAGE}.sha256"
echo -e "${GREEN}✅ Checksum ready${NC}"
echo ""

# Step 10: README
cat > "README.txt" <<EOF
MDMA Beacon - Raspberry Pi 5 Bootable Image
============================================

Created: $(date)
Image: $FINAL_IMAGE
SHA256: $(cat "${FINAL_IMAGE}.sha256")

QUICK START
-----------

Flash to SD (8GB+):
  - Raspberry Pi Imager
  - Etcher  
  - dd: xz -dc $FINAL_IMAGE | sudo dd of=/dev/sdX bs=4M

Then: http://welcome-to-mdma.local

GitHub: github.com/johlrogge/modular-digital-music-array
EOF

# Done!
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo -e "${GREEN}🎉 SUCCESS!${NC}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "Image: $FINAL_IMAGE"
echo "Size: $(du -h "$FINAL_IMAGE" | cut -f1)"
echo "Location: $OUTPUT_DIR"
echo ""
ls -lh "$OUTPUT_DIR"
echo ""
echo "Flash and boot → http://welcome-to-mdma.local"
echo ""
