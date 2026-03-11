# mdma-audio

File playback source service for MDMA (v0.1.0). Wraps `playback_engine` (Symphonia decoder + rubato resampler + PipeWire output) and exposes it as a source service via `stream_source_protocol` over NNG Rep0.

[Back to workspace README](../../README.md)

---

## What it does

- Listens on `ipc:///run/mdma/streams/audio.sock` for `StreamCommand` messages
- Resolves content hashes to file paths via the library IPC (`ipc:///run/mdma/library.sock`)
- Decodes FLAC, WAV, AIFF, and MP3 via Symphonia
- Upsamples all sources to 192 kHz using rubato (high-quality sinc resampler)
- Outputs a fixed-rate 192 kHz PipeWire stream — the DAC always sees full resolution
- Responds with `StreamResponse` (including `StreamPlaybackState` and `StreamTrackInfo`) as playback progresses

## IPC interface

- **Command socket:** `ipc:///run/mdma/streams/audio.sock` (NNG Rep0) — receives `StreamCommand`, responds with `StreamResponse`
- **Library socket:** `ipc:///run/mdma/library.sock` — used to resolve content hash → file path

`mdma-audio` is not exposed externally. `mdma-playback` connects to it directly over IPC.

## Build

```bash
cargo build --package mdma-audio
```

Cross-compile for Raspberry Pi:

```bash
cargo zigbuild --package mdma-audio --target aarch64-unknown-linux-musl --release
```

## Run

```bash
cargo run --package mdma-audio
```

The service requires PipeWire running on the host. On the Pi, WirePlumber is started via a runit service with a `context.exec` drop-in for headless operation.

`mdma-audio` must start after `mdma-library` and before `mdma-playback`. Runit service order: mdma-library → mdma-audio → mdma-playback.
