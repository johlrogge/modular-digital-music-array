# Getting Beacon Working - Simple Steps

## 🎯 Goal

Get ONE working beacon Pi that:
- ✅ Boots from SD card
- ✅ Runs beacon service
- ✅ Accessible at welcome-to-mdma.local
- ✅ Shows provisioning web UI

**That's it! No NVMe, no users, no fancy stuff yet.**

---

## 📋 Steps

### 1. Flash Vanilla Void to SD Card

```bash
# Download Void for Pi
cd ~/mdma-images
curl -LO https://repo-default.voidlinux.org/live/current/void-rpi-aarch64-20250202.img.xz

# Find SD card
lsblk

# Flash it
xz -dc void-rpi-aarch64-20250202.img.xz | \
  sudo dd of=/dev/sdX bs=4M status=progress conv=fsync

sync
sudo eject /dev/sdX
```

**Time:** 10-15 minutes

---

### 2. Boot Pi and Find It

```bash
# Insert SD card, power on Pi, wait 60 seconds

# Find it on network
cd ~/mdma-workspace
just pi-scan

# Or wait for it to appear
just pi-wait

# Or check router's DHCP leases
```

**Expected output:**
```
Nmap scan report for 192.168.0.164
Host is up (0.0012s latency).
MAC Address: DC:A6:32:XX:XX:XX (Raspberry Pi Trading)
```

---

### 3. SSH to Pi

```bash
# Connect (password: voidlinux)
ssh root@192.168.0.164

# Or use recipe
just pi-connect
```

**You're in!**
```
root@void-live:~#
```

---

### 4. Copy and Run Setup Script

**On your dev machine:**
```bash
# Copy setup script
scp ~/Downloads/setup-beacon-on-pi.sh root@192.168.0.164:/root/
```

**On the Pi:**
```bash
# Make executable and run
chmod +x setup-beacon-on-pi.sh
./setup-beacon-on-pi.sh
```

**Script does:**
- Updates system
- Configures MDMA repository
- Installs dbus, avahi, beacon
- Sets hostname to welcome-to-mdma
- Enables and starts services
- Verifies everything

**Time:** 3-5 minutes

---

### 5. Test It Works

**On the Pi:**
```bash
# Check services
sv status beacon dbus avahi-daemon

# Should show all running:
# run: beacon: (pid 1234) 30s
# run: dbus: (pid 1235) 30s
# run: avahi-daemon: (pid 1236) 30s

# Check beacon listening
ss -tulpn | grep :80

# Should show:
# tcp   LISTEN 0.0.0.0:80   users:(("beacon",pid=1234))

# Check hostname
hostname
# welcome-to-mdma
```

**From your dev machine:**
```bash
# Test mDNS
ping welcome-to-mdma.local

# Test web interface
curl http://welcome-to-mdma.local/

# Open browser
http://welcome-to-mdma.local/
```

**If you see the beacon web UI: ✅ SUCCESS!**

---

### 6. Create Golden Image

**Once everything works, shutdown the Pi:**
```bash
# On the Pi
shutdown -h now
```

**Wait for shutdown (green LED stops), then remove SD card.**

**On your dev machine:**
```bash
# Insert SD card
lsblk

# Create golden image
cd ~/mdma-workspace
just golden-create-image /dev/sdX

# This creates:
# ~/mdma-images/golden/mdma-beacon-golden-TIMESTAMP.img.xz
```

**Time:** 10-15 minutes

---

### 7. Test Golden Image

**Flash golden image to a different SD card:**
```bash
xz -dc ~/mdma-images/golden/mdma-beacon-golden-*.img.xz | \
  sudo dd of=/dev/sdX bs=4M status=progress conv=fsync
```

**Boot Pi, wait 60 seconds:**
```bash
ping welcome-to-mdma.local
http://welcome-to-mdma.local/
```

**If it works without any setup: ✅ GOLDEN IMAGE COMPLETE!**

---

## 🎉 Done!

**You now have:**
- ✅ Working beacon on one Pi
- ✅ Golden image ready to distribute
- ✅ Can flash to unlimited Pis
- ✅ Each boots as welcome-to-mdma.local

**Time invested:** ~30-45 minutes once  
**Time per future Pi:** 10-15 minutes (just flash!)

---

## 🔧 Troubleshooting

### Can't find Pi on network

```bash
# Check Pi has power (red LED on)
# Check Pi is booting (green LED blinks)
# Wait full 60 seconds
# Check ethernet cable
# Try scanning again
just pi-scan
```

### Can't ping welcome-to-mdma.local

```bash
# Check you have nss-mdns installed
sudo pacman -S nss-mdns

# Update /etc/nsswitch.conf
sudo sed -i 's/^hosts:.*/hosts: files mymachines mdns_minimal [NOTFOUND=return] resolve [!UNAVAIL=return] dns/' /etc/nsswitch.conf

# Restart avahi
sudo systemctl restart avahi-daemon

# Try pinging again
ping welcome-to-mdma.local
```

### Beacon not running

```bash
# SSH to Pi
ssh root@192.168.0.164

# Check service status
sv status beacon

# Check if binary exists
ls -la /usr/bin/beacon

# Check if service definition exists
ls -la /etc/sv/beacon/

# Check logs
tail -100 /var/log/socklog/current | grep beacon
```

### Services fail to start

```bash
# Check supervise symlinks exist
ls -la /etc/sv/beacon/supervise
ls -la /etc/sv/dbus/supervise
ls -la /etc/sv/avahi-daemon/supervise

# All should show symlinks to /run/runit/supervise.*

# If missing, create them
ln -sf /run/runit/supervise.beacon /etc/sv/beacon/supervise
ln -sf /run/runit/supervise.dbus /etc/sv/dbus/supervise
ln -sf /run/runit/supervise.avahi-daemon /etc/sv/avahi-daemon/supervise

# Restart services
sv restart beacon dbus avahi-daemon
```

---

## 📝 Quick Reference

```bash
# Find Pi
just pi-scan

# Connect to Pi
just pi-connect

# Or manually
ssh root@<IP>  # password: voidlinux

# On Pi, check status
sv status beacon dbus avahi-daemon
ss -tulpn | grep :80
hostname

# From dev machine
ping welcome-to-mdma.local
http://welcome-to-mdma.local/

# Create golden image
just golden-create-image /dev/sdX
```

---

## 🎯 What's Next?

**Once you have a working golden image:**

1. Flash it to multiple Pis
2. Each boots as welcome-to-mdma.local
3. Each shows beacon provisioning UI
4. Ready to provision to NVMe (later)

**For now:** Just get that first beacon working!

---

**Keep it simple, get it working, iterate from there!** 🚀
