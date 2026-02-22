#!/usr/bin/env bash
# Build mdma-bandcamp for ARM64 (Raspberry Pi 5)

set -euo pipefail

echo "Building mdma-bandcamp for aarch64..."

# Use cargo-zigbuild if available (devenv), otherwise fall back to plain cargo
if command -v cargo-zigbuild &> /dev/null; then
    cargo zigbuild --release --target aarch64-unknown-linux-gnu --bin mdma-bandcamp
else
    cargo build --release --target aarch64-unknown-linux-gnu --bin mdma-bandcamp
fi

echo ""
echo "Build complete"
file target/aarch64-unknown-linux-gnu/release/mdma-bandcamp
ls -lh target/aarch64-unknown-linux-gnu/release/mdma-bandcamp
