#!/usr/bin/env bash
# Build mdma-gateway for ARM64 (Raspberry Pi 5)

set -euo pipefail

echo "Building mdma-gateway for aarch64..."

# Use cargo-zigbuild if available (devenv), otherwise fall back to plain cargo
if command -v cargo-zigbuild &> /dev/null; then
    cargo zigbuild --release --target aarch64-unknown-linux-gnu --bin mdma-gateway
else
    cargo build --release --target aarch64-unknown-linux-gnu --bin mdma-gateway
fi

echo ""
echo "Build complete"
file target/aarch64-unknown-linux-gnu/release/mdma-gateway
ls -lh target/aarch64-unknown-linux-gnu/release/mdma-gateway
