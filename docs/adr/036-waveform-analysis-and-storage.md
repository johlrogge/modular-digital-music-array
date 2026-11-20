# ADR-036: Waveform analysis and storage

## Status

Accepted, 2026-11-20 (Joakim decided). Not yet implemented.

- **Builds on:** ADR-004 (hard replace), ADR-010 (AudioHash), ADR-013 (duplicate merges), ADR-035 (64 KiB service-value cap).
- **Related:** ADR-005 (rekordbox round-trip contract), ADR-021 and ADR-030 (replicas and local reads), ADR-037 (AIFF ingest).

## Context

- **Two consumers need waveforms.**
  - The console's beatgrid editor needs a large scrolling waveform with beats and bars, and an overview strip with cues.
  - cdj-media will write Pioneer ANLZ waveform tags.
- **The library has no waveform data today.** The only per-blob work it does is in `components/library_service/src/identity.rs`.
  - `identity_facts_for_blob(path, BlobFactSet, &BlobFactSources)` computes AudioHash, PcmHash and `DurationMs`.
  - Ingest calls it with `BlobFactSet::INGEST` (AudioHash only).
  - The explicit backfill calls it through `LibraryRequest::IdentityBackfill(IdentityBackfillScope)` in `service/backfill.rs`.
- **The backfill worker already has the right properties.** It is a single thread at nice 19 with the idle IO class (`lower_current_thread_priority`). It sleeps as long as each track took (`MIN_DUTY_SLEEP`), calls `drop_page_cache` after each blob, and can be cancelled and resumed.
- **AudioHash (ADR-010) is the right key for audio-derived data.** It is `audio-sha256-v1:<64 hex>` (`music_primitives::AudioHash`). A retag keeps it; a re-encode or a hard replace changes it.
- **`audio_decoder` invents a sample rate when it can't find one.** `AudioSource::init_decoder` does `track.codec_params.sample_rate.unwrap_or(44100)`. A wrong rate silently stretches every analysis column and `duration_ms`, and makes playback run at the wrong speed.
- **Blobs live under `LibraryService::music_dir`**, which the console calls `music_root` (default `/music`): `blobs/<2ch>/<hash>.<ext>`, `cover-art/`, `archive/`.

## Decision

### 1. Analysis

1. **Input.** The blob is decoded with `audio_decoder::AudioSource`, the same decoder and gapless trimming as playback, so time zero is the same as for cues and grids. Only `samples[..valid_samples]` of each `DecodedSegment` is used. There is no resampling.
2. **Mono.** Each frame becomes the mean of its channels.
3. **Bands.** RBJ biquads, Q = 1/√2 (Butterworth), with coefficients for the track's rate. The state is f64, starts at zero and runs across the whole track.
   - **full:** unfiltered mono.
   - **low:** low-pass at 200 Hz.
   - **mid:** high-pass at 200 Hz, then low-pass at 2.5 kHz.
   - **high:** high-pass at 2.5 kHz.
4. **Columns.** 150 columns per second, the same as Pioneer's detail waveforms (§6).
   - Column `i` covers frames `[floor(i·rate/150), floor((i+1)·rate/150))`, cut off at the frame count. This also works when rate/150 is not a whole number, as at 32 kHz.
   - The column count is `L = ceil(frames·150 / rate)`, computed in integers.
5. **Value.** Each column holds the peak `|x|` of the band, stored as `round(clamp(peak, 0, 1) · 255)`. The scale is linear, absolute and the same for all bands. There is no normalisation per track or per band, so tracks and bands can be compared. Consumers scale for display.
6. **RMS plane.** In addition to the four peak planes, each column holds the full-band RMS, `sqrt(mean(x²))` of the unfiltered mono over the column's window, stored on the same scale (`round(clamp(rms, 0, 1) · 255)`). Added at cdj-media's request (2026-11-20) for Pioneer's "whiteness" and similar density displays; adding it later would have forced a v2.

### 2. File format v1

The path is `<music_dir>/waveforms/v1/<digest>.wfm`. `<digest>` is the 64 lowercase hex characters of the track's AudioHash, without the `audio-sha256-v1:` prefix. AudioHash is the only key; there is no fallback key.

| Offset | Size | Field | Value |
|---|---|---|---|
| 0 | 4 | magic | `MDWF` |
| 4 | 1 | format version | `1` |
| 5 | 1 | plane count | `5` |
| 6 | 2 | columns per second | `150`, u16 LE |
| 8 | 4 | analysis sample rate | the track's rate in Hz, u32 LE |
| 12 | 4 | column count `L` | u32 LE |
| 16 | 8 | frame count | decoded frames after gapless trimming, u64 LE |
| 24 | 5·L | planes | `full[L]`, `low[L]`, `mid[L]`, `high[L]`, `rms[L]`, u8 each |

- **Layout.** The planes are planar, in the band order above, which is fixed by the version. The file size is exactly `24 + 5·L` bytes.
- **Size.** About 270 KB per 6-minute track; about 245 MB for the current library of about 900 tracks.
- **Invalid files.** A reader rejects a file if the magic is wrong, the version is not the one of its directory, the plane count is not 5, the column rate is not 150, or the size is wrong. A rejected file counts as absent, and the next backfill recomputes it.
- **Writing.** The file is written to `<digest>.wfm.tmp` in the same directory and then `rename(2)`d into place, so readers never see a partial file.
- **The component.** A new pure component, `components/waveform`, owns:
  - the analyser (`push` interleaved f32, then `finish`);
  - the `Waveform` type and a `Plane` enum (`Full`, `Low`, `Mid`, `High`, `Rms`);
  - `read`/`write` over `io::Read`/`io::Write`;
  - `Waveform::downsample(columns)`.

  It does no file-system I/O and has no dependency on the decoder.

### 3. Versioning

1. Any change to the filters, crossovers, scale, column rate, band set, layout or AudioHash version makes a new version: a new directory (`v2/`) and a new header version.
2. Readers read only their own version. A missing file in their version means "no waveform".
3. During a rollout both versions exist, and a backfill fills the new one. Deleting the old directory is part of the new version's rollout, not of GC.

### 4. Computation: the BlobBackfill lane

1. **The rename.** `IdentityBackfill*` becomes `BlobBackfill*`: the scope, progress, status and start types, the `start_`/`cancel_`/`run_identity_backfill` methods, and the `identity_backfill`/`identity_cancel` fields. The wire tags stay as they are, through `#[serde(rename = "IdentityBackfill")]` and similar, so the rename is a refactor and not a protocol change.
2. **A waveform step.**
   - `BlobBackfillScope` gains `include_waveform`, with `#[serde(default)]`.
   - `Work` gains `waveform: WaveformStep { Skip, Compute }`.
   - A track is planned for the step when it has, or computes in the same item, an AudioHash, and `waveforms/v1/<digest>.wfm` is absent or invalid.
   - `BlobFactSet` stays about facts only.
3. **After ingest.** Ingest stays synchronous and cheap (AudioHash only, as today). After `write_and_project`, it puts a waveform item on the BlobBackfill worker's queue. Ingest items are taken before planned backfill items and do not create a run in `BlobBackfillStatus`. The lane keeps its single thread, priority, duty cycle and `drop_page_cache`.
4. **No recheck under `retire_gate`.** The step writes a file, not facts, so it cannot bring a retired entity back. A track retired during the computation leaves an orphan file, which §5 removes.
5. **Backfill of the existing library.** An explicit `BlobBackfill` run with `include_waveform` processes the existing library, and resumes like today: tracks whose file exists are left out.
6. **Waveforms never gate anything.** A track without a waveform can be listed, searched, queued, played and edited. A failed analysis is logged and counted in `failed`, and writes nothing. Consumers draw nothing, or leave the tag out.

### 5. Lifecycle

- **Retag:** the AudioHash is unchanged, so the file is kept and nothing is recomputed.
- **Changed audio** (re-encode or hard replace, ADR-004): a new AudioHash, so a new file after ingest; the old file becomes an orphan.
- **Same-audio duplicates** (ADR-013) share one file.
- **GC** runs on the BlobBackfill worker at the end of every explicit run. It removes each `waveforms/v1/*.wfm` whose digest no indexed track carries (live and soft-deleted tracks both count as carrying it, since soft-deleted tracks can still be restored), and removes stale `*.wfm.tmp` files. GC and computation share one thread, so they never overlap. The worst a race can do is remove a file that the next backfill recomputes, which §4.6 makes acceptable.
- **Rebuildable:** the store is derived data under `/music`.

### 6. Access

1. **IPC.** The library owns the store.
   - `LibraryRequest::Waveform { hash: ContentHash }` resolves the track's AudioHash and reads the file.
   - The answer is `LibraryResponse::Waveform(Option<WaveformFile>)`: the raw `.wfm` bytes, base64 in JSON. Every consumer parses it with `waveform::Waveform::read`.
   - `None` means "no waveform". It is a normal answer, not a new `ProtocolError` variant, because old clients can't decode new error variants. An unknown hash is `TrackNotFound`.
   - The gateway forwards it like other library requests, and `library_service_stub` answers `None`.
   - Waveforms are not in the fact log, so replicas (ADR-021, ADR-030) don't have them; clients always ask the node.
2. **Console.** `GET /api/tracks/:hash/waveform?columns=N` answers JSON `{per_second, frames, sample_rate, columns, span_ms, full, low, mid, high, rms}`, each plane base64 u8, with `span_ms = L·1000/150`.
   - The console downsamples on the server with `Waveform::downsample`: output column `j` is the maximum of source columns `[floor(j·L/N), floor((j+1)·L/N))` for the peak planes, which keeps peaks, and the root of the mean of squares for the RMS plane.
   - Without `columns`, or with `N ≥ L`, the answer is native (150/s, never upsampled).
   - The answer is 404 `{error}` when there is no waveform. Library failures map through `library_failure_status`.
   - The beatgrid editor fetches native columns for the zoomed view (bars from `/api/tracks/:hash/timeline`), and `columns` equal to the canvas width for the overview strip.
3. **cdj-media** reads waveforms through the same IPC request and the same `waveform` component. Pioneer's formats are its business; the library stores none of them.

| ANLZ tag | Pioneer shape | From the stored data |
|---|---|---|
| PWV3, detail (.EXT) | 150/s, 5-bit height + 3-bit whiteness | 1:1 with columns; height `full >> 3`; whiteness from the band balance |
| PWV5, colour detail (.EXT) | 150/s, 3+3+3-bit RGB + 5-bit height | 1:1; RGB from low/mid/high, height from full |
| PWAV, preview (.DAT) | 400 entries, height + whiteness | `downsample(400)` |
| PWV2, tiny preview (.DAT) | 100 entries, 4-bit height | `downsample(100)` |
| PWV4, colour preview (.EXT) | 1200 entries × 6 bytes | `downsample(1200)`; byte meanings are cdj-media's call |
| PWV6/PWV7, 3-band (.2EX) | 3 bytes per entry | low/mid/high directly, if cdj-media targets them |

### 7. Prerequisite: no invented sample rate

`AudioSource::init_decoder` stops doing `unwrap_or(44100)`. An unknown rate becomes a `DecoderError` (a new `UnknownSampleRate` variant, or `Decode`), so such a file fails to open rather than playing or being analysed at the wrong speed. Proposed, to confirm in P2: the channel count gets the same treatment (`unwrap_or(2)`).

## Cost

Estimates only; P1 adds an ignored timing test and P5 measures on a Pi.

- **Per track on a Pi 5,** 6-minute 44.1 kHz FLAC or MP3: decode about 1.5–4 s, downmix plus four biquads and the RMS sum over about 16 M samples about 0.3–0.6 s, so about 2–5 s of one core. Hi-res tracks take about twice as long.
- **The whole library,** about 900 tracks at the 50 % duty cycle: roughly 1–2.5 hours at nice 19.
- **Ingest** adds a few seconds of background work per track.
- **No shared decode yet.** A run that asks for PcmHash and waveform decodes twice. Sharing the decode waits until measurements show it is needed.

## Phased plan

- **P0:** rename IdentityBackfill to BlobBackfill (§4.1). No change in behaviour or on the wire.
- **P1:** `components/waveform`: analysis, `.wfm` read/write, `downsample`. Tests: 50 Hz sine lands in low, 1 kHz in mid, 8 kHz in high; a full-scale sine gives 255 in full and silence gives 0; the column count (including 32 kHz and a partial last segment); RMS of a full-scale sine is about 180 (1/√2), of a full-scale square 255; write/read round trip; rejection of bad magic, version, band count or size; downsampling keeps peaks and never upsamples; the ignored timing test.
- **P2:** the `audio_decoder` rate fix (§7).
- **P3:** the library store, `include_waveform`, IPC `Waveform`, the gateway, `library_service_stub`.
- **P4:** CLI (hicli): `mdma library blobs backfill [--pcm] [--duration] [--waveform]` replacing `library identity` (the old name as an alias if hicli supports aliases), and `mdma library waveform <hash>` printing column count, span and peak per band.
- **P5:** the automatic step after ingest (§4.3), the backfill of the existing library, and the cost measurement on a Pi.
- **P6:** the console endpoint and canvas: the editor's zoomed view with bars, and the overview strip with cues.
- **P7:** the GC sweep (§5).
- **P8:** the cdj-media adapter (§6.3), after the open questions are settled.

## Consequences

- The beatgrid editor gets data: native resolution is one request of about 360 KB per track, and the overview needs one small request.
- Waveforms are comparable by eye because the scale is absolute; quiet tracks look quiet.
- A retag never triggers a recompute. Changed audio leaves orphans until the next explicit run's GC.
- **Tracks without a waveform:** tracks whose AudioHash failed (for example some M4A/MP4 layouts `audio_hash` rejects with `UnsupportedContainer`), and tracks whose rate is unknown (§7). AIFF is not ingestible as of this ADR; ADR-037 converts AIFF to FLAC on ingest, so converted tracks get waveforms. An AIFF stored as AIFF under ADR-037 gets an AudioHash too, since AudioHash v1 supports AIFF and AIFF-C (ADR-010, amended 2026-11-20), and so a waveform, provided the decoder can play it (symphonia 0.5.5 takes AIFF-C `twos` and `sowt` at 16 bits only; see ADR-037).
- About 270 KB per 6 minutes on `/music`, rebuildable by a backfill.
- Gateway, library and console deploy together (ADR-004). The CLI is upgraded with the node, as usual.

## Alternatives considered

- **Store waveforms as facts.** Rejected: 270 KB is far over ADR-035's 64 KiB `OpaqueValue` cap; facts stay small because they go to every replica; and the append-only log would grow with every recompute.
- **Compute on demand when the console asks.** Rejected: each view would wait 2–5 s, decoding would compete with playback exactly when the user is active, and the work would be repeated. Computing eagerly in the background is the same work, done once at low priority.
- **Use rekordbox's ANLZ as the source.** Rejected: it covers only tracks rekordbox analysed, is quantised to Pioneer's 5-bit heights and colour schemes, and would put Pioneer data in the library, against ADR-005 and the MDMA-primary rekordbox sync.
- **Normalise per track.** Rejected: tracks would no longer be comparable.
- **Fall back to ContentHash when there is no AudioHash.** Rejected: ContentHash changes on a retag, and two keys complicate GC and lookup.
- **Store floats or a pyramid of resolutions.** Rejected: u8 is plenty for drawing and for Pioneer's 3–5-bit fields, and downsampling about 54 k columns on request is cheap.

## Future: mixer-matched band profiles (not urgent)

Joakim would like the UI to offer a **4-band waveform whose crossovers match a specific DJ mixer's EQ**, for example the Allen & Heath Xone:92/96, a Pioneer 4-band EQ, and other 4-band EQs, so that what the waveform shows matches what the EQ knobs cut.

- This is a different band set and different crossovers, so under §3 it is a new version or a parallel **band profile**: for example `waveforms/<profile>/v1/<digest>.wfm`, with the profile's crossover frequencies recorded in the header.
- The 3-band profile above stays the default and the source for Pioneer tags.
- Each profile is about 270 KB per 6-minute track (5 planes), computed by the same BlobBackfill lane, only for profiles the user enables.
- To decide when built: the exact crossover frequencies and filter slopes per mixer, and whether profiles are per node or per user.

## cdj-media's request (2026-11-20) and answers

cdj-media asked for one resolution (150 entries/s over the whole track; it reduces to 400, 100 and 1200 columns itself), linear 0..1 full-band peak plus low/mid/high peaks after a ~200 Hz / 2.5 kHz split, windows `[i·rate/150, (i+1)·rate/150)` with count `ceil(frames·150/rate)` (rekordbox's PWV3 count), peak not RMS, plus RMS as an extra field if available, keyed by audio hash and aligned with the exported file's timeline. cdj-media maps values to Pioneer heights and whiteness itself, calibrated against rekordbox's waveforms. This ADR serves exactly that:

1. **Resolution and values:** native 150/s, linear u8 (0..255 ≙ 0..1), planes full/low/mid/high peak plus full-band RMS (§1.6). Crossovers: 200 Hz and 2.5 kHz, 2nd-order Butterworth.
2. **Windows and count:** identical to cdj-media's formula (§1.4).
3. **Timeline:** columns start at sample 0 of the gapless-trimmed decode at the track's native rate, the same timeline as playback, cues and grids. Two small differences from an exported file: AAC exports are trimmed at the container duration and may keep up to about 1 ms of padding; exports of sources above 48 kHz are resampled to 48 kHz, so they stay time-aligned but have a different frame count. cdj-media should therefore take the entry count from the waveform's own `frames` and `sample_rate`, not from the exported file. The difference is at most one entry at the end.
4. **Format:** the endpoint without `columns` returns the native planes, with `per_second`, `frames` and `sample_rate`.

## Remaining open questions (with cdj-media)

1. What are PWV4's six bytes derived from? (cdj-media's call; the planes above are the inputs.)
2. An MP3 offset that players or rekordbox need is cdj-media's correction, applied to waveform, grid and cues alike.
