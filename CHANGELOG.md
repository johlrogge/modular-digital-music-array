# Changelog

All notable changes to MDMA are documented here.

---

## [Unreleased]

### Added

- **Marks (ADR-016 phases 0-1).** A bookmark is now the default mark, "liked". Named marks such as "banger" or "opener" sit beside it.
  - `mdma mark [NAME]` and `mdma unmark [NAME]` act on the hashes piped on stdin. Run from a terminal without a pipe, they act on the playing track. A pipe with no hashes is an error ("No hashes provided on stdin.").
  - `mdma marked [NAME]` lists tracks with that mark, or with any mark when NAME is left out. `mdma marked liked` lists only the default mark.
  - Mark names are lowercase, 1-64 characters, with no control or invisible characters. "liked" is reserved for the default mark.
  - Marks written before this release, with scopes that are not valid names, are still read, and `marked` lists them. They can't yet be removed from the CLI; removal is a planned follow-up.
  - New library requests: `ListMarked { filter: Any | Only(mark) }`, `WriteMark` and `UnmarkTrack`. `UnmarkTrack` retracts every live entry of the mark, each with its own source.
- **TUI:** `b` marks the playing track "liked". It reports "Marked liked" or "Already marked liked".

### Changed

- **BREAKING: `mdma bookmark` and `mdma bookmarks` are now aliases of `mark` and `marked`.** The `--scope` option and the HASH argument are gone. Pipe hashes on stdin instead, and pass the mark name as NAME.
- **BREAKING: `ListBookmarked` is removed** (it was never released). Use `ListMarked`. `WriteBookmark` stays as a deprecated alias of `WriteMark`.
  - Upgrade the node first: gateway and library (and console) together, then the CLI and TUI. A new gateway in front of an old library would time out, which is why those go together.
  - Older CLIs and TUIs keep working against the new node through `WriteBookmark`: their scopes are lowercased, a repeat writes nothing, and an invalid scope gets an error.
- **Old nodes.** A node that does not know a request now gives one error: "The node did not understand this request; it may be older than this CLI — upgrade the node." There is no fallback. The TUI shows the same text with "TUI" in place of "CLI". A new CLI against an old node shows this message.
- **Bookmark behaviour:**
  - `WriteBookmark` scopes are lowercased.
  - An empty, too-long or control/invisible scope is rejected with Internal.
  - Repeating a bookmark or mark writes nothing, and the original time is kept.
  - Retracted bookmarks no longer appear in listings.
  - Merge dedup compares bookmarks by mark, so "Banger" equals "banger".
- The now-playing error text is "Nothing playing".

- **BREAKING: mdma-cli now parses with hicli 0.2; clap is removed (ADR-015).**
  - Bad argument values are rejected while parsing, with exit code 2, before anything connects. Runtime failures (node unreachable, service error) exit 1.
  - The parser's error text replaces the old custom messages.
  - A bad `MDMA_NODE` is reported only when a node is needed. It exits 1 and names the variable.
  - `--node` (and `MDMA_NODE`) is `host[:port]`, port 1-65535; anything else is a usage error.
  - Track hashes on argv follow the stdin rule: at least 8 lowercase hex characters, optional `sha256:` prefix.
  - `search`: `--bpm` and `--year` reject bad values, `--key` and `--duration` now work, `--energy` must be 1-10.
  - Date options (`--added`, `--started`, `--stopped`) are date expressions. A value starting with a dash must be glued: `--added=-7`. `--added -7` as two words is rejected.
  - `mdma help <cmd>` is gone; use `mdma <cmd> --help`. `source NAME --help` shows the source group's page.
  - `--version` prints `Version: x.y.z`.
  - Completions come from the hidden `mdma generate-completions <bash|zsh|fish>`. elvish and PowerShell completions are dropped. Regenerate any installed completion scripts.

### Fixed

- **The library replies `InvalidRequest` to requests it cannot parse** (#97). Before, it sent no reply, and the stub's server died.
- `queue edit` and `playlist edit` use a unique temp file.

---

## [0.36.1] — 2026-11-04

### Fixed

- **The library no longer skips early facts at startup.** About 370 lines written by the first releases are now read.
  - `HasAlbumArt` stored as `true`/`false` is read as present/absent.
  - The early play events `Played` and `Skipped` are read as `TrackStopped` (completed or skipped).
  - This restores album-art flags and the earliest play history on existing libraries. The facts log is not rewritten, and new facts use the current format.

---

## [0.36.0] — 2026-11-04

### Added

- **Identity backfill.** `mdma library identity backfill [--pcm] [--no-wait]`, `status` and `cancel` compute AudioHash, and PcmHash on request, for existing tracks. It runs at low priority. AudioHash is now also computed at ingest.
- **PcmHash v1 (ADR-012).** A container-independent identity hash over decoded lossless samples (FLAC, WAV, AIFF). Compute it with `mdma library identity backfill --pcm`.
- **System logging.** Provisioned nodes now run socklog (`socklog-unix`) as the syslog daemon on `/dev/log`. Logs go under `/var/log/socklog/` (#89).
- **avahi-daemon is enabled on upgrade** (mdma-admin INSTALL), which fixes mDNS discovery on nodes where it was missing (#80).
- **Duplicate scan and merge (ADR-013).**
  - `mdma library dedup` lists duplicate groups: automatic, or needs confirmation with reasons.
  - `mdma library dedup --apply` merges the automatic groups.
  - `mdma library dedup merge <winner> <loser>... [-y]` merges a confirmed group.
  - Rules: latest fact wins per attribute, the newest copy is kept, ItemIds are unioned, and play history follows `Replaces`.
- **Self-healing at ingest.** A re-downloaded copy of the same purchase with the same audio merges automatically.
- **Archive.** Merged-away files go to `/music/archive/` and are never deleted directly.
  - `mdma library archive` lists them.
  - `mdma library archive empty [--yes]` is the only way to delete them permanently. It deletes only what was listed.
- **Deploy recipes.** `just deploy-<svc> [host]` takes an optional host; it defaults to prod (`PI_HOST`). Recipes use the project ssh config with BatchMode, so they never hang on a host-key prompt.

### Changed

- **Bandcamp hardening.** Sync defers, and resync and check refuse, when the library is unreachable, instead of re-downloading everything.
- **BREAKING: library IPC protocol.** Dedup and archive messages were added, and `blobs_removed` was renamed `blobs_archived`.
- **`mdma source` now takes the source name first: `mdma source <source-name> <command>`.** This applies to every per-source command: `sync`, `status`, `downloads`, `cancel`, `pause`, `resume`, `resync`, `check-item`, `check-updates`. For example, `mdma source sync bandcamp` is now `mdma source bandcamp sync`. `mdma source list` is unchanged, and `list` can't be used as a source name.
- **The `mdma bandcamp` command group moved under `mdma source`, with no deprecated alias.**
  - `mdma bandcamp playlists` is now `mdma source <name> playlists`.
  - `mdma bandcamp import-playlist <slug-or-url> [--name N] [--dry-run] [--no-wait]` is now `mdma source <name> import-playlist <slug-or-url> [--name N] [--dry-run] [--no-wait]`.
  - `mdma bandcamp import-status` is now `mdma source <name> import-status`.
  - A source without playlist support answers "source '<name>' does not support playlists" (or "... playlist import") and the command exits 1.

### Docs

- ADR-012 (PcmHash identity tier) is accepted. ADR-013 (duplicate merge rules) is added. ADR-014 (merge by fold) is proposed.

### Upgrade notes

The library IPC protocol changed. Library, gateway, console and CLI must be upgraded together: upgrade all mdma-* packages together, by name.

---

## [0.35.0] — 2026-11-03

### Added

- **`audio_hash` component (ADR-010, #55).** Computes an AudioHash: a tag-independent identity hash over container-defined byte ranges.
  - FLAC: audio frames.
  - MP3: frames, including Xing/LAME.
  - WAV: `fmt ` plus `data`.
  - MP4/M4A: `mdat`.
  - Trailing APEv2, Lyrics3v2, ID3v2-footer and ID3v1 tags are stripped, strongest signature first.
  - It never decodes and never panics on malformed input. Hashes are versioned as `audio-sha256-v1:<hex>`.
- **`AudioHash` newtype and fact.** A validated `AudioHash` newtype and a `MusicValue::AudioHash` fact variant.

### Not yet active

- Ingest does not compute or write AudioHash yet, and there is no backfill. Both come in a later release. No user-visible behaviour change.

### Docs

- ADR-010 is accepted, with the exact frozen v1 definition. It shipped in 0.34.0's tree; the code ships here.

### Known issues

- `mdma source sync` still times out client-side; the sync continues on the node (#81).
- The gateway serialises requests (#84).
- rekordbox import treats artist+title matches with different durations as matched (#85).
- Playback writes play events under short hashes (#86).
- Library projection follow-ups: retract scan cost, cover cache not refreshed while running (#87).

### Upgrade notes

`MusicValue` gained a variant. Per ADR-004, redeploy all services that embed the protocol together (gateway, library, console): upgrade all mdma-* packages together, by name.

---

## [0.34.0] — 2026-11-03

### Changed

- **Read-your-writes projection (ADR-011).** Every library write now reaches the in-memory index through one `catch_up` path: read stream, apply, advance cursor, under a lock.
  - The fact subscriber shares this path.
  - `WriteFact`, `RetractFact` and bookmarks now show up immediately.

### Fixed

- Facts applied twice after ingest. Duplicate memory cues and styles stayed in memory until restart and could leak into a rekordbox export taken in that window. Counter drift is fixed too.
- Back-to-back track replaces no longer lose `Replaces` ancestry (#44).
- **Ghost entries.** Replaced or retracted hashes no longer reappear as empty entries after replay. A track exists only while it has live facts.
- **Phantom rows.** Play-event-only entries written under short or empty hashes by older playback (#86) are no longer listed as tracks. On prod data the track count goes from 983 to 870.
  - Removing the empty-key phantom also makes playlist lazy repair work again. Before, every playlist line counted as live.
- **Deadlock.** Fixed a lock-order inversion in the ingest dedup path.
- **Multi-source facts.** Retracting one source's facts now keeps values that another source still asserts, instead of clearing them.

### Docs

- ADR-011 is new. ADR-004 is amended.

### Known issues

- `mdma source sync` still times out client-side; the sync continues on the node (#81).
- The gateway serialises requests (#84).
- rekordbox import treats artist+title matches with different durations as matched (#85).
- Playback writes play events under short hashes (#86).
- Library projection follow-ups: retract scan cost, cover cache not refreshed while running (#87).

### Upgrade notes

No protocol change. Only mdma-library behaviour changes, but upgrade all mdma-* packages together, by name.

Expect `tracks_indexed` / the library track count to drop by the number of phantom entries.

---

## [0.33.0] — 2026-11-03

### Added

- **Bandcamp playlist import runs on the node.** `mdma bandcamp import-playlist <slug-or-url>` now starts a background job in mdma-bandcamp.
  - It fetches the playlist and matches it against the library.
  - If owned tracks are missing, it runs a full collection sync and waits for the downloads.
  - It re-matches, then writes the playlist once. It never writes an empty playlist, and it replaces an existing one.
  - The CLI polls for progress.
  - New flag `--no-wait`, and new command `mdma bandcamp import-status`.
  - `--dry-run` reports matches and writes nothing.
  - Paused or timed-out downloads are reported in the result.
  - If an import is already running, a new one is refused ("Import NOT started", exit 1).

### Changed

- **Shared playlist import.** Playlist matching and writing moved into a shared `playlist_import` component. The rekordbox import uses it too.
- **rekordbox export path length.** New export paths are capped at 250 bytes of the encoded rekordbox Location, measured in NFD the way macOS stores names. rekordbox on macOS rejects paths of about 256 bytes and halts the whole import.
  - Long names are shortened title first, then album, then artist. Each gets a stable `~hash` suffix.
  - Paths already in the existing `rekordbox.xml` are kept, so rekordbox keeps their cues. `--replace` skips this grandfathering.
- **Source protocol.** `SourceRequest` and `SourceResponse` gained import variants.

### Fixed

- **Bandcamp sync.** Dedup no longer holds the download queue lock during library calls, so status polls don't stall.
- Items already queued or downloading are not queued twice, by sync, import or resync.
- Cancelling a queued item ends any wait on it.

### Internal

- CI frees runner disk space before building (#83).

### Known issues

- `mdma source sync` still times out client-side; the sync continues on the node (#81).
- The gateway serialises requests (#84).
- rekordbox import treats artist+title matches with different durations as matched (#85).
- Playback writes play events under short hashes (#86).

### Upgrade notes

mdma-gateway, mdma-bandcamp and the CLI must all be 0.33.0. The source protocol gained import variants.

Upgrade the mdma-* packages by name, never a bare `xbps-install -Su`, which would pull in the rpi-kernel on units that haven't run migrate-bootfs.

---

## [0.32.0] — 2026-11-02

### Added

- **Bandcamp playlist import.**
  - `mdma bandcamp playlists` lists your Bandcamp fan playlists, private ones included.
  - `mdma bandcamp import-playlist <slug-or-url> [--name N] [--dry-run]` imports a playlist into an MDMA playlist, in the same order.
  - Tracks are matched against the library by artist, title and duration.
  - A per-track report is printed: `✓` matched, `?` ambiguous or duration differs (not written), `✗` not in library.
  - It warns if fewer tracks were fetched than the playlist has.
  - It aborts without writing if a library lookup fails.
  - Re-running replaces the playlist, including manual edits.
  - The Bandcamp service fetches through the Pi's stored cookies. Only https bandcamp.com URLs are accepted.

### Changed

- **Source protocol.** New `ListPlaylists` and `GetPlaylist` requests and an `Unsupported` error.
- **mdma-bandcamp** answers unparseable requests with an error instead of stalling the gateway.
- **Shared playlist writing** for the rekordbox and Bandcamp imports. It only replaces on "already exists", and a lookup failure no longer silently drops tracks.

### Internal

- Fixed Rust 1.99 clippy lints across the workspace (#62). The workspace is clippy-clean with all targets.

### Known issues

- Long source operations block the gateway (#81).
- Avahi renames the node to `-2` when ethernet and wifi share a subnet (#80).
- Wifi association is delayed after boot (#79).
- Argon NEO 5 with wifi shows NVMe instability (#74, #69; see the research in #69).

### Upgrade notes

mdma-gateway, mdma-bandcamp and the CLI must all be 0.32.0. The gateway parses source messages, so older gateways reject the new requests.

The `mdma bandcamp` commands use a 30 s CLI timeout, but the gateway caps source requests at 10 s, and the gateway is blocked while a request runs. Don't run imports or syncs during playback (#81).

Units provisioned before 0.26.1 that haven't run migrate-bootfs: upgrade by naming the mdma-* packages explicitly, never a bare `xbps-install -Su`.

---

## [0.31.0] — 2026-11-02

### Added

- **Clean power-button shutdown (#77).** A short press on the Pi 5 power button now shuts the node down cleanly.
  - mdma-admin depends on acpid, and Void's stock handler runs `shutdown -P now`.
  - runit stops the services in order and a wtmp shutdown record is written.
  - There is no filesystem recovery on the next boot.
  - Verified on stage hardware. A long press is still a hard power cut by the PMIC; avoid it.
- **Persistent kernel log (#77).** The new `mdma-klog` service writes the kernel log to `/var/log/mdma-klog`, with svlogd rotation capped at about 20 MB.
  - The ring buffer is replayed once per boot, so the previous boot's kernel messages survive a reboot or power cut.
  - Caveat: after a hard cut, the last seconds before it may be missing, and an NVMe dropout cannot write its own errors to the NVMe.
- **Coverage.** New installs enable both during provisioning; existing nodes get them on the mdma-admin upgrade. The beacon SD also gets acpid.
- **Disabling.** To disable either service permanently: `touch /etc/sv/<svc>/down`. Upgrades re-link a removed `/var/service` link.

### Known issues

- Beacon self-update leaves the package half-configured (`uu`); run `xbps-reconfigure beacon` once afterwards (#76).
- mdma-playback logs a bare "Error:" after boot (#78).
- Prod halted after its ethernet was unplugged. Evidence points to a held power button (a hard PMIC cut), not software; the new mdma-klog will settle any recurrence (#74).
- The TUI and dj-workspace still use only `MDMA_NODE`.
- The beacon has no DNS fallback when the router's resolver does not answer, and the startup clock-sync wait is ineffective (#68).
- The update page may stay on "Update started" (#67).
- The wifi SSID is free text, so typos go unnoticed (#71).

---

## [0.30.0] — 2026-11-02

### Added

- **The CLI finds nodes on its own (#73).** `MDMA_NODE` is no longer required.
  - Nodes announce `_mdma._tcp` over mDNS. The gateway answers a new Identify request.
  - The CLI resolves its target in this order:
    1. `--node`, then `MDMA_NODE`
    2. local IPC on the Pi
    3. the remembered node, verified by Identify
    4. discovery, or an interactive pick
  - mDNS results are only candidates. Every address is confirmed by Identify before it is used or remembered, so a swapped Pi on the same IP is never silently targeted.
  - New commands: `mdma node list`, `select`, `forget` and `show`.
  - The remembered node is cached at `~/.config/mdma/node.json` on Linux, and under Application Support on macOS.
  - First use on macOS shows a Local Network permission prompt.

### Fixed

- **Packages are now assembled from `void-packages/srcpkgs` (#75).**
  - The 0.29.0 beacon package was missing the `beacon-wifi` service and its dependencies, so the wifi fallback from #72 could not start. 0.30.0 ships them.
  - The gateway now depends on avahi and ships a REMOVE script that withdraws its mDNS announcement on uninstall.
  - CI verifies every built package against its declaration.

### Known issues

- Prod halted after its ethernet was unplugged; under investigation (#74).
- The TUI and dj-workspace still use only `MDMA_NODE`.
- The beacon has no DNS fallback when the router's resolver does not answer, and the startup clock-sync wait is ineffective (#68).
- The update page may stay on "Update started" (#67).
- The wifi SSID is free text, so typos go unnoticed (#71).

### Upgrade notes

Beacon SD cards on 0.29.0 should self-update to get `beacon-wifi`.

Older gateways (0.29.0 and earlier) answer Identify as "unsupported". The CLI then treats them as unverifiable and won't remember them through discovery. Use `--node` until they are updated.

---

## [0.29.0] — 2026-11-02

### Added

- **The beacon reuses the installed system's wifi when there is no ethernet (#72).** Service mode, recovery and reinstall now work on wifi-only units without a cable.
  - It mounts the root partition read-only, never touches `/music`, and never touches the NVMe at all when ethernet is linked.
  - It copies the wifi config verbatim into tmpfs, so a 5 GHz `freq_list` pin survives. Credentials never land on the SD card.
  - The UI shows the network source and warns about provisioning over wifi (#69).

### Changed

- **Reinstall, Wipe all, and Preserve with a reformatted root keep the current wifi settings** when the wifi fields are left blank on a beacon running on the installed system's wifi. To provision without wifi, boot the beacon with ethernet attached.
- **The beacon package now depends on wpa_supplicant, dhcpcd, iw and wireless-regdb**, and ships a `beacon-wifi` service (log in `/var/log/beacon-wifi`).

### Known issues

- The beacon has no DNS fallback when the router's resolver does not answer, and the startup clock-sync wait is ineffective (#68).
- The update page may stay on "Update started" (#67).
- The wifi SSID is free text, so typos go unnoticed (#71).

### Upgrade notes

Existing beacon SD cards: self-update installs the new dependencies and enables `beacon-wifi`.

Older images may lack the Pi 5 wifi firmware, which comes from `rpi-firmware`. If `/usr/lib/firmware/brcm/brcmfmac43455-sdio.raspberrypi,5-model-b.bin` is missing, install `rpi-firmware`, or flash a new image. New images include it.

---

## [0.28.2] — 2026-11-02

### Fixed

- **mdma-bandcamp no longer crash-loops when `/etc/mdma/bandcamp-cookies.json` is missing or invalid (#70).** Previously it exited and runit restarted it every second, producing two log lines a second and constant NVMe writes on any node without a Bandcamp login. It now stays up in a "not configured" state and reports "Bandcamp not configured: upload cookies via the web console". It picks up cookies after the console upload, which restarts the service. The stale `mdma-909.local` hint is gone from the error.

### Correction

- **0.28.1 presented PCIe Gen1 as the fix for the NVMe dropout (#69). Further testing shows it only delayed it.** On a Pi 5 in an Argon NEO 5 M.2 case with 2.4 GHz wifi active, Gen1 (`dtparam=pciex1_gen=1`) postponed the dropout from about 1 minute to about 25 minutes. With the wifi limited to 5 GHz (`freq_list` in `wpa_supplicant.conf`), the unit has stayed stable for over an hour so far. The Gen1 setting stays. On affected hardware, prefer 5 GHz wifi or ethernet. Investigation continues in #69.

### Known issues

- The beacon has no DNS fallback when the router's resolver does not answer, and the startup clock-sync wait is ineffective (#68).
- The update page may stay on "Update started" (#67).
- The wifi SSID is free text, so typos go unnoticed (#71).

---

## [0.28.1] — 2026-11-01

### Fixed

- **NVMe dropout on Pi 5 with wifi active (#69): root cause found.** In an Argon NEO 5 M.2 case the NVMe dropped off PCIe within a minute of boot whenever the Pi's wifi radio was active (2.4 GHz transmissions interfering with the PCIe Gen2 link on the case's ribbon). Ruled out: power supply, kernel version (6.12.87 vs 6.12.95), `/boot` consistency, and the NVMe APST/ASPM kernel params added in 0.28.0. Provisioning now always writes `dtparam=pciex1_gen=1` into the NVMe boot partition's `config.txt` (PCIe Gen1, about 250 MB/s, ample for audio). The write is section-aware (applies at top level / `[all]`) and idempotent. The 0.28.0 cmdline params stay as a defensive measure.
- **`cmdline.txt` and `config.txt` are now written atomically** (temp file + fsync + rename). Previously `cmdline.txt` was rewritten in place, and a power loss could leave it empty (unbootable).
- **A missing `config.txt` on the target boot partition is now a provisioning error** instead of being silently created.

### Correction

- 0.28.0 said the NVMe dropout was fixed by disabling APST/ASPM. That was incomplete: those params did not prevent it; PCIe Gen1 does.

### Known issues

- The beacon has no DNS fallback when the router's resolver does not answer, and the startup clock-sync wait is ineffective (#68).
- mdma-bandcamp crash-loops when no cookie file exists (#70).
- The update page may stay on "Update started" (#67).
- The wifi SSID is free text, so typos go unnoticed (#71).

### Upgrade notes

Already-provisioned nodes do not get the setting automatically. To apply it by hand, append `dtparam=pciex1_gen=1` under an `[all]` section at the end of `/boot/config.txt` and reboot. Verify with `cat /sys/bus/pci/devices/0001:01:00.0/current_link_speed`, which should print `2.5 GT/s PCIe`. Alternatively, reprovision with "Keep existing system and data".

`config.txt` is an rpi-firmware conf_file: package updates keep your edit and write new defaults to `config.txt.new-*`.

Hardware note: metal cases with the NVMe on a ribbon near the wifi antenna (e.g. Argon NEO 5) are susceptible. Ethernet-only units were not affected.

---

## [0.28.0] — 2026-10-29

### Fixed

- **NVMe dropout on Pi 5 (#69).** With kernel 6.12.95 the NVMe controller dropped off PCIe about 65 seconds after boot (`controller is down; will reset: CSTS=0xffffffff`), hanging all disk I/O and SSH. Power was ruled out. Provisioning now writes `nvme_core.default_ps_max_latency_us=0 pcie_aspm=off pcie_port_pm=off` into the boot partition's `cmdline.txt`. The write is idempotent: reprovisioning with Preserve does not duplicate the parameters, and conflicting values are replaced.

### Changed

- **Beacon provisioning form: one exclusive choice for existing data.** "Reinstall OS" and "Wipe drive" were independent checkboxes, and wipe silently won when both were ticked. They are now a single radio group (keep existing system and data / reinstall OS / wipe drive), posted as one `existing_data` field. Wipe still requires the typed confirmation word and the drive fingerprint. A page reload never restores a hidden wipe selection.

### Known issues

- The beacon has no DNS fallback when the router's resolver does not answer (#68).
- mdma-bandcamp crash-loops when no cookie file exists (#70).
- The update page may stay on "Update started" (#67).

### Upgrade notes

Already-provisioned nodes do not get the NVMe parameters automatically, because only provisioning writes `cmdline.txt`. To apply them by hand, append ` nvme_core.default_ps_max_latency_us=0 pcie_aspm=off pcie_port_pm=off` to the single line in `/boot/cmdline.txt` and reboot. Alternatively, reprovision with "Keep existing system and data".

Update the beacon through its UI ("Update") to get the new form and the NVMe fix.

---

## [0.27.1] — 2026-10-26

### Fixed

- **Beacon self-update never worked: wrong dependency.** The beacon package depended on `avahi-daemon`, which is not a Void package. The package is `avahi`; `avahi-daemon` is only its service name. Every beacon update since 0.22.1 failed with `MISSING: avahi-daemon>=0`.
- **Beacon self-update could not find the MDMA package repo (#66).** SD images never configured the MDMA repository, so `xbps-install -u beacon` reported "Package 'beacon' not found". The beacon package now ships `/etc/xbps.d/10-mdma.conf` as a conf_file. The image build now refuses to produce an image without it, including when a stale cached platformfs is present.
- **The update check reported "Already at latest version" when it had actually failed.** Failures are now reported with xbps's message. When the beacon is already up to date, the install step is skipped.

### Correction

- The 0.22.1 entry "beacon depends on `avahi-daemon` — was `avahi`" was wrong; `avahi` was the correct package name. The old line is left as it is.

### Known issues

- The update page may stay on "Update started" when there is nothing to update, or when the update fails (#67).

### Upgrade notes

Beacons from an image built before 0.27.1 have no MDMA repo configured, so they cannot self-update to 0.27.1 until you do this once, as root on the beacon (`ssh root@welcome-to-mdma.local`, password `voidlinux`):

```
echo 'repository=https://johlrogge.github.io/modular-digital-music-array/aarch64' > /etc/xbps.d/10-mdma.conf
xbps-install -S
xbps-install -u beacon
```

After that, the update button in the beacon UI works.

No `avahi-daemon` workaround is needed when upgrading straight to 0.27.1, because xbps checks the new package's dependencies, which are correct. Alternatively, flash a fresh 0.27.1 beacon image.

---

## [0.27.0] — 2026-10-26

### Added

- **Optional wifi in the beacon provisioning form** (#41) — three fields: SSID, passphrase, and a required country code.
  - The provisioned Pi prefers ethernet whenever it is plugged in and falls back to wifi otherwise. This comes from dhcpcd route metrics, with no custom fallback logic.
  - wpa_supplicant runs from the stock runit service, pinned to wlan0.
  - The passphrase never reaches the Pi: the beacon derives the WPA PSK (PBKDF2) and writes only that, to a 0600 file.
  - The passphrase is redacted from all logs, including the beacon's `/logs` page. It is never echoed back into a page; the wipe-override form asks for it again.
  - The plan summary says whether existing wifi is kept: kept when existing data is preserved; removed when the OS is reinstalled or wiped, or the root partition is reformatted.
  - Recovery: if wifi fails, plug in ethernet and reprovision.
  - See ADR-008.
- **New `private-file` component** — writes credential files atomically: a unique temp file, then fsync and rename, with the exact mode applied. A symlink at the target is replaced, not followed. mdma-console's cookie file now uses it.
- **New `wifi-config` component** — validated SSID, passphrase and country types, plus PSK derivation and wpa_supplicant config rendering.

### Known limitations

- WPA3-only (SAE) networks are not supported; WPA2 and WPA2/WPA3 mixed mode work.
- Only one network can be configured.
- Changing wifi on an already-provisioned Pi requires reprovisioning.
- `<host>.local` may resolve to two addresses while both ethernet and wifi are up.

### Upgrade notes

Wifi is set only at provisioning time, so existing installs are not affected. Leaving the wifi fields blank behaves exactly like 0.26.1. The 0.26.1 migration notes (`migrate-bootfs`) still apply to Pis provisioned before 0.26.1.

---

## [0.26.1] — 2026-10-26

### Fixed

- **NVMe boot partition is now mounted at `/boot`** (#65) — kernel updates via xbps reached the rootfs `/boot` but not the boot partition the firmware reads, so a Pi hung on its next reboot after `xbps-install -Su`.
- **Provisioning never destroys data by accident** (ADR-007):
  - an existing partition table is never rewritten;
  - failed partition reads abort instead of being treated as empty;
  - data partitions are formatted only when created in that run;
  - fstab uses PARTUUID, with `nofail` on data partitions;
  - the beacon no longer updates itself mid-provision.
- `/etc/mdma` is writable by the `mdma` group, and the Bandcamp cookie file is mode 0600 (#38).
- The SD image's `config.txt` no longer accumulates duplicate initramfs lines (#64).

### Added

- **Reboot guard** — `mdma admin boot-health` checks that the boot partition holds the installed kernel, and `mdma-admin` refuses to reboot into a kernel without matching modules (`--force` overrides).
- **`/usr/libexec/mdma/migrate-bootfs`** for already-provisioned units: dry-run by default, `--apply` to change anything, resumable, never touches `/music`.
- **Beacon "Boot order" page** (#63) — switch between SD-first (rescue) and NVMe-first (normal).
- ADR-006 (boot partition at `/boot`) and ADR-007 (provisioning never destroys data partitions); ADR-002 amended.

### Changed

- Force-wipe is replaced by **Reinstall OS** (boot/root/var only) and a deliberate **Wipe drive**, which requires typing `WIPE` and checks that the drives have not changed since the page loaded.

### Upgrade notes

Do not run `xbps-install -Su` on units provisioned before 0.26.1 until `migrate-bootfs` has run. Install the new `mdma-admin` first (`xbps-install -u mdma-admin`), then run `/usr/libexec/mdma/migrate-bootfs` (review the dry-run, then `--apply`).

---

## [0.26.0] — 2026-10-24

### Added

- **Gapless playback** — the next queued track is preloaded and the mixer switches to it sample-accurately when the current track ends (verified on hardware: 0-sample gap/overlap at the join). A sample-rate change between tracks (e.g. 44.1 to 48 kHz) falls back to playing the current track out fully and reopening the output (~90 ms gap, no cut tail).
- **Encoder delay/padding trimming** for lossy formats (MP3/AAC) so joins are gapless there too.
- **Skip reuses the preloaded track** (~10 ms to start).
- **Immediate pause/resume** (resumes at the exact sample); volume is applied at the output so changes are heard immediately. Engine side only; CLI/playback control for volume and seek is not yet exposed (see #60).
- **Stream source protocol** — `Preload`/`ClearPreload` commands and `preloaded` (the track that plays next) in the status.

### Changed

- Now-playing and position follow what is actually heard (previously ran ~4 s ahead).
- Logging: status polls and per-track lifecycle messages are at debug level; info logs contain only meaningful events (track advances, loads, errors). Underrun warnings only for real mid-stream starvation.
- Removed the unused `admin-ipc-client` component; builds go through polylith profiles only.

### Fixed

- PipeWire output data race between the RT callback and the control thread (#58) — could crash on skip/stop.
- Old-track audio could leak into the next track on skip.
- `mdma-audio` and `mdma-playback` no longer exit on unknown/unparseable commands.

### Upgrade note

Deploy `mdma-audio` before `mdma-playback`: a new `mdma-playback` sends `Preload`, which 0.25.x `mdma-audio` cannot parse. Package upgrades via `xbps-install -Su` handle both together.

---

## [0.25.2] — 2026-10-24

### Fixed

- **Auto-advance no longer cuts the last ~4–5 s of every track** — a track now counts as finished only after its ring buffer is empty and the PipeWire output has played everything out (two consecutive silent callbacks). Underrun warnings no longer spam the log while idle, and the audio callback no longer allocates per call.

### Added

- **`mdma-playback` logs each automatic track advance** — finished track, next track, and position/duration.

### Changed

- devenv-generated `.claude` paths are no longer tracked; `Cargo.lock` catch-up.

### Known limitations

- Very short tracks (under ~4.5 s) can still be cut in a rare timing window.
- Gapless playback is planned next.

---

## [0.24.0] — 2026-06-19

### Changed

- **`mdma library track replace` is now a HARD replace** — the new file is ingested, the old track's facts are fully retracted (the old hash stops resolving entirely, not just hidden), a `Replaces` provenance link is recorded on the new track, and playlists rewire to the new track. Self-healing replacement chains are followed on read, including multi-step chains.
- **Track lifecycle commands moved under `mdma library track`** — `replace`, `delete`, `restore`, and `orphans` are now subcommands of `mdma library track` (previously top-level `mdma track`).

### Fixed

- **Fact retraction now matches on value AND source** — a metadata correction or single-source retraction no longer clobbers a still-valid value asserted by a different source. Closes #96, closes #2.

---

## [0.23.0] — 2026-06-12

### Added

- **Track lifecycle commands** — manage track identity over time without rewriting history.
  - `mdma track replace <old-hash> <new-file>` — ingest a better-quality version of a track; the old track is marked `SupersededBy` and every playlist referencing it is rewritten in place with position preserved. The file path must be on the device running the library service.
  - `mdma track delete <hash>` — soft-delete: track is hidden from search, lists, and exports; the file remains on disk and is fully recoverable.
  - `mdma track restore <hash>` — un-hide a soft-deleted or superseded track.
  - `mdma track orphans` — list hidden tracks (deleted or superseded) as garbage-collection candidates.
  - Design: pure-assert facts (`SupersededBy`, `Deleted`) per ADR-003; content hash remains the library's identity key. Closes #25.
- **CDJ-safe AIFF/WAV export** — `mdma rekordbox export` now downsamples sources above 48 kHz (96→48 and 88.2→44.1 using musical-ratio halving via the soxr resampler) so Pioneer CDJs no longer reject exported files as an illegal format. Sources at ≤48 kHz pass through untouched. Every resample is logged. Closes #28.

### Fixed

- **CI stability** — switched to stable nixpkgs (nixos-25.11) and a slim CI shell, eliminating the recurring Nix cache-eviction failures (CVE-2025-5244).

---

## [0.22.1] — 2026-06-10

### Fixed

- **Package build pipeline silently dropped `depends=` from void-packages templates** — `xbps-create` was invoked without `--deps`, so runtime dependencies declared in void-packages `template` files were never embedded in the built `.xbps` packages. mdma-console, mdma-audio, mdma-playback, and beacon now declare correct runtime dependencies in the built packages. Affected all releases since v0.3.0.
- **Rekordbox AIFF export no longer fails on fresh installs** — ffmpeg is now pulled automatically as a transitive dependency of mdma-console; manual `xbps-install -y ffmpeg` is no longer required after provisioning.
- **mdma-playback correctly depends on mdma-audio** — the dependency was declared in the void-packages template but was silently dropped by the build pipeline; it is now embedded in the built package.
- **beacon depends on `avahi-daemon`** — was `avahi`, which is a different package name and broke transitive dependency resolution.

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
