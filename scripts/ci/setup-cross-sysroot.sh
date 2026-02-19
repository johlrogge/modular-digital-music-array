#!/usr/bin/env bash
# Download aarch64 PipeWire libraries from Void Linux for cross-compilation
#
# Creates .cross/aarch64-sysroot/ with headers and shared libraries needed
# to cross-compile mdma-playback from x86_64.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
SYSROOT="$PROJECT_ROOT/.cross/aarch64-sysroot"
VOID_REPO="https://repo-default.voidlinux.org/current/aarch64"
TMPDIR=$(mktemp -d)

trap 'rm -rf "$TMPDIR"' EXIT

echo "Setting up aarch64 cross-compilation sysroot..."

# Download packages
echo "  Downloading aarch64 PipeWire packages from Void Linux..."
curl -sL "$VOID_REPO/libpipewire-1.4.9_1.aarch64.xbps" -o "$TMPDIR/libpipewire.xbps"
curl -sL "$VOID_REPO/pipewire-devel-1.4.9_1.aarch64.xbps" -o "$TMPDIR/pipewire-devel.xbps"

# Extract
echo "  Extracting..."
mkdir -p "$TMPDIR/extract"
cd "$TMPDIR/extract"
tar xf "$TMPDIR/libpipewire.xbps" 2>/dev/null
tar xf "$TMPDIR/pipewire-devel.xbps" 2>/dev/null

# Create sysroot structure
echo "  Creating sysroot at $SYSROOT..."
rm -rf "$SYSROOT"
mkdir -p "$SYSROOT/usr/lib/pkgconfig" "$SYSROOT/usr/include"

# Copy libraries and headers
cp -a "$TMPDIR/extract/usr/lib/libpipewire-0.3.so"* "$SYSROOT/usr/lib/"
cp -a "$TMPDIR/extract/usr/include/pipewire-0.3" "$SYSROOT/usr/include/"
cp -a "$TMPDIR/extract/usr/include/spa-0.2" "$SYSROOT/usr/include/"

# Create pkg-config files with absolute paths to sysroot
cat > "$SYSROOT/usr/lib/pkgconfig/libpipewire-0.3.pc" <<EOF
prefix=$SYSROOT/usr
includedir=\${prefix}/include
libdir=\${prefix}/lib

moduledir=\${libdir}/pipewire-0.3

Name: libpipewire
Description: PipeWire Interface
Version: 1.4.9
Requires: libspa-0.2
Libs: -L\${libdir} -lpipewire-0.3
Cflags: -I\${includedir}/pipewire-0.3 -D_REENTRANT
EOF

cat > "$SYSROOT/usr/lib/pkgconfig/libspa-0.2.pc" <<EOF
prefix=$SYSROOT/usr
includedir=\${prefix}/include
libdir=\${prefix}/lib

plugindir=\${libdir}/spa-0.2

Name: libspa
Description: Simple Plugin API
Version: 0.2
Cflags: -I\${includedir}/spa-0.2 -D_REENTRANT
EOF

echo ""
echo "Sysroot ready at: $SYSROOT"
echo "  Libraries: $(ls "$SYSROOT/usr/lib/"*.so* | wc -l) files"
echo "  Headers:   pipewire-0.3, spa-0.2"
echo ""
echo "To cross-compile: just playback-cross"
