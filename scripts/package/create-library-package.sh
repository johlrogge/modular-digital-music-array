#!/usr/bin/env bash
# Create Void package for mdma-library
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/create-service-package.sh" \
    --bin mdma-library \
    --desc "MDMA music library service" \
    --cargo-toml projects/mdma-library/Cargo.toml \
    --extra-dirs "music/inbox music/blobs metadata" \
    --extra-install "$SCRIPT_DIR/install/mdma-library.sh"
