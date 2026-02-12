# Package Building and Distribution

## Philosophy: Deploy First

Code should run on live units within minutes of being written. This requires:
- Automated package builds on git push
- Minimal manual intervention
- Fast distribution to all units
- Safe rollback if issues arise

## Package vs Ansible Boundary

**Void Packages contain:**
- MDMA application binaries (Rust executables)
- Runtime dependencies
- runit service definitions
- Application configuration templates

**Ansible manages:**
- System topology (partitions, mounts)
- System packages (rarely change)
- Service configuration files
- Directory structures
- Secrets deployment

**The Principle**: If it changes with git commits, it's a package. If it defines how the system is shaped, it's Ansible.

## xbps-src Package Structure

MDMA uses `xbps-src` for building Void packages:

```
void-packages/
├── srcpkgs/
│   ├── mdma-909/
│   │   └── template
│   ├── mdma-101/
│   │   └── template
│   ├── mdma-303/
│   │   └── template
│   └── mdma-common/
│       └── template
```

### Package Template Example: mdma-909

```bash
# Template file
pkgname=mdma-909
version=0.1.0
revision=1
archs="aarch64"
build_style=cargo
hostmakedepends="cargo pkg-config"
makedepends="alsa-lib-devel"
depends="mdma-common alsa-utils avahi-daemon"
short_desc="MDMA main processing unit"
maintainer="Your Name <your@email>"
license="MIT"
homepage="https://github.com/yourusername/mdma"
distfiles="https://github.com/yourusername/mdma/archive/v${version}.tar.gz"
checksum="SKIP"  # Or actual checksum

post_install() {
    # Install runit service
    vsv mdma-909
    
    # Install configuration
    vinstall config/mdma-909.toml 644 etc/mdma
}
```

### runit Service Definition

Location: `srcpkgs/mdma-909/files/mdma-909/run`

```bash
#!/bin/sh
exec 2>&1
exec chpst -u mdma:mdma /usr/bin/mdma-909 --config /etc/mdma/mdma-909.toml
```

## GitHub Actions Build Pipeline

### Workflow File: `.github/workflows/build-packages.yml`

```yaml
name: Build MDMA Packages

on:
  push:
    branches: [main]
    tags: ['v*']
  pull_request:

jobs:
  build:
    runs-on: ubuntu-latest
    
    steps:
      - name: Checkout MDMA
        uses: actions/checkout@v4
        with:
          path: mdma
      
      - name: Checkout void-packages
        uses: actions/checkout@v4
        with:
          repository: void-linux/void-packages
          path: void-packages
      
      - name: Setup xbps-src
        run: |
          cd void-packages
          ./xbps-src binary-bootstrap
      
      - name: Copy package templates
        run: |
          cp -r mdma/packaging/srcpkgs/* void-packages/srcpkgs/
      
      - name: Build packages
        run: |
          cd void-packages
          ./xbps-src pkg mdma-common
          ./xbps-src pkg mdma-909
          ./xbps-src pkg mdma-101
          ./xbps-src pkg mdma-303
      
      - name: Collect packages
        run: |
          mkdir -p packages
          cp void-packages/hostdir/binpkgs/*.xbps packages/
      
      - name: Upload to GitHub Releases
        if: startsWith(github.ref, 'refs/tags/')
        uses: softprops/action-gh-release@v1
        with:
          files: packages/*.xbps
          token: ${{ secrets.GITHUB_TOKEN }}
      
      - name: Upload artifacts
        uses: actions/upload-artifact@v4
        with:
          name: mdma-packages
          path: packages/*.xbps
```

## Package Distribution Strategies

### Option 1: GitHub Releases (Simple)

**Pros:**
- No infrastructure needed
- Automatic versioning
- Easy to access from anywhere

**Cons:**
- Requires GitHub API for updates
- Not a "real" xbps repository
- Manual download and install

**Unit update process:**
```bash
# On MDMA unit
curl -LO https://github.com/user/mdma/releases/latest/download/mdma-909-0.1.0_1.aarch64.xbps
xbps-install -y mdma-909-0.1.0_1.aarch64.xbps
```

### Option 2: Local xbps Repository (Recommended)

**Pros:**
- Native xbps workflow (`xbps-install -Su`)
- Fast on local network
- Supports dependency resolution
- Can host on any HTTP server

**Cons:**
- Requires hosting infrastructure
- Repository maintenance needed

**Setup:**

1. **Create repository structure** (on dev machine or server):
```bash
mkdir -p /srv/mdma-repo/aarch64
```

2. **Copy packages to repository**:
```bash
cp void-packages/hostdir/binpkgs/*.xbps /srv/mdma-repo/aarch64/
```

3. **Generate repository index**:
```bash
cd /srv/mdma-repo
xbps-rindex -a aarch64/*.xbps
```

4. **Serve repository**:

Simple HTTP server:
```bash
cd /srv/mdma-repo
python3 -m http.server 8080
```

Or nginx configuration:
```nginx
server {
    listen 80;
    server_name repo.mdma.local;
    root /srv/mdma-repo;
    autoindex on;
}
```

5. **Configure units to use repository**:

Ansible template for `/etc/xbps.d/mdma-repo.conf`:
```
repository=http://repo.mdma.local/aarch64
```

6. **Update units**:
```bash
xbps-install -Su
```

### Option 3: GitHub Pages Repository (Hybrid)

**Pros:**
- Free hosting
- HTTPS by default
- Git-based updates
- No server maintenance

**Cons:**
- Public (unless private repo)
- Size limits (1GB recommended)

**Setup:**

1. **Create `gh-pages` branch in MDMA repo**
2. **GitHub Actions workflow** to publish packages:

```yaml
- name: Setup repository
  run: |
    mkdir -p gh-pages/aarch64
    cp packages/*.xbps gh-pages/aarch64/
    cd gh-pages
    xbps-rindex -a aarch64/*.xbps

- name: Deploy to GitHub Pages
  uses: peaceiris/actions-gh-pages@v3
  with:
    github_token: ${{ secrets.GITHUB_TOKEN }}
    publish_dir: ./gh-pages
```

3. **Configure units**:
```
repository=https://yourusername.github.io/mdma/aarch64
```

## Update Workflow

### Manual Update (Safe)

```bash
ssh mdma@mdma-909-studio.local

# Check for updates
xbps-install -Sn

# Review changes
xbps-install -Sun

# Apply updates (after confirming no audio playback)
sudo xbps-install -Su

# Verify health
mdma-health-check
```

### Automated Update (Deploy First Philosophy)

**mdma-update wrapper script** (shipped in mdma-common package):

```bash
#!/bin/bash
set -e

# Check if audio is active
if pgrep -f mdma-909 > /dev/null; then
    echo "⚠️  MDMA services running. Stop playback first."
    exit 1
fi

# Snapshot current package versions
xbps-query -l > /var/lib/mdma/snapshots/pre-update-$(date +%s).txt

# Update package database
xbps-install -S

# Check for updates
if xbps-install -Sn | grep -q mdma; then
    echo "📦 Updates available:"
    xbps-install -Sun | grep mdma
    
    # Apply updates
    xbps-install -Su
    
    echo "✅ Update complete"
    
    # Run health check
    if mdma-health-check; then
        echo "✅ Health check passed"
    else
        echo "❌ Health check failed - consider rollback"
        exit 1
    fi
else
    echo "✅ No updates available"
fi
```

Usage:
```bash
mdma-update
```

### Rollback Procedure

If an update breaks the system:

```bash
# View snapshot history
ls -lh /var/lib/mdma/snapshots/

# Check what changed
diff /var/lib/mdma/snapshots/pre-update-{previous}.txt \
     /var/lib/mdma/snapshots/pre-update-{latest}.txt

# Downgrade specific package
xbps-install -f mdma-909-0.0.9_1

# Or restore entire snapshot
while read pkg; do
    xbps-install -f "$pkg"
done < /var/lib/mdma/snapshots/pre-update-{previous}.txt
```

## Health Check Implementation

**mdma-health-check script**:

```bash
#!/bin/bash

EXIT_CODE=0

echo "🔍 MDMA Health Check"
echo "===================="

# Check audio devices
if aplay -l | grep -q "card"; then
    echo "✓ Audio devices detected"
else
    echo "✗ No audio devices found"
    EXIT_CODE=1
fi

# Check mounts
for mount in /music /metadata /cdj-export; do
    if mountpoint -q "$mount" 2>/dev/null; then
        echo "✓ $mount mounted"
    elif [ -d "$mount" ]; then
        echo "⚠ $mount exists but not mounted"
    fi
done

# Check services
for svc in mdma-909 avahi-daemon; do
    if sv status "$svc" >/dev/null 2>&1; then
        echo "✓ $svc running"
    else
        echo "✗ $svc not running"
        EXIT_CODE=1
    fi
done

# Check NFS export (909 only)
if [ -f /etc/exports ]; then
    if systemctl is-active nfs-server >/dev/null 2>&1; then
        echo "✓ NFS export active"
    else
        echo "✗ NFS export not active"
        EXIT_CODE=1
    fi
fi

# Check network
if avahi-browse -t _workstation._tcp | grep -q mdma; then
    echo "✓ mDNS beacon broadcasting"
else
    echo "✗ mDNS beacon not broadcasting"
    EXIT_CODE=1
fi

echo "===================="
if [ $EXIT_CODE -eq 0 ]; then
    echo "✅ All checks passed"
else
    echo "❌ Some checks failed"
fi

exit $EXIT_CODE
```

## Development Workflow

### Local Development → Live Deployment

1. **Write code** in Rust workspace
2. **Test locally** on development machine
3. **Commit and push** to main branch
4. **GitHub Actions** builds packages automatically
5. **Packages published** to repository (GitHub Pages or local)
6. **Units check for updates** (manual or cron)
7. **Apply update** with `mdma-update`
8. **Verify** with `mdma-health-check`

Time from git push to running on live units: **2-5 minutes**

### Emergency Hotfix Workflow

1. Create hotfix branch
2. Apply fix
3. Tag with version (e.g., `v0.1.1`)
4. Push tag → triggers build
5. SSH to unit: `mdma-update`
6. Verify fix applied

Time to live: **<60 seconds** after package built

## Monitoring Updates

### Cron-based Update Check

Ansible template for `/etc/cron.d/mdma-update-check`:

```cron
# Check for updates daily at 4 AM
0 4 * * * mdma /usr/bin/mdma-update-check-notify
```

Script notifies via log file or could integrate with notification system.

### Update Notification

Units can broadcast update availability via mDNS TXT records:

```bash
# Set TXT record when update available
avahi-set-host-name-alias "mdma-909-studio-update-available"
```

Discovery script on dev machine detects this and notifies user.

## Best Practices

1. **Never update during playback** - Check audio state first
2. **Always snapshot before update** - Enable rollback
3. **Run health checks after update** - Verify system stable
4. **Test on one unit first** - Don't update entire fleet simultaneously
5. **Keep local package cache** - Fast rollback if needed
6. **Version everything** - Git tags drive package versions
7. **Automate but don't auto-apply** - Require manual trigger or approval
