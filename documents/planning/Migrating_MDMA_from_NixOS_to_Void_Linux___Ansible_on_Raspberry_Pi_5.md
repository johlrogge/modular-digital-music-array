# Migrating MDMA from NixOS to Void Linux + Ansible on Raspberry Pi 5

## Why We're Migrating

After extensive research into Raspberry Pi 5 audio capabilities, **NixOS proves unsuitable for professional audio applications** on this platform:

- **Pi 5 nixos-hardware module fails to build** with kernel module errors
- **No real-time kernel support** exists for Pi 5 on NixOS
- **Audio subsystem failures** including PipeWire device detection issues and >500ms latency
- **BCM2712 SoC and RP1 southbridge** require vendor-specific kernels incompatible with RT patches

**Void Linux emerges as the optimal alternative**, offering:
- **Official Pi 5 support** with dedicated rpi5-kernel package
- **4x faster SSH login** compared to Raspberry Pi OS
- **Minimal resource usage** (200MB RAM baseline)
- **Real-time scheduling capabilities** essential for professional audio
- **Strong audio framework support** including PipeWire and JACK

**Ansible replaces NixOS's declarative configuration management** with:
- **Agentless architecture** minimizing embedded system overhead
- **YAML-based configurations** approaching NixOS's reproducibility
- **Excellent audio system configuration** for RT kernels and audio groups
- **Mature ecosystem** with proven Pi deployment patterns

This migration maintains our infrastructure-as-code philosophy while enabling the professional audio capabilities MDMA requires.

---

## Prerequisites

- Raspberry Pi 5 (4GB or 8GB recommended)
- MicroSD card (32GB minimum, Class 10/A2 rating)
- Ethernet connection for initial setup
- Another Linux machine for Ansible control

## Phase 1: Void Linux Installation

### Step 1: Download and Flash Void Linux

```bash
# Download latest Void Linux rpi5 image
curl -LO "https://repo-default.voidlinux.org/live/current/void-rpi5-PLATFORMFS-$(date +%Y%m%d).img.xz"

# Flash to SD card (replace /dev/sdX with your device)
xz -d void-rpi5-*.img.xz
sudo dd if=void-rpi5-*.img of=/dev/sdX bs=4M status=progress conv=fsync
```

### Step 2: Pre-configure SSH Access and Serial Console

```bash
# Mount both boot and root partitions
mkdir -p /tmp/rpi-boot /tmp/rpi-root
sudo mount /dev/sdX1 /tmp/rpi-boot  # Boot partition (FAT32)
sudo mount /dev/sdX2 /tmp/rpi-root  # Root partition (ext4)

# Enable SSH on first boot
sudo touch /tmp/rpi-boot/ssh

# Configure serial console in boot config
sudo tee -a /tmp/rpi-boot/config.txt << 'EOF'

# Serial console configuration for debugging
enable_uart=1
dtparam=uart0=on
core_freq=250
EOF

# Update cmdline.txt for serial console
sudo cp /tmp/rpi-boot/cmdline.txt /tmp/rpi-boot/cmdline.txt.backup
sudo sed -i 's/$/ console=serial0,115200 console=tty1/' /tmp/rpi-boot/cmdline.txt

# Pre-configure SSH key for root user (eliminates default password)
sudo mkdir -p /tmp/rpi-root/root/.ssh
sudo cp ~/.ssh/id_rsa.pub /tmp/rpi-root/root/.ssh/authorized_keys
sudo chmod 700 /tmp/rpi-root/root/.ssh
sudo chmod 600 /tmp/rpi-root/root/.ssh/authorized_keys

# Enable serial console login in Void Linux
sudo tee -a /tmp/rpi-root/etc/ttys << 'EOF'
ttyAMA0	"/sbin/agetty 115200"	vt100	on  secure
EOF

# Optional: Disable password authentication entirely
sudo sed -i 's/#PasswordAuthentication yes/PasswordAuthentication no/' /tmp/rpi-root/etc/ssh/sshd_config
sudo sed -i 's/#PubkeyAuthentication yes/PubkeyAuthentication yes/' /tmp/rpi-root/etc/ssh/sshd_config

# Unmount partitions
sudo umount /tmp/rpi-boot /tmp/rpi-root
rmdir /tmp/rpi-boot /tmp/rpi-root

# Insert SD card and boot Pi 5
# Both SSH and serial console will be available
```

### Serial Console Connection

For debugging via serial console, you'll need:

**Hardware:**
- USB-to-TTL serial adapter (3.3V logic level - FTDI FT232RL or similar)
- 3 jumper wires

**Pi 5 GPIO Serial Pins:**
- Pin 6 (GND) → Serial adapter GND
- Pin 8 (GPIO14/TXD) → Serial adapter RX  
- Pin 10 (GPIO15/RXD) → Serial adapter TX
- **DO NOT connect VCC/5V**

**Connection:**
```bash
# On your development machine
sudo apt install minicom  # or screen, picocom

# Connect to serial console (adjust device as needed)
sudo minicom -b 115200 -D /dev/ttyUSB0

# Alternative with screen
sudo screen /dev/ttyUSB0 115200

# Alternative with picocom
sudo picocom -b 115200 /dev/ttyUSB0
```

**Serial Console Usage:**
- Boot messages appear automatically
- Login prompt available if network/SSH fails
- Full root shell access for emergency recovery
- `Ctrl-A X` to exit minicom, `Ctrl-A K` to exit screen

### Step 3: Initial System Setup

```bash
# SSH into Pi with your key (no password needed!)
ssh root@<pi-ip-address>

# Update system and install essential packages
xbps-install -Su
xbps-install -S void-repo-nonfree void-repo-multilib
xbps-install -S python3 python3-pip sudo curl wget git

# Create non-root user for MDMA
useradd -m -G wheel,audio,video mdma
passwd mdma
echo "mdma ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/mdma

# Copy SSH key to MDMA user
mkdir -p /home/mdma/.ssh
cp /root/.ssh/authorized_keys /home/mdma/.ssh/
chown -R mdma:mdma /home/mdma/.ssh
chmod 700 /home/mdma/.ssh
chmod 600 /home/mdma/.ssh/authorized_keys

# Disable root SSH login for security
sed -i 's/#PermitRootLogin yes/PermitRootLogin no/' /etc/ssh/sshd_config
sv restart sshd

# Test MDMA user SSH access
# ssh mdma@<pi-ip-address>  # Should work with your key
```

### Step 4: Audio System Preparation

```bash
# Install base audio packages
xbps-install -S alsa-utils pipewire wireplumber pipewire-alsa \
  rtkit dbus elogind

# Enable services
ln -s /etc/sv/dbus /var/service/
ln -s /etc/sv/elogind /var/service/
ln -s /etc/sv/rtkit /var/service/

# Reboot to ensure services start
reboot
```

## Phase 2: Ansible Setup and Configuration

### Step 1: Ansible Control Machine Setup

```bash
# On your control machine (not the Pi)
pip install ansible

# Create MDMA Ansible project structure
mkdir -p mdma-ansible/{inventory,playbooks,roles,group_vars,host_vars}
cd mdma-ansible

# Create inventory file
cat > inventory/hosts.yml << 'EOF'
all:
  children:
    mdma_nodes:
      hosts:
        mdma-909:
          ansible_host: <pi-ip-address>
          ansible_user: mdma
          mdma_role: master
        # Add more nodes as needed
      vars:
        ansible_ssh_common_args: '-o StrictHostKeyChecking=no'
EOF
```

### Step 2: Base System Configuration Playbook

```bash
# Create base system playbook
cat > playbooks/base-system.yml << 'EOF'
---
- name: Configure MDMA Base System
  hosts: mdma_nodes
  become: yes
  tasks:
    - name: Update package database
      xbps:
        update_cache: yes

    - name: Install essential packages
      xbps:
        name:
          - git
          - curl
          - htop
          - neofetch
          - rustup
          - build-essential
          - pkg-config
          - cmake
          - python3-devel
        state: present

    - name: Configure hostname
      hostname:
        name: "{{ inventory_hostname }}"

    - name: Add hostname to /etc/hosts
      lineinfile:
        path: /etc/hosts
        line: "127.0.1.1 {{ inventory_hostname }}"
        insertafter: "127.0.0.1 localhost"

    - name: Set timezone
      file:
        src: /usr/share/zoneinfo/Europe/Stockholm
        dest: /etc/localtime
        state: link
        force: yes

    - name: Configure locale
      lineinfile:
        path: /etc/default/libc-locales
        line: "en_US.UTF-8 UTF-8"
        state: present
      notify: reconfigure locales

  handlers:
    - name: reconfigure locales
      command: xbps-reconfigure -f glibc-locales
EOF
```

### Step 3: Audio System Configuration Playbook

```bash
cat > playbooks/audio-system.yml << 'EOF'
---
- name: Configure Professional Audio System
  hosts: mdma_nodes
  become: yes
  tasks:
    - name: Install audio packages
      xbps:
        name:
          - alsa-utils
          - alsa-plugins
          - pipewire
          - wireplumber
          - pipewire-alsa
          - pipewire-pulse
          - pipewire-jack
          - rtkit
          - jack
          - qjackctl
        state: present

    - name: Create audio group configuration
      group:
        name: audio
        state: present

    - name: Add MDMA user to audio groups
      user:
        name: mdma
        groups: audio,wheel,video
        append: yes

    - name: Configure audio limits
      blockinfile:
        path: /etc/security/limits.conf
        block: |
          # Audio configuration for MDMA
          @audio   -  rtprio     95
          @audio   -  memlock    unlimited
          @audio   -  nice       -19

    - name: Enable PipeWire services for user
      become_user: mdma
      systemd:
        name: "{{ item }}"
        enabled: yes
        scope: user
      loop:
        - pipewire.service
        - pipewire-pulse.service
        - wireplumber.service

    - name: Configure PipeWire for low latency
      become_user: mdma
      copy:
        dest: /home/mdma/.config/pipewire/pipewire.conf.d/99-low-latency.conf
        content: |
          context.properties = {
              default.clock.rate = 48000
              default.clock.quantum = 128
              default.clock.min-quantum = 64
              default.clock.max-quantum = 512
          }
        mode: '0644'
      notify: restart pipewire

  handlers:
    - name: restart pipewire
      become_user: mdma
      systemd:
        name: pipewire.service
        state: restarted
        scope: user
EOF
```

### Step 4: Rust Development Environment Playbook

```bash
cat > playbooks/rust-environment.yml << 'EOF'
---
- name: Setup Rust Development Environment
  hosts: mdma_nodes
  become_user: mdma
  tasks:
    - name: Install Rust toolchain
      shell: |
        rustup-init -y --default-toolchain stable
        source ~/.cargo/env
        rustup component add rustfmt clippy
      args:
        creates: /home/mdma/.cargo/bin/rustc

    - name: Install cargo-watch for development
      shell: |
        source ~/.cargo/env
        cargo install cargo-watch
      args:
        creates: /home/mdma/.cargo/bin/cargo-watch

    - name: Clone MDMA repository
      git:
        repo: https://github.com/your-org/mdma.git  # Update with actual repo
        dest: /home/mdma/mdma
        version: main
      become: no

    - name: Create MDMA systemd service
      become: yes
      copy:
        dest: /etc/sv/mdma/run
        content: |
          #!/bin/sh
          cd /home/mdma/mdma
          exec chpst -u mdma:audio /home/mdma/.cargo/bin/cargo run --release --bin mdma-909 2>&1
        mode: '0755'
      notify: restart mdma

    - name: Create MDMA service directory
      become: yes
      file:
        path: /etc/sv/mdma
        state: directory

    - name: Enable MDMA service (but don't start yet)
      become: yes
      file:
        src: /etc/sv/mdma
        dest: /var/service/mdma
        state: link
      when: mdma_role == "master"

  handlers:
    - name: restart mdma
      become: yes
      command: sv restart mdma
      when: mdma_role == "master"
EOF
```

### Step 5: Hardware-Specific Configuration Playbook

```bash
cat > playbooks/hardware-config.yml << 'EOF'
---
- name: Configure Pi 5 Hardware for Audio
  hosts: mdma_nodes
  become: yes
  tasks:
    - name: Configure boot parameters for audio
      lineinfile:
        path: /boot/config.txt
        line: "{{ item }}"
        state: present
      loop:
        - "# Audio configuration for MDMA"
        - "dtparam=audio=on"
        - "audio_pwm_mode=2"
        - "disable_audio_dither=1"
        - "# GPU memory split for audio processing"
        - "gpu_mem=128"
      notify: reboot required

    - name: Install USB audio interface support
      xbps:
        name:
          - alsa-firmware
          - linux-firmware
        state: present

    - name: Configure USB audio device priority
      copy:
        dest: /etc/modprobe.d/alsa-base.conf
        content: |
          # Prefer USB audio over built-in
          options snd_usb_audio index=0
          options snd_bcm2835 index=1
        mode: '0644'
      notify: regenerate initramfs

    - name: Optimize kernel for real-time audio
      sysctl:
        name: "{{ item.key }}"
        value: "{{ item.value }}"
        sysctl_file: /etc/sysctl.d/99-audio.conf
      loop:
        - { key: "vm.swappiness", value: "10" }
        - { key: "fs.inotify.max_user_watches", value: "524288" }
        - { key: "kernel.sched_rt_runtime_us", value: "-1" }

  handlers:
    - name: reboot required
      debug:
        msg: "Reboot required for boot configuration changes"

    - name: regenerate initramfs
      command: dracut -f
EOF
```

### Step 6: Main Site Playbook

```bash
cat > site.yml << 'EOF'
---
- import_playbook: playbooks/base-system.yml
- import_playbook: playbooks/audio-system.yml
- import_playbook: playbooks/rust-environment.yml
- import_playbook: playbooks/hardware-config.yml
EOF
```

### Step 7: Ansible Configuration

```bash
cat > ansible.cfg << 'EOF'
[defaults]
inventory = inventory/hosts.yml
host_key_checking = False
timeout = 30
gathering = smart
fact_caching = memory

[ssh_connection]
ssh_args = -o ControlMaster=auto -o ControlPersist=60s
pipelining = True
EOF
```

## Phase 3: Deployment and Verification

### Step 1: Run Ansible Deployment

```bash
# Test connectivity
ansible all -m ping

# Deploy full configuration
ansible-playbook site.yml

# Reboot after initial deployment
ansible all -b -m reboot
```

### Step 2: Verify Audio System

```bash
# SSH back into Pi after reboot
ssh mdma@<pi-ip-address>

# Check audio devices
aplay -l
pactl info

# Test PipeWire is running
systemctl --user status pipewire
pw-top

# Check real-time capabilities
sudo -u mdma rtkit-test

# Verify MDMA service (if master node)
sudo sv status mdma
```

### Step 3: Performance Verification

```bash
# Check system performance
htop
free -h
df -h

# Test audio latency (with JACK)
jack_iodelay

# Verify network performance
iperf3 -c <other-node-ip>  # If you have multiple nodes
```

## Phase 4: MDMA Integration

### Step 1: Build and Test MDMA

```bash
# Build MDMA project
cd ~/mdma
cargo build --release

# Run initial tests
cargo test

# Start MDMA service
sudo sv start mdma  # On master nodes
```

### Step 2: Configure Audio Interface

```bash
# If using iFi zen DAC v3 via USB
# Device should be automatically detected

# If using HiFiBerry DAC+ Pro
sudo xbps-install -S linux-rpi5
# Add to /boot/config.txt:
echo "dtoverlay=hifiberry-dacplus" | sudo tee -a /boot/config.txt
sudo reboot

# Verify audio routing
aplay -D hw:0,0 /usr/share/sounds/alsa/Front_Left.wav
```

### Step 3: Network Configuration for Multi-Node Setup

```bash
# Configure static IP (optional but recommended)
cat > /etc/rc.conf << 'EOF'
HOSTNAME="mdma-909"
HARDWARECLOCK="UTC"
TIMEZONE="Europe/Stockholm"
KEYMAP="us"
FONT="ter-v16n"
EOF

# Add to Ansible for multiple nodes
# Update inventory with static IPs
# Configure firewall rules for MDMA ports
```

## Ongoing Management

### Ansible Maintenance Commands

```bash
# Update all packages
ansible all -b -m xbps -a "update_cache=yes upgrade=yes"

# Restart MDMA services
ansible mdma_nodes -b -m command -a "sv restart mdma"

# Deploy configuration changes
ansible-playbook site.yml --tags audio-system

# Check system status
ansible all -m setup -a "gather_subset=hardware"
```

### Adding New Nodes

1. Flash new SD card with Void Linux
2. Add to `inventory/hosts.yml`
3. Run `ansible-playbook site.yml --limit new-node`
4. Configure audio interfaces and network settings

### Backup and Recovery

```bash
# Backup configuration
ansible all -m fetch -a "src=/home/mdma/mdma/config dest=./backups/"

# Create SD card images
sudo dd if=/dev/sdX of=mdma-node-backup-$(date +%Y%m%d).img bs=4M

# Restore configurations
ansible-playbook site.yml  # Idempotent restoration
```

## Troubleshooting

### Serial Console Access

**If network/SSH fails, use serial console:**
```bash
# Connect via serial (see hardware setup above)
sudo screen /dev/ttyUSB0 115200

# Check network configuration
ip addr show
systemctl status dhcpcd  # or whatever network service

# Restart networking
sv restart dhcpcd
# or configure static IP:
# echo 'ip=192.168.1.100/24' >> /etc/rc.conf
```

**View boot logs via serial:**
```bash
# Serial console shows all boot messages
# Useful for debugging kernel panics, service failures
dmesg | tail -50
sv status
```

### Common Issues

**Serial console not working:**
- Verify 3.3V logic level adapter (5V will damage Pi!)
- Check wire connections (TX→RX, RX→TX, GND→GND)
- Ensure `enable_uart=1` in `/boot/config.txt`
- Try different baud rates: 9600, 38400, 115200

**Audio device not detected:**
```bash
sudo xbps-install -S alsa-firmware linux-firmware
sudo sv restart alsa
```

**PipeWire not starting:**
```bash
systemctl --user restart pipewire
systemctl --user restart wireplumber
```

**Permission issues:**
```bash
sudo usermod -a -G audio,realtime mdma
```

**Network connectivity:**
```bash
# Check network configuration
ip addr show
ping 8.8.8.8
# Access via serial if SSH fails
```

### Performance Optimization

**For real-time audio:**
```bash
# Install RT kernel (when available)
sudo xbps-install -S linux-rpi5-rt

# Optimize CPU governor
echo performance | sudo tee /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor
```

---

This migration guide provides a complete path from NixOS to a production-ready Void Linux + Ansible setup optimized for professional audio applications on Raspberry Pi 5. The declarative nature of Ansible configurations maintains infrastructure-as-code benefits while enabling the real-time audio capabilities essential for MDMA.