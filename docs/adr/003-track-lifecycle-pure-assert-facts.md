# ADR-003: Track lifecycle: soft-delete and supersession are pure-assert facts; hidden = deleted OR superseded

## Status

Accepted — 2026-06-12

## Decision

- Track lifecycle is modeled as two additive `MusicValue` fact variants: `SupersededBy { replacement, timestamp }` and `Deleted { timestamp }`. Both are pure asserts.
- A track is **hidden** from default views iff `deleted_at.is_some() || superseded_by.is_some()`. Hidden tracks are filtered at `list_tracks`/`search_tracks`/`get_track` but NOT at `resolve_hash` (so restore/replace/facts can still address them).
- `SupersededBy` is asserted on the **OLD** track only, pointing to the replacement (old→new, single asserted direction). No reverse `Supersedes` fact is stored.
- `Deleted` and `SupersededBy` are distinct concerns: deletion is user intent to hide; supersession is "a better version of this work exists."
- **Recovery = retract the `Deleted` fact** — the only sanctioned retraction in this design. Restore does not touch `SupersededBy`.
- Ingest/metadata facts are never retracted by delete or replace.
- In-memory index entries are never removed from the `tracks` Vec; hidden state is a per-entry flag.
- Blobs and symlinks are left untouched; physical reclamation is deferred to a future GC pass (`mdma track orphans` lists candidates, read-only).

## Why

- Consistent with ADR-001 (ACID is sole writer of append-only facts): lifecycle is expressed as facts, not destructive mutation, preserving full history and auditability.
- Pure-assert + single sanctioned retraction keeps the fact algebra simple and the in-memory projection deterministic on replay.
- Content hash remains the entity key. A richer work-identity model (grouping remasters/versions as one logical work) was explicitly raised and **deferred**; `SupersededBy` gives a pragmatic old→new link without committing to that model.

## Alternatives considered

- **Hard delete (drop facts/entries)** — rejected: violates append-only, loses recoverability and history.
- **Bidirectional supersession facts (old→new and new→old)** — rejected: redundant, two sources of truth to keep consistent.
- **Work-identity entity above content hash** — deferred: larger design; content hash stays the entity key for now (see GH #25/#26/#27 brainstorm notes).
- **Retract ingest facts on delete** — rejected: would make restore lossy and conflate provenance with visibility.

## Consequences

- Replace is idempotent but not transactional: ingest → assert `SupersededBy` → rewrite playlists can partially complete; re-running converges. Clear error messages are required.
- Playlist rewrite operates on first-token hash matching with short-hash prefix support; short-hash ambiguity across two tracks can mis-rewrite — a uniqueness guard is tracked as a follow-up.
- Disk is not reclaimed on delete/replace; a future GC must walk live vs. superseded/deleted hashes.
- All services embedding `library_ipc_protocol` (gateway, library, console) must be redeployed together for the new variants.
