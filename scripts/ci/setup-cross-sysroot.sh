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

# Dynamically discover the current PipeWire version in the Void Linux repo
echo "  Discovering current PipeWire version from Void Linux repo..."
LIBPIPEWIRE_PKG=$(curl -sL "$VOID_REPO/" | grep -oE 'libpipewire-[0-9][0-9._]+\.aarch64\.xbps' | sort -V | tail -1)
if [ -z "$LIBPIPEWIRE_PKG" ]; then
  echo "ERROR: Could not discover libpipewire package from $VOID_REPO/" >&2
  exit 1
fi

# Extract the version suffix (e.g. "1.6.2_1") from the libpipewire package name
# and reuse it for pipewire-devel to avoid false positives from independent grepping
# (e.g. pipewire-devel-6.6.2_1 which is actually a Linux kernel package).
PIPEWIRE_PKG_VER=$(echo "$LIBPIPEWIRE_PKG" | grep -oE '[0-9][0-9._]+(?=\.aarch64\.xbps)')
if [ -z "$PIPEWIRE_PKG_VER" ]; then
  echo "ERROR: Could not extract version suffix from package name: $LIBPIPEWIRE_PKG" >&2
  exit 1
fi

LIBPIPEWIRE_PKG="libpipewire-${PIPEWIRE_PKG_VER}.aarch64.xbps"
PIPEWIRE_DEVEL_PKG="pipewire-devel-${PIPEWIRE_PKG_VER}.aarch64.xbps"

# Extract the version string (e.g. "1.4.9_1" → "1.4.9") for use in the .pc file
# Package name format: libpipewire-<version>_<rev>.aarch64.xbps
PIPEWIRE_VER=$(echo "$LIBPIPEWIRE_PKG" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)
if [ -z "$PIPEWIRE_VER" ]; then
  echo "ERROR: Could not parse PipeWire version from package name: $LIBPIPEWIRE_PKG" >&2
  exit 1
fi

echo "  Found: $LIBPIPEWIRE_PKG (version $PIPEWIRE_VER)"
echo "  Found: $PIPEWIRE_DEVEL_PKG"

# Download packages
echo "  Downloading aarch64 PipeWire packages from Void Linux..."
curl -fsSL "$VOID_REPO/$LIBPIPEWIRE_PKG" -o "$TMPDIR/libpipewire.xbps"
curl -fsSL "$VOID_REPO/$PIPEWIRE_DEVEL_PKG" -o "$TMPDIR/pipewire-devel.xbps"

# Extract
echo "  Extracting..."
mkdir -p "$TMPDIR/extract"
cd "$TMPDIR/extract"
tar xf "$TMPDIR/libpipewire.xbps"
tar xf "$TMPDIR/pipewire-devel.xbps"

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
Version: $PIPEWIRE_VER
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
echo "  PipeWire version: $PIPEWIRE_VER"
echo "  Libraries: $(ls "$SYSROOT/usr/lib/"*.so* | wc -l) files"
echo "  Headers:   pipewire-0.3, spa-0.2"
echo ""
echo "To cross-compile: just playback-cross"
