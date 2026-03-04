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

# Run BDD tests
[group('test')]
bdd:
    cargo test --package mdma-bdd --test cucumber -- -vv

# Cross-compile a standard MDMA binary for aarch64
[group('build')]
cross bin label=bin:
    #!/usr/bin/env bash
    set -euo pipefail
    export ZIG_GLOBAL_CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/zig"
    mkdir -p "$ZIG_GLOBAL_CACHE_DIR"
    echo "Building {{label}} for aarch64..."
    cargo zigbuild --release --target aarch64-unknown-linux-gnu --bin {{bin}}
    echo ""
    echo "{{label}} built!"
    file target/aarch64-unknown-linux-gnu/release/{{bin}}
    ls -lh target/aarch64-unknown-linux-gnu/release/{{bin}}

# Quick cross-compile beacon using cargo-zigbuild (devenv provides zig + target)
[group('build')]
beacon-cross: (cross "beacon" "Beacon")

# Build beacon with native cargo (requires system cross-compiler)
[group('build')]
beacon-native:
    cargo build --release --target aarch64-unknown-linux-gnu --bin beacon
    @echo ""
    @file target/aarch64-unknown-linux-gnu/release/beacon
    @ls -lh target/aarch64-unknown-linux-gnu/release/beacon

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
    if command -v cargo-zigbuild &> /dev/null; then
        echo "cargo-zigbuild available (recommended)"
        echo "   Use: just beacon-cross"
    else
        echo "cargo-zigbuild not found"
        echo "   Restart devenv shell or add cargo-zigbuild to devenv.nix"
    fi
    echo ""
    if command -v zig &> /dev/null; then
        echo "zig available: $(zig version)"
    else
        echo "zig not found"
        echo "   Restart devenv shell or add zig to devenv.nix"
    fi
    echo ""
    if command -v aarch64-linux-gnu-gcc &> /dev/null; then
        echo "aarch64-linux-gnu-gcc available (alternative)"
        echo "   Use: just beacon-native"
    else
        echo "aarch64-linux-gnu-gcc not found (optional)"
    fi
    echo ""
    echo "Rust target:"
    if rustup target list 2>/dev/null | grep -q "aarch64-unknown-linux-gnu"; then
        echo "aarch64-unknown-linux-gnu target installed"
    elif rustc --print target-list | grep -q "aarch64-unknown-linux-gnu"; then
        echo "aarch64-unknown-linux-gnu target available"
    else
        echo "aarch64-unknown-linux-gnu target not available"
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

# Rapid deploy beacon to Pi for development iteration
# Set PI_PASSWORD env var or it defaults to 'voidlinux'
[group('dev')]
deploy-dev: beacon-cross
    #!/usr/bin/env bash
    set -euo pipefail

    HOST="welcome-to-mdma.local"
    BEACON="target/aarch64-unknown-linux-gnu/release/beacon"
    PASS="${PI_PASSWORD:-voidlinux}"

    # Helper function for SSH commands
    run_ssh() {
        if ssh -4 -o BatchMode=yes -o ConnectTimeout=5 "root@${HOST}" true 2>/dev/null; then
            ssh -4 "root@${HOST}" "$@"
        else
            sshpass -p "$PASS" ssh -4 -o StrictHostKeyChecking=no "root@${HOST}" "$@"
        fi
    }

    # Helper function for SCP
    run_scp() {
        if ssh -4 -o BatchMode=yes -o ConnectTimeout=5 "root@${HOST}" true 2>/dev/null; then
            scp -4 "$@"
        else
            sshpass -p "$PASS" scp -4 -o StrictHostKeyChecking=no "$@"
        fi
    }

    echo "Deploying beacon to $HOST..."

    # Copy binary to Pi
    run_scp "$BEACON" "root@${HOST}:/tmp/"

    # Stop service, move binary, ensure logging is set up, restart
    run_ssh 'sv stop beacon 2>/dev/null || true; mv /tmp/beacon /usr/bin/; mkdir -p /var/log/beacon /etc/sv/beacon/log; if [ ! -f /etc/sv/beacon/log/run ]; then echo "#!/bin/sh" > /etc/sv/beacon/log/run; echo "exec svlogd -tt /var/log/beacon" >> /etc/sv/beacon/log/run; chmod +x /etc/sv/beacon/log/run; rm -f /var/service/beacon; sleep 1; ln -sf /etc/sv/beacon /var/service/beacon; sleep 2; else sv start beacon 2>/dev/null || true; fi'

    echo "Beacon deployed and restarted!"
    echo ""
    echo "Tailing logs (Ctrl+C to stop)..."
    run_ssh 'tail -n 30 -f /var/log/beacon/current'

# ============================================================================
# CI/CD Build Recipes (Work Locally and in GitHub Actions)
# ============================================================================

# Build a single MDMA binary for CI
[group('ci')]
ci-build bin:
    ./scripts/ci/build-binary.sh {{bin}}

# Build beacon for CI/CD (local or GitHub Actions)
[group('ci')]
ci-build-beacon: (ci-build "beacon")

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
ci-pipeline: ci-build-beacon ci-package-beacon
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

# Build all binaries for CI in minimal cargo zigbuild invocations (Phase 1: 5 simple, Phase 2: playback)
[group('ci')]
ci-build-all: setup-playback-sysroot
    ./scripts/ci/build-all.sh

# Build beacon Void package
[group('package')]
pkg-beacon:
    ./scripts/package/create-package.sh

# Build mdma-library for CI
[group('ci')]
ci-build-library: (ci-build "mdma-library")

# Build mdma-console for CI
[group('ci')]
ci-build-console: (ci-build "mdma-console")

# Build mdma-playback for CI
[group('ci')]
ci-build-playback:
    ./scripts/ci/build-playback.sh

# Build mdma-gateway for CI
[group('ci')]
ci-build-gateway: (ci-build "mdma-gateway")

# Build mdma-bandcamp for CI
[group('ci')]
ci-build-bandcamp: (ci-build "mdma-bandcamp")

# Build mdma-library Void package
[group('package')]
pkg-library:
    ./scripts/package/create-library-package.sh

# Build mdma-console Void package
[group('package')]
pkg-console:
    ./scripts/package/create-console-package.sh

# Build mdma-playback Void package
[group('package')]
pkg-playback:
    ./scripts/package/create-playback-package.sh

# Build mdma-gateway Void package
[group('package')]
pkg-gateway:
    ./scripts/package/create-gateway-package.sh

# Build mdma-bandcamp Void package
[group('package')]
pkg-bandcamp:
    ./scripts/package/create-bandcamp-package.sh

# Create repository structure and index (all packages)
[group('package')]
pkg-repository: ci-build-all pkg-beacon pkg-library pkg-console pkg-playback pkg-gateway pkg-bandcamp
    ./scripts/package/create-repository.sh

# Full package build pipeline (what CI runs!)
[group('package')]
pkg-build-all: check-prereqs pkg-repository
    @echo ""
    @echo "🎉 Package build complete!"
    @echo ""
    @echo "Repository ready at: build/repository/"
    @echo "Packages: beacon, mdma-library, mdma-console, mdma-playback, mdma-gateway, mdma-bandcamp"
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

# ============================================================================
# Image Creation
# ============================================================================

# Validate sudo credentials early (before long builds)
[group('image')]
confirm-sudo:
    @echo "Image creation requires root. Requesting sudo now..."
    @sudo -v

# Create SD card image with beacon installed via xbps
[group('image')]
create-image: confirm-sudo pkg-build-all
    sudo ./scripts/image/create-sd-card-simple.sh

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

# SSH into the provisioned Pi (mdma-909.local)
pi-ssh:
    ssh -4 -i ~/.ssh/mdma_pi admin@mdma-909.local

# SSH into the unprovisioned beacon Pi (welcome-to-mdma.local)
pi-ssh-beacon:
    ssh -4 root@welcome-to-mdma.local

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


# run beacon
[group("run")]
run-beacon:
    cargo run --bin beacon

# run console locally
[group("run")]
run-console:
    cargo run --bin mdma-console

# Cross-compile console for aarch64
[group('build')]
console-cross: (cross "mdma-console" "Console")

# Set up aarch64 PipeWire sysroot for cross-compiling playback
[group('build')]
setup-playback-sysroot:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -d ".cross/aarch64-sysroot/usr/lib" ]; then
        echo "Sysroot already exists at .cross/aarch64-sysroot/"
        echo "  To recreate, run: rm -rf .cross/aarch64-sysroot && just setup-playback-sysroot"
    else
        ./scripts/ci/setup-cross-sysroot.sh
    fi

# Cross-compile playback server for aarch64
[group('build')]
playback-cross: setup-playback-sysroot
    ./scripts/ci/build-playback.sh

# Cross-compile library service for aarch64
[group('build')]
library-cross: (cross "mdma-library" "Library")

# Internal: deploy a standard MDMA service to the provisioned Pi
[group('dev')]
_deploy-svc svc bin *extra_scp:
    #!/usr/bin/env bash
    set -euo pipefail
    HOST="${PI_HOST:-mdma-909.local}"
    SSH_KEY="$HOME/.ssh/mdma_pi"
    BINARY="target/aarch64-unknown-linux-gnu/release/{{bin}}"
    RUN_SCRIPT="void-packages/srcpkgs/{{svc}}/files/{{svc}}/run"
    POST_INSTALL="scripts/deploy/post-install-{{svc}}.sh"

    echo "Deploying {{svc}} to $HOST..."

    # Upload binary, run script, and any extra files
    scp -4 -i "$SSH_KEY" "$BINARY" "$RUN_SCRIPT" {{extra_scp}} "admin@${HOST}:/tmp/"

    ssh -4 -i "$SSH_KEY" "admin@${HOST}" 'sudo sv stop {{svc}} 2>/dev/null || true
        sudo mv /tmp/{{bin}} /usr/bin/
        sudo chmod +x /usr/bin/{{bin}}
        sudo mkdir -p /etc/sv/{{svc}}/log /var/log/{{svc}} /run/mdma
        sudo cp /tmp/run /etc/sv/{{svc}}/run
        sudo chmod +x /etc/sv/{{svc}}/run
        printf "#!/bin/sh\nexec svlogd -tt /var/log/{{svc}}\n" | sudo tee /etc/sv/{{svc}}/log/run > /dev/null
        sudo chmod +x /etc/sv/{{svc}}/log/run
        sudo ln -sf /etc/sv/{{svc}} /var/service/{{svc}} 2>/dev/null || true
        for i in 1 2 3 4 5; do sleep 1; [ -d /var/service/{{svc}}/supervise ] && break; done
        sudo sv start {{svc}} 2>/dev/null || true
        sleep 1
        sudo sv status {{svc}} 2>/dev/null || echo "{{svc}}: waiting for runit (check manually)"'

    # Run post-install hook if it exists
    if [ -f "$POST_INSTALL" ]; then
        echo "Running post-install for {{svc}}..."
        ssh -4 -i "$SSH_KEY" "admin@${HOST}" 'bash -s' < "$POST_INSTALL"
    fi

    echo "{{svc}} deployed!"

# Deploy console to Pi
[group('dev')]
deploy-console: console-cross
    @just _deploy-svc mdma-console mdma-console

# Deploy library to Pi
[group('dev')]
deploy-library: library-cross
    @just _deploy-svc mdma-library mdma-library

# Deploy playback to Pi
[group('dev')]
deploy-playback: playback-cross
    @just _deploy-svc mdma-playback mdma-playback

# Cross-compile gateway for aarch64
[group('build')]
gateway-cross: (cross "mdma-gateway" "Gateway")

# Cross-compile acid service for aarch64
[group('build')]
acid-cross: (cross "mdma-acid" "Acid")

# Cross-compile bandcamp for aarch64
[group('build')]
bandcamp-cross: (cross "mdma-bandcamp" "Bandcamp")

# Deploy gateway to Pi (single external TCP port)
[group('dev')]
deploy-gateway: gateway-cross
    @just _deploy-svc mdma-gateway mdma-gateway

# Cross-compile CLI (mdma) for aarch64
[group('build')]
cli-cross: (cross "mdma" "CLI")

# Deploy CLI (mdma) to Pi - installs as /usr/bin/mdma, no service required
[group('dev')]
deploy-cli: cli-cross
    #!/usr/bin/env bash
    set -euo pipefail

    HOST="${PI_HOST:-mdma-909.local}"
    BINARY="target/aarch64-unknown-linux-gnu/release/mdma"
    SSH_KEY="$HOME/.ssh/mdma_pi"

    echo "Deploying mdma CLI to $HOST..."

    scp -4 -i "$SSH_KEY" "$BINARY" "admin@${HOST}:/tmp/"

    ssh -4 -i "$SSH_KEY" "admin@${HOST}" 'sudo mv /tmp/mdma /usr/bin/mdma
        sudo chmod +x /usr/bin/mdma
        mdma --version'

    echo ""
    echo "CLI deployed! Run: mdma --help"

# Deploy bandcamp service to Pi (includes conf file)
[group('dev')]
deploy-bandcamp: bandcamp-cross
    @just _deploy-svc mdma-bandcamp mdma-bandcamp "void-packages/srcpkgs/mdma-bandcamp/files/mdma-bandcamp/conf"

# Deploy acid service to Pi
[group('dev')]
deploy-acid: acid-cross
    @just _deploy-svc mdma-acid mdma-acid
