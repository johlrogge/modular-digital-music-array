#!/usr/bin/env bash
# Create Void package for mdma-bandcamp
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/create-service-package.sh" \
    --bin mdma-bandcamp \
    --desc "MDMA Bandcamp music source" \
    --cargo-toml projects/mdma-bandcamp/Cargo.toml \
    --extra-dirs "etc/mdma" \
    --extra-files "void-packages/srcpkgs/mdma-bandcamp/files/mdma-bandcamp/conf:etc/mdma/bandcamp.conf.example" \
    --extra-install "$SCRIPT_DIR/install/mdma-bandcamp.sh"
