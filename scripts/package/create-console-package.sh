#!/usr/bin/env bash
# Create Void package for mdma-console
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/create-service-package.sh" \
    --bin mdma-console \
    --desc "MDMA web console" \
    --cargo-toml bases/mdma_console/Cargo.toml \
    --extra-install "$SCRIPT_DIR/install/mdma-console.sh"
