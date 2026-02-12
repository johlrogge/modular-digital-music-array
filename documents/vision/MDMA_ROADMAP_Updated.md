# MDMA Roadmap

**Last updated:** February 12, 2026

## Current Status Summary

**Milestone 1 Part 1 (Pi Provisioning): 100% COMPLETE**

NVMe boot working with kernel sync. mdma-console stub deployed (port 3000). `just deploy-dev` and `just deploy-console` working.

**Milestone 1 Part 2 (Music Library): ~40% Complete**

mdma-library service operational with nng IPC. mdma-cli tool for testing. Partial hash matching, facts display, stainless-facts locking fixed.

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

**What's next:**

1. **HTTP upload in mdma-console** - POST endpoint writes to inbox
2. **Library browsing in mdma-console** - Read from mdma-library via nng
3. **Inbox watcher** - inotify-based auto-ingest
4. **Bandcamp integration** - Study bandsnatch, then native library

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
    inbox/              # Drop files here
    blobs/              # Content-addressed storage
        a1/
            b2c3d4...sha256.flac

/metadata/
    facts.jsonl         # Main fact stream (source of truth)
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

**Immediate priority:** Connect mdma-console to mdma-library

**What's critical right now:**
1. Add HTTP upload endpoint to mdma-console
2. Add library browsing page to mdma-console
3. Test end-to-end: upload -> ingest -> browse

**What comes after console integration:**
1. Inbox watcher (inotify-based)
2. Bandcamp integration
3. Audio playback service

**Philosophy:** Get the library visible in the UI, then add more sources and playback.

---

## Update History

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

1. **Add HTTP upload to mdma-console** (1-2 hours)
   - POST /upload endpoint
   - Writes to /music/inbox/
   - Response with ingest status

2. **Add library browsing to mdma-console** (2-3 hours)
   - Connect to mdma-library via nng
   - Display track list
   - Track detail page with facts

3. **Test end-to-end workflow** (30 min)
   - Upload file via web UI
   - Verify appears in library
   - Browse and view facts

4. **Inbox watcher** (2-3 hours)
   - inotify-based file watching
   - Auto-trigger ingest pipeline
   - Handle rate limiting for bulk drops

**Current recommendation:** Focus on mdma-console integration to make the library visible and usable through a web browser.
