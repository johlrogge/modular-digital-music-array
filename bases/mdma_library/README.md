# mdma-library

Version: **0.6.0**

Music library service for MDMA. Manages a content-addressed blob store, indexes track metadata as an immutable fact stream, and serves search and lookup requests over NNG IPC.

[Back to workspace README](../../README.md)

---

## What it does

- Watches `/music/inbox/` for new audio files; ingests on arrival
- Stores audio in content-addressed blobs under `/music/blobs/<hash-prefix>/<full-hash>.<ext>`
- Tracks all metadata — artist, title, album, BPM, key, play history — as typed facts in `/metadata/facts.jsonl`
- Records an `AddedAt` timestamp for every track at ingest time; queryable and sortable via `--added` in the CLI
- Album art cache: serves album-level cover art as a fallback when a track has no embedded art
- Facts are immutable and append-only (`stainless_facts`). The entire library state can be rebuilt from `facts.jsonl` at any time
- Fact writes are delegated to `mdma-acid` (`ipc:///run/mdma/acid.sock`) — the library service does not write directly to `facts.jsonl`
- Serves search, list, get, and facts queries via NNG IPC
- Accepts file ingest requests (inbox management) from the CLI and console

## Storage layout

```
/music/
    inbox/              # drop files here — watched by mdma-library
    downloads/          # staging area for in-progress downloads
    blobs/              # content-addressed audio storage
        a1/
            b2c3d4...sha256.flac

/metadata/
    facts.jsonl         # main fact stream — source of truth for all metadata
```

## IPC interface

Listens on `ipc:///run/mdma/library.sock`. Accepts `LibraryRequest` messages. Not directly accessible externally — all traffic is routed through the gateway on TCP port 5555.

## Build

```bash
cargo build --package mdma-library
```

## Run

```bash
cargo run --package mdma-library
```
