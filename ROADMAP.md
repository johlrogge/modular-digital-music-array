# MDMA Roadmap

**Last updated:** February 22, 2026

## Where We Are

**Milestone 1 Part 1 (Pi Provisioning): COMPLETE**

NVMe boot working. mdma-console stub deployed. `just deploy-dev` working.

**Milestone 1 Part 2 (Music Library): Operational**

mdma-library running with nng IPC. 339 tracks indexed. Bandcamp sync operational. mdma-cli for search/list/facts from laptop.

**Milestone 1 Part 3 (Audio Playback): COMPLETE — Feb 20, 2026**

Playback bugs fixed and verified on real hardware. 192 kHz upsampled output through iFi USB DAC. Full chain working.

**Gateway + Packaging: COMPLETE — Feb 22, 2026**

All services behind the gateway. Only port 5555 exposed. void-packages as single source of truth for run scripts. Clean deploy from laptop via `just deploy-*`. Verified end-to-end: ping, status, search, queue, playback, source commands all work through gateway.

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

### ~~1. Hi-Res Resampler~~ — COMPLETE

`rubato` resampler integrated. PipeWire stream fixed at 192 kHz. All sources upsampled at load time. iFi DAC indicator confirms 192 kHz output.

---

### ~~2. Queue + Search + CLI Polish~~ — COMPLETE

**Queue: COMPLETE**

- In-memory queue with `ContentHash` as the track identifier (not paths)
- Commands: `queue append`, `queue next`, `queue list`, `queue clear`, `queue remove`, `queue replace`, `queue edit`
- Auto-advances when a track finishes
- `queue replace`: atomic swap of entire queue from stdin — enables shuffle, re-sort, playlist load
- `queue edit`: opens queue in `$EDITOR` using shared playlist format, applies changes via `queue replace`
- **Persistent queue**: saved to `queue.json` on every mutation, restored on service restart
- `ContentHash` moved to `playback_primitives` — available across all services without circular deps

**Search: COMPLETE**

- New `library_search` crate: composable `TrackQuery` with implicit AND semantics
- `StringQuery`: Contains (all-words), Initialism (CamelCase + all-caps like `CBL`), Regex
- `NumericQuery`: Exact, Range (`124..132`), Tolerance (`128+-4`)
- `DurationQuery`: Exact, AtLeast, AtMost, Range, WithPrecision (named-unit bucket)
- `KeyQuery`: Camelot-based exact and tolerance (traditional notation → Camelot at parse time)
- CLI: `mdma search [QUERY] [--artist] [--title] [--album] [--label] [--genre] [--bpm] [--key] [--duration] [--year] [--source]`
- CLI: `mdma search fact-values-for <FACT_TYPE>` — discover all values for any fact type
- **Stdin intersection filter**: piped hashes narrow search results — enables playlist viewing and chain filtering
- **dmenu workflow operational:** `mdma search --artist "CBL" | dmenu | mdma queue append`
- Deployed and verified on real hardware against 339-track library

**Pipe Composition: COMPLETE**

- `mdma sort <field> <-a|-d>`: reads hashes from stdin, sorts by bpm/title/artist/album/duration
- Multi-key sort via chaining: `mdma sort title -a | mdma sort artist -a`
- Unified playlist format: lines starting with 8-12 hex chars are track entries; comments/blanks ignored
- All commands output same format — directly saveable as `.plist` and composable with dmenu/grep/shuf
- `queue remove` resolves short hashes to canonical sha256 before sending to service

**CLI Output: COMPLETE**

- Colored tabular output using `corsett` (column sizing) + `colored` (ANSI colors)
- Hash: dim gray, Artist: green, Title: bold, Duration: dim gray
- TTY-aware: colored table in terminal, canonical `hash  Artist - Title  [duration]` when piped
- Queue list: dim position numbers, same column layout as search
- Column shortening with right-ellipsis (…) via corsett's `RightEllipsis<FreeText>` algorithm

---

### ~~3a. Single API Gateway + Packaging~~ — COMPLETE

**Deployed and verified end-to-end on Pi — Feb 22, 2026.**

Gateway routes all traffic through `tcp://0.0.0.0:5555`. Library and playback are IPC-only (no `--tcp` flags). Only ports exposed: 5555 (gateway) and 80 (console).

```
External clients (laptop CLI)
       |
       ▼
┌─────────────────┐
│  mdma-gateway   │  tcp://0.0.0.0:5555
│  (router/proxy) │
└────────┬────────┘
         │ local IPC
    ┌────┴──────────────────────────┐
    │         │                    │
    ▼         ▼                    ▼
mdma-library  mdma-playback  /run/mdma/sources/*.sock
(ipc fixed)   (ipc fixed)   (auto-discovered)
```

**void-packages as single source of truth:**
- All 6 services have srcpkg entries: `void-packages/srcpkgs/<name>/template` + `files/<name>/run`
- Package scripts and deploy recipes copy run scripts from void-packages (no more heredocs)
- CLI: `LibraryBackend` + `PlaybackBackend` enums dispatch via gateway or direct IPC
- `MDMA_GATEWAY` is the only env var needed from laptop
- Provisioning (stage5) installs and enables all 6 services

---

### 3b. Codebase Cleanup

**Why now:** The architecture is stable. Dead code is clear. Warnings accumulate. Clean house before building new features.

- Remove dead crates: `bandcamp_ipc_protocol`, `bandcamp_ipc_client` (replaced by `source_protocol` + `gateway_client`)
- Fix all compiler warnings across workspace
- Remove half-finished experiments, consolidate components
- Run as a dedicated focused pass

---

### 4. Web UI Player Controls

**Why next:** The console (`mdma-console`) is a stub. It needs to be a usable player interface — the first front-end that isn't the CLI. This is what beta testers will interact with.

- Now playing display (track, artist, position)
- Queue view (current queue, highlight playing track)
- Basic controls: play, stop, next, queue append/remove
- Search and browse library
- Requires pub/sub or polling for live updates (see 5)

---

### 5. Pub/Sub Events

**Why:** Enables live UIs (web, TUI, desktop). There is real state to observe (position, current track, queue contents).

**Events (unsolicited):**
- `track_started`, `track_ended`, `position_update`, `queue_changed`
- Subscribers get push notifications without polling
- Enables: live position display in web UI, auto-updating queue, TUI that refreshes

---

### 6. MP3 Support + Manual Upload Source

**Why:** Beta testers have large MP3 libraries. FLAC-only blocks adoption. Not "hifi" but pragmatic.

- Add MP3 decoding to playback engine (Symphonia supports it — enable the feature)
- `mdma-library` must accept `.mp3` files in inbox alongside `.flac`
- **Manual upload source:** a simple "source" that accepts file uploads (SCP/HTTP) and drops them in inbox
  - Enables beta testers to get their music onto the Pi without Bandcamp
  - Could be as simple as an HTTP upload endpoint on the console, or an `mdma upload <file>` CLI command

---

### 7. Bandcamp Configuration

**Missing piece:** How does a fresh install configure the bandcamp service?

- Username (`--username`) is a CLI flag / config — needs a config file or provisioning step
- Cookies (`--cookies /etc/mdma/bandcamp-cookies.json`) must be provided manually
  - Current approach: SCP cookies file to Pi
  - Better: `mdma source configure bandcamp` interactive flow, or web UI form
- Document the manual process now, automate later

---

### 8. Stream Management (Silence → Off)

- Auto-shutdown PipeWire stream after N seconds of silence (queue empty, no track playing)
- Auto-restart when a track is queued/played
- Quality-of-life, not a blocker

---

## Future Clients

### TUI Client

Terminal-based player interface. Real-time display of now playing, queue, search. Runs on the laptop. Talks to the gateway.

Think: `cmus` or `ncmpcpp` but for MDMA. Rust TUI framework (ratatui or similar).

### mdmamp (Desktop Player)

**mdmamp** — MDMA Music Player. Graphical desktop client built with **Bevy**. The name flirts with Winamp.

Full-featured player UI: library browser, queue management, now playing with waveform, search. Connects to the Pi's gateway from any machine on the network.

Long-term vision: the primary way non-technical users interact with the system.

---

## Validation: Full Reinstall from SD Card

Deferred until time permits. Not blocking current work, but must happen before beta.

**Constraint:** `/music` must survive. The NVMe partition layout keeps `/music` on its
own partition (`/dev/nvme0n1p4`), so reinstalling root does not touch it. Verify this
holds before wiping anything.

**What to test:**
- Fresh SD card boot → beacon → provision NVMe → all services start automatically
- `/music` and `/metadata` intact after reinstall
- `mdma search` works immediately against the existing library
- Bandcamp sync resumes without re-downloading
- Bandcamp cookies + username configured correctly

This should happen before inviting other users onto the system.

---

## What to Defer

- Manual DJ mixing (MIDI, crossfader) — after queue + virtual decks proven
- MDMA-101 hardware — long-term; design data model to accommodate it
- Gapless playback — desirable but not blocking queue MVP
- Multi-deck UI — after single queue works
- CDJ/Pro DJ Link integration — after Milestone 1 complete
- Auto-updates — manual deploys fine during development
- TUI client — after web UI proves the interaction model
- mdmamp — after pub/sub and web UI are solid

---

## Active Service Architecture

```
stainless_facts    = Generic fact stream operations (crate, mandatory access layer)
music_facts        = Types only (MusicValue, FactSource, ContentHash, newtypes)
mdma-gateway       = API gateway: single TCP port, routes to all services
mdma-library       = Library service with nng IPC interface
mdma-playback      = Audio playback service (Symphonia + PipeWire + rubato)
mdma-bandcamp      = Bandcamp download service (source_protocol)
mdma-console       = HTTP frontend (web UI player)
mdma-cli           = CLI frontend (gateway-aware, dual-mode dispatch)
mdma-tui           = TUI client (ratatui, future)
mdmamp             = Desktop player (Bevy, future)
beacon             = Provisioning and service discovery

source_protocol    = Unified request/response for music sources
gateway_protocol   = Envelope types (library + playback + source)
gateway_client     = NNG client for the gateway
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

**Environment vars for CLI from laptop (gateway mode — preferred):**
```bash
export MDMA_GATEWAY="tcp://mdma-909.local:5555"
```

All services are behind the gateway. No per-service TCP ports exposed.

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

- **2026-02-22:** Priority 3a fully complete — gateway deployed and verified on Pi.
  - All services behind gateway: only ports 5555 (gateway) and 80 (console) exposed
  - Library and playback run IPC-only (no more `--tcp` flags, no ports 5557/5558)
  - void-packages as single source of truth: 6 srcpkg entries with templates + run scripts
  - Package scripts and deploy recipes copy run scripts from void-packages (no heredocs)
  - CLI: `LibraryBackend` + `PlaybackBackend` enums with Direct/Gateway variants
  - Deploy recipes handle first-time install (wait for runit supervise directory)
  - devenv.nix: only `MDMA_GATEWAY` env var, `mdma-status` checks gateway + console
  - Provisioning (stage5): installs and enables all 6 services
  - Verified: ping, status, search, list, queue, playback play/stop/now, source list/status
  - **Next:** Pub/sub events, codebase audit, or full reinstall validation

- **2026-02-21 (night):** Priority 3a (Gateway) code complete.
  - `source_protocol`: unified request/response for all music sources (Ping, GetStatus, Sync, ListDownloads, Cancel, Pause, Resume)
  - `gateway_protocol`: envelope wrapping library, playback, source, and ListSources
  - `gateway_client`: typed NNG client with `library_request()`, `playback_command()`, `source_request()`, `list_sources()`
  - `mdma_gateway`: single TCP entry point, routes to library/playback IPC + auto-discovered sources in `/run/mdma/sources/`
  - `mdma-bandcamp` adapted: uses `source_protocol` instead of `bandcamp_ipc_protocol`, socket at `/run/mdma/sources/bandcamp.sock`
  - CLI: `Commands::Bandcamp` → `Commands::Source` (list/sync/status/downloads/cancel/pause/resume)
  - CLI: `--gateway`/`MDMA_GATEWAY` for single-address mode; direct IPC fallback preserved
  - Deprecated: `bandcamp_ipc_protocol`, `bandcamp_ipc_client` (still in workspace, unused)
  - All 22 tests pass, workspace builds clean
  - **Next:** Deploy gateway + updated bandcamp to Pi, verify end-to-end, then pub/sub events

- **2026-02-21:** Priority 2 fully complete. Queue, search, sort, pipe composition, and CLI polish all done.
  - `queue replace`: atomic queue swap from stdin (shuffle, playlist load, re-sort)
  - `queue edit`: opens queue in `$EDITOR`, applies via `queue replace`
  - Persistent queue: `queue.json` saved on every mutation, restored on service restart
  - `mdma sort`: stable sort by bpm/title/artist/album/duration, chainable for multi-key
  - Stdin intersection filter on search: piped hashes narrow results
  - Unified playlist format: 8-12 hex first token = track entry, everything else ignored
  - Colored tabular output: `corsett` column sizing + `colored` ANSI — hash/artist/title/duration columns
  - TTY-aware: colored table in terminal, composable canonical format when piped
  - Fixed playback service cwd (relative blob paths required `/music` working directory)
  - Fixed `queue remove` short hash resolution
  - **Next:** Priority 3 — Single API Gateway + Pub/Sub Events

- **2026-02-20 (night):** Priority 2 complete. Rich search operational.
  - `library_search` crate: TrackQuery with Initialism/Contains/Regex/Numeric/Duration/Key
  - All-caps treated as initialism (`CBL` → Carbon Based Lifeforms)
  - `mdma search fact-values-for <FACT_TYPE>` for value discovery
  - `deploy-library` justfile bug fixed (TCP flag was dropped on each deploy)
  - Stray test playback process killed (was burning a full CPU core for 39 hours)
  - Observed: 44.1→192 kHz upsampling uses ~2.5/4 cores; configurable rate deferred
  - **Next:** Priority 3 — Single API Gateway + Pub/Sub Events

- **2026-02-20 (evening):** Resampler complete. Queue and now-playing operational.
  - `rubato` resampler done: 192 kHz output confirmed on iFi DAC
  - Queue overhaul: stores `ContentHash` + path pairs, exposes hashes (not paths)
  - `queue list`: tty-aware human display (artist/title/duration) or raw hashes when piped
  - `queue remove`: by arg or stdin, composes with `queue list | dmenu | queue remove`
  - `playback now`: shows currently playing track, same tty-aware format as queue list
  - `ContentHash` moved to `playback_primitives` — cross-service identifier without new deps

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
