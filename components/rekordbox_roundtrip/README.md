# rekordbox_roundtrip

Semantic mapping between Rekordbox XML types and MDMA music facts.

[Back to workspace README](../../README.md)

---

## What it does

Converts Rekordbox `<TEMPO>` grid anchors and `<POSITION_MARK>` cue points to typed MDMA `MusicValue` facts, and converts MDMA memory-cue facts back to Rekordbox position marks for export.

**Rekordbox is the beat-grid master.** MDMA reads `<TEMPO>` on import and stores it as a `BeatGrid` fact, but never writes `<TEMPO>` back on export.

Key functions:

- `grid_from_tempo(anchor, bpm)` — converts a `TempoAnchor` to `MusicValue::BeatGrid`; seconds are rounded (not truncated) to milliseconds; `beats_per_bar` is always 4 (4/4)
- `cues_from_position_marks(marks)` — converts `PositionMark` elements to `MusicValue::MemoryCue` facts; unknown types (e.g. Rekordbox 7 phrase markers, `Type=1`) are silently skipped
- `position_marks_from_cues(cues)` — exact inverse of `cues_from_position_marks`; round-trips losslessly for memory cues, hot cues, and loops
- `build_hash_to_location(entries)` — builds a `hash → export-URI` map that resolves all three hash forms a playlist may use (full `sha256:…`, 12-char short, 8-char legacy); short-hash collisions: first writer wins
- `location_to_hash(entries)` — reverse map: `export-URI → full content hash`

## Cue type mapping

| Rekordbox | MDMA |
|-----------|------|
| `Type=0`, `Num=-1` | `CueKind::Memory`, `index=None` |
| `Type=0`, `Num≥0` | `CueKind::Hot`, `index=Some(num)` |
| `Type=4` + `End` | `CueKind::Loop { length_ms }` |
| `Type=1` (phrase) | skipped |

## Build

```bash
cargo polylith cargo --profile dev build --package rekordbox-roundtrip
```

This component has no system-level dependencies beyond the workspace.
