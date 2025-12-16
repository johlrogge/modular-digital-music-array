#!/usr/bin/env bash
# Create Void package for beacon with proper xbps metadata

set -euo pipefail

BEACON="target/aarch64-unknown-linux-gnu/release/beacon"
PACKAGE_DIR="build/package"
PACKAGES_DIR="build/packages"

echo "📦 Creating beacon Void package..."

# Verify beacon binary exists
if [ ! -f "$BEACON" ]; then
    echo "❌ Beacon binary not found at $BEACON"
    echo "   Run: just ci-build-beacon"
    exit 1
fi

# Clean and create package structure
rm -rf "$PACKAGE_DIR"
mkdir -p "$PACKAGE_DIR/usr/bin"
mkdir -p "$PACKAGE_DIR/etc/sv/beacon"

echo "  → Cleaning any leftover metadata files..."
# Remove any leftover plist files from old manual packaging approach
rm -f "$PACKAGE_DIR/props.plist" "$PACKAGE_DIR/files.plist"

# Copy beacon binary
echo "  → Copying beacon binary..."
cp "$BEACON" "$PACKAGE_DIR/usr/bin/"
chmod +x "$PACKAGE_DIR/usr/bin/beacon"

# Copy or create runit service
if [ -f "void-packages/srcpkgs/beacon/files/beacon/run" ]; then
    echo "  → Using service script from void-packages/..."
    cp void-packages/srcpkgs/beacon/files/beacon/run "$PACKAGE_DIR/etc/sv/beacon/"
else
    echo "  → Creating default service script..."
    cat > "$PACKAGE_DIR/etc/sv/beacon/run" <<'RUNSCRIPT'
#!/bin/sh
exec 2>&1
exec chpst -u root /usr/bin/beacon --apply --port 80
RUNSCRIPT
fi
chmod +x "$PACKAGE_DIR/etc/sv/beacon/run"

# Create supervise symlink (required by runit)
echo "  → Creating supervise symlink..."
ln -sf /run/runit/supervise.beacon "$PACKAGE_DIR/etc/sv/beacon/supervise"

# Create INSTALL script for proper service management (the Void way!)
echo "  → Creating INSTALL script..."
cat > "$PACKAGE_DIR/INSTALL" <<'INSTALLSCRIPT'
#!/bin/sh
# INSTALL script for beacon package
# Handles service enablement the Void Linux way

case "${ACTION}" in
post)
    # Enable beacon service by creating symlink to /var/service
    # This is the standard Void Linux method for enabling runit services
    if [ ! -d /var/service ]; then
        mkdir -p /var/service
    fi
    
    # Enable beacon service
    if [ ! -e /var/service/beacon ]; then
        ln -sf /etc/sv/beacon /var/service/beacon
        echo "beacon service enabled (will start on next boot)"
    fi
    ;;
esac
INSTALLSCRIPT
chmod +x "$PACKAGE_DIR/INSTALL"

# Get version from beacon's Cargo.toml (single source of truth!)
if [ -f "bases/beacon/Cargo.toml" ]; then
    VERSION=$(grep '^version = ' bases/beacon/Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')
    echo "  📦 Version from bases/beacon/Cargo.toml: ${VERSION}"
else
    echo "  ❌ Error: bases/beacon/Cargo.toml not found!"
    exit 1
fi

# Revision defaults to 1 (increment only for package-only changes)
REVISION="1"

FULLVERSION="${VERSION}_${REVISION}"

# xbps-create will handle all metadata automatically
echo "  → Package version: ${FULLVERSION}"

# Create xbps package using xbps-create (proper way!)
echo "  → Creating xbps package with xbps-create..."
mkdir -p "$PACKAGES_DIR"

# Get absolute paths to avoid any confusion
PACKAGE_DIR_ABS=$(realpath "$PACKAGE_DIR")
PACKAGES_DIR_ABS=$(realpath "$PACKAGES_DIR")

echo "  → Verifying source directory..."
echo "  → Package directory: $PACKAGE_DIR_ABS"
if [ ! -d "$PACKAGE_DIR_ABS" ]; then
    echo "  ❌ Package directory doesn't exist!"
    exit 1
fi

echo "  → Directory contents:"
ls -lR "$PACKAGE_DIR_ABS" | head -30

# Use xbps-create to build the package properly
# xbps-create outputs to current directory, so cd there first
cd "$PACKAGES_DIR_ABS"

echo "  → Running xbps-create in: $(pwd)"
echo "  → Source directory: $PACKAGE_DIR_ABS"

# Run xbps-create with CORRECT syntax
# Required: -A (arch), -n (pkgver), -s (desc)
if XBPS_TARGET_ARCH=aarch64 xbps-create \
    -A aarch64 \
    -n "beacon-${FULLVERSION}" \
    -s "MDMA provisioning beacon" \
    -H "https://github.com/johlrogge/modular-digital-music-array" \
    -l MIT \
    -m "Joakim Rohlén <joakim@roehlen.com>" \
    -D "avahi>=0" \
    -D "dbus>=0" \
    "$PACKAGE_DIR_ABS" 2>&1; then
    echo "  → xbps-create succeeded"
else
    XBPS_EXIT_CODE=$?
    echo "  ❌ xbps-create failed with exit code: $XBPS_EXIT_CODE"
    echo "  → Directory contents:"
    ls -la
    exit 1
fi

echo "  → Package creation complete"
echo "  → Files in $(pwd):"
ls -la

# Return to original directory
cd - > /dev/null

# Verify package was created
if [ ! -f "$PACKAGES_DIR/beacon-${FULLVERSION}.aarch64.xbps" ]; then
    echo "❌ Error: Package not created!"
    echo "   Expected: $PACKAGES_DIR/beacon-${FULLVERSION}.aarch64.xbps"
    echo "   Directory contents:"
    ls -la "$PACKAGES_DIR/"
    exit 1
fi

echo ""
echo "✅ Package created: $PACKAGES_DIR/beacon-${FULLVERSION}.aarch64.xbps"
ls -lh "$PACKAGES_DIR/beacon-${FULLVERSION}.aarch64.xbps"
