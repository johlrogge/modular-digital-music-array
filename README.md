# MDMA — Modular Distributed Music Architecture

A hi-fi music player for Raspberry Pi 5. Indexes your FLAC and MP3 library, streams to a USB DAC at 192 kHz via PipeWire, and is fully controlled from the command line — composable with dmenu for keyboard-driven browsing and queuing.

The acronym is a nod to electronic music culture. The system exists to keep the music going at parties without being physically tied to equipment.

---

## What it does

MDMA runs headlessly on a Pi 5 with an NVMe drive. You control it from your laptop over the network.

```bash
# Search — implicit AND, all filters composable
mdma search --artist CBL --bpm "128+-4"

# Filter by when tracks were added (date expressions: ~ = today, ^week = start of week)
mdma search --added "^week"

# Compose with dmenu for keyboard-driven browsing
mdma search fact-values-for Artist | dmenu | xargs -I{} mdma search --artist {}
mdma search --artist CBL | dmenu | mdma queue append

# Pipe composition: search → sort → queue
mdma search --artist CBL | mdma sort title -a | mdma queue append

# Export a set of tracks as AIFF (e.g. for Rekordbox import)
mdma search --bpm "128..132" --key "8A" | mdma export --lossless-format aiff --output ./rekordbox-prep/

# Playlists — create, populate, and compose with pipes
mdma playlist create friday-night
mdma search --genre Techno | mdma playlist add friday-night
mdma playlist get friday-night | mdma queue replace

# Find which playlists contain a track (or a set of tracks from stdin)
mdma search --artist CBL | mdma playlist contains --all

# Bookmark the currently playing track
mdma bookmark

# Bookmark a specific track by content hash, associated with a named set
mdma bookmark sha256:abc123 --scope "friday-night-picks"

# List all bookmarked tracks
mdma bookmarks

# Subscribe to live events (now playing, queue changes)
mdma subscribe
```

---

## Architecture

```
Your laptop (mdma CLI)
        |
        | TCP port 5555
        v
  mdma-gateway          — single entry point, routes all requests
        |
        | local IPC (nng)
   -----+-----+----------+------------+
   |         |           |            |
mdma-library  mdma-playback  mdma-bandcamp  /run/mdma/sources/*.sock
                   |                        (auto-discovered sources)
              mdma-audio        — file playback source (ipc:///run/mdma/streams/audio.sock)

mdma-acid                — standalone fact-writing service (ipc:///run/mdma/acid.sock)

mdma-console             — web UI on port 80
Event bus (port 5556)    — pub/sub for live clients
```

Service startup order: mdma-library → mdma-audio → mdma-playback

The library is a content-addressed blob store with an immutable fact stream (`stainless_facts`). Every track attribute — artist, BPM, key, play history — is a typed fact appended to `facts.jsonl`. Nothing is ever overwritten. Fact writes go through `mdma-acid`, a dedicated service that owns the `facts.jsonl` file and accepts batched writes from any other service via IPC.

Audio path: FLAC/MP3 → Symphonia decoder (std::thread) → rubato resampler → PipeWire stream at source native rate → iFi USB DAC

---

## Workspace members

This is a [Polylith](https://polylith.gitbook.io/polylith/) workspace. Deployable services live in `projects/`; shared logic lives in `components/`; abstract base traits live in `bases/`.

| Service (project) | Description |
|-------------------|-------------|
| `mdma-beacon` | Provisioning server — installs and configures MDMA on a fresh Raspberry Pi |
| `mdma-admin` | System-level operations service (EEPROM, reboot) — root-owned, IPC socket `0660 root:mdma` |
| `mdma-gateway` | Single TCP gateway (port 5555) routing to all internal IPC services |
| `mdma-library` | Library service — content-addressed storage and fact-based metadata |
| `mdma-acid` | ACID service — standalone append-only fact stream writer |
| `mdma-playback` | Queue manager — drives audio sources via StreamClient; persists queue to `/metadata/queue.json` |
| `mdma-audio` | Audio playback source — wraps PlaybackEngine (Symphonia + rubato + PipeWire), speaks `stream_source_protocol` over NNG |
| `mdma-tui` | Terminal UI — multi-pane tabs (nnn-style, `1`–`5`/`6`–`0`), field-aware search grammar (`:artist`, `:bpm`, `:added`, …), DJ shortcuts (`A`, `P`, `d`/`p`, `u`), modal keybindings, command palette, live queue sync, intelligent column compression |
| `mdma-bandcamp` | Bandcamp collection sync — downloads purchases into the library inbox |
| `mdma-console` | Web management console — player controls, search, queue, upload, export |
| `mdma-cli` | CLI — search, queue, playlists, playback, export, subscribe, bookmarks, shell completions |

**Key components** (shared libraries):

| Component | Description |
|-----------|-------------|
| `date_expression` | Relative date syntax parser used by all date-based queries (`~`, `^`, `$`, `+/-N`, `/`-separated components) |
| `library_search` | Composable `TrackQuery` with string, numeric, duration, key, and date filters |
| [`playback_engine`](components/playback_engine/README.md) | Real-time audio: Symphonia decoder + rubato resampler + PipeWire output. Single-track model (`Option<Track>`). |
| `stream_source_protocol` | `StreamCommand`/`StreamResponse`/`StreamTrackInfo`/`StreamPlaybackState` — protocol between playback and audio source services |
| `music_primitives` | BPM, Key, Mode types |
| `storage_primitives` | Type-safe `ByteSize` |
| `media_protocol` | Command/Response protocol between CLI and services |

---

## Getting started

```bash
# Enter reproducible dev environment (Nix/devenv)
devenv shell

# Build everything (Polylith workspace)
cargo polylith cargo --profile dev build

# Run all tests
cargo polylith cargo --profile dev test

# Watch mode: check → test → build → clippy on save
just watch
```

Cross-compile and deploy to a Raspberry Pi:

```bash
just beacon-cross       # cross-compile beacon for aarch64
just deploy-dev         # build and deploy all services to welcome-to-mdma.local
```

From the laptop, set `MDMA_NODE` and all CLI commands route to the Pi automatically:

```bash
export MDMA_NODE="mdma-909.local"
mdma ping
```

---

## Installing the CLI (macOS)

Download the latest `mdma-cli-macos-arm64` artifact from [GitHub Actions](https://github.com/johlrogge/modular-digital-music-array/actions/workflows/build-and-package.yml), or use the GitHub CLI:

```bash
gh run download -R johlrogge/modular-digital-music-array -n mdma-cli-macos-arm64
xattr -d com.apple.quarantine mdma
chmod +x mdma
sudo mv mdma /usr/local/bin/
```

Or build from source:

```bash
cargo polylith cargo --profile dev build --release -p mdma-cli
cp profiles/dev/target/release/mdma /usr/local/bin/
```

---

See [ROADMAP.md](ROADMAP.md) for detailed status and planned work.

---

## What's new in 0.24.0

`mdma track replace` is now a **hard replace**: the old track's facts are fully retracted and the old hash stops resolving entirely — it disappears from search and list results, not just hidden. The new track records a `Replaces` provenance link. Playlists referencing the old hash are rewritten immediately; any that still point at it self-heal on read by following the replacement chain (multi-step chains are followed automatically).

`mdma track orphans` now lists blobs on disk with no live track reference: hard-replace leftovers appear with reason `no live facts`, alongside soft-deleted tracks (reason `deleted`). The command is read-only — a GC candidate list only.

`mdma track delete` / `restore` are unchanged (soft, reversible).

---

## What's new in 0.23.0

Track lifecycle: `mdma track replace` swaps in a better-quality version of a track and rewrites every playlist that references the old one in place. `mdma track delete`/`restore` give you soft-delete with full recovery, and `mdma track orphans` surfaces hidden tracks for future garbage collection. CDJ-safe exports: `mdma rekordbox export` now downsamples sources above 48 kHz via soxr so Pioneer CDJs never reject the output as illegal format.

### Earlier in 0.22.1

Packaging hotfix: runtime dependencies are now correctly embedded in all built `.xbps` packages. Fresh provisions no longer require a manual `xbps-install -y ffmpeg` to get rekordbox AIFF export working.

### Earlier in 0.22.0

Service mode gives you a button on the web console (and `mdma admin service-mode enable`) to flip the Pi's EEPROM `BOOT_ORDER` to SD-first, so the next reboot drops straight onto the beacon SD for reprovisioning — no SSH, no physical hardware removal required. After provisioning completes, the beacon auto-reverts `BOOT_ORDER` to NVMe-first. A new root-owned `mdma-admin` service owns all system-level operations; its IPC socket is `0660 root:mdma` so only the gateway can dispatch privileged commands.

### Earlier in 0.21.0

Pi 5 NVMe boot is now reliable out of the box: the provisioner sets the correct GPT partition type GUID (Microsoft Basic Data) and writes `PCIE_PROBE=1` to EEPROM — without both, fresh Pi 5 units ignore the NVMe slot entirely. The beacon form also gains an always-available "Force Wipe and Re-provision" button (hostname-typing confirmation required) so destructive reprovisioning no longer requires hitting an error state first.

### Earlier in 0.20.2

Hotfix for `mdma rekordbox export`. Tracks that were present in `<COLLECTION>` were sometimes missing from `<PLAYLIST>` entries, and re-running export accumulated duplicate `<COLLECTION>` entries for tracks already written. Both symptoms were caused by a case mismatch in `file://` URI comparison; URIs are now normalised before lookup.

### Earlier in 0.20.0

`mdma rekordbox export` — export the library (or a piped set of tracks) as a `rekordbox.xml` file for direct Rekordbox import. Tracks are transcoded to AIFF and arranged in an artist/album directory tree. Playlist structure is preserved. Subsequent runs update the XML in-place without re-transcoding existing tracks.

### Earlier in 0.18.0

`mdma fact add` and `mdma fact retract` — edit track metadata directly from the CLI without re-importing or running a rekordbox sync. Covers 20 user-facing fields; built on the `FactsAsserted`/`FactsRetracted` protocol split from 0.17.0.

### Earlier in 0.17.0

**Breaking change** — `AcidEvent::FactsWritten` is replaced by `FactsAsserted` and `FactsRetracted`. External subscribers must update to the `acid/` prefix and match both new event variants. Also fixes a latent cursor-drift bug where the ACID cursor never advanced on `RetractOk`.

### Earlier in 0.16.7

The upload dialog's file picker now includes `.m4a`. The 0.16.6 server-side M4A ingest support was gated by a missing `accept=` entry — phones and desktop browsers hid M4A files in the chooser. The misleading `.aif`/`.aiff` entries (export-only) are also removed.

### Earlier in 0.16.6

M4A files (MPEG-4 audio — AAC or ALAC) are now ingestible alongside FLAC, MP3, and WAV. DRM-protected files surface as decode errors; filename-derived metadata fallback applies as with WAV.

### Earlier in 0.16.5

WAV files are now ingestible. When no embedded tags are present, title and artist are derived from the filename stem using a `" - "` split; a later manual edit overrides the derived facts naturally.

### Earlier in 0.16.4

Hotfix — systematic audit of all runit run scripts (#85) found the same root-owned-directory pattern from the 0.16.1–0.16.3 hotfixes in three more services; `mdma-acid`, `mdma-bandcamp`, and `mdma-library` now all `chown` their working directories to `mdma:mdma` before dropping privileges.

### Earlier in 0.16.3

Hotfix — 0.16.2 added `/music/cover-art` to the `chown` line in the `mdma-library` runit run script but missed the matching `mkdir -p`, so the directory was never created and the chown silently no-op'd; 0.16.3 adds the missing `mkdir -p` entry.

### Earlier in 0.16.2

Hotfix — cover-art directory creation failed after service restart with the same root-ownership issue that affected `/music/inbox` in 0.16.1. `/music/cover-art` is now included in the `chown mdma:mdma` line in the `mdma-library` runit run script.

### Earlier in 0.16.1

Hotfix — bandcamp sync failed after service restart due to `/music/inbox` and `/music/blobs` being left owned by `root` on each `mdma-library` startup. The runit run script now chowns those directories to `mdma:mdma` before dropping privileges.

### Earlier in 0.16.0

Minor release — ACID is now the sole reader and writer for all library facts.

### Library — ACID sole source of truth (#71)

All library fact reads and writes now go through ACID over IPC. Previously, the library read `facts.jsonl` directly from disk in five places and wrote retractions directly in two more. Those bypasses are gone.

New ACID protocol operations added to support the migration: `RetractFacts`, `ReadEntity`, `RetractOk`, and `EntityFacts`. `IndexedTrackInfo` gained an `item_id` field. A new `event_cursor` enables incremental `TrackStarted`/`TrackStopped` reads so services can ask for facts since a known position rather than replaying the full stream.

ADR-001 (`docs/adr/001-acid-sole-writer-of-facts-jsonl.md`) is now **Accepted**.

### Packaging

INSTALL scripts for all 8 services now create `/var/log/<svc>` at install time. This was previously missing for `mdma-audio` and `mdma-acid`.

### Earlier in this series (0.15.4 – 0.15.5)

- Library bootstraps fully from ACID on startup; no file fallback; fails loud if ACID is unreachable.
- Cursor-on-disk removed — no more silent restart drift.
- ACID logs its active backend at startup.
- `replay_from_file` added to the in-memory ACID backend for dev/test seeding.
- Production profile wired into CI.

---

## What's new in 0.14.0

### Console

- **Queue playlists** — send a whole playlist to the queue directly from the web console.
- **Reorder queue** — move entries up/down with ↑/↓ buttons.

### CLI

- **`--not` with stdin excludes** — pipe track IDs into `mdma search --not` to exclude them from results.
- **`--played never`** — filter for tracks that have never been played.
- **Sort by `start`/`stop`** — `mdma sort start` and `mdma sort stop` order by last-started / last-stopped timestamps.

### Library

- `TrackInfo` now exposes `last_started` and `last_stopped` fields.

---

## What's new in 0.13.0

### Search and filters

- **Field-aware search grammar** in SearchPane: `:artist 'bonobo'`, `:bpm 120`, `:title 'foo bar'`, `:added -7`, etc. Bare words search `any_text` (title/artist/album/label/genre).
- **Live search** — results update on every keystroke.
- `s` and `/` filter the current list live (pane display-string match). `,` clears selection.
- Genre counts show real track numbers.
- `:history [days]` opens a SearchPane preset with `:started '-N..~'`. Default 7 days.
- CLI: `mdma search --added -7` now accepts hyphen-prefixed date expressions.

### Multi-pane tabs (nnn-style)

- **5 tab slots per side.** Keys `1`–`5` = left side; `6`–`9`, `0` = right side.
- Pressing a key for an empty slot clones the currently-focused pane into it.
- No explicit close — tabs overwrite by navigation or by being cloned into.
- Tab bar with LRU-priority shrinking of inactive titles.

### DJ-workflow shortcuts

| Key | Action |
|-----|--------|
| `A` | Add currently-playing track to focused playlist or queue |
| `P` | Play selected track(s) immediately (queue_next + skip, queue preserved) |
| `x` / `X` | Select for cut |
| `d` | Cut selection to clipboard |
| `p` | Paste after cursor |
| `Shift+J` / `Shift+K` | Move selected block up/down (Kakoune/Helix semantics) |
| `u` | Undo last mutation on the active pane (playlist-only in v1) |

### UX fixes

- Panes with text input capture all keys until `Esc` — no more `a`/`s`/`P` hijacking during search typing.
- Palette no longer eats `h`/`l` in `:help` etc.
- Album drill-down default sort: disc asc, track asc.
- Inbox scanner ignores macOS AppleDouble sidecar files (`._*`).

### Playback engine

- MP3 decoder no longer strips zero-padding from decoded segments — fixes scratchy/roboty MP3 playback.
- Decoder moved from tokio task to `std::thread` to avoid IPC contention.
- Mixer no longer busy-spins on a full output ring.
- Mixer no longer silence-pads when the track buffer briefly underruns.
- Output uses the source's **native sample rate** — 44.1 kHz MP3 is no longer resampled to 192 kHz unnecessarily.
- Flush on track change — skip latency ~50 ms instead of seconds.
- `allowed-rates` PipeWire config added so the graph can switch rate natively.

### Library

- `mdma-library` creates `playlists/` on startup and before each write — fresh installs can reorder playlists immediately.
- Retract-aware fact fold.

### Bandcamp

- `mdma source check-item <source> <id>` and `mdma source check-updates <source> [--apply]` — on-demand stale-item detection via track-count comparison.
- `mdma source resync <source> <id>` — force-resync for manual override.

---

## What's new in 0.11.0

- **Polylith workspace migration** — root `Cargo.toml` is now a stub. All builds go through `cargo polylith cargo --profile <profile> <cmd>`. Dev builds use the `dev` profile; production (mdma-acid) uses the `production` profile.
- **Bookmarks** — `mdma bookmark [<hash>] [--scope <set-name>]` bookmarks the currently playing track (or a specific track by hash). `mdma bookmarks` lists all bookmarked tracks. Bookmarks are stored as `Bookmarked` facts in the ACID store with `FactOrigin::User` provenance.
- **TUI bookmark keybinding** — in Playback mode, pressing `b` bookmarks the currently playing track immediately.
- **`FactOrigin::User`** — new variant in `music_facts::FactOrigin` for facts initiated directly by the user (e.g. bookmarks), distinct from ingestion-time origins.

---

## What's new in 0.9.0

- **mdma-tui** (v0.3.0) — intelligent column compression in track lists. BPM and key columns are dropped first when horizontal space is tight, then duration, then artist. The artist/title separator ` — ` is hidden when the artist column is not shown. Artist compresses before title so the title remains visible as long as possible.

---

## What's new in 0.8.1

- **playback_engine** (v0.8.1) — removed multi-deck abstraction. The engine now manages a single `Option<Track>` directly, with no phantom type parameters or deck-indexed state. Atomic orderings corrected for aarch64: `SeqCst` replaced with `Acquire`/`Release` pairs throughout.
- **mdma-audio** (v0.1.1) — updated to match the simplified `PlaybackEngine` API. No behaviour change; single-track model was already the effective usage.

---

## What's new in 0.8.0

- **mdma-tui** (v0.2.0) — terminal UI client. Dual-pane layout (browser/queue by default). Modal keybindings: Normal mode for navigation, Playback mode (`p`) for transport controls. Command palette (`:` prefix) for play/pause/stop/next/clear/shuffle/quit and pane switching. `q` appends the selection to the queue; `Q` inserts it next. `?` opens a help overlay. Live queue sync: subscribes to the event bus and refreshes queue panes automatically on `QueueChanged`.
- **mdma-playback** (v0.6.0) — `Play`/`PlayQueue` now resumes a paused track instead of popping the next queue entry, matching expected toggle behaviour.
- **nng-transport** — all client sockets now apply a 5-second send/receive timeout. Connections to an unreachable Pi no longer hang indefinitely.

---

## What's new in 0.7.0

- **mdma-audio** (v0.1.0) — new service that wraps PlaybackEngine and speaks `stream_source_protocol` over NNG Rep0 at `ipc:///run/mdma/streams/audio.sock`. Resolves content hashes to file paths via the library IPC.
- **mdma-playback** (v0.5.0) — now a pure queue manager. No longer has a direct PlaybackEngine dependency. Drives `mdma-audio` (and future sources) via `StreamClient`. Queue entries carry `source: String` (default `"audio"`) instead of a file path. Queue persists to `/metadata/queue.json`.
- **stream_source_protocol** (v0.1.0) — new component defining the `StreamCommand`/`StreamResponse`/`StreamTrackInfo`/`StreamPlaybackState` types used between playback and source services.
- **Bug fixes** — playback errors now propagate correctly; `RUST_LOG=info` added to all service run scripts for visible logging.

---

## What's new in 0.6.2

- **Date expression fix** — single-component date expressions (e.g. `15`) are now interpreted as day, not year, matching the positional spec (most-significant to least-significant: year/month/day; specify only the least-significant components you care about). `15` means the 15th of the current month; `3/15` means March 15th of the current year.
- **CLI `--added` help text** — the help string for `--added` now correctly describes date expression syntax and the positional component order.

---

## What's new in 0.6.1

- **Package build fix** — package scripts now handle `version.workspace = true` correctly; all 5 service packages build and publish successfully.
- **Independent base versioning** — each base now carries its own version in its `Cargo.toml`. mdma-gateway, mdma-bandcamp, and mdma-acid are at 0.6.0. mdma-library 0.4.0, mdma-console 0.4.0, mdma-playback 0.4.0, mdma-cli 0.5.1.

---

## What's new in 0.6.0

- **Album Order** — CLI and console album views now sort by DiscNumber then TrackNumber, so albums appear in the correct playing order.
- **AddedAt Tracking** — Every track records when it was added to the library. Query with `--added` in the CLI and sort by added date.
- **Album Art Cache** — Album-level cover art fallback: when a track has no embedded art, MDMA serves the album's cached cover instead.
- **Date Expressions** — New `date_expression` crate with a concise relative date syntax. Use `~` for today, `^` for start-of-period, `$` for end-of-period, `+/-N` for offsets, and `/`-separated components for arbitrary dates. Integrated into all date-based queries including `--added`.
- **CI** — git-hooks input added to devenv.yaml; hook pipeline now runs in CI.
