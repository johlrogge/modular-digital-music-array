# Network scanning recipes for finding Raspberry Pi

# Scan network for Raspberry Pi devices
pi-scan:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "🔍 Scanning for Raspberry Pi devices on network..."
    echo ""
    
    # Get local network range
    NETWORK=$(ip route | grep default | awk '{print $3}' | cut -d. -f1-3)
    
    if [ -z "$NETWORK" ]; then
        echo "❌ Could not detect network range"
        exit 1
    fi
    
    echo "Network: $NETWORK.0/24"
    echo ""
    
    # Check if nmap is installed
    if ! command -v nmap &> /dev/null; then
        echo "❌ nmap not found. Install it with:"
        echo "   sudo pacman -S nmap"
        exit 1
    fi
    
    echo "Scanning... (this takes ~30 seconds)"
    echo ""
    
    # Scan for devices and filter for Raspberry Pi
    sudo nmap -sn $NETWORK.0/24 | grep -B 2 "Raspberry\|DC:A6:32\|B8:27:EB\|E4:5F:01" || {
        echo "❌ No Raspberry Pi devices found"
        echo ""
        echo "Make sure:"
        echo "  - Pi is powered on"
        echo "  - Ethernet cable is connected"
        echo "  - Pi has had 60 seconds to boot"
        exit 1
    }
    
    echo ""
    echo "💡 To connect:"
    echo "   ssh root@<IP>"
    echo "   Password: voidlinux"

# Quick scan showing all devices (faster, less detailed)
pi-scan-quick:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "🔍 Quick network scan..."
    echo ""
    
    NETWORK=$(ip route | grep default | awk '{print $3}' | cut -d. -f1-3)
    
    if [ -z "$NETWORK" ]; then
        echo "❌ Could not detect network range"
        exit 1
    fi
    
    # Use arp-scan if available (faster)
    if command -v arp-scan &> /dev/null; then
        sudo arp-scan --localnet | grep -i "raspberry\|b8:27:eb\|dc:a6:32\|e4:5f:01" || {
            echo "No Raspberry Pi found"
            exit 1
        }
    else
        echo "Install arp-scan for faster scanning:"
        echo "  sudo pacman -S arp-scan"
        echo ""
        echo "Using nmap instead..."
        just pi-scan
    fi

# Scan and auto-connect to first found Pi
pi-connect:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "🔍 Finding Raspberry Pi..."
    
    NETWORK=$(ip route | grep default | awk '{print $3}' | cut -d. -f1-3)
    
    if ! command -v nmap &> /dev/null; then
        echo "❌ nmap not found. Install: sudo pacman -S nmap"
        exit 1
    fi
    
    # Scan and extract IP
    PI_IP=$(sudo nmap -sn $NETWORK.0/24 | grep -B 2 "Raspberry\|DC:A6:32\|B8:27:EB\|E4:5F:01" | grep "Nmap scan report" | head -1 | awk '{print $5}')
    
    if [ -z "$PI_IP" ]; then
        echo "❌ No Raspberry Pi found"
        exit 1
    fi
    
    echo "✅ Found Pi at: $PI_IP"
    echo ""
    echo "Connecting... (password: voidlinux)"
    echo ""
    
    ssh root@$PI_IP

# Check if specific IP is a Raspberry Pi
pi-check IP:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "🔍 Checking {{IP}}..."
    echo ""
    
    if ! command -v nmap &> /dev/null; then
        echo "❌ nmap not found. Install: sudo pacman -S nmap"
        exit 1
    fi
    
    # Check if host is up
    if ! ping -c 1 -W 1 {{IP}} &> /dev/null; then
        echo "❌ Host {{IP}} is not responding"
        exit 1
    fi
    
    echo "Host is up, checking details..."
    echo ""
    
    # Get MAC and manufacturer
    sudo nmap -sn {{IP}} | grep -A 1 "{{IP}}"
    
    echo ""
    echo "💡 To connect:"
    echo "   ssh root@{{IP}}"

# Show network scanning help
pi-scan-help:
    @echo "📡 Network Scanning Commands"
    @echo ""
    @echo "Find Raspberry Pi on your network:"
    @echo ""
    @echo "  just pi-scan           # Full scan (recommended, ~30s)"
    @echo "  just pi-scan-quick     # Quick scan (if arp-scan installed)"
    @echo "  just pi-connect        # Find and auto-connect"
    @echo "  just pi-check <IP>     # Check if specific IP is a Pi"
    @echo ""
    @echo "Installation:"
    @echo "  sudo pacman -S nmap         # Required"
    @echo "  sudo pacman -S arp-scan     # Optional (faster scanning)"
    @echo ""
    @echo "Troubleshooting:"
    @echo "  - Make sure Pi is powered on"
    @echo "  - Wait 60 seconds after power-on"
    @echo "  - Use ethernet (WiFi not configured yet)"
    @echo "  - Check router's DHCP leases"
    @echo ""
    @echo "Default credentials:"
    @echo "  Username: root"
    @echo "  Password: voidlinux"

# Monitor for Pi appearing on network
pi-wait:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "⏳ Waiting for Raspberry Pi to appear on network..."
    echo "   (Press Ctrl+C to stop)"
    echo ""
    
    NETWORK=$(ip route | grep default | awk '{print $3}' | cut -d. -f1-3)
    
    if ! command -v nmap &> /dev/null; then
        echo "❌ nmap not found. Install: sudo pacman -S nmap"
        exit 1
    fi
    
    COUNT=0
    while true; do
        COUNT=$((COUNT + 1))
        echo "Scan #$COUNT..."
        
        PI_IP=$(sudo nmap -sn $NETWORK.0/24 | grep -B 2 "Raspberry\|DC:A6:32\|B8:27:EB\|E4:5F:01" | grep "Nmap scan report" | head -1 | awk '{print $5}' || true)
        
        if [ -n "$PI_IP" ]; then
            echo ""
            echo "✅ Found Pi at: $PI_IP"
            echo ""
            echo "To connect:"
            echo "   ssh root@$PI_IP"
            echo "   Password: voidlinux"
            break
        fi
        
        sleep 5
    done
