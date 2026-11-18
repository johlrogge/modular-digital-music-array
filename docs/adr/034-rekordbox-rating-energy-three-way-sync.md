# ADR-034: Rekordbox rating ↔ energy three-way sync

## Status

Accepted, 2026-11-18 (Joakim approved the direction)

Joakim explicitly accepted two points: a local clear gives way to a later rekordbox change, and import runs before export.

Builds on ADR-032 (three-way merge, local wins), ADR-020 (source-owned facts), ADR-016 (marks, scalar facts and the console energy write). Related: ADR-027 (the same CAS-minded, base-comparing approach for edits).

## Context

- Rekordbox XML has **no per-field modification time**. The TRACK attributes include only `DateAdded`.
- Today `rekordbox_xml` never reads `Rating`, and export hardcodes `Rating="0"` (`components/rekordbox_xml/src/lib.rs`, around line 388).
- Import writes `--enrich` facts with `rekordbox_source()`: tool `mdma`, origin `Rekordbox` (`projects/mdma-cli/src/origins.rs`).
- Joakim wants "the most recent value wins". Without timestamps that can only be decided by comparing against the **last value exchanged**. That is the same three-way strategy as ADR-032: base, theirs, ours, local wins.

## Decision

### 1. Mapping

1. `Stars(u8)`, 0..=5, lives in `rekordbox_roundtrip`. It is Pioneer vocabulary and stays out of the library.
2. Rating to stars: `round(r / 51)`. A rating above 255 gets a warning and counts as 0.
3. Stars to energy: `2s`, with 0 meaning no energy.
4. Energy to stars: `ceil(e / 2)`, with no energy meaning 0.
5. Star to energy to star is the identity, so **every comparison happens in star space**. Example: energy 7 exports as 4★; re-importing 4★ keeps 7.

### 2. Three-way merge

b = base (none counts as 0★), t = theirs (rekordbox now), o = stars(ours).

| Condition | Action |
|---|---|
| t == b | keep ours |
| t ≠ b, o == b | take t |
| t ≠ b, o == 0 | take t (a local clear gives way to a new rekordbox change) |
| t ≠ b, o == t | converged, write nothing |
| t ≠ b, o ∉ {b, t, 0} | conflict; local wins, and it is reported |

1. "Take t" writes energy `2t` with `rekordbox_source`, or clears if t is 0.
2. After a non-dry-run import, base := t for every matched track, conflicts included, so each conflict is reported only once.
3. No base needs no special case: it counts as 0★.
4. **Asymmetry, on record:** a local clear yields to a later rekordbox change.

### 3. Storage

1. Base lives in a CLI-owned state file, `$XDG_STATE_HOME/mdma/rekordbox-exchange.json`, written atomically:
   `{nodes: {<node_key>: {tracks: {content_hash: stars}, last_import, last_export}}}`.
   The node key is the lowercased host of the remote node, or `local` for the services on this machine. A save rewrites only its own node's entry. A stored value above 5 makes the file invalid: it is an error, never read as 0 stars.
2. It is not a fact. The library holds no Pioneer data.
3. Base is per machine, and per node within it.

### 4. Sources

1. Import keeps `rekordbox_source`.
2. The Latest fold in `library_projection` is unchanged: a freshly applied import shows, and a later user set wins.
3. Clear retracts energy from **every source that writes it**. `REKORDBOX_ORIGIN` and `rekordbox_source` move to `library_ipc_client` next to `user_fact_source`, and an `energy_sources()` list is added.
4. The console clear and the import's "take 0" both use that list.

### 5. Export

1. Write `Rating = stars(ours) × 51`, and set base := that value for each exported track.
2. Documented order: **import, then export**. Otherwise a rating changed in rekordbox and not yet imported is overwritten.

### 6. Reporting

- stdout: `Energy: applied 12, cleared 1, kept local 3 (conflict), unchanged 240`.
- stderr, one line per conflict: `Artist - Title: kept MDMA 7 (4★), rekordbox has 2★`.
- Dry-run prints "Would …" and writes neither facts nor base.

### 7. Shared helper

`merge3_scalar<T: Eq>(base, theirs, ours) -> Decision { Keep, Take(T), Converged, Conflict }` is pure local-wins. It sits next to `merge3` in the existing `components/sequence_merge` component (on develop since f7786ba), as a `scalar` module, so "three-way, local wins" is one concept. The rekordbox "clear yields" rule stays in the rekordbox caller.

## Test plan

- `rekordbox_xml`: `Rating` parse and write.
- rstest tables for the conversions (rating to stars, stars to energy, energy to stars).
- Property tests:
  - star to energy to star is the identity;
  - export then re-import keeps every energy 1..10;
  - `merge3_scalar` properties (theirs == base keeps ours; ours == base takes theirs; converged writes nothing; local wins on conflict) and idempotence.
- The decision-table cases, one per row, including the local-clear case.
- `library_projection`: rekordbox 8, then user 6, then rekordbox 4 shows 4; retract-all shows none.
- Console clear with a rekordbox level present.
- CLI tests with a fake library: apply, clear, conflict report, dry-run writes nothing, base written atomically.

## Consequences

- Round-trips are stable.
- A user clear stays until rekordbox changes the rating.
- Base is per machine, and per node within it. Two exchanges with the same node running at once can leave a stale base; that heals on the next import, so there is no lock.
- An export before an import can overwrite a changed rekordbox rating.
- Each conflict is reported once.

## Alternatives considered

- **Base as library facts.** Rejected: it puts Pioneer data in the library, and every export would write facts.
- **Base derived from the latest rekordbox-source fact.** Rejected: exports write no facts, conflicts would be re-reported, and it would aggregate outside `library_projection`.
- **Writing imports as the user source.** Rejected: provenance is lost.
- **A precedence fold.** Not needed.
