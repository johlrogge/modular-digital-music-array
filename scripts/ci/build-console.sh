#!/usr/bin/env bash
# Build mdma-console for ARM64 (Raspberry Pi 5)

set -euo pipefail

echo "Building mdma-console for aarch64..."

# Use cargo-zigbuild if available (devenv), otherwise fall back to plain cargo
if command -v cargo-zigbuild &> /dev/null; then
    cargo zigbuild --release --target aarch64-unknown-linux-gnu --bin mdma-console
else
    cargo build --release --target aarch64-unknown-linux-gnu --bin mdma-console
fi

echo ""
echo "Build complete"
file target/aarch64-unknown-linux-gnu/release/mdma-console
ls -lh target/aarch64-unknown-linux-gnu/release/mdma-console
