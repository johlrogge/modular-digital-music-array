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

# If sysroot already contains pipewire headers, skip the whole setup (cache hit)
if [ -d "$SYSROOT/usr/include/pipewire-0.3" ]; then
  echo "Sysroot already present at $SYSROOT, skipping setup."
  exit 0
fi

echo "Setting up aarch64 cross-compilation sysroot..."

# ---------------------------------------------------------------------------
# Variant-aware PipeWire discovery with retries
# Void may serve glibc (.aarch64.xbps) or musl (.aarch64-musl.xbps) — accept both.
# Prefer glibc when both exist, fall back to musl.
# ---------------------------------------------------------------------------
echo "  Discovering current PipeWire version from Void Linux repo..."

INDEX_BODY=""
HTTP_STATUS=""
for attempt in 1 2 3; do
  HTTP_STATUS=$(curl -s -o /dev/null -w '%{http_code}' "$VOID_REPO/")
  if [ "$HTTP_STATUS" = "200" ]; then
    INDEX_BODY=$(curl -sL "$VOID_REPO/")
    break
  fi
  echo "  Attempt $attempt: HTTP $HTTP_STATUS from $VOID_REPO/ — retrying in $((attempt * 2))s..." >&2
  sleep $((attempt * 2))
done

if [ -z "$INDEX_BODY" ]; then
  echo "ERROR: Could not fetch package index from $VOID_REPO/ (HTTP $HTTP_STATUS)" >&2
  echo "Response preview:" >&2
  curl -sL "$VOID_REPO/" 2>&1 | head -c 400 >&2
  exit 1
fi

# Try glibc variant first, then musl
LIBPIPEWIRE_PKG=$(echo "$INDEX_BODY" | grep -oE 'libpipewire-[0-9][0-9._]+\.aarch64\.xbps' | sort -V | tail -1 || true)
VARIANT_SUFFIX=".aarch64"

if [ -z "$LIBPIPEWIRE_PKG" ]; then
  LIBPIPEWIRE_PKG=$(echo "$INDEX_BODY" | grep -oE 'libpipewire-[0-9][0-9._]+\.aarch64-musl\.xbps' | sort -V | tail -1 || true)
  VARIANT_SUFFIX=".aarch64-musl"
fi

if [ -z "$LIBPIPEWIRE_PKG" ]; then
  echo "ERROR: Could not discover libpipewire package from $VOID_REPO/" >&2
  echo "HTTP status: $HTTP_STATUS" >&2
  echo "Response preview (first 400 bytes):" >&2
  echo "$INDEX_BODY" | head -c 400 >&2
  exit 1
fi

# Extract the version suffix (e.g. "1.6.6_1") from the matched filename.
# Strip the leading "libpipewire-" and the trailing variant+extension.
PIPEWIRE_PKG_VER=$(echo "$LIBPIPEWIRE_PKG" \
  | sed "s/libpipewire-//;s/${VARIANT_SUFFIX//./\\.}\.xbps//")
if [ -z "$PIPEWIRE_PKG_VER" ]; then
  echo "ERROR: Could not extract version suffix from package name: $LIBPIPEWIRE_PKG" >&2
  exit 1
fi

LIBPIPEWIRE_PKG="libpipewire-${PIPEWIRE_PKG_VER}${VARIANT_SUFFIX}.xbps"
PIPEWIRE_DEVEL_PKG="pipewire-devel-${PIPEWIRE_PKG_VER}${VARIANT_SUFFIX}.xbps"

# Extract the version string (e.g. "1.4.9_1" → "1.4.9") for use in the .pc file
# Package name format: libpipewire-<version>_<rev>.<variant>.xbps
PIPEWIRE_VER=$(echo "$LIBPIPEWIRE_PKG" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)
if [ -z "$PIPEWIRE_VER" ]; then
  echo "ERROR: Could not parse PipeWire version from package name: $LIBPIPEWIRE_PKG" >&2
  exit 1
fi

echo "  Found: $LIBPIPEWIRE_PKG (variant: ${VARIANT_SUFFIX}, version: $PIPEWIRE_VER)"
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
