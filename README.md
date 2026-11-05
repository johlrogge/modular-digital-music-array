# MDMA — Modular Distributed Music Architecture

A hi-fi music player for Raspberry Pi 5. Indexes your FLAC and MP3 library, streams to a USB DAC at 192 kHz via PipeWire, and is fully controlled from the command line — composable with dmenu for keyboard-driven browsing and queuing.

The acronym is a nod to electronic music culture. The system exists to keep the music going at parties without being physically tied to equipment.

---

## What it does

MDMA runs headlessly on a Pi 5 with an NVMe drive. You control it from your laptop over the network.

```bash
# Search — implicit AND, all filters composable
mdma search --artist CBL --bpm "128+-4"

# Filter by role, energy, and key — new in 0.25.0
mdma search --role banger --bpm "128+-3" --key "8A~"
mdma search --energy "8..10" --genre Techno

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
mdma bookmark sha256:abc12345 --scope "friday-night-picks"

# List all bookmarked tracks
mdma bookmarks

# Hand-tag role and energy — new in 0.25.0
mdma fact set sha256:abc12345 role banger
mdma fact set sha256:abc12345 energy 9

# Import Rekordbox XML and land beat-grid / memory-cue facts — new in 0.25.0
mdma rekordbox import --enrich rekordbox.xml

# Export memory cues back to Rekordbox (MDMA never writes beat grids back)
mdma rekordbox export --output rekordbox-out.xml

# List your Bandcamp playlists and import one (preview with --dry-run)
# Import runs as a background job on the node — new in 0.33.0
mdma source bandcamp playlists
mdma source bandcamp import-playlist my-playlist --dry-run
mdma source bandcamp import-playlist my-playlist --no-wait
mdma source bandcamp import-status

# Compute AudioHash (and PcmHash with --pcm) for existing tracks — new in 0.36.0
mdma library identity backfill --pcm
mdma library identity status

# Find and merge duplicates; merged-away files go to /music/archive — new in 0.36.0
mdma library dedup
mdma library dedup --apply
mdma library dedup merge <winner> <loser> -y
mdma library archive
mdma library archive empty --yes

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

This is a [Polylith](https://polylith.gitbook.io/polylith/) workspace. Deployable services live in `projects/`; shared logic lives in `components/`; abstract base traits live in `bases/` (client, service, http_server, tui, bevy-app).

| Service (project) | Description |
|-------------------|-------------|
| `mdma-beacon` | Provisioning server — installs and configures MDMA on a fresh Raspberry Pi, with optional wifi (ethernet preferred, wifi fallback) |
| `mdma-admin` | System-level operations service (EEPROM, reboot) — root-owned, IPC socket `0660 root:mdma` |
| `mdma-gateway` | Single TCP gateway (port 5555) routing to all internal IPC services |
| `mdma-library` | Library service — content-addressed storage and fact-based metadata |
| `mdma-acid` | ACID service — standalone append-only fact stream writer |
| `mdma-playback` | Queue manager — drives audio sources via StreamClient; persists queue to `/metadata/queue.json` |
| `mdma-audio` | Audio playback source — wraps PlaybackEngine (Symphonia + rubato + PipeWire), speaks `stream_source_protocol` over NNG |
| `mdma-tui` | Terminal UI — multi-pane tabs (nnn-style, `1`–`5`/`6`–`0`), field-aware search grammar (`:artist`, `:bpm`, `:added`, …), DJ shortcuts (`A`, `P`, `d`/`p`, `u`), modal keybindings, command palette, live queue sync, intelligent column compression |
| `mdma-bandcamp` | Bandcamp source — collection sync into the library inbox, fan playlist listing/fetching, and background playlist import jobs for `mdma source bandcamp` |
| `mdma-console` | Web management console — player controls, search, queue, upload, export |
| `mdma-cli` | CLI — search, queue, playlists, playback, export, subscribe, bookmarks, Bandcamp playlist import, shell completions |
| `dj-workspace` | Bevy GUI app (laptop-only, not packaged for Pi) — playlist browser and BPM/Key/Role/Energy candidate finder with sortable results; connects via gateway (`dj-workspace --node <host>`) |

**Key components** (shared libraries):

| Component | Description |
|-----------|-------------|
| `date_expression` | Relative date syntax parser used by all date-based queries (`~`, `^`, `$`, `+/-N`, `/`-separated components) |
| `library_search` | Composable `TrackQuery` with string, numeric, duration, key, and date filters |
| [`playback_engine`](components/playback_engine/README.md) | Real-time audio: Symphonia decoder + rubato resampler + PipeWire output. Single-track model (`Option<Track>`). |
| `stream_source_protocol` | `StreamCommand`/`StreamResponse`/`StreamTrackInfo`/`StreamPlaybackState` — protocol between playback and audio source services |
| `music_primitives` | BPM, Key, Mode, TrackRole types |
| `music_facts` | `MusicValue` fact variants: BPM, Key, BeatGrid, MemoryCue, Role (TrackRole), Energy (1–10), and more; `FactOrigin` provenance enum |
| `storage_primitives` | Type-safe `ByteSize` |
| `media_protocol` | Command/Response protocol between CLI and services |
| `private_file` | Atomic writer for credential files (unique temp file, fsync, rename, exact mode; symlinks at the target are replaced) |
| `wifi_config` | Validated SSID, passphrase and country types; WPA PSK derivation and wpa_supplicant config rendering |
| `rekordbox_roundtrip` | Maps rekordbox beat-grid anchors and position marks to/from MDMA facts; used by `mdma rekordbox import --enrich` and `mdma rekordbox export` |

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
just deploy-library     # deploy one service; optional host, defaults to prod (PI_HOST)
just deploy-library mdma-stage.local
```

Deploy recipes use the project ssh config with BatchMode, so they never hang on a host-key prompt.

From the laptop on the same network, the CLI finds nodes on its own:

```bash
mdma node list      # discover nodes (confirmed by Identify)
mdma node select    # pick and remember one
mdma ping
```

`MDMA_NODE` and `--node` still override discovery:

```bash
export MDMA_NODE="mdma-909.local"
mdma ping
```

`--node` and `MDMA_NODE` take `host[:port]` (port 1-65535). A bad `--node` is a usage error (exit 2). A bad `MDMA_NODE` is reported only when a command needs a node; it exits 1 and names the variable.

### Help, errors and completions

```bash
mdma search --help      # help for one command (there is no `mdma help <cmd>`)
mdma source bandcamp --help   # `source NAME --help` shows the source group's page
mdma --version          # prints "Version: x.y.z"

# Shell completions: bash, zsh or fish
mdma generate-completions bash > ~/.local/share/bash-completion/completions/mdma
```

`generate-completions` is hidden from `--help`. elvish and PowerShell completions are no longer provided. Regenerate any completion script you installed earlier.

Bad argument values are rejected while parsing, before anything connects, with the parser's error text and exit code 2. Runtime failures (node unreachable, service error) exit 1. Track hashes on the command line need at least 8 lowercase hex characters, with an optional `sha256:` prefix. A date option value that starts with a dash must be glued to the flag: `--added=-7` (`--added -7` is rejected).

---

## Installing on macOS

Copy `scripts/install-mdma-macos.sh` from this repo to your Mac and run it. It requires the `gh` CLI (authenticated) and installs both `mdma` and `mdma-tui` to `/usr/local/bin/` by default.

```bash
# Install latest release
bash install-mdma-macos.sh

# Install a specific tag
bash install-mdma-macos.sh v0.36.1

# Install to a user-writable location (no sudo)
MDMA_INSTALL_DIR=~/.local/bin bash install-mdma-macos.sh
```

The script resolves the tag to its CI run, downloads both macOS arm64 artifacts, strips the quarantine attribute, and installs with correct permissions. Artifacts expire after roughly 90 days; if none are found the script will tell you how to trigger a rebuild via `workflow_dispatch`.

Or build from source:

```bash
cargo polylith cargo --profile dev build --release -p mdma-cli
cp profiles/dev/target/release/mdma /usr/local/bin/
```

---

See [ROADMAP.md](ROADMAP.md) for detailed status and planned work.

---

## Unreleased

### Breaking: mdma-cli moves from clap to hicli 0.2 (ADR-015)

Bad argument values are now usage errors (exit 2, parser's error text) raised before anything connects; runtime failures exit 1. Scripts that parse the old messages or depend on the loosened values below need updating. See CHANGELOG.md for the full list.

## What's new in 0.36.1

### Early facts are no longer skipped at startup

The library used to skip about 370 lines written by the first releases. They are now read: `HasAlbumArt` stored as `true`/`false` is read as present/absent, and the early play events `Played` and `Skipped` are read as `TrackStopped` (completed or skipped). This restores album-art flags and the earliest play history on existing libraries. The facts log is not rewritten, and new facts use the current format. See CHANGELOG.md.

---

## What's new in 0.36.0

### Identity backfill, duplicate merge and archive

`mdma library identity backfill [--pcm] [--no-wait]`, `status` and `cancel` compute AudioHash, and PcmHash on request, for existing tracks, at low priority. AudioHash is now computed at ingest.

PcmHash v1 (ADR-012) is a container-independent identity hash over decoded lossless samples (FLAC, WAV, AIFF); `--pcm` computes it.

`mdma library dedup` lists duplicate groups: automatic, or needs confirmation with reasons (ADR-013). `--apply` merges the automatic groups; `mdma library dedup merge <winner> <loser>... [-y]` merges a confirmed group. Latest fact wins per attribute, the newest copy is kept, ItemIds are unioned, and play history follows `Replaces`. A re-downloaded copy of the same purchase with the same audio now merges automatically at ingest.

Merged-away files go to `/music/archive/`, never deleted directly. `mdma library archive` lists them; `mdma library archive empty [--yes]` is the only way to delete them permanently, and it deletes only what was listed.

Bandcamp sync defers, and resync and check refuse, when the library is unreachable, instead of re-downloading everything. `just deploy-<svc> [host]` takes an optional host (default prod / `PI_HOST`) and never hangs on a host-key prompt.

Provisioned nodes now run socklog as the syslog daemon; logs go under `/var/log/socklog/`. avahi-daemon is enabled on upgrade, which fixes mDNS discovery on nodes where it was missing.

ADR-012 is accepted, ADR-013 is new, and ADR-014 (merge by fold) is proposed.

**Upgrade notes:** BREAKING. The library IPC protocol changed (dedup and archive messages; `blobs_removed` renamed `blobs_archived`). Library, gateway, console and CLI must be upgraded together: upgrade all mdma-* packages together, by name. See CHANGELOG.md.

---

## What's new in 0.35.0

### New `audio_hash` component (not yet active)

A new `audio_hash` component computes an AudioHash: a tag-independent identity hash over container-defined byte ranges (FLAC audio frames; MP3 frames including Xing/LAME; WAV `fmt ` plus `data`; MP4/M4A `mdat`). Trailing APEv2, Lyrics3v2, ID3v2-footer and ID3v1 tags are stripped, strongest signature first. It never decodes, never panics on malformed input, and is versioned as `audio-sha256-v1:<hex>`. A validated `AudioHash` newtype and a `MusicValue::AudioHash` fact variant come with it. ADR-010 is accepted with the exact frozen v1 definition (refs #55).

Ingest does not compute or write AudioHash yet, and there is no backfill; both come in a later release. No user-visible behaviour change.

**Upgrade notes:** `MusicValue` gained a variant. Per ADR-004, redeploy all services that embed the protocol together (gateway, library, console): upgrade all mdma-* packages together, by name.

**Known issues:** #81, #84, #85, #86 and #87 carry over from 0.34.0. See CHANGELOG.md.

---

## What's new in 0.34.0

### Library: read-your-writes projection

Every library write now reaches the in-memory index through one `catch_up` path (read stream, apply, advance cursor, under a lock), shared with the fact subscriber. This fixes facts being applied twice after ingest, which left duplicate memory cues and styles in memory until restart and could leak into a rekordbox export taken in that window. It also fixes counter drift. `WriteFact`, `RetractFact` and bookmarks now show up immediately.

Also fixed: back-to-back track replaces no longer lose `Replaces` ancestry (#44); replaced or retracted hashes no longer reappear as empty entries after replay (a track exists only while it has live facts); a lock-order inversion in the ingest dedup path (deadlock); and retracting one source's facts now keeps values another source still asserts.

Play-event-only phantom entries written under short or empty hashes by older playback (#86) are no longer listed as tracks. On prod data the track count goes from 983 to 870. Removing the empty-key phantom also makes playlist lazy repair work again; before, every playlist line counted as live.

ADR-011 is new and ADR-004 is amended.

**Upgrade notes:** No protocol change; only mdma-library behaviour changes. Upgrade all mdma-* packages together, by name. Expect `tracks_indexed` / the library track count to drop by the number of phantom entries.

**Known issues:** #81, #84, #85 and #86 carry over from 0.33.0; new #87 (library projection follow-ups: retract scan cost, cover cache not refreshed while running). See CHANGELOG.md.

---

## What's new in 0.33.0

### Bandcamp playlist import runs on the node

`mdma source bandcamp import-playlist <slug|url>` now starts a background job in mdma-bandcamp. It fetches the playlist and matches it against the library. If owned tracks are missing, it runs a full collection sync and waits for the downloads, then re-matches and writes the playlist once. It never writes an empty playlist, and it replaces an existing one. The CLI polls for progress.

`--no-wait` returns right after starting the job; `mdma source bandcamp import-status` shows the job's progress and result. `--dry-run` reports matches and writes nothing. Paused or timed-out downloads are reported in the result. If an import is already running, a new one is refused ("Import NOT started", exit 1).

Playlist matching and writing now live in a shared `playlist_import` component, which the rekordbox import uses too.

### Bandcamp sync fixes

Dedup no longer holds the download queue lock during library calls, so status polls don't stall. Items already queued or downloading are not queued twice, by sync, import or resync. Cancelling a queued item ends any wait on it.

### rekordbox export path length

New export paths are capped at 250 bytes of the encoded rekordbox Location, measured in NFD the way macOS stores names; rekordbox on macOS rejects paths of about 256 bytes and halts the whole import. Long names are shortened title first, then album, then artist, each with a stable `~hash` suffix. Paths already in the existing `rekordbox.xml` are kept, so rekordbox keeps their cues; `--replace` skips this.

**Upgrade notes:** `SourceRequest`/`SourceResponse` gained import variants, so mdma-gateway, mdma-bandcamp and the CLI must all be 0.33.0. Upgrade the mdma-* packages by name, never a bare `xbps-install -Su`, which would pull in the rpi-kernel on unmigrated units.

**Known issues:** `mdma source bandcamp sync` still times out client-side, though the sync continues on the node (#81); the gateway serialises requests (#84); rekordbox import treats artist+title matches with different durations as matched (#85); playback writes play events under short hashes (#86). See CHANGELOG.md.

---

## What's new in 0.32.0

### Bandcamp playlist import

`mdma source bandcamp playlists` lists your Bandcamp fan playlists, private ones included. `mdma source bandcamp import-playlist <slug-or-url> [--name N] [--dry-run]` imports one into an MDMA playlist in the same order, matching tracks against the library by artist, title and duration.

It prints a per-track report: `✓` matched, `?` ambiguous or duration differs (not written), `✗` not in library. It warns if fewer tracks were fetched than the playlist has, and aborts without writing if a library lookup fails. Re-running replaces the playlist, including manual edits. The Bandcamp service fetches through the Pi's stored cookies and accepts only https bandcamp.com URLs.

The source protocol gains `ListPlaylists` and `GetPlaylist` requests and an `Unsupported` error. mdma-bandcamp now answers unparseable requests with an error instead of stalling the gateway. The rekordbox and Bandcamp imports share playlist writing, which only replaces on "already exists" and no longer silently drops tracks on a lookup failure.

**Upgrade notes:** mdma-gateway, mdma-bandcamp and the CLI must all be 0.32.0; older gateways reject the new requests. The `mdma source bandcamp` commands use a 30 s CLI timeout but the gateway caps source requests at 10 s and is blocked while one runs, so don't run imports or syncs during playback (#81). Units provisioned before 0.26.1 that haven't run migrate-bootfs: name the mdma-* packages explicitly, never a bare `xbps-install -Su`.

**Known issues:** long source operations block the gateway (#81); avahi renames the node to `-2` when ethernet and wifi share a subnet (#80); wifi association delays after boot (#79); Argon NEO 5 + wifi NVMe instability (#74, #69). See CHANGELOG.md.

---

## Unreleased

### System log capture with socklog (#89)

wpa_supplicant, dhcpcd and avahi-daemon log through `vlogger` to `/dev/log`. There was no syslog daemon, so those logs were lost. Nodes now run `socklog-unix` (package `socklog-void`), which writes rotated logs per facility under `/var/log/socklog/`. Daemon-facility messages from those three services are in `/var/log/socklog/daemon/current`; for a wifi dropout, `grep -E 'wpa_supplicant|dhcpcd' /var/log/socklog/daemon/current`. Kernel messages stay in `/var/log/mdma-klog`.

New installs enable it during provisioning; existing nodes get it (and avahi-daemon, #80) on the mdma-admin upgrade. To disable permanently: `touch /etc/sv/socklog-unix/down`.

---

## What's new in 0.31.0

### Clean power-button shutdown and persistent kernel log (#77)

A short press on the Pi 5 power button now shuts the node down cleanly. mdma-admin depends on acpid and Void's stock handler runs `shutdown -P now`, so runit stops the services in order, a wtmp shutdown record is written, and the next boot needs no filesystem recovery. Verified on stage hardware. A long press is still a hard power cut by the PMIC; avoid it.

The new `mdma-klog` service writes the kernel log to `/var/log/mdma-klog` (svlogd rotation, about 20 MB cap) and replays the ring buffer once per boot, so the previous boot's kernel messages survive a reboot or power cut. After a hard cut the last seconds may be missing, and an NVMe dropout cannot write its own errors to the NVMe.

New installs enable both during provisioning; existing nodes get them on the mdma-admin upgrade. The beacon SD also gets acpid. To disable either permanently: `touch /etc/sv/<svc>/down` (upgrades re-link a removed `/var/service` link). See CHANGELOG.md.

---

## What's new in 0.30.0

### The CLI finds nodes on its own (#73)

Nodes announce `_mdma._tcp` over mDNS and the gateway answers a new Identify request, so `MDMA_NODE` is no longer required. The CLI resolves its target in order: `--node` then `MDMA_NODE`, local IPC on the Pi, the remembered node (verified by Identify), then discovery or an interactive pick. mDNS results are only candidates; every address is confirmed by Identify before it is used or remembered, so a swapped Pi on the same IP is never silently targeted.

New commands: `mdma node list`, `select`, `forget` and `show`. The choice is cached at `~/.config/mdma/node.json` on Linux (under Application Support on macOS, where first use shows a Local Network permission prompt).

Gateways at 0.29.0 or older answer Identify as "unsupported" and are not remembered through discovery; use `--node` until they are updated. The TUI and dj-workspace still use only `MDMA_NODE`.

### Packaging fix (#75)

Packages are now assembled from `void-packages/srcpkgs`. The 0.29.0 beacon package lacked the `beacon-wifi` service and its dependencies, so the wifi fallback could not start; 0.30.0 ships them (beacon SD cards on 0.29.0 should self-update). The gateway now depends on avahi and withdraws its mDNS announcement on uninstall. CI verifies every built package against its declaration. See CHANGELOG.md.

---

## What's new in 0.29.0

### The beacon reuses the installed system's wifi (#72)

With no ethernet, the beacon now borrows the wifi of the installed system, so service mode, recovery and reinstall work on wifi-only units without a cable. It mounts the root partition read-only, never touches `/music`, and never touches the NVMe at all when ethernet is linked. The wifi config is copied verbatim into tmpfs, so a 5 GHz `freq_list` pin survives, and credentials never land on the SD card. The UI shows the network source and warns about provisioning over wifi (#69).

Reinstall, Wipe all, and Preserve with a reformatted root keep the current wifi settings when the wifi fields are left blank on such a beacon. To provision without wifi, boot the beacon with ethernet attached.

The beacon package now depends on wpa_supplicant, dhcpcd, iw and wireless-regdb, and ships a `beacon-wifi` service (log in `/var/log/beacon-wifi`).

Existing beacon SD cards: self-update installs the dependencies and enables `beacon-wifi`. Older images may lack the Pi 5 wifi firmware (from `rpi-firmware`). If `/usr/lib/firmware/brcm/brcmfmac43455-sdio.raspberrypi,5-model-b.bin` is missing, install `rpi-firmware` or flash a new image. See CHANGELOG.md.

---

## What's new in 0.28.2

### mdma-bandcamp no longer crash-loops without cookies (#70)

Without a valid `/etc/mdma/bandcamp-cookies.json`, mdma-bandcamp used to exit and be restarted by runit every second (two log lines a second, constant NVMe writes). It now stays up "not configured" and reports "Bandcamp not configured: upload cookies via the web console". It picks up cookies after the console upload, which restarts the service.

### Correction to 0.28.1 (#69)

PCIe Gen1 only delayed the NVMe dropout on 2.4 GHz wifi (about 25 minutes instead of about 1 minute). With wifi limited to 5 GHz (`freq_list` in `wpa_supplicant.conf`), the unit has stayed stable for over an hour so far. The Gen1 setting stays; on affected hardware prefer 5 GHz or ethernet. Investigation continues in #69. See CHANGELOG.md.

---

## What's new in 0.28.1

### NVMe dropout root cause found (#69)

In an Argon NEO 5 M.2 case the NVMe dropped off PCIe within a minute of boot whenever the Pi's wifi radio was active (2.4 GHz interfering with the PCIe Gen2 link on the case's ribbon). Ruled out: power supply, kernel version (6.12.87 vs 6.12.95), /boot consistency, and the 0.28.0 APST/ASPM params. Provisioning now always writes `dtparam=pciex1_gen=1` into the boot partition's `config.txt` (PCIe Gen1, about 250 MB/s, ample for audio). It is section-aware (top level / `[all]`) and idempotent. The 0.28.0 cmdline params stay as a defensive measure.

**Correction:** 0.28.0 claimed APST/ASPM fixed the dropout. They did not; PCIe Gen1 does.

### Safer boot config writes

`cmdline.txt` and `config.txt` are now written atomically (temp file, fsync, rename); previously a power loss during the `cmdline.txt` rewrite could leave it empty and the node unbootable. A missing `config.txt` on the target is now a provisioning error instead of being silently created.

**Upgrade note:** already-provisioned nodes do not get the setting automatically. Append `dtparam=pciex1_gen=1` under an `[all]` section at the end of `/boot/config.txt` and reboot; verify with `cat /sys/bus/pci/devices/0001:01:00.0/current_link_speed` (expect `2.5 GT/s PCIe`). Or reprovision with "Keep existing system and data". `config.txt` is a conf_file, so package updates keep your edit and write new defaults to `config.txt.new-*`. Metal cases with the NVMe on a ribbon near the wifi antenna (e.g. Argon NEO 5) are susceptible; ethernet-only units were not affected.

**Known issues:** no DNS fallback in the beacon / startup clock-sync wait ineffective (#68); mdma-bandcamp crash-loops without a cookie file (#70); update page may hang at "Update started" (#67); wifi SSID is free text, so typos go unnoticed (#71). See CHANGELOG.md.

---

## What's new in 0.28.0

### NVMe dropout fixed (#69)

With kernel 6.12.95 the NVMe controller dropped off PCIe about 65 seconds after boot, hanging all disk I/O and SSH. Provisioning now writes `nvme_core.default_ps_max_latency_us=0 pcie_aspm=off pcie_port_pm=off` into `cmdline.txt`, idempotently (reprovisioning with Preserve does not duplicate; conflicting values are replaced).

### Exclusive existing-data choice

The beacon form's "Reinstall OS" and "Wipe drive" checkboxes are now one radio group (keep existing system and data / reinstall OS / wipe drive), posted as a single `existing_data` field. Previously wipe silently won when both were ticked. Wipe still needs the typed confirmation word and drive fingerprint.

**Upgrade note:** already-provisioned nodes do not get the NVMe parameters automatically. Append ` nvme_core.default_ps_max_latency_us=0 pcie_aspm=off pcie_port_pm=off` to the single line in `/boot/cmdline.txt` and reboot, or reprovision with "Keep existing system and data". Update the beacon via its UI to get the new form.

**Known issues:** no DNS fallback in the beacon when the router's resolver does not answer (#68); mdma-bandcamp crash-loops when no cookie file exists (#70); update page may hang at "Update started" (#67). See CHANGELOG.md.

---

## What's new in 0.27.1

### Beacon self-update fixed (#66)

Self-update never worked. The beacon package depended on `avahi-daemon` (a service name, not a Void package; the package is `avahi`), and SD images never configured the MDMA package repo. The package now depends on `avahi` and ships `/etc/xbps.d/10-mdma.conf`; the image build refuses to produce an image without it. The update check also no longer reports "Already at latest version" when it actually failed. Known issue: the update page may stay on "Update started" when there is nothing to update or the update fails (#67).

**Upgrade note:** beacons from an image built before 0.27.1 need the repo configured once, as root on the beacon:

```
echo 'repository=https://johlrogge.github.io/modular-digital-music-array/aarch64' > /etc/xbps.d/10-mdma.conf
xbps-install -S
xbps-install -u beacon
```

Or flash a fresh 0.27.1 image. See CHANGELOG.md.

---

## What's new in 0.27.0

### Optional wifi at provisioning (#41)

The beacon form takes an optional SSID, passphrase and (required) country code. The provisioned Pi prefers ethernet when plugged in and falls back to wifi, via dhcpcd route metrics. wpa_supplicant runs from the stock runit service on wlan0. The passphrase never reaches the Pi: the beacon derives the WPA PSK and writes only that, to a 0600 file. It is redacted from all logs and never echoed back into a page. The plan summary says whether existing wifi is kept or removed. If wifi fails, plug in ethernet and reprovision. See ADR-008.

New components: `private_file` (atomic credential-file writer, now used for the console cookie file) and `wifi_config` (validated types, PSK derivation, wpa_supplicant config).

**Limitations:** no WPA3-only (SAE) networks; one network only; changing wifi needs reprovisioning; `<host>.local` may resolve to two addresses while ethernet and wifi are both up.

**Upgrade note:** wifi is set only at provisioning time, so existing installs are unaffected. Blank wifi fields behave exactly like 0.26.1.

---

## What's new in 0.26.1

### Boot partition and safer provisioning

The NVMe boot partition is now mounted at `/boot` (#65), so kernel updates via xbps land where the firmware reads them. Before, a Pi could hang on its next reboot after `xbps-install -Su`. `mdma admin boot-health` checks the boot partition, and `mdma-admin` refuses to reboot into a kernel without matching modules (`--force` overrides).

Provisioning no longer destroys data by accident: an existing partition table is never rewritten, failed reads abort, and data partitions are formatted only when created in that run. Force-wipe is replaced by "Reinstall OS" (boot/root/var only) and a deliberate "Wipe drive" that requires typing WIPE. The beacon gains a "Boot order" page (#63) for SD-first rescue or NVMe-first normal boot.

### Fixes

- `/etc/mdma` is writable by the `mdma` group; the Bandcamp cookie file is 0600 (#38).
- SD image `config.txt` no longer accumulates duplicate initramfs lines (#64).
- New ADR-006 and ADR-007; ADR-002 amended.

**Upgrade note:** on units provisioned before 0.26.1, do not run `xbps-install -Su` until `migrate-bootfs` has run. Install the new `mdma-admin` first, then run `/usr/libexec/mdma/migrate-bootfs` (dry-run by default, `--apply` to change, resumable, never touches `/music`).

---

## What's new in 0.26.0

### Gapless playback

The next queued track is preloaded and the mixer switches to it sample-accurately when the current track ends (0-sample gap/overlap verified on hardware). MP3/AAC encoder delay and padding are trimmed so lossy joins are gapless too. If the sample rate changes between tracks (e.g. 44.1 to 48 kHz), the current track plays out fully and the output is reopened (~90 ms gap, no cut tail). Skip reuses the preloaded track (~10 ms to start).

### Playback engine

Pause/resume is immediate and resumes at the exact sample. Volume is applied at the output so changes are heard immediately (engine side only; CLI/playback control for volume and seek is not yet exposed, see #60). Now-playing and position follow what is actually heard (previously ~4 s ahead).

### Fixes and housekeeping

- PipeWire output data race between the RT callback and control thread fixed (#58); it could crash on skip/stop.
- Old-track audio no longer leaks into the next track on skip.
- `mdma-audio` and `mdma-playback` no longer exit on unknown or unparseable commands.
- Stream source protocol gains `Preload`/`ClearPreload` commands and a `preloaded` status field.
- Quieter logs: polls and per-track lifecycle at debug level.

**Upgrade note:** deploy `mdma-audio` before `mdma-playback` (the new `mdma-playback` sends `Preload`, which 0.25.x `mdma-audio` cannot parse). `xbps-install -Su` upgrades both together.

---

## What's new in 0.25.0

### Rekordbox round-trip data contract

`rekordbox_xml` now parses and writes `<TEMPO>` grid anchors and `<POSITION_MARK>` cues. A new `rekordbox_roundtrip` component maps these to typed MDMA facts (`BeatGrid`, `MemoryCue`). `mdma rekordbox import --enrich` reads a Rekordbox XML file and lands those facts in the library. `mdma rekordbox export` writes memory cues back. MDMA never writes `<TEMPO>` back — Rekordbox stays beat-grid master.

### New MusicValue fact variants

`music_facts::MusicValue` gains four new variants: `BeatGrid` (grid anchor from Rekordbox), `MemoryCue` (named cue point), `Role` (`TrackRole` enum — e.g. `banger`, `warmup`, `transition`), and `Energy` (integer 1–10).

### Role and energy are searchable and hand-taggable

```bash
mdma search --role banger --bpm "128+-3" --key "8A~"
mdma fact set sha256:<hash> role banger
mdma fact set sha256:<hash> energy 9
```

### dj-workspace — Bevy GUI app

New laptop-only Bevy/egui application (`dj-workspace`). Not packaged for the Pi. Connects to any MDMA node via the gateway:

```bash
dj-workspace --node mdma-909.local
```

Features a playlist browser and a candidate finder with BPM, Key, Role, and Energy filters and sortable results. Built on the new `bevy-app` base.

---

## What's new in 0.24.0

`mdma library track replace` is now a **hard replace**: the old track's facts are fully retracted and the old hash stops resolving entirely — it disappears from search and list results, not just hidden. The new track records a `Replaces` provenance link. Playlists referencing the old hash are rewritten immediately; any that still point at it self-heal on read by following the replacement chain (multi-step chains are followed automatically).

`mdma library track orphans` now lists blobs on disk with no live track reference: hard-replace leftovers appear with reason `no live facts`, alongside soft-deleted tracks (reason `deleted`). The command is read-only — a GC candidate list only.

`mdma library track delete` / `restore` are unchanged (soft, reversible).

---

## What's new in 0.23.0

Track lifecycle: `mdma library track replace` swaps in a better-quality version of a track and rewrites every playlist that references the old one in place. `mdma library track delete`/`restore` give you soft-delete with full recovery, and `mdma library track orphans` surfaces hidden tracks for future garbage collection. CDJ-safe exports: `mdma rekordbox export` now downsamples sources above 48 kHz via soxr so Pioneer CDJs never reject the output as illegal format.

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

- `mdma source <source> check-item <id>` and `mdma source <source> check-updates [--apply]` — on-demand stale-item detection via track-count comparison.
- `mdma source <source> resync <id>` — force-resync for manual override.

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
