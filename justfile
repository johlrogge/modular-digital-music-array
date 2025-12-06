default:
    just --list

# watch and run check, test, build and clippy when files change
[group('build')]
watch:
    cargo watch -x check -x test -x build -x clippy

# just build
[group('build')]
build:
    cargo build

    
    
# MDMA Build Recipes - Beacon Focus
# Simplified for Milestone 1: Just get beacon running

# Quick cross-compile beacon using cross-rs (recommended for Arch)
[group('build')]
beacon-cross:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v cross &> /dev/null; then
        echo "Installing cross (Docker-based cross-compiler)..."
        cargo install cross --git https://github.com/cross-rs/cross
    fi
    echo "Building beacon for aarch64..."
    cross build --release --target aarch64-unknown-linux-gnu --bin beacon
    echo ""
    echo "✅ Beacon built!"
    file target/aarch64-unknown-linux-gnu/release/beacon
    ls -lh target/aarch64-unknown-linux-gnu/release/beacon

# Build beacon with native cargo (requires system cross-compiler)
[group('build')]
beacon-native:
    cargo build --release --target aarch64-unknown-linux-gnu --bin beacon
    @echo ""
    @file target/aarch64-unknown-linux-gnu/release/beacon
    @ls -lh target/aarch64-unknown-linux-gnu/release/beacon

# Strip beacon binary for production
[group('build')]
beacon-strip:
    #!/usr/bin/env bash
    set -euo pipefail
    BEACON="target/aarch64-unknown-linux-gnu/release/beacon"
    if [ ! -f "$BEACON" ]; then
        echo "❌ Beacon not built yet. Run 'just beacon-cross' first"
        exit 1
    fi
    SIZE_BEFORE=$(stat -c%s "$BEACON" 2>/dev/null || stat -f%z "$BEACON")
    aarch64-linux-gnu-strip "$BEACON" || strip "$BEACON"
    SIZE_AFTER=$(stat -c%s "$BEACON" 2>/dev/null || stat -f%z "$BEACON")
    echo "Beacon stripped:"
    echo "  Before: $(numfmt --to=iec-i $SIZE_BEFORE 2>/dev/null || echo $SIZE_BEFORE bytes)"
    echo "  After:  $(numfmt --to=iec-i $SIZE_AFTER 2>/dev/null || echo $SIZE_AFTER bytes)"
    ls -lh "$BEACON"

# Check beacon dependencies for cross-compilation compatibility  
[group('build')]
beacon-deps:
    cargo tree --target aarch64-unknown-linux-gnu --package mdma-beacon

# Set up Cargo config for cross-compilation (native gcc method)
[group('build')]
setup-cross:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "Setting up Cargo cross-compilation config..."
    mkdir -p .cargo
    if [ ! -f .cargo/config.toml ]; then
        echo '[target.aarch64-unknown-linux-gnu]' > .cargo/config.toml
        echo 'linker = "aarch64-linux-gnu-gcc"' >> .cargo/config.toml
        echo '' >> .cargo/config.toml
        echo '[build]' >> .cargo/config.toml
        echo '# Uncomment to make aarch64 the default target' >> .cargo/config.toml
        echo '# target = "aarch64-unknown-linux-gnu"' >> .cargo/config.toml
        echo "✅ Created .cargo/config.toml"
    else
        echo "⚠️  .cargo/config.toml already exists"
        cat .cargo/config.toml
    fi

# Check if cross-compilation toolchain is available
[group('build')]
check-toolchain:
    #!/usr/bin/env bash
    echo "Checking cross-compilation options..."
    echo ""
    if command -v cross &> /dev/null; then
        echo "✅ cross-rs available (recommended)"
        echo "   Use: just beacon-cross"
    else
        echo "❌ cross-rs not found"
        echo "   Install: cargo install cross --git https://github.com/cross-rs/cross"
    fi
    echo ""
    if command -v aarch64-linux-gnu-gcc &> /dev/null; then
        echo "✅ aarch64-linux-gnu-gcc available"
        echo "   Use: just beacon-native"
    else
        echo "❌ aarch64-linux-gnu-gcc not found"
        echo "   Install (AUR): yay -S aarch64-linux-gnu-gcc"
    fi
    echo ""
    echo "Rust target:"
    if rustup target list | grep -q "aarch64-unknown-linux-gnu (installed)"; then
        echo "✅ aarch64-unknown-linux-gnu target installed"
    else
        echo "❌ aarch64-unknown-linux-gnu target not installed"
        echo "   Install: rustup target add aarch64-unknown-linux-gnu"
    fi

# Watch beacon and rebuild on changes (for development)
[group('dev')]
beacon-watch:
    cargo watch -x 'build --bin beacon'

# Run beacon locally (x86_64 - for development/testing)
[group('dev')]
beacon-run:
    cargo run --bin beacon

# Build beacon for current platform (development)
[group('dev')]
beacon-dev:
    cargo build --bin beacon
    @ls -lh target/debug/beacon
