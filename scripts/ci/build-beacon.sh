#!/usr/bin/env bash
# Build beacon for ARM64 (Raspberry Pi 5)

set -euo pipefail

echo "🔨 Building beacon for aarch64..."
cargo build --release --target aarch64-unknown-linux-gnu --bin beacon

echo ""
echo "✅ Build complete"
file target/aarch64-unknown-linux-gnu/release/beacon
ls -lh target/aarch64-unknown-linux-gnu/release/beacon
