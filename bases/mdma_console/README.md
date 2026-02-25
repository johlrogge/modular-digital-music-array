# mdma-console

Version: **0.3.3**

Web management console for MDMA. Built with Axum and Askama. Provides a browser UI for everything the CLI does — player controls, queue management, library search, Bandcamp sync, file upload, and track export.

[Back to workspace README](../../README.md)

---

## What it provides

- Now playing display — track, artist, album, BPM, key, duration
- Player controls — play queue, stop, skip
- Queue view — current queue with remove buttons, clear all
- Library search — artist, BPM, key filters, add-to-queue from results
- Live updates via SSE (server-sent events bridged from the playback pub/sub)
- ZIP/audio upload — drag and drop or file picker; triggers ingest automatically
- Track export — select tracks and download as AIFF or WAV
- Bandcamp sync — trigger collection sync, view download status
- Bandcamp configuration — set cookies and username at `/bandcamp/config`
- Package management — view and update installed MDMA packages
- Inbox management — view pending files, trigger ingest

## Access

On a provisioned Pi: `http://mdma-909.local`

## Port binding

The console binds to port 80, which requires `cap_net_bind_service` on the binary. This is set during provisioning. If running outside the Pi, either run as root or use a port above 1024.

## Build

```bash
cargo build --package mdma-console
```

## Run

```bash
cargo run --package mdma-console
```
