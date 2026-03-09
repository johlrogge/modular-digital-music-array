# MDMA — Modular Distributed Music Architecture

Version: **0.6.1**

A hi-fi music player for Raspberry Pi 5. Indexes your FLAC and MP3 library, streams to a USB DAC at 192 kHz via PipeWire, and is fully controlled from the command line — composable with dmenu for keyboard-driven browsing and queuing.

The acronym is a nod to electronic music culture. The system exists to keep the music going at parties without being physically tied to equipment.

---

## What's new in 0.6.1

- **Package build fix** — package scripts now handle `version.workspace = true` correctly; all 5 service packages build and publish successfully.
- **Independent base versioning** — each base now carries its own version in its `Cargo.toml`. Bases that use workspace versioning (gateway, bandcamp, acid) are at 0.6.1. Independently versioned bases: mdma-library 0.4.0, mdma-console 0.4.0, mdma-playback 0.4.0, mdma-cli 0.5.0.

---

## What's new in 0.6.0

- **Album Order** — CLI and console album views now sort by DiscNumber then TrackNumber, so albums appear in the correct playing order.
- **AddedAt Tracking** — Every track records when it was added to the library. Query with `--added` in the CLI and sort by added date.
- **Album Art Cache** — Album-level cover art fallback: when a track has no embedded art, MDMA serves the album's cached cover instead.
- **Date Expressions** — New `date_expression` crate with a concise relative date syntax. Use `~` for today, `^` for start-of-period, `$` for end-of-period, `+/-N` for offsets, and `/`-separated components for arbitrary dates. Integrated into all date-based queries including `--added`.
- **CI** — git-hooks input added to devenv.yaml; hook pipeline now runs in CI.

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

# Subscribe to live events (now playing, queue changes)
mdma subscribe
```

Full CLI documentation: [bases/mdma_cli/README.md](bases/mdma_cli/README.md)

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
                                            (auto-discovered sources)
mdma-acid                — standalone fact-writing service (ipc:///run/mdma/acid.sock)

mdma-console             — web UI on port 80
Event bus (port 5556)    — pub/sub for live clients
```

The library is a content-addressed blob store with an immutable fact stream (`stainless_facts`). Every track attribute — artist, BPM, key, play history — is a typed fact appended to `facts.jsonl`. Nothing is ever overwritten. Fact writes go through `mdma-acid`, a dedicated service that owns the `facts.jsonl` file and accepts batched writes from any other service via IPC.

Audio path: FLAC/MP3 → Symphonia decoder → rubato resampler → 192 kHz PipeWire stream → iFi USB DAC

---

## Workspace members

| Base | Description | README |
|------|-------------|--------|
| [beacon](bases/beacon/) | Provisioning server — installs and configures MDMA on a fresh Raspberry Pi | [README](bases/beacon/README.md) |
| [mdma_gateway](bases/mdma_gateway/) | Single TCP gateway (port 5555) routing to all internal IPC services | [README](bases/mdma_gateway/README.md) |
| [mdma_library](bases/mdma_library/) | Library service — content-addressed storage and fact-based metadata | [README](bases/mdma_library/README.md) |
| [mdma_acid](bases/mdma_acid/) | ACID service — standalone append-only fact stream writer | [README](bases/mdma_acid/README.md) |
| [mdma_playback](bases/mdma_playback/) | Audio playback — Symphonia + rubato resampler + PipeWire | [README](bases/mdma_playback/README.md) |
| [mdma_bandcamp](bases/mdma_bandcamp/) | Bandcamp collection sync — downloads purchases into the library inbox | [README](bases/mdma_bandcamp/README.md) |
| [mdma_console](bases/mdma_console/) | Web management console — player controls, search, queue, upload, export | [README](bases/mdma_console/README.md) |
| [mdma_cli](bases/mdma_cli/) | CLI — search, queue, playlists, playback, export, subscribe, shell completions | [README](bases/mdma_cli/README.md) |

**Key components** (shared libraries):

| Component | Description |
|-----------|-------------|
| `date_expression` | Relative date syntax parser used by all date-based queries (`~`, `^`, `$`, `+/-N`, `/`-separated components) |
| `library_search` | Composable `TrackQuery` with string, numeric, duration, key, and date filters |
| `playback_engine` | Real-time audio: Symphonia decoder + rubato resampler + PipeWire output |
| `music_primitives` | BPM, Key, Mode types |
| `storage_primitives` | Type-safe `ByteSize` |
| `media_protocol` | Command/Response protocol between CLI and services |

---

## Getting started

```bash
# Enter reproducible dev environment (Nix/devenv)
devenv shell

# Build everything
cargo build

# Run all tests
cargo test

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
cargo build --release -p mdma-cli
cp target/release/mdma /usr/local/bin/
```

---

See [ROADMAP.md](ROADMAP.md) for detailed status and planned work.
