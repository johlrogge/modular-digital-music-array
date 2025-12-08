#!/usr/bin/env bash
# Get current package version

set -euo pipefail

TEMPLATE="void-packages/srcpkgs/beacon/template"

if [ -f "$TEMPLATE" ]; then
    VERSION=$(grep '^version=' "$TEMPLATE" | cut -d= -f2)
    REVISION=$(grep '^revision=' "$TEMPLATE" | cut -d= -f2)
    echo "beacon-${VERSION}_${REVISION}"
else
    echo "beacon-0.1.0_1 (default - no template found)"
fi
