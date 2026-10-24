#!/usr/bin/env bash
# Create Void package for mdma-admin
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/create-service-package.sh" \
    --bin mdma-admin \
    --desc "MDMA Admin privileged system-level operations service" \
    --cargo-toml projects/mdma-admin/Cargo.toml
