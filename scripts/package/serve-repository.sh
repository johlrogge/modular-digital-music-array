#!/usr/bin/env bash
# Serve repository locally for testing

set -euo pipefail

REPO_DIR="build/repository"

if [ ! -d "$REPO_DIR/aarch64" ]; then
    echo "❌ Repository not built"
    echo "   Run: just pkg-build-all"
    exit 1
fi

LOCAL_IP=$(hostname -I 2>/dev/null | awk '{print $1}' || echo "localhost")

echo "🌐 Serving repository at http://localhost:8080"
echo ""
echo "On your Pi, configure:"
echo "  echo 'repository=http://${LOCAL_IP}:8080/aarch64' | sudo tee /etc/xbps.d/99-mdma-local.conf"
echo "  sudo xbps-install -S"
echo "  sudo xbps-install -y beacon"
echo ""
echo "Press Ctrl+C to stop"
echo ""

cd "$REPO_DIR"
python3 -m http.server 8080
