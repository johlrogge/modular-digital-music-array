# playback_engine

Real-time audio playback component for MDMA. Decodes audio files via Symphonia, resamples to the configured output rate via rubato, and writes to a PipeWire stream. Manages a single track at a time (`Option<Track>`).

[Back to workspace README](../../README.md)

---

## What it does

- Decodes FLAC, WAV, AIFF, and MP3 via Symphonia (runs in a dedicated `std::thread` to avoid IPC contention)
- Resamples decoded PCM with rubato's high-quality sinc resampler only when the source rate differs from the output rate
- Outputs a PipeWire stream at the source's native sample rate — 44.1 kHz files no longer forced to 192 kHz; PipeWire `allowed-rates` config enables graph-level rate switching
- Flushes the pipeline on track change — skip latency ~50 ms
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
cargo polylith cargo --profile dev build --package playback-engine
```

Cross-compile for Raspberry Pi:

```bash
cargo polylith cargo --profile production zigbuild --package playback-engine --target aarch64-unknown-linux-musl --release
```

Building requires PipeWire and libspa development headers. These are provided automatically in the `devenv` shell.

## Notes

- The decoder runs in a `std::thread` (not a tokio task) to avoid blocking the async runtime during IPC calls.
- The mix thread runs independently of the async runtime and uses an `Arc<AtomicBool>` with `Acquire`/`Release` ordering (correct on aarch64) to signal shutdown.
- The `Deck` type from `playback_primitives` is not used here. It is retained in that crate for future DJ-mode features.
