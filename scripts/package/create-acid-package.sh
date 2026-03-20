#!/usr/bin/env bash
# Create Void package for mdma-acid
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/create-service-package.sh" \
    --bin mdma-acid \
    --desc "MDMA fact store service" \
    --cargo-toml projects/mdma-acid/Cargo.toml
