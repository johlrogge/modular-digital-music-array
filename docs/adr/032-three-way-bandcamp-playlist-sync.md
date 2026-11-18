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
4. Sync first downloads owned tracks that are missing, as the import job does. This can take as long as an import, so a sync is a background job like the import: `SyncPlaylist` answers `PlaylistSyncStarted { job_id }` and `GetPlaylistSyncStatus` follows it.
5. An item already in base keeps its cached match while the matcher cannot resolve it; it is neither pending nor removed.

### 5. Storage

1. **Decided:** base, pending and the match cache live in `mdma-bandcamp`'s state file `playlist-links.json`, written atomically:
   `{local name, bandcamp id/url, base: [track_id], pending, matches, last_synced}`.
2. They are not facts: playlists are not fact entities, and base lives in Bandcamp-ID space.
3. A future per-track `BandcampTrackId` fact (source-owned, ADR-020) may replace the match cache. Revisit when playlists become facts.

### 6. Writing

1. A generic library-side component `sequence_merge` computes `merge3(base, theirs, ours) -> target`. It is generic over the key type and knows nothing about Bandcamp.
2. `playlist_edit::ops_between(current, target) -> Vec<EditOp>` turns the target into ADR-027 ops.
3. The ops are sent with `PlaylistEdit { version }`. That gives CAS, comment and short-ID preservation, an atomic write, and Undo via revert.
4. On Conflict: retry from the snapshot the Conflict carries, up to 3 attempts in all. Base is saved only on Applied.
5. **Crash window.** A crash between the PlaylistEdit being applied and the new base being saved must not lose what the sync did. Before it writes, a sync saves `pending_base: {target, new_base}` in the link state: the entries the playlist will hold, and the new base. The next sync settles a leftover `pending_base` with the current entries, before it merges, by one rule:
   - the entries equal `target`: the edit landed, so base becomes `new_base`;
   - anything else: `pending_base` is discarded and base stays.

   An edit that changes nothing writes no `pending_base`. After the third Conflict the staged `pending_base` is cleared, since nothing was written. A sound rule would need the library's version history, to tell "our edit landed, then the user changed the playlist" from "our edit never landed"; that was rejected for a two-fault case (see Consequences). Discarding after a landed edit is safe: the merge adds only items ours does not have, so it adds no duplicate. What is lost is the memory of what the sync added, so an item or an extra copy (see the amendment on ambiguous items) the user deleted in the meantime comes back once, and a Bandcamp removal of an item the sync added is not applied. The match cache and the pending list are saved with `pending_base`: they only describe Bandcamp and the library, so they are right whether or not the edit landed.
6. The same merge is meant for rebasing offline edits (the ADR-016 outbox).

### 7. First sync with no base

There is no special path. A link starts with an empty base and the sync runs `merge3(&[], theirs, ours)` like any other: every item of theirs that ours does not already have, by occurrence, is added, so an item already in the playlist is skipped as `Duplicates::Skip` would, and local-only items stay. Base is then set to theirs (without pending items). (Amended in phase 3: this replaces "add the missing items with `Duplicates::Skip`".)

### 8. Link, rename and delete

Linking a playlist to the Bandcamp playlist it is already linked to keeps the link and its base (a reset would bring back every track the user deleted); linking it to another one replaces the link and starts without a base.

Renaming the local playlist updates the link. Deleting it drops the link. Moving it to the bin (ADR-033) deactivates the link. These follow library events, which `mdma-bandcamp` does not subscribe to yet; until it does, a sync of a link whose playlist is gone fails with "not found" and the link stays until it is replaced by a new `LinkPlaylist`.

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

## Amendment 2026-11-18: typed "not linked" and console routes (phase 4)

`SyncPlaylist` for a playlist that is not linked is answered with the typed `SourceError::PlaylistNotLinked { name }`, not `InvalidRequest` with a message to be read. The console maps it to 409; `InvalidRequest` is always 422. The console reads the links and the sync job through `GET /api/bandcamp/playlist-links` and `GET /api/bandcamp/playlist-sync`, so no static path sits under `/api/playlists/`, where `:name` is a user's playlist name.

## Amendment 2026-11-18: ambiguous items add every candidate

§4 item 1 no longer counts Ambiguous as pending: Missing, DurationMismatch and null-field tracks still are. An ambiguous Bandcamp item resolves to **all** its candidate hashes, in the order the matcher returns them.

- **Sync:** the first candidate is the item's linked copy (its Bandcamp key and match in the cache). The other candidates are inserted right after it as local entries (`SyncKey::Local`), so merge3 never removes them. The item is applied: it enters base and is not pending. The extras are inserted only when the item first becomes applied, that is, while it is not yet in base; a later sync finds the key in base and adds nothing. A candidate the result already holds is not added again: one that is the linked copy of any item of theirs, one that is already an entry of the playlist, or one already added as an extra of an earlier item. An item already in base keeps its cached match when it turns ambiguous.
- **After that** the extra copies are ordinary local entries. Deleting the linked copy is a local deletion and stays deleted; the other copy stays. Deleting an extra copy is simply kept.
- **Outside §10:** the extras are expanded after `merge3`, from the keys it returns, so they are not keys of the merge and the §10 properties do not speak of them. What still holds: the sync is idempotent (the next sync finds the item in base and adds no extras), and theirs == base still gives ours (every item is in base, so none adds extras). "Every result item is in ours union theirs" does not hold for an extra copy.
- **Import job:** every candidate is written as consecutive entries where the item sits, and the report says "ambiguous: added N candidates". A library track of unknown duration passes the matcher's duration check and counts as a match; a track whose duration differs is not written.
- **The user removes the unwanted copies** by hand.

Rejected: keep Ambiguous pending. A silent skip hides the track from the playlist until someone notices and resolves the match, while a visible extra copy is plain to see and one delete away from right. A silent skip is worse than a visible extra copy.

The sync report has no field of its own for this: the extras count in `added`.

## Amendment 2026-11-18: links carry the playlist id

A link also keeps the Bandcamp playlist's id (`bandcamp_id`, optional so older link files still load). The console joins the fan's playlist listing to the links on it, not on the URL, which has several spellings (slug, with or without the user, with a query). The listing's `id` and a fetched playlist's `id` are the same number. A link without an id, made before this, gets it at its next link or sync and joins nothing until then.

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
- One exception to "local deletions stay deleted": if a sync crashes after its edit landed and the user then changes the playlist before the next sync, the edit is not recognised as landed, so an item or an extra copy the user deleted in between comes back once (§6.5). It takes two faults, and the merge adds no duplicate.
- Link state lives outside the library, in `mdma-bandcamp`. A playlist edited on another node does not carry its link.
- Pending items are shown from the link state, not from the playlist file: the file holds the linked copies of applied items, and the extra copies of ambiguous ones.

## Alternatives considered

- **Base stored as a fact.** Rejected for now: playlists are not fact entities, and base is in Bandcamp-ID space. Revisit when playlists become facts (§5.3).
- **A scheduled sync.** Rejected: a sync changes the playlist the user is looking at, so it runs only when the user asks.
- **A Bandcamp-specific merge.** Rejected: the rules are about sequences, not Bandcamp. A generic `sequence_merge` can be property-tested alone and reused for the outbox.
