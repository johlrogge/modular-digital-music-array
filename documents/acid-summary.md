# ACID — Audio Collection Indexing Database

> **Status:** Architecture complete · ⛔ Blocked — awaiting service discovery

ACID is a long-running service that acts as the single writer for MDMA's music metadata. It accepts facts from multiple producers over inter-process communication, appends them to an immutable chronological fact stream stored as JSON lines, and builds in-memory aggregated indexes for fast querying.

Philosophically inspired by Datomic and Rich Hickey's ideas about immutable data. The name is also a nod to the classic database guarantee concept — a subtle joke given the broader project naming conventions.

---

## Core Principles

1. **Fact stream is sacred** — once written, facts are never modified. Only append.
2. **Timestamp ordering** — facts must arrive in chronological order. Out-of-order batches are rejected in full.
3. **Deduplication is separate** — download first, deduplicate later in a separate parallel process.
4. **Correctness over throughput** — low write frequency is expected. Optimize for reliability.
5. **Content-based identity** — a track's identity is its SHA256 hash, not its file path. Moves are repairable.

---

## Architecture

```
┌─────────────────┐                    ┌──────────────┐
│ Download Client │──── Facts/IPC ────▶│              │
└─────────────────┘                    │     ACID     │──▶ Fact Stream (JSON lines)
                                       │   Service    │
┌─────────────────┐                    │              │──▶ In-memory Indexes (ECS)
│ Audio Analyzer  │──── Facts/IPC ────▶│              │
│    (future)     │                    └──────────────┘
└─────────────────┘
```

All producers communicate via `nng` (nanomsg-next-generation) using a request/reply pattern.

---

## Timestamp Ordering Strategy

Facts are timestamped by the client. On receiving a batch, ACID:

1. Sorts facts by timestamp (stable sort)
2. Rejects the entire batch if any fact is older than the stream's `latest_timestamp`
3. Writes atomically on success and updates `latest_timestamp`

Clock skew is handled bluntly: reject and instruct the client to synchronise via NTP. Rejected batches include the current stream tip so clients know exactly where they need to be.

---

## Fact Value Types

ACID imposes no schema on fact values. Any producer can write any fact about any entity — the fact space is open and evolves independently of its consumers. New fact types can be introduced at any time without coordinating with or breaking existing clients.

**Clients must silently ignore unknown fact types.** This is a hard contract. If a client fails on encountering an unknown fact, it is the client that is broken, not the fact stream.

The following are illustrative examples of the kinds of facts that might be stored, not an exhaustive or fixed schema:

| Category | Examples |
|---|---|
| **Identity** | `FilePath`, `AudioFingerprint`, `ContentHash` (SHA256) |
| **Core Metadata** | `Title`, `Artist`, `Album`, `Year`, `Genre`, `Label`, `Isrc` |
| **Audio Analysis** | `Bpm`, `BpmConfidence`, `Key`, `KeyConfidence`, `Duration`, `ReplayGain`, `DynamicRange` |
| **User Data** | `Tag`, `Rating`, `Energy`, `Color`, `Comment` |
| **Performance** | `PlayCount`, `LastPlayed`, `CuePoint`, `LoopRegion` |
| **Provenance** | `DownloadDate`, `ReleaseDate`, `PurchaseUrl`, `StreamingUrl`, `UserVerified` |

---

## Fact Source Structure

Each fact carries a structured `FactSource` with three fields: `tool` (which binary wrote it), `version` (for reproducibility), and `origin` — a typed enum covering `Bandcamp`, `Beatport`, `FilesystemScan`, `AudioAnalysis`, and others.

This is the "hybrid" approach: more structured than raw strings, simpler than full Datomic-style transaction entities.

---

## Implementation Phases

### Phase 1 — Core Service _(Week 1)_

Inter-process communication server via `nng`, timestamp ordering logic, batch write/reject protocol, integration tests.

**Deliverable:** Service accepts fact batches and rejects out-of-order writes.

### Phase 2 — Downloader Integration _(Week 2)_

Add `AcidClient` to `media-downloader`. Compute content hash post-download. Detect origin from URL. Write facts automatically on each download.

**Deliverable:** `download-cli` writes facts to ACID automatically.

### Phase 3 — Deduplication Detection _(Week 3–4)_

Read fact stream, build `content_hash → Vec<FilePath>` mapping. Report duplicates. No automatic deletion — identification only.

**Deliverable:** Duplicate detection report with no data loss risk.

### Phase 4 — Query & Aggregation _(Week 4–5)_

In-memory indexes via `stainless-facts`. BPM range, genre set, artist map. Query protocol over inter-process communication. `acid-query` command-line tool.

**Deliverable:** Search library by artist, BPM range, genre in <100ms.

---

## Open Questions

| Question | Recommendation |
|---|---|
| Clock synchronisation — skewed clients? | Reject batch, tell client to sync via NTP |
| Service unavailable during download? | Downloader fails fast with clear error |
| Batch size limits? | Maximum 1,000 facts per batch |
| Concurrent writers? | File locking — one writer at a time, client retries on rejection |
| Fact stream too large? | Not a concern yet — 100k tracks ≈ 200MB. Compaction is a future problem. |

---

## Scale Estimates

| Metric | Value |
|---|---|
| Average facts per track | ~20 |
| Fact stream at 10,000 tracks | ~200,000 facts / ~20MB |
| Metadata partition (planned) | 88GB |
| Target query latency | <100ms |

---

*MDMA — Modular Distributed Music Architecture · stainless-facts · nng · rayon · lofty · rusty-chromaprint*
