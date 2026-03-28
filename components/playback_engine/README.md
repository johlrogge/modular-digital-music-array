# playback_engine

Real-time audio playback component for MDMA. Decodes audio files via Symphonia, resamples to the configured output rate via rubato, and writes to a PipeWire stream. Manages a single track at a time (`Option<Track>`).

[Back to workspace README](../../README.md)

---

## What it does

- Decodes FLAC, WAV, AIFF, and MP3 via Symphonia
- Resamples decoded PCM to the configured output sample rate (default 192 kHz) using rubato's high-quality sinc resampler
- Outputs a fixed-rate PipeWire stream — the DAC always sees a consistent rate
- Manages a single loaded track (`Option<Track>`); loading a new track stops and replaces any existing one
- Exposes volume control in dBFS via the `Volume` type
- Supports hot-swap of the PipeWire output device without restarting the process
- Persists and restores the audio output device configuration from a JSON file

## API surface

```rust
PlaybackEngine::new(config_path)  // create engine, start mix thread
engine.load_track(&path).await    // decode and buffer a file
engine.play()                     // start/resume playback
engine.stop()                     // pause playback (track stays loaded)
engine.unload_track()             // discard loaded track
engine.set_volume(volume)         // set output level (dBFS)
engine.set_output(device_name)    // hot-swap PipeWire device
engine.list_outputs()             // enumerate available sinks
engine.position_ms()              // current position (None if nothing loaded)
engine.duration_ms()              // track duration  (None if nothing loaded)
engine.is_track_finished()        // true after last frame is consumed
engine.shutdown()                 // stop mix thread and release resources
```

`PlaybackEngine` is not `Send`. It is intended to be owned by a single-threaded async runtime (e.g. the `mdma-audio` server loop).

## Build

```bash
# Generate the dev profile manifest first, then build
cargo polylith profile build dev --no-build
cargo build --manifest-path profiles/dev/Cargo.toml --package playback-engine
```

Cross-compile for Raspberry Pi:

```bash
cargo polylith profile build production --no-build
cargo zigbuild --manifest-path profiles/production/Cargo.toml --package playback-engine --target aarch64-unknown-linux-musl --release
```

Building requires PipeWire and libspa development headers. These are provided automatically in the `devenv` shell.

## Notes

- The mix thread runs independently of the async runtime and uses an `Arc<AtomicBool>` with `Acquire`/`Release` ordering (correct on aarch64) to signal shutdown.
- The `Deck` type from `playback_primitives` is not used here. It is retained in that crate for future DJ-mode features.
