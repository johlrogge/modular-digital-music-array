#!/usr/bin/env bash
# Create Void package for mdma-acid
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Generate the production profile manifest (file-backed fact-store)
echo "=== Generating production profile manifest ==="
cd "$PROJECT_ROOT"
cargo polylith profile build production --no-build

exec "$SCRIPT_DIR/create-service-package.sh" \
    --bin mdma-acid \
    --desc "MDMA fact store service" \
    --cargo-toml projects/mdma-acid/Cargo.toml
