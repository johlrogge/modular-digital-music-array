#!/usr/bin/env bash
# Create Void package for beacon

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
mkdir -p "$PACKAGE_DIR/var/db/xbps/pkgdb"

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

# Get version info
if [ -f "void-packages/srcpkgs/beacon/template" ]; then
    VERSION=$(grep '^version=' void-packages/srcpkgs/beacon/template | cut -d= -f2)
    REVISION=$(grep '^revision=' void-packages/srcpkgs/beacon/template | cut -d= -f2)
else
    VERSION="0.1.0"
    REVISION="1"
    echo "  ⚠️  No template found, using default version ${VERSION}_${REVISION}"
fi

FULLVERSION="${VERSION}_${REVISION}"
SIZE=$(stat -c%s "$PACKAGE_DIR/usr/bin/beacon" 2>/dev/null || stat -f%z "$PACKAGE_DIR/usr/bin/beacon")

echo "  → Creating package metadata..."
cat > "$PACKAGE_DIR/var/db/xbps/pkgdb/beacon-${FULLVERSION}.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple Computer//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>pkgname</key>
  <string>beacon</string>
  <key>version</key>
  <string>${FULLVERSION}</string>
  <key>architecture</key>
  <string>aarch64</string>
  <key>installed_size</key>
  <integer>${SIZE}</integer>
</dict>
</plist>
PLIST

# Create xbps package
echo "  → Creating xbps package archive..."
mkdir -p "$PACKAGES_DIR"
cd "$PACKAGE_DIR"
tar -czf "../packages/beacon-${FULLVERSION}.aarch64.xbps" .
cd - > /dev/null

echo ""
echo "✅ Package created: $PACKAGES_DIR/beacon-${FULLVERSION}.aarch64.xbps"
ls -lh "$PACKAGES_DIR/beacon-${FULLVERSION}.aarch64.xbps"
