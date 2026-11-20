# ADR-005: Rekordbox round-trip data contract: RB masters beat grids, MDMA masters cues, projected state over IPC

## Status

Accepted — 2026-07-07

Builds on ADR-003 (track lifecycle as pure-assert facts) and ADR-004 (retract-all
+ FactAggregator projection with (value,source) provenance) as the fact-semantics
foundation. Cues and beat grids are ordinary `MusicValue` facts subject to the same
assert/retract folding.

## Context

MDMA and Rekordbox (RB) both hold performance metadata for the same audio: beat
grids and cue points. Round-tripping through RB's XML must not let the two tools
overwrite each other's authority, and must not let re-imports accumulate stale
data. Two blockers motivated pinning the contract: export sourced cues by parsing
raw fact-log display strings (ignoring assert/retract folding, so retracted cues
re-appeared and re-imports duplicated), and `--enrich` wrote BeatGrid/MemoryCue
facts unconditionally (unbounded ACID growth on every sync).

## Decision

- **Asymmetric authority contract.**
  - **Beat grids flow RB → MDMA only.** Import reads the first `<TEMPO>` anchor and
    asserts a `BeatGrid` fact. MDMA **never writes `<TEMPO>` grid data back** on
    export; the exported `<TEMPO>` element carries BPM only and is otherwise
    unchanged. RB remains the beat-grid master. (**Superseded by the amendment
    2026-11-20**: MDMA owns grids on export and writes its stored grid as `<TEMPO>`;
    a placeholder is never written.)
  - **Cues flow MDMA → RB.** `MemoryCue` facts export to `<POSITION_MARK>` elements.
    RB imports them with "overwrite all cue points." MDMA is the cue master.

- **Export and enrich read PROJECTED state, never the raw fact log.** Cues and the
  beat grid are read from `TrackInfo.memory_cues` / `TrackInfo.beat_grid` over IPC —
  the folded projection that honours assert/retract (per ADR-004's FactAggregator).
  The append-only fact log is never parsed for this: raw lines ignore retraction and
  accumulate across re-imports. `TrackInfo` gained `memory_cues: Vec<CueInfo>` and
  `beat_grid: Option<BeatGridInfo>` (`#[serde(default)]`) for this purpose;
  `LibraryResponse::Track` is boxed to keep the enum small.

- **Enrich writes are idempotency-guarded (`already_current`).** Before asserting
  BPM, Key, BeatGrid, or each MemoryCue, enrich compares the incoming value field-by-
  field against the projected state and skips exact matches. This keeps the ACID log
  bounded on the repeated sync path — re-running an unchanged export writes nothing.

- **Track identity across the boundary.** RB keys tracks by file `Location`; MDMA
  keys by `ContentHash`. The export manifest carries typed `HashLocation { hash,
  location }` pairs to map both directions on re-import; the metadata matcher
  (artist/title/duration) is the fallback when no manifest entry resolves.

- **`rekordbox_roundtrip` stays a pure conversion component**, depending only on
  `rekordbox-xml` and `music-facts` — no `library_ipc_protocol` coupling. It converts
  `PositionMark ↔ MusicValue::MemoryCue` and `TempoAnchor → MusicValue::BeatGrid`
  as exact inverses.

## Why

- Asymmetric authority is the only assignment that avoids clobber: RB's grid
  analysis is mature and MDMA has none yet, so grids are read-only from RB; cues are
  a first-class MDMA concept, so MDMA owns them and RB is told to overwrite.
- Reading projected state (not the raw log) is what makes retraction meaningful end-
  to-end: a cue retracted in MDMA must vanish from export, and a re-import must not
  duplicate. Only the folded projection guarantees this (ADR-003/004).
- Idempotency guards keep the append-only log from growing on every sync, which
  matters on the Pi where the fact store is the durable ACID log.

## Alternatives considered

- **Parse cues from the raw fact log display strings.** Rejected: ignores
  assert/retract folding — retracted cues resurface and re-imports duplicate.
- **Symmetric grid ownership (MDMA writes `<TEMPO>` back).** Rejected until MDMA has
  native beat analysis; would clobber RB's authoritative grid with round-trip noise.
- **Unconditional enrich writes.** Rejected: unbounded ACID growth on every sync.
- **Location-only or hash-only identity.** Rejected: Location breaks when files move;
  hash is absent from RB's model. The manifest pairs them, matcher is the fallback.

## Consequences

- All services embedding `library_ipc_protocol` (gateway, library, console) redeploy
  together; `TrackInfo` gained fields (backward-compatible via `#[serde(default)]`,
  no `deny_unknown_fields`, so old and new peers interoperate).
- On enrich, a failed `get_track` falls back to writing (safe default), so a transient
  IPC failure can re-write facts; the ACID log tolerates this and a projection re-derive
  is unaffected.
- RB hot-cue/loop slot indices are truncated to `u8` (lossy by design; RB slots are 0–7).

## Deferred

- **MDMA-native beat analysis** ("cut the cord" increment) will populate `BeatGrid`
  through the same fact shape; at that point grid authority may become MDMA's and the
  RB→MDMA-only rule is revisited.
- **Variable-tempo grids** are not modelled: a grid is one anchor and one BPM. (Amended
  2026-11-20: import now reads every `<TEMPO>` anchor and imports only the sets that one
  anchor describes exactly; see the amendment below.)

## Amendment 2026-11-20: MDMA owns grids on export; placeholders are never written

Supersedes the first decision's "beat grids flow RB → MDMA only" and "RB remains the beat-grid master". The "Symmetric grid ownership" alternative is no longer rejected.

**Why.** The export wrote a placeholder `<TEMPO Inizio="0.000" Bpm=<AverageBpm> Metro="4/4" Battito="1">` for every track with a BPM, because it had no grid to write. rekordbox kept those placeholders for many tracks (58 of 372 exported copies in one real library), and `rekordbox import --enrich` read them back as real grids. Where rekordbox held two copies of one track, one with the placeholder and one with the real grid, the last copy processed won.

**Decided.**

- **The export writes MDMA's stored grid as `<TEMPO>`**: `Inizio` is `first_beat_ms / 1000` (3 decimals), `Battito` is 1 (the first beat is a downbeat), `Bpm` is the grid's BPM (2 decimals) and `Metro` is `beats_per_bar/4`. This is the exact inverse of the import's `grid_from_tempo` for a single-anchor grid (`tempo_from_grid` in `rekordbox_roundtrip`).
- **A track without a grid gets no `<TEMPO>`**, so rekordbox analyses it itself. A placeholder is never written.
- **Merging into an existing rekordbox.xml:** MDMA's grid wins when it has one. When it has none, the existing `<TEMPO>` set of that track is kept, not deleted, unless it is a placeholder (the existing file is usually MDMA's own earlier export, so placeholders are dropped, on tracks that are refreshed and on tracks that pass through). Every anchor of a kept set is written, so a set is never collapsed.
- **The import ignores placeholder grids**: a TEMPO set of exactly one anchor at `Inizio` 0.000 on beat 1 of a 4/4 bar carries no grid information. It is not written and retracts nothing, so MDMA's grid stays. The report lists it as `placeholder-grid`. A real grid whose first beat is at 0 ms in 4/4 cannot be told apart and is skipped too. The predicate lives on `TempoSet` in `rekordbox_xml`, since it describes the shape of the XML.
- **Variable tempo.** MDMA never holds a grid it knows is a truncation, but it keeps grids that are exact. The import reads every anchor. A set of several anchors is *consistent*, and collapses to a single grid (its first anchor's) within 5 ms of every anchor, when every later anchor has the same `Bpm` (to 2 decimals) and `Metro` as the first, sits within 5 ms of a beat of the first anchor's grid, and has the `Battito` that the bar position extrapolated from the first anchor gives. A consistent set is imported as the single grid. An inconsistent one (`TempoKind::Variable`) writes no grid, is reported as `variable-tempo` (anchor count, the first anchor that breaks the rule and why, and the worst offset in ms), and retracts the library's grid if that grid equals the first anchor's. If no other source has a grid for the track, the export then writes no `<TEMPO>` for it, so rekordbox keeps its own; if another source does, MDMA owns grids and the export writes that grid. In one real library, 122 of 159 multi-anchor tracks were consistent and 37 drifted by up to 188 ms.
- **Several rekordbox tracks matching one library track:** placeholders are ignored; a variable-tempo copy means no grid, whatever the others say; otherwise one distinct grid is used, and two or more distinct grids write no grid and are reported as `grid-conflict` (with each candidate's TrackID, Location, Inizio, Battito and Bpm). There is no tolerance between copies: they are different files, a transcode can shift the grid, so a grid from another file may not apply exactly. That is why a disagreement is reported and not resolved.
- Cues still flow MDMA → RB; BPM and key are unchanged by this amendment.
