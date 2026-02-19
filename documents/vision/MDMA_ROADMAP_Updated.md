# MDMA Roadmap

**Last updated:** February 19, 2026

## Current Status Summary

**Milestone 1 Part 1 (Pi Provisioning): 100% COMPLETE**

NVMe boot working with kernel sync. mdma-console stub deployed (port 3000). `just deploy-dev` and `just deploy-console` working.

**Milestone 1 Part 2 (Music Library): ~60% Complete**

mdma-library service operational with nng IPC. 329 tracks indexed. Bandcamp sync operational. `by-artist` symlink tree for browsing. mdma-cli for search/list/facts from laptop.

**Milestone 1 Part 3 (Audio Playback): FIRST PLAYBACK ACHIEVED**

End-to-end playback proven on Feb 19, 2026: laptop CLI -> TCP -> Pi library -> playback engine -> PipeWire -> iFi USB DAC. Audio plays but has known bugs (speed, stop command). Cross-compilation pipeline working with aarch64 PipeWire sysroot.

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

#### 3. Audio Playback - **IN PROGRESS** (First playback achieved)

**Status:** End-to-end playback proven on real hardware. Known bugs to fix.

**First playback: February 19, 2026**
- Track: Applescal - Cymbals Rush
- Chain: laptop `mdma` CLI -> TCP:5557 -> mdma-playback -> PipeWire -> iFi USB DAC
- Result: Audio plays! But too fast (sample rate mismatch) and stop command doesn't work.

**What works:**
- mdma-playback service running on Pi as runit service
- Stock Void Linux PipeWire service (headless, with WirePlumber via context.exec drop-in)
- iFi (by AMR) HD USB Audio detected as PipeWire Audio/Sink
- CLI `playback play <hash>` resolves track from library and sends to playback
- CLI `playback stop` sends command (server acknowledges but doesn't actually stop)
- Cross-compilation from x86_64 dev machine using aarch64 PipeWire sysroot
- `just playback-cross` handles sysroot setup + build in one command
- `just deploy-playback` cross-compiles and deploys to Pi

**Hi-Fi audio requirements:**
- Minimum 96 kHz sample rate (CD is 44.1 kHz, hi-res is 96/192 kHz)
- 24-bit depth minimum (CD is 16-bit)
- Bit-perfect passthrough to USB DAC when possible
- No unnecessary resampling — match source format to DAC capabilities
- The iFi HD USB Audio supports up to 384 kHz / 32-bit — use what the DAC offers

**Known bugs:**
1. **Playback too fast** — sample rate mismatch between decoded audio and PipeWire stream
2. **Stop doesn't stop** — stop command returns success but audio continues playing
3. **No sample rate negotiation** — playback engine should match source file sample rate

**Output stack:**
- iFi (by AMR) HD USB Audio (current, connected)
- HDMI audio (available as fallback)
- 3.5mm headphone jack (Pi built-in, low quality)

**Technical stack:**
- `mdma-playback` service (runit, runs as `mdma` user)
- PipeWire (stock Void service, `_pipewire` user)
- WirePlumber (launched by PipeWire via context.exec)
- Symphonia for audio decoding
- `PIPEWIRE_RUNTIME_DIR=/run/pipewire` for socket access

**What's next:**
1. Fix sample rate handling — read source file rate, configure PipeWire stream to match
2. Fix stop command — actually stop the decode/playback thread
3. Verify 24-bit/96 kHz passthrough to iFi DAC
4. Gapless playback between tracks
5. Volume control

**Success criteria:**
- Music plays at correct speed with correct pitch
- Stop/pause commands work
- 24-bit/96 kHz minimum output quality
- Audio quality matches or exceeds phone playback
- Gapless playback between tracks

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
- [x] Music syncs from at least one source (Bandcamp operational, 329 tracks indexed)
- [ ] Can browse library in web UI
- [x] Audio plays through USB DAC (first playback achieved, bugs to fix)
- [x] Can control playback through CLI (play/stop from laptop)
- [ ] Audio plays at correct speed with hi-fi quality (96 kHz / 24-bit)
- [ ] System is stable enough to use at a party

**User value unlocked:** No more jarring Spotify cuts during gatherings. Professional hi-fi playback from dedicated hardware.

**Current completion:** ~60% overall (provisioning complete, library operational, playback proven but buggy, UI pending)

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

**Immediate priority:** Fix playback bugs and achieve hi-fi quality audio

**What's critical right now:**
1. Fix playback speed — sample rate mismatch in playback engine
2. Fix stop command — playback continues after stop acknowledged
3. Investigate missing Bandcamp downloads (e.g. Carbon Based Lifeforms not downloaded despite being in collection)
4. Verify 24-bit / 96 kHz passthrough to iFi DAC

**What comes after:**
1. Gapless playback
2. Connect mdma-console to mdma-library (browse library in web UI)
3. Playback controls in web UI
4. Unified IPC gateway (future architecture, see Future Vision section)

**Philosophy:** Audio is playing. Fix the quality, then build the UI around it.

---

## Update History

- **2026-02-19:** First end-to-end playback achieved
  - Applescal - Cymbals Rush played from laptop CLI through Pi to iFi USB DAC
  - Full chain: mdma CLI -> TCP -> mdma-library (track lookup) -> mdma-playback -> PipeWire -> iFi
  - Stock Void PipeWire service works headless (WirePlumber via context.exec drop-in)
  - Cross-compilation pipeline: aarch64 PipeWire sysroot from Void packages, cargo-zigbuild with glibc 2.38
  - `just playback-cross` and `just deploy-playback` working
  - Known bugs: playback too fast (sample rate), stop doesn't stop
  - Hi-fi requirement established: 96 kHz / 24-bit minimum, bit-perfect passthrough
  - DevOps principle added: Void-first approach, never build on Pi

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

1. **Fix playback speed** — Read source file sample rate, configure PipeWire stream to match instead of assuming a fixed rate. This is the #1 blocker for usable playback.

2. **Fix stop command** — The playback engine acknowledges the stop but doesn't actually stop the decode/output thread. Debug the command dispatch in the server.

3. **Investigate missing Bandcamp downloads** — Carbon Based Lifeforms (Interloper album + Rymden3000 single) in user's collection but not downloaded. Check bandcamp sync logs, cache state, and whether certain items are skipped.

4. **Verify hi-fi passthrough** — After fixing sample rate, test with a known 24-bit/96 kHz FLAC to confirm bit-perfect output through the iFi DAC.

**Current recommendation:** Fix the two playback bugs first (speed + stop). These are small engine fixes that unlock real-world testing with the full music library.
