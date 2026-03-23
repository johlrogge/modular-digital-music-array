#!/usr/bin/env bash
# Create Void package for mdma-acid
# The binary must be pre-built with the production profile (file-backed fact-store).
# See build-all.sh Phase 1f or justfile acid-cross for the build command.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

cd "$PROJECT_ROOT"

exec "$SCRIPT_DIR/create-service-package.sh" \
    --bin mdma-acid \
    --desc "MDMA fact store service" \
    --cargo-toml projects/mdma-acid/Cargo.toml
