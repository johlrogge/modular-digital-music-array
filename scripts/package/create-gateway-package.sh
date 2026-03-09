#!/usr/bin/env bash
# Create Void package for mdma-gateway
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/create-service-package.sh" \
    --bin mdma-gateway \
    --desc "MDMA API gateway" \
    --cargo-toml bases/mdma_gateway/Cargo.toml \
    --extra-install "$SCRIPT_DIR/install/mdma-gateway.sh"
