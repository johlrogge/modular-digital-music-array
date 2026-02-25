# mdma-gateway

Version: **0.3.3**

Single TCP entry point for MDMA. Listens on port 5555, inspects the request envelope, and routes to the appropriate internal IPC service. This is the only externally exposed port on the Pi (besides port 80 for the web console).

[Back to workspace README](../../README.md)

---

## What it does

- Accepts NNG TCP connections on `tcp://0.0.0.0:5555`
- Routes `LibraryRequest` messages to `ipc:///run/mdma/library.sock`
- Routes `PlaybackCommand` messages to `ipc:///run/mdma/playback.sock`
- Routes `SourceRequest` messages to the appropriate socket in `/run/mdma/sources/` (auto-discovered)
- Handles `ListSources` to enumerate all connected source services
- Bridges the playback event socket (IPC Pub0) to TCP port 5556 for external pub/sub clients

## Architecture

```
External clients (laptop CLI, web console)
          |
          | tcp://0.0.0.0:5555
          v
    mdma-gateway
          |
     -----+-------+---------------------------+
     |            |                           |
     v            v                           v
mdma-library  mdma-playback   /run/mdma/sources/*.sock
(ipc fixed)   (ipc fixed)     (auto-discovered)

Event bus: tcp://0.0.0.0:5556 (bridges playback-events IPC)
```

Sources are auto-discovered by scanning `/run/mdma/sources/` for `*.sock` files. Adding a new source service requires only placing its socket in that directory.

## Build

```bash
cargo build --package mdma-gateway
```

## Run

```bash
cargo run --package mdma-gateway
```

On the Pi, the gateway is managed by runit. The run script is the single source of truth for socket addresses.
