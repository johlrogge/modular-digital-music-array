# Modular Distributed Music Architecture (MDMA)

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
mdma queue next           # skip to next
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
