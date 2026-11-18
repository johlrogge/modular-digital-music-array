# ADR-032: Three-way Bandcamp playlist sync

## Status

Proposed, 2026-11-18 (Joakim accepted every decision below)

Builds on ADR-027 (CAS playlist edits, `EditOp`, revert), ADR-026 (playlists hold canonical hashes; files are never canonicalised), ADR-020 (source-owned facts), ADR-016 (marks and the outbox). Related: ADR-013 (dedup rewrites playlists).

## Context

- `mdma-bandcamp` imports a Bandcamp playlist with an import job (`projects/mdma-bandcamp/src/service.rs`, `components/bandcamp_api/src/playlist.rs`, `components/playlist_import`). `match_source_playlist` matches each Bandcamp item to a library track.
- Today it **drops unmatched items** and **replaces the playlist whole**. A local edit is lost on the next import, and an item that could not be matched is lost for good, even when it would match after a download.
- It **stores no Bandcamp `track_id` to hash mapping**, so it cannot tell what Bandcamp changed since the last import.
- ADR-027 gives the library compare-and-swap edits, line preservation, atomic writes and undo by revert. A sync can be built on those and needs no new write path.
- Joakim wants to keep a playlist in step with Bandcamp while editing it locally.

## Decision

### 1. A one-way three-way merge

1. **Decided:** a sync merges three lists of the same items:
   - **base**: the Bandcamp list as last synced;
   - **theirs**: the Bandcamp list now;
   - **ours**: the local playlist.
2. Bandcamp's changes (base to theirs) are applied to ours. **Local changes win.**
3. MDMA never writes to Bandcamp.
4. Sync runs only from a console **Sync** button, never on a schedule.
5. The console shows a "Bandcamp" link badge on a linked playlist.

### 2. Rules

1. **Added on Bandcamp:** the item is added only if it is not in base, so a local deletion stays deleted. It goes after its nearest predecessor in theirs that is present in the result. An item that is first in theirs goes first; otherwise, with no such predecessor, it goes at the end.
2. **Removed on Bandcamp:** the item is removed if that occurrence is still present in ours.
3. **Moved on Bandcamp:** Bandcamp's moved tracks are the shared tracks outside the longest sequence Bandcamp kept in order. A track you reordered locally keeps your order; Bandcamp's reorderings apply to tracks you didn't touch. You touched a track if you moved it past a track that neither you nor Bandcamp moved. Moving a track past one Bandcamp has since removed doesn't count. Reordering only among tracks Bandcamp also moved is not detected, and Bandcamp's order wins there. A move is placed like an add. A move whose anchor is gone stays where it is in ours.

### 3. Identity

Items are identified by occurrence keys `(key, n)`: the n-th occurrence of `key` in a list. A local "Add again" copy gets a higher n, so it is local-only, and a Bandcamp removal takes away only the matching occurrence.

### 4. Pending items

1. Bandcamp items not matched to the library are **pending**. That covers Missing, Ambiguous, DurationMismatch, and removed or preorder tracks with null fields.
2. Pending items are never recorded as applied in base, so they are added once they resolve.
3. The console shows them greyed out with their title.
4. Sync first downloads owned tracks that are missing, as the import job does.

### 5. Storage

1. **Decided:** base, pending and the match cache live in `mdma-bandcamp`'s state file `playlist-links.json`, written atomically:
   `{local name, bandcamp id/url, base: [track_id], pending, matches, last_synced}`.
2. They are not facts: playlists are not fact entities, and base lives in Bandcamp-ID space.
3. A future per-track `BandcampTrackId` fact (source-owned, ADR-020) may replace the match cache. Revisit when playlists become facts.

### 6. Writing

1. A generic library-side component `sequence_merge` computes `merge3(base, theirs, ours) -> target`. It is generic over the key type and knows nothing about Bandcamp.
2. `playlist_edit::ops_between(current, target) -> Vec<EditOp>` turns the target into ADR-027 ops.
3. The ops are sent with `PlaylistEdit { version }`. That gives CAS, comment and short-ID preservation, an atomic write, and Undo via revert.
4. On Conflict: re-snapshot and retry, up to 3 times. Base is saved only on Applied.
   With duplicate items, a crash between the PlaylistEdit being applied and the new base being saved can add a duplicate copy on the next sync. Phase 3 must close that window, for example by recording the pending target or version in the link state before writing.
5. The same merge is meant for rebasing offline edits (the ADR-016 outbox).

### 7. First sync with no base

Add the missing items (`Duplicates::Skip`), then set base = theirs.

### 8. Rename and delete

Renaming the local playlist updates the link. Deleting it drops the link.

### 9. Undo after a sync

Undo reverts the file but keeps the new base. Undone additions therefore count as local deletions and stay deleted.

### 10. Merge properties (property-tested)

- theirs == base implies the result is ours;
- ours == base implies the result is theirs;
- ours == theirs implies the result is ours;
- local deletions never come back;
- a track removed on Bandcamp is absent;
- a track added on Bandcamp is present, unless you deleted it or already have it;
- tracks only in the local playlist keep their relative order;
- the merge is idempotent: `merge3(base, theirs, merge3(base, theirs, ours))` equals `merge3(base, theirs, ours)`. This holds for unique keys. With duplicates occurrences are renumbered from the result's order and it can fail;
- every result item is in ours union theirs.

## Amendment 2026-11-18: delete bins (ADR-033)

§8 changes for delete. A deleted playlist goes to the Bin, and its link stays but is inactive and cannot sync. Restore re-activates the link; restoring under a new name counts as a rename and updates the link. Purge drops the link. Rename is unchanged. See ADR-033.

## Phased plan

0. This ADR plus the ROADMAP entry.
1. `sequence_merge` with proptest for the §10 properties.
2. `ops_between`: applying the ops gives target; untouched lines stay byte-identical.
3. `mdma-bandcamp` requests `LinkPlaylist`, `SyncPlaylist`, `ListPlaylistLinks`.
4. Console routes in `api/playlists.rs`, plus `playlist.js`: badge, Sync button, link picker, and a toast with counts and Undo.
5. Stage2 hardware test.

## Consequences

- A local edit survives a sync, and a local deletion stays deleted.
- Unmatched Bandcamp items are no longer lost: they wait as pending and arrive when they resolve.
- Concurrent edits are safe: the sync goes through ADR-027's version check and a conflict retries.
- One sync is one atomic write, so one Undo takes it back.
- The merge is reusable for the outbox rebase, and has no Bandcamp knowledge.
- Link state lives outside the library, in `mdma-bandcamp`. A playlist edited on another node does not carry its link.
- Pending items are shown from the link state, not from the playlist file: the file holds only matched items.

## Alternatives considered

- **Base stored as a fact.** Rejected for now: playlists are not fact entities, and base is in Bandcamp-ID space. Revisit when playlists become facts (§5.3).
- **A scheduled sync.** Rejected: a sync changes the playlist the user is looking at, so it runs only when the user asks.
- **A Bandcamp-specific merge.** Rejected: the rules are about sequences, not Bandcamp. A generic `sequence_merge` can be property-tested alone and reused for the outbox.
