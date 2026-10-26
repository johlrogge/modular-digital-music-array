#!/usr/bin/env bash
# Create Void package for mdma-admin
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/create-service-package.sh" \
    --bin mdma-admin \
    --desc "MDMA Admin privileged system-level operations service" \
    --cargo-toml projects/mdma-admin/Cargo.toml \
    --deps "tar>=0 diffutils>=0 util-linux>=0 xbps>=0" \
    --extra-files "void-packages/srcpkgs/mdma-admin/files/mdma-admin/migrate-bootfs:usr/libexec/mdma/migrate-bootfs"
