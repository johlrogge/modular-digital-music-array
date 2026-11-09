# ADR-016: Marks, client-collected facts and per-node replicas

## Status

Proposed, 2026-11-06

Builds on ADR-001 (ACID is the only writer), ADR-009 (node identity), ADR-011 (single projection path) and ADR-014 (merge by fold). Reshapes the unreleased `ListBookmarked` request.

## Context

**Bookmarks today.**
- `MusicValue::Bookmarked { scope: Option<String>, timestamp: DateTime<Utc> }` (components/music_facts/src/value.rs:346-350).
  - `scope` is free text, and `None` means unscoped.
  - The value carries its own time.
- ADR-013:54 unions `Bookmarked` on merge. ADR-014:58 refines that to one per scope, with the latest winning within a scope. Sameness is decided by scope (library_service/src/service/dedup.rs:148-155).
- `WriteBookmark { hash, scope }` is defined in library_ipc_protocol/src/lib.rs:330. Its handler (service.rs:2046-2064) stamps `Utc::now()` with source `FactSource { tool: "mdma", version, origin: User }`.
- Clients:
  - the CLI's `mdma bookmark [--scope]` (projects/mdma-cli/src/tracks.rs:379);
  - the TUI's `b` key (projects/mdma-tui/src/input.rs:347), which always writes `scope: None`.
- There is no unbookmark. The only way to remove a bookmark is the generic `RetractFact`.
- `ListBookmarked` (on develop, a3f655c) is `tracks_where(is_bookmarked)` and ignores scope (service.rs:370-376, 1799).
  - It is deployed on stage2 but not on prod.
  - The CLI `bookmarks` command used it (f249d28) until phase 1 replaced it with `ListMarked`.

**Facts and the log.**
- A fact is `stainless_facts::Fact<E,V,S>(entity, value, timestamp, source, Assert|Retract)`. The attribute is the serde tag of `MusicValue`.
- `FactSource { tool, version, origin }` has no node, device or user field.
- ACID is the only writer of `facts.jsonl` (ADR-001).
  - It stamps each line with `Utc::now()`, and that time never goes backwards (ADR-014).
  - `ReadStream` takes an opaque cursor (`line:<n>`), and ACID publishes `acid/facts/*` events.
- Every node has its own independent ACID log. The node id is `/etc/hostname` (ADR-009). On the client, `NodeCache` lives in `~/.config/mdma/node.json`.

**Offline replica branch.**
- `feature/offline-library-replica` is local only: 3 commits ahead of develop (448b524, 628abb7, 4da80f9) and 55 behind it.
- What it has:
  - The gateway forwards only ACID reads (Ping, ReadStream, ReadEntity) and refuses writes.
  - The projection is extracted into `components/library_projection`.
- What it lacks: a client store, a sync protocol and a design doc.

**Intent (Joakim).**
1. A bookmark is a "liked" mark. Macro buttons apply named marks such as "banger" or "opener".
2. Clients collect facts asynchronously and push them when they choose. The server syncs facts out to clients.
3. With several nodes, facts are kept per node.

Related issues:
- #95: bookmarks list in one request.
- #97: a library IPC parse failure gets no reply.

## Decision

Points marked **decided** were decided by Joakim. Everything else is **proposed**.

### 1. Marks

1. **Reuse `Bookmarked` as the mark fact. No new variant, no migration.**
   - `scope: None` is the *default mark*: today's bookmark. **Decided:** it is called "liked".
   - `scope: Some(name)` is the named mark `name`.
   - A track holds at most one live mark per name, and the latest wins within a name (ADR-014 §3). A track may hold several different marks.
   - Existing facts are already marks. The immutable log is never rewritten.
2. **Mark names are validated at the edges, not in storage.**
   - `MarkName` is a newtype in music_facts: trimmed, non-empty, at most 64 characters, no control characters. It is otherwise lenient, following ADR-015's "names may be lenient".
   - **Decided:** mark names fold to lowercase. "Banger" and "banger" are one mark. `MarkName` folds on construction, so new writes are always lowercase.
     - Folding is `str::to_lowercase`. It is not Unicode case folding and not NFC normalisation, so canonically equivalent spellings are different marks.
     - Invisible format characters (such as zero-width spaces) are rejected too, as well as control characters.
   - `enum Mark { Default, Named(MarkName) }` maps to `Option<String>` on the fact.
   - The protocol carries `Mark`/`MarkName`, and so do the CLI and TUI arguments. The CLI's arg newtype wraps `MarkName`.
   - `MusicValue::Bookmarked.scope` stays `Option<String>`, so reading history can never fail on an old free-text scope.
   - **Decided:** "liked" is reserved. `MarkName` rejects it. Typing `liked` on the CLI means the default mark, so no named mark can be called "liked".
   - **Decided:** a legacy scope that isn't a valid mark name (e.g. empty, or over 64 characters) is still read and projected, and `marked` (`Any`) lists it. It cannot be written again, and it can be removed only with `mdma fact retract`.
3. **Protocol** (library_ipc_protocol):
   - **`WriteMark { hash, mark: Mark }`** replaces `WriteBookmark`.
     - **Decided:** `WriteBookmark` stays as a deprecated server-side alias for old clients, because prod's CLI and TUI use it. It is documented as deprecated but has no attribute.
     - **Decided:** re-marking a live mark is a no-op. Nothing is written, and the original time is kept. A macro pressed twice writes one fact.
   - **`UnmarkTrack { hash, mark: Mark }`** retracts every live `Bookmarked` assertion with that name on the track's chain head, matching on (value, source).
     - That includes entries that came from merge losers (ADR-014 §7).
     - If the mark isn't live, it is a no-op.
   - **`ListMarked { filter: MarkFilter }`**, with `enum MarkFilter { Any, Only(Mark) }`, replaces `ListBookmarked` outright. The explicit `Any` avoids overloading `None` to mean both "the default mark" and "any mark".
4. **Why reshape `ListBookmarked` now.** It isn't on prod and has no CLI user, so changing it today only means redeploying stage2. Once the #95 CLI ships against it, every later change needs a compatibility path for old nodes.
5. **Old nodes.** A new client that sends `ListMarked` or `WriteMark` to an older library currently gets no reply (#97).
   - #97 is fixed first: the library answers an unparseable request with an invalid-request error, following the same pattern as ADR-009 §4.
   - **Decided:** when a node doesn't understand a request, the client says: "The node did not understand this request; it may be older than this CLI — upgrade the node."
   - There is no fallback, and never one to `ListBookmarked`: no silent fallback (ADR-001).
6. **Macro buttons** are client configuration that maps a button to a `Mark`.
   - Pressing a button sends `WriteMark { hash: <playing track>, mark }`. Offline, it queues the same mark instead (§2).
   - No protocol is added per button.
7. **CLI naming (decided).** The commands are `mdma mark [name]`, `mdma unmark [name]` and `mdma marked [name]`.
   - **Decided:** `mark` and `unmark` with no name act on the default mark ("liked").
   - **Decided:** `marked` with no name lists tracks with any mark (`MarkFilter::Any`). `marked liked` lists only the default mark.
   - **Decided:** `bookmark` and `bookmarks` are true CLI aliases of `mark` and `marked`. They take the same arguments. The old `--scope` option and the HASH argument are gone.
   - **Decided (2026-11-09):** `mark` and `unmark` act on the tracks whose hashes arrive on stdin (pipe style, like `sort` and `view`). Piped means required: a non-TTY stdin with no hashes is an error ("No hashes provided on stdin."). The playing track is used only when stdin is a TTY.

### 2. Client-collected facts

1. **The server is the master (decided).**
   - ACID stays the only writer (ADR-001).
   - The node's ACID log time is the authoritative fact time. Clients never supply it.
   - Facts flow from server to client as the source of truth.
   - A client batches fact updates: it queues them and does not own them.
2. **Server → client: read-only replication**, as on the branch.
   - The client keeps a replica of the node's log. It is fed only by `ReadStream` from the client's cursor, and followed by `acid/facts/*` events.
   - The replica is projected with `library_projection`, the same fold as the library (ADR-011, ADR-014 §14).
   - The replica is a copy owned by the client, not a second writer of `facts.jsonl`.
   - When the replica is stale, the client says so, showing the cursor and the time of the last sync.
3. **Client → server: a batched, validated push.**
   - `PushFacts { batch: Vec<ClientFact> }` goes gateway → library, not to ACID directly.
   - The library validates each fact against its projection:
     - its attribute is on the whitelist (marks and unmarks only, at first);
     - its `MarkName` is valid;
     - its entity is resolved through the replaced-by map (ADR-014 §6);
     - its source origin is `User`.
   - The library writes the batch through ACID as one batch, under `retire_gate` like the other writers (ADR-013).
   - Raw `WriteFacts` stays refused at the gateway. This one request type is the only exception to the branch's write block.
4. **Idempotent push.**
   - A pushed assert is dropped if an identical live assertion exists. Identical means the same entity, value and source; for marks, the value includes its time.
   - A pushed retract of a value that isn't live is a no-op.
   - A retried batch therefore writes nothing twice, with no new `Fact` field.
   - An explicit idempotency key (client id plus a sequence number, recorded in the log) is deferred until the whitelist grows beyond values that carry their own time.
5. **An offline unmark is observed-remove.**
   - A queued unmark carries the exact mark values the client saw in its replica. It therefore cannot remove a mark another client added later.
   - An online `UnmarkTrack` is the same operation, applied to the server's current view.
6. **Local read-your-writes.**
   - The client projects the replica plus its outbox, so its own queued marks show immediately.
   - An unpushed mark-then-unmark pair cancels out in the outbox.
7. **Value time** (e.g. `Bookmarked.timestamp`): see Open questions 1.

### 3. Per-node facts

1. **Proposed: a separate replica and outbox per node, never merged.**
   - The client stores each replica under the node id from ADR-009 Identify. Each replica has its own cursor, because a cursor only means something for the log that issued it.
   - A replica is applied only after Identify confirms the node id. A node that can't be verified gets no replica.
   - A fact is pushed only to the node it was collected against.
   - "Synced out to any client" means any client of that node.
2. **No node field on `FactSource` for now.**
   - Within one node's log, the node is implicit.
   - A node field is needed only for cross-node merging, which is still open (Open questions 4).
   - If it is added later, it will be `node: Option<NodeId>` with a serde default, so legacy lines read as `None`.

## Why

- **Reusing `Bookmarked`** gives marks with no `MusicValue` change, no reader redeploy and no legacy alias. ADR-014 already has the right per-name semantics.
- **Validating at the edges** keeps illegal names out of new writes without making history unreadable.
- **The server as master** preserves ADR-014's assumption that time only moves forward, which latest-wins depends on. Client clocks (laptops, or Pis without an RTC) cannot break ordering.
- **Pushing through the library** puts validation where the projection is. ACID stays a plain writer, and there is one write path with its gate.
- **Deduplicating by content** is enough for values that carry their own time, and needs no schema change.
- **Per-node replicas** follow from per-node logs and cursors, and avoid inventing cross-node ordering before anything needs it.

## Alternatives considered

- **A new `Marked { mark, timestamp }` variant.** Rejected:
  - every reader would have to be redeployed (ADR-004);
  - history cannot be rewritten, so `Bookmarked` would need an alias forever;
  - the only gain is the name.

  A Rust-only rename with `#[serde(rename = "Bookmarked")]` is still possible (Open questions 2).
- **`MarkName` inside `MusicValue`.** Rejected: deserialising a legacy free-text scope could fail and make the log unreadable.
- **`ListMarked { mark: Option<Mark> }`, with `None` meaning any mark.** Rejected: on the fact, `None` already means the default mark.
- **Client-supplied ACID timestamps.** Rejected by decision 2.1.
- **Raw `WriteFacts` through the gateway.** Rejected: there is no validation, any attribute could be written, and it bypasses the redirect and `retire_gate`.
- **An idempotency key on `Fact`** (a stainless_facts change). Deferred (2.4).
- **A merged multi-node log, CRDTs or vector clocks.** Deferred: there is no ordering across nodes' ACID clocks, and no stated need yet.

## Consequences

- **Protocol changes:**
  - `WriteMark`, `UnmarkTrack`, `ListMarked` and `PushFacts` are added.
  - `ListBookmarked` is removed, and `WriteBookmark` is deprecated.
  - Gateway, library, console, CLI and TUI are redeployed together (ADR-004).
  - Stage2 must be redeployed to drop `ListBookmarked`.
- **The fact schema does not change** in phases 1–3.
- **The replica branch's gateway write block** is relaxed for exactly one validated request type.
- **There is no authentication**, just as for today's library writes over the gateway on port 5555. Push adds no new trust boundary, but it makes the existing one more visible.
- **Push order.** A batched fact's authoritative time is its push time. An offline change can therefore win over a server change made in between. This is harmless for marks (one per name), and it is the reason the push whitelist starts narrow.
- **Disk:** each client stores about one copy of every node log it follows.
- **ADR-001** may later want an amendment that names client replicas explicitly, as copies fed from the stream.

## Open questions

1. **What time does a batched fact's own value carry** (`Bookmarked.timestamp`, and future values that carry their own time)? The client's observation time, or a time the node sets at ingest?
   - **Proposed:** keep the client's observation time as an advisory value, used only for display ("marked at"). It is never used for ordering, which stays on ACID time (2.1). The node clamps it so it is never later than ingest.
   - The alternative is for the node to overwrite it at ingest. That is simpler and consistent with an online `WriteMark`, but it loses when an offline mark was actually made.
2. **Should the Rust variant be renamed** from `Bookmarked` to `Marked`, with `#[serde(rename = "Bookmarked")]`? The wire format and the attribute stay unchanged, but `mdma facts` would still print `Bookmarked`.
3. **With two decks, which track does a macro mark:** the focused deck, the one on air, or the one most recently started?
4. **Cross-node:** should a mark made on stage show up on prod? If so, it needs a node field on `FactSource`, and a merge rule that doesn't compare ACID times across nodes.
5. **Log identity:** should `ReadStream` expose a log id, such as a hash of the first line? A replica could then detect a different log behind the same node id after a reprovision or rename.
6. **What goes on the push whitelist beyond marks** (cues, energy, play events), when, and with what conflict rule?
7. **Authentication** for gateway writes, push included.

## Phasing

0. Fix #97: the library replies to unparseable requests. **Implemented on feature/marks.**
1. **Marks. Implemented on feature/marks.** Add `MarkName`/`Mark`, `WriteMark` and `UnmarkTrack`, and replace `ListBookmarked` with `ListMarked`. Move the CLI and TUI over. This unblocks the #95 CLI step. Deploy to stage2, then prod. Phase 1 is no longer blocked on open questions.
2. **Read-only replica.** Rebase `feature/offline-library-replica` onto develop, bringing the ADR-014 fold into `library_projection`. Add a read-only replica and cursor per node.
3. **Client push:** the outbox, `PushFacts`, a marks-only whitelist and content dedup.
4. **Cross-node:** merging across nodes, only if Open questions 4 is answered yes.

Macro buttons can ship any time after phase 1 for online use. They work offline after phase 3.

## Related

- ADR-001, ADR-004, ADR-009, ADR-011, ADR-013, ADR-014, ADR-015
- #95: bookmarks list in one request.
- #97: a library parse failure gets no reply.
- feature/offline-library-replica (448b524, 628abb7, 4da80f9)
