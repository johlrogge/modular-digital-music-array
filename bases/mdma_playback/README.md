# mdma_playback

Real-time audio playback server for MDMA. Decodes audio using Symphonia and outputs via PipeWire. Exposes an NNG IPC socket and accepts commands from the gateway or CLI.

## Build

```bash
cargo build --package mdma-playback
```

## Run

```bash
cargo run --package mdma-playback
```

Listens on `ipc:///run/mdma/playback.sock` by default.

## Back to workspace

[Workspace README](../../README.md)
