# MDMA Roadmap

**Last updated:** February 18, 2026

## Current Status Summary

**Milestone 1 Part 1 (Pi Provisioning): 100% COMPLETE**

NVMe boot working with kernel sync. mdma-console stub deployed (port 3000). `just deploy-dev` and `just deploy-console` working.

**Milestone 1 Part 2 (Music Library): ~60% Complete**

mdma-library service operational with nng IPC. mdma-cli tool for testing. Partial hash matching, facts display, stainless-facts locking fixed.

**Bandcamp integration: OPERATIONAL on Pi**

mdma-bandcamp service running on Pi. Syncing 114+ item collection. ZIP extraction working. Files landing in `/music/inbox`. Library ingestion pending trigger.

---

## Milestone 1: The Installable Player (MVP)

**Goal:** Raspberry Pi 5 that plays your music collection with quality audio output.

**No custom hardware, no mixing capabilities - just great playback.**

### The 4-Part Critical Path

#### 1. Provision Raspberry Pi 5 - **COMPLETE**

**Status:** Fully operational

**What works:**
- **Beacon provisioning (SD card setup):**
  - Flash vanilla Void Linux to SD card
  - Boot Pi, run setup script on live hardware
  - Beacon configures itself automatically
  - 100% reliable, tested on real Pi 5
  - 5-minute provisioning workflow

- **Network discovery:**
  - `just pi-scan` finds Pis on network with nmap
  - mDNS resolution working (`mdma-909.local`)
  - Auto-connect functionality

- **Beacon functionality:**
  - Web interface accessible at `http://mdma-909.local/`
  - Hardware detection working (Pi 5, RAM, NVMe drive)
  - All services running (beacon, dbus, avahi-daemon)
  - Runit supervision working correctly

- **NVMe partitioning and formatting:**
  - All partitions created and formatted
  - Metadata partition: 12 GB
  - Music partition: 476 GB usable space

- **NVMe boot:**
  - Pi boots from NVMe
  - Kernel sync working
  - Beacon runs from NVMe at user-specified hostname
  - SSH access with user's key

- **DevOps infrastructure:**
  - Package distribution via GitHub Pages
  - Void Linux packages (xbps) for all components
  - Automated builds via GitHub Actions
  - `just deploy-dev` for rapid iteration

**Success criteria:** ALL MET

---

#### 2. Music Library - **IN PROGRESS** (~40% Complete)

**Status:** Core infrastructure operational, integration with mdma-console pending

**What works:**

- **mdma-library service:**
  - Runs as daemon with nng IPC interface
  - Listens on `ipc:///tmp/mdma-test/run/library.sock` (dev) or `/run/mdma/library.sock` (prod)
  - Handles requests: Ping, GetStatus, ListTracks, GetTrack, GetFacts, Search, IngestFile

- **Typestate ingestion pipeline:**
  - InboxFile -> ValidatedAudio -> ExtractedTrack -> IndexedTrack
  - Content-addressed blob storage (`/music/blobs/{hash[0:2]}/{hash}.flac`)
  - SHA256 content hashing for deduplication
  - Metadata extraction from FLAC files

- **Fact storage via stainless-facts:**
  - Append-only fact stream at `/metadata/facts.jsonl`
  - Fixed locking: writer releases lock between writes, readers can access concurrently
  - Facts loaded into memory on startup for fast search

- **mdma-cli tool:**
  - `mdma tracks` - List all tracks in library
  - `mdma info <hash>` - Show track details (supports partial hashes like git)
  - `mdma facts <hash>` - Show all facts for a track with nice formatting

- **Display trait for MusicValue:**
  - Human-readable formatting (e.g., "5:32" for duration, "24.68 MB" for file size)
  - variant_name() for fact type display (e.g., "Duration", "FileSize")

**Service architecture:**
```
mdma-library (daemon)
    |
    +-- nng IPC (ipc:///run/mdma/library.sock)
    |
    +-- /metadata/facts.jsonl (stainless-facts)
    |
    +-- /music/blobs/ (content-addressed storage)

mdma-cli (client) <-- nng IPC --> mdma-library
mdma-console (future) <-- nng IPC --> mdma-library
```

**What works (Bandcamp integration):**

- **mdma-bandcamp service:**
  - Async NNG IPC server (nng blocking I/O bridged to tokio via channels)
  - Listens on `ipc:///run/mdma/bandcamp.sock` or TCP
  - Handles: Ping, GetStatus, ReloadCookies, Sync, ListDownloads, CancelDownload, PauseAll, ResumeAll
  - Cookie file from `/var/lib/mdma-bandcamp/cookies.txt` (Netscape/browser export format)

- **Download pipeline:**
  - Syncs Bandcamp collection (tested with 114 items)
  - Magic-byte detection: ZIPs extracted to inbox, single FLACs renamed to `Artist - Title.flac`
  - Staging at `/music/downloads/` (not inbox), atomic move on completion
  - Track-oriented cache at `/var/lib/mdma-bandcamp/bandcamp.cache`

- **nng_transport component:**
  - Shared `connect(address)` used by both library-ipc-client and bandcamp-ipc-client
  - mDNS resolution via `avahi-resolve -4 -n` for `.local` hostnames (NNG can't resolve DNS)

- **CLI env vars:**
  - `MDMA_LIBRARY_SOCKET` and `MDMA_BANDCAMP_SOCKET` for easy Pi connection

**What's next:**

1. **Trigger library ingestion** - Run `mdma ingest-all` after Bandcamp sync fills inbox
2. **Atomic ZIP extraction** - Currently ZIP entries are written directly to inbox; should stage each file to `/music/downloads/{filename}.extract` first, then `rename()` into inbox (same pattern as single tracks). Confirmed needed: ZIPs are used for all multi-track albums.
3. **Inbox watcher** - inotify-based auto-ingest (so library stays up-to-date automatically)
4. **HTTP upload in mdma-console** - POST endpoint writes to inbox
5. **Library browsing in mdma-console** - Read from mdma-library via nng

**Success criteria:**
- Can drop files in inbox -> they appear in library
- Can upload via web UI -> they appear in library
- Can browse library in mdma-console
- Duplicates detected and skipped

**Current completion:** 40% (core service working, UI integration pending)

---

#### 3. Audio Playback - **PLANNED**

**Status:** Not started, blocked by music library completion

**Output stack progression:**
- Phase 1: 3.5mm -> RCA (basic validation)
- Phase 2: USB DAC support (iFi zen BLUE v3 Bluetooth - your reference)
- Phase 3: High-end DACs (iFi, Fosi Audio, etc.)

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

**Not started yet - blocked by:** Music library UI (Part 2)

---

#### 4. User Interface - **PLANNED**

**Status:** Stub deployed (mdma-console on port 3000), needs library integration

**Options being evaluated:**
- Web interface (fastest to implement, works from any device)
- Bevy desktop app (macOS and Linux native, future)

**Success criteria:**
- Can browse music library
- Can play/pause/skip tracks
- Can see what's currently playing
- Responsive enough for party use

**Not started yet - blocked by:** Playback working (Part 3)

---

### Milestone 1 Complete When:

- [x] Can provision a Pi from scratch
- [x] Pi boots from NVMe with user-specified hostname
- [ ] Music syncs from at least one source (inbox working, bandcamp pending)
- [ ] Can browse library in web UI
- [ ] Audio plays through at least 3.5mm output
- [ ] Can control playback through some interface
- [ ] System is stable enough to use at a party

**User value unlocked:** No more jarring Spotify cuts during gatherings. Professional playback from dedicated hardware.

**Current completion:** ~35% overall (provisioning complete, library core working, UI and playback pending)

---

## Development Workflow

**Prerequisites:**
- Provisioned Pi on network
- SSH access configured
- Pi accessible at `mdma-909.local`

**Fast iteration cycle:**

```bash
# Deploy library service to Pi
just deploy-dev

# Deploy console to Pi
just deploy-console

# Test locally with test data
just test-library
```

**Development flow:**
1. Make code changes locally
2. `just deploy-dev` -> Deploy to Pi
3. Test changes on real hardware
4. Iterate

**No reflashing needed** - just redeploy binaries and restart services.

---

## Component Architecture

```
music_facts        = Types only (MusicValue, FactSource, ContentHash, newtypes)
stainless_facts    = Generic fact stream operations (external crate)
mdma-library       = Library SERVICE with nng IPC interface (the brain)
mdma-console       = HTTP frontend, talks to mdma-library via nng
mdma-cli           = CLI frontend, talks to mdma-library via nng
beacon             = Provisioning and service discovery
```

**Storage layout:**
```
/music/
    inbox/              # Drop files here (watched by mdma-library)
    downloads/          # Staging area for in-progress downloads (NOT inbox)
    blobs/              # Content-addressed storage
        a1/
            b2c3d4...sha256.flac

/metadata/
    facts.jsonl         # Main fact stream (source of truth)

/var/lib/mdma-bandcamp/
    cookies.txt         # Bandcamp session cookies (Netscape format)
    bandcamp.cache      # Track-oriented download cache

/etc/mdma/
    bandcamp-cookies.json  # Alternative location (Cookie Quick Manager format)
```

---

## Milestone 2: CDJ Integration

**Goal:** Serve music library to Pioneer CDJ equipment via Pro DJ Link protocol.

**Status:** On hold pending Milestone 1 completion

**Requirements:**
- CDJ-2000 compatibility (priority)
- NFS export for CDJ access
- Waveform generation
- Beat grid alignment

**Not started yet - blocked by:** Milestone 1 completion

---

## Beyond Milestone 2: Future Vision

**Not prioritized yet - noted for context:**

### Architecture: Unified IPC Gateway (Future)

**Problem:** As more microservices are added (mdma-library, mdma-bandcamp, future playback, CDJ, etc.), each service exposes its own port/socket. External clients (CLI, console, remote) must know each service's address individually. This doesn't scale and complicates firewall/network setup.

**Solution:** A single MDMA gateway service that:
- Exposes **one public port** (e.g., `tcp://mdma-909.local:5555`)
- Microservices **register** on a local bus (IPC socket only, not exposed externally)
- Gateway routes requests to the appropriate service by type/namespace
- Clients are discoverable and addressable via the public interface

```
External clients
       |
       ▼
┌─────────────────┐
│  mdma-gateway   │  (single public port: 5555)
│  (router/proxy) │
└────────┬────────┘
         │ local IPC bus
    ┌────┴─────────────────────────┐
    │         │                   │
    ▼         ▼                   ▼
mdma-library  mdma-bandcamp  mdma-playback
(registered)  (registered)   (registered)
```

**Benefits:**
- One port to forward/expose
- Services can come and go without client reconfiguration
- Natural place for auth, rate limiting, observability
- Enables future service mesh for multi-room setups

**When to implement:** After Milestone 1 complete and multiple services are running on the Pi simultaneously.

### Milestone 3: Basic Mixing (Automated)
- Intelligent beatmatching
- Automatic transitions
- Handoff between auto and manual modes

### Milestone 4: MDMA-101 Hardware
- Jog wheel controller with force feedback
- Dedicated screen (OLED)
- Physical cueing interface

### Milestone 5: Multi-room Expansion
- Multiple MDMA-303 satellite nodes
- Synchronized playback across rooms

### Milestone 6: Network Effects
- Shared library discovery
- Collaborative playlist building

---

## Strategic Principles

### Deploy First Philosophy
Code runs on live hardware within minutes of being written.

### Test on Real Hardware First
Chroot and QEMU don't match real Pi behavior.

### Iterative Development
Small steps. Get one thing working, then iterate.

### Stainless Facts as Foundation
Immutable fact streams enable evolution without breaking collected data.

### Type-Driven Safety
Rust's type system prevents illegal states.

---

## Current Focus

**Immediate priority:** Close the library ingestion loop (Bandcamp → inbox → indexed)

**What's critical right now:**
1. Trigger `mdma ingest-all` to index the 200+ tracks already in `/music/inbox`
2. Verify end-to-end: Bandcamp download → inbox → library indexed
3. Add inbox watcher so future downloads auto-ingest

**What comes after:**
1. Connect mdma-console to mdma-library (browse library in web UI)
2. Audio playback service
3. Unified IPC gateway (future architecture, see Future Vision section)

**Philosophy:** Music is in the inbox. Close the loop before adding more features.

---

## Update History

- **2026-02-18:** Bandcamp integration operational
  - mdma-bandcamp service running on Pi (async NNG + tokio)
  - 114-item Bandcamp collection syncing successfully
  - ZIP extraction for albums, single FLAC rename for tracks
  - nng_transport component extracted (shared mDNS resolution via avahi)
  - MDMA_LIBRARY_SOCKET / MDMA_BANDCAMP_SOCKET env vars for CLI
  - Future architecture: unified IPC gateway (one public port, services register locally)

- **2026-02-12:** Part 2 progress update
  - mdma-library service fully operational
  - mdma-cli tool for testing
  - Partial hash matching (like git short refs)
  - Facts command with Display trait formatting
  - Fixed stainless-facts locking for concurrent read/write
  - Removed facts_cache workaround

- **2026-01-12:** Milestone 1 Part 1 complete
  - NVMe boot working with kernel sync
  - mdma-console stub deployed
  - Development workflow operational

- **2026-01-03:** Major architecture update
  - Service discovery architecture designed
  - Development workflow with `just deploy-pi`
  - Gherkin verification testing strategy

- **2025-12-16:** Beacon operational on real Pi 5

---

## Next Session Recommendations

**Immediate actions:**

1. **Trigger library ingestion** (5 min)
   ```bash
   export MDMA_LIBRARY_SOCKET="tcp://mdma-909.local:5555"
   mdma ingest-all
   # Verify: mdma tracks
   ```

2. **Inbox watcher** - inotify-based auto-ingest
   - Auto-trigger ingest pipeline when files land in inbox
   - Eliminates manual `ingest-all` trigger after each sync

3. **Add HTTP upload to mdma-console**
   - POST /upload endpoint writes to /music/inbox/
   - Triggers ingest after upload

4. **Add library browsing to mdma-console**
   - Connect to mdma-library via nng
   - Display track list, track detail page with facts

**Current recommendation:** Trigger `ingest-all` first to prove the full pipeline end-to-end, then add the inbox watcher to automate it.
