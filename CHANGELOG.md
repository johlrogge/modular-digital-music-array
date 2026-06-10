# Changelog

All notable changes to MDMA are documented here.

---

## [0.22.0] — 2026-06-10

### Added

- **Service mode** — boot-to-beacon recovery path accessible from the web console (`/admin`) and CLI (`mdma admin service-mode status|enable|disable`). Flips EEPROM `BOOT_ORDER` to SD-first so the next reboot lands on the beacon SD card; beacon auto-reverts to NVMe-first after provisioning completes. Closes #37.
- **`mdma-admin` service** — new root-owned service that owns system-level operations (EEPROM writes, reboot). Exposed at `/run/mdma/admin.sock` (`0660 root:mdma`) so only the gateway can dispatch privileged operations.
- **`mdma admin reboot`** — graceful system reboot via CLI.
- **Web console `/admin` page** — status, enable, disable, and reboot buttons; destructive actions are confirm-gated.

### Changed

- **EEPROM helpers extracted to shared `rpi_eeprom` component** — consumed by both `mdma-beacon` stage5 and `mdma-admin`. No user-visible change.

### Security

- **Admin IPC socket restricted to `root:mdma` (`0660`)** — the `/run/mdma/admin.sock` socket is only accessible to `root` and the `mdma` group, preventing unprivileged processes from issuing EEPROM or reboot commands.

---

## [0.21.0] — 2026-06-10

### Added

- **"Force Wipe and Re-provision" button on the beacon main form** — always-visible destructive reprovision option with a hostname-typing confirmation gate. Previously the option only appeared in an error panel when a partition layout incompatibility was detected.
- **Nonfree repo support for Pi firmware tools** — Void Linux's nonfree repository is now added to both image build and provisioning so `rpi-eeprom` is available on the provisioned Pi.
- **Clock-sync wait before beacon HTTP serve** — the beacon waits for chrony to synchronise the system clock before accepting connections, avoiding TLS certificate verification failures on a fresh boot.

### Fixed

- **Pi 5 wouldn't boot from NVMe — GPT partition type GUID** — Pi 5 firmware refused to treat a "Linux filesystem" GPT partition type as an NVMe boot candidate even when its content was FAT32. The boot partition now receives the Microsoft Basic Data GUID.
- **Pi 5 wouldn't boot from NVMe — missing `PCIE_PROBE=1`** — without this flag written to EEPROM alongside `BOOT_ORDER=0xf164`, the bootloader does not probe PCIe at all, making the NVMe slot in `BOOT_ORDER` a no-op. Fresh Pis now get this written explicitly during provisioning.

### Changed

- **Beacon SD image size bumped to 2GiB** — required to fit the rpi-eeprom toolchain.

---

## [0.20.2] — 2026-05-30

### Fixed

- **`mdma source sync bandcamp` — duplicate re-downloads when Bandcamp lists multiple purchase entries for the same album** — if you bought an album more than once (e.g. vinyl and digital), MDMA would re-download it on every sync. A pre-download identity probe now checks for an exact normalized artist + album match among tracks already in the library; when a match is found the new item ID is backfilled so future syncs short-circuit via the cheaper ID path.
- **Bandcamp tracks with filenames containing adjacent dots silently dropped** — tracks whose title ends in a period (e.g. `Paramnésie I..flac`, where the period is part of the title before the extension) were rejected by an overly broad `..` substring guard. The check now rejects only genuine `..` path components. Dropped files now emit a warning instead of disappearing without a trace.

---

## [0.20.1] — 2026-05-12

### Fixed

- **`mdma rekordbox export` — missing playlist tracks and duplicate collection entries** — tracks would appear in `<COLLECTION>` but be absent from one or more `<PLAYLIST>` entries in the exported `rekordbox.xml`. On repeated export runs, duplicate `<COLLECTION>` entries for the same source track accumulated because the existing-XML matcher failed to recognise tracks it had already written. Root cause: `file://` URIs produced by two different code paths differed in case, and the URI comparison was case-sensitive. URIs are now normalised to lowercase at every lookup boundary before comparison.

---

## [0.20.0] — 2026-05-09

### CLI — Rekordbox XML export (`mdma rekordbox export`)

Exports the library (or a piped set of track IDs) as a `rekordbox.xml` file that Rekordbox can import directly. Tracks are transcoded to AIFF and laid out under an artist/album directory tree. Playlist structure is preserved. Subsequent export runs update the XML in-place — tracks already present in the file are not re-transcoded.

---

## [0.18.0] — 2026-05-04

### CLI — Manual fact editing (`mdma fact add` / `mdma fact retract`)

Users can now add or retract individual metadata facts on tracks directly from the CLI, without going through a re-import or rekordbox sync.

```
# Add a corrected BPM:
mdma fact add sha256:abc123 --field bpm --value 128.5

# Add a key in Camelot or traditional notation:
mdma fact add sha256:abc123 --field key --value 8B
mdma fact add sha256:abc123 --field key --value "C Major"

# Retract an incorrect genre tag:
mdma fact retract sha256:abc123 --field main-genre --value "Electronic"
```

Supported fields: `title`, `artist`, `album`, `album-artist`, `track-number`, `disc-number`, `year`, `bpm`, `key`, `main-genre`, `style-descriptor`, `full-genre`, `isrc`, `label`, `recording-year`, `recording-date` (YYYY-MM-DD), `comment`, `beatport-track-url`, `beatport-label-url`, `bandcamp-url`.

Facts are written with `FactOrigin::User` attribution, identical to bookmarks. Partial hashes are accepted (git-style prefix matching). Tab-completion on `--field` is supported via shell completion scripts.

**Note:** `mdma fact retract` writes a value-specific Retract record to ACID, but the library's in-memory materialization currently clears the scalar field regardless of the retracted value. If you retract a value that doesn't match the currently-displayed value, the field will briefly appear cleared in memory until the next library restart re-derives it from the fact stream. (Tracked as a follow-up.)

---

## [0.17.0] — 2026-05-01

### Breaking Changes

**`AcidEvent::FactsWritten` removed.** The single event variant is replaced by two distinct variants — `FactsAsserted` and `FactsRetracted` — so subscribers can react to assertions and retractions independently. The corresponding pub/sub topics split from a single topic to `acid/facts/asserted` and `acid/facts/retracted`. External consumers must subscribe to the `acid/` prefix and match both new variants.

### Fixed

- **ACID cursor drift after retractions** — the ACID cursor (and `line_count`) was only advanced on `WriteOk`, never on `RetractOk`, so cursors drifted after every retraction batch. The cursor now advances correctly on both `WriteOk` and `RetractOk`.

---

## [0.16.7] — 2026-05-01

### Console — M4A upload picker fix

The upload dialog's file input `accept=` list and help text now include `.m4a`. The 0.16.6 release added server-side M4A ingest support, but the HTML file picker was still restricted to `.flac/.mp3/.wav/.aif/.aiff/.zip`, which caused phones and most desktop browsers to hide M4A files in the chooser even though the server would have accepted them. The `.aif` and `.aiff` entries have also been removed from the picker — AIFF is export-only and listing it was misleading.

---

## [0.16.6] — 2026-05-01

### Library — M4A ingestion

M4A files (MPEG-4 audio container carrying AAC or ALAC) are now ingestible alongside FLAC, MP3, and WAV. The filename-derived metadata fallback introduced in 0.16.5 applies to M4A as well — when no embedded title or artist tags are present, both are derived from the filename stem using a `" - "` split. AIFF remains export-only. DRM-protected M4A files (Apple FairPlay) are not decoded; they surface as decode errors rather than silently producing empty tracks.

---

## [0.16.5] — 2026-05-01

### Library — WAV ingestion with filename-derived metadata fallback

WAV files are now ingestible. When no embedded title or artist tags are present, the library derives them from the filename stem using a `" - "` split (preserving titles that contain dashes). AIFF remains export-only. Because stainless-facts use replace semantics, a later manual edit naturally overrides any filename-derived fact.

---

## [0.16.4] — 2026-05-01

### Runit chown audit — all remaining services fixed (#85)

A systematic audit of all runit run scripts found the same root-owned-directory pattern that drove the 0.16.1–0.16.3 hotfixes in three more services; all three are fixed in one sweep.

- `mdma-acid`: `/metadata` and `/run/mdma` added to `chown mdma:mdma` — ACID writes `facts.jsonl` to `/metadata` but the directory was never chowned, so a fresh provision would fail on first write.
- `mdma-bandcamp`: `/run/mdma/sources` and `/var/lib/mdma` added to `chown mdma:mdma` — socket directory and cache location were `mkdir`'d but left owned by root.
- `mdma-library`: `/metadata` added to the existing `chown` line — the directory was created by `mkdir -p` but ownership was never set, leaving ACID-routed writes broken on a fresh install.

All three are latent bugs currently masked by directories being mode 0755 on existing provisioned systems.

---

## [0.16.3] — 2026-05-01

### Library

The 0.16.2 hotfix added `/music/cover-art` to the `chown mdma:mdma` line in the runit run script but omitted it from the preceding `mkdir -p`. The directory was never created, so the `chown` silently no-op'd and the library still could not write cover-art files in a root-owned `/music`. Fix: `/music/cover-art` added to the `mkdir -p` call so the directory is created before ownership is set.

---

## [0.16.2] — 2026-05-01

### Library

Cover-art directory creation failed with `Permission denied` after any `mdma-library` service restart — the same root-ownership issue that affected `/music/inbox` in 0.16.1. The runit run script created `/music/cover-art` before `chpst` dropped privileges, leaving the directory owned by `root:root` and unwritable for the `mdma` user. Fix: `/music/cover-art` added to the existing `chown mdma:mdma` line alongside `/music/inbox`, `/music/blobs`, and `/run/mdma`.

---

## [0.16.1] — 2026-05-01

### Library

Bandcamp track extraction failed with `Permission denied` after any `mdma-library` service restart. The runit run script created `/music/inbox` and `/music/blobs` during startup but left them owned by `root:root 755`. Because bandcamp runs as the `mdma` user, it could not write extracted tracks to those directories until the service happened to be started as the correct user. Fix: the run script now calls `chown mdma:mdma` on `/music/inbox`, `/music/blobs`, and `/run/mdma` immediately after the `mkdir` calls, before `chpst` drops privileges.

---

## [0.16.0] — 2026-04-27

### Library — ACID is now sole reader and writer for all facts (#71)

- All library fact reads and writes now go through ACID over IPC. The six direct `facts.jsonl` read sites and two direct write sites in `service.rs` are gone.
- New ACID protocol operations: `RetractFacts` request, `ReadEntity` request, `RetractOk` response, `EntityFacts` response.
- Library refactored at all call sites to use the new protocol operations — no more direct file access from the library service.
- `IndexedTrackInfo` gained an `item_id` field for round-trip entity identity.
- New `event_cursor` for incremental `TrackStarted`/`TrackStopped` reads — services can ask for facts since a known position without re-reading the full stream.
- ADR-001 (`docs/adr/001-acid-sole-writer-of-facts-jsonl.md`) promoted to **Accepted**. The north star is locked.

### Packaging

- INSTALL scripts for all 8 services now create `/var/log/<svc>` at install time. Previously `mdma-audio` and `mdma-acid` were missing this step, causing log directory errors on first install.

### Polylith

- `dirs` and `socket2` added to `Polylith.toml`. Profile drift fix: `cargo polylith change-profile` no longer drops these dependencies.

---

## [0.15.5] — 2026-04-27

### Library

- Bootstrap reads every fact from ACID on startup — no file fallback. If ACID is unreachable or returns nothing, the service fails loudly instead of silently falling back to `facts.jsonl`.
- Removed the cursor-on-disk dance: the library no longer persists a restart cursor to disk and no longer asks ACID for facts after a stale cursor.
- ACID fails loudly on errors — no silent partial state on startup.

### ACID

- Logs its backend (`fact_store_memory` or `fact_store_file`) at startup so the active profile is visible in the service log.

---

## [0.15.4] — 2026-04-27

### ACID

- `replay_from_file` added to the in-memory ACID backend — enables seeding the in-memory store from an existing `facts.jsonl` for development and testing.

### CI

- Production profile wired into the CI build pipeline.

---

## [0.15.3] — 2026-04-27

### Beacon / Provisioning

- Stage 3 false-skip after `wipefs` fixed — partition detection now uses `blkid` instead of `lsblk` (#53).
- SD and NVMe `cmdline.txt` both now reference the NVMe partition by PARTUUID (#54, #55).
- `/music` subdirectories created and chowned during provisioning (#60).
- `bandcamp.conf` seeded from `.example` on first boot (#61).
- `authorized_keys` written with 0600 permissions — was silently rejected by OpenSSH at 0664 (#62).

### Logging

- Services write logs to disk via `svlogd`. Beacon UI tails the on-disk log rather than the process stream. New `/logs` page lists rotated log files.

---

## [0.14.0] — 2026-04-21

### Console

- Queue playlists directly from the web console.
- Reorder queue entries with ↑/↓ buttons.

### CLI

- `--not` with stdin excludes: pipe track IDs to exclude from search results.
- `--played never` filter: list tracks that have never been played.
- Sort by `start` and `stop` (last started / last stopped timestamps).

### Library

- `TrackInfo` now exposes `last_started` and `last_stopped` fields.

---

## [0.13.0] — 2026-04-19

### TUI — Search and filters

- Field-aware search grammar in SearchPane: `:artist 'bonobo'`, `:bpm 120`, `:title 'foo bar'`, `:added -7`, etc. Bare words search any_text (title/artist/album/label/genre).
- Live search — results update on every keystroke.
- `s` and `/` filter the current list live (pane display-string match).
- `,` clears selection.
- Genre counts show real track numbers.
- `:history [days]` opens a SearchPane preset with `:started '-N..~'`. Default 7 days.
- CLI: `mdma search --added -7` now accepts hyphen-prefixed date expressions.

### TUI — Multi-pane tabs (nnn-style)

- 5 tab slots per side. Keys `1`–`5` = left side; `6`–`9`, `0` = right side.
- Pressing a key for an empty slot clones the currently-focused pane into it.
- No explicit close — tabs overwrite by navigation or by being cloned into.
- Tab bar with LRU-priority shrinking of inactive titles.

### TUI — DJ-workflow shortcuts

- `A` adds currently-playing track to focused playlist or queue.
- `P` plays selected track(s) immediately (queue_next + skip, queue preserved).
- Playlist cut/paste: `x`/`X` to select; `d` cuts to clipboard; navigate; `p` pastes after cursor.
- `Shift+J`/`Shift+K` moves the entire selected block up/down (Kakoune/Helix semantics; non-contiguous gracefully declined).
- `u` undoes the last mutation on the active pane (playlist-only in v1).

### TUI — UX fixes

- Panes with text input capture all keys until `Esc` — no more `a`/`s`/`P` hijacking during search typing.
- Palette no longer eats `h`/`l` in `:help` etc.
- Album drill-down default sort: disc asc, track asc.
- Inbox scanner ignores macOS AppleDouble sidecar files (`._*`).

### Playback engine

- MP3 decoder no longer strips zero-padding from decoded segments — fixes scratchy/roboty MP3 playback.
- Decoder moved from tokio task to `std::thread` to avoid IPC contention.
- Mixer no longer busy-spins on a full output ring.
- Mixer no longer silence-pads when the track buffer briefly underruns.
- Output uses the source's native sample rate — 44.1 kHz MP3 no longer resampled to 192 kHz unnecessarily.
- Flush on track change — skip latency ~50 ms instead of seconds.
- `allowed-rates` PipeWire config added so the graph can switch rate natively.

### Library

- `mdma-library` creates `playlists/` subdirectory on startup and before each write — fresh installs can reorder playlists on first try.
- Retract-aware fact fold.

### Bandcamp

- `mdma source check-item <source> <id>` — on-demand stale check via track-count comparison.
- `mdma source check-updates <source> [--apply]` — batch stale detection across the collection.
- `mdma source resync <source> <id>` — force-resync for manual override.

---

## [0.11.0]

- **Polylith workspace migration** — root `Cargo.toml` is now a stub. All builds go through `cargo polylith cargo --profile <profile> <cmd>`.
- **Bookmarks** — `mdma bookmark [<hash>] [--scope <set-name>]` and `mdma bookmarks`. Stored as `Bookmarked` facts with `FactOrigin::User`.
- **TUI bookmark keybinding** — `b` in Playback mode bookmarks the currently playing track.
- **`FactOrigin::User`** — new variant for user-initiated facts.

## [0.9.0]

- **mdma-tui** — intelligent column compression in track lists. BPM and key columns dropped first when space is tight, then duration, then artist.

## [0.8.1]

- **playback_engine** — removed multi-deck abstraction; single `Option<Track>` model. `SeqCst` atomics replaced with `Acquire`/`Release` pairs for aarch64 correctness.
- **mdma-audio** — updated to match simplified `PlaybackEngine` API.

## [0.8.0]

- **mdma-tui** — dual-pane terminal UI (browser/queue). Modal keybindings: Normal and Playback modes. Command palette (`:`) for play/pause/stop/next/clear/shuffle/quit. `q`/`Q` queue append/next. `?` help overlay. Live queue sync via event bus.
- **mdma-playback** — `Play`/`PlayQueue` resumes a paused track instead of popping the queue.
- **nng-transport** — 5-second send/receive timeouts on all client sockets.

## [0.7.0]

- **mdma-audio** — new service wrapping PlaybackEngine, speaks `stream_source_protocol` over NNG.
- **mdma-playback** — pure queue manager; drives sources via `StreamClient`. Queue entries carry `source: String`.
- **stream_source_protocol** — new component: `StreamCommand`/`StreamResponse`/`StreamTrackInfo`/`StreamPlaybackState`.

## [0.6.2]

- **Date expression fix** — single-component expressions (e.g. `15`) interpreted as day, not year.
- **CLI `--added` help text** — corrected to describe positional component order.

## [0.6.1]

- **Package build fix** — `version.workspace = true` handled correctly in package scripts.

## [0.6.0]

- **Album Order** — CLI and console album views sort by DiscNumber then TrackNumber.
- **AddedAt Tracking** — every track records when it was added; queryable via `--added`.
- **Album Art Cache** — album-level cover art fallback.
- **Date Expressions** — new `date_expression` crate with relative date syntax (`~`, `^`, `$`, `+/-N`, `/`-separated).
- **CI** — git-hooks input added to devenv.yaml.
