#!/usr/bin/env bash
# Create Void package for mdma-bandcamp service

set -euo pipefail

BINARY="target/aarch64-unknown-linux-gnu/release/mdma-bandcamp"
PACKAGE_DIR="build/package-bandcamp"
PACKAGES_DIR="build/packages"

echo "📦 Creating mdma-bandcamp Void package..."

# Verify binary exists
if [ ! -f "$BINARY" ]; then
    echo "❌ Binary not found at $BINARY"
    echo "   Run: just bandcamp-cross"
    exit 1
fi

# Clean and create package structure
rm -rf "$PACKAGE_DIR"
mkdir -p "$PACKAGE_DIR/usr/bin"
mkdir -p "$PACKAGE_DIR/etc/sv/mdma-bandcamp/log"
mkdir -p "$PACKAGE_DIR/etc/mdma"

# Copy binary
echo "  → Copying mdma-bandcamp binary..."
cp "$BINARY" "$PACKAGE_DIR/usr/bin/"
chmod +x "$PACKAGE_DIR/usr/bin/mdma-bandcamp"

# Copy runit service script from void-packages (single source of truth)
echo "  → Copying service script from void-packages..."
cp "void-packages/srcpkgs/mdma-bandcamp/files/mdma-bandcamp/run" "$PACKAGE_DIR/etc/sv/mdma-bandcamp/run"
chmod +x "$PACKAGE_DIR/etc/sv/mdma-bandcamp/run"

# Create runit log service
echo "  → Creating log service script..."
cat > "$PACKAGE_DIR/etc/sv/mdma-bandcamp/log/run" <<'LOGSCRIPT'
#!/bin/sh
exec svlogd -tt /var/log/mdma-bandcamp
LOGSCRIPT
chmod +x "$PACKAGE_DIR/etc/sv/mdma-bandcamp/log/run"

# Create INSTALL script
echo "  → Creating INSTALL script..."
cat > "$PACKAGE_DIR/INSTALL" <<'INSTALLSCRIPT'
#!/bin/sh
case "${ACTION}" in
post)
    # Create directories
    mkdir -p /var/log/mdma-bandcamp
    mkdir -p /run/mdma/sources /var/lib/mdma
    mkdir -p /music/downloads /music/inbox
    mkdir -p /etc/mdma

    # Create mdma user if doesn't exist
    if ! id mdma >/dev/null 2>&1; then
        useradd -r -s /sbin/nologin -d /music -c "MDMA Service User" mdma || true
    fi

    # Set ownership
    chown -R mdma:mdma /music /run/mdma /var/lib/mdma 2>/dev/null || true

    # Enable service
    if [ ! -d /var/service ]; then
        mkdir -p /var/service
    fi
    if [ ! -e /var/service/mdma-bandcamp ]; then
        ln -sf /etc/sv/mdma-bandcamp /var/service/mdma-bandcamp
        echo "mdma-bandcamp service enabled"
    fi

    # Restart if running
    if sv status mdma-bandcamp >/dev/null 2>&1; then
        sv restart mdma-bandcamp
        echo "mdma-bandcamp service restarted"
    fi
    ;;
esac
INSTALLSCRIPT
chmod +x "$PACKAGE_DIR/INSTALL"

# Get version from Cargo.toml (workspace version)
if [ -f "Cargo.toml" ]; then
    VERSION=$(grep '^version = ' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')
    echo "  📦 Version from workspace Cargo.toml: ${VERSION}"
else
    echo "  ❌ Error: Cargo.toml not found!"
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
    -n "mdma-bandcamp-${FULLVERSION}" \
    -s "MDMA Bandcamp music source" \
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
if [ ! -f "$PACKAGES_DIR/mdma-bandcamp-${FULLVERSION}.aarch64.xbps" ]; then
    echo "❌ Error: Package not created!"
    exit 1
fi

echo ""
echo "✅ Package created: $PACKAGES_DIR/mdma-bandcamp-${FULLVERSION}.aarch64.xbps"
ls -lh "$PACKAGES_DIR/mdma-bandcamp-${FULLVERSION}.aarch64.xbps"
