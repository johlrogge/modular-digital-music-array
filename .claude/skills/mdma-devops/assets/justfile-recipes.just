# MDMA DevOps Justfile Recipes
# Copy these recipes into your project's justfile

# Default recipe - show available commands
default:
    @just --list

# Provision an SD card for MDMA unit
# Usage: just provision-sd mdma-909-studio 909 /dev/sdX
provision-sd hostname role device:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "🔍 Provisioning SD card for {{hostname}} (role: {{role}})"
    echo "   Device: {{device}}"
    echo ""
    
    # Safety check
    if [[ "{{device}}" == "/dev/sda" ]] || [[ "{{device}}" == "/dev/nvme0n1" ]]; then
        echo "❌ Refusing to write to {{device}} - looks like your system drive!"
        exit 1
    fi
    
    # Confirm
    read -p "⚠️  This will ERASE {{device}}. Continue? (yes/no): " confirm
    if [[ "$confirm" != "yes" ]]; then
        echo "Aborted."
        exit 1
    fi
    
    # Download base image if not cached
    CACHE_DIR="$HOME/.cache/mdma"
    BASE_IMAGE="$CACHE_DIR/void-rpi-aarch64-base.tar.xz"
    
    mkdir -p "$CACHE_DIR"
    
    if [[ ! -f "$BASE_IMAGE" ]]; then
        echo "📥 Downloading Void Linux base image..."
        curl -L -o "$BASE_IMAGE" \
            "https://repo-default.voidlinux.org/live/current/void-rpi-aarch64-ROOTFS-20230628.tar.xz"
    fi
    
    # Unmount device if mounted
    sudo umount {{device}}* 2>/dev/null || true
    
    # Partition SD card
    echo "📝 Partitioning {{device}}..."
    sudo parted -s {{device}} mklabel msdos
    sudo parted -s {{device}} mkpart primary fat32 0% 512MB
    sudo parted -s {{device}} mkpart primary fat32 512MB 1024MB
    sudo parted -s {{device}} set 1 boot on
    
    # Format partitions
    echo "💾 Formatting partitions..."
    sudo mkfs.vfat -F 32 -n BOOT {{device}}1
    sudo mkfs.vfat -F 32 -n BOOT-SPARE {{device}}2
    
    # Mount boot partition
    MOUNT_POINT="/tmp/mdma-sd-provision"
    sudo mkdir -p "$MOUNT_POINT"
    sudo mount {{device}}1 "$MOUNT_POINT"
    
    # Extract base system
    echo "📦 Extracting base system..."
    sudo tar -xJf "$BASE_IMAGE" -C "$MOUNT_POINT"
    
    # Configure hostname
    echo "⚙️  Configuring hostname: {{hostname}}"
    echo "{{hostname}}" | sudo tee "$MOUNT_POINT/etc/hostname" >/dev/null
    
    # Install SSH keys
    echo "🔑 Installing deployment keys..."
    sudo mkdir -p "$MOUNT_POINT/root/.ssh"
    sudo chmod 700 "$MOUNT_POINT/root/.ssh"
    
    if [[ -f "$HOME/.ssh/mdma_deploy_ed25519.pub" ]]; then
        sudo cp "$HOME/.ssh/mdma_deploy_ed25519.pub" \
            "$MOUNT_POINT/root/.ssh/authorized_keys"
        sudo chmod 600 "$MOUNT_POINT/root/.ssh/authorized_keys"
    else
        echo "⚠️  Warning: No deployment key found at ~/.ssh/mdma_deploy_ed25519.pub"
        echo "   Generate with: ssh-keygen -t ed25519 -f ~/.ssh/mdma_deploy_ed25519"
    fi
    
    # Configure Avahi
    echo "📡 Configuring mDNS..."
    sudo tee "$MOUNT_POINT/etc/avahi/avahi-daemon.conf" >/dev/null <<EOF
    [server]
    host-name={{hostname}}
    use-ipv4=yes
    use-ipv6=no
    enable-dbus=no
    
    [publish]
    publish-addresses=yes
    publish-hinfo=yes
    publish-workstation=yes
    EOF
    
    # Enable Avahi service
    sudo ln -sf /etc/sv/avahi-daemon "$MOUNT_POINT/etc/runit/runsvdir/default/" || true
    
    # Create beacon marker
    echo "🔵 Setting up beacon mode..."
    sudo tee "$MOUNT_POINT/etc/rc.local" >/dev/null <<EOF
    #!/bin/sh
    # Beacon mode - awaiting Ansible provisioning
    echo "{{hostname}}" > /run/beacon-hostname
    echo "{{role}}" > /run/beacon-role
    
    # Detect NVMe drives
    ls /dev/nvme* 2>/dev/null | wc -l > /run/beacon-nvme-count || echo "0" > /run/beacon-nvme-count
    
    # Optional: Start simple HTTP beacon on port 8080
    # Requires Python (usually included in base image)
    (
        cd /run
        cat > beacon.json <<BEACON_EOF
    {
        "status": "beacon",
        "hostname": "{{hostname}}",
        "role": "{{role}}",
        "needs_provisioning": true,
        "nvme_detected": true,
        "nvme_count": $(cat /run/beacon-nvme-count)
    }
    BEACON_EOF
        python3 -m http.server 8080 >/dev/null 2>&1 &
    ) || true
    EOF
    
    sudo chmod +x "$MOUNT_POINT/etc/rc.local"
    
    # Sync and unmount
    echo "💾 Syncing..."
    sync
    sudo umount "$MOUNT_POINT"
    sudo rmdir "$MOUNT_POINT"
    
    echo ""
    echo "✅ SD card provisioned successfully!"
    echo ""
    echo "Next steps:"
    echo "1. Insert SD card into Raspberry Pi 5"
    echo "2. Connect NVMe drive(s) to the Pi"
    echo "3. Power on the Pi"
    echo "4. Wait ~60 seconds for network initialization"
    echo "5. Run: just discover-units"
    echo "6. Run: just ansible-provision {{hostname}}.local"
    echo ""

# Discover MDMA units on the network via mDNS
discover-units:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "🔍 Scanning for MDMA units..."
    echo ""
    
    # Use avahi-browse if available
    if command -v avahi-browse >/dev/null 2>&1; then
        timeout 5 avahi-browse -t _workstation._tcp 2>/dev/null | \
            grep mdma | \
            awk '{print $4}' | \
            sort -u || echo "No units found via avahi-browse"
    fi
    
    echo ""
    echo "Attempting to ping known hostnames:"
    
    # Try common hostnames
    for host in mdma-909-studio mdma-101-bedroom mdma-303-kitchen; do
        if ping -c 1 -W 1 "${host}.local" >/dev/null 2>&1; then
            echo "  ✓ ${host}.local (responding)"
            
            # Try to fetch beacon status
            if command -v curl >/dev/null 2>&1; then
                status=$(curl -s -m 2 "http://${host}.local:8080/beacon.json" 2>/dev/null || echo "{}")
                if [[ "$status" != "{}" ]]; then
                    echo "    $(echo "$status" | jq -r 'if .needs_provisioning then "needs provisioning" else "already provisioned" end')"
                fi
            fi
        fi
    done
    
    echo ""
    echo "💡 To provision a unit, run:"
    echo "   just ansible-provision <hostname>.local"

# Provision a discovered unit via Ansible
ansible-provision hostname:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "🚀 Provisioning {{hostname}} via Ansible..."
    
    # Check if Ansible is installed
    if ! command -v ansible-playbook >/dev/null 2>&1; then
        echo "❌ Ansible not found. Install with:"
        echo "   pip install ansible"
        exit 1
    fi
    
    # Check if playbook exists
    PLAYBOOK="ansible/provision-mdma.yml"
    if [[ ! -f "$PLAYBOOK" ]]; then
        echo "❌ Playbook not found: $PLAYBOOK"
        echo "   Create the playbook first (see MDMA DevOps skill documentation)"
        exit 1
    fi
    
    # Run Ansible playbook
    ansible-playbook -i "{{hostname}}," "$PLAYBOOK" \
        --extra-vars "target_host={{hostname}}" \
        --become
    
    echo ""
    echo "✅ Provisioning complete!"
    echo ""
    echo "Next steps:"
    echo "1. Verify health: ssh mdma@{{hostname}} 'mdma-health-check'"
    echo "2. Import music library if needed"
    echo "3. Test audio playback"

# Provision all discovered units
ansible-provision-all:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "🚀 Provisioning all discovered units..."
    
    # Find all mdma-*.local hosts that respond
    hosts=()
    for host in mdma-909-studio mdma-101-bedroom mdma-303-kitchen; do
        if ping -c 1 -W 1 "${host}.local" >/dev/null 2>&1; then
            hosts+=("${host}.local")
        fi
    done
    
    if [[ ${#hosts[@]} -eq 0 ]]; then
        echo "❌ No units found"
        exit 1
    fi
    
    echo "Found ${#hosts[@]} unit(s):"
    for host in "${hosts[@]}"; do
        echo "  - $host"
    done
    echo ""
    
    read -p "Provision all units? (yes/no): " confirm
    if [[ "$confirm" != "yes" ]]; then
        echo "Aborted."
        exit 1
    fi
    
    # Provision each unit
    for host in "${hosts[@]}"; do
        echo ""
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo "Provisioning: $host"
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        just ansible-provision "$host"
    done
    
    echo ""
    echo "✅ All units provisioned!"

# SSH into a unit
ssh-unit hostname:
    ssh mdma@{{hostname}}.local

# Run health check on a unit
health-check hostname:
    ssh mdma@{{hostname}}.local 'mdma-health-check'

# Update a unit's packages
update-unit hostname:
    ssh mdma@{{hostname}}.local 'mdma-update'

# Generate SSH deployment keys
generate-deploy-keys:
    #!/usr/bin/env bash
    set -euo pipefail
    
    KEY_PATH="$HOME/.ssh/mdma_deploy_ed25519"
    
    if [[ -f "$KEY_PATH" ]]; then
        echo "⚠️  Deployment keys already exist at $KEY_PATH"
        exit 1
    fi
    
    echo "🔑 Generating deployment SSH keys..."
    ssh-keygen -t ed25519 -f "$KEY_PATH" -C "mdma-deploy" -N ""
    
    echo ""
    echo "✅ Keys generated:"
    echo "   Private: $KEY_PATH"
    echo "   Public:  ${KEY_PATH}.pub"
    echo ""
    echo "These keys will be automatically used when provisioning SD cards."

# Clean up package build artifacts
clean-packages:
    rm -rf void-packages/hostdir/binpkgs/*.xbps
    rm -rf void-packages/masterdir/builddir/*

# Build MDMA packages locally (requires void-packages checkout)
build-packages:
    #!/usr/bin/env bash
    set -euo pipefail
    
    if [[ ! -d "void-packages" ]]; then
        echo "📥 Cloning void-packages..."
        git clone --depth 1 https://github.com/void-linux/void-packages.git
    fi
    
    cd void-packages
    
    # Bootstrap if needed
    if [[ ! -d "masterdir" ]]; then
        echo "🔧 Bootstrapping xbps-src..."
        ./xbps-src binary-bootstrap
    fi
    
    # Copy package templates
    echo "📦 Copying MDMA package templates..."
    cp -r ../packaging/srcpkgs/* srcpkgs/
    
    # Build packages
    echo "🔨 Building packages..."
    ./xbps-src pkg mdma-common
    ./xbps-src pkg mdma-909
    ./xbps-src pkg mdma-101
    ./xbps-src pkg mdma-303
    
    echo ""
    echo "✅ Packages built:"
    ls -lh hostdir/binpkgs/mdma-*.xbps
