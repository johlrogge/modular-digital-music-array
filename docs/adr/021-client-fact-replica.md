# ADR-021: Client fact replica and completion cache

## Status

Proposed, 2026-11-14

Amends ADR-016 §2-3 (this is the first use of the per-node replica; it answers open question 5) and ADR-017 §6.9 (the migration replaces the log epoch). Related: ADR-001, ADR-009, ADR-011, ADR-014 (§14), ADR-020.

## Context

- hicli completes on every keystroke and gets only the raw word being completed (QUICKREF §9). Completion must never block on the network, so it cannot ask the node.
- ACID's `ReadStream` pages by cursor `line:<n>`. A cursor past the end returns nothing, so a rewritten or shorter log looks up to date to a client that holds an old cursor. ADR-017 §6 rewrites the log in place; line positions survive, content does not.
- ADR-016 already decided per-node, read-only replicas, folded with `library_projection` (§2.2, §3.1). Nothing implements them yet. Completion needs one, so it becomes the first user.
- The gateway forwards every ACID request, writes included (before phase 1). A client-side replica needs only reads over the network.

## Decision

### 1. One raw-line replica per node

- The client keeps the node's raw fact-log lines under `dirs::cache_dir()/mdma/nodes/<node-id>/`:
  - `facts.jsonl`: the log lines, as served.
  - `meta.json`: `epoch`, `lines`, `last_line_sha256`, `synced_at`, `last_attempt`, `failures` (failed attempts in a row, for the backoff), `hosts`.
  - `probe.json`: the playlist and source names probed at each sync (§5).
  - `completion.json`: derived (§4).
  - `sync.lock`: the flock file (§6). It is never deleted: a reset takes the lock, removes everything else and fails as busy while a sync holds it.
- The node id comes from `Identify` (ADR-009). A node that cannot be identified gets no replica.
- `meta.json` is the commit point. A sync appends to `facts.jsonl` and fsyncs, then writes `meta.json` with tmp+rename. A crash between the two leaves lines past `meta.lines`.
- On open, a torn tail is truncated to `meta.lines`. A shorter file, a committed region that is not UTF-8 or a checksum mismatch on the last line wipes the replica and refetches. Per-line parse failures are not the store's business. The fold keeps a line it cannot read (a node newer than this client) in the replica, leaves it out of `completion.json` and counts it there (`unreadable`); `replica status` reports "N lines this mdma cannot read; upgrade mdma". The replica is not discarded for it: that would refetch the whole log at every sync and never back off.

### 2. Log identity: epoch and pinned reads

- ACID keeps `metadata_dir/facts.epoch`, a random id. Any tool that rewrites the log replaces it.
- `StreamChunk` carries `epoch` and `total_lines`.
- `ReadStream` takes an optional `LinePin { line, sha256 }`: the client's last line and its digest.
- A pinned read whose pinned line is missing (which includes a cursor past the end) or differs answers `StreamDiverged`. Reads from line 0 are unpinned. The replica resets and refetches from line 0.
- The epoch is read at ACID start. Anything that rewrites the log does so with ACID stopped (ADR-017 §6.9).
- A change of epoch resets the replica the same way.
- A node that sends no epoch is too old. The client reports "node too old for the replica", keeps no replica and does not fall back silently.
- Pages are 5,000 lines.

### 3. The gateway forwards only ACID reads

- Once phase 1 lands, writes to ACID go through the library or a local socket, never over the network gateway. The gateway rejects ACID requests that are not reads.
- A replica therefore needs no write path, and an ACID upgrade does not widen what a remote client can do.

### 4. One projection

- The fold is `library_projection`, the component the library service uses. Polylith, one implementation, no trait. The CLI depends on it.
- ADR-014 §14's split-point property test covers it, so the client and the library cannot drift.
- After each sync the client rebuilds `completion.json` from the projection: marks, artists, titles, albums, labels, genres, styles and origins. It is small (about 100 KB today) and is the only file completion reads besides `meta.json`.

### 5. Non-fact data is probed, not replicated

- Playlists are files and sources are sockets, not facts. Each sync probes both (`PlaylistList`, the source list) and stores the answer in `completion.json`.
- Probe data is served for up to 24 h, then dropped.
- **Decided (Joakim):** this data does not become facts to suit completion.

### 6. Refresh is always detached

- **Decided (Joakim):** nothing syncs inline.
- Completion, and commands after they finish, spawn `mdma replica sync --quiet` when the last attempt is older than 30 s, or after a write. The child runs under `setsid` with stdin, stdout and stderr on null, so it holds no shell pipe and completion does not wait for it.
- `last_attempt` is touched before the spawn, so the 30 s window holds across shells.
- After failures the interval backs off from 30 s to 5 min.
- One sync per node: a non-blocking flock on `sync.lock`. A second sync exits at once.
- The node for completion (and for `replica status` and `reset`) is found offline, in this order: (1) an explicit `--node`, which must match a replica's host or id and has no fallback; (2) on the node itself, the replica synced as `localhost`; (3) `MDMA_NODE` matched against `meta.hosts`; (4) the id in `node.json`, only when `MDMA_NODE` is unset or names that node's host. When several replicas list a host, the most recently synced wins. Completion does no mDNS lookup. The sync checks the node id from `Identify` first, and a mismatch means a different node behind that name.

### 7. Titles complete as search terms

- **Decided (Joakim):** a title completes as a search term, not as a title-to-hash mapping.
- The shell inserts the matched value and shows the description only as a hint. Offering "Artist - Title" for a hash would insert the hash into a search query, or a title where a hash is expected.

## Why

- Completion reads local files and is fast whatever the network does.
- An epoch and a pinned read close the hole in ADR-016 open question 5.
- Raw lines plus the shared fold make the replica a copy of the log, so it can be folded again when the fold changes.
- Detached refresh keeps the shell responsive and keeps all failure handling out of the completion path.

## Alternatives considered

- **Replicate a projection instead of the log.** Rejected. The client could not refold when the fold changes. A projection on the wire is a new versioned format and couples client and node fold versions (ADR-004, redeploy together), while the log format is already stable. The outbox is folded over the replica with the same fold (ADR-016 §2.6); keeping facts keeps that one fold.
- **A new library request that returns the log.** Rejected. ACID owns the log and the library holds no raw lines (ADR-001). The library would only proxy ACID's cursor paging, with a second cursor contract to keep compatible.
- **Check only the first line's hash for identity.** Rejected. It misses any rewrite or truncation that leaves the first line alone. The epoch plus a pin on the last synced line catches those.
- **Sync inline when stale.** Rejected by Joakim: completion and commands must not wait on the network.

## Consequences

- About one log copy per node per client, 7 MB today.
- Gateway and ACID should upgrade together, for availability, not correctness: a mismatched deploy fails closed ("node too old").
- The ADR-017 migration tool must replace `facts.epoch` (ADR-017 §6.9, step 4).
- The CLI depends on `library_projection`.
- Enables offline and remote search, a TUI following events, and the ADR-016 outbox later (in the state directory, not the cache).
- Wiping a cache loses nothing: it is rebuilt from the node.

## Open questions

1. **hicli completion context.** Completion gets only the raw word, not earlier arguments (johlrogge/hicli#33).
2. **Case-folding** against bash `compgen`: should the CLI match case-insensitively itself, or leave it to the shell?
3. **Stale warning** in `mdma replica status` when the last sync is old.
4. **Disk limit and pruning** of nodes not seen for N days.

## Phasing

1. **Gateway ACID reads only.** Port the read-only guard to the gateway's concurrent handler arm. `fix(gateway): forward only ACID reads over the network`.
2. **`library_projection`.** Extract the pure fold from `library_service` into a component, with the split-point property test. `library_service` uses it. A pure refactor.
3. **ACID log epoch.** `LinePin`, `facts.epoch`, `StreamChunk.epoch` and `total_lines`, `StreamDiverged`, and a pinned client read. Gateway and ACID deploy together.
4. **`fact_replica` and `mdma replica`.** Store, verify and sync with injected effects. Commands `replica sync [--quiet]` (hidden), `replica status` and `replica reset [--node]`. Needs phases 2 and 3 for the fold and the epoch.
5. **Snapshot and completion.** Build `completion.json`, add the detached refresh, and wire completion for marks, playlists, search filters and query, `--source` and sources. Acceptance: `mark <Tab>` answers in about 20 ms offline, a new mark completes within 30 s, and three shells syncing never corrupt the replica.

Phases 1-3 are independent.

## Related

- ADR-001, ADR-009, ADR-011
- ADR-014 (§14, shared fold)
- ADR-016 (§2-3, amended)
- ADR-017 (§6.9, amended)
- ADR-020
