#!/usr/bin/env bash
# Bump package revision number

set -euo pipefail

TEMPLATE="void-packages/srcpkgs/beacon/template"

if [ ! -f "$TEMPLATE" ]; then
    echo "❌ Template not found at $TEMPLATE"
    echo "   Create void-packages/srcpkgs/beacon/template first"
    exit 1
fi

CURRENT_REV=$(grep '^revision=' "$TEMPLATE" | cut -d= -f2)
NEW_REV=$((CURRENT_REV + 1))

# Create backup
cp "$TEMPLATE" "${TEMPLATE}.backup"

# Update revision
if command -v sed &> /dev/null; then
    sed -i "s/^revision=.*/revision=${NEW_REV}/" "$TEMPLATE"
else
    # Fallback for BSD sed (macOS)
    sed -i '' "s/^revision=.*/revision=${NEW_REV}/" "$TEMPLATE"
fi

echo "✅ Bumped revision: $CURRENT_REV → $NEW_REV"
echo ""

# Show new version
./scripts/utils/get-version.sh
