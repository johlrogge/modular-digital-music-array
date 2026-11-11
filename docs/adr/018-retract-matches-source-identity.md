# ADR-018: A retract matches value and source identity, not the whole source

## Status

Accepted, 2026-11-11

Amends ADR-004 (retract matching: value + full `FactSource` becomes value + source identity). Builds on stainless_facts ADR-001 (johlrogge/stainless_facts#15). Related: ADR-013, ADR-017.

## Context

ADR-004 chose value + source matching on purpose, to fix #2: an identical assertion from another source must survive a retract. That guarantee stays.

The defect was that "source" meant the whole `FactSource`, compared with a derived `PartialEq` (commit 0490c847). That included the tool version and the origin payload, such as FilesystemScan's `scan_time`. The accident was not designed.

Consequences:
- A fact written by an older library version, or by an earlier scan, could not be retracted.
- `RetractFact` (`user_fact_source()` stamps the current crate version) and `TrackRestore` silently failed on old facts.

Joakim's rule: "A new version of a source is not a different source." A source keeps the same identity across versions.

stainless_facts issue #15 asked what identifies an assertion. Its ADR-001 answers: identity is (entity, value).

## Decision

A Retract cancels every live assertion on the entity with an equal value and the same **source identity**.

Source identity is the tool plus the origin kind (the `FactOrigin` variant). The `version` stays on every line as recorded provenance (what wrote it), but it is never part of identity or matching.

From ADR-017's migration release, writers stop writing the origin's `data` part (scan time, URLs, ids) and old lines are stripped. This release changes the matching, and the origin a Bandcamp ingest declares (with no payload). Stripping old lines is ADR-017's. Every origin then becomes `{"type":"<Kind>"}`, as `User` and `Unknown` already are. A new source looks like this:

`{"tool":"mdma-library","version":"0.4.0","origin":{"type":"FilesystemScan"}}`

ADR-017's migration strips `data` from the origin of every old line. It is a generic rule over all lines, with no source-specific logic: it keeps `tool`, `version` and `origin.type`. Deserialization does not handle the old shapes, because that would make the library depend on source-specific knowledge.

Until that migration release, matching compares tool and origin kind and ignores version and origin data. Retracts work across versions from the first deploy of this change.

The stainless model is (entity, value). MDMA's projection adds the source identity on top, for provenance. That is what keeps #2 and the per-source resync working.

Facts that repeat, such as plays, still carry their own identity in the value. For plays that is `at` (ADR-017), as stainless_facts ADR-001 recommends.

A Bandcamp ingest writes all its facts with origin Bandcamp, declared by the ingest request rather than guessed from the path, and with one tool and version for the whole ingest.
- A resync retracts Bandcamp's Album, Title, Artist, TrackNumber and Year and re-downloads. If the download is byte-identical (the same hash), the library re-extracts the metadata and re-asserts whatever Bandcamp no longer has live, so once the re-download is ingested, the track has its metadata again. Between the retract and that ingest it has none (and if the download fails, until a retry succeeds).
- Existing lines are not relabelled. For a track ingested before this change, the resync retracts nothing, and the re-download adds Bandcamp-origin facts alongside the old ones. Nothing visible changes: style and memory-cue projections treat equal values from different sources as one, so a re-download cannot duplicate styles.
- Exception: the Bandcamp service's `ItemId` backfill goes through `WriteFact`, which stamps the user source (`mdma`/User). Fixing that needs a protocol change and is tracked separately. `ItemId` is never retracted by a resync, so this does not affect correctness.

## Evidence

Stage2's fact log was replayed under exact-source matching and under source identity. Every retract that matched under exact-source matching also matches under source identity: 146 matched, and the end state is identical.

The 21 unmatched play retracts miss under both rules. Their values were re-serialised lossily, which has nothing to do with the source. ADR-017's migration handles them.

Source identities in stage2's log:

| Tool / origin kind | Lines |
|---|---|
| `mdma-library` / FilesystemScan | 15,766 |
| `mdma-library` / Unknown | 3,762 |
| `mdma` / User | 1,959 |
| `mdma-playback` / Unknown | 1,466 |
| `mdma-dedup` / Unknown | 6 |

Nothing is lost by dropping the origin data (stage2 and architect review):
- Stage2 has one origin payload: FilesystemScan `{"scan_time": "...", "inferred_source": null}` on 15,766 lines. Nothing reads `scan_time`; the AddedAt fact and the line timestamp cover it. `inferred_source` is always null.
- Bandcamp `artist_url` duplicates the `BandcampUrl` fact, parsed from the same comment.
- Beatport `track_url`, `label_url` and `track_id` are always null. The real values are the `BeatportTrackUrl`, `BeatportLabelUrl` and `BeatportTrackId` facts.
- No client, protocol, CLI, TUI or export reads `FactSource`. ACID treats it as opaque JSON.
- The other two kinds on stage2 have no payload: Unknown on 5,234 lines, User on 1,959.

## Consequences

- The matching rule in the interim release compares tool and origin kind only. Once the migration has run and no origin has data, the rule is simply equality of tool and kind, with no payload to ignore.
- Every component that writes facts must keep its `tool` name stable across versions.
- Callers write one retract per (value, source identity).
- A per-source operation, such as the Bandcamp resync, naturally removes only its own source's assertions. ADR-013:53 holds.
- Plays are unaffected: nothing in the code applies play retracts today (`refresh_event_timestamps` skips non-Assert lines, and plays are not provenance-tracked).

## Alternatives considered

- **Cancel by value, whatever the source.** Rejected: it breaks #2 and the per-source resync.
- **Treat values written after a cut-off time differently.** Rejected: overcomplicated.
- **Address individual assertions by line number in stainless_facts.** Declined upstream.

## Related

- ADR-004 (amended), ADR-013, ADR-017
- stainless_facts ADR-001: assertion identity is (entity, value) (johlrogge/stainless_facts#15)
