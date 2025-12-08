#!/usr/bin/env bash
# Test package installation on Pi

set -euo pipefail

PI_HOST="${1:-}"

if [ -z "$PI_HOST" ]; then
    echo "Usage: $0 PI_HOST"
    echo "Example: $0 192.168.0.38"
    echo "Example: $0 welcome-to-mdma.local"
    exit 1
fi

REPO_DIR="build/repository"

if [ ! -d "$REPO_DIR/aarch64" ]; then
    echo "❌ Repository not built"
    echo "   Run: just pkg-build-all"
    exit 1
fi

echo "🧪 Testing package installation on $PI_HOST..."
echo ""

# Start local repository server in background
cd "$REPO_DIR"
python3 -m http.server 8080 &
SERVER_PID=$!
cd - > /dev/null

# Give server time to start
sleep 2

# Get local IP
LOCAL_IP=$(hostname -I 2>/dev/null | awk '{print $1}' || echo "localhost")

echo "  → Configuring Pi to use local repository..."
ssh root@"$PI_HOST" "echo 'repository=http://${LOCAL_IP}:8080/aarch64' > /etc/xbps.d/99-mdma-local.conf" || {
    kill $SERVER_PID
    echo "❌ Failed to configure Pi"
    exit 1
}

echo "  → Updating package index..."
ssh root@"$PI_HOST" "xbps-install -S" || {
    kill $SERVER_PID
    echo "❌ Failed to update package index"
    exit 1
}

echo "  → Installing beacon package..."
ssh root@"$PI_HOST" "xbps-install -y beacon" || {
    kill $SERVER_PID
    echo "❌ Failed to install beacon"
    exit 1
}

echo "  → Restarting beacon service..."
ssh root@"$PI_HOST" "sv restart beacon" || {
    kill $SERVER_PID
    echo "⚠️  Failed to restart service (may not exist yet)"
}

# Kill local server
kill $SERVER_PID

echo ""
echo "✅ Package installed and service restarted on $PI_HOST"
echo ""
echo "Test beacon at: http://$PI_HOST"
