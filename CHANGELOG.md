# Changelog

All notable changes to MDMA are documented here.

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
