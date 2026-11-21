# ADR-037: AIFF ingest stored as FLAC, verified lossless, original chunks kept in the blob

## Status

Accepted, 2026-11-20 (Joakim decided). Not yet implemented.

- **Builds on:** ADR-003 (ContentHash entity key), ADR-004 (redeploy together), ADR-010 (AudioHash), ADR-012 (PcmHash), ADR-013 (duplicate merges).
- **Related:** ADR-034 (POPM rating stays in the `aiff` block until mapped), ADR-036 (waveforms).

## Context

- **AIFF matters to the first end user, whose library is AIFF.** AIFF is export-only today:
  - `AudioFormat::is_ingestible` (`components/library_ingestion/src/pipeline.rs`) excludes it;
  - `components/inbox_utils/src/lib.rs` (`INGEST_EXTENSIONS`, `NON_INGESTIBLE_ERROR`) duplicates the policy.
- **Space.** Storing PCM uncompressed wastes about 30–45 % of the NVMe compared with FLAC.
- **ADR-012 anticipates this.** It expects AIFF→FLAC at ingest and makes PcmHash equality of source and output the acceptance check.
- **FLAC's tags can't hold everything an AIFF does:**
  - ID3 GEOB frames (Serato cue points and beat grid), PRIV (Traktor), POPM;
  - the AIFF chunks MARK, INST, APPL, COMT and ANNO.

  ffmpeg's `-map_metadata` and lofty's tag conversion both drop these.
- **libFLAC's `flac --keep-foreign-metadata`** stores every non-audio AIFF chunk verbatim in a FLAC APPLICATION block with the registered id `aiff`. `flac -d --keep-foreign-metadata` rebuilds the original AIFF byte for byte.
- **Today's identity support:**
  - PcmHash covers AIFF and AIFF-C `NONE`/`twos`/`sowt`, integer PCM, 1 or 2 channels (`audio_hash/src/pcm/aiff.rs`).
  - PcmHash on FLAC uses claxon. It refuses 32-bit and the depths that need FLAC bps code 000.
  - AudioHash v1 did not support AIFF. Phase 4 added it (ADR-010, amended 2026-11-20).

## Decision

1. **Convert when verified, otherwise store as AIFF.**
   - An AIFF that passes the acceptance check (5) is stored as a FLAC blob.
   - An AIFF that can't be converted or verified is stored **as an AIFF blob**, unchanged. No AIFF is left behind in the inbox.
   - Storing AIFF blobs requires:
     - AIFF playback (symphonia's AIFF support, enabled and tested);
     - AIFF rules in AudioHash v1, the same approach as WAV: hash the audio data range and ignore tag chunks, so those tracks get identity facts and waveforms (ADR-036);
     - export serving the AIFF unchanged.
2. **Encoding.**
   - It uses libFLAC: `flac -8 --keep-foreign-metadata --verify`.
   - Every non-audio AIFF chunk is stored verbatim in the FLAC APPLICATION block `aiff`.
   - `mdma-library` depends on `flac`, version 1.4 or newer.
3. **Tags for tools.** Vorbis comments and all PICTURE blocks are written with lofty from the AIFF's tag. They are a view for tools: cover extraction, export, rekordbox. The lossless record is the `aiff` block.
4. **Where facts come from.** `generate_facts` is split into `tag_facts`, `file_facts` and `name_facts`.
   - **Tag facts** (`tag_facts`) come from the source AIFF, read before conversion. They are only what the tags state: title, artist, album, track and disc numbers, year, BPM, key, genre, comment, catalog and encoder fields. They carry the content source.
   - **File facts** (`file_facts`) come from the stored blob. They are FilePath, AddedAt, Format, FileSizeBytes, BitDepth, Channels, SampleRate, DurationSeconds and DurationMs, Bitrate and HasAlbumArt. They carry the library source. HasAlbumArt is file-side because after conversion it describes the FLAC's PICTURE blocks.
   - **Name facts** (`name_facts`) are the title, artist and track number parsed from a file name when the tags state no title and no artist. They come from the original file name of the source, never from the staged, hash-named file. They carry the content source and are not part of the tag-fact comparison in check 5.4.
5. **Acceptance.** Every check runs on the final file, before it enters `blobs/`:
   1. COMM channels, rate and frames equal STREAMINFO.
   2. PcmHash(AIFF) equals PcmHash(FLAC). Claxon decodes this, independently of libFLAC.
   3. `flac -d --keep-foreign-metadata` rebuilds a file whose SHA-256 equals the AIFF's ContentHash.
   4. The tag facts read from the FLAC equal those read from the AIFF. Only tag facts are compared (not file or name facts). The FLAC's PICTURE blocks match the AIFF's APIC frames in count, type, MIME type and bytes. One exception: `EncoderSoftware` facts in the FLAC that the AIFF does not state are ignored, because lofty reads the Vorbis vendor string (libFLAC, or empty once the tags are rewritten) as one.

   If any check fails, the file is **refused** and decision 1's fallback applies: the AIFF is stored as AIFF, and the reason is logged and shown in the ingest result. With today's PcmHash and libFLAC that covers:
   - float and compressed AIFF-C;
   - more than 2 channels;
   - bit depths other than 8, 12, 16, 20 and 24. These are exactly the depths claxon decodes (libFLAC writes the others with the STREAMINFO fallback, which claxon does not take), so 32-bit is refused;
   - non-integer sample rates;
   - any rebuild mismatch;
   - **every AIFF-C, with libFLAC 1.5.0** (the only version tested; libFLAC-version dependent, and the Pi's version has to be checked in phase 0/9). `twos` fails at encode (`can't handle AIFF-C compression type`). `NONE` and `sowt` encode but fail check 5.3 (`stored main chunk length differs from written length`), with or without FVER and ID3, whatever the compression name. A libFLAC that handles AIFF-C turns these into conversions with no code change;
   - tags lofty cannot read. lofty 0.18.2 skips the pad byte of an odd-length `COMT` chunk twice and loses its place (`no "SSND" chunk`), so the tag facts cannot be read and the file is refused. Phase 4 makes AIFF-blob ingest tolerate unreadable tags (file and name facts only, with a warning in the ingest result), so no AIFF is left behind. Whether a newer lofty fixes this has not been verified (the registry cache here holds the index only, up to 0.25.5, no sources); upgrading is a separate change.

   **Refusal vs environment.** Only a refusal, which is about the file, gives an AIFF blob. A failure of the machine (no `flac`, or one older than 1.4; staging-directory or staged-file I/O; a `flac` that cannot be started or is killed) is an *environment error*: the ingest stays pending and is retried, and the file is not stored as AIFF because of it. The encoder returns the two as separate error kinds.
6. **Identity.**
   - The blob's ContentHash is the entity key.
   - A new fact `EncodedFrom(ContentHash)` records the source AIFF's hash. It is multi-valued and united on merge (ADR-013), and it is part of the ingest dedup set. Re-ingesting the same AIFF is then a duplicate, with no decode.
   - PcmHash is written at ingest for converted tracks.
7. **Where it runs.**
   - Conversion runs on a background worker at nice 19 / idle IO, never on the IPC thread.
   - It stages in `/music/staging/<source-hash>.flac`.
   - Phase 6's startup sweep removes `*.encoding.flac`, `*.rebuilt.aiff` and orphaned `<hex>.flac` in staging, and never promotes a leftover.
   - The order is: blob rename, then facts, then removal of the source.
   - A queued job keeps the request's writer and facts in a sidecar, `staging/jobs/<hash of the path>.json`, until it ends stored or its file is gone; the startup sweep leaves that directory alone and re-queues from it (an orphan sidecar is removed).
   - On SIGTERM or SIGINT `mdma-library` stops the worker after the job in hand. An encode can outlast the supervisor's wait: `sv stop` waits 7 s and reports a timeout; the process is killed only by `sv force-stop` or the system shutdown. The library bounds its own wait (3 s) and exits; the `flac` child dies with it (`PR_SET_PDEATHSIG`), and the startup sweep removes what an interrupted encode left in staging.
   - The ingest protocol gains a pending state.
8. **The source AIFF** is deleted after a successful, verified conversion. Check 5.3 proves the FLAC rebuilds it byte for byte.
9. **Same audio, different bytes.** A retagged AIFF, or an MDMA-exported AIFF re-imported, becomes a new track with an equal PcmHash. It is handled by the ADR-013 dedup scan, which offers the merge for confirmation.

## Why

- Lossless decoding is bit-exact, and check 5.3 proves the original file can be recovered byte for byte: the audio and every chunk. Deleting the source loses nothing, and no metadata sidecar or archive copy has to be kept consistent.
- One acceptance rule replaces a per-variant table. Widening PcmHash later widens what converts.
- Facts from the source mean the conversion can't change what the library knows.
- Falling back to an AIFF blob means no user file is left behind.

## Testing

- **No fixture files in the repo.** AIFF files are built in memory at test time by the `audio-fixtures` component (`components/audio_fixtures`, dev-dependency only). `AiffBuilder` makes synthetic tones or noise at 8 to 32 bits, mono or stereo, AIFF or AIFF-C (`NONE`, `twos`, `sowt`). Depths that are not a multiple of 8 are left-justified in their byte container. The builder adds extra chunks and an `ID3 ` chunk (v2.3 and v2.4). The ID3 tag carries TIT2/TPE1/TALB/TCON/TDRC (v2.4) or TYER (v2.3)/TRCK/TPOS/TBPM/TKEY/COMM/TXXX (two)/USLT/POPM, two APICs (front and back), a GEOB "Serato Markers_" and a PRIV "TRAKTOR4". Extra chunks are MARK/INST/COMT/ANNO, including an odd-length chunk with its pad byte. Refusal cases are fl32, ulaw, 3 channels and 32-bit. The builder makes `twos`, `NONE` and `sowt` files because PcmHash reads them, but with libFLAC 1.5.0 none of them converts (decision 5): the encoder's tests pin the refusal per codec, and a libFLAC that fixes it flips them to verified. The builder's tests parse its output with lofty, and `audio_hash` tests read it back with PcmHash.
- **No real, copyrighted audio in the repo.** An ignored test runs the full conversion and verification against local real files given by an environment variable, `MDMA_REAL_AIFF_DIR`:
  - a Bandcamp release downloaded as `aiff-lossless`, which covers the common case;
  - a Beatport download, which covers most of the rest.

## Consequences

- FLAC blobs carry the original chunks. Cover art is stored twice: inside the ID3 chunk and as a PICTURE block.
- AIFF blobs exist for unverifiable files. Playback, AudioHash, waveforms and export must handle them.
- **No space saving for AIFF-C**, while libFLAC cannot round-trip it. Phase 9 measures how common AIFF-C is in the real libraries.
- MDMA AIFF exports (16-bit for sources of 16 bits or less, 24-bit otherwise, ≤48 kHz) PcmHash-match the stored FLAC. Exports resampled from >48 kHz do not (ADR-012).
- `MusicValue` gains `EncodedFrom`, and the ingest protocol gains a pending state. Per ADR-004, gateway, library and console redeploy together.
- **Cost** (to be measured on a Pi): about 10–15 s per 6-minute 24-bit track, in the background.
- **Future:** export may hand back the original AIFF for blobs with an `aiff` block, including Serato/Traktor data, instead of re-muxing with ffmpeg.

## Alternatives considered

- **ffmpeg AIFF→FLAC.** It drops GEOB, PRIV, POPM and the AIFF chunks, and the library does not ship ffmpeg.
- **A sidecar of the ID3 chunk.** A second file to keep consistent, and it misses MARK and INST.
- **Keep the AIFF in `archive/`.** No space saved.
- **Leave unverifiable AIFFs in the inbox.** Rejected by Joakim: a pity to leave them behind when AIFF playback is not hard.
- **PcmHash-only acceptance.** It proves the audio, not the metadata.

## Phased plan

0. **Packaging:** `depends="flac>=1.4"` for `mdma-library`. Devops checks `flac --version` (1.4 or newer) on a Pi, and that `--keep-foreign-metadata` works with AIFF.
1. **Generated fixtures:** the in-memory `AiffBuilder` of the `audio-fixtures` component (`components/audio_fixtures`), no files and no regen script.
2. **Refactor:** split `generate_facts` into `tag_facts` / `file_facts` / `name_facts`, with no behaviour change.
3. **The verified encoder:** `encode_aiff_to_flac(src, staging) -> Result<Verified, Refusal>`. Its acceptance also compares the FLAC's picture count and types with the AIFF's APIC frames.
4. **AIFF blob support** (done 2026-11-20):
   - AudioHash v1 AIFF rules (ADR-010 amendment);
   - symphonia AIFF playback (the `aiff` feature was already on in `audio-decoder`; now tested);
   - export pass-through (`export_plan` in the console, tested);
   - ingest tolerates unreadable tags: `generate_facts_tolerant` gives file facts (stream properties from `COMM` via `audio_hash::stream_info`, any compression) and name facts plus a typed `IngestWarning`, shown as text in the `IngestResult::Success` message (no protocol change; a typed field comes with phase 6). `HasAlbumArt` is left out, as unknown. Only AIFF is tolerated (widening it to other formats would change their behaviour and is a separate decision); if `COMM` is unreadable too the ingest still fails;
   - export: `format=original` always passes through; `format=aiff` serves an AIFF blob directly only when it is plain AIFF (not AIFF-C) within the export limits (rate ≤ 48 kHz, depth ≤ 24, the transcoder's rule), otherwise it transcodes.

   **Known limits of symphonia 0.5.5** (found in phase 4; resolved for integer PCM by phase 4b, which no longer sends those files to symphonia; float and µ-law/A-law AIFF-C and AIFF with more than two channels still use symphonia):
   - AIFF-C `twos` and `sowt` play at 16 bits only (`NONE` at 8, 16, 24, 32), and plain AIFF only at 8, 16, 24, 32 bits (not 12 or 20), so an AIFF blob outside these returns a decode error at playback. Block-aligned `SSND` (non-zero `offset` or `blockSize`) is unsupported too.
   - It takes the `SSND` chunk length, which includes the 8-byte header, as the sample byte count. When another chunk follows `SSND` (typically `ID3 `), the first 8 bytes of that chunk are decoded as audio: 4 extra 16-bit samples (about 0.1 ms of noise) at the very end of the track, and the reported length is that much too long. The real samples are intact. The audio-decoder test pins this.

4b. **AIFF integer-PCM playback through `audio_hash`'s PCM layout** (fixes symphonia 0.5.5's 16-bit-only `twos`/`sowt` and its SSND-length tail). Our own decode path for integer AIFF and AIFF-C `NONE`/`twos`/`sowt`, using the layout PcmHash already reads. **Phase 8 is gated on 4b**: AIFF is not made ingestible until blobs play correctly.
   **Done (2026-11-20):** `audio_hash::aiff_pcm_layout` exposes the exact layout (byte order, container bytes, significant bits, channels, rate, frames, sample byte range) from the same parser PcmHash uses. `audio_decoder::AudioSource` chooses its backend in one place: integer-PCM AIFF/AIFF-C (1 or 2 channels) is decoded from that layout; everything else uses symphonia unchanged. Results: every depth from 8 to 32 bits plays bit-exact in plain AIFF and AIFF-C `NONE`/`twos`/`sowt`; the frame count and duration are exact (no tail after a trailing `ID3 ` chunk); seeks are frame-exact; a seek past the end gives end of stream. An AIFF whose `SSND` length disagrees with `COMM`'s frame count is refused at open (stricter than symphonia).
5. **`MusicValue::EncodedFrom`,** with a golden wire test.
6. **The library service:** the conversion worker, staging, crash order, `EncodedFrom` in the dedup set, PcmHash at ingest, and the pending state in the protocol.
7. **An export round-trip test.**
8. **Enable AIFF:**
   - `is_ingestible`;
   - `inbox_utils` derives from it, removing the duplicate;
   - integration tests.

   **Done (2026-11-21):** `AudioFormat::is_ingestible` is true for AIFF (`.aiff`/`.aif`, any case). `AudioFormat` now owns the extension list (`extensions`, `ingestible_extensions`), and `inbox_utils` derives everything from it (its `INGEST_EXTENSIONS`, `AUDIO_EXTENSIONS` and `NON_INGESTIBLE_ERROR` are gone; the policy lives in a small dependency-free `audio_format` component that `library_ingestion` re-exports and `inbox-utils` depends on, so clients don't pull in the ingestion pipeline). Upload type detection is typed (`inbox_utils::Upload { Zip, Audio(AudioFormat) }`) through the one container sniffer, `audio_format::sniff`, which `audio_hash` also uses; this adds M4A uploads. The console serves the upload `accept` list in `GET /api/inbox` (`accept`), so `addmusic.js` no longer lists extensions. The phase-6 bridge (`ConversionConfig::accepting_aiff`, `InboxFile::validate_accepting`, the AIFF filter in startup recovery) is removed: there is one validation path. Tests: an AIFF ingested through the default config ends as a FLAC blob with `EncodedFrom`, `PcmHash` and the FLAC's format fact; ZIP extraction takes AIFF.
9. **On a Pi:** time real AIFFs, both Bandcamp and Beatport, and record the results here.
