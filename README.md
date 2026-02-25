# MDMA — Modular Distributed Music Architecture

Version: **0.3.1**

MDMA is a distributed DJ system for Raspberry Pi 5 running Void Linux. It moves the music experience from a phone to a dedicated device with professional playback capabilities, using NVMe storage via M.2 HAT.

---

## Workspace members

| Base | Description | README |
|------|-------------|--------|
| [beacon](bases/beacon/) | Provisioning server with web UI — installs and configures MDMA on a fresh Pi | [bases/beacon/README.md](bases/beacon/README.md) |
| [mdma_playback](bases/mdma_playback/) | Real-time audio playback server (Symphonia + PipeWire) | [bases/mdma_playback/README.md](bases/mdma_playback/README.md) |
| [mdma_cli](bases/mdma_cli/) | Command-line interface for controlling MDMA over the gateway | [bases/mdma_cli/README.md](bases/mdma_cli/README.md) |
| [mdma_console](bases/mdma_console/) | Web management console (Axum + Askama) | [bases/mdma_console/README.md](bases/mdma_console/README.md) |
| [mdma_library](bases/mdma_library/) | Music library service — scans, indexes, and serves track metadata | [bases/mdma_library/README.md](bases/mdma_library/README.md) |
| [mdma_bandcamp](bases/mdma_bandcamp/) | Bandcamp download service — fetches purchases and imports to inbox | [bases/mdma_bandcamp/README.md](bases/mdma_bandcamp/README.md) |
| [mdma_gateway](bases/mdma_gateway/) | Single TCP gateway (port 5555) that routes to all internal IPC services | [bases/mdma_gateway/README.md](bases/mdma_gateway/README.md) |

---

## Build quickstart

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

### Cross-compile beacon for Raspberry Pi

```bash
just beacon-cross   # Uses cargo-zigbuild (recommended)
```

### Deploy to a running Pi

```bash
just deploy-dev     # Build and deploy to welcome-to-mdma.local
```

---

## Architecture overview

```
bases/          Binary entry points (services and tools)
  beacon/         Provisioning server
  mdma_playback/  Audio playback
  mdma_cli/       CLI tool
  mdma_console/   Web console
  mdma_library/   Library service
  mdma_bandcamp/  Bandcamp source
  mdma_gateway/   API gateway

components/     Shared libraries
  playback_engine/      Real-time audio (Symphonia + PipeWire)
  music_primitives/     BPM, Key, Mode types
  playback_primitives/  Volume, Deck types
  media_protocol/       Command/response protocol
  storage_primitives/   Type-safe ByteSize
  bandcamp_api/         Bandcamp HTTP client
  gateway_protocol/     Gateway request/response types
  ... (see Cargo.toml for full list)
```

All services communicate over NNG IPC sockets locally. The gateway exposes a single TCP endpoint (`tcp://<host>:5555`) to the outside world.

See [ROADMAP.md](ROADMAP.md) for detailed status and planned work.

---

A hi-fi music player for Raspberry Pi 5. Indexes your FLAC library, streams to a USB DAC at 192 kHz, and is fully controlled from the command line — composable with dmenu for keyboard-driven browsing and queuing.

## What it does today

MDMA runs headlessly on a Pi 5 with an NVMe drive. You control it from your laptop over the network with the `mdma` CLI.

```bash
# Search your library
mdma search "rymden"
mdma search --artist "Carbon Based Lifeforms"
mdma search --artist CBL          # initialism — same result
mdma search --bpm "128+-4"        # 124–132 BPM
mdma search --artist CBL --bpm "128+-4"   # implicit AND

# Discover what's in your library
mdma search fact-values-for Artist
mdma search fact-values-for Label
mdma search fact-values-for Source   # bandcamp, upload, ...

# Queue tracks
mdma queue append <hash>
mdma queue list
mdma queue next <hash>    # prepend a track to play next
mdma queue remove <hash>
mdma queue clear

# Playback
mdma playback play <hash>
mdma playback stop
mdma playback now         # show currently playing track

# Compose with dmenu
mdma search --artist CBL | dmenu | mdma queue append
mdma search fact-values-for Artist | dmenu | xargs -I{} mdma search --artist {}
mdma queue list | dmenu | mdma queue remove
```

## Search syntax

String fields (`--artist`, `--title`, `--album`, `--label`, `--genre`, `--style`):

| Input | Mode | Example match |
|-------|------|--------------|
| `carbon based` | Contains (all words, any order) | "Carbon Based Lifeforms" |
| `CarbBased` | Initialism (CamelCase prefix match) | "Carbon Based Lifeforms" |
| `CBL` | Initialism (all-caps = each letter) | "Carbon Based Lifeforms" |
| `/^Carbon.*/` | Regex | "Carbon Based Lifeforms" |

Numeric fields (`--bpm`, `--year`):

| Input | Meaning |
|-------|---------|
| `128` | Exact (±0.5) |
| `124..132` | Range |
| `128+-4` | 124–132 (symmetric tolerance) |
| `128+4` | 128–132 (only higher) |
| `128-4` | 124–128 (only lower) |

Duration (`--duration`): `7m15s`, `7m`, `>5m`, `<8m`, `6m..8m`

Key (`--key`): `Am`, `A minor`, `8B`, `8B+-1`, `8B+-1~` (include relative key)

## Playlist management

Playlists are plain text files — one hash per line. The Unix pipeline composes them.

```bash
# Save current queue as a playlist
mdma queue list > friday_night.plist

# View a playlist (tty-aware: shows artist/title on terminal, hashes when piped)
cat friday_night.plist | mdma search

# Filter a playlist to tracks by a specific artist
cat friday_night.plist | mdma search --artist=CBL > cbl_set.plist

# Sort playlist alphabetically (stable — chain for multi-key sort)
cat friday_night.plist | mdma sort title -a | mdma sort artist -a > sorted.plist

# Multi-key sort: primary=artist asc, tiebreak=title asc (read right-to-left)
cat friday_night.plist | mdma sort title -a | mdma sort artist -a

# Merge two playlists, deduplicated
sort -u friday_night.plist saturday_night.plist > weekend.plist

# Shuffle and load into queue
cat weekend.plist | shuf | mdma queue append

# Search → sort → queue
mdma search --artist=CBL | mdma sort title -a | mdma queue append

# Filter high-BPM tracks out of the queue
mdma queue list | mdma search --bpm=>128 | mdma queue remove
```

`mdma sort` reads hashes from stdin, fetches metadata, and outputs sorted hashes.
Null values sort last regardless of direction. Stable sort makes chaining correct.

Sort fields: `bpm`, `title`, `artist`, `album`, `duration`

## Architecture

```
Your laptop
    |  TCP (mdma CLI)
    v
Raspberry Pi 5
    |
    +-- mdma-library   (indexes FLAC library, serves search/facts)
    +-- mdma-playback  (Symphonia decoder → rubato resampler → PipeWire → USB DAC)
    +-- mdma-bandcamp  (syncs Bandcamp collection to library)
    +-- mdma-console   (HTTP frontend, stub)
    +-- beacon         (provisioning — how the Pi gets set up in the first place)
```

Audio path: FLAC → Symphonia → rubato resampler (192 kHz) → PipeWire → iFi USB DAC

Library: immutable fact stream (`stainless_facts`) — every track attribute is a typed fact, appended never overwritten. The entire library state can be rebuilt from `facts.jsonl`.

## Setup

**Target:** Raspberry Pi 5, Void Linux, NVMe via M.2 HAT, iFi USB DAC

**Network:** Pi is at `mdma-909.local`. All services are reached through a single gateway on port 5555.

```bash
# Environment (add to shell profile)
export MDMA_NODE="mdma-909.local"
```

The CLI derives the gateway address (`tcp://mdma-909.local:5555`) from `MDMA_NODE` automatically.

**SSH to Pi:**
```bash
just pi-ssh          # provisioned Pi (mdma-909.local)
just pi-ssh-beacon   # unprovisioned Pi (welcome-to-mdma.local)
```

## Installing the CLI (macOS)

Download the latest `mdma-cli-macos-arm64` artifact from [GitHub Actions](https://github.com/johlrogge/modular-digital-music-array/actions/workflows/build-and-package.yml) (select the latest successful run, scroll to Artifacts). Downloading artifacts requires being logged into GitHub.

Or download via the command line (requires [GitHub CLI](https://cli.github.com/)):

```bash
gh run download -R johlrogge/modular-digital-music-array -n mdma-cli-macos-arm64
xattr -d com.apple.quarantine mdma
chmod +x mdma
sudo mv mdma /usr/local/bin/
```

Or build from source:

```bash
# Requires Rust toolchain (https://rustup.rs)
cargo build --release -p mdma-cli
cp target/release/mdma /usr/local/bin/
```

macOS will block the unsigned binary on first run. Remove the quarantine flag:

```bash
xattr -d com.apple.quarantine /usr/local/bin/mdma
```

### Setup

Set `MDMA_NODE` as described in the [Setup](#setup) section above, then reload your shell profile.

### Verify

```bash
mdma --help
mdma source list
```

## Development

Requires: Rust, Zig, cargo-zigbuild, PipeWire libs (enter `devenv shell`)

```bash
# Build and deploy to Pi
just deploy-library    # cross-compile + scp + restart mdma-library
just deploy-playback   # cross-compile + scp + restart mdma-playback

# Local development
just watch             # check → test → build → clippy on file changes
cargo test             # run all tests
```

## What's next

- **API gateway** (`mdma-api`) — single TCP entry point replacing the current per-service ports
- **Pub/sub events** — push notifications for track changes, position, queue updates
- After that: gapless playback, mixlists, MIDI controller (A&H K3)

See [ROADMAP.md](ROADMAP.md) for the full picture.

## Why MDMA?

Modular Distributed Music Architecture. The acronym is a nod to electronic music culture. The system exists to keep the music going at parties without being physically tied to equipment.

## License

MIT — see [LICENSE](LICENSE)
