# ADR-012: PcmHash identity tier — exact hash of decoded lossless samples, container-independent, versioned

## Status

Accepted — 2026-11-03

## Context

AudioHash v1 (ADR-010) hashes encoded bytes, so any re-encode changes it: FLAC compression level, WAV↔FLAC, AIFF→FLAC. ADR-010 lists these as known false negatives. MDMA will re-encode AIFF (and similar formats once ingestible) to FLAC at ingest to save space and must keep identity across that conversion. Lossless decoding is bit-exact by specification, so a hash over decoded samples does not depend on which decoder or decoder version produced them. Lossy decoders are only tolerance-accurate and are out of scope. This is tier 1b of issue #55.

## Decision

- New fact `MusicValue::PcmHash(PcmHash)`, alongside AudioHash; ContentHash remains the entity key (ADR-003, ADR-004).
- Format: `pcm-sha256-v1:<64 lowercase hex>`; validated newtype in music_primitives. The version is part of the value.
- Scope: integer PCM in FLAC, WAV (`WAVE_FORMAT_PCM`; `WAVE_FORMAT_EXTENSIBLE` with PCM subformat), AIFF, and AIFF-C with compression `NONE`, `twos` or `sowt`; 1 or 2 channels. Everything else → no fact (MP3, AAC, float PCM, compressed AIFF-C, more than 2 channels, RF64, Ogg FLAC, ALAC for now).
- v1 = SHA-256( "mdma-pcmhash\0v1\0" || 0x01 || u8(channels) || u32_le(sample_rate) || Σ i32_le(sample) )
  - 0x01 = signed integer PCM; other values are reserved.
  - Samples are interleaved frame by frame, channels in file order. Every decoded sample is hashed; leading/trailing silence and padding are never trimmed.
  - Each sample is its signed value left-justified to 32 bits: `s << (32 − stored_bits)`. stored_bits is FLAC STREAMINFO bits-per-sample; for WAV, block align ÷ channels × 8; for AIFF, ceil(sampleSize ÷ 8) × 8. 8-bit WAV is unsigned: subtract 128 first. AIFF and AIFF-C `NONE`/`twos` are big-endian, `sowt` and WAV little-endian.
  - Bits per sample is not hashed: a signal stored at 16 bits and the same signal zero-padded to 24 bits hash equal. WAV `wValidBitsPerSample` and `dwChannelMask` are ignored.
  - Sample rate: FLAC STREAMINFO, WAV `nSamplesPerSec`, AIFF 80-bit extended rate, which must be an exact integer.
- Container walking reuses the AudioHash v1 rules (ADR-010): FLAC leading ID3v2 chain, metadata walk and trailer stripping; WAV first `fmt ` and first `data`. The FLAC decoder is given only `fLaC`, the file's STREAMINFO (marked last) and the audio frame range; it never sees other metadata or tags.
- FLAC is decoded with claxon. Every frame must have the STREAMINFO channel count and every sample must fit STREAMINFO bits. If the STREAMINFO MD5 is non-zero, the MD5 of the decoded samples in FLAC's MD5 form (interleaved, signed, little-endian, ceil(bits ÷ 8) bytes, right-justified) must match it. If STREAMINFO total samples is non-zero, it must equal the decoded count.
- WAV `data` must hold a whole number of frames; AIFF `SSND` sample bytes after `offset` must equal numSampleFrames × frame size. Zero frames → no fact.
- Failure (unsupported, malformed, decode error, MD5 or count mismatch, decoder panic) → no fact; ingest continues. Backfill is explicit, never at service startup.
- Computed in the audio_hash component (`pcm` module), which gains claxon and md-5. The AudioHash v1 code path stays decoder-free; ADR-010's "std + sha2 only" applies to v1.

## Why

- Decoded lossless samples are uniquely defined by the bitstream, so the definition can be frozen independently of any decoder. Swapping the decoder does not change the definition.
- Left-justification compares samples as fractions of full scale: 16-bit s and 24-bit 256·s are the same signal exactly, while different signals stay different. MDMA's own AIFF/WAV export writes 24-bit (audio_transcoder), so with bits in the preimage an exported copy of a 16-bit FLAC would never match its source. Bit depth is already a separate fact.
- The container tag is deliberately absent: cross-container equality is the point.
- Claxon frames strictly, rejects CRC failures and never resyncs; feeding it only STREAMINFO and frames makes the hash independent of tags. The STREAMINFO MD5, computed from the same decoded values, catches decoder bugs.
- Golden-hash tests plus the version prefix make any change to the preimage deliberate.

## Alternatives considered

- **Symphonia FLAC** — already in the workspace for playback and its sample math is exact, but its demuxer silently resyncs over damaged data and drops the final frame when trailing bytes follow (ADR-010); some arithmetic panics on malformed input in debug builds; and it is not in the library binary. Kept as a test oracle.
- **Own FLAC decoder** — about 600 lines; deferred unless claxon's gaps (escape-coded residuals, 32-bit) bite.
- **Right-justified, bits in the header (the STREAMINFO MD5 packing)** — rejected: zero-padded copies would not match; see Why.
- **STREAMINFO MD5 as the identity** — rejected: often unset, FLAC-only, width-dependent. Used as a check instead.
- **Float PCM with raw IEEE bits** — deferred; format byte reserved. No conversion target, since FLAC cannot carry float.
- **Channel mask in the preimage** — rejected: inconsistently written and does not change samples; scope is mono/stereo.

## Consequences

- The same audio as FLAC at any level or block size, WAV, AIFF and AIFF-C gets one PcmHash, while their AudioHash values differ.
- A future AIFF→FLAC conversion must compute PcmHash of source and output and keep the source on mismatch; this is the conversion's acceptance check.
- Adding a container or replacing the decoder needs no new version as long as the golden and cross-format tests pass. Changing the preimage requires v2, coexisting v1/v2 facts and a re-backfill.
- Known false negatives (no fact is written):
  - lossy files;
  - resampled, dithered or gain-changed copies;
  - more than 2 channels;
  - float PCM;
  - FLACs claxon cannot decode:
    - escape-coded residual partitions;
    - frame headers that take bits-per-sample from STREAMINFO (code 000), which libFLAC writes for depths outside {8, 12, 16, 20, 24} — so 4–7, 17–19, 21–23 and 32-bit FLAC;
    - STREAMINFO min block size below 16 (some variable-blocksize encoders);
  - FLAC with a wrong STREAMINFO total-sample count;
  - WAV/AIFF with trailing partial frames.
- Requires a full decode: order of tens of minutes for about 30 GB of FLAC on a Pi 5 (to be measured). Backfill runs at low priority beside playback.
- Tests use checked-in reference fixtures made with libFLAC and sox (with a regeneration script), unlike ADR-010's synthetic-only fixtures.
- MusicValue gains a variant: per ADR-004, gateway, library and console must be redeployed together.
- ALAC: future work under this same definition; needs an MP4 demuxer and ALAC decoder (likely symphonia) and has no MD5, so it relies on cross-format tests.

## Related

- ADR-010 — AudioHash v1; container walking reused; "std + sha2 only" narrowed to v1
- ADR-003 — content hash as entity key
- ADR-004 — FactAggregator; redeploy-together rule for `MusicValue` changes
- Issue #55 (tiered track identity, tier 1b)
