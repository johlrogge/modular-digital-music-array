#!/usr/bin/env bash
# Create Void package for beacon
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/create-service-package.sh" \
    --bin beacon \
    --desc "MDMA provisioning beacon" \
    --cargo-toml projects/mdma-beacon/Cargo.toml \
    --deps "avahi>=0 dbus>=0" \
    --extra-install "$SCRIPT_DIR/install/beacon.sh"
