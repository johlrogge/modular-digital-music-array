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

Audio path: FLAC/MP3 → Symphonia decoder → rubato resampler → 192 kHz PipeWire stream → iFi USB DAC

---

## Workspace members

This is a [Polylith](https://polylith.gitbook.io/polylith/) workspace. Deployable services live in `projects/`; shared logic lives in `components/`; abstract base traits live in `bases/`.

| Service (project) | Description |
|-------------------|-------------|
| `mdma-beacon` | Provisioning server — installs and configures MDMA on a fresh Raspberry Pi |
| `mdma-gateway` | Single TCP gateway (port 5555) routing to all internal IPC services |
| `mdma-library` | Library service — content-addressed storage and fact-based metadata |
| `mdma-acid` | ACID service — standalone append-only fact stream writer |
| `mdma-playback` | Queue manager — drives audio sources via StreamClient; persists queue to `/metadata/queue.json` |
| `mdma-audio` | Audio playback source — wraps PlaybackEngine (Symphonia + rubato + PipeWire), speaks `stream_source_protocol` over NNG |
| `mdma-tui` | Terminal UI — dual-pane browser/queue, modal keybindings, command palette, live queue sync via events, intelligent column compression, bookmark keybinding (`b` in Playback mode) |
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
