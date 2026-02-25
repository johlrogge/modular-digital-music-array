# mdma_bandcamp

Bandcamp download service for MDMA. Fetches purchased releases from Bandcamp using stored cookies, stages downloads, and moves completed files to the music inbox. Exposes an NNG IPC source socket for integration with the gateway.

## Build

```bash
cargo build --package mdma-bandcamp
```

## Run

```bash
cargo run --package mdma-bandcamp -- \
  --cookies /etc/mdma/bandcamp-cookies.json \
  --downloads-dir /music/downloads \
  --inbox-dir /music/inbox
```

## Back to workspace

[Workspace README](../../README.md)
