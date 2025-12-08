#!/bin/sh
# MDMA Bootstrap Script
# Runs once on first boot to install beacon package from GitHub

set -e

LOG=/var/log/mdma-bootstrap.log
REPO_URL="https://johlrogge.github.io/modular-digital-music-array"

log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $*" | tee -a "$LOG"
}

log "=== MDMA Bootstrap Starting ==="

# Check if already bootstrapped
if [ -f /var/lib/mdma/.bootstrapped ]; then
    log "System already bootstrapped. Exiting."
    exit 0
fi

log "Configuring MDMA package repository..."
cat > /etc/xbps.d/99-mdma-repo.conf << EOF
repository=${REPO_URL}/aarch64
EOF

log "Updating package index..."
if ! xbps-install -S 2>&1 | tee -a "$LOG"; then
    log "ERROR: Failed to update package index"
    log "Check network connectivity and repository URL"
    exit 1
fi

log "Installing beacon package..."
if ! xbps-install -y beacon 2>&1 | tee -a "$LOG"; then
    log "ERROR: Failed to install beacon package"
    exit 1
fi

log "Installing dependencies..."
# These should be pulled in by beacon's depends, but ensure they're there
xbps-install -y dbus avahi-daemon 2>&1 | tee -a "$LOG" || true

log "Enabling services..."
# Enable dbus (required by avahi)
if [ ! -e /var/service/dbus ]; then
    ln -s /etc/sv/dbus /var/service/
    log "Enabled dbus service"
fi

# Enable avahi-daemon (mDNS)
if [ ! -e /var/service/avahi-daemon ]; then
    ln -s /etc/sv/avahi-daemon /var/service/
    log "Enabled avahi-daemon service"
fi

# Enable beacon (if not already enabled)
if [ ! -e /var/service/beacon ]; then
    ln -s /etc/sv/beacon /var/service/
    log "Enabled beacon service"
fi

log "Creating bootstrap marker..."
mkdir -p /var/lib/mdma
touch /var/lib/mdma/.bootstrapped

log "Starting services..."
sv start dbus 2>&1 | tee -a "$LOG" || true
sv start avahi-daemon 2>&1 | tee -a "$LOG" || true
sv start beacon 2>&1 | tee -a "$LOG" || true

log "=== MDMA Bootstrap Complete ==="
log "Beacon should be available at http://$(hostname).local"
log "If mDNS doesn't work, find IP with: ip addr show"

# Mark bootstrap script as complete so it doesn't run again
mv "$0" "$0.completed"
