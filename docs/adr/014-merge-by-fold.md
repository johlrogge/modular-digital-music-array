# ADR-014: Merge by fold: the projection folds `Replaces`, latest wins by original timestamp

## Status

Proposed, 2026-11-04

Amends ADR-013 (mechanism, fact rules, play history, crash safety) and ADR-004 (hard replace as retract-all plus forward-inherited `Replaces`, eager playlist rewrite). Keeps ADR-011's single projection path and extends it.

## Context

ADR-013 phase 2 merges by **carrying**:
- `plan_carry` and `carry_rule` (service/dedup.rs) work out which loser facts the winner lacks.
- `merge_into` re-asserts those facts on the winner. They keep their original `FactSource` but get a **new ACID timestamp**.
- `retract_all_entity_facts_locked` then retracts the loser.

The restamping is the root defect. A carried value is "newer" than everything the winner holds, so per-attribute latest-wins can no longer be worked out from the log. That is the ItemId scalar bug: the winner shows the loser's ItemId. `plan_carry` works around it for a single group by planning all losers up front. Later merges, retractions and resyncs still see the restamped copies.

The projection already approximates a fold in several places:
- `refresh_event_timestamps` walks the lineage (`replaces_lineage`, `play_times_for`) to borrow losers' plays, but keeps only the last start and stop per key.
- It resolves #86 short hashes by prefix uniqueness.
- `resolve_through_replaces` walks reverse `Replaces` edges to repair playlists.

That is three code paths, each interpreting `Replaces` in its own way.

Projection mechanics today:
- `apply_fact_to_track` folds one fact into one entity.
- Provenance is `Vec<(MusicValue, FactSource)>` with no timestamp. A retract re-resolves the scalar from the last surviving entry, so fold order decides.
- `FactAggregator::assert` receives no timestamp; service.rs:818 passes `Utc::now()`.
- stainless-facts 0.3 adds `apply(&Fact)`, which sees the timestamp, plus `aggregate_facts_into_with` and `is_retired`. MDMA has not adopted it yet and is pinned to rev 2aa820a.
- Its fold is per entity, so there is no hook for moving state between entities.

## Decision

### 1. The projection folds `Replaces`

- When the fold meets `Replaces(X)` asserted on T, it resolves X through the replaced-by map to R.
- If R is live and R ≠ T:
  - R's aggregated state is folded into T;
  - R becomes **replaced**: not live, not listed, and not in the live set of `content_hashes`;
  - the replaced-by map records R → T.
- **Absorbed set.** Every aggregate records the set of entities it has absorbed, transitively; T's set gains R plus R's own set.
  - `Replaces(X)` with X already in T's absorbed set, or X = T, is a no-op.
  - This makes the edge idempotent and cycles inert. It also neutralises the forward-inherited `Replaces` already on disk from ADR-004 replaces and phase 2 merges.
  - No event ids or per-event dedupe are needed. A pure fold never double-counts: A→B copies A's state, and B→C copies B's, which already contains A's. Counting twice could only happen by following an edge twice, and the absorbed set prevents that.
- `Replaces(X)` where X has already been folded into another live entity R ≠ T is **inert**. The first fold wins and the case is logged. This stops a stray edge from silently merging two winners.
- `Replaces(X)` where X has no aggregate (unknown, or retracted legacy data) folds nothing, but still records X → T in the replaced-by map.

### 2. Latest wins by original timestamp

- Every folded fact keeps its original timestamp, its `FactSource` and its **origin entity**, i.e. the entity of the log line.
- Provenance entries become `(value, source, timestamp, origin)`.
- The per-attribute winner is the maximum of (timestamp, log order); fold order no longer matters. A loser value therefore never beats a newer winner value, wherever `Replaces` sits in the log.
- The same comparator applies within a single entity. On a log with monotonic timestamps it gives the same results as today.

### 3. Fold strategies per attribute

- **Latest:** every scalar (Title, Artist, Album, Bpm, Key, Energy, BeatGrid, Label, Source, …), and **AddedAt**.
- **Union:** StyleDescriptor, MemoryCue, Bookmarked (united by mark name; each name is present or absent independently; see ADR-016), ItemId, and the TrackStarted/TrackStopped events.
- **Keep-target**, meaning the winner's own values stand and folded values are ignored: FilePath, Format, DurationSeconds, Bitrate, FileSizeBytes, HasAlbumArt, EncoderSoftware, EncodedBy, CoverArtPath, AudioHash, PcmHash, BitDepth, Channels, SampleRate.
- **Not folded:**
  - Deleted, because a soft-delete belongs to the copy it hid;
  - the loser's own `Replaces` edges, because its absorbed set already accounts for them.
- The strategies are an exhaustive `match` on `MusicValue`, replacing `carry_rule`. A new variant must be given a strategy.

### 4. No carry step, no retire on merge

- `plan_carry`, `carry_rule` and `CarryRule` are removed, along with "copied facts stamped now".
- A merge writes exactly one fact: `Replaces(loser)` on the winner. Its source is `mdma-dedup`, or the user source for `track replace`.
- No retracts follow `Replaces`. Merge and replace no longer use `retract_all_entity_facts_locked`.
- **Forward-inherited `Replaces` are no longer written.** They only existed so that A could reach C in one hop. The replaced-by map resolves transitively to the chain head, with path compression.

### 5. Ingest self-heal is one write

- AudioHash v1 and the source ItemId are known before the write. The automatic group is decided from the index plus the new track's prospective facts.
- `Replaces(loser…)` are the **first** facts of the new track's ingest batch, so the merge is atomic. Everything after them in the batch is newer and wins as normal.
- If ACID stamps facts in one batch with equal timestamps, log order breaks the tie.
- Merges between two existing tracks write `Replaces` alone and rely on the timestamp comparison. This covers an old copy that gained an ItemId and loses to a newer copy, `dedup --apply`, and a confirmed merge.

### 6. "Replaced" means not live

- `is_live` means the entity has tracked provenance **and** is not replaced.
- `resolve_hash(loser)` still returns TrackNotFound, as in ADR-004. The replaced-by map is the resolver for playlists and for late facts.
- **Late facts.** A fact logged against a replaced hash after the fold is redirected to the chain head. This covers playback events, a re-ingest of the loser's exact bytes, and retracts.
- **Re-ingest of a replaced hash** is treated like an existing duplicate of the head: the inbox file is removed, the source facts go to the head, and the blob is not re-imported.

### 7. Retraction on the winner

- A retract addressed to T, directly or by redirect, matches on (value, source). It prefers the entry whose origin is the retract's own entity, and otherwise takes the latest matching entry in T's absorbed lineage.
- So a value that came from the loser can be retracted on the winner. A legacy retract-all of the loser (lines logged against the loser, then redirected) removes exactly the loser-origin entries.
- `retract_source_facts` must read the facts of every entity in the head's lineage, not only `read_entity(head)`. It writes each retract against the fact's origin entity, and the redirect plus origin precedence route it correctly.

### 8. Undo

- Retracting a `Replaces(X)` on T un-folds it:
  - entries with origin ∈ {X} ∪ absorbed(X) move back to X;
  - X's absorbed set is restored;
  - the redirects into T for those entities are removed;
  - both aggregates are re-resolved.
- Facts logged against X after the fold go back to X (their origin is X).
- Un-folding is a sequential operation on the log, so a full replay and incremental catch-up agree.
- **Undo is an in-memory rebuild only.** When `catch_up` sees a retracted `Replaces`, it rebuilds the affected entities (T, X and X's absorbed set) from their facts in the local ACID log. A full in-memory rebuild of the projection from the local log is an acceptable fallback. Nothing is re-fetched from sources (for example no Bandcamp re-download) and no files are touched.
- **Undo restores the blob from the archive.** A merge moves X's blob to `archive/` (§10), so an undo moves it back. If the archive has been emptied, the undo restores X's metadata but not its audio, so X would come back live with a missing blob; the undo must then refuse or show X as file-missing (see Open questions, 2).

### 9. Play history

- TrackStarted and TrackStopped are folded like any union attribute. Each event keeps its own timestamp and origin and moves into the head's event list.
- History is the union over the lineage, ordered by (timestamp, log order), straight from the fold.
- `last_started`, `last_stopped` and the play count derive from it; play count is the number of TrackStarted events.
- Only asserts count. Retracts of play events, which legacy retire wrote, are ignored as they are today.
- **No other code path follows `Replaces`.**
  - The lineage walk in `refresh_event_timestamps` (`replaces_lineage`, `play_times_for`), with its separate `event_cursor` and `play_events` map, is removed. Events arrive through the single `catch_up` projection (ADR-011).
  - The #86 short-hash prefix lookups are removed in favour of alias entries in the redirect map.
  - `resolve_through_replaces` becomes a replaced-by lookup.
- **Pairing starts and stops.** Events carry no deck or queue reference: `StartReason` is {OnRequest, ByQueue} and `StopReason` is {OnRequest, OnCompletion, OnSkip}.
  - Two copies playing at once on two decks interleave as start, start, stop, stop on the head. The count and the last-started/stopped values stay correct, but per-play durations are ambiguous.
  - Pairing within one origin entity tells two *different* copies apart. Nothing can tell apart the same hash playing on two decks.
  - Adding a session or deck id to play facts would be a `MusicValue` change, requiring a redeploy of all readers together (ADR-004), and is out of scope.
- **Memory.**
  - Play events cost about 24 B each (a 12 B timestamp, a reason byte and a u32 origin index, padded). 100k events is about 2.4 MB and 1M about 24 MB; prod has about 870 tracks.
  - Adding timestamp and origin to provenance costs about 16–24 B per entry; about 30k entries is about 0.7 MB.
  - Negligible on a Pi 5.

### 10. Loser blob archiving (ADR-013, revised for the fold model)

- The loser's blob is moved to `/music/archive/` (same relative layout, no overwrite) after its `Replaces` is durable, under `retire_gate`, followed by the merge-orphan cleanup. A merge never deletes a file; the archive is emptied only by the explicit `mdma library archive empty`.
- An orphan candidate is a hash that a live `Replaces` from `mdma-dedup` names and that the projection holds as **replaced**. ADR-004 leftovers are still out of scope.
- **#91 liveness recheck.** "No live facts" is no longer the test. Immediately before archiving, under the gate, re-check that:
  - the hash is still replaced;
  - its chain head is live;
  - the head's blob is a different file.

  A concurrent retraction of the `Replaces` (an undo) or a retire of the head therefore keeps the blob.
- `retire_gate` stays, with two remaining jobs:
  - serialising the check-then-write in merges (both members live, loser not already folded elsewhere);
  - guarding blob archiving.
- The backfill can no longer resurrect a loser. A write to a replaced hash is redirected, and its identity facts are keep-target on the head, so they are ignored.

### 11. ADR-004 hard replace

- `track replace` becomes: ingest the new file with `Replaces(old)` as the first fact of its batch. There is no retract-all and no inherited ancestors.
- To revert A→B back to A (replacing B with A's bytes), retract B's `Replaces(A)`, which un-folds it and makes A live, then assert `Replaces(B)` on A.

### 12. Playlists

- **Recommended: resolve at projection level.**
  - `PlaylistGet` and export map each line through the replaced-by map.
  - `.plist` files are not rewritten by a merge, so an undo is clean and playlists point back at X again.
  - One mechanism covers every playlist, including those that were offline at merge time.
- **Alternative: keep the eager rewrite.**
  - Playlist files stay self-contained for external readers.
  - But an undo leaves playlists pointing at the head, and the merge remains a second writer of playlist state.
- Lazy write-back repair is optional in either case; correctness no longer depends on it.

### 13. Bandcamp `has_facts` and ItemId

- `fact_index` is built from live provenance, which now includes the folded ItemIds (union). A retired copy's purchase therefore stays known without being restamped.
- `content_hashes_for_item_id` returns the head.
- Resync retraction follows §7.

### 14. Offline replica (`library_projection`)

- The fold, the redirect map and the absorbed set live in one shared function. Bulk load, incremental `catch_up`, the test file loader and `library_projection` on the offline branch all use it; these are ADR-011's equivalent paths.
- The equivalence test gains `Replaces` fixtures and a split-point property: for any prefix/suffix split of a log, folding the suffix into fold(prefix) equals fold(whole). The property covers the redirect map, the absorbed sets and edge retraction.
- **#86 short-hash aliases** depend on the current key set: a prefix is unique only relative to the hashes known so far. To keep the paths equivalent, aliases are resolved against the full key set at read time rather than fixed at fold time.

## Why

- **The log alone determines the result.** A merged entity is computed from facts with their real timestamps, so ADR-013's "one entity as if all facts had been asserted on it" actually holds. The carry only approximated it, once.
- **Atomic and idempotent by construction.** A merge is one fact. Repeating it is a no-op because of the absorbed set, and a crash cannot leave a half-carried state.
- **One interpreter of `Replaces`.** Three ad-hoc lineage walks are removed, and history needs no event ids.
- **Undoable.** Retracting the edge reverses the merge, as long as the blob still exists.

## Alternatives considered

- **Keep carry plus retire (ADR-013 phase 2).** Rejected:
  - restamping corrupts latest-wins, as the ItemId bug shows;
  - the merge is several non-atomic writes;
  - it needs whole-group up-front planning to be correct even once.
- **Copy play events at merge time.** Rejected: it restamps them, double-counts on chained merges and bloats the log.
- **Hard merge that deletes old facts.** Rejected for now: it loses history and undo, and the log space it would save is negligible.
- **Per-event dedupe by (origin, log position).** Rejected: a pure fold cannot double-count, and the absorbed set covers the only real risk, following an edge twice.
- **Implement in MDMA only, never upstream.** Interim only. The capability is generic; see johlrogge/stainless_facts#14.

## Consequences

- **Fact schema:** no new `MusicValue` variants.
- **Protocol:** no change expected. `OrphanReason::NoLiveFacts` covers replaced blobs; a dedicated `Replaced` reason is an optional follow-up.
- **Readers must upgrade first.** A reader with the old projection would see a fold-only merge as two live copies, with the head missing the loser's values. Roll out in two steps:
  1. Ship the fold projection to every reader: the library, `library_projection`, the stub and the BDD harness. It projects legacy data the same way as today.
  2. Then switch the writers: one-write self-heal, `Replaces`-only merge and replace, no carry, no inheritance.

  The library and the replica must move together within each step. Downgrading after step 2 shows the duplicates again but loses no data.
- **Existing data:**
  - **Phase 2 merges and ADR-004 replaces** fold the loser at its `Replaces`. The loser's later retract-all lines are redirected and remove the loser-origin entries (§7).
  - **Restamped carried copies** stay on the head, so already-merged tracks project as they do today, including the ItemId display issue on those few items. These are left as they are (open question 4, decided); prod has had no merges.
  - **Inherited `Replaces`** are no-ops because of the absorbed set.
  - **Play events on retired losers** fold through their `Replaces`.
- **Order of work:**
  1. Adopt stainless-facts 0.3: `apply` with the timestamp, `aggregate_facts_into_with`, `is_retired`. This alone makes the bulk and incremental paths one fold.
  2. Add timestamp and origin to provenance, and switch to the (timestamp, log order) comparator.
  3. Add the fold, the redirect map and the absorbed set as a map-level wrapper in MDMA. Move play events into the fold and delete `refresh_event_timestamps`.
  4. Switch the writers: one-write self-heal, `Replaces`-only merge and replace. Remove `plan_carry`.
  5. Upstream the wrapper once stainless_facts provides fold-entity-into-entity (johlrogge/stainless_facts#14).
- **Clock dependence.** Latest-wins across entities relies on ACID timestamps never going backwards. ACID guarantees this: each new fact gets a timestamp at or after the previous one, even if the system clock steps back, for example on a Pi 5 without an RTC battery before NTP has synced.
- **Equivalence test.** The ADR-011 test is extended (§14). Removing `play_events` and `event_cursor` also simplifies the lock order.

## Open questions

Answers marked **decided** were given by Joakim. Those marked **proposed** are defaults awaiting his decision.

1. Playlists: resolve at projection level only, or keep the eager rewrite? **Proposed:** resolve at projection level.
2. Undo after the blob has been removed? **Decided:** a merge never deletes the blob; it moves it to the archive (§10), so an undo can restore the file from `archive/` (the relative layout makes that a plain move back). Only after `mdma library archive empty` is the audio gone, and then the §8 caveat applies.
3. Does ACID guarantee monotonic timestamps? **Decided:** yes. ACID never lets timestamps go backwards: each new fact gets a timestamp at or after the previous one, even if the system clock steps back (see Consequences, Clock dependence).
4. Legacy restamped carried values (the ItemId display on already-merged items): repair them or accept them? **Decided:** leave them. Stage2 is for experimenting and can be fixed later, and prod has had no merges.
5. How is a `Replaces` retraction undone? **Decided:** an in-memory rebuild only. Rebuild the affected entities (T, X and X's absorbed set) from their facts in the local ACID log; a full in-memory rebuild from the local log is an acceptable fallback. Nothing is re-fetched from sources and no files are touched (§8).

## Related

ADR-003 · ADR-004 (amended) · ADR-010 · ADR-011 (single projection path; extended) · ADR-012 · ADR-013 (amended) · #86 (short play-event hashes) · #91 (liveness recheck before blob removal) · johlrogge/stainless_facts#14 (fold entity into entity)
