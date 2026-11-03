# ADR-010: AudioHash identity tier — container-aware byte-range hash, versioned, decoder-independent

## Status

Proposed — 2026-11-03

## Context

ContentHash (SHA-256 of the whole file) is the entity key (ADR-003, ADR-004). Retagging changes it, so the same audio can exist as multiple entities (e.g. one Bandcamp purchase downloaded twice under two album names). A future merge needs a tag-independent identity. ADR-003/004 deferred a work-identity model; this ADR adds the lowest tier only (issue #55, tier 1).

## Decision

- New fact `MusicValue::AudioHash(AudioHash)`; ContentHash remains the entity key.
- Format: `audio-sha256-v1:<64 lowercase hex>`; validated newtype in music_primitives. The version is part of the value; hashes of different versions never compare equal.
- v1 = SHA-256( "mdma-audiohash\0v1\0" || container_tag[4] || Σ(u64_le(len) || bytes) ) over container-defined ranges:
  - FLAC: bytes after the last metadata block to EOF (after skipping a leading ID3v2 and stripping trailing ID3v1/APEv2/Lyrics3v2/ID3v2-footer).
  - MP3: after the leading ID3v2 chain (and zero padding) to EOF minus the same trailers; the Xing/LAME frame is included.
  - WAV: `fmt ` payload, then `data` payload; all other chunks ignored.
  - M4A/MP4: all top-level `mdat` payloads in file order.
  container_tag ∈ {b"flac", b"mp3\0", b"wav\0", b"mp4\0"}. Zero total audio bytes → error, no fact.
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
- Changing the algorithm requires v2, coexisting v1/v2 facts, and a re-backfill.
- MusicValue gains a variant: per ADR-004, every service embedding `library_ipc_protocol` (gateway, library, console) must be redeployed together.

## Related

- ADR-003 — content hash as entity key; work-identity model deferred
- ADR-004 — FactAggregator projection; redeploy-together rule for `MusicValue` changes
- Issue #55 (tiered track identity, tier 1)
