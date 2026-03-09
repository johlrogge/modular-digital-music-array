# mdma-playback

Real-time audio playback server for MDMA. Decodes audio using Symphonia, resamples to 192 kHz with rubato, and outputs via PipeWire to the iFi USB DAC.

[Back to workspace README](../../README.md)

---

## What it does

- Accepts play/stop/queue commands over NNG IPC from the gateway
- Decodes FLAC, WAV, AIFF, and MP3 via Symphonia
- Upsamples all sources to 192 kHz using rubato (high-quality sinc resampler)
- Outputs a fixed-rate 192 kHz PipeWire stream — the DAC always sees full resolution
- Probes the iFi DAC's maximum supported rate at startup; falls back to 192 kHz if probing fails
- Publishes playback events (TrackStarted, TrackEnded, TrackStopped, QueueChanged) on a Pub0 socket for live clients
- Writes `Played` and `Skipped` facts to the fact stream on track end and manual stop
- Persists the queue to `queue.json` on every mutation; restores on restart

## IPC interface

Listens on `ipc:///run/mdma/playback.sock`. Accepts `PlaybackCommand` messages (protobuf via `media_protocol`). Not directly accessible externally — all traffic is routed through the gateway on TCP port 5555.

Event socket: `ipc:///run/mdma/playback-events.sock` (Pub0). The gateway bridges this to TCP port 5556.

## Build

```bash
cargo build --package mdma-playback
```

Cross-compile for Raspberry Pi:

```bash
just playback-cross
just deploy-playback
```

## Run

```bash
cargo run --package mdma-playback
```

The service requires PipeWire running on the host. On the Pi, WirePlumber is started via a runit service with a `context.exec` drop-in for headless operation.
