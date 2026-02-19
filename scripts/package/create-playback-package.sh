#!/usr/bin/env bash
# Create Void package for mdma-playback service

set -euo pipefail

BINARY="target/aarch64-unknown-linux-gnu/release/mdma-playback"
PACKAGE_DIR="build/package-playback"
PACKAGES_DIR="build/packages"

echo "📦 Creating mdma-playback Void package..."

# Verify binary exists
if [ ! -f "$BINARY" ]; then
    echo "❌ Binary not found at $BINARY"
    echo "   Run: just playback-cross"
    exit 1
fi

# Clean and create package structure
rm -rf "$PACKAGE_DIR"
mkdir -p "$PACKAGE_DIR/usr/bin"
mkdir -p "$PACKAGE_DIR/etc/sv/mdma-playback/log"

# Copy binary
echo "  → Copying mdma-playback binary..."
cp "$BINARY" "$PACKAGE_DIR/usr/bin/"
chmod +x "$PACKAGE_DIR/usr/bin/mdma-playback"

# Create runit service script
echo "  → Creating service script..."
cat > "$PACKAGE_DIR/etc/sv/mdma-playback/run" <<'RUNSCRIPT'
#!/bin/sh
exec 2>&1
# Create runtime directory if needed
mkdir -p /run/mdma

# PipeWire must be running for audio output.
# The mdma user needs access to the PipeWire socket.
# Set XDG_RUNTIME_DIR so PipeWire client can find the socket.
export XDG_RUNTIME_DIR="/run/user/$(id -u mdma)"

# Run as mdma user (must be in audio group)
if id mdma >/dev/null 2>&1; then
    exec chpst -u mdma /usr/bin/mdma-playback \
        --socket ipc:///run/mdma/playback.sock \
        --tcp tcp://0.0.0.0:5557
else
    exec /usr/bin/mdma-playback \
        --socket ipc:///run/mdma/playback.sock \
        --tcp tcp://0.0.0.0:5557
fi
RUNSCRIPT
chmod +x "$PACKAGE_DIR/etc/sv/mdma-playback/run"

# Create runit log service
echo "  → Creating log service script..."
cat > "$PACKAGE_DIR/etc/sv/mdma-playback/log/run" <<'LOGSCRIPT'
#!/bin/sh
exec svlogd -tt /var/log/mdma-playback
LOGSCRIPT
chmod +x "$PACKAGE_DIR/etc/sv/mdma-playback/log/run"

# Create INSTALL script
echo "  → Creating INSTALL script..."
cat > "$PACKAGE_DIR/INSTALL" <<'INSTALLSCRIPT'
#!/bin/sh
case "${ACTION}" in
post)
    # Create directories
    mkdir -p /var/log/mdma-playback
    mkdir -p /run/mdma

    # Create mdma user if doesn't exist
    if ! id mdma >/dev/null 2>&1; then
        useradd -r -s /sbin/nologin -d /music -c "MDMA Service User" mdma || true
    fi

    # Add mdma user to audio group for PipeWire access
    usermod -a -G audio mdma 2>/dev/null || true

    # Set ownership
    chown -R mdma:mdma /run/mdma 2>/dev/null || true

    # Enable service
    if [ ! -d /var/service ]; then
        mkdir -p /var/service
    fi
    if [ ! -e /var/service/mdma-playback ]; then
        ln -sf /etc/sv/mdma-playback /var/service/mdma-playback
        echo "mdma-playback service enabled"
    fi

    # Restart if running
    if sv status mdma-playback >/dev/null 2>&1; then
        sv restart mdma-playback
        echo "mdma-playback service restarted"
    fi
    ;;
esac
INSTALLSCRIPT
chmod +x "$PACKAGE_DIR/INSTALL"

# Get version from Cargo.toml
if [ -f "bases/mdma_playback/Cargo.toml" ]; then
    VERSION=$(grep '^version = ' bases/mdma_playback/Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')
    echo "  📦 Version from bases/mdma_playback/Cargo.toml: ${VERSION}"
else
    echo "  ❌ Error: bases/mdma_playback/Cargo.toml not found!"
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
    -n "mdma-playback-${FULLVERSION}" \
    -s "MDMA audio playback server" \
    -D "pipewire>=0 wireplumber>=0" \
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
if [ ! -f "$PACKAGES_DIR/mdma-playback-${FULLVERSION}.aarch64.xbps" ]; then
    echo "❌ Error: Package not created!"
    exit 1
fi

echo ""
echo "✅ Package created: $PACKAGES_DIR/mdma-playback-${FULLVERSION}.aarch64.xbps"
ls -lh "$PACKAGES_DIR/mdma-playback-${FULLVERSION}.aarch64.xbps"
