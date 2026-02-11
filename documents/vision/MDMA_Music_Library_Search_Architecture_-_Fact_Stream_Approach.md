# MDMA Music Library Search Architecture - Fact Stream Approach

## Core Architecture Decision: Facts Over Git

After extensive discussion, we've replaced git-based storage with an **immutable fact stream** inspired by Datomic. This provides:
- Time-travel capability through the fact stream itself
- Simpler operational model (append-only writes)
- Easy backup/restore (just the fact stream)
- No merge conflicts (facts are immutable)
- Rebuild aggregates anytime from facts

## Fact Stream Design

### Fact Structure (Rust)

```rust
#[derive(Serialize, Deserialize, Debug)]
struct Fact {
    entity: TrackId,        // Music fingerprint (Chromaprint)
    value: Value,           // Typed attribute + value
    timestamp: DateTime<Utc>,
    source: Source,         // User/system that created this fact
    operation: Operation,
}

#[derive(Serialize, Deserialize, Debug)]
enum Operation {
    Assert,   // Add/set value
    Retract,  // Remove value (for multi-valued attributes)
}

#[derive(Serialize, Deserialize, Debug)]
enum Value {
    Bpm(Bpm),
    Tag(Tag),
    Genre(Genre),
    Key(Key),
    FilePath(FilePath),
    Description(Description),
    Unknown { attribute: String, value: String },
}

// Newtypes for domain concepts
struct Bpm(u16);          // Stored as BPM × 100 (128.50 → 12850)
struct Tag(String);
struct Genre(String);
struct Key(String);
struct FilePath(PathBuf);
struct Description(String);
```

### Serialization Format: Ron (Rusty Object Notation)

Facts are stored as newline-delimited Ron:

```ron
(entity: "abc123def456", value: Bpm(12850), timestamp: "2024-01-15T10:30:00Z", source: "alice", operation: Assert)
(entity: "abc123def456", value: Tag("techno"), timestamp: "2024-01-15T10:31:00Z", source: "alice", operation: Assert)
(entity: "abc123def456", value: Tag("minimal"), timestamp: "2024-01-15T10:32:00Z", source: "alice", operation: Assert)
(entity: "abc123def456", value: Tag("techno"), timestamp: "2024-01-20T14:20:00Z", source: "alice", operation: Retract)
```

**Why Ron:**
- Rust-native with excellent serde support
- Human-readable for debugging
- UTF-8 strings handled naturally
- Positional fields keep it compact
- No TOML/YAML complexity

### Unknown Attributes Pattern

The `Unknown` variant handles attributes not yet known to the code:

```rust
// Fact stream might contain new attributes
(entity: "xyz789", value: Unknown(attribute: "energy_level", value: "high"), ...)

// Log these for observation
// When ready to use, promote to typed variant:
enum Value {
    // ... existing variants
    EnergyLevel(EnergyLevel),  // Newly promoted!
    Unknown { attribute: String, value: String },
}
```

Compiler forces handling new variants everywhere - gradual, safe schema evolution.

## Schema Definition (Rust Code)

Schema lives in Rust code, not config files:

```rust
pub fn schema() -> Schema {
    Schema::builder()
        .add("bpm", Cardinality::One, ValueType::Bpm)
        .add("tag", Cardinality::Many, ValueType::Tag)
        .add("genre", Cardinality::Many, ValueType::Genre)
        .add("key", Cardinality::One, ValueType::Key)
        .add("file_path", Cardinality::One, ValueType::FilePath)
        .add("description", Cardinality::One, ValueType::Description)
        .build()
}

enum Cardinality {
    One,   // Latest value wins (e.g., BPM)
    Many,  // Set of values (e.g., tags - needs Assert/Retract)
}
```

## Track Identity: Audio Fingerprinting

**Primary ID**: Chromaprint/AcoustID fingerprint
- Content-based: same audio = same ID
- Handles duplicates across sources
- Different editions fingerprint differently (as they should)

**Duplicate Resolution**:
```
/audio_cache/
  abc123def456.flac -> /bandcamp/Artist/Album/Track.flac  (winner)

# duplicates.txt component  
abc123def456 /youtube/downloads/same_track.mp3
abc123def456 /soundcloud/Artist - Track.m4a
```

Quality scoring (on-demand):
- Source priority: Bandcamp FLAC > others
- File size, sample rate, bitrate as tiebreakers

## Aggregation Strategy

### In-Memory Indexes

Aggregates are **eventually consistent views** of the fact stream:

```rust
struct MusicIndex {
    // Entity -> Attributes (for cardinality-one)
    single_valued: HashMap<TrackId, HashMap<Attribute, Value>>,
    
    // Attribute -> Values -> Entities (for cardinality-many)
    multi_valued: HashMap<Attribute, BTreeMap<Value, HashSet<TrackId>>>,
    
    // Attribute -> Entity -> Values (for cardinality-many, entity-first queries)
    by_entity: HashMap<TrackId, HashMap<Attribute, HashSet<Value>>>,
}
```

### Aggregation Logic

```rust
fn apply_fact(index: &mut MusicIndex, fact: &Fact, schema: &Schema) {
    let attr_def = schema.get(&fact.value.attribute_name());
    
    match (attr_def.cardinality, fact.operation) {
        (Cardinality::One, _) => {
            // Latest wins - just overwrite
            index.single_valued
                .entry(fact.entity)
                .or_default()
                .insert(attr_def.name, fact.value.clone());
        }
        (Cardinality::Many, Operation::Assert) => {
            // Add to set
            index.multi_valued
                .entry(attr_def.name)
                .or_default()
                .entry(fact.value.clone())
                .or_default()
                .insert(fact.entity);
        }
        (Cardinality::Many, Operation::Retract) => {
            // Remove from set
            if let Some(entities) = index.multi_valued
                .get_mut(&attr_def.name)
                .and_then(|m| m.get_mut(&fact.value)) 
            {
                entities.remove(&fact.entity);
            }
        }
    }
}
```

### Startup Strategy

**Start Simple**: Rebuild from facts on startup
```rust
fn load_index(fact_stream: &Path) -> Result<MusicIndex> {
    let mut index = MusicIndex::new();
    
    for line in BufReader::new(File::open(fact_stream)?).lines() {
        let fact: Fact = ron::from_str(&line?)?;
        apply_fact(&mut index, &fact, &schema());
    }
    
    Ok(index)
}
```

**Optimize Later**: Memory-mapped pre-built indexes when startup time matters
```rust
// Fast path
if let Ok(index) = memmap_index("aggregates/index.dat") {
    return Ok(index);
}

// Fallback - rebuild
let index = rebuild_from_facts()?;
save_index(&index)?;
Ok(index)
```

## Time Travel

Query historical state by filtering facts:

```rust
fn index_at_time(facts: &[Fact], timestamp: DateTime<Utc>) -> MusicIndex {
    facts.iter()
        .filter(|f| f.timestamp <= timestamp)
        .fold(MusicIndex::new(), |mut idx, fact| {
            apply_fact(&mut idx, fact, &schema());
            idx
        })
}
```

Time travel is a **rare query** - optimize for current state, rebuild historical views on-demand.

## Playlist Mirroring & Temporary Collections

### Temporary Facts for Parties

```rust
// Party-specific facts have distinct source
(entity: "temp123", value: FilePath("/tmp/party/track.flac"), timestamp: "2024-03-15T20:00:00Z", source: "party_2024_03_15", operation: Assert)

// After party, filter out temp facts
let permanent_index = facts.iter()
    .filter(|f| !f.source.starts_with("party_"))
    .fold(MusicIndex::new(), |idx, f| { apply_fact(idx, f); idx });
```

### Playlist Mirroring (via song.link)

1. Fetch playlist from YouTube/SoundCloud
2. Resolve tracks via song.link API to Bandcamp/other sources
3. Download high-quality versions
4. Generate fingerprints
5. Write facts with source tracking:

```ron
(entity: "abc123", value: Tag("from_playlist:summer_vibes"), timestamp: "2024-01-15T10:00:00Z", source: "playlist_sync", operation: Assert)
(entity: "abc123", value: FilePath("/music/Artist/Track.flac"), timestamp: "2024-01-15T10:01:00Z", source: "bandcamp", operation: Assert)
```

Track playlist membership via tags, detect updates by re-syncing and comparing facts.

## Conflict Resolution (Manual Merge UI)

For potential duplicates (similar metadata, different fingerprints):

```rust
struct ConflictCandidate {
    track_a: TrackId,
    track_b: TrackId,
    similarity_score: f32,
    metadata_comparison: Vec<(Attribute, Value, Value)>,
}
```

UI shows side-by-side comparison:
- Preview both tracks
- Show metadata differences
- User chooses: "Same track" (merge) or "Different tracks" (keep both)
- On merge: retract losing track's facts, mark as duplicate

Make conflict resolution **fast and addictive** - Tinder for music duplicates!

## Data Characteristics

### Scale Estimates
- 10,000 tracks × 20 facts each × 100 bytes = **20MB fact stream**
- 100,000 tracks × 50 facts each × 100 bytes = **500MB fact stream**

Fact streams fit comfortably in RAM, git performance concerns eliminated.

### Access Patterns
- **Fact stream**: Append-only writes, sequential reads for aggregation
- **Aggregates**: Fast lookups, range queries (BPM 125-130), set membership (all techno tracks)

Different data structures for different purposes:
- Fact stream: Simple append, newline-delimited
- Aggregates: HashMap/BTreeMap optimized for queries

## Implementation Phases

### Phase 1: Core Fact Stream
1. Fact structure and Ron serialization
2. Bandcamp FLAC crawler with fingerprinting
3. Basic fact writing (file paths, detected BPM, tags from metadata)
4. Simple aggregation to in-memory index

### Phase 2: Query Interface
1. Bevy + Tokio server
2. Basic search queries (by BPM range, by tag, by artist)
3. TUI client for browsing
4. REPL for programmatic queries

### Phase 3: Advanced Features
1. Conflict detection and manual merge UI
2. Playlist mirroring (song.link integration)
3. Temporary collection support
4. Time travel queries

## Key Design Principles

1. **Immutable Facts**: Never modify history, only append
2. **Separation of Concerns**: Facts (data) vs Schema (policy) vs Aggregates (query optimization)
3. **Eventual Consistency**: Aggregates can be rebuilt anytime
4. **Explicit Unknowns**: Unknown variant handles schema evolution gracefully
5. **Operational Simplicity**: Text files, no complex databases, easy debugging
6. **Type Safety**: Rust compiler enforces handling of new attributes

## Backup & Recovery

**Backup**: Copy fact stream file
**Recovery**: Rebuild aggregates from fact stream
**Corruption**: Human-readable Ron format allows manual fixes with text editor

No database expertise required - just text file operations.