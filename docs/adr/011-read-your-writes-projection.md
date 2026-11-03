# ADR-011: Read-your-writes projection via a single catch-up path

## Status

Accepted — 2026-11-03

Amends ADR-004: supersedes its "Alternatives considered" entry that deferred synchronous in-memory updates (#44) and its Consequences note about the sub-second `Replaces` window. The rest of ADR-004 remains in force.

## Context

- The library's fact subscriber replays the library's *own* ACID writes (ADR-001: ACID is the sole writer of facts).
- The write paths (ingest, dedup, delete/restore, source-fact retraction, and replace's `Replaces` assert) also hand-applied their facts in memory. Facts were therefore applied twice: duplicate provenance, duplicate styles and memory_cues, counter drift.
- Replace had the opposite gap (#44): `Replaces` facts were not in memory until replay, so back-to-back replaces lost ancestors.
- Replay of retract lines also resurrected removed entities as empty "ghost" entries, so old hashes resolved again, violating ADR-004 (a replaced hash must stop resolving).

## Decision

- A single `catch_up()`, run under a dedicated mutex, atomically does read_stream(cursor) → apply_stream_lines → advance cursor.
- The fact subscriber calls it, and every writer calls it right after its ACID write returns. This gives read-your-writes through one projection path: the same `apply_fact_to_track` that bulk load uses via `FactAggregator` (ADR-004).
- Hand-rolled in-memory projections in write paths are removed.
- An entry exists only while it has live tracked provenance. The shared fold (`apply_fact_to_track`) resets an entity to empty when a retract leaves its provenance empty, clearing untracked fields (AddedAt, FilePath, timestamps), so a later re-assertion starts fresh on every path.
- Non-live entries are dropped by `retain(is_live)` on bulk load and by a single sweep at the end of each `catch_up` (after all pages, never mid-batch). Bulk load, replay and catch-up are therefore equivalent for both liveness and field values.
- Request handlers reach ACID writes only through `write_and_project` / `retract_and_project`, so a new write path cannot skip projection.

## Why

- One projection path means the in-memory index can only reflect what ACID recorded, exactly once. Double-apply and missing-apply are both impossible by construction rather than by per-path discipline.
- Writers observe their own writes immediately, closing the #44 window without a second write-side code path.
- Keeping retract non-creating and dropping provenance-less entries keeps replay faithful to ADR-004's hard-replace semantics.

## Alternatives considered

- **Per-path synchronous apply plus per-value idempotence** — the initial #44 fix. Correct for `Replaces` only, it duplicated bookkeeping three times, and it doesn't generalise to the upcoming merge.
- **Set semantics for every value** — rejected: conflicts with multiset value types; the ACID log is the source of truth for multiplicity.

## Consequences

- Writers pay one extra read_stream round-trip per write.
- Merge-with-carry-over can re-assert any value type safely.
- Entities that never had a tracked fact (e.g. play events written under short or empty hashes by older mdma-playback, #86) are no longer projected as tracks. On prod data this removes 113 phantom rows (983 → 870), including an empty-key entity that made playlist lazy repair treat every line as live.
- ADR-004's deferred-#44 alternative and its sub-second-window consequence no longer apply.

## Related

- ADR-001 — ACID sole writer of facts.jsonl
- ADR-004 — FactAggregator projection; hard replace (amended by this ADR)
- Issue #44 (synchronous in-memory `Replaces` provenance)
