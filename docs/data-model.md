# MDMA Data Model: Facts and Indexes

## Facts

MDMA stores music metadata as an append-only fact stream (via `stainless-facts`).
Each fact is a triple:

```
(entity, attribute, value, transaction)
   ↓          ↓         ↓         ↓
ContentHash  MusicValue  MusicValue  FactSource
```

Example facts for a single track:

```
sha256:abc…  Title         "Vacker"
sha256:abc…  Artist        "Rymden"
sha256:abc…  BPM           128
sha256:abc…  Key           "C Major"
sha256:abc…  MainGenre     "Electronic"
sha256:abc…  StyleDescr…   "Ambient"
sha256:abc…  StyleDescr…   "Driving"
sha256:abc…  Label         "Napalm Records"
sha256:abc…  Source        "bandcamp"
```

Multiple facts of the same type can exist for one entity (e.g. `StyleDescriptor`).
The fact stream is never mutated — corrections are new facts (retraction support
is deferred to stainless-facts).

---

## Datomic Index Patterns

Datomic's four indexes apply directly to MDMA's fact stream:

| Index | Sort order | Use case in MDMA |
|-------|-----------|-----------------|
| **EAVT** | Entity → Attribute → Value → Tx | "All facts about track X" — the pull pattern used by `GetFacts`. MDMA rebuilds this in memory by aggregating facts keyed on `ContentHash`. |
| **AEVT** | Attribute → Entity → Value → Tx | "All tracks that have a BPM fact" — useful for listing all indexed fields, or finding sparse tracks. |
| **AVET** | Attribute → Value → Entity → Tx | "All tracks where BPM = 128" — range queries, the query index. MDMA's `fact_index: HashMap<fact_type, HashSet<String>>` is a lightweight AVET approximation: it answers "does value V exist for attribute A?" but not "which entity has it". |
| **VAET** | Value → Attribute → Entity → Tx | Reverse references (e.g. "all tracks on label X"). Deferred — useful once label/artist entities exist. |

---

## Current Implementation

### EAVT in memory — `IndexedTrackInfo`

On startup, `load_tracks_from_facts` scans the entire fact stream and folds it
into a `Vec<IndexedTrackInfo>` keyed on `ContentHash`. This is an in-memory EAVT
reconstruction. It supports:

- `GetTrack` / `GetFacts` — O(n) scan with hash prefix match
- `Search` — full linear scan via `matches_query` in `library-search`

Performance note: linear scan is adequate up to ~10 000 tracks (~20 ms at
1 µs/track). Beyond that, a proper EAVT B-tree index or SQLite would pay off.

### AVET approximation — `fact_index`

```rust
fact_index: HashMap<String, HashSet<String>>
//           ^fact_type   ^all distinct values
```

This answers:
- `HasFact { fact_type, value }` — O(1) existence check
- `HasFacts { fact_type, values }` — O(k) batch check
- `GetFactValues { fact_type }` — return sorted distinct values for a type

It does **not** map back to entities. To do "all tracks with genre = Techno" you
still scan `IndexedTrackInfo`. To answer that query in O(log n) you would need a
proper AVET index mapping `(fact_type, value) → Vec<ContentHash>`.

---

## When to add explicit indexes

| Scale | Approach |
|-------|----------|
| < 10 000 tracks | Current in-memory scan — fast enough, zero overhead |
| 10 000–100 000 | AVET map: `HashMap<(String, String), Vec<ContentHash>>` |
| > 100 000 | SQLite with covering indexes, or embedded RocksDB |

The fact stream (JSONL file) remains the source of truth regardless of which
index tier is used. Indexes are always derived from facts and can be rebuilt.

---

## Query system (`library-search` crate)

`TrackQuery` is a composable filter evaluated against `TrackFields` by
`matches_query`. All non-None fields use implicit AND semantics.

```
any_text  → OR across title/artist/album/label/genre
artist    → StringQuery (Contains | Initialism | Regex)
bpm       → NumericQuery (Exact | Range | Tolerance)
key       → KeyQuery (Steps | Span) over a typed Key — Camelot wheel math
duration  → DurationQuery (Exact | AtLeast | AtMost | Range | WithPrecision)
genre     → StringQuery against MainGenre
style     → StringQuery against any StyleDescriptor
label     → StringQuery
year      → NumericQuery
source    → exact string (case-insensitive)
```

See `components/library_search/src/query.rs` for full type definitions.

### Key query grammar

- `8A` exact; `8A+-2`, `8A+2`, `8A-1` steps round the Camelot wheel; `6A..10B` a span.
- Steps are **Straight** by default, a T shape: up to n wheel steps in the centre's
  letter, or exactly the relative key (8A <-> 8B), never both. `8A+-2` is
  6A 7A 8A 9A 10A 8B.
- The `d` suffix (`8A+-2d`) is distance only, any path: a wheel step and an A/B switch
  each cost 1, so it adds the diagonals (7B, 9B). Works on `+-`, `+` and `-`.
- `,+7` after a steps term adds an exact move from the centre, keeping the letter
  (`8A+-1,+7` adds 3A, the power jump). Any signed offset is accepted (`,-7`).

**Amendment 2026-11-17:** `8A+-2` used to mean any path; it now means Straight, and the
old meaning is spelled `8A+-2d`. At +-1 the two shapes give the same keys (7A 9A 8B),
so `8A+-1` is unchanged. Spans are unchanged.
