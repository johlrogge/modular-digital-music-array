# ADR-013: Duplicate merge rules — latest wins per attribute, newest blob kept, auto-merge only within one purchase

## Status

Accepted — 2026-11-03

## Context

ADR-010 (AudioHash v1) and ADR-012 (PcmHash v1) define tag-independent identity facts. They are computed at ingest and by an explicit backfill, never at startup (ADR-010 l.28, ADR-012 l.25). Neither ADR says how duplicates are merged. ADR-010 l.45 requires that identical audio on different releases "must preserve release context or be user-confirmed".

ADR-004 defines hard replace:
- `Replaces(old)` and forward-inherited ancestors on the new track;
- retract-all on the old track;
- eager playlist rewrite, plus lazy `PlaylistGet` repair.

Today hard replace is only reachable for a newly ingested file, through `library track replace` (`handle_track_replace`, components/library_service/src/service.rs:2071).

Observed on stage2, Bandcamp item `p387309426` (Tagliabue, "Espansione"):
- It was first a one-track prerelease, album "Espansione - Part III & IV" (June).
- The full release later appeared as "Espansione - Part 3 & 4" (September) under the same ItemId, and was downloaded again.
- The audio frames and STREAMINFO MD5 are byte-identical. Only VORBIS_COMMENT differs.
- The result is two live tracks and an ambiguous playlist-import match.
- 6 of 242 items show this two-date pattern.

The Bandcamp source never writes `Replaces`. Two paths re-download without linking:
- A forced resync logs a failed `retract_source_facts` and continues (projects/mdma-bandcamp/src/service.rs:643-656).
- A sync with the library unreachable falls back to cache-only dedup and returns the whole collection (service.rs:251-284).

## Decision

**Detection.**
- Two live tracks are duplicates if they have equal AudioHash v1 or equal PcmHash v1.
- Hashes are compared only within the same version.
- A group is a connected component of that relation.

**Automatic vs confirmed.**
- A group is merged automatically only if every member shares the same source ItemId (same purchase) and no member is soft-deleted (ADR-003).
- Every other group (different ItemId, no ItemId, or a soft-deleted member) is listed for user confirmation and never merged automatically (ADR-010 l.45).
- A user-confirmed merge uses the same operation as an automatic one.

**Winner.**
- The newest copy wins: the one with the latest AddedAt. Ties go to the lowest ContentHash, so the choice is deterministic.
- All other members are losers.
- The winner's blob is kept, so the file's embedded metadata is the most recent.

**Fact rules: latest wins, per attribute.**
- "Latest" means the ACID fact timestamp of the live assertion.
- The merged track carries exactly what a single entity would show if all members' facts had been asserted on it.
- **Scalar attributes** (Title, Album, Bpm, Key, Energy, …):
  - The value with the latest timestamp across the group wins.
  - If the winner already holds it, nothing is written. Otherwise it is asserted on the winner with its original `FactSource`.
  - Values that lose on an attribute are not carried over.
- **Origin** has no precedence: user and source facts compete on timestamp only. Copied facts keep their original `FactSource`, so a later `retract_source_facts(bandcamp)` still removes only Bandcamp-origin facts.
- **Multi-valued attributes** (StyleDescriptor, cue points, Bookmarked per scope): the union of the members' values.
- **File-derived attributes** are always the winner's and are never copied: FilePath, Format, DurationSeconds, Bitrate, FileSizeBytes, HasAlbumArt, EncoderSoftware, EncodedBy, CoverArtPath, AudioHash, PcmHash.
- **AddedAt: latest wins, generally.** This applies to MDMA's projection of AddedAt for every track, not only in merges. There is no "first seen" and no list of values. The projection rule changes from keep-first (service.rs:357) to keep-latest.
- **Replaces:** the winner asserts `Replaces(loser)` plus every `Replaces(X)` the loser carried (ADR-004 forward inheritance), deduplicated and never pointing at itself.
- **Play history** (TrackStarted/TrackStopped) is not copied (re-asserting would restamp the events). Instead the projection follows `Replaces`: a track's play history includes the plays recorded on every hash it replaces (transitively), with their original timestamps. This applies to dedup merges and to ADR-004 `track replace` alike.

**Mechanism.** A generalised ADR-004 replace, merge(winner, loser), for two existing tracks:
1. Read both entities from ACID. Gather the loser's ancestors before retracting.
2. Assert the carried facts and `Replaces` on the winner, with source tool `mdma-dedup`.
3. `retract_all_entity_facts(loser)`.
4. Eagerly rewrite playlists from loser to winner. Lazy `PlaylistGet` repair is unchanged.
5. Remove the loser's blob from `/music/blobs`, and its extracted cover art if no live track references it. Only do this once steps 2–3 are durable in ACID.

For groups of more than two, merge each loser into the winner in turn.

Blob removal is product behaviour. It is unrelated to operator rules about `/music`.

**Crash safety.**
- Facts are written first and files deleted after.
- A merge is idempotent: carried values that are already present are not re-asserted, and `Replaces` is deduplicated.
- A crash before step 3 leaves the group detectable, and a rerun completes it.
- A crash after step 3 leaves a merge orphan: a blob with no live facts whose hash appears in a live `Replaces` fact from `mdma-dedup`.
- An idempotent cleanup deletes merge orphans only, re-checking liveness immediately before each deletion. It runs with `dedup --apply` and after an ingest merge.
- Other `NoLiveFacts` orphans, including ADR-004 replace leftovers, are not touched.
- **Retire gate.** Every path that retires an entity (`retract_all_entity_facts`: ADR-004 replace, the merge in step 3) and every out-of-band writer that asserts facts on existing entities (e.g. the identity backfill) serialises on one `retire_gate` in the library service. A writer re-checks liveness under the gate right before writing, so it cannot resurrect an entity retired concurrently. Lock order: `retire_gate` before the projection cursor.

**Where it runs.**
- **Explicit `dedup` scan** (CLI/IPC). It lists automatic and confirmation groups. `--apply` merges the automatic groups only.
- **Self-healing at ingest.** Once AudioHash v1 is computed for a new track, it is merged with any live track that shares its source ItemId and AudioHash. The new copy wins. PcmHash-only matches are left for the scan.

**Hardening of the Bandcamp source.**
- `queue_item_forcibly`/resync must not ignore a failed `retract_source_facts` or an unreachable library. It returns an error and does not queue.
- Normal sync must not treat the collection as new when the library is unreachable or the `has_facts` query fails. It defers instead of falling back to cache-only dedup.

## Why

- Latest-per-attribute matches the existing single-entity projection, where the last assertion wins. A merged track therefore behaves as if it had always been one entity: the newest tags win, and user work that exists only on an older copy survives.
- Keeping the newest blob keeps the file on disk consistent with the newest facts, which matters for export and Rekordbox.
- Same ItemId plus exact identity is the narrowest condition under which the copies are certainly the same product. Different releases carry release context that must not be silently discarded (ADR-010).
- Reusing ADR-004 keeps one replace model, one playlist repair path and one ancestry rule.

## Alternatives considered

- **User origin always outranks source origin.** Rejected: it departs from single-entity semantics, and a stale user value could never be corrected by newer tags.
- **Keep the oldest blob and rewrite its tags.** Rejected: rewriting the file changes its ContentHash, which is the entity key.
- **Auto-merge across ItemIds.** Rejected (ADR-010 l.45). Such groups are listed for confirmation instead.
- **Earliest AddedAt, or a list of AddedAt values.** Rejected: re-acquired or re-released material should surface as recently added.
- **Delete all `NoLiveFacts` orphans.** Rejected: the scope stays limited to merge losers. Garbage-collecting ADR-004 leftovers is a separate decision.

## Consequences

- A track that is re-ingested or merged shows its newest AddedAt, so it appears in "recently added" again. This is intended.
- The AddedAt projection change lives in MDMA (library_service on develop, library_projection on the offline branch). stainless-facts may later provide a generic LatestWins fold strategy for it (johlrogge/stainless_facts issue).
- The protocol additions (dedup scan/apply) change `library_ipc_protocol`. Per ADR-004, gateway, library and console are redeployed together.
- A backfill is required before the scan is meaningful. Costs on a Pi 5: AudioHash about 4 s/GB, PcmHash about 36 s/GB. Both stay explicit and low priority.
- Blob deletion is irreversible and depends on the merge being correct. That is why automatic merges require exact identity plus the same ItemId.
- ADR-012 was accepted on 2026-11-03, so PcmHash facts may be written (backfill `--pcm`); its preimage is now frozen and any change requires a new version.
- PcmHash groups within one ItemId can differ in container, so the kept blob may be WAV rather than FLAC. The samples are identical by definition.
- The projection resolves play events through the `Replaces` chain, so last-played / play counts on a merged or replaced track include its predecessors' plays. Events keyed by short hashes (#86) must resolve to the full hash before this lookup.

## Related

ADR-003 (entity key, soft delete) · ADR-004 (hard replace, `Replaces`, redeploy-together) · ADR-010 (AudioHash v1; release context) · ADR-011 (read-your-writes after each merge write) · ADR-012 (PcmHash v1) · issue #55 (tiered track identity)
