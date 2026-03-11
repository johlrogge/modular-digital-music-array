# mdma-playback

Pure queue manager for MDMA (v0.5.0). Accepts play/stop/queue commands over NNG IPC and drives audio source services via `StreamClient`. Has no direct dependency on `playback_engine` — audio decoding and output is handled by `mdma-audio` and other future sources.

[Back to workspace README](../../README.md)

---

## What it does

- Accepts play/stop/queue commands over NNG IPC from the gateway
- Drives audio sources (e.g. `mdma-audio`) via `StreamClient` using `stream_source_protocol`
- Queue entries carry a `source` field (default `"audio"`) identifying which source service handles playback
- Publishes playback events (TrackStarted, TrackEnded, TrackStopped, QueueChanged) on a Pub0 socket for live clients
- Writes `Played` and `Skipped` facts to the fact stream on track end and manual stop
- Persists the queue to `/metadata/queue.json` on every mutation; restores on restart

## IPC interface

Listens on `ipc:///run/mdma/playback.sock`. Accepts `PlaybackCommand` messages (protobuf via `media_protocol`). Not directly accessible externally — all traffic is routed through the gateway on TCP port 5555.

Event socket: `ipc:///run/mdma/playback-events.sock` (Pub0). The gateway bridges this to TCP port 5556.

Connects to `mdma-audio` at `ipc:///run/mdma/streams/audio.sock` (and any other sources registered under `/run/mdma/streams/`).

## Service startup order

`mdma-audio` must be running before `mdma-playback` starts. Full order: mdma-library → mdma-audio → mdma-playback.

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

The service requires `mdma-audio` (or another source service) to be running and listening on its IPC socket.
