#!/usr/bin/env bash
# Build a single MDMA binary for ARM64 (Raspberry Pi 5)
# Usage: build-binary.sh <bin-name>
set -euo pipefail

BIN="${1:?Usage: build-binary.sh <bin-name>}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

export ZIG_GLOBAL_CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/zig"
mkdir -p "$ZIG_GLOBAL_CACHE_DIR"

echo "Building $BIN for aarch64..."

if command -v cargo-zigbuild &> /dev/null; then
    cargo polylith cargo --profile dev zigbuild --release --target aarch64-unknown-linux-gnu.2.38 \
        --bin "$BIN"
else
    cargo polylith cargo --profile dev build --release --target aarch64-unknown-linux-gnu \
        --bin "$BIN"
fi

echo ""
echo "Build complete: $BIN"
file "target/aarch64-unknown-linux-gnu/release/$BIN"
ls -lh "target/aarch64-unknown-linux-gnu/release/$BIN"
