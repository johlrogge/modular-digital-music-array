#!/usr/bin/env bash
# Build all MDMA binaries for ARM64 (Raspberry Pi 5)
#
# All projects are root workspace members (polylith workspace migration).
# --manifest-path projects/*/Cargo.toml resolves to the root workspace automatically.
#
# Phase 1:  mdma-library
# Phase 1b: mdma-gateway
# Phase 1c: mdma-bandcamp
# Phase 1d: beacon
# Phase 1e: mdma-console
# Phase 1f: mdma-acid via production profile (file-backed fact-store, not memory)
# Phase 2:  mdma-playback with PipeWire sysroot env vars (separate target suffix)
#
# Requires aarch64 PipeWire sysroot at .cross/aarch64-sysroot/ for Phase 2
# (see scripts/ci/setup-cross-sysroot.sh to create it)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
SYSROOT="$PROJECT_ROOT/.cross/aarch64-sysroot"

if [ ! -d "$SYSROOT/usr/lib" ]; then
    echo "Error: aarch64 sysroot not found at $SYSROOT"
    echo "Run: scripts/ci/setup-cross-sysroot.sh"
    exit 1
fi

# Nix-provided zig bakes in a build-time cache path that doesn't exist at runtime
export ZIG_GLOBAL_CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/zig"
mkdir -p "$ZIG_GLOBAL_CACHE_DIR"

echo "=== Phase 1: Building mdma-library ==="
cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.38 \
    --manifest-path "$PROJECT_ROOT/projects/mdma-library/Cargo.toml" \
    --bin mdma-library

echo ""
echo "=== Phase 1b: Building gateway ==="
cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.38 \
    --manifest-path "$PROJECT_ROOT/projects/mdma-gateway/Cargo.toml" \
    --bin mdma-gateway

echo ""
echo "=== Phase 1c: Building bandcamp ==="
cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.38 \
    --manifest-path "$PROJECT_ROOT/projects/mdma-bandcamp/Cargo.toml" \
    --bin mdma-bandcamp

echo ""
echo "=== Phase 1d: Building beacon ==="
cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.38 \
    --manifest-path "$PROJECT_ROOT/projects/mdma-beacon/Cargo.toml" \
    --bin beacon

echo ""
echo "=== Phase 1e: Building console ==="
cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.38 \
    --manifest-path "$PROJECT_ROOT/projects/mdma-console/Cargo.toml" \
    --bin mdma-console

echo ""
echo "=== Phase 1f: Building acid (production profile, file-backed fact-store) ==="
cargo polylith profile build production --no-build
cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.38 \
    --manifest-path "$PROJECT_ROOT/profiles/production/Cargo.toml" \
    --bin mdma-acid

echo ""
echo "=== Phase 2: Building playback (with PipeWire sysroot) ==="
echo "  Sysroot: $SYSROOT"

# Cross-compilation environment for PipeWire
export PKG_CONFIG_PATH_aarch64_unknown_linux_gnu="$SYSROOT/usr/lib/pkgconfig"
export PKG_CONFIG_ALLOW_CROSS=1
export BINDGEN_EXTRA_CLANG_ARGS_aarch64_unknown_linux_gnu="--sysroot=$SYSROOT -I$SYSROOT/usr/include/pipewire-0.3 -I$SYSROOT/usr/include/spa-0.2"

# Target glibc 2.38 to match Void Linux's PipeWire build
cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.38 \
    --manifest-path "$PROJECT_ROOT/projects/mdma-playback/Cargo.toml" \
    --bin mdma-playback

cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.38 \
    --manifest-path "$PROJECT_ROOT/projects/mdma-audio/Cargo.toml" \
    --bin mdma-audio

echo ""
echo "=== All binaries built ==="
for bin in beacon mdma-library mdma-console mdma-gateway mdma-bandcamp mdma-acid mdma-playback mdma-audio; do
    file "$PROJECT_ROOT/target/aarch64-unknown-linux-gnu/release/$bin"
    ls -lh "$PROJECT_ROOT/target/aarch64-unknown-linux-gnu/release/$bin"
done
