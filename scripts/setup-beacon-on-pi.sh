#!/bin/bash
# MDMA Beacon Setup Script
# Run this on a fresh Void Linux Pi to configure it as a beacon
set -euo pipefail

echo "🎯 MDMA Beacon Setup Script"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# 1. Update system
echo "📦 Updating system..."
xbps-install -Suy xbps
xbps-install -Suy

echo ""
echo "✅ System updated"
echo ""

# 2. Configure repository
echo "🔧 Configuring MDMA repository..."
cat > /etc/xbps.d/10-mdma.conf <<'EOF'
repository=https://johlrogge.github.io/modular-digital-music-array/aarch64
EOF

xbps-install -S

echo "✅ Repository configured"
echo ""

# 3. Install packages
echo "📦 Installing packages..."
echo "   Installing dbus..."
xbps-install -y dbus

echo "   Installing avahi..."
xbps-install -y avahi

echo "   Installing beacon..."
xbps-install -y beacon

echo "✅ Packages installed"
echo ""

# 4. Set hostname
echo "🏷️  Setting hostname to welcome-to-mdma..."
echo "welcome-to-mdma" > /etc/hostname
hostname welcome-to-mdma

echo "✅ Hostname set"
echo ""

# 5. Enable services
echo "🔧 Enabling services..."

# Services should be auto-enabled by packages, but verify
if [ ! -L /var/service/dbus ]; then
    ln -sf /etc/sv/dbus /var/service/
    echo "   Enabled dbus"
else
    echo "   dbus already enabled"
fi

if [ ! -L /var/service/avahi-daemon ]; then
    ln -sf /etc/sv/avahi-daemon /var/service/
    echo "   Enabled avahi-daemon"
else
    echo "   avahi-daemon already enabled"
fi

if [ ! -L /var/service/beacon ]; then
    ln -sf /etc/sv/beacon /var/service/
    echo "   Enabled beacon"
else
    echo "   beacon already enabled"
fi

echo "✅ Services enabled"
echo ""

# 6. Restart services
echo "🔄 Starting services..."

sv restart dbus
sv restart avahi-daemon
sv restart beacon

# Wait a moment
sleep 3

echo "✅ Services started"
echo ""

# 7. Verify everything
echo "🔍 Verification:"
echo ""

echo "=== Hostname ==="
hostname
echo ""

echo "=== Installed Packages ==="
xbps-query -l | grep -E "beacon|avahi|dbus"
echo ""

echo "=== Service Status ==="
sv status dbus avahi-daemon beacon
echo ""

echo "=== Beacon Listening ==="
ss -tulpn | grep :80
echo ""

echo "=== mDNS Services ==="
avahi-browse -apt 2>&1 | head -10 || echo "(avahi-browse check - may need a moment)"
echo ""

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "✅ Setup complete!"
echo ""
echo "📋 Next steps:"
echo "   1. Test from your computer:"
echo "      ping welcome-to-mdma.local"
echo "      http://welcome-to-mdma.local/"
echo ""
echo "   2. If everything works, shutdown and image:"
echo "      shutdown -h now"
echo ""
echo "   3. Then create image from SD card on your dev machine"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
