#!/usr/bin/env bash
# Create Void package for mdma-console service

set -euo pipefail

BINARY="target/aarch64-unknown-linux-gnu/release/mdma-console"
PACKAGE_DIR="build/package-console"
PACKAGES_DIR="build/packages"

echo "📦 Creating mdma-console Void package..."

# Verify binary exists
if [ ! -f "$BINARY" ]; then
    echo "❌ Binary not found at $BINARY"
    echo "   Run: just console-cross"
    exit 1
fi

# Clean and create package structure
rm -rf "$PACKAGE_DIR"
mkdir -p "$PACKAGE_DIR/usr/bin"
mkdir -p "$PACKAGE_DIR/etc/sv/mdma-console/log"

# Copy binary
echo "  → Copying mdma-console binary..."
cp "$BINARY" "$PACKAGE_DIR/usr/bin/"
chmod +x "$PACKAGE_DIR/usr/bin/mdma-console"

# Copy runit service script from void-packages (single source of truth)
echo "  → Copying service script from void-packages..."
cp "void-packages/srcpkgs/mdma-console/files/mdma-console/run" "$PACKAGE_DIR/etc/sv/mdma-console/run"
chmod +x "$PACKAGE_DIR/etc/sv/mdma-console/run"

# Create runit log service
echo "  → Creating log service script..."
cat > "$PACKAGE_DIR/etc/sv/mdma-console/log/run" <<'LOGSCRIPT'
#!/bin/sh
exec svlogd -tt /var/log/mdma-console
LOGSCRIPT
chmod +x "$PACKAGE_DIR/etc/sv/mdma-console/log/run"

# Create INSTALL script
echo "  → Creating INSTALL script..."
cat > "$PACKAGE_DIR/INSTALL" <<'INSTALLSCRIPT'
#!/bin/sh
case "${ACTION}" in
post)
    # Create log directory
    mkdir -p /var/log/mdma-console

    # Allow binding to privileged ports without root
    if command -v setcap >/dev/null 2>&1; then
        setcap 'cap_net_bind_service=+ep' /usr/bin/mdma-console
        echo "mdma-console: granted CAP_NET_BIND_SERVICE capability"
    else
        echo "WARNING: setcap not found, mdma-console may not bind to port 80"
    fi

    # Enable service
    if [ ! -d /var/service ]; then
        mkdir -p /var/service
    fi
    if [ ! -e /var/service/mdma-console ]; then
        ln -sf /etc/sv/mdma-console /var/service/mdma-console
        echo "mdma-console service enabled"
    fi

    # Restart if running
    if sv status mdma-console >/dev/null 2>&1; then
        sv restart mdma-console
        echo "mdma-console service restarted"
    fi
    ;;
esac
INSTALLSCRIPT
chmod +x "$PACKAGE_DIR/INSTALL"

# Get version from Cargo.toml
if [ -f "bases/mdma_console/Cargo.toml" ]; then
    VERSION=$(grep '^version = ' bases/mdma_console/Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')
    echo "  📦 Version from bases/mdma_console/Cargo.toml: ${VERSION}"
else
    echo "  ❌ Error: bases/mdma_console/Cargo.toml not found!"
    exit 1
fi

REVISION="1"
FULLVERSION="${VERSION}_${REVISION}"

echo "  → Package version: ${FULLVERSION}"

# Create xbps package
echo "  → Creating xbps package with xbps-create..."
mkdir -p "$PACKAGES_DIR"

PACKAGE_DIR_ABS=$(realpath "$PACKAGE_DIR")
PACKAGES_DIR_ABS=$(realpath "$PACKAGES_DIR")

cd "$PACKAGES_DIR_ABS"

if XBPS_TARGET_ARCH=aarch64 xbps-create \
    -A aarch64 \
    -n "mdma-console-${FULLVERSION}" \
    -s "MDMA web console" \
    -H "https://github.com/johlrogge/modular-digital-music-array" \
    -l MIT \
    -m "Joakim Ohlrogge <joakim.ohlrogge@agical.se>" \
    "$PACKAGE_DIR_ABS" 2>&1; then
    echo "  → xbps-create succeeded"
else
    XBPS_EXIT_CODE=$?
    echo "  ❌ xbps-create failed with exit code: $XBPS_EXIT_CODE"
    exit 1
fi

cd - > /dev/null

# Verify package was created
if [ ! -f "$PACKAGES_DIR/mdma-console-${FULLVERSION}.aarch64.xbps" ]; then
    echo "❌ Error: Package not created!"
    exit 1
fi

echo ""
echo "✅ Package created: $PACKAGES_DIR/mdma-console-${FULLVERSION}.aarch64.xbps"
ls -lh "$PACKAGES_DIR/mdma-console-${FULLVERSION}.aarch64.xbps"
