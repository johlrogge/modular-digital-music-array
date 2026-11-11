# ADR-019: USB stick sync: manifest-owned, profile-driven, one-way

## Status

Proposed, 2026-11-11

Depends on the planned client-owned fact sources ADR (not yet written) for import provenance (§7). Related: ADR-010, ADR-012, ADR-013 (dedup tiers), ADR-018 (source identity).

## Context

Joakim wants MDMA to fill USB sticks for other players, the car (BMW i4, iDrive 8) first. A stick is plugged into the Pi. MDMA fills it with the stick's playlists and named queries, offers to import any new music it finds, and ejects it, CDJ-style, with a status light and a hardware eject button.

What exists:
- `mdma export` (`projects/mdma-cli/src/transfer.rs`) already writes playlists to a directory. It has path sanitising and shortening (`sanitize_path_component`, `shorten_component`, case-folded collision handling in `plan_export`). Its cost function `path_cost` measures the rekordbox `Location` URI, not FAT names.
- `mdma-admin` is the privileged service (`AdminRequest` in `components/admin_ipc_protocol`). It is the only place root operations belong.
- The library inbox is the single ingest path that monitoring observes.
- Car facts are thin (forum quotes of the manual): FLAC, MP3, M4A and AAC listed, WAV reported, FAT32 and exFAT recommended, NTFS not working. Folder and file-count limits are unknown.

## Decision

Points marked **Decided** were decided by Joakim. Everything else is **Proposed**.

### 1. Stick profiles are settings, not facts (Decided)

A profile maps a stick to a selection: named playlists, named queries (library search grammar) and a target format list. Profiles are device configuration, not music knowledge, so they live in a settings file under `/metadata` (which survives a reinstall). mdma-usb is its only writer. The CLI edits it over the gateway (`mdma usb assign`, `mdma usb show`).

### 2. Identity is the filesystem UUID (Decided)

- The key is the volume UUID (vfat/exFAT volume serial). The label is a display name only. When a known UUID shows up with a new label, mdma-usb updates the stored label.
- The first link may be made by label (`mdma usb assign <label> …`). It binds when exactly one attached stick has that label. mdma-usb then records the UUID, and after that the label is never used for matching. If several attached sticks share the label, the assign is refused.
- **Non-unique UUIDs (cloned sticks, e.g. from `dd`):**
  - Inserted one at a time, clones are the same stick to MDMA and get the same profile. This is safe, because the sync state (the manifest, §4) lives on each stick, so each clone is diffed against its own contents.
  - Two sticks with the same UUID attached at the same time: neither is mounted or synced, the LED shows an error, and the CLI reports the collision.
  - A stick without a UUID cannot be linked.

### 3. Layout (Decided; naming may be revisited)

```
/Playlists/<playlist>/NNN - Artist - Title.<ext>
/Queries/<query>/Artist - Title.<ext>
/.mdma/manifest.json
/.mdma/tmp/                 (in-flight copies)
```

`NNN` keeps playlist order. A track in two members is written twice. The planner warns when a folder goes over 255 files (a precaution, not a known limit).

Path safety reuses the `transfer.rs` logic, moved into a shared component rather than copied. The component takes its cost function and limits as parameters: rekordbox `Location` bytes for export, FAT long-name units for sticks. FAT adds a few rules: strip trailing dots and spaces, drop control characters, and treat names that differ only in letter case as the same name.

### 4. Manifest-driven one-way mirror (Decided)

- `/.mdma/manifest.json` is versioned. It records the stick UUID and, for each file MDMA wrote: relative path, library content hash, written size and mtime, member, and written format.
- **Core invariant:** MDMA deletes or overwrites only paths listed in the manifest. Files not in the manifest are never touched.
- Mirror: a track that leaves the selection is deleted from the stick (manifest-owned files only).
- An owned file whose size or mtime has changed is left alone and dropped from the manifest. It becomes the user's file.
- The plan is a pure function: (desired selection, manifest, stick listing) gives copies, deletions, disowned files, skips and import candidates. This makes it unit-testable, and `--dry-run` prints it.
- Writes: copy to `/.mdma/tmp/`, flush, rename into place, flush the directory. The manifest is written the same way after each batch. An interrupted sync is safe and resumes. Leftover files in `/.mdma/tmp/` are MDMA's and are cleaned up on the next mount.

### 5. Formats (Decided)

Copy the stored format. Convert (via `components/audio_transcoder`) only formats the profile's format list cannot play. The manifest records the source hash and the written format, so a converted file is not converted again on every sync.

### 6. Lifecycle: auto-sync, auto-eject (Decided)

- Insert a stick with a profile: mount, sync, write the manifest, flush, unmount. The LED then shows safe to remove.
- Unknown sticks are not synced and not written to.
- Eject (button, CLI or console): finish the current file, write the manifest, flush, unmount.
- Detection: poll `/sys/block` in mdma-usb (no libudev dependency). A udev `RUN+=` notifier is the later alternative.

### 7. Import from a stick (Decided; provenance depends on the planned ADR)

- Candidates are ingestible audio files on the stick that are not in the manifest and not under `/.mdma`.
- A candidate whose content hash matches a library track is not imported. The AudioHash and PcmHash tiers (ADR-010/012/013) catch retags at ingest.
- The user confirms the import. Files are copied to `/music/inbox/usb/<uuid>/…` and go through the existing inbox, the one ingest path that monitoring sees. Files on the stick are never moved or deleted.
- **Open concern:** a plain inbox ingest writes facts under the library's own identity, so they would not carry a USB source. Under the planned client-owned fact sources ADR, the library must **not** gain an `IngestSource::Usb`. Instead mdma-usb states its own source (tool `mdma-usb`, with an origin kind it owns) in the ingest request's `writer` field, and the library treats it as opaque. Until that ADR lands, imports carry the library identity, and that is accepted.

### 8. Mounting is privileged and narrow

- New `AdminRequest` variants: `ListRemovable`, `Mount { device, mode: ReadWrite | ReadOnly }`, `Unmount { device }`.
- mdma-admin checks the device itself rather than trusting the caller: it must be on the USB bus (sysfs path) and the filesystem must be vfat or exfat. Anything else is refused, so the NVMe can never be mounted or written.
- Mount point `/run/mdma/usb/<uuid>`, never under `/music`. Options `nosuid,nodev,noexec`, owned by the mdma-usb user. Not `sync`: mdma-usb flushes explicitly instead.

### 9. Playback must not suffer

- Copies run at low CPU and I/O priority (`nice` 19, ionice idle class).
- Pi 5 USB power: the total budget is 600 mA unless a 5 V/5 A supply is detected or `usb_max_current_enable=1` is set (then 1.6 A). The stick shares this with a bus-powered DAC. Phase 1 checks the kernel log for undervoltage and over-current, and listens for audio glitches, during a sync while playing.

### 10. Status light and eject button (Decided: NeoPixel + button; pin open)

**Driving a WS2812 on a Pi 5.** GPIO on the Pi 5 goes through the RP1 chip, and rpi_ws281x (which uses BCM PWM/PCM plus DMA) has no Pi 5 support. Candidates:
- **SPI MOSI encoding (preferred).** Each WS2812 bit is sent as 3 SPI bits at about 2.4 MHz (or 8 bits at about 6.4 MHz) through `spidev`, for example with `ws2812-spi` over `linux-embedded-hal`. This is pure Rust and cross-compiles easily. It needs the SPI0 MOSI pin (GPIO10) or another SPI bus's MOSI.
- **RP1 PIO.** Raspberry Pi's `piolib` (`/dev/pio0`) has a ws2812 program and can use any RP1 GPIO. It needs C FFI and a recent kernel and firmware.
- **USB-attached LED controller** (e.g. a small RP2040 board). This uses no GPIO and suits a fitted HyperPixel.

Electrical: WS2812 data expects about 0.7 × VDD. Use a 74AHCT125-class level shifter (or a 3.3 V-tolerant variant). Keep brightness low; one pixel draws up to about 60 mA at full white.

**HyperPixel conflict.** The HyperPixel 4 uses DPI, which takes almost every header GPIO, including SPI0. If it is fitted, SPI MOSI is unavailable, and only PIO on a spare pin or a USB controller remains. The HyperPixel may or may not be fitted, so the pin choice is open. The button uses `gpiocdev` on a free pin, or the USB controller.

**LED states (proposed colours; the pattern carries the meaning for colour-blind safety):**

| State | Light | Remove? |
|---|---|---|
| No stick | off | n/a |
| Mounted, idle (e.g. waiting for import confirmation) | blue, solid | no, eject first |
| Syncing (writing) | amber, pulsing | no |
| Importing (reading from the stick) | cyan, pulsing | no |
| Safe to remove (unmounted) | green, solid | yes |
| Error, still mounted | red, fast blink | no |
| Error, unmounted (sync incomplete) | red, solid | yes |

The state-to-light mapping is a pure function of mdma-usb's state (usb events on the event bus).

### 11. Future: guest sticks as a temporary library layer (out of scope; not precluded)

Playing from a guest's stick without syncing means indexing it and making it browseable only while it is inserted. This design keeps that possible:
- Unknown sticks are never written.
- Mounting supports `ReadOnly` and lives in ephemeral `/run`.
- Stick state lives on the stick or in settings, never in facts, so a layer can sit over the library without polluting the fact log.
- The planner's stick listing and hashing step is separate and can be reused to build an in-memory index.

## Why

- **The manifest as an ownership boundary** makes "MDMA never destroys the user's files" a structural property, not a convention.
- **UUID over label:** labels clash (FAT allows 11 characters, and many sticks ship as "USB"). Keeping the UUID-to-profile link separate from the sync state on the stick makes clones harmless.
- **One ingest path:** imports through the inbox are visible to the same monitoring as every other ingest.
- **A pure planner** gives tests and a dry run for free, before any hardware is involved.

## Alternatives considered

- **Profiles as facts.** Rejected: device configuration, not music knowledge (§1).
- **Label as identity.** Rejected: collisions. The label stays a display name and is used only for the first link.
- **Two-way sync.** Rejected: ownership becomes ambiguous.
- **Accumulate instead of mirror.** Rejected by Joakim. The stick reflects the selection.
- **Album tree plus M3U playlists.** Deferred: the car's M3U support is unproven (Phase 0).
- **Import directly into the library, bypassing the inbox.** Rejected: a second ingest path.
- **`IngestSource::Usb` in the library.** Rejected under the planned client-owned fact sources ADR: the library stays source-agnostic.
- **udisks2 for mounting.** Rejected: brings in D-Bus and polkit when mdma-admin already exists.
- **rpi_ws281x.** Rejected: no Pi 5 support.

## Consequences

- New components: the stick planner and manifest model, a shared path-sanitising component (moved out of `transfer.rs`, with `mdma export` switched to it), and the usb IPC protocol.
- New project `mdma-usb`: an unprivileged service with a gateway route and a srcpkg.
- `admin_ipc_protocol` and `mdma-admin` gain the narrow mount API.
- Event bus: usb events (inserted, syncing, progress, importing, safe to remove, error).
- Import provenance waits for the planned client-owned fact sources ADR.

## Phasing

0. **Car spike, no code:** `mdma export` onto one FAT32 and one exFAT stick (FLAC, MP3, M4A, WAV, 3-level nesting, a 300-file folder, a long name, one `.m3u`). Settles formats, limits and M3U support.
1. **Manual sync and software eject:** planner, manifest, mount API, `mdma usb sync <label|uuid> --playlist … [--dry-run]`, `mdma usb eject`.
2. **Stored profiles and query sets:** settings file, `assign` and `show`, UUID binding.
3. **Hotplug:** `/sys/block` watcher, auto-sync, auto-eject, console status and eject.
4. **LED and button.**
5. **Import:** after the planned client-owned fact sources ADR.

## Open questions

1. Should a pending label-only link bind silently on the first matching insert, or ask for confirmation? Many sticks are labelled "USB".
2. Do profiles refer to playlists by name (a rename breaks the profile) or by a stable id?
3. Should there be a "make distinct" command that rewrites a clone's volume serial?
4. Is exfat built into the Pi kernel? Does the car need MBR rather than GPT?
5. LED and button pins: will the HyperPixel be fitted, and does it work on a Pi 5 at all? Which of SPI, PIO or a USB controller, then?
6. Are the proposed LED colours acceptable?
7. Does ionice actually take effect with the Pi's I/O scheduler (mq-deadline or BFQ), or is a copy rate limit needed?
8. What is the settings file's name and format under `/metadata`?
9. Stick-file naming: confirm, or revise after Phase 0.

## Related

- ADR-010, ADR-012, ADR-013 (dedup tiers that catch retagged imports)
- ADR-018 (source identity); the planned client-owned fact sources ADR (import provenance)
