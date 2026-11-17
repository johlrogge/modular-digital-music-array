# ADR-030: Local reads from the client replica, and a lazy node

## Status

Accepted, 2026-11-17 (Joakim decided)

Amends ADR-022 §3 (the client-facts interface gains a runtime source), §1 (NodeError texts gain who needed the node) and §6 (staleness is labelled on every local read). Builds on ADR-021 (replica), ADR-014 §14 (split-point test), ADR-004 (one fold). Related: ADR-011, ADR-016, ADR-029 (one matcher, in Rust).

## Context

File and line references below are as of 5d2cf8f.

- Joakim, 2026-11-17: an offline `mdma search` fails. Search, facts and other reads should work without a node; playback and other node commands need one. The same goes for the TUI. The connection should be lazy.
- The CLI resolves and connects lazily already (`Ctx` is `mdma_client::Node`). It fails offline because `search` asks the node: `search.rs:265` `ctx.library()` → `node.rs:193-196` → `nng_transport::connect` (blocking dial, `lib.rs:60-62`, `:114`).
- The TUI connects eagerly at startup (`mdma-tui/src/main.rs:78-82`) and exits when that fails (`:88-93`).
- The replica (ADR-021) holds every raw line of the node's log under `dirs::cache_dir()/mdma/nodes/<id>/` (about 7 MB). `mdma plays` already folds it with `library_projection` (`plays.rs:79`, `replica.rs:406-425`).
- The search pieces still live in `library_service`:
  - the `TrackFields` mapping (`service.rs:2384-2401`);
  - the hidden filter and the matcher (`service.rs:2374-2406`);
  - `to_track_info` and the album cover cache (`:2095-2157`);
  - fact values (`:1071-1081`);
  - marked (`:1049-1051`);
  - fact formatting (`:366`).
- Three things make the node and a local fold disagree today:
  - search results come out in `HashMap` order (`library_projection` `project`);
  - play times are folded only for `--started`/`--stopped` queries (`service.rs:2375`);
  - the album cover cache is built once at start and never refreshed (`service.rs:407`).
- `library_service_stub` has its own `TrackFields` mapping (`as_fields`, stub `service.rs:142`).

## Decision

### 1. One search, one order, in library_projection

1. `library_projection` gains the pure read side, moved from `library_service` without reshaping:
   - `track_info(&IndexedTrackInfo, &AlbumCovers) -> TrackInfo`;
   - `AlbumCovers::build(&[IndexedTrackInfo])`;
   - `search_fields(&IndexedTrackInfo) -> TrackFields`;
   - `search(tracks, covers, &TrackQuery) -> Vec<TrackInfo>` (hidden filter, `matches_query`, order);
   - `visible(tracks, covers, limit)`;
   - `marked(tracks, covers, &MarkFilter)`;
   - `fact_values(&fact_index, &FactType) -> Vec<String>`;
   - `entity_facts(lines, &ContentHash) -> Vec<(String, String)>` and `format_fact_for_display`.
2. **Order (Decided).** Every list of tracks, from search, list or marked, from the node or local, is ordered by artist, album, disc, track, then content hash as the final tiebreak. `sort_catalog(&mut [TrackInfo])` is the one implementation, and it works on `TrackInfo` so the stub can use it.
   - Text compares case-insensitively, by `str::to_lowercase` (the folding ADR-016 uses for mark names). Equal folded text is then ordered by the original text, so the order is total.
   - Disc and track compare as numbers.
   - A missing value sorts last, for every key.
3. The library service calls these under its locks. It refreshes plays (`refresh_event_timestamps`) before every read that returns `TrackInfo`, not only for date queries. `AlbumCovers` is rebuilt when the projection catches up. With those changes the node and a local fold over the same lines give the same answer.
   - `AlbumCovers` picks deterministically: each album takes its cover from the track with the smallest content hash among those that have a cover (plain string comparison), so the choice does not depend on fold or track order.
4. There is no second `TrackFields` mapping, no client-side re-count and no separate renderer for local results. Both sources yield `TrackInfo`, and the CLI prints it with `print_tracks`.
   - **Decided:** `library_service_stub`'s own fold is folded in now, not as a follow-up. That is `IndexedTrack`, its `apply_fact`/`retract_fact` and `as_fields` (stub `service.rs:58-161`, `:799-865`). The stub holds `library_projection::IndexedTrackInfo`, applies facts with `library_projection::apply_fact`, and answers through the §1 functions and `sort_catalog`. Its writes (`WriteMark`, `UnmarkTrack`) build the fact and apply it the same way. It already depends on `library-projection`.

### 2. The read source is a property of the backend

1. `client_facts_replica` (the ADR-022 §3 implementation of `client-facts`) exposes `Facts`, a closed enum with two variants:
   - `Replica(ReplicaView)`: the folded replica, with plays applied and `AlbumCovers`;
   - `Node(&Node)`: lazy (§4).
2. `Facts` answers the reads that facts can answer: `search`, `tracks(limit)`, `track(hash)`, `marked(filter)`, `fact_values(type)`, `facts_of(hash)` and `plays`.
   - Each method is one `match` with two arms.
   - The replica arm calls the §1 functions.
   - The node arm sends the existing `LibraryRequest`, which the service answers with the same §1 functions.
   - `plays` has no `LibraryRequest` yet. `Facts::Node`'s `plays` arm answers with the error "`mdma plays` reads only the replica: the node has no plays request yet; drop --facts=node". When `LibraryRequest::Plays` exists, the arm sends it like the others.
3. **Choosing the source (Decided).** `--facts=replica|node` is a global option of every client. **The default is `replica` from the start**, because offline-first is the point. §6 measures whether a cache is needed; it does not decide the default.
   - `MDMA_FACTS` is an optional convenience with the same values. Nothing requires it. The flag wins over the variable. An invalid value is a usage error.
   - **There is never a silent fallback.**
     - **No replica** is an error that says how to get one, with the existing texts: "no replica of <id> yet; a background sync creates it, or run `mdma replica sync`". With no node known at all: "no node to find a replica for; use --node NAME or run `mdma node select`". The command exits 1 and never asks the node. `after_command` starts the background sync as usual.
     - **An unreadable replica** is the same error with its cause.
     - **An unreachable node** under `--facts=node` is an error that names the replica as the alternative, with its age.
4. This is an enum, not a trait. The build-time interface is still `client-facts` (ADR-022 §3); it decides which sources a build has. The runtime flag picks one of them. `LibraryBackend::{Direct, Gateway}` already works this way. A later `client_facts_remote` would offer only `Node` and answer `--facts=replica` with "this build has no replica".
5. Writes and node-only reads (playlists, queue, playback, sources, inbox, admin, export) do not go through `Facts`. They use `Node` directly, as today.

### 3. Local reads say where they come from

- Every answer from the replica writes one note to stderr: "replica of <id>, synced <age>".
- It adds "behind: the last sync attempt failed" when `last_attempt > synced_at` or `failures > 0`, and "N lines this mdma cannot read; upgrade mdma" when there are unreadable lines.
- Stdout carries only the answer, so pipes stay clean.
- There is one note function. `plays` uses it too.
- The TUI shows the same text in its status line.

### 4. One lazy node

1. Nothing connects until something needs the node. `mdma_client::Node` resolves on first use (as today). It caches each connection (library, playback, gateway) after the first success and retries after a failure.
   - A remote node's library, playback and admin connections are one gateway socket. The socket is shared by clones and serialises requests (`nng_transport::SharedSocket`, send to receive under one lock), because a Req0 socket has one request in flight.
   - "Events" is an address (`Node::event_gateway`), not a cached connection: the subscriber owns its SUB socket. The address still passes the reachability check.
2. **Reachability.** The first use of a remote node is bounded: the probe connects without waiting and sends `Identify`. Sending is limited by `IDENTIFY_TIMEOUT` and so is receiving, so the probe takes at most 2 × `IDENTIFY_TIMEOUT` (2 s), plus name resolution for a host name. The outcome is cached for the process: on failure, further attempts fail at once for 10 s, so a TUI never freezes on each keystroke. There is one mechanism for every caller, not one per command.
   - A node that answered "who are you" while the resolver verified it (a cached node, the usual case) counts as reached, and the answer (`Answer`: the `NodeIdentity`, or why there is none) is kept. The probe runs only when nobody has asked yet (`--node`, `MDMA_NODE`). `replica sync` reads `Node::identity()` instead of asking again. It still opens its own gateway client with its 10 s `LINK_TIMEOUT` for the large ACID reads; the probe bound applies only to "does it answer".
   - Silence reads "<host> (<ip>) did not answer in time". The dial is asynchronous, so a refused or unreachable node also shows up as silence; a dial that fails outright keeps its wording.
   - Known risk: the probe gates `replica sync` on an explicit target (`--node`, `MDMA_NODE`), where the resolver verified nothing. A node that answers "who are you" slower than the probe bound fails where the sync's own 10 s `LINK_TIMEOUT` identify used to succeed. Raise the probe bound or give the `Node` one if it shows up.
   - For P5: one gateway socket serialises library, playback and admin requests, so a slow request blocks the others. Per-service sockets are a cheap change in `Node` (drop the shared `gateway_client` slot) if it shows up.
3. **Errors name what needed the node.** `Node` carries a `needed_by` label, set once where the client starts, and there is no default: every client names itself.
   - Every error about resolving or reaching the node (`Resolve`, `Connect` for local IPC and after a successful probe, and the probe failure) reads "<needed_by> needs the node: <text>". One function (`Node::labelled`, public, idempotent) applies the label; a client that connects on its own (`replica sync`) uses it too. `NeedsGateway`, `NoRemoteNode`, `InvalidEnv` and the sources directory error are not about reaching the node and stay as they were.
   - The CLI sets it from the parsed command path (`main.rs`): "mdma queue append needs the node: <the connect text>";
   - the TUI sets "mdma-tui" and prefixes the feature in the pane's error.
   - The `Facts::Node` arm adds the hint "--facts=replica answers from the local replica (synced <age>)".
4. Resolution failures behave like connect failures: they do not stop a client from starting, and they are kept the same way. A failed resolution (nothing found, cancelled prompt) is kept for 10 s with its text; asking again within the window fails at once, without browsing or prompting again.
   - Only the first resolution of a process may ask the user to pick or print a note. A retry after the window runs silent and non-interactive.
   - A cancelled prompt (`ResolveError::Cancelled`) stands for the whole process: the user chose no node, and it is never retried.
   - Not yet: a transport timeout on an already cached connection does not mark the node unreachable. P5 (the TUI) should call that out when it wires the unreachable state.
5. **The TUI** resolves before raw mode (ADR-022 §2), so the pick prompt still works.
   - A failed resolution or connect becomes the "node unreachable" state, shown in the queue, now-playing and playlist panes. The browser and search keep working from `Facts`.
   - **Decided:** cancelling the pick prompt starts the TUI offline with no node. The node panes show "no node selected; run `mdma node select`". `Facts` opens the replica chosen offline by ADR-021 §6's order (the remembered node). With no replica, the browser shows the §2.3 missing-replica text.
6. **Background sync** stays detached (ADR-021 §6, ADR-022 §5). A local read never waits for it, never takes `sync.lock` and never fails because of it.

### 5. What reads locally

- **(a) local:** search, fact-values-for, list, get, view, sort, facts, marked and plays in the CLI; browser, search and genre groups in the TUI.
- **(c) partial:**
  - `rekordbox import` matching is local, its writes need the node;
  - TUI queue and now-playing rows take their metadata from `Facts`, while queue and playback need the node;
  - TUI playlist rows likewise.
- **(b) node only:** writes, playlists (files), queue, playback, events, sources, inbox, admin, status, ping, export, upload and library maintenance.
- **Covers and blob paths** are node-relative strings. Locally they are the same text, but the files are not there. No CLI or TUI view renders them; exports stay (b).

### 6. Measuring decides whether a cache is needed

- **Decided:** the replica is the default from the start (§2.3). Measurement decides only whether reads need a cache.
- **Tools.**
  - `library_projection`'s example `fold_cost` times read, `project_lines`, `plays.apply_to`, `AlbumCovers::build`, search and sort over a given `facts.jsonl`, and prints p50 and p95 over 20 runs.
  - `hyperfine` times `mdma search --facts=replica` and `mdma search --facts=replica | mdma view --title -`.
  - Run both on the laptop and on a Pi 5 (on the node the CLI reads the `localhost` replica).
- **Thresholds.**
  - Laptop: search p95 at most 150 ms; the two-command pipeline at most 300 ms.
  - Pi 5: search p95 at most 300 ms.
  - TUI start (one cold fold): at most 500 ms on the laptop.
- **What the measurement triggers.**
  - **Within all thresholds:** record the numbers here (machine, replica lines and bytes, release) and do nothing more. Re-measure when the replica passes twice the recorded line count, or when a release changes the fold.
  - **Over any threshold:** build the fold checkpoint (ADR-031, written when triggered). In short:
    - It is `library_projection::FoldState`, the unswept fold state with serde.
    - The sync writes it to `fold.ckpt` under `sync.lock`, keyed by `{epoch, lines, bytes, last_line_sha256}` and the release version. `completion.json` is built from the same state.
    - Readers validate the key, fold only the lines after it, and build the view.
    - There is one path: no checkpoint means the origin at line 0.
    - It is produced by the same fold. It is derived and deletable, so it is not a second mapping and not a projection on the wire (ADR-021).
    - Its precondition is a reference-counted `fact_index` in `library_projection`, so incremental retracts stop scanning every track.
    - Positions, an entity index and resumable folds are also wanted natively in stainless_facts: johlrogge/stainless_facts#16 (positions), #17 (entity index) and #18 (resumable folds and checkpoints). The MDMA checkpoint is shaped so that it can move onto them.
  - **Still over after the checkpoint:** next try a binary codec for the checkpoint, then split provenance into a second file loaded only by reads that need it (marked, `facts`), each step measured.
- `facts <hash>` reads raw lines with a byte prefilter on the full hash, confirmed by ACID's `is_entity_match`. An entity index waits for stainless_facts (johlrogge/stainless_facts#17) or a measurement that shows the need.
- The replica fold must not stop silently at an unreadable line. MDMA counts such lines itself (ADR-021 §1); stainless_facts' `FactIterator` ends there without an error (johlrogge/stainless_facts#19), so MDMA does not use it for this.

### 7. Equivalence

One fixture through the real ACID path (`make_service_with_facts`) must give the same result two ways:
- the service's `Search`;
- `project_lines` over the log *as ACID holds it after the service started* (the backfills write facts), then `plays.apply_to`, then `search`.

The comparison covers every `TrackInfo` field and the order, serialised as JSON. Equivalence is also checked for `ListTracks`, `GetTrack`, `ListMarked` and `GetFactValues`. The test never uses the test-only `load_tracks_from_facts` fold.

### 8. Layout

- `library_projection` (interface `library-projection`) gains dependencies on `library-search` and `acid-protocol`. It already depends on `library-ipc-protocol` and `music-facts`. `library_search` does not depend on `library_projection`, so there is no cycle.
- `client_facts_replica` (interface `client-facts`, new) depends on `mdma-client`, `fact-replica`, `library-projection`, `acid-protocol` and `gateway-client`.
- `mdma_client` does not depend on `client-facts` or `library_projection`, so bevy-app and bdd stay light and there is no cycle.
- `mdma-cli` and `mdma-tui` depend on `client-facts` and `mdma-client`. `mdma-cli` drops its direct `fact-replica` and `library-projection` dependencies (ADR-022 §4).
- `library_service` and `library_service_stub` depend on `library-projection` for the §1 functions and `sort_catalog`.

### 9. Offline writes are a future increment

- Writes stay node-only in this ADR (§2.5). Offline writes come later: marks and edits through the ADR-016 outbox, and playlists especially (ADR-027's CAS edits queued against a cached playlist version).
- This design must not block them:
  - `Facts::Replica`'s view is a pure function of the replica lines (plus, after §6, a checkpoint of a prefix of them). It never contains outbox, probe or playlist data.
  - Local read-your-writes (ADR-016 §2.6) will fold the outbox on top of the reader's own copy of the state, per read. Queuing or pushing invalidates nothing in the cache.
  - The outbox lives in the state directory, not the cache, so `replica reset` never loses it.

## Why

- One fold, one matcher and one order mean the replica and the node cannot disagree, and the §7 test proves it on one fixture.
- Making the source a property of `Facts` keeps every command free of "where from" logic. Adding a read means adding one method with two arms.
- A lazy node with one cached reachability check gives every client the same failure behaviour and keeps the TUI responsive.
- Labelling the source and age is honest, and never falling back keeps a broken sync visible (ADR-022 §6).

## Alternatives considered

- **Build-time choice only** (a replica profile and a node profile). Rejected: Joakim wants `--facts` at run time, and a build cannot pick per invocation.
- **A trait over `LibraryBackend`** with a replica implementation. Rejected: runtime polymorphism by trait where a closed enum does, and it would have to stub every write.
- **A replica variant inside `LibraryBackend`** (in `mdma_client`). Rejected: `mdma_client` would depend on `library_projection` and `fact_replica`, and `client_facts_replica` (which needs `Node`) would form a cycle.
- **Fall back to the node when the replica is missing, or to the replica when the node is down.** Rejected (ADR-022 §6, Joakim).
- **Per-command offline handling.** Rejected: special cases, and drift.

## Consequences

- Wire: none. `LibraryRequest` is unchanged. Search order changes for every client (console, TUI, CLI), deliberately.
- The library and console redeploy for the order; old clients keep working.
- With the replica as default, every local read folds the replica. Before §6's checkpoint that is about 7 MB per CLI command, and a pipeline folds once per command. §6 decides whether a checkpoint is built.
- First use with no replica fails with how to get one (§2.3). The background sync started by that command makes the next one work.
- `library_service_stub` loses its own fold (§1.4).
- `NodeError` texts change (they gain "`mdma X` needs the node:"). That is a deliberate amendment of ADR-022 §1's byte-identical texts.
- The TUI holds one fold in memory and applies new lines on its refresh (`catch_up`). It searches per keystroke in memory.

## Open questions

All seven are answered (2026-11-17):
1. The order is case-insensitive with missing values last.
2. `MDMA_FACTS` is optional.
3. `plays --facts=node` is an error until a plays request exists.
4. A cancelled pick prompt starts the TUI offline.
5. No replica is an error that says how to get one.
6. The thresholds are in §6.
7. The stub is folded in now.

New: whether ADR-031 is needed is decided by §6's measurement.

## Phasing

P0a (read side into `library_projection`), P1 (`client_facts_replica` move) and P3 (lazy node) run in parallel. P0b (service consistency, equivalence test and the stub fold-in) follows P0a; P2 (`Facts`) follows P0a and P1; P4 (CLI) and P5 (TUI) follow P2 and P3; P6 (measure, record, and trigger ADR-031 if needed) is last.

## Related

ADR-004, ADR-011, ADR-014 (§14), ADR-016, ADR-021, ADR-022 (§1, §3, §6 amended), ADR-027, ADR-029.

stainless_facts issues: johlrogge/stainless_facts#16 (positions), #17 (entity index), #18 (resumable folds and checkpoints), #19 (FactIterator stops silently at an unreadable line).
