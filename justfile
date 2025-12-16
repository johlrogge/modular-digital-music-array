default:
    just --list

# Check prerequisites for package building
[group('setup')]
check-prereqs:
    ./scripts/utils/check-prerequisites.sh

# watch and run check, test, build and clippy when files change
[group('build')]
watch:
    cargo watch -x check -x test -x build -x clippy

# just build
[group('build')]
build:
    cargo build

# Quick cross-compile beacon using cross-rs (recommended for Arch)
[group('build')]
beacon-cross:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v cross &> /dev/null; then
        echo "Installing cross (Docker-based cross-compiler)..."
        cargo install cross --git https://github.com/cross-rs/cross
    fi
    echo "Building beacon for aarch64..."
    cross build --release --target aarch64-unknown-linux-gnu --bin beacon
    echo ""
    echo "✅ Beacon built!"
    file target/aarch64-unknown-linux-gnu/release/beacon
    ls -lh target/aarch64-unknown-linux-gnu/release/beacon

# Build beacon with native cargo (requires system cross-compiler)
[group('build')]
beacon-native:
    cargo build --release --target aarch64-unknown-linux-gnu --bin beacon
    @echo ""
    @file target/aarch64-unknown-linux-gnu/release/beacon
    @ls -lh target/aarch64-unknown-linux-gnu/release/beacon

# Strip beacon binary for production
[group('build')]
beacon-strip:
    #!/usr/bin/env bash
    set -euo pipefail
    BEACON="target/aarch64-unknown-linux-gnu/release/beacon"
    if [ ! -f "$BEACON" ]; then
        echo "❌ Beacon not built yet. Run 'just beacon-cross' first"
        exit 1
    fi
    SIZE_BEFORE=$(stat -c%s "$BEACON" 2>/dev/null || stat -f%z "$BEACON")
    aarch64-linux-gnu-strip "$BEACON" || strip "$BEACON"
    SIZE_AFTER=$(stat -c%s "$BEACON" 2>/dev/null || stat -f%z "$BEACON")
    echo "Beacon stripped:"
    echo "  Before: $(numfmt --to=iec-i $SIZE_BEFORE 2>/dev/null || echo $SIZE_BEFORE bytes)"
    echo "  After:  $(numfmt --to=iec-i $SIZE_AFTER 2>/dev/null || echo $SIZE_AFTER bytes)"
    ls -lh "$BEACON"

# Check beacon dependencies for cross-compilation compatibility  
[group('build')]
beacon-deps:
    cargo tree --target aarch64-unknown-linux-gnu --package mdma-beacon

# Set up Cargo config for cross-compilation (native gcc method)
[group('build')]
setup-cross:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "Setting up Cargo cross-compilation config..."
    mkdir -p .cargo
    if [ ! -f .cargo/config.toml ]; then
        echo '[target.aarch64-unknown-linux-gnu]' > .cargo/config.toml
        echo 'linker = "aarch64-linux-gnu-gcc"' >> .cargo/config.toml
        echo '' >> .cargo/config.toml
        echo '[build]' >> .cargo/config.toml
        echo '# Uncomment to make aarch64 the default target' >> .cargo/config.toml
        echo '# target = "aarch64-unknown-linux-gnu"' >> .cargo/config.toml
        echo "✅ Created .cargo/config.toml"
    else
        echo "⚠️  .cargo/config.toml already exists"
        cat .cargo/config.toml
    fi

# Check if cross-compilation toolchain is available
[group('build')]
check-toolchain:
    #!/usr/bin/env bash
    echo "Checking cross-compilation options..."
    echo ""
    if command -v cross &> /dev/null; then
        echo "✅ cross-rs available (recommended)"
        echo "   Use: just beacon-cross"
    else
        echo "❌ cross-rs not found"
        echo "   Install: cargo install cross --git https://github.com/cross-rs/cross"
    fi
    echo ""
    if command -v aarch64-linux-gnu-gcc &> /dev/null; then
        echo "✅ aarch64-linux-gnu-gcc available"
        echo "   Use: just beacon-native"
    else
        echo "❌ aarch64-linux-gnu-gcc not found"
        echo "   Install (AUR): yay -S aarch64-linux-gnu-gcc"
    fi
    echo ""
    echo "Rust target:"
    if rustup target list | grep -q "aarch64-unknown-linux-gnu (installed)"; then
        echo "✅ aarch64-unknown-linux-gnu target installed"
    else
        echo "❌ aarch64-unknown-linux-gnu target not installed"
        echo "   Install: rustup target add aarch64-unknown-linux-gnu"
    fi

# Watch beacon and rebuild on changes (for development)
[group('dev')]
beacon-watch:
    cargo watch -x 'build --bin beacon'

# Run beacon locally (x86_64 - for development/testing)
[group('dev')]
beacon-run:
    cargo run --bin beacon

# Build beacon for current platform (development)
[group('dev')]
beacon-dev:
    cargo build --bin beacon
    @ls -lh target/debug/beacon

# ============================================================================
# CI/CD Build Recipes (Work Locally and in GitHub Actions)
# ============================================================================

# Build beacon for CI/CD (local or GitHub Actions)
[group('ci')]
ci-build-beacon:
    ./scripts/ci/build-beacon.sh

# Strip beacon for CI/CD deployment
[group('ci')]
ci-strip-beacon:
    ./scripts/ci/strip-beacon.sh

# Package beacon into deployable archive (legacy tar.gz format)
[group('ci')]
ci-package-beacon:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "📦 Packaging beacon..."
    TIMESTAMP=$(date +%Y%m%d-%H%M%S)
    PACKAGE_NAME="mdma-beacon-${TIMESTAMP}.tar.gz"
    
    mkdir -p dist
    tar -czf "dist/${PACKAGE_NAME}" \
        -C target/aarch64-unknown-linux-gnu/release \
        beacon
    
    echo "✅ Packaged: dist/${PACKAGE_NAME}"
    ls -lh "dist/${PACKAGE_NAME}"

# Full CI pipeline (build + strip + package) - legacy tar.gz
[group('ci')]
ci-pipeline: ci-build-beacon ci-strip-beacon ci-package-beacon
    @echo ""
    @echo "✅ CI Pipeline Complete!"
    @echo "   Beacon is ready for deployment"

# Test that beacon runs (sanity check)
[group('ci')]
ci-test-beacon:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "🧪 Testing beacon binary..."
    BEACON="target/aarch64-unknown-linux-gnu/release/beacon"
    if [ ! -f "$BEACON" ]; then
        echo "❌ Beacon not built. Run 'just ci-build-beacon' first"
        exit 1
    fi
    
    # Can't actually run ARM binary on x86, but we can check it's valid
    echo "Checking binary format..."
    file "$BEACON" | grep -q "ARM aarch64" || {
        echo "❌ Not an ARM64 binary!"
        exit 1
    }
    
    echo "Checking binary is executable..."
    test -x "$BEACON" || {
        echo "❌ Not executable!"
        exit 1
    }
    
    echo "✅ Beacon binary looks good (ARM64, executable)"

# Clean CI artifacts
[group('ci')]
ci-clean:
    rm -rf dist/
    rm -rf target/aarch64-unknown-linux-gnu/release/beacon
    @echo "✅ CI artifacts cleaned"

# Simulate full CI pipeline locally
[group('ci')]
ci-simulate:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "🎭 Simulating CI Pipeline Locally"
    echo "=================================="
    echo ""
    just ci-pipeline
    just ci-test-beacon
    echo ""
    echo "✅ Local CI simulation complete!"
    echo "   This is exactly what GitHub Actions will run"

# Check for local path dependencies (fails CI)
[group('ci')]
ci-check-deps:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "🔍 Checking for local path dependencies..."
    
    FOUND_PATHS=0
    for file in $(find . -name "Cargo.toml" -not -path "./target/*"); do
        if grep -E '^\s*path\s*=\s*"' "$file" | grep -v "workspace = true" > /dev/null 2>&1; then
            echo "❌ Found local path dependency in: $file"
            grep -n -E '^\s*path\s*=\s*"' "$file" | grep -v "workspace = true"
            FOUND_PATHS=1
        fi
    done
    if [ $FOUND_PATHS -eq 1 ]; then
        echo ""
        echo "❌ ERROR: Local path dependencies found!"
        echo "These will fail in CI. Use git dependencies instead:"
        echo '  stainless-facts = { git = "https://github.com/johlrogge/stainless_facts" }'
        exit 1
    fi
    
    echo "✅ No local path dependencies found"

# ============================================================================
# Void Package Building (Scripts-Based - No Auto-Install!)
# ============================================================================

# Build beacon Void package
[group('package')]
pkg-beacon: ci-build-beacon ci-strip-beacon
    ./scripts/package/create-package.sh

# Create repository structure and index
[group('package')]
pkg-repository: pkg-beacon
    ./scripts/package/create-repository.sh

# Full package build pipeline (what CI runs!)
[group('package')]
pkg-build-all: check-prereqs pkg-repository
    @echo ""
    @echo "🎉 Package build complete!"
    @echo ""
    @echo "Repository ready at: build/repository/"
    @echo ""
    @echo "To test locally:"
    @echo "  1. Serve repository: just pkg-serve"
    @echo "  2. On Pi: configure and install"

# Serve repository locally for testing
[group('package')]
pkg-serve:
    ./scripts/package/serve-repository.sh

# Test package installation on local Pi
[group('package')]
pkg-test-install PI_HOST:
    ./scripts/package/test-install.sh {{PI_HOST}}

# Show package version
[group('package')]
pkg-version:
    ./scripts/utils/get-version.sh

# Bump package revision (for same version)
[group('package')]
pkg-bump-revision:
    ./scripts/utils/bump-revision.sh

# Clean package build artifacts
[group('package')]
pkg-clean:
    rm -rf build/
    @echo "🧹 Package build directory cleaned"

# ============================================================================
# Maintenance
# ============================================================================

# Create archive
[group('maintenance')]
archive:
    #!/usr/bin/env bash
    set -euo pipefail
    TIMESTAMP=$(date +%Y%m%d_%H%M%S)
    ARCHIVE_NAME="mdma-workspace-${TIMESTAMP}.tar.gz"
    echo "Creating archive: ${ARCHIVE_NAME}"
    tar \
      --exclude='target' \
      --exclude='node_modules' \
      --exclude='.git' \
      --exclude='*.iso' \
      --exclude='*.img' \
      --exclude='*.qcow2' \
      --exclude='.cargo/registry' \
      --exclude='.cargo/git' \
      --exclude='*.tar.gz' \
      --exclude='*.tar' \
      --exclude='*.tar.bz2' \
      --exclude='*/benches/test_data/*' \
      --exclude='*/tests/test_data/*' \
      --exclude='*.flac' \
      --exclude='*.wav' \
      --exclude='*.mp3' \
      --exclude='*.jsonl' \
      --exclude='build' \
      --exclude='phantomjs' \
      --exclude='node-compile-cache' \
      --exclude='hsperfdata_*' \
      --exclude='dist' \
      --exclude='*.xbps' \
      --exclude='uv-*.lock' \
      -czf "/tmp/${ARCHIVE_NAME}" .
    mv "/tmp/${ARCHIVE_NAME}" .
    echo "✅ Created: ${ARCHIVE_NAME}"
    ls -lh "${ARCHIVE_NAME}"

# Check prerequisites including image creation tools
[group('setup')]
check-prereqs-image:
    ./scripts/utils/check-prerequisites.sh --image

# ============================================================================
# Image Creation
# ============================================================================

# Create SD card image with beacon installed via xbps
[group('image')]
create-image: check-prereqs-image pkg-build-all
    ./scripts/image/create-sd-card-simple.sh

# Network scanning recipes for finding Raspberry Pi

# Scan network for Raspberry Pi devices
pi-scan:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "🔍 Scanning for Raspberry Pi devices on network..."
    echo ""
    
    # Get local network range
    NETWORK=$(ip route | grep default | awk '{print $3}' | cut -d. -f1-3)
    
    if [ -z "$NETWORK" ]; then
        echo "❌ Could not detect network range"
        exit 1
    fi
    
    echo "Network: $NETWORK.0/24"
    echo ""
    
    # Check if nmap is installed
    if ! command -v nmap &> /dev/null; then
        echo "❌ nmap not found. Install it with:"
        echo "   sudo pacman -S nmap"
        exit 1
    fi
    
    echo "Scanning... (this takes ~30 seconds)"
    echo ""
    
    # Scan for devices and filter for Raspberry Pi
    sudo nmap -sn $NETWORK.0/24 | grep -B 2 "Raspberry\|DC:A6:32\|B8:27:EB\|E4:5F:01" || {
        echo "❌ No Raspberry Pi devices found"
        echo ""
        echo "Make sure:"
        echo "  - Pi is powered on"
        echo "  - Ethernet cable is connected"
        echo "  - Pi has had 60 seconds to boot"
        exit 1
    }
    
    echo ""
    echo "💡 To connect:"
    echo "   ssh root@<IP>"
    echo "   Password: voidlinux"

# Quick scan showing all devices (faster, less detailed)
pi-scan-quick:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "🔍 Quick network scan..."
    echo ""
    
    NETWORK=$(ip route | grep default | awk '{print $3}' | cut -d. -f1-3)
    
    if [ -z "$NETWORK" ]; then
        echo "❌ Could not detect network range"
        exit 1
    fi
    
    # Use arp-scan if available (faster)
    if command -v arp-scan &> /dev/null; then
        sudo arp-scan --localnet | grep -i "raspberry\|b8:27:eb\|dc:a6:32\|e4:5f:01" || {
            echo "No Raspberry Pi found"
            exit 1
        }
    else
        echo "Install arp-scan for faster scanning:"
        echo "  sudo pacman -S arp-scan"
        echo ""
        echo "Using nmap instead..."
        just pi-scan
    fi

# Scan and auto-connect to first found Pi
pi-connect:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "🔍 Finding Raspberry Pi..."
    
    NETWORK=$(ip route | grep default | awk '{print $3}' | cut -d. -f1-3)
    
    if ! command -v nmap &> /dev/null; then
        echo "❌ nmap not found. Install: sudo pacman -S nmap"
        exit 1
    fi
    
    # Scan and extract IP
    PI_IP=$(sudo nmap -sn $NETWORK.0/24 | grep -B 2 "Raspberry\|DC:A6:32\|B8:27:EB\|E4:5F:01" | grep "Nmap scan report" | head -1 | awk '{print $5}')
    
    if [ -z "$PI_IP" ]; then
        echo "❌ No Raspberry Pi found"
        exit 1
    fi
    
    echo "✅ Found Pi at: $PI_IP"
    echo ""
    echo "Connecting... (password: voidlinux)"
    echo ""
    
    ssh root@$PI_IP

# Check if specific IP is a Raspberry Pi
pi-check IP:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "🔍 Checking {{IP}}..."
    echo ""
    
    if ! command -v nmap &> /dev/null; then
        echo "❌ nmap not found. Install: sudo pacman -S nmap"
        exit 1
    fi
    
    # Check if host is up
    if ! ping -c 1 -W 1 {{IP}} &> /dev/null; then
        echo "❌ Host {{IP}} is not responding"
        exit 1
    fi
    
    echo "Host is up, checking details..."
    echo ""
    
    # Get MAC and manufacturer
    sudo nmap -sn {{IP}} | grep -A 1 "{{IP}}"
    
    echo ""
    echo "💡 To connect:"
    echo "   ssh root@{{IP}}"

# Show network scanning help
pi-scan-help:
    @echo "📡 Network Scanning Commands"
    @echo ""
    @echo "Find Raspberry Pi on your network:"
    @echo ""
    @echo "  just pi-scan           # Full scan (recommended, ~30s)"
    @echo "  just pi-scan-quick     # Quick scan (if arp-scan installed)"
    @echo "  just pi-connect        # Find and auto-connect"
    @echo "  just pi-check <IP>     # Check if specific IP is a Pi"
    @echo ""
    @echo "Installation:"
    @echo "  sudo pacman -S nmap         # Required"
    @echo "  sudo pacman -S arp-scan     # Optional (faster scanning)"
    @echo ""
    @echo "Troubleshooting:"
    @echo "  - Make sure Pi is powered on"
    @echo "  - Wait 60 seconds after power-on"
    @echo "  - Use ethernet (WiFi not configured yet)"
    @echo "  - Check router's DHCP leases"
    @echo ""
    @echo "Default credentials:"
    @echo "  Username: root"
    @echo "  Password: voidlinux"

# Monitor for Pi appearing on network
pi-wait:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "⏳ Waiting for Raspberry Pi to appear on network..."
    echo "   (Press Ctrl+C to stop)"
    echo ""
    
    NETWORK=$(ip route | grep default | awk '{print $3}' | cut -d. -f1-3)
    
    if ! command -v nmap &> /dev/null; then
        echo "❌ nmap not found. Install: sudo pacman -S nmap"
        exit 1
    fi
    
    COUNT=0
    while true; do
        COUNT=$((COUNT + 1))
        echo "Scan #$COUNT..."
        
        PI_IP=$(sudo nmap -sn $NETWORK.0/24 | grep -B 2 "Raspberry\|DC:A6:32\|B8:27:EB\|E4:5F:01" | grep "Nmap scan report" | head -1 | awk '{print $5}' || true)
        
        if [ -n "$PI_IP" ]; then
            echo ""
            echo "✅ Found Pi at: $PI_IP"
            echo ""
            echo "To connect:"
            echo "   ssh root@$PI_IP"
            echo "   Password: voidlinux"
            break
        fi
        
        sleep 5
    done

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

