# REFACTORING.md — mdma polylith improvements

This document is an instruction set for **rust-architect**. It describes three
refactoring tasks to be executed in order, with verification steps after each.
Do NOT make changes beyond what is described here.

---

## 1. Why polylith is worth the extra crates

Before touching any code, here is the rationale to cite when the overhead of
new crates is questioned.

**Independent compilation units.** Splitting `playback_engine` means
`cargo build -p audio-decoder` compiles without PipeWire. On a dev machine
without a sound system, in CI, or when cross-compiling only the decoder, this
matters. Today, changing `resampler.rs` retriggers compilation of the entire
engine including the PipeWire bindings.

**Clearer dependency boundaries.** `pipewire_output.rs` and `pipewire_devices.rs`
pull in `pipewire` and `libspa-sys` — platform-specific, hard to cross-compile.
Once isolated in `audio-output`, no other component can accidentally gain that
dependency through playback_engine's public interface.

**Faster incremental builds.** When `pipeline.rs` in `library_service` changes,
only `library_ingestion` + `library_service` recompile — not every crate that
depends on `library_service`'s current monolithic interface.

**The stub pattern proves the architecture.** `library_service_stub` demonstrates
that the BDD project can replace a full component with a lightweight in-memory
implementation at the workspace level, with zero changes to test code. This is
polylith's core claim: implementations are swappable without changing callers.

**Each new crate is small.** `audio-resampler` is roughly 100 LOC. The cost is
one `Cargo.toml` and one `lib.rs`. The benefit is a named, independently
buildable and testable unit.

---

## 2. Split `playback_engine` into four components

### What stays in `playback_engine`

Retain these files unchanged:
- `lib.rs` — `PlaybackEngine` struct and its public API
- `mixer.rs` — `Mixer`
- `track.rs` — `Track` (including `TestSource` helper)
- `audio_config.rs` — `AudioOutputConfig`, `load_audio_config`, `save_audio_config`
- `error.rs` — `PlaybackError`

`playback_engine` gains three new workspace dependencies (see below).
Its `lib.rs` re-exports must be updated to pull types from the new crates.

### 2a. Extract `audio-decoder`

**Source file:** `components/playback_engine/src/source.rs`

**New component path:** `components/audio_decoder/`

**Public interface to expose:**
```rust
pub trait Source { ... }         // audio source interface
pub struct AudioSource { ... }   // Symphonia-based decoder
pub struct DecodedSegment { ... }
pub struct AudioSegment { ... }
pub struct SegmentIndex { ... }
pub const SEGMENT_SIZE: usize = 1024;
```

**Steps:**
1. `cargo polylith add component audio-decoder` (or create `components/audio_decoder/` manually with `Cargo.toml` + `src/lib.rs`)
2. Move the contents of `source.rs` into `components/audio_decoder/src/lib.rs`
3. Add dependencies to `components/audio_decoder/Cargo.toml`: `symphonia` (and any feature flags currently in playback_engine for symphonia)
4. In `components/playback_engine/Cargo.toml`, add `audio-decoder = { path = "../audio_decoder" }`
5. In `playback_engine/src/lib.rs`, replace `mod source;` with `use audio_decoder::*;` (or explicit re-exports)
6. Delete `components/playback_engine/src/source.rs`

**Validation:**
```bash
cargo build -p audio-decoder
cargo build -p playback-engine
```

### 2b. Extract `audio-output`

**Source files:** `components/playback_engine/src/pipewire_output.rs`,
`components/playback_engine/src/pipewire_devices.rs`

**New component path:** `components/audio_output/`

**Public interface to expose:**
```rust
pub struct PipewireOutput { ... }   // PipeWire stream manager
pub enum StreamCommand { SetActive(bool), Shutdown }
pub struct AudioSink { ... }        // device descriptor (id, name, description, max_sample_rate)
pub fn parse_sinks(...) -> ...
pub fn list_sinks(...) -> ...
```

**Steps:**
1. Create `components/audio_output/` with `Cargo.toml` + `src/lib.rs`
2. Move `pipewire_output.rs` and `pipewire_devices.rs` into `components/audio_output/src/`
   and re-export from `src/lib.rs`
3. Add `pipewire` and `libspa-sys` (and any other platform deps) to
   `components/audio_output/Cargo.toml`. Remove them from `playback_engine/Cargo.toml`.
4. In `components/playback_engine/Cargo.toml`, add `audio-output = { path = "../audio_output" }`
5. Update `playback_engine/src/lib.rs` re-exports
6. Delete `pipewire_output.rs` and `pipewire_devices.rs` from `playback_engine/src/`

**Validation:**
```bash
cargo build -p audio-output
cargo build -p playback-engine
```

### 2c. Extract `audio-resampler`

**Source file:** `components/playback_engine/src/resampler.rs`

**New component path:** `components/audio_resampler/`

**Public interface to expose:**
```rust
pub struct Resampler { ... }
// methods: new(), process_segment()
```

**Steps:**
1. Create `components/audio_resampler/` with `Cargo.toml` + `src/lib.rs`
2. Move contents of `resampler.rs` into `components/audio_resampler/src/lib.rs`
3. Add `rubato` dependency to `components/audio_resampler/Cargo.toml`. Remove from `playback_engine/Cargo.toml`.
4. In `components/playback_engine/Cargo.toml`, add `audio-resampler = { path = "../audio_resampler" }`
5. Update `playback_engine/src/lib.rs` re-exports
6. Delete `components/playback_engine/src/resampler.rs`

**Validation:**
```bash
cargo build -p audio-resampler
cargo build -p playback-engine
cargo test -p playback-engine
```

---

## 3. Split `library_service` — extract `library_ingestion`

### What to extract

**Source file:** `components/library_service/src/pipeline.rs`

This file contains a self-contained typestate pipeline with no dependency on
`LibraryService`'s in-memory state. It is safe to move without touching `service.rs`
or `ipc.rs`.

**New component path:** `components/library_ingestion/`

**Public interface to expose:**
```rust
pub struct InboxFile { ... }
pub struct ValidatedAudio { ... }
pub struct ExtractedTrack { ... }
pub struct IndexedTrack { ... }
pub enum IngestError { ... }
pub enum UploadSource { ... }
pub enum AudioFormat { ... }
// AudioFormat methods: from_extension(), extension(), is_ingestible()
// Pipeline methods: InboxFile::validate(), ValidatedAudio::extract_metadata(), ExtractedTrack::import()
```

**Steps:**
1. Create `components/library_ingestion/` with `Cargo.toml` + `src/lib.rs`
2. Move contents of `pipeline.rs` into `components/library_ingestion/src/lib.rs`
3. Copy across any dependencies that `pipeline.rs` uses (sha2, walkdir, symlink helpers, etc.) —
   check `library_service/Cargo.toml` and move only what `pipeline.rs` imports
4. In `components/library_service/Cargo.toml`, add `library-ingestion = { path = "../library_ingestion" }`
5. In `library_service/src/lib.rs`, replace `mod pipeline;` with
   `pub use library_ingestion::*;` (or import only what service.rs uses)
6. Delete `components/library_service/src/pipeline.rs`

**Do NOT move** `ipc.rs` or `service.rs`.

**Validation:**
```bash
cargo build -p library-ingestion
cargo build -p library-service
cd tests/bdd && cargo test
```

---

## 4. Extract `tests/bdd/` as `projects/bdd/` with a library stub

This task has five sub-steps that must be done in order.

### 4a. Move `tests/bdd/` → `projects/bdd/`

1. Create `projects/bdd/` (move the directory: `mv tests/bdd projects/bdd`)
2. Add a `[workspace]` table to `projects/bdd/Cargo.toml`:

```toml
[workspace]
members = ["."]
resolver = "2"

[workspace.dependencies]
# inherit shared deps from here if needed
```

3. Create `projects/bdd/.cargo/config.toml` to share the build cache:

```toml
[build]
target-dir = "../../target"
```

4. Remove `tests/bdd` from the root workspace `members` list in
   `/home/johlrogge/projects/modular-digital-music-array/Cargo.toml`

### 4b. Create `components/library_service_stub/`

The stub must present **the same package name** (`library-service`) and the same
public interface as `library_service`. The BDD tests import `library-service`;
swapping the path is all that changes.

**Public surface to implement** (derived from `library_service/src/lib.rs`,
`ipc.rs`, and `service.rs`):

```rust
// Re-export these to match library_service's public API:
pub use ipc::IpcServer;
pub use service::{LibraryService, ServiceError};
```

**Implementation rules:**
- `LibraryService` backed by an in-memory `HashMap` — no file scanning, no sha2 hashing
- `IpcServer` backed by nng (keep the same transport so BDD scenarios work unchanged)
- `ServiceError` must have the same variants (or at least be `From`-compatible with
  what BDD steps currently match against)
- No `fact_generator`, no `fact_writer`, no `pipeline` — those are real-world concerns
- Stub crate `Cargo.toml` must declare `name = "library-service"` (same package name
  as the real component)

### 4c. Wire the stub into `projects/bdd/`

In `projects/bdd/Cargo.toml`, under `[dependencies]`, replace the real
`library-service` path with the stub:

```toml
[dependencies]
library-service = { path = "../../components/library_service_stub" }
```

All other dependencies remain as-is (nng-transport, library-ipc-protocol, etc. still
resolve from the root workspace via relative paths).

### 4d. Update the justfile

In the root `justfile`, update the `bdd` recipe from:

```just
bdd:
    cargo test --package mdma-bdd --test cucumber -- -vv
```

to:

```just
bdd:
    cd projects/bdd && cargo test -- -vv
```

### 4e. Preservation rule

The existing test harness (`harness.rs`, `world.rs`, per-scenario NNG socket paths,
tempfile isolation, parallel execution) is correct. **Do not change it** unless the
stub's interface makes a change strictly necessary. Prefer adapting the stub to match
the harness, not the other way around.

---

## 5. Verification sequence

Run these commands in order after all four tasks are complete.

```bash
# 1. Full workspace build (confirms no broken dependencies)
cargo build --workspace

# 2. New components build in isolation
cargo build -p audio-decoder
cargo build -p audio-output
cargo build -p audio-resampler
cargo build -p library-ingestion
cargo build -p library-service-stub

# 3. Root workspace tests (unit + integration, excluding bdd)
cargo test --workspace --exclude mdma-bdd

# 4. BDD project (now uses stub)
cd projects/bdd && cargo test -- -vv

# 5. Clippy clean
cargo clippy --workspace -- -D warnings
cd projects/bdd && cargo clippy -- -D warnings
```

If step 4 fails because the stub does not implement a method the BDD harness
calls, fix the stub — not the harness.
