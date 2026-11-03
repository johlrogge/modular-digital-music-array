# ADR-010: AudioHash identity tier — container-aware byte-range hash, versioned, decoder-independent

## Status

Accepted — 2026-11-03

## Context

ContentHash (SHA-256 of the whole file) is the entity key (ADR-003, ADR-004). Retagging changes it, so the same audio can exist as multiple entities (e.g. one Bandcamp purchase downloaded twice under two album names). A future merge needs a tag-independent identity. ADR-003/004 deferred a work-identity model; this ADR adds the lowest tier only (issue #55, tier 1).

## Decision

- New fact `MusicValue::AudioHash(AudioHash)`; ContentHash remains the entity key.
- Format: `audio-sha256-v1:<64 lowercase hex>`; validated newtype in music_primitives. The version is part of the value; hashes of different versions never compare equal.
- v1 = SHA-256( "mdma-audiohash\0v1\0" || container_tag[4] || Σ(u64_le(len) || bytes) ) over container-defined ranges, in file order. container_tag ∈ {b"flac", b"mp3\0", b"wav\0", b"mp4\0"}.
  - **Trailer stripping** (FLAC and MP3): trailing tags are stripped by repeating the following checks against the current end of the range, in this order, until none matches; the range end never moves below its start. A match on the signature commits to that tag type; any subsequent inconsistency is an error (no fact). Strongest signatures are checked first so that a weak signature (`TAG`) inside another tag cannot misfire.
    1. APEv2/APEv1 footer: `APETAGEX` at end−32 with version (u32 LE at footer offset 8) 1000 or 2000. Size = u32 LE at footer offset 12, must be ≥ 32; total = size, plus 32 if flags (u32 LE at footer offset 20) bit 31 is set, in which case `APETAGEX` must also appear at end−total. Strip total.
    2. Lyrics3v2: `LYRICS200` at end−9 preceded by 6 ASCII decimal digits (size); total = size + 15, and `LYRICSBEGIN` must appear at end−total. Strip total.
    3. Appended ID3v2.4: `3DI` at end−10 with major version 4, revision ≠ 0xFF, footer flag 0x10 set, syncsafe size; total = size + 20, and `ID3` with major version 4 must appear at end−total. Strip total.
    4. ID3v1: `TAG` at end−128; strip 128. Then, if `TAG+` is at end−227, strip 227 more.
  - **FLAC:** skip a leading ID3v2 chain, require `fLaC`, walk metadata blocks until the last-block flag; the range is from the first byte after the last block to the stripped end. STREAMINFO and all other metadata blocks are excluded.
  - **MP3:** skip a leading ID3v2 chain and then up to 64 KiB of `0x00` padding (acceptance only; skipped bytes are never hashed); the range is from the first frame sync to the stripped end. The Xing/LAME/VBRI frame is included.
  - **WAV:** walk RIFF chunks; the payload of the *first* `fmt ` chunk, then the payload of the *first* `data` chunk; later duplicates and all other chunks are ignored. The odd-size pad byte is skipped, not hashed. A `data` size of `0xFFFFFFFF` means the payload runs to EOF; a size of 0 means an empty payload.
  - **M4A/MP4:** walk top-level boxes; every `mdat` payload in file order. The payload excludes the 8-byte header (16 bytes when size==1, 64-bit largesize); size==0 means the box runs to EOF. Empty `mdat` boxes still contribute `u64_le(0)`.
- **Empty audio** → error, no fact: FLAC/MP3 range empty after stripping; WAV `data` payload empty; MP4 no `mdat`, or all `mdat` payloads empty.
- Computed by a new `audio_hash` component with std + sha2 only. Never via a decoder or demuxer.
- Failure (unsupported, malformed, empty audio) → no fact; ingest continues.
- Backfill is explicit (CLI/IPC request), never synchronous at service startup.

## Why

- A byte-range definition can be specified, reproduced by any tool, and frozen. A Symphonia-packet definition is "whatever the demuxer emits": in 0.5.5 the MP3 reader syncs non-strictly after the first frame and can emit false packets from trailing tags, and the FLAC parser drops the final frame when trailing bytes follow it. Both would make the hash depend on exactly the tags it must ignore.
- Decoded PCM is rejected: lossy decoder output can change between versions.
- Golden-hash tests plus the version prefix make any change to the algorithm deliberate.

## Alternatives considered

- **Symphonia demuxed packets (all formats)** — rejected, see Why.
- **Hybrid with Symphonia sample tables for M4A** — deferred as a v2 candidate if mdat hashing gives false negatives.
- **Decoded-PCM hash / acoustic fingerprint** — rejected for this tier (unstable / probabilistic); a possible higher tier.
- **FLAC STREAMINFO MD5** — deferred; lossless-only, may be unset.

## Consequences

- Identical encoded audio on different releases (album vs compilation) gets the same AudioHash. The merge must preserve release context or be user-confirmed.
- Known false negatives: re-encodes, transcodes, rebuilt MP3 VBR headers, Lyrics3v1, WAV fmt-variant rewrites, RF64.
- The golden-hash tests in the audio_hash component are generated from, and checked against, a hand-built preimage of this definition; changing either requires v2.
- Changing the algorithm requires v2, coexisting v1/v2 facts, and a re-backfill.
- MusicValue gains a variant: per ADR-004, every service embedding `library_ipc_protocol` (gateway, library, console) must be redeployed together.

## Related

- ADR-003 — content hash as entity key; work-identity model deferred
- ADR-004 — FactAggregator projection; redeploy-together rule for `MusicValue` changes
- Issue #55 (tiered track identity, tier 1)
