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
      --exclude='*/benches/test_data/*' \
      --exclude='*/tests/test_data/*' \
      --exclude='*.flac' \
      --exclude='*.wav' \
      --exclude='*.mp3' \
      --exclude='*.jsonl' \
      -czf "/tmp/${ARCHIVE_NAME}" .
    mv "/tmp/${ARCHIVE_NAME}" .
    echo "✅ Created: ${ARCHIVE_NAME}"
    ls -lh "${ARCHIVE_NAME}"

# ============================================================================
# Image Creation
# ============================================================================

# Create SD card image with beacon installed via xbps
[group('image')]
create-image: check-prereqs pkg-build-all
    ./scripts/image/create-image-with-xbps.sh
