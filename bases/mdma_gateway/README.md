# mdma_gateway

Single API gateway for MDMA. Listens on TCP port 5555 and routes requests to the appropriate internal NNG IPC service (library, playback, sources). This is the only port exposed externally on the Pi.

## Build

```bash
cargo build --package mdma-gateway
```

## Run

```bash
cargo run --package mdma-gateway -- \
  --listen tcp://0.0.0.0:5555 \
  --library-socket ipc:///run/mdma/library.sock \
  --playback-socket ipc:///run/mdma/playback.sock
```

## Back to workspace

[Workspace README](../../README.md)
