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
