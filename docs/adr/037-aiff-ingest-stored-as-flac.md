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
  - AudioHash v1 does not support AIFF (`audio_hash/src/lib.rs`).

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
   4. The tag facts read from the FLAC equal those read from the AIFF. Only tag facts are compared (not file or name facts), and the FLAC's PICTURE blocks match the AIFF's APIC frames in count and types.

   If any check fails or can't run, the conversion is refused and decision 1's fallback applies: the AIFF is stored as AIFF, and the reason is logged and shown in the ingest result. With today's PcmHash that covers:
   - float and compressed AIFF-C;
   - more than 2 channels;
   - 32-bit, and depths claxon can't decode;
   - non-integer sample rates;
   - any rebuild mismatch.
6. **Identity.**
   - The blob's ContentHash is the entity key.
   - A new fact `EncodedFrom(ContentHash)` records the source AIFF's hash. It is multi-valued and united on merge (ADR-013), and it is part of the ingest dedup set. Re-ingesting the same AIFF is then a duplicate, with no decode.
   - PcmHash is written at ingest for converted tracks.
7. **Where it runs.**
   - Conversion runs on a background worker at nice 19 / idle IO, never on the IPC thread.
   - It stages in `/music/staging/<source-hash>.flac`.
   - The order is: blob rename, then facts, then removal of the source.
   - The ingest protocol gains a pending state.
8. **The source AIFF** is deleted after a successful, verified conversion. Check 5.3 proves the FLAC rebuilds it byte for byte.
9. **Same audio, different bytes.** A retagged AIFF, or an MDMA-exported AIFF re-imported, becomes a new track with an equal PcmHash. It is handled by the ADR-013 dedup scan, which offers the merge for confirmation.

## Why

- Lossless decoding is bit-exact, and check 5.3 proves the original file can be recovered byte for byte: the audio and every chunk. Deleting the source loses nothing, and no sidecar or archive copy has to be kept consistent.
- One acceptance rule replaces a per-variant table. Widening PcmHash later widens what converts.
- Facts from the source mean the conversion can't change what the library knows.
- Falling back to an AIFF blob means no user file is left behind.

## Testing

- **No fixture files in the repo.** AIFF files are built in memory at test time by the `audio-fixtures` component (`components/audio_fixtures`, dev-dependency only). `AiffBuilder` makes synthetic tones or noise at 8 to 32 bits, mono or stereo, AIFF or AIFF-C (`NONE`, `twos`, `sowt`). Depths that are not a multiple of 8 are left-justified in their byte container. The builder adds extra chunks and an `ID3 ` chunk (v2.3 and v2.4). The ID3 tag carries TIT2/TPE1/TALB/TCON/TDRC (v2.4) or TYER (v2.3)/TRCK/TPOS/TBPM/TKEY/COMM/TXXX (two)/USLT/POPM, two APICs (front and back), a GEOB "Serato Markers_" and a PRIV "TRAKTOR4". Extra chunks are MARK/INST/COMT/ANNO, including an odd-length chunk with its pad byte. Refusal cases are fl32, ulaw, 3 channels and 32-bit. The builder's tests parse its output with lofty, and `audio_hash` tests read it back with PcmHash.
- **No real, copyrighted audio in the repo.** An ignored test runs the full conversion and verification against local real files given by an environment variable, `MDMA_REAL_AIFF_DIR`:
  - a Bandcamp release downloaded as `aiff-lossless`, which covers the common case;
  - a Beatport download, which covers most of the rest.

## Consequences

- FLAC blobs carry the original chunks. Cover art is stored twice: inside the ID3 chunk and as a PICTURE block.
- AIFF blobs exist for unverifiable files. Playback, AudioHash, waveforms and export must handle them.
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
4. **AIFF blob support:**
   - AudioHash v1 AIFF rules;
   - symphonia AIFF playback;
   - export pass-through.
5. **`MusicValue::EncodedFrom`,** with a golden wire test.
6. **The library service:** the conversion worker, staging, crash order, `EncodedFrom` in the dedup set, PcmHash at ingest, and the pending state in the protocol.
7. **An export round-trip test.**
8. **Enable AIFF:**
   - `is_ingestible`;
   - `inbox_utils` derives from it, removing the duplicate;
   - integration tests.
9. **On a Pi:** time real AIFFs, both Bandcamp and Beatport, and record the results here.
