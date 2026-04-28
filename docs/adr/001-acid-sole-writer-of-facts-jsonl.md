# ADR-001: ACID is the sole writer and source of truth for facts.jsonl

## Status
Proposed

## Decision

**ACID is the sole writer and authoritative source of truth for `facts.jsonl`.**

Concretely:

1. No service other than ACID writes to `facts.jsonl`. Ever.
2. No service reads `facts.jsonl` directly from disk. Facts are obtained
   only via ACID's IPC stream.
3. Services that need to mutate facts (e.g. library retractions) must do so
   through an ACID-side write API. Direct file mutation by another service
   is a violation, not an optimisation.
4. ACID owns the on-disk format. Consumers depend on the IPC contract, not
   on file layout.

## Why

Facts about the music library — track metadata, content hashes, retractions,
event timestamps — live in `facts.jsonl`. The ACID service
(`components/acid_service/`) owns this file and exposes facts to other services
over IPC. ACID has two interchangeable backends selected at build time via
polylith profiles: `fact_store_memory` (dev) and `fact_store_file` (production).

In practice, the boundary has leaked. The library service has historically:

- read `facts.jsonl` directly from disk in several code paths
  (`service.rs:1237`, `1610`, `1640`, plus others), and
- written `facts.jsonl` directly for retractions
  (`service.rs:1283`, `1327`).

Version 0.15.5 fixed an acute symptom of this leak. The library was persisting
a "last-seen cursor" to disk, then on restart asking ACID for facts *after*
that cursor. ACID returned zero (the cursor was stale or the in-memory store
was empty in that environment), so the library silently fell back to reading
`facts.jsonl` from disk and reconstructed its world from there. The system
appeared healthy. Output looked correct. ACID was bypassed entirely.

This is the worst class of bug: a silent fallback that produces plausible
results while the real contract is broken. It hides the failure of one
component behind the back-channel access of another, and it makes the
"shared file" a de-facto coordination point that no one designed and no
one owns.

The 0.15.5 hotfix removed the read-side fallback in the bootstrap path: the
library now reads every fact from ACID on startup, or fails loudly. The other
direct-file accesses remain and are tracked under task #71.

This ADR locks the principle so #71 has a north star and so future code does
not drift back into bypass territory.

## Alternatives considered

- **Status quo (multiple readers/writers of `facts.jsonl`)** — produced
  the 0.15.5 charade. Rejected.
- **ACID-preferred with file fallback** — same failure mode as status quo.
  Hides bugs by design. Rejected.
- **Move `facts.jsonl` ownership to the library, ACID becomes a cache** —
  inverts the dependency; ACID's value is precisely that it owns the
  authoritative log. Rejected.

## Consequences

### Positive

- **Loud failure replaces silent partial state.** Every service restart
  either succeeds (ACID had the data, the service received it) or fails
  visibly (ACID was unreachable or empty). No more "looks like it works"
  while the data path is broken.
- **ACID's on-disk format is free to evolve.** As long as the IPC contract
  is stable, ACID can change file structure, add indexing, switch encoding,
  or split into multiple files without coordinating with consumers.
- **Aggregate-persistence-per-service becomes the natural pattern.** The
  intended future architecture — each service persists its own dense
  aggregate and asks ACID only for deltas since a cursor — falls out of
  this rule cleanly. It is no longer a workaround for "expensive bootstrap";
  it is the design. The cursor is meaningful precisely because ACID is the
  sole authority on what comes after it.
- **One owner, one invariant.** Concurrency, crash safety, and durability
  of `facts.jsonl` are ACID's problem. No other service has to reason about
  them.

### Negative

- **Bootstrap cost scales with fact count.** Every library restart re-reads
  every fact from ACID over IPC. At 10k+ facts this becomes noticeable; at
  much larger scales it becomes painful. Mitigation: the
  aggregate-persistence-per-service work (follow-up task) lets services
  persist a dense local view and ask ACID only for the tail since a cursor.
  This ADR is what makes that mitigation safe: the cursor is meaningful
  only because no one else writes the file.
- **Library writes need an ACID write API.** Today the library writes
  retractions directly to `facts.jsonl`. Until #71 routes those writes
  through ACID, the system is in violation of this ADR. This is known and
  scheduled.

## Implementation Plan

- **Task #71** — route all library writes (retractions, etc.) through an
  ACID write API. Remove the six direct read sites in `service.rs` and the
  two direct write sites. After #71, `facts.jsonl` is opened by exactly
  one process: ACID.
- **Follow-up: aggregate persistence per service.** Each service persists
  its own aggregate plus the cursor of the last fact it consumed. On
  restart, the service loads its aggregate locally and asks ACID for facts
  since the cursor. This makes restart cost proportional to deltas, not
  to total fact count.

## Counter-examples (what this ADR does NOT propose)

- **Not a "shared file" with cooperative multi-writer protocols.** We are
  not adding file locks, sequence tokens, or write-coordination logic
  between services. There is one writer. Period.
- **Not "ACID is preferred but file access is allowed in a pinch."** There
  is no fallback. A service that cannot reach ACID fails. A silent fallback
  is the bug this ADR exists to prevent.
- **Not "the library may read directly for performance."** Performance
  concerns are addressed by aggregate persistence and cursor-based deltas,
  not by reaching around ACID.
