#!/usr/bin/env bash
# Strip beacon binary to reduce size

set -euo pipefail

BEACON="target/aarch64-unknown-linux-gnu/release/beacon"

if [ ! -f "$BEACON" ]; then
    echo "❌ Beacon not built. Run 'just ci-build-beacon' first"
    exit 1
fi

echo "🔪 Stripping beacon binary..."

# Try multiple strip commands (different platforms)
if command -v aarch64-linux-gnu-strip &> /dev/null; then
    aarch64-linux-gnu-strip "$BEACON"
elif command -v strip &> /dev/null; then
    strip "$BEACON"
else
    echo "⚠️  No strip command available"
    exit 0
fi

echo "✅ Stripped"
ls -lh "$BEACON"
