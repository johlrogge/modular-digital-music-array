#!/usr/bin/env bash
# Create Void package for mdma-library service

set -euo pipefail

BINARY="target/aarch64-unknown-linux-gnu/release/mdma-library"
PACKAGE_DIR="build/package-library"
PACKAGES_DIR="build/packages"

echo "📦 Creating mdma-library Void package..."

# Verify binary exists
if [ ! -f "$BINARY" ]; then
    echo "❌ Binary not found at $BINARY"
    echo "   Run: just library-cross"
    exit 1
fi

# Clean and create package structure
rm -rf "$PACKAGE_DIR"
mkdir -p "$PACKAGE_DIR/usr/bin"
mkdir -p "$PACKAGE_DIR/etc/sv/mdma-library/log"
mkdir -p "$PACKAGE_DIR/music/inbox"
mkdir -p "$PACKAGE_DIR/music/blobs"
mkdir -p "$PACKAGE_DIR/metadata"

# Copy binary
echo "  → Copying mdma-library binary..."
cp "$BINARY" "$PACKAGE_DIR/usr/bin/"
chmod +x "$PACKAGE_DIR/usr/bin/mdma-library"

# Create runit service script
echo "  → Creating service script..."
cat > "$PACKAGE_DIR/etc/sv/mdma-library/run" <<'RUNSCRIPT'
#!/bin/sh
exec 2>&1
# Create directories if needed
mkdir -p /music/inbox /music/blobs /metadata /run/mdma
# Run as mdma user if exists, otherwise root
if id mdma >/dev/null 2>&1; then
    exec chpst -u mdma /usr/bin/mdma-library \
        --music-dir /music \
        --metadata-dir /metadata \
        --socket ipc:///run/mdma/library.sock
else
    exec /usr/bin/mdma-library \
        --music-dir /music \
        --metadata-dir /metadata \
        --socket ipc:///run/mdma/library.sock
fi
RUNSCRIPT
chmod +x "$PACKAGE_DIR/etc/sv/mdma-library/run"

# Create runit log service
echo "  → Creating log service script..."
cat > "$PACKAGE_DIR/etc/sv/mdma-library/log/run" <<'LOGSCRIPT'
#!/bin/sh
exec svlogd -tt /var/log/mdma-library
LOGSCRIPT
chmod +x "$PACKAGE_DIR/etc/sv/mdma-library/log/run"

# Create INSTALL script
echo "  → Creating INSTALL script..."
cat > "$PACKAGE_DIR/INSTALL" <<'INSTALLSCRIPT'
#!/bin/sh
case "${ACTION}" in
post)
    # Create directories
    mkdir -p /var/log/mdma-library
    mkdir -p /music/inbox /music/blobs /metadata /run/mdma

    # Create mdma user if doesn't exist
    if ! id mdma >/dev/null 2>&1; then
        useradd -r -s /sbin/nologin -d /music -c "MDMA Service User" mdma || true
    fi

    # Set ownership
    chown -R mdma:mdma /music /metadata /run/mdma 2>/dev/null || true

    # Enable service
    if [ ! -d /var/service ]; then
        mkdir -p /var/service
    fi
    if [ ! -e /var/service/mdma-library ]; then
        ln -sf /etc/sv/mdma-library /var/service/mdma-library
        echo "mdma-library service enabled"
    fi

    # Restart if running
    if sv status mdma-library >/dev/null 2>&1; then
        sv restart mdma-library
        echo "mdma-library service restarted"
    fi
    ;;
esac
INSTALLSCRIPT
chmod +x "$PACKAGE_DIR/INSTALL"

# Get version from Cargo.toml
if [ -f "bases/mdma_library/Cargo.toml" ]; then
    VERSION=$(grep '^version = ' bases/mdma_library/Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')
    echo "  📦 Version from bases/mdma_library/Cargo.toml: ${VERSION}"
else
    echo "  ❌ Error: bases/mdma_library/Cargo.toml not found!"
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
    -n "mdma-library-${FULLVERSION}" \
    -s "MDMA music library service" \
    -H "https://github.com/johlrogge/modular-digital-music-array" \
    -l MIT \
    -m "Joakim Rohlén <joakim@roehlen.com>" \
    "$PACKAGE_DIR_ABS" 2>&1; then
    echo "  → xbps-create succeeded"
else
    XBPS_EXIT_CODE=$?
    echo "  ❌ xbps-create failed with exit code: $XBPS_EXIT_CODE"
    exit 1
fi

cd - > /dev/null

# Verify package was created
if [ ! -f "$PACKAGES_DIR/mdma-library-${FULLVERSION}.aarch64.xbps" ]; then
    echo "❌ Error: Package not created!"
    exit 1
fi

echo ""
echo "✅ Package created: $PACKAGES_DIR/mdma-library-${FULLVERSION}.aarch64.xbps"
ls -lh "$PACKAGES_DIR/mdma-library-${FULLVERSION}.aarch64.xbps"
