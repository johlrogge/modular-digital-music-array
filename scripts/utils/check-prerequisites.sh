#!/usr/bin/env bash
# Check prerequisites for beacon package building

set -euo pipefail

MISSING=()

echo "🔍 Checking prerequisites..."
echo ""

# Check for xbps tools
if ! command -v xbps-rindex &> /dev/null; then
    echo "❌ xbps-rindex not found"
    MISSING+=("xbps")
else
    echo "✅ xbps-rindex installed"
fi

# Check for cross-compilation target
if ! rustup target list | grep -q "aarch64-unknown-linux-gnu (installed)"; then
    echo "❌ Rust ARM64 target not installed"
    MISSING+=("rust-target")
else
    echo "✅ Rust ARM64 target installed"
fi

# Check for cross-compiler (optional but recommended)
if command -v aarch64-linux-gnu-gcc &> /dev/null; then
    echo "✅ aarch64-linux-gnu-gcc installed"
elif command -v cross &> /dev/null; then
    echo "✅ cross (Docker-based) installed"
else
    echo "⚠️  No ARM64 cross-compiler found (optional)"
    echo "   For native builds: install aarch64-linux-gnu-gcc"
    echo "   For Docker builds: cargo install cross"
fi

echo ""

# If anything missing, show installation instructions
if [ ${#MISSING[@]} -ne 0 ]; then
    echo "❌ Missing prerequisites!"
    echo ""
    echo "To install on Arch Linux:"
    echo ""
    
    for item in "${MISSING[@]}"; do
        case "$item" in
            xbps)
                echo "  # Install xbps tools"
                echo "  sudo pacman -S xbps"
                echo ""
                ;;
            rust-target)
                echo "  # Install Rust ARM64 target"
                echo "  rustup target add aarch64-unknown-linux-gnu"
                echo ""
                ;;
        esac
    done
    
    echo "After installing, run this command again."
    exit 1
fi

echo "✅ All prerequisites installed!"
