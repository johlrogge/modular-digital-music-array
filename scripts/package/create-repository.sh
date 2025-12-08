#!/usr/bin/env bash
# Create and index package repository

set -euo pipefail

PACKAGES_DIR="build/packages"
REPO_DIR="build/repository"

echo "📚 Creating package repository..."
echo ""

# Check for xbps-rindex
if ! command -v xbps-rindex &> /dev/null; then
    echo "❌ xbps-rindex not found"
    echo ""
    echo "Install with:"
    echo "  sudo pacman -S xbps"
    echo ""
    exit 1
fi

if [ ! -d "$PACKAGES_DIR" ] || [ -z "$(ls -A $PACKAGES_DIR/*.xbps 2>/dev/null)" ]; then
    echo "❌ No packages found in $PACKAGES_DIR"
    echo "   Run: just pkg-beacon"
    exit 1
fi

# Create repository structure
rm -rf "$REPO_DIR"
mkdir -p "$REPO_DIR/aarch64"

# Copy packages
echo "  → Copying packages..."
cp "$PACKAGES_DIR"/*.xbps "$REPO_DIR/aarch64/"

# Generate repository index
echo "  → Generating repository index..."
cd "$REPO_DIR"
# For cross-architecture, set XBPS_TARGET_ARCH environment variable
XBPS_TARGET_ARCH=aarch64 xbps-rindex -a aarch64/*.xbps
cd - > /dev/null

echo ""
echo "✅ Repository created in $REPO_DIR/"
echo ""
echo "Files:"
ls -lh "$REPO_DIR/aarch64/"
