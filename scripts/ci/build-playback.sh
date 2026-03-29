#!/usr/bin/env bash
# Build mdma-playback for ARM64 (Raspberry Pi 5)
#
# Requires aarch64 PipeWire sysroot at .cross/aarch64-sysroot/
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

echo "Building mdma-playback for aarch64..."
echo "  Sysroot: $SYSROOT"

# Cross-compilation environment for PipeWire
export PKG_CONFIG_PATH_aarch64_unknown_linux_gnu="$SYSROOT/usr/lib/pkgconfig"
export PKG_CONFIG_ALLOW_CROSS=1
export BINDGEN_EXTRA_CLANG_ARGS_aarch64_unknown_linux_gnu="--sysroot=$SYSROOT -I$SYSROOT/usr/include/pipewire-0.3 -I$SYSROOT/usr/include/spa-0.2"

# Nix-provided zig bakes in a build-time cache path that doesn't exist at runtime
export ZIG_GLOBAL_CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/zig"
mkdir -p "$ZIG_GLOBAL_CACHE_DIR"

# Target glibc 2.38 to match Void Linux's PipeWire build
if command -v cargo-zigbuild &> /dev/null; then
    cargo polylith cargo --profile dev zigbuild --release --target aarch64-unknown-linux-gnu.2.38 \
        --bin mdma-playback
else
    cargo polylith cargo --profile dev build --release --target aarch64-unknown-linux-gnu \
        --bin mdma-playback
fi

echo ""
echo "Build complete"
file target/aarch64-unknown-linux-gnu/release/mdma-playback
ls -lh target/aarch64-unknown-linux-gnu/release/mdma-playback
