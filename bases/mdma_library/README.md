# mdma_library

Music library service for MDMA. Scans the music directory, indexes track metadata, and serves search and lookup requests over an NNG IPC socket.

## Build

```bash
cargo build --package mdma-library
```

## Run

```bash
cargo run --package mdma-library
```

Listens on `ipc:///run/mdma/library.sock` by default.

## Back to workspace

[Workspace README](../../README.md)
