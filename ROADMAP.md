# MDMA Roadmap

**Last updated:** February 25, 2026

## Where We Are

**Milestone 1 Part 1 (Pi Provisioning): COMPLETE**

NVMe boot working. mdma-console stub deployed. `just deploy-dev` working.

**Milestone 1 Part 2 (Music Library): Operational**

mdma-library running with nng IPC. 347 tracks indexed. Bandcamp sync operational. mdma-cli for search/list/facts from laptop.

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
- Two external NNG TCP ports: 5555 (API gateway, req/rep) + 5556 (event bus, pub/sub) — no per-service ports
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

### ~~3b. Codebase Cleanup~~ — COMPLETE (Feb 22, 2026)

- All clippy warnings resolved across workspace (was ~33, now 0)
- 6 dead crates removed: `bandcamp_ipc_protocol`, `bandcamp_ipc_client`,
  `media_downloader`, `audio_fingerprint`, `download_cli`, `media_ctl`
- `mdma-console` migrated from `bandcamp_ipc_client` to `gateway_client`
- Beacon: legacy `ActionLegacy` trait + unused error variants removed
- Agent workflow: code-minion + commit agent + architect review loop

---

### ~~4. Pub/Sub Events + Play History Facts~~ — COMPLETE (Feb 22, 2026)

**Pub/sub events:**
- `event_protocol` crate: `PlaybackEvent` enum (TrackStarted, TrackEnded, TrackStopped, QueueChanged)
- Topic-prefixed wire format: `{topic}\0{json}` for nng Pub/Sub filtering
- Playback server publishes events via Pub0 at all state transitions
- Gateway bridges IPC events to TCP port 5556 (raw byte passthrough)
- CLI: `mdma subscribe` with rich formatting (resolves artist/title from library for TrackStarted)

**Play history facts:**
- `Played(DateTime)` and `Skipped(DateTime)` variants in `MusicValue`
- Playback writes `Played` fact on natural track end, `Skipped` on manual stop
- Uses existing `stainless_facts` infrastructure
- Enables: play count queries, `--never-played`, `--recently-played`

**Deployed and smoke tested (6/6 pass) on Pi.**

---

### ~~5. Polybar Widget~~ — COMPLETE (Feb 23, 2026)

First external pub/sub consumer. Proves the event model works from a separate process.

- Shell script (`scripts/polybar/mdma-now-playing.sh`) subscribes via `mdma subscribe` pipe mode
- Parses JSON events with `jq`, resolves hashes to `Artist - Title` via `mdma search`
- Queries `mdma playback now` on startup for initial state
- Click actions: left=play, middle=skip (stop+play), right=stop
- Gateway address passed as CLI argument (no env var dependency from polybar)
- Polybar `format-prefix` ensures clickable icon even when stopped

---

### ~~6. Web UI Player Controls~~ — COMPLETE (Feb 23, 2026)

**Why next:** The console (`mdma-console`) is a stub. It needs to be a usable player interface — the first front-end that isn't the CLI. This is what beta testers will interact with.

- Now playing display (track, artist, position)
- Queue view (current queue, highlight playing track)
- Basic controls: play, stop, next, queue append/remove
- Search and browse library
- Uses pub/sub for live updates (no polling)

---

**Future enhancement -- Tabbed UI:**
The current single-page layout mixes playback (now playing, queue, controls) with library management (search, inbox, bandcamp, packages). As the UI grows, split into two tabs:
- **Playback tab:** Now playing, queue, player controls -- the DJ-facing view
- **Library tab:** Search, inbox management, bandcamp sync, package updates -- the librarian view

This keeps each view focused and prevents the page from becoming an endless scroll. Not blocking current work.

**Future enhancement -- Cover Art & Fact Stream Aggregation:**
The web library should display cover art for tracks. This is a good case for fact stream aggregation and persisting: generate library pages as new tracks are added to the library, and use Stainless facts functionality to only request facts after a certain timestamp to update as needed. This avoids rebuilding the entire library view on every request and enables incremental, event-driven UI updates.

---

### ~~7. ZIP Upload for Library Ingestion~~ — COMPLETE

Web upload via console operational. CLI `mdma upload` deferred — web workflow sufficient for now.

- ~~Extract `extract_zip_to_inbox` from `mdma-bandcamp` into a shared component~~ — DONE (`components/inbox_utils/` exists)
- ~~Add `Upload` command to gateway protocol (accepts file bytes + source metadata)~~ — DONE
- ~~Gateway: receives file, extracts to inbox, triggers ingest, returns hashes~~ — DONE
- ~~Supports: ZIP archives (extracts audio files), individual FLAC/WAV/AIFF files~~ — DONE
- ~~Uses existing `IngestSource::Upload` provenance tracking~~ — DONE
- CLI: `mdma upload <file>` — deferred; web workflow sufficient for now

---

### ~~8. MP3 Support~~ — COMPLETE

MP3 decoding enabled in Symphonia. Library accepts .mp3 files in inbox. 8 MP3 tracks in library alongside 339 FLACs.

- ~~Add MP3 decoding to playback engine (Symphonia supports it — enable the feature)~~ — DONE
- ~~`mdma-library` must accept `.mp3` files in inbox alongside `.flac`~~ — DONE
- ~~`mdma upload` (priority 7) already handles getting files onto the Pi~~ — DONE

---

### ~~9. Track Export (`mdma export`)~~ — COMPLETE

CLI `mdma export` operational. Reads hashes from stdin, pulls audio from Pi via gateway, transcodes to AIFF/WAV. Console export endpoint also working.

**Interface:**

```
mdma search --artist CBL | mdma export --format=aiff --output ./export/
cat my_hardcore_techno.plist | mdma export --format=aiff
mdma search --bpm 128-132 --key 8A | mdma export --format=aiff --output ./rekordbox-prep/
```

- ~~Reads content hashes from stdin (one per line, piped from `mdma search` or playlist files)~~ — DONE
- ~~Pulls audio from Pi via gateway~~ — DONE
- ~~Converts to target format (AIFF/WAV)~~ — DONE
- ~~Writes to output directory with metadata-based filenames (Artist - Title.aiff)~~ — DONE
- ~~Runs on the laptop, not the Pi~~ — DONE
- Console export endpoint also working

**Smart format selection (Feb 25, 2026):**
- `--lossless-format` and `--lossy-format` CLI flags for per-category format selection
- `FormatCategory` enum classifies source formats as lossless (FLAC, WAV, AIFF) or lossy (MP3, AAC, OGG)
- `--format` still works as blanket override; new flags enable smart per-source decisions
- 75 tests passing

**Guiding use case:** Rekordbox preparation — export a set of tracks as AIFF, import into Rekordbox for club/CDJ use. But the tool is format-generic, not Rekordbox-specific.

**Shared infrastructure:** The `audio_transcoder` component is reused by Rekordbox Sync (Priority 12) and Virtual CDJ (Priority 13).

**Later:** `mdma rekordbox export` wraps `mdma export` + generates Rekordbox XML with BPM, key, artist, title. Identity mapping facts for round-trip sync.

---

### ~~Infrastructure: Git-flow, Conventional Commits, CI, Release Workflow~~ — COMPLETE (Feb 25, 2026)

Developer tooling and process improvements to support sustainable releases.

- **Git-flow branching**: `develop` branch established, gitflow CLI added to devenv. CI runs tests on all branches; packages built only on main/master.
- **Conventional Commits**: Skill file (`.claude/skills/conventional-commits/SKILL.md`) added. All commits follow `<type>(<scope>): <description>` format enforced by agent workflow.
- **Documenter agent**: Agent definition added for release workflow README updates.
- **Release process**: 9-step git-flow release workflow documented in `.claude/skills/mdma-devops/references/releases.md`.
- **DevOps skill overhaul**: Ansible references removed. xbps command reference added. Provisioning and recovery docs updated.
- **CI split**: Test job (all branches) separated from build-and-publish job (main/master only).

---

### ~~10. Bandcamp Configuration~~ — COMPLETE (Feb 25, 2026)

Web-based configuration for Bandcamp cookies and username via mdma-console. Fresh installs can configure bandcamp through the web UI at http://mdma-909.local/bandcamp/config.

- Web UI form for cookie and username configuration
- Cookies stored in Netscape format at `/var/lib/mdma-bandcamp/cookies.txt`
- Username persisted to `/etc/mdma/bandcamp-username`
- Service restart after configuration update

---

### 11. Stream Management (Silence → Off)

- Auto-shutdown PipeWire stream after N seconds of silence (queue empty, no track playing)
- Auto-restart when a track is queued/played
- Quality-of-life, not a blocker

---

### 12. Rekordbox Sync

**Why:** Bridge MDMA and Rekordbox for club/CDJ preparation. Two phases.

**Interface:** Rekordbox XML format (official, stable). Not the encrypted SQLite DB.

**Identity mapping:** `PioneerMapping` fact (ContentHash ↔ TrackID + file path).
Persisted via stainless_facts. Created on first export, maintained across syncs.
Named `PioneerMapping` (not Rekordbox-specific) because it maps to the Pioneer
ecosystem broadly — shared with Priority 13 (Virtual CDJ).

**Phase A — MDMA to Rekordbox (export):**
- `mdma rekordbox export --playlist <name> --output <path>` on laptop
- Pulls tracks from Pi, converts FLAC to AIFF locally (metadata + album art support)
- Generates Rekordbox XML with file paths, BPM, key, artist, title
- Records identity mapping as facts for future syncs
- User imports XML in Rekordbox

**Phase B — Rekordbox to MDMA (import):**
- `mdma rekordbox import <rekordbox.xml>`
- Matches tracks via identity mapping facts (falls back to metadata matching)
- Imports playlists into MDMA playlist system
- Imports/updates metadata: tags, rating, comments

**Prerequisites:** First-class playlists, tags/rating facts.

**Conversion runs on the laptop, not the Pi.**

**Shared infrastructure:** The FLAC-to-AIFF transcoding pipeline and ContentHash-to-TrackID
identity mapping are also used by Priority 13 (Virtual CDJ). Factor these into reusable
components when implementing.

---

### 13. Virtual CDJ (Network Media Server for Physical CDJs)

**Why:** Eliminate USB sticks entirely. MDMA serves its library directly to physical CDJs
on the local network. The CDJ browses and plays tracks as if a USB stick were inserted.

**Target hardware:** CDJ-900 (confirmed on local network). CDJ-2000/3000 also supported.

**How it works:** CDJs can mount NFS shares and treat them as media sources. MDMA generates
the Pioneer directory structure (PDB database + ANLZ analysis files) on `/cdj-export` and
serves it via NFSv3. The CDJ sees the NFS share as equivalent to a USB stick.

**Phase A — Static NFS Export (MVP):**
- `pioneer_export` component: generates `PIONEER/` directory structure from MDMA library
  - PDB database: tracks, artists, albums, playlists (via `rekordcrate` crate)
  - ANLZ files: beatgrid, basic waveform data (from BPM/key facts)
  - Audio files: AIFF transcoded from FLAC (reuses Rekordbox Sync transcoding pipeline)
- Generates to `/cdj-export` on MDMA-909 (secondary NVMe, already provisioned)
- NFS export already configured in provisioning (NFSv3, read-only, LAN only)
- CLI: `mdma cdj export` — full library export, `mdma cdj export --playlist <name>`
- Incremental: only transcode/regenerate changed tracks

**Phase B — Reactive Export:**
- Subscribe to library change events (new tracks, metadata updates)
- Automatically regenerate affected PDB entries and ANLZ files
- Background AIFF transcoding on ingest (Pi has the CPU headroom)

**Phase C — Pro DJ Link Participation (stretch):**
- MDMA announces itself on the Pro DJ Link network (port 50000)
- Responds to DeviceSQL queries from CDJs (port 12523/1051)
- Serves track data and metadata on demand
- Foundation exists in `prodj` research project (packet parsing, player registry)
- Reference: https://djl-analysis.deepsymmetry.org/djl-analysis/packets.html

**Shared with Priority 12:**
- `pioneer_export` component (PDB/ANLZ generation)
- FLAC-to-AIFF transcoding pipeline
- ContentHash ↔ Pioneer TrackID identity mapping facts

**Prerequisites:** Priority 12 Phase A (transcoding pipeline, identity mapping).

**Hardware:** MDMA-909 variant with secondary NVMe (`/cdj-export` partition).

**Key crate:** `rekordcrate` (PDB/ANLZ read/write, Rust, available on crates.io).

---

## Future Clients

Ordered by complexity — simplest ships first.

### ~~Polybar Widget~~ (priority 5 — COMPLETE)

Status bar module. Shell script + polybar config. First pub/sub consumer — proved the event model works from an external process.

### TUI Client

Terminal-based player interface. Real-time display of now playing, queue, search. Runs on the laptop. Talks to the gateway.

Think: `cmus` or `ncmpcpp` but for MDMA. Rust TUI framework (ratatui or similar).

### mdmamp (Desktop Player)

**mdmamp** — MDMA Music Player. Graphical desktop client built with **Bevy**. The name flirts with Winamp.

Full-featured player UI: library browser, queue management, now playing with waveform, search. Connects to the Pi's gateway from any machine on the network.

Long-term vision: the primary way non-technical users interact with the system.

---

## Before Beta

The following must work reliably before inviting beta testers onto the system.

### Getting Started (Onboarding)

The golden path for setting up a new MDMA node:

1. Start with a Raspberry Pi running Void Linux from SD card
2. SSH in and run the install script — it sets the hostname and installs the beacon via XBPS
3. Browse to `welcome-to-mdma.local` and provision through the web UI
4. All services start automatically on the NVMe

This flow must be smooth and require no manual intervention. It is the first thing a beta tester will experience.

### Recovery

If the system becomes unbootable or needs to be reprovisioned:

1. Edit `cmdline.txt` on the SD card to boot back into beacon mode
2. Browse to `welcome-to-mdma.local` and re-provision through the web UI
3. Re-provisioning will NOT overwrite existing partitions
4. Music and metadata are preserved on the NVMe

This gives users a safe path back without losing their library.

### Validation: Full Reinstall from SD Card

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
- CDJ/Pro DJ Link integration — documented as Priority 13 (Virtual CDJ); Phase C (full protocol participation) deferred until Phase A (static NFS export) is validated
- Auto-updates — manual deploys fine during development
- TUI client — after polybar + web UI prove the interaction model
- mdmamp — after pub/sub, polybar, and web UI are solid
- MCP tools for smoke testing — custom MCP server wrapping mdma CLI for structured, approval-free agent testing; Bash allowlisting suffices for now

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
event_protocol     = Pub/sub event types + topic-prefixed wire format
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

**Environment vars for CLI from laptop:** `MDMA_NODE` sets the node hostname; the CLI derives the gateway address automatically.
```bash
export MDMA_NODE="mdma-909.local"
```

Two external ports: 5555 (gateway) and 5556 (events). No per-service TCP ports exposed.

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

- **2026-02-25:** Smart export: per-category format resolution (`--lossless-format`, `--lossy-format`). Infrastructure: git-flow branching, conventional commits skill, CI split (test all branches / publish main only), 9-step release workflow, devops skill overhaul (Ansible removed, xbps added), documenter agent defined.

- **2026-02-23:** Priorities 7+8 implementation started. library_crawler removed (replaced by library_service). New Priority 9 (Track Export) added — composable `mdma export` command for Rekordbox/CDJ workflow.

- **2026-02-23:** Priority 6 (Web UI Player Controls) complete.
  - Now playing display with track title, artist, album, BPM, key, duration
  - Queue view with remove buttons, clear all
  - Player controls: play queue, stop, skip
  - Library search with artist/BPM/key filters, add-to-queue from results
  - SSE event bridge: live updates from playback pub/sub events
  - Package management, inbox ingestion, bandcamp sync all in single-page UI
  - **Next:** Priority 7 -- ZIP Upload for Library Ingestion

- **2026-02-23:** Priority 5 (Polybar Widget) complete.
  - First external pub/sub consumer — proves event model from a separate process
  - Shell script: subscribes to events, resolves hashes via `mdma search`, outputs to polybar
  - Click actions: play/skip/stop with full gateway path
  - Gateway address as CLI argument (no env var needed from polybar)
  - **Next:** Priority 6 — Web UI Player Controls

- **2026-02-22 (night, late):** Priority 4 (Pub/Sub Events + Play History Facts) fully complete.
  - event_protocol crate with topic-prefixed wire format
  - Playback publishes TrackStarted/Ended/Stopped/QueueChanged via Pub0
  - Gateway bridges IPC events to TCP 5556 (raw byte passthrough)
  - CLI: `mdma subscribe` with artist/title lookup for TrackStarted
  - Played/Skipped facts written to fact stream
  - All deployed, smoke tested (6/6 pass)
  - **Next:** Priority 5 — Polybar Widget

- **2026-02-22 (late):** Priority 3b (Codebase Cleanup) fully complete.
  - 6 dead crates removed, console migrated to gateway_client
  - All clippy warnings resolved, dead code removed
  - Agent workflow established: code-minion → rust-architect → commit
  - `cargo build`, `cargo clippy`, `cargo test` all clean (0 warnings)
  - **Next:** Priority 4 — Pub/Sub Events + Play History Facts

- **2026-02-22 (night):** Roadmap restructured — pub/sub + play history facts moved to priority 4.
  - Pub/sub events (`track_started`, `track_ended`, `position_update`, `queue_changed`) are prerequisite for all live UIs
  - Play history facts: `played` and `skipped` facts via stainless_facts — enables play count, never-played, recently-played queries
  - Polybar widget added as priority 5 — first pub/sub consumer, immediate dev value
  - Web UI moved to priority 6, MP3 to 7, Bandcamp config to 8, Stream management to 9

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
