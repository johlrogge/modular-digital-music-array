# MDMA Roadmap

**Last updated:** January 12, 2026

## Current Status Summary

**Milestone 1 Part 1 (Pi Provisioning): 85% Complete** Ã¢ÂÂ³

NVMe partitioning âœ… and formatting âœ… complete with detailed per-partition planning. Next step: OS installation to NVMe.

---

## Milestone 1: The Installable Player (MVP)

**Goal:** Raspberry Pi 5 that plays your music collection with quality audio output.

**No custom hardware, no mixing capabilities - just great playback.**

### The 4-Part Critical Path

#### 1. Provision Raspberry Pi 5 Ã¢ÂÂ³ **IN PROGRESS** (85% Complete)

**Status:** Partitioning âœ… and formatting âœ… complete, OS installation next

**What works:**
- **Beacon provisioning (SD card setup):**
  - Flash vanilla Void Linux to SD card
  - Boot Pi, run setup script on live hardware
  - Beacon configures itself automatically
  - 100% reliable, tested on real Pi 5
  - 5-minute provisioning workflow
  
- **Network discovery:**
  - `just pi-scan` finds Pis on network with nmap
  - mDNS resolution working (`welcome-to-mdma.local`)
  - Auto-connect functionality

- **Beacon functionality:**
  - Web interface accessible at `http://welcome-to-mdma.local/`
  - Hardware detection working (Pi 5, RAM, NVMe drive)
  - All services running (beacon, dbus, avahi-daemon)
  - Runit supervision working correctly

- **NVMe partitioning:**
  - Partition detection and layout working
  - Partitions created: root, var, metadata, music
  - Metadata partition: 12 GB (corrected from 88 GB)
  - Music partition: 476 GB usable space

- **NVMe formatting:**
  - Intelligent formatting with per-partition status
  - Idempotent: skips already-formatted partitions
  - Handles mounted partitions gracefully
  - Clear plan showing what will be formatted vs skipped
  - All partitions verified after formatting

- **DevOps infrastructure:**
  - Package distribution via GitHub Pages
  - Void Linux packages (xbps) for all components
  - Automated builds via GitHub Actions
  - Local image debugging tools

**What's next:**

**A. Install Void Linux to NVMe (IMMEDIATE NEXT STEP)**

Stage 4 installation is currently stubbed. Need to implement:
- Mount all NVMe partitions to /mnt/mdma-install
- Install base-system package using xbps-install
- Configure /etc/fstab with all mount points
- Set hostname from user input
- Install SSH key from user input
- Unmount all partitions

**B. Configure Pi 5 for NVMe boot**
- Mount NVMe partitions
- Install base Void system to NVMe root
- Configure fstab with all mount points
- Set user-specified hostname in `/etc/hostname`
- Install user's public SSH key to `/root/.ssh/authorized_keys`
- Copy boot files to NVMe boot partition
- Update cmdline.txt to boot from NVMe root

**C. Configure Pi 5 for NVMe boot**
- Update EEPROM: `BOOT_ORDER=0xf416` (try NVMe first, fallback to SD)
- Verify boot configuration
- Document rollback procedure (SD card still bootable)

**D. First boot from NVMe**
- Reboot Pi
- Pi boots from NVMe
- Beacon starts from NVMe
- User navigates to `http://[user-hostname].local/`
- Provisioning complete!

**E. Development workflow (AFTER NVMe boot works)**
- `just deploy-pi` Ã¢â€ â€™ SCP latest binaries to Pi, restart services
- Fast iteration: code Ã¢â€ â€™ deploy Ã¢â€ â€™ test cycle
- No need to reflash entire system

**F. Verification testing**
- Gherkin-based test suite
- `just verify-pi` Ã¢â€ â€™ Runs automated checks
- Verifies:
  - Pi booted from NVMe
  - Beacon accessible at hostname.local
  - All partitions mounted
  - Services running
  - SSH access works

**Success criteria:**
- âœ… Can partition NVMe (DONE)
- âœ… Can format partitions (DONE - with intelligent per-partition planning)
- â³ Can install Void to NVMe
- â³ Pi boots from NVMe
- â³ Beacon runs from NVMe at user-specified hostname
- â³ SSH access with user's key
- â³ Development workflow (`just deploy-pi`) working
- â³ Verification tests passing

**Current completion:** 85% (partitioning and formatting done, installation next)

---

#### 2. Service Discovery Architecture Ã¢ÂÂ³ **PLANNED**

**Status:** Architecture designed, implementation after NVMe boot works

**Why after NVMe boot:** Services need to register with beacon. If beacon is on SD card during development, we'll have to reconfigure everything when switching to NVMe. Boot from NVMe first, THEN implement service discovery.

**Design principles:**
- **Device-level discovery** via UDP broadcast
- **Service-level details** via beacon HTTP API
- **Local service registration** with beacon
- **Modular architecture** - different devices announce different capabilities

**Architecture:**

```
Ã¢â€Å’Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€Â
Ã¢â€â€š  Network - UDP Discovery (port 42069)           Ã¢â€â€š
Ã¢â€â€š  "MDMA-909 at 192.168.1.100"                   Ã¢â€â€š
Ã¢â€â€Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€Â¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€Ëœ
                    Ã¢â€â€š
                    Ã¢â€â€š Found device, query for services
                    Ã¢â€â€š
Ã¢â€Å’Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€“Â¼Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€Â
Ã¢â€â€š  Beacon HTTP API (port 8080)                    Ã¢â€â€š
Ã¢â€â€š  GET /api/services                              Ã¢â€â€š
Ã¢â€â€š  Ã¢â€ â€™ [{library, playback, cdj-link}]              Ã¢â€â€š
Ã¢â€â€Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€Â¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€Ëœ
                    Ã¢â€â€š
                    Ã¢â€â€š Connect to specific services
                    Ã¢â€â€š
Ã¢â€Å’Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€“Â¼Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€Â
Ã¢â€â€š  Local Services (IPC/nng)                       Ã¢â€â€š
Ã¢â€â€š  - mdma-library    (localhost:5555)             Ã¢â€â€š
Ã¢â€â€š  - mdma-playback   (localhost:5556)             Ã¢â€â€š
Ã¢â€â€š  - mdma-cdj-link   (localhost:5557)             Ã¢â€â€š
Ã¢â€â€Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€Ëœ
```

**UDP Device Announcement:**

```rust
// Beacon announces the device
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceAnnouncement {
    pub device_type: DeviceType,     // "mdma-909", "mdma-101", "mdma-303"
    pub device_name: String,         // "Living Room"
    pub beacon_ip: String,           // "192.168.1.100"
    pub beacon_port: u16,            // 8080
    pub version: String,             // "0.1.0"
}

// Beacon announces on startup + responds to queries
impl Beacon {
    async fn announce_startup() {
        broadcast_udp(DeviceAnnouncement { ... });
    }
    
    async fn respond_to_query(from: SocketAddr) {
        send_udp_to(DeviceAnnouncement { ... }, from);
    }
}
```

**Beacon HTTP Service Registry:**

```rust
// Services register with local beacon
// POST /api/services/register
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceInfo {
    pub name: String,                // "library"
    pub port: u16,                   // 5555
    pub capabilities: Vec<String>,   // ["search", "browse", "metadata"]
    pub status: ServiceStatus,       // Running
}

// Clients query beacon for services
// GET /api/services
// Ã¢â€ â€™ [ServiceInfo, ServiceInfo, ...]

// Check specific service health
// GET /api/services/library/health
// Ã¢â€ â€™ { "status": "running" }
```

**Service Registration Example:**

```rust
// In mdma-library/src/main.rs
#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    // Start the library service
    let library = LibraryService::new().await?;
    
    // Register with local beacon
    let client = reqwest::Client::new();
    client.post("http://localhost:8080/api/services/register")
        .json(&ServiceInfo {
            name: "library".to_string(),
            port: 5555,
            capabilities: vec!["search".into(), "browse".into()],
            status: ServiceStatus::Running,
        })
        .send()
        .await?;
    
    // Run the service
    library.run().await?;
    Ok(())
}
```

**Client Discovery Flow:**

```rust
// In mdma-101/src/main.rs
async fn discover_909() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Discover devices via UDP
    let discovery = DiscoveryService::new().await?;
    discovery.query_devices().await?;
    
    // Listen for device announcements
    let device = discovery.wait_for_device(DeviceType::Mdma909).await?;
    
    // 2. Query beacon for available services
    let services: Vec<ServiceInfo> = reqwest::get(
        format!("http://{}:{}/api/services", device.beacon_ip, device.beacon_port)
    ).await?.json().await?;
    
    // 3. Find the service you need
    let library = services.iter()
        .find(|s| s.name == "library")
        .ok_or("No library service found")?;
    
    // 4. Connect to the service via IPC
    let library_url = format!("tcp://{}:{}", device.beacon_ip, library.port);
    connect_to_service(&library_url).await?;
    
    Ok(())
}
```

**Modular Capabilities by Device:**

```rust
// MDMA-909 announces:
{
  "device_type": "mdma-909",
  "device_name": "Living Room",
  "capabilities": ["library", "playback", "cdj-link"]
}

// MDMA-303 announces:
{
  "device_type": "mdma-303", 
  "device_name": "Bedroom",
  "capabilities": ["playback"]  // No library, just a satellite
}

// MDMA-101 announces:
{
  "device_type": "mdma-101",
  "device_name": "Controller",
  "capabilities": ["control"]  // Just a controller
}
```

**Implementation plan:**

1. **Create `mdma-discovery` component** (after NVMe boot works)
   - UDP device announcement/query
   - Message types: `DeviceAnnouncement`, `Query`
   - Broadcast and unicast support

2. **Add service registry to beacon**
   - `HashMap<String, ServiceInfo>` for local services
   - HTTP endpoints: `/api/services/*`
   - Service registration, listing, health checks

3. **Services register at startup**
   - Each service POSTs to beacon on startup
   - Periodic re-registration (heartbeat)
   - Beacon marks stale services

4. **Client libraries**
   - Helper functions for discovery
   - Device finding, service querying
   - Connection establishment

**Success criteria:**
- Beacon announces device via UDP
- Services register with beacon via HTTP
- Clients discover devices and query for services
- Different device types announce different capabilities
- Foundation for modular MDMA ecosystem

**Not started yet - blocked by:** NVMe boot completion

---

#### 3. Sync Music (ACID + Crawlers)

**Status:** Architecture designed, implementation after service discovery

**Music sources:**
- YouTube Music (priority - streaming service integration)
- Bandcamp (you already sync this manually)
- Beatport (you buy tracks here)

**Technical approach:**
- ACID (Audio Collection Indexing Database) with immutable fact streams
- SHA256 content hashing for deduplication
- Crawlers for each music source
- YT-DLP for YouTube Music downloads
- Stainless-facts as underlying storage

**Service architecture:**
```rust
// mdma-library service registers with beacon
{
  "name": "library",
  "port": 5555,
  "capabilities": ["search", "browse", "metadata", "download"]
}

// Clients discover 909, query beacon, connect to library service
```

**Component structure:**
- `mdma-library` service for library management
- Separate processes for crawler, fingerprinting, aggregation
- IPC communication over nng
- Persistent storage on NVMe `/music` and `/metadata` partitions

**Success criteria:**
- Music appears in ACID database automatically
- Duplicates detected across sources
- Metadata preserved and queryable
- Library survives schema evolution (thanks to immutable facts)
- Library service registered with beacon

**Not started yet - blocked by:** Service discovery implementation

---

#### 4. Audio Playback

**Status:** Not started

**Output stack progression:**
- Phase 1: 3.5mm Ã¢â€ â€™ RCA (basic validation)
- Phase 2: USB DAC support (iFi zen BLUE v3 Bluetooth - your reference)
- Phase 3: High-end DACs (iFi, Fosi Audio, etc.)

**Service architecture:**
```rust
// mdma-playback service registers with beacon
{
  "name": "playback",
  "port": 5556,
  "capabilities": ["play", "pause", "skip", "volume", "gapless"]
}
```

**Technical approach:**
- `mdma-playback` user/service
- ALSA/PipeWire for audio routing
- Gapless playback support
- Volume control

**Success criteria:**
- Music plays from Raspberry Pi
- Audio quality matches or exceeds phone playback
- Volume control works
- Gapless playback between tracks
- Playback service registered with beacon

**Not started yet - blocked by:** Music library (Part 3)

---

#### 5. User Interface

**Status:** Not started

**Discovery-enabled UI:**
```rust
// UI discovers 909 via service discovery
let devices = discover_mdma_devices().await;
let device = devices.iter().find(|d| d.device_type == DeviceType::Mdma909);

// Query beacon for services
let services = query_beacon_services(device).await;

// Connect to library for browsing
connect_to_service(services.library).await;

// Connect to playback for control
connect_to_service(services.playback).await;
```

**Options being evaluated:**
- Web interface (fastest to implement, works from any device)
- Bevy desktop app (macOS and Linux native)

**Design constraint:** Build as if the 101 hardware exists
- Consider jog wheel navigation
- Think small screen constraints
- Plan for tactile control
- ACID designed for this - UI can evolve freely

**Success criteria:**
- Can browse music library
- Can play/pause/skip tracks
- Can see what's currently playing
- Responsive enough for party use
- Discovers 909 automatically

**Not started yet - blocked by:** Playback working (Part 4)

---

### Milestone 1 Complete When:

- ÃƒÂ¢Ã…â€œÃ¢â‚¬Â¦ Can provision a Pi from scratch (75% done - partitioning works)
- ÃƒÂ¢Ã‚Â³ Pi boots from NVMe with user-specified hostname
- ÃƒÂ¢Ã‚Â³ Service discovery architecture implemented
- ÃƒÂ¢Ã‚Â³ Music syncs from at least YouTube Music
- ÃƒÂ¢Ã‚Â³ Audio plays through at least 3.5mm output
- ÃƒÂ¢Ã‚Â³ Can control playback through some interface
- ÃƒÂ¢Ã‚Â³ System is stable enough to use at a party

**User value unlocked:** No more jarring Spotify cuts during gatherings. Professional playback from dedicated hardware.

**Current completion:** ~20% overall (NVMe provisioning in progress, other parts pending)

---

## Development Workflow

**Prerequisites:**
- Provisioned Pi on network
- SSH access configured
- Pi accessible at `[hostname].local`

**Fast iteration cycle:**

```bash
# Deploy latest code to Pi
just deploy-pi [hostname]

# What it does:
# 1. Build release binaries
# 2. SCP to Pi
# 3. Restart affected services via runit
# 4. Tail logs

# Example
just deploy-pi living-room
```

**Verification testing:**

```bash
# Run automated verification suite
just verify-pi [hostname]

# Uses Gherkin-style tests to verify:
# - Pi is reachable
# - Booted from NVMe
# - All partitions mounted correctly
# - Beacon accessible
# - Services running
# - SSH access works
```

**Development flow:**
1. Make code changes locally
2. `just deploy-pi` Ã¢â€ â€™ Deploy to Pi
3. Test changes on real hardware
4. `just verify-pi` Ã¢â€ â€™ Run verification suite
5. Iterate

**No reflashing needed** - just redeploy binaries and restart services.

---

## Testing Strategy

**Gherkin verification suite** (`just verify-pi [hostname]`):

```gherkin
Feature: MDMA Pi Provisioning
  Scenario: Pi boots from NVMe
    Given a provisioned Pi at "[hostname].local"
    When I check the root filesystem
    Then it should be mounted from /dev/nvme0n1p2
    
  Scenario: All partitions mounted
    Given a provisioned Pi
    When I check mount points
    Then /boot should be mounted from /dev/nvme0n1p1
    And /var should be mounted from /dev/nvme0n1p3
    And /music should be mounted from /dev/nvme0n1p4
    And /metadata should be mounted from /dev/nvme0n1p5
    
  Scenario: Beacon is accessible
    Given a provisioned Pi
    When I navigate to http://[hostname].local/
    Then I should see the beacon interface
    And hardware should be detected
    
  Scenario: SSH access works
    Given a provisioned Pi
    When I SSH to [hostname].local
    Then I should connect successfully with my key
    
  Scenario: Services are running
    Given a provisioned Pi
    When I check runit service status
    Then beacon should be running
    And dbus should be running
    And avahi-daemon should be running
```

**Implementation:**
- Cucumber-rs or similar Gherkin runner
- SSH into Pi to run checks
- HTTP requests to verify beacon
- Exit codes indicate pass/fail
- Runs in CI for golden image validation

---

## Milestone 2: CDJ Integration

**Goal:** Serve music library to Pioneer CDJ equipment via Pro DJ Link protocol.

**Why:** Validates real DJ workflow, enables beta testing with someone who has a CDJ.

**Status:** Protocol reverse-engineering started, on hold pending Milestone 1 completion

### Requirements

#### CDJ Compatibility (Priority Order)
1. **CDJ-2000** - Most important, your main unit
2. **CDJ-900** - Secondary priority  
3. **CDJ-2000 Nexus** - Least important (newer protocol)

#### Service Architecture

```rust
// mdma-cdj-link service registers with beacon
{
  "name": "cdj-link",
  "port": 5557,
  "capabilities": ["pro-dj-link", "nfs-export", "waveform", "metadata"]
}
```

**Technical Implementation:**

- MDMA acts as virtual CDJ on network
- Serves tracks to physical Pioneer equipment
- Maintains Pro DJ Link compatibility
- Waveform generation for CDJ displays
- AIFF/WAV transcoding (on secondary NVMe for MDMA-909)

**Technical approach:**
- `mdma-cdj-link` service
- NFS export for CDJ access
- `/cdj-export` partition on secondary NVMe (future)
- Real-time transcoding as needed

**Success criteria:**
- CDJ recognizes MDMA as media source
- Can browse library from CDJ interface
- Can load and play tracks
- Waveforms display correctly
- Beat grid alignment works

### Beta Testing Validation

**Beta tester discovers 909:**
```rust
// CDJ sees MDMA-909 on Pro DJ Link network
// User can also use 101 controller
// Both use service discovery to find the 909
```

**What we're testing:**
- Does CDJ integration actually work in practice?
- Is library management good enough for DJ workflow?
- Are there dealbreaker UX issues?
- Does this beat the phone experience?

### Milestone 2 Complete When:
- CDJ-2000 can browse MDMA library
- Can load and play tracks from MDMA
- System is stable enough for practice session
- Beta tester validates the experience

**User value unlocked:** Professional DJ equipment without USB stick management. Your curated library available on CDJs.

**Not started yet - blocked by:** Milestone 1 completion

---

## Beyond Milestone 2: Future Vision

**Not prioritized yet - noted for context:**

### Milestone 3: Basic Mixing (Automated)
- Intelligent beatmatching
- Automatic transitions
- "Offline mix preparation" from couch
- Handoff between auto and manual modes
- Detects when DJ takes over manually

### Milestone 4: MDMA-101 Hardware
- Jog wheel controller with force feedback (optional)
- Dedicated screen (OLED)
- Physical cueing interface
- Browser/controller in one unit
- High-resolution encoder technologies
- Uses service discovery to find 909

### Milestone 5: Multi-room Expansion
- Multiple MDMA-303 satellite nodes
- Synchronized playback across rooms
- Different music in different zones
- Network distribution
- 303s register with beacon as playback-only devices

### Milestone 6: Network Effects
- Friend network deployment
- Shared library discovery (via service discovery)
- Collaborative playlist building
- Community features

---

## Strategic Principles

### Deploy First Philosophy
Code runs on live hardware within minutes of being written. Rapid iteration from development to live units. Sub-5-minute deployment times via `just deploy-pi`.

### Test on Real Hardware First
Chroot and QEMU don't match real Pi behavior. Setup script on live Pi > golden images during development. Validate before automating. Gherkin tests verify real hardware.

### Iterative Development
Small steps. Get one thing working, then iterate. Ask: "Can this one step be done in two steps?" Don't pursue perfection before proving the concept.

### Modular Architecture via Service Discovery
Beacon announces device-level capabilities. Services register locally with beacon. Clients discover devices, query for services, connect via IPC. Different device types announce different capabilities - this IS the modularity.

### ACID as Foundation
Immutable fact streams enable evolution without breaking collected data. Build the data model right, iterate on interfaces forever. UI can change, data persists.

### Incremental CDJ Strategy
Start with serving, add displays, implement analysis. Don't build everything at once - prove each piece works.

### Type-Driven Safety
Rust's type system prevents illegal states. Use compile-time validation to eliminate runtime errors. Type-state pattern for workflows.

### Separation of Concerns
- SD card: Minimal bootloader/beacon (long-lived)
- NVMe: Runtime operations, OS, high-write activities
- Music/Metadata: Separate partitions, survive OS reinstalls

---

## Current Focus

**Immediate priority:** Complete NVMe provisioning (format, install, boot)

**What's critical right now:**
1. Format the NVMe partitions
2. Install Void Linux to NVMe
3. Configure hostname and SSH keys
4. Update EEPROM for NVMe boot
5. Verify first boot from NVMe
6. Set up development workflow (`just deploy-pi`)
7. Create verification test suite (`just verify-pi`)

**What comes after NVMe boot:**
1. Implement service discovery architecture
2. Beacon announces device via UDP
3. Beacon provides service registry HTTP API
4. Foundation ready for actual services

**What's not a priority right now:**
- Golden images (setup script + deploy works)
- Actual music services (need discovery first)
- UI development (need services first)

**Philosophy:** Get NVMe boot 100% right, then service discovery, then actual features. The foundation must be rock-solid.

---

## Blockers

**Previously resolved:**
- ÃƒÂ¢Ã…â€œÃ¢â‚¬Â¦ mDNS/Avahi not broadcasting - **FIXED**
  - Solution: Explicit package installation (dbus, avahi, beacon)
  - Supervise symlinks required for runit services
  - Timing: Sleep 5 seconds after enabling services

**Current:**
- None! Partitioning works, formatting is next step

**Watch for:**
- EEPROM update issues (test rollback to SD boot)
- NVMe compatibility (some drives need firmware updates)
- Hostname propagation (mDNS needs to pick up new hostname)
- YT-DLP rate limiting (Part 3)
- DAC driver support (Part 4)
- Pro DJ Link protocol gaps (Milestone 2)

---

## Time Estimates

| Task | Time |
|------|------|
| Format NVMe partitions | 30 minutes |
| Install Void to NVMe | 2-3 hours |
| Configure boot from NVMe | 1-2 hours |
| EEPROM update and testing | 1 hour |
| First boot verification | 30 minutes |
| Development workflow setup | 1-2 hours |
| Gherkin test suite | 2-3 hours |
| **Total for NVMe completion** | **8-12 hours** |
| | |
| Service discovery component | 3-4 hours |
| Beacon service registry | 2-3 hours |
| UDP device announcements | 1-2 hours |
| Client discovery library | 2-3 hours |
| **Total for service discovery** | **8-12 hours** |

---

## Hardware Configurations

### MDMA-909 (Main Processing Unit)
**Primary NVMe (512GB):**
- `/boot` (512MB) - Boot partition
- `/` (16GB) - Operating system
- `/var` (8GB) - Logs, journals
- `/music` (400GB) - FLAC library
- `/metadata` (88GB) - ACID fact streams

**Announces as:**
```json
{
  "device_type": "mdma-909",
  "capabilities": ["library", "playback", "cdj-link"]
}
```

**Secondary NVMe (512GB) - Future:**
- `/cdj-export` (512GB) - AIFF/WAV cache for CDJ access via NFS

### MDMA-101 (Browser/Controller)
**Single NVMe (512GB):**
- Same partition layout as 909
- Plus hardware interface (jog wheel, screen)
- Force feedback rotary controller (optional)

**Announces as:**
```json
{
  "device_type": "mdma-101",
  "capabilities": ["control", "browse"]
}
```

### MDMA-303 (Satellite Node)
**Single NVMe (512GB):**
- Minimal install
- Network audio streaming
- Multi-room distribution

**Announces as:**
```json
{
  "device_type": "mdma-303",
  "capabilities": ["playback"]
}
```

---

## Update History

- **2026-01-03:** Major architecture update
  - Corrected current state: partitioning works
  - Clarified next steps: formatting, OS installation, boot config
  - Added service discovery architecture (to be implemented after NVMe boot)
  - Added development workflow with `just deploy-pi`
  - Added Gherkin verification testing strategy
  - Emphasized modular architecture via beacon service registry
  - Updated completion estimates realistically

- **2025-12-16:** Milestone 1 Part 1 completion claimed (premature)
  - Beacon working with setup script
  - Network discovery operational
  - NVMe detected but not yet formatted/installed

- **2025-12-07:** Major progress on beacon
  - Bootable image creation working
  - Beacon operational on real Pi 5
  - Web interface accessible

- **2024-12-06:** Initial roadmap created

---

## Next Session Recommendations

**Immediate actions:**

1. **Format NVMe partitions** (30 min)
   - Verify partition layout
   - Format each partition with appropriate filesystem
   - Test mount/unmount

2. **Install Void Linux to NVMe** (2-3 hours)
   - Mount all partitions
   - Install base system
   - Configure fstab
   - Set hostname from user input
   - Install SSH key from user input
   - Copy boot files

3. **Configure NVMe boot** (1-2 hours)
   - Update EEPROM boot order
   - Update cmdline.txt
   - Verify configuration
   - Document rollback procedure

4. **Test first boot** (30 min)
   - Reboot Pi
   - Verify boot from NVMe
   - Verify hostname.local resolves
   - Verify SSH access
   - Verify beacon accessible

5. **Set up development workflow** (1-2 hours)
   - Create `just deploy-pi` target
   - Test SCP deployment
   - Test service restart
   - Document workflow

6. **Create verification tests** (2-3 hours)
   - Write Gherkin feature files
   - Implement test steps
   - Run against live Pi
   - Integrate into `just verify-pi`

**After NVMe boot is solid:**
- Implement service discovery architecture
- Add beacon service registry endpoints
- Create `mdma-discovery` component
- Test device announcements and service queries

**Current recommendation:** Focus 100% on NVMe boot completion. Once that's rock-solid, service discovery will be straightforward to add.
