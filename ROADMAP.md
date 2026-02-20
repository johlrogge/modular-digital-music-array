# MDMA Roadmap

**Last updated:** February 20, 2026

## Where We Are

**Milestone 1 Part 1 (Pi Provisioning): COMPLETE**

NVMe boot working. mdma-console stub deployed. `just deploy-dev` working.

**Milestone 1 Part 2 (Music Library): Operational**

mdma-library running with nng IPC. 329 tracks indexed. Bandcamp sync operational. mdma-cli for search/list/facts from laptop.

**Milestone 1 Part 3 (Audio Playback): COMPLETE — Feb 20, 2026**

Playback bugs fixed and verified on real hardware (commit 01c21db). Rymden3000 (24-bit / 44.1 kHz FLAC) plays at correct speed and full fidelity through iFi USB DAC. Full chain working: laptop CLI → TCP → mdma-library → mdma-playback → PipeWire → iFi DAC.

**First real playback milestone: complete.**

---

## The Vision

**A continuous hi-fi stream for parties.**

Always output at the iFi DAC's maximum supported rate (probe at startup — likely 192 kHz). Upsample all sources. A single ordered queue feeds the stream. The system is a music player first; DJ tools come later.

**Virtual deck progression:**
1. Now: one queue → deck A (music player)
2. Near future: multiple virtual decks for mixlists (sets with mixing instructions)
3. Later: MIDI controller (A&H K3) for manual mixing
4. Eventually: MDMA-101 hardware

**Architecture constraints (locked):**
- One NNG TCP socket (API gateway) — no more per-service ports (5555/5556/5557)
- Message bus / pub/sub for unsolicited events (position, track change, queue)
- **stainless_facts is the only way to interact with facts.** Any new fact need = new capability in stainless_facts. Never parse or write JSONL manually.
- Virtual deck is the internal abstraction — the queue feeds it; users don't address decks directly yet
- Facts are immutable — "skip" means appending a skipped fact, not editing state
- `rubato` for resampling — it is the standard Rust high-quality resampler
- iFi max rate probed at startup — don't hardcode 192 kHz; fall back to 192 kHz if probing fails

---

## Priorities

### 1. Hi-Res Resampler — Foundation of Everything

**Why first:** Audio quality is the point. Building a queue on a wrong-rate pipeline is building on sand.

**What it means:**
- Probe iFi DAC for actual max rate at startup (via PipeWire negotiation or device info), default to 192 kHz
- Add `rubato` crate as resampler: FlacSource → Resampler → 192 kHz F32LE → Mixer → PipeWire
- PipeWire stream fixed at 192 kHz from creation — source rate no longer matters, everything gets upsampled
- Resolves the sample-rate-lock limitation from the MVP

**Verify:** Load Rymden3000 (44.1 kHz source). iFi DAC indicator shows 192 kHz.

---

### 2. Queue + stainless_facts Search (Together)

**Why together:** Search without queue is incomplete; queue without search means you can't find tracks to add. dmenu falls out naturally once both exist.

**stainless_facts search:**
- Add query/filter capabilities to the crate (search by title, artist, etc.)
- All library consumers use this API — zero raw JSONL parsing anywhere
- This is a crate-level feature, not a workaround

**Queue:**
- Persistent ordered list of track IDs
- Commands: enqueue, play-next, skip, reorder, clear
- Auto-advances when a track finishes (EOF → pop next from queue, load, play)
- **Storage: in-memory only** — no fact stream, no persistence across restarts. When the design stabilises (play statistics, history, set logging), write into a separate fact stream and eventually join with the library fact stream
- Feeds deck A only; deck abstraction stays for future expansion
- Silence when queue is empty is acceptable for now

**dmenu integration:** No special work needed. Once you can search, pipe results into dmenu and pipe selection into enqueue. Unix pipelines compose naturally.

---

### 3. Single API Gateway + Pub/Sub Events

**Why third:** After queue, there is real state to observe (position, current track, queue contents). Pub/sub becomes genuinely useful.

**Gateway:**
- One TCP port replaces the current 5555/5556/5557 split
- Routes commands to internal services (library, playback, bandcamp)
- Internal services stay on IPC sockets — only the gateway is TCP-accessible
- Clients see one address regardless of internal topology

```
External clients
       |
       ▼
┌─────────────────┐
│  mdma-gateway   │  (single public port)
│  (router/proxy) │
└────────┬────────┘
         │ local IPC bus
    ┌────┴──────────────────────────┐
    │         │                    │
    ▼         ▼                    ▼
mdma-library  mdma-bandcamp  mdma-playback
(registered)  (registered)   (registered)
```

**Pub/sub events (unsolicited):**
- `track_started`, `track_ended`, `position_update`, `queue_changed`
- Subscribers get push notifications without polling
- Enables: live position display, queue UI that updates automatically, dmenu that refreshes when queue changes

---

### 4. Codebase Audit and Cleanup

**Why fourth:** After the above, it is clear what is alive vs. what is dead.

- Read each component, ask: "does anything in library / console / playback depend on this?"
- If no → delete
- Remove half-finished experiments, consolidate components, align everything with the three active bases
- Run as a dedicated focused pass, not incrementally

---

### 5. Stream Management (Silence → Off)

After queue management is working:
- Auto-shutdown PipeWire stream after N seconds of silence (queue empty, no track playing)
- Auto-restart when a track is queued/played
- Quality-of-life, not a blocker

---

## What to Defer

- Manual DJ mixing (MIDI, crossfader) — after queue + virtual decks proven
- MDMA-101 hardware — long-term; design data model to accommodate it
- Gapless playback — desirable but not blocking queue MVP
- Multi-deck UI — after single queue works
- CDJ/Pro DJ Link integration — after Milestone 1 complete
- Auto-updates — manual deploys fine during development

---

## Active Service Architecture

```
stainless_facts    = Generic fact stream operations (crate, mandatory access layer)
music_facts        = Types only (MusicValue, FactSource, ContentHash, newtypes)
mdma-library       = Library service with nng IPC interface
mdma-playback      = Audio playback service (Symphonia + PipeWire + rubato)
mdma-bandcamp      = Bandcamp download service
mdma-console       = HTTP frontend
mdma-cli           = CLI frontend
beacon             = Provisioning and service discovery
```

**Storage layout:**
```
/music/
    inbox/              # Drop files here (watched by mdma-library)
    downloads/          # Staging area for in-progress downloads
    blobs/              # Content-addressed storage
        a1/
            b2c3d4...sha256.flac

/metadata/
    facts.jsonl         # Main fact stream (source of truth)

/var/lib/mdma-bandcamp/
    cookies.txt         # Bandcamp session cookies (Netscape format)
    bandcamp.cache      # Track-oriented download cache
```

---

## Development Workflow

```bash
# Cross-compile and deploy playback to Pi
just playback-cross && just deploy-playback

# Deploy library service
just deploy-dev

# Watch mode (check → test → build → clippy)
just watch

# Connect to Pi
ssh -4 -i ~/.ssh/mdma_pi admin@mdma-909.local
```

**Environment vars for CLI from laptop:**
```bash
export MDMA_LIBRARY_SOCKET="tcp://mdma-909.local:5555"
export MDMA_BANDCAMP_SOCKET="tcp://mdma-909.local:5556"
export MDMA_PLAYBACK_SOCKET="tcp://mdma-909.local:5557"
```

---

## Strategic Principles

- **Deploy First:** Code runs on live hardware within minutes of being written
- **stainless_facts is the access layer:** Never parse or write JSONL directly
- **Facts are immutable:** Append new facts, never mutate existing ones
- **Type-driven safety:** Rust's type system prevents illegal states
- **Void-first:** Never build on the Pi; cross-compile from dev machine
- **Minimal complexity:** The right amount of abstraction is the minimum needed now

---

## Update History

- **2026-02-20:** Playback bugs fixed and verified. First real playback milestone complete.
  - Rymden3000 (24-bit / 44.1 kHz) plays at correct speed and full fidelity
  - New vision locked: 192 kHz always-on upsampled stream via `rubato`
  - Architecture decisions locked: single NNG gateway, stainless_facts mandatory, in-memory queue
  - Roadmap restructured around four ordered priorities: resampler → queue+search → gateway+pubsub → cleanup
  - Renamed MDMA_ROADMAP.md → ROADMAP.md at project root

- **2026-02-19:** First end-to-end playback achieved
  - Applescal - Cymbals Rush played from laptop CLI through Pi to iFi USB DAC
  - Full chain: mdma CLI → TCP → mdma-library → mdma-playback → PipeWire → iFi
  - Stock Void PipeWire service works headless (WirePlumber via context.exec drop-in)
  - Cross-compilation pipeline: aarch64 PipeWire sysroot, cargo-zigbuild with glibc 2.38
  - Known bugs at time: playback too fast (sample rate), stop didn't stop

- **2026-02-18:** Bandcamp integration operational
  - mdma-bandcamp service running on Pi (async NNG + tokio)
  - 114-item Bandcamp collection syncing
  - ZIP extraction for albums, single FLAC rename for tracks
  - nng_transport component extracted (shared mDNS resolution via avahi)

- **2026-02-12:** Library service fully operational
  - mdma-library with nng IPC, stainless-facts, content-addressed blob storage
  - mdma-cli tool with partial hash matching
  - Fixed stainless-facts locking for concurrent read/write

- **2026-01-12:** Milestone 1 Part 1 complete
  - NVMe boot working with kernel sync
  - mdma-console stub deployed
  - Development workflow operational

- **2025-12-16:** Beacon operational on real Pi 5
