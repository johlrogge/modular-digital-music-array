# ADR-020: Fact sources are client-owned; the library is source-agnostic

## Status

Accepted, 2026-11-11

Amends ADR-018 (the origin kind becomes opaque and client-owned; one source per ingest is replaced by a split). Related: ADR-004, ADR-013, ADR-017, ADR-019 (§7 USB import).

## Context

ADR-018 made a retract match on source identity: the tool plus the origin kind. The library still knows specific sources:
- `FactOrigin` is a closed enum in `music_facts` (Beatport, Bandcamp, FilesystemScan, Unknown, User), with `FactOrigin::infer` sniffing paths and tag comments for "beatport" and "bandcamp". On stage2 it never fired.
- `IngestSource` names Bandcamp and Beatport in the protocol, and `source_facts` writes the literal `Source("bandcamp")` and `ItemId` facts.
- The Bandcamp resync attribute list (`is_retractable_bandcamp_attribute`) and a string-to-origin mapping (`origin_matches_source_name`) live in `library_service`.
- The library stamps its own tool and version on facts a client supplied, so it claims to have written data it did not.

Every new source (USB import, Rekordbox) would need a library change. ADR-019 §7 already needs USB imports to carry their own source.

mdma-playback already stamps its own source (`mdma-playback` / `Unknown`) and writes straight to ACID. That is the model to generalise.

## Decision

The origin kind is an opaque string owned by the writing client. The library never matches it against literals. It only compares `{tool, origin}`.

### 1. Origin is information, not security

Clients are trusted. The library validates that tool and origin kind are non-empty, nothing more. Any client can name any source.

### 2. Clients stamp their own tool and version

Only a client knows its own version, so only the client can say what wrote the data. Clients send their whole `FactSource` (tool, version, origin). The library writes it as given.

| Writer | tool / origin |
|---|---|
| mdma-bandcamp | `mdma-bandcamp` / `Bandcamp` (ingest and ItemId backfill share the constants) |
| User-facing UIs (CLI, TUI, console) | `mdma` / `User` (one shared user identity, so a fact written in the TUI can be retracted from the CLI) |
| `mdma transfer` uploads (mdma-cli) | `mdma` / `Upload` |
| Rekordbox import (mdma-cli) | `mdma` / `Rekordbox` |
| Playback | `mdma-playback` / `Unknown` |
| Library scan facts (FilePath, Format, AddedAt, stream properties) | `mdma-library` / `FilesystemScan` |
| Library-derived facts (CoverArtPath, AudioHash, PcmHash) | `mdma-library` / `Unknown` |
| Dedup | `mdma-dedup` / `Unknown` |

- Rekordbox gets its own origin so that a re-import can retract its own earlier values without touching manual edits. Until now they were indistinguishable from the user's.
- Uploads get their own origin for the same reason: their tags stay distinguishable from manual edits.
- The library's own identities become private constants in `library_service`. Only the user default goes into `library_ipc_protocol`, as protocol semantics: the source of an unattributed write. Origins owned by the CLI (Rekordbox, Upload) live in mdma-cli, not in the protocol crate.
- The client libraries have no defaulting write or retract methods. Every `WriteFact` and `RetractFact` states a `FactSource`, carrying the version of the binary that writes it. Mark and lifecycle requests (`WriteMark`, `UnmarkTrack`, `TrackDelete`, `TrackRestore`, `TrackReplace`) carry no source yet and are stamped with the library's user default. The absent-source default in the library remains only for old clients, until the cleanup phase.

### 3. Ingest split (reverses ADR-018's one source per ingest)

- Everything the library measures or computes carries the library's source: FilePath, Format, AddedAt, CoverArtPath, AudioHash, PcmHash, and the stream properties (BitDepth, Channels, SampleRate, Duration, Bitrate, FileSize, HasAlbumArt). Scan facts use `mdma-library` / `FilesystemScan`, derived facts (cover art, hashes) `mdma-library` / `Unknown`.
- Only tag-derived values and client-supplied facts (ItemId, Source) carry the requester's writer source.
- Provenance facts are stated by the client, including the CLI upload: `mdma transfer` sends writer `mdma` / `Upload` and facts `[Source("upload")]`. The library writes no Source fact of its own.
- An ingest that supplies `facts` without a `writer` is rejected: the library never states a client's facts under its own identity.

The reason is accurate provenance: the library does not claim to have written what the client said. It is not a safety boundary. Any client can name any source, so this does not stop a client retracting structural facts. What scopes a resync retract is its attribute list, which now lives in the client (mdma-bandcamp).

With no writer and no facts in the request (scan, legacy upload), behaviour is as before, minus `infer`. A legacy upload gets no Source fact.

### 4. Wire shape unchanged

`{"tool":"mdma-bandcamp","version":"0.4.0","origin":{"type":"Bandcamp"}}`. The kind strings used today (`FilesystemScan`, `Unknown`, `User`, `Bandcamp`) are the new opaque strings, so every existing identity is preserved byte for byte.

- The reader has no `data` field, and serde ignores unknown fields. It reads every line in today's log, and the writer never emits `data`.
- This is generic and needs no source knowledge, so it is consistent with ADR-018's rejection of source-specific tolerant deserialization.
- No mapping, no re-attribution of existing lines. A resync of an item whose lines predate this change retracts nothing from them, as ADR-018 already accepts.
- ADR-017's strip-`data` migration becomes log hygiene, not a correctness requirement.

### 5. Protocol changes

- `WriteFact` and `RetractFact` get an optional `source: Option<FactSource>`. Absent means `mdma` / `User`, so old CLIs keep working. `RetractFact` was hard-wired to that.
- `IngestFile` gains `writer: Option<FactSource>` and `facts: Vec<MusicValue>`. The client supplies its provenance facts (e.g. `Source("bandcamp")`, `ItemId`), written under `writer`. The duplicate path re-reads tags when a writer is present.
- `IngestSource` shrinks to `Upload`.
- `RetractSourceFacts { item_id, source, attributes }` replaces `{ item_id, source_name }`. The library retracts each live fact on the ItemId's entities whose source identity equals `source`'s and whose attribute is in the list. Retract lines are stamped with the request's source.
- `attributes` is an `AttributeList` type, validated when the request is parsed: non-empty, every name a known attribute (typo protection). The service does no runtime check.
- The resync list (Album, Title, Artist, TrackNumber, Year) moves into mdma-bandcamp.

### 6. Retracting imported values

Rekordbox imports are `mdma` / `Rekordbox` and uploads are `mdma` / `Upload`, so a user retract of them must name that origin: `mdma fact retract --origin Rekordbox` or `mdma fact retract --origin Upload`. Every client retract states its source, so this needs no special method. A retract that matches nothing is reported as such, not as success.

### 7. Unparseable fact-log lines are counted

Lines that fail to parse are counted rather than dropped silently. The count is exposed in library status. Each load logs one summary warning; per-line detail is logged at debug level only. This is pulled forward from ADR-017, because a library older than this change reading new lines would otherwise lose them without a trace.

### 8. USB import

ADR-019 §7 states its own source this way: mdma-usb sends its tool and origin kind in the ingest request's `writer`, and the library gains no `IngestSource::Usb`.

## Why

- A new source costs no library change.
- Provenance says who wrote a fact, including the version.
- The wire shape and every existing identity stay as they are, so this ships ahead of ADR-017's migration.

## Alternatives considered

- **`DeclaredOrigin`: a library-side whitelist of origins clients may claim.** Rejected. The library would again need a list of specific sources, and it would be an authorization boundary for clients that are trusted anyway.
- **A flat `"origin":"bandcamp"` wire shape.** Rejected: the work would have to wait for ADR-017's migration or carry two reader shapes.
- **Re-attributing `mdma-library` / `Bandcamp` lines to `mdma-bandcamp`.** Rejected. Those lines exist only where a build with the ADR-018 ingest ran, and stage2 has none.
- **A per-binary tool name for UIs.** Rejected: it would split "the user" into several sources.
- **The library keeps stamping the whole ingest.** Rejected: wrong provenance for tags and client facts.

## Consequences

- `FactOrigin`, `DeclaredOrigin`, `infer`, `UploadSource`, `source_facts`, `map_upload_source`, `is_retractable_bandcamp_attribute` and `origin_matches_source_name` are removed.
- Every writer must keep its tool and origin kind stable across versions (ADR-018).
- Rollback of the library below this change means restoring from backup.
- `ItemId` is documented as unique within a source, but the library compares it globally (retract lookups, dedup). That only matters once a second source writes ItemIds. Recorded here, not fixed.
- Later: remove `IngestSource` and the absent-source default once every client sends its source.

## Related

- ADR-004, ADR-013, ADR-017
- ADR-018 (amended)
- ADR-019 (USB sync, §7)
