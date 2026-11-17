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
3. The library service calls these under its locks. It refreshes the lineage (`refresh_lineage`: plays and covers) before every read that returns `TrackInfo` (Search, ListTracks, GetTrack, ListMarked and PlaylistEdit), not only for date queries. `AlbumCovers` is rebuilt when the projection catches up. With those changes the node and a local fold over the same lines give the same answer.
   - `AlbumCovers` picks deterministically: each album takes its cover from the track with the smallest content hash among those that have a cover (plain string comparison), so the choice does not depend on fold or track order.
   - A track's cover is its effective one (`IndexedTrackInfo::effective_cover_art_path`): its own `CoverArtPath`, else the one it inherits along `Replaces` (ADR-013, amendment 2026-11-17). The one record of what outlives an entity is `LineageLog` (it was `PlayLog`): its `plays` and its `covers`, one `record`, one `merge`. `LineageLog::apply_to` sets the inherited cover together with the play times, so `AlbumCovers` is built after `apply_to` on the node (which rebuilds it when it refreshes the lineage), the stub and the replica alike. `ResolvedLineage` (it was `ResolvedPlays`) owns the one walk of `Replaces`, and `Projection::lineage` (it was `plays`) holds the log.
   - The inherited cover is the one the lineage member that was replaced last had live when it was replaced. `LineageLog` keeps, per key and in stream order, the `CoverArtPath` assertions and retracts and a `Retired` mark where a `Replaces` of it is asserted. It folds each key with the ADR-018 retract rule: a `Retired` closes the key and takes the snapshot, and a cover asserted after it (a re-ingest) reopens it for the next `Retired`. Both merge paths write `Replaces` before they retract the loser, so a cover retracted before it was the user's intent and stays gone, and the merge's own retracts are ignored. The order of the stream matters for covers, so a log is appended to in stream order and `merge` appends what was recorded later.
4. There is no second `TrackFields` mapping, no client-side re-count and no separate renderer for local results. Both sources yield `TrackInfo`, and the CLI prints it with `print_tracks`.
   - **Decided:** `library_service_stub`'s own fold is folded in now, not as a follow-up. That is `IndexedTrack`, its `apply_fact`/`retract_fact` and `as_fields` (stub `service.rs:58-161`, `:799-865`). The stub holds `library_projection::IndexedTrackInfo`, applies facts with `library_projection::apply_fact`, and answers through the §1 functions and `sort_catalog`. Its writes (`WriteMark`, `UnmarkTrack`) build the fact and apply it the same way. It already depends on `library-projection`.
   - The stub follows the node in the lineage too: it keeps a `LineageLog`, records every fact it projects (`record(&fact)`), and applies it to its tracks before every catalog read (`with_catalog`), as the node does in `refresh_lineage`. A `Replaces` or a cover written after startup takes effect there as it does on the node.

### 2. The read source is a property of the backend

1. `client_facts_replica` (the ADR-022 §3 implementation of `client-facts`) exposes `Facts`, a closed enum with two variants:
   - `Replica(ReplicaView)`: the folded replica, with plays applied and `AlbumCovers`;
   - `Node(&Node)`: lazy (§4).
   - As built, `Facts<N>` is `Replica(Box<ReplicaView>)` or `Node(N)` with `N: Deref<Target = Node>`, so the CLI holds `&Node` and the TUI holds `Rc<Node>`.
2. `Facts` answers the reads that facts can answer: `search`, `tracks(limit)`, `track(hash)`, `marked(filter)`, `fact_values(type)`, `facts_of(hash)`, `play_histories` and `recent_plays`.
   - Each method is one `match` with two arms.
   - The replica arm calls the §1 functions.
   - The node arm sends the existing `LibraryRequest`, which the service answers with the same §1 functions.
   - `play_histories` and `recent_plays` have no `LibraryRequest` yet. `Facts::Node`'s arms for them answer with the error "`mdma plays` reads only the replica: the node has no plays request yet; drop --facts=node". When a plays request exists, the arms send it like the others.
3. **Choosing the source (Decided).** `--facts=replica|node` is a global option of every client. **The default is `replica` from the start**, because offline-first is the point. §6 measures whether a cache is needed; it does not decide the default.
   - `MDMA_FACTS` is an optional convenience with the same values. Nothing requires it. The flag wins over the variable. An invalid value is a usage error. An empty `--facts` or `MDMA_FACTS` counts as unset, in every client: `FactsChoice::parse_given(&str) -> Result<Option<FactsChoice>, UnknownFactsChoice>` is the one parser, and the CLI and the TUI both use it. (hicli cannot express "empty is unset" for a typed value; johlrogge/hicli#40.)
   - `--facts` governs only the category (a) reads of §5. A read whose answer becomes a node write, or that serves a node-only pane, reads from the node whatever `--facts` says: a stale replica would give a partial answer and the write would act on it. It is still the `Facts` API, as `Facts::Node(node)`, so there is one code path.
   - **There is never a silent fallback.**
     - **No replica** is an error that says how to get one, with the existing texts: "no replica of <id> yet; a background sync creates it, or run `mdma replica sync`". With no node known at all: "no node to find a replica for; use --node NAME or run `mdma node select`". The command exits 1 and never asks the node. The client requests a background sync (the CLI's `after_command`, the TUI at start and on `:refresh`), so the next read finds a replica.
     - **An unreadable replica** is the same error with its cause.
     - **An unreachable node** under `--facts=node` is an error that names the replica as the alternative, with its age.
4. This is an enum, not a trait. The build-time interface is still `client-facts` (ADR-022 §3); it decides which sources a build has. The runtime flag picks one of them. `LibraryBackend::{Direct, Gateway}` already works this way. A later `client_facts_remote` would offer only `Node` and answer `--facts=replica` with "this build has no replica".
5. Writes and node-only operations (playlists, queue, playback, sources, inbox, admin, export) use `Node` directly, as today. The reads that feed a write, and the rows of a node-only pane (the TUI's queue and playlists), go through `Facts::Node` and never use the `--facts` choice: the node's own answer is what the write acts on.

### 3. Local reads say where they come from

- Every answer from the replica writes one note to stderr: "replica of <id>, synced <age>". A refusal counts as an answer ("track not found" from a stale replica is exactly when the age matters), so the note is printed whenever the replica opened and was asked. When it did not open, there is no note: the error says why.
- It adds "behind: the last sync attempt failed" when `last_attempt > synced_at` or `failures > 0`, and "N lines this mdma cannot read; upgrade mdma" when there are unreadable lines.
- Stdout carries only the answer, so pipes stay clean.
- There is one note function. `plays` uses it too.
- The TUI shows the same text in its status line.

### 4. One lazy node

1. Nothing connects until something needs the node. `mdma_client::Node` resolves on first use (as today). It caches each connection (library, playback, gateway) after the first success and retries after a failure.
   - A remote node's library, playback and admin connections are one gateway socket. The socket is shared by clones and serialises requests (`nng_transport::SharedSocket`, send to receive under one lock), because a Req0 socket has one request in flight.
   - "Events" is an address (`Node::event_gateway`, or `Node::event_address` once resolved), not a cached connection: the subscriber owns its SUB socket and dials without waiting (`dial_async`), so it connects and reconnects by itself. The address does not pass the reachability check.
2. **Reachability.** The first use of a remote node is bounded: the probe connects without waiting and sends `Identify`. Sending is limited by `IDENTIFY_TIMEOUT` and so is receiving, so the probe takes at most 2 × `IDENTIFY_TIMEOUT` (2 s), plus name resolution for a host name. The outcome is cached for the process: on failure, further attempts fail at once for 10 s, so a TUI never freezes on each keystroke. There is one mechanism for every caller, not one per command.
   - A node that answered "who are you" while the resolver verified it (a cached node, the usual case) counts as reached, and the answer (`Answer`: the `NodeIdentity`, or why there is none) is kept. The probe runs only when nobody has asked yet (`--node`, `MDMA_NODE`). `replica sync` reads `Node::identity()` instead of asking again. It still opens its own gateway client with its 10 s `LINK_TIMEOUT` for the large ACID reads; the probe bound applies only to "does it answer".
   - Silence reads "<host> (<ip>) did not answer in time". The dial is asynchronous, so a refused or unreachable node also shows up as silence; a dial that fails outright keeps its wording.
   - Known risk: the probe gates `replica sync` on an explicit target (`--node`, `MDMA_NODE`), where the resolver verified nothing. A node that answers "who are you" slower than the probe bound fails where the sync's own 10 s `LINK_TIMEOUT` identify used to succeed. Raise the probe bound or give the `Node` one if it shows up.
   - **Known limitation (standing).** A remote node's library, playback and admin requests share one gateway socket, which serialises them: a slow request blocks the others. A service that hangs behind the gateway does so too: its request times out after the request timeout (5 s), the transport records the node as unreachable (§4.4), and every service of the node is then dropped for 10 s, though only one was hung. Per-service sockets are a cheap change in `Node` (drop the shared `gateway_client` slot), and the only cure, if it shows up.
3. **Errors name what needed the node.** `Node` carries a `needed_by` label, set once where the client starts, and there is no default: every client names itself.
   - Every error about resolving or reaching the node (`Resolve`, `Connect` for local IPC and after a successful probe, and the probe failure) reads "<needed_by> needs the node: <text>". One function (`Node::labelled`, public, idempotent) applies the label; a client that connects on its own (`replica sync`) uses it too. `NeedsGateway`, `NoRemoteNode`, `InvalidEnv` and the sources directory error are not about reaching the node and stay as they were.
   - The CLI sets it from the parsed command path (`main.rs`): "mdma queue append needs the node: <the connect text>";
   - the TUI sets "mdma-tui" and prefixes the feature in the pane's error.
   - The `Facts::Node` arm adds the hint "--facts=replica answers from the local replica (synced <age>)".
4. Resolution failures behave like connect failures: they do not stop a client from starting, and they are kept the same way. A failed resolution (nothing found, cancelled prompt) is kept for 10 s with its text; asking again within the window fails at once, without browsing or prompting again.
   - Only the first resolution of a process may ask the user to pick or print a note. A retry after the window runs silent and non-interactive.
   - A cancelled prompt (`ResolveError::Cancelled`) stands for the whole process: the user chose no node, and it is never retried.
   - **Sticky reach (Decided).** A node that stops answering on a connection already cached is unreachable too, and the transport says so, not the callers.
     - `nng_transport::SharedSocket` carries a reach cell that all its clones share. A request that fails with an unreachable error records the error text in it. The one classification is `NngClientError::is_unreachable()`: `TimedOut`, `ConnectionRefused`, `ConnectionReset` and `Closed` (also as the cause of a failed dial) mean unreachable; `Serialization`, a service's own error and the other nng errors do not. `library_ipc_client::ClientError::is_unreachable()` is built on it (a protocol refusal is never unreachable), and the playback and gateway clients have it through `NngClientError`.
     - `Node::library()`, `playback()` and `admin()` ask the cell of each cached connection (`take_unreachable`) before handing one out. If one went dead, the node drops it and the connections that share its socket: all of a remote node's (they are one socket), but on local IPC only that service, so a hung library leaves playback usable (the window is kept per group of connections). A remote node is put back to "must answer the reachability check", and for `RETRY_AFTER` (10 s, counted from the ask that first heard of the failure) every ask fails with the transport's text, labelled like every node error, without dialling. After the window the node dials again and a remote node is probed again.
     - A request that was already in flight when the cell was set is not retried; only the next ask is affected. The cell is told once (taking it clears it).
     - Clients that call methods on a backend they hold (the TUI's panes) need nothing of their own to notice: the next call to `Node` finds out. A loop that keeps calling the held backend does not look in between, so such loops stop at the first error (the TUI's queueing does), rather than waiting out the timeout once per item.
5. **The TUI** resolves before raw mode (ADR-022 §2), so the pick prompt still works. `Link::new` resolves the node and probes it once there (a remote node: at most 2 s, §4.2), then opens `Facts`. That is the only wait before raw mode.
   - A failed resolution or connect becomes the "node unreachable" state, shown in the queue, now-playing and playlist panes. The browser and search keep working from `Facts`.
   - The node panes read their row metadata from the node (§5): `Link::node_facts()` is always `Facts::Node`, whatever `--facts` says, so a playlist is never written back from rows that came from a replica.
   - **Events.** The TUI subscribes once, on the first tick after the node is resolved (`Node::event_address`, which never resolves and never probes), whether or not the node answers: the SUB socket dials without waiting and reconnects by itself, so nothing resubscribes. A message that cannot be read (`AppEvent::Malformed`) is news for the status line only. A subscription ends only when the socket fails (`SubscriberError`) or nobody listens any more; the thread then exits, the channel closes, and the TUI does not subscribe again. While the node is reached and no subscription is live (not resolved yet, no event address, or ended), the status line says "no live updates".
   - **Background sync.** At start the TUI asks for a background sync with `Urgency::WhenStale`, and `:refresh` asks with `Urgency::Now` (it also folds the replica again and refreshes every pane, even when the fold fails). It finds `mdma` next to its own executable, then on `PATH` (an executable file, in an absolute directory); if a sync is due and it finds none, the status line says so. It keeps the child that `spawn_background_sync` returns and reaps it with `try_wait` on tick; the CLI drops it, as it exits soon. It never syncs inline.
   - The status line shows the lines of `source_note`, then what is wrong with the node.
   - **Decided:** cancelling the pick prompt starts the TUI offline with no node. The node panes show "no node selected; run `mdma node select`". `Facts` opens the replica chosen offline by ADR-021 §6's order (the remembered node). With no replica, the browser shows the §2.3 missing-replica text.
6. **Background sync** stays detached (ADR-021 §6, ADR-022 §5). A local read never waits for it, never takes `sync.lock` and never fails because of it.

### 5. What reads locally

- **(a) local:** search, fact-values-for, list, get, view, sort, facts, marked and plays in the CLI; browser, search and genre groups in the TUI.
- **(b) node only:** writes, playlists (files), queue, playback, events, sources, inbox, admin, status, ping, export, upload and library maintenance. This includes `rekordbox import` (the matching and its writes), and the rows of the TUI's queue, now-playing and playlist panes. The rule: a read whose answer becomes a node write, or that serves a node-only pane, reads from the node. `rekordbox import` matches with `Facts::Node`, because a stale replica gives fewer matches and the import would replace a good playlist with a partial one; the panes are shown from the same node that gave their hashes.
- **(c) partial:** none.
- **Covers and blob paths** are node-relative strings. Locally they are the same text, but the files are not there. No CLI or TUI view renders them; exports stay (b).

### 6. Measuring decides whether a cache is needed

- **Decided:** the replica is the default from the start (§2.3). Measurement decides only whether reads need a cache.
- The plays refresh (§1.3) adds an ACID `read_stream` round trip to every `TrackInfo` read on the node. Measure it, because the console polls.
- **Tools.**
  - `library_projection`'s example `fold_cost` times read, `project_lines`, `plays.apply_to`, `AlbumCovers::build`, search and sort over a given `facts.jsonl`, and prints p50 and p95 over 20 runs.
  - `just replica-search-timing` times `mdma search --facts=replica` and `mdma search --facts=replica | mdma view --title -` with a shell loop (`hyperfine` is not in the devenv), on a copy of the replica whose `synced_at` and `last_attempt` are set to now, so no background sync lands in the timings and the real cache is untouched. `just replica-fold-cost <facts.jsonl>` runs the example on the production profile.
  - Run both on the laptop and on a Pi 5 (on the node the CLI reads the `localhost` replica).
  - Agents cannot run these recipes: the just MCP allowlist blocks them (metadev#69). A human runs them.
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
  - **Still over after the checkpoint:** next try a binary codec for the checkpoint, then split provenance into a second file loaded only by reads that need it, each step measured. Since ADR-016's 2026-11-17 amendment, provenance is read by every `TrackInfo` read (it carries the marks) and every search (`--mark`), so the split no longer helps those reads: only `facts` would still avoid it.
- **Measured (P6, laptop).**
  - Machine johlmut, AMD Ryzen 7 PRO 6850H. Release `mdma` 0.38.3. Replica `mdma-stage2`: 24000 lines, 6904676 bytes. Artist filter "Carbon Based Lifeforms". 20 runs each. The Pi 5 is not measured yet.
  - `fold_cost`, in process:

    | stage | p50 ms | p95 ms |
    |---|---|---|
    | read file | 1.4 | 2.3 |
    | `project_lines` | 60.1 | 70.5 |
    | plays apply | 1.1 | 2.1 |
    | `AlbumCovers::build` | 0.2 | 0.5 |
    | search, default query | 1.3 | 2.3 |
    | search, artist filter | 0.2 | 0.4 |
    | `sort_catalog` | 0.5 | 1.2 |
    | total | 66.1 | 79.4 |

  - End to end, release binary, process start included:

    | command | p50 ms | p95 ms |
    |---|---|---|
    | `mdma --facts=replica search --artist` | 87.7 | 90.9 |
    | `search \| view --title -` | 98.2 | 102.0 |

  - **Verdict: within every laptop threshold.** Search p95 91 ms against 150; the pipeline 102 ms against 300; the cold fold (TUI start proxy, `fold_cost` total) 79 ms against 500. No checkpoint, and ADR-031 is not triggered. Re-measure when the replica passes 48000 lines, or when a release changes the fold.
  - `project_lines` is about 90% of the cost; a checkpoint, if ever needed, would target it.
  - The Pi 5 threshold and the node-side plays refresh round trip are still unmeasured.
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
- `mdma_client` does not depend on `client-facts`, so bevy-app and bdd stay light and there is no cycle. It may depend on `library_projection`, which is pure, for the one hash-resolution rule; `library_projection` never depends on `mdma_client`.
- `mdma-cli` and `mdma-tui` depend on `client-facts` and `mdma-client`. `mdma-cli` drops its direct `fact-replica` and `library-projection` dependencies (ADR-022 §4). For that, `Facts` answers plays in values rather than handing out the fold: `play_histories(&[ContentHash])` (the hash resolved inside `Facts`, each answered or refused on its own, the plays of replaced tracks included) and `recent_plays(n)`, with `Play`, `PlayHistory` and `Stop` re-exported. A future node arm (`LibraryRequest::Plays`) can carry the same values.
- `client-facts` also owns starting the detached background sync (`spawn_background_sync(mdma, node)`, with `detach`), so the TUI can request one; the throttle is `request_sync`.
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

- Wire: `LibraryRequest` is unchanged. `TrackInfo` gains `genre`, `label` and `styles`, additive with serde defaults, so old nodes and clients still decode (ADR-022 0a); `library_projection::track_info` fills them. Search order changes for every client (console, TUI, CLI), deliberately.
- The library and console redeploy for the order; old clients keep working.
- With the replica as default, every local read folds the replica. Before §6's checkpoint that is about 7 MB per CLI command, and a pipeline folds once per command. §6 decides whether a checkpoint is built.
- First use with no replica fails with how to get one (§2.3). The background sync started by that command makes the next one work.
- `library_service_stub` loses its own fold (§1.4).
- `NodeError` texts change (they gain "`mdma X` needs the node:"). That is a deliberate amendment of ADR-022 §1's byte-identical texts.
- The TUI holds one fold in memory. Its refresh is a full refold, via `:refresh`; incremental catch-up waits for §6's measurement (and the checkpoint, if it is built). It searches per keystroke in memory.

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

P0a (read side into `library_projection`), P1 (`client_facts_replica` move) and P3 (lazy node) run in parallel. P0b (service consistency, equivalence test and the stub fold-in) follows P0a; P2 (`Facts`) follows P0a and P1; P4 (CLI) follows P2 and P3; P5 (TUI) follows P2 and P3 and depends on the reach cell (§4.4), which is part of the transport and `Node`; P6 (measure, record, and trigger ADR-031 if needed) is last.

## Related

ADR-004, ADR-011, ADR-014 (§14), ADR-016, ADR-021, ADR-022 (§1, §3, §6 amended), ADR-027, ADR-029.

stainless_facts issues: johlrogge/stainless_facts#16 (positions), #17 (entity index), #18 (resumable folds and checkpoints), #19 (FactIterator stops silently at an unreadable line).
