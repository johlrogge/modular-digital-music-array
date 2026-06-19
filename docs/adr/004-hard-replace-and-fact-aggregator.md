# ADR-004: Hard replace via retract-all + Replaces fact; FactAggregator projection with (value,source) provenance

## Status

Accepted — 2026-06-18

Amends and supersedes the **replace/supersession** half of ADR-003. ADR-003's
soft-delete (`Deleted`) and restore decisions remain in force. The `SupersededBy`
variant and its "hidden = deleted OR superseded" rule are withdrawn.

## Context

ADR-003 modeled replace as a pure-assert `SupersededBy { replacement }` fact on the
OLD track, kept the old entry hidden-but-resolvable, and named retraction (other than
un-deleting) a thing to avoid. Closing #96 (retraction must match the asserted value)
and #2 (a fact must survive retraction by one source if another source still asserts it)
required making retraction a first-class, trustworthy operation. Once retraction is
trusted, hiding-via-supersession is redundant and the cleaner identity model is a true
replacement.

## Decision

- **Content hash remains the entity key.** A richer work-identity model stays deferred
  (per ADR-003).

- **Projection foundation: `FactAggregator`.** The three hand-rolled fact projections
  (bulk load, file load, incremental stream) are replaced by a single
  `stainless_facts::FactAggregator` impl on `IndexedTrackInfo`, driven by
  `aggregate_facts`. A 3-path equivalence test proves the bulk, incremental, and test
  projections produce identical state. Bulk load no longer stamps placeholder
  TrackStarted/TrackStopped facts; `refresh_event_timestamps` is the sole authority for
  those fields.

- **(value, source) provenance.** `IndexedTrackInfo` carries a `provenance:
  Vec<(MusicValue, FactSource)>`. Retraction matches on **value + source**, not value
  alone. This fixes #96 (Retract(Bpm=120) is a no-op when only Bpm=128 is asserted) and
  #2 (retracting one source's assertion leaves another source's identical assertion
  standing).

- **Hard replace = retract-all-old + `Replaces(old)` on new.** `SupersededBy` is
  removed. `library track replace`:
  1. gathers the old track's `Replaces(*)` ancestors from provenance (before retraction);
  2. ingests the new file;
  3. asserts `Replaces(old)` plus all inherited ancestors on the NEW track (deduped);
  4. `retract_all_entity_facts(old)` — reads all of old's asserted (value,source) pairs
     from ACID and retracts each, then drops the in-memory entry. The old hash stops
     resolving (TrackNotFound), not hidden.
  5. eagerly rewrites playlists old→new.

- **Forward-inherited `Replaces`.** When C replaces B, C asserts `Replaces(B)` *and*
  every hash B had replaced (e.g. `Replaces(A)`). Reverse lookup is therefore always
  single-hop from the live track, and a multi-hop chain survives full retraction of
  intermediates. Inherited facts are written through ACID (durable across restart).

- **Self-healing playlists.** On `PlaylistGet`, any unresolvable hash line is repaired by
  a reverse-`Replaces` walk (`resolve_through_replaces`, cycle- and depth-guarded to 64
  hops) to the live successor; the .plist is amended in place via tmp+rename. Eager
  rewrite handles the common case; lazy repair covers playlists not present at
  replace time.

- **Orphans redefined.** `mdma library track orphans` walks `{music_dir}/blobs/**` and lists any
  blob whose hash is absent from the live index as `OrphanReason::NoLiveFacts` (the
  hard-replace leftover, primary GC candidate). Soft-deleted tracks remain
  `OrphanReason::Deleted`. The `SupersededBy` reason is removed.

- **`delete`/`restore` unchanged.** Soft delete asserts `Deleted{timestamp}` (hidden,
  recoverable by retracting it). This is distinct from hard replace and is not affected.

## Why

- **Retraction is now a trusted fundamental, reversing ADR-003.** ADR-003 avoided
  retraction to keep the fact algebra simple; #96/#2 showed correct projection *requires*
  value+source-matched retraction. With that primitive trustworthy, hard replace is
  simpler and more honest than hide-via-`SupersededBy`: the replaced identity genuinely
  ceases to exist rather than lingering as a resolvable-but-hidden entry.
- A single `FactAggregator` projection removes three divergent code paths and their
  drift risk; the equivalence test makes divergence a test failure.
- Forward-inheritance keeps reverse lookup O(1) per generation and makes the chain
  robust to intermediate retraction — no need to keep dead intermediates addressable.

## Alternatives considered

- **Keep `SupersededBy` (ADR-003).** Rejected: redundant once retraction is trusted;
  leaves dead identities resolvable and complicates "hidden" semantics.
- **Bidirectional or stored reverse `Supersedes`.** Rejected: forward-inherited
  `Replaces` gives single-hop reverse lookup without a second source of truth.
- **Lazy-only playlist repair (no eager rewrite).** Rejected: eager rewrite fixes the
  common case immediately; lazy repair is the safety net. They are idempotent together.
- **Synchronously updating in-memory provenance in the replace path.** Deferred: the
  background fact subscriber reconciles it; ACID is the source of truth. Tracked as #44.

## Consequences

- Replace is still not transactional: ingest → assert Replaces → retract-all → rewrite
  can partially complete; ACID remains the source of truth and a restart re-derives a
  consistent index. Clear error messages required.
- The new track's *in-memory* `Replaces` provenance is populated by the background fact
  subscriber, not synchronously in `handle_track_replace`. A sub-second window exists
  where back-to-back replaces could gather an incomplete ancestor set in memory; ACID is
  unaffected and a restart self-heals. Tracked as #44.
- Disk is not reclaimed on replace; the orphaned blob is a `NoLiveFacts` GC candidate
  (future GC work).
- All services embedding `library_ipc_protocol` (gateway, library, console) must be
  redeployed together: the `MusicValue` enum changed (`SupersededBy` removed,
  `Replaces` added) and `OrphanReason` changed.
- Removing `SupersededBy` is a breaking fact-schema change; existing fact streams
  containing `SupersededBy` would fail to deserialize and must be migrated. Verified safe
  for this deployment: zero `SupersededBy` facts on the production fact stream as of
  2026-06-18.
