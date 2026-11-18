#!/usr/bin/env bash
# install-mdma-macos.sh — kept so existing install commands keep working.
# The installer is scripts/install-mdma.sh (detects macOS arm64 / Linux x86_64).
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/johlrogge/modular-digital-music-array/master/scripts/install-mdma-macos.sh | bash
#   curl -fsSL .../install-mdma-macos.sh | bash -s v0.39.1

set -euo pipefail

URL="https://raw.githubusercontent.com/johlrogge/modular-digital-music-array/master/scripts/install-mdma.sh"

script="$(curl --proto '=https' --tlsv1.2 -fsSL "$URL")" || { echo "error: could not download $URL" >&2; exit 1; }
[ -n "$script" ] || { echo "error: empty installer from $URL" >&2; exit 1; }

exec bash -c "$script" install-mdma "$@"
