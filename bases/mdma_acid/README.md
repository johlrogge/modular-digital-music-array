# mdma-acid

Standalone fact-writing service for MDMA. Owns the append-only fact stream (`facts.jsonl`) and accepts batched write and read requests over NNG IPC. Extracted from the library service so that any other service (playback, bandcamp, future analysers) can write facts without going through the library.

ACID stands for Append-only Content-Indexed Database.

[Back to workspace README](../../README.md)

---

## What it does

- Listens on `ipc:///run/mdma/acid.sock` (and optionally a TCP address)
- Accepts `WriteFacts` requests: a batch of typed facts for a named entity (e.g. `track:sha256:abc123`)
- Appends facts to `/metadata/facts.jsonl` using `stainless_facts` — nothing is ever overwritten
- Accepts `ReadStream` requests: returns a paginated slice of the raw fact stream
- Domain-agnostic: fact values and sources are arbitrary JSON; ACID does not interpret them

## Protocol

Defined in `components/acid_protocol`. Three request variants:

| Request | Description |
|---------|-------------|
| `Ping` | Liveness check — returns `Pong` |
| `WriteFacts { entity, facts }` | Append a batch of facts for an entity |
| `ReadStream { after_line, limit }` | Read up to `limit` raw lines from the stream starting after `after_line` |

The client library is in `components/acid_client`.

## IPC interface

- IPC: `ipc:///run/mdma/acid.sock`
- Optional TCP (for remote access): configured via `--tcp` flag, e.g. `tcp://0.0.0.0:5560`
- Routed through the gateway on TCP port 5555 alongside all other services

## Storage

```
/metadata/
    facts.jsonl     # append-only fact stream — written exclusively by mdma-acid
```

## Build

```bash
cargo build --package mdma-acid
```

## Run

```bash
cargo run --package mdma-acid

# Custom metadata directory
cargo run --package mdma-acid -- --metadata-dir /metadata

# Also expose a TCP listener
cargo run --package mdma-acid -- --tcp tcp://0.0.0.0:5560
```

On the Pi, mdma-acid is managed by runit and starts before mdma-library and mdma-playback.
