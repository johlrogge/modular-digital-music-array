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
    unchanged. RB remains the beat-grid master.
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
- **Variable-tempo grids** are out of scope: import captures the first `<TEMPO>` anchor
  only; multi-anchor grids are not modelled.
