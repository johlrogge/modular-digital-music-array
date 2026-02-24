#!/usr/bin/env bash
# Check prerequisites for MDMA development

set -euo pipefail

MISSING=()
CHECK_IMAGE=false

# Parse arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        --image)
            CHECK_IMAGE=true
            shift
            ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 [--image]"
            exit 1
            ;;
    esac
done

echo "🔍 Checking prerequisites..."
echo ""

# ============================================================================
# Package Building Prerequisites (Always Required)
# ============================================================================

# Check for xbps tools
if ! command -v xbps-rindex &> /dev/null; then
    echo "❌ xbps-rindex not found"
    MISSING+=("xbps")
else
    echo "✅ xbps-rindex installed"
fi

# Check ARM64 target availability
TARGET_FOUND=false

# First try rustup (if available and managing targets)
if command -v rustup &> /dev/null; then
    if rustup target list 2>/dev/null | grep -q "aarch64-unknown-linux-gnu (installed)"; then
        echo "✅ Rust ARM64 target installed (rustup)"
        TARGET_FOUND=true
    fi
fi

# Fallback: check rustc directly (Nix-managed Rust)
if [ "$TARGET_FOUND" = false ] && command -v rustc &> /dev/null; then
    if rustc --print target-list 2>/dev/null | grep -q "aarch64-unknown-linux-gnu"; then
        echo "✅ Rust ARM64 target available (Nix-managed)"
        TARGET_FOUND=true
    fi
fi

if [ "$TARGET_FOUND" = false ]; then
    echo "❌ Rust ARM64 target not installed"
    MISSING+=("rust-target")
fi

# Check for cross-compiler (cargo-zigbuild preferred, gcc as fallback)
if command -v cargo-zigbuild &> /dev/null; then
    echo "✅ Cross-compilation via cargo-zigbuild"
elif command -v aarch64-linux-gnu-gcc &> /dev/null; then
    echo "✅ Cross-compiler available (aarch64-linux-gnu-gcc)"
else
    MISSING+=("cross-compiler")
fi

# ============================================================================
# Image Creation Prerequisites (Only if --image flag)
# ============================================================================

if [ "$CHECK_IMAGE" = true ]; then
    echo ""
    echo "🔍 Checking image creation prerequisites..."
    echo ""
    
    # Check for guestfish (libguestfs - for partition detection)
    if ! command -v guestfish &> /dev/null; then
        echo "❌ guestfish not found"
        MISSING+=("libguestfs")
    else
        echo "✅ guestfish installed"
    fi
    
    # Check for guestmount (libguestfs - for mounting)
    if ! command -v guestmount &> /dev/null; then
        echo "❌ guestmount not found"
        MISSING+=("libguestfs")
    else
        echo "✅ guestmount installed"
    fi
fi

echo ""

# If anything missing, show installation instructions
if [ ${#MISSING[@]} -ne 0 ]; then
    echo "❌ Missing prerequisites!"
    echo ""
    echo "To install on Arch Linux:"
    echo ""
    
    # Remove duplicates
    UNIQUE_MISSING=($(printf "%s\n" "${MISSING[@]}" | sort -u))
    
    for item in "${UNIQUE_MISSING[@]}"; do
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
            cross-compiler)
                echo "  # Install a cross-compiler (choose one)"
                echo "  cargo install cargo-zigbuild   # recommended"
                echo "  # or: install aarch64-linux-gnu-gcc via your package manager"
                echo ""
                ;;
            libguestfs)
                echo "  # Install libguestfs (for image creation)"
                echo "  sudo pacman -S libguestfs"
                echo ""
                ;;
        esac
    done
    
    echo "After installing, run this command again."
    exit 1
fi

if [ "$CHECK_IMAGE" = true ]; then
    echo "✅ All prerequisites installed (including image creation tools)!"
else
    echo "✅ All prerequisites installed!"
    echo ""
    echo "To check image creation prerequisites, run:"
    echo "  $0 --image"
fi
