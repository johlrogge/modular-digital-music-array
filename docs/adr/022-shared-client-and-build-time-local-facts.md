# ADR-022: Shared client and build-time local facts

## Status

Accepted, 2026-11-15 (Joakim approved the plan)

Amends ADR-021 §4 (who depends on `library_projection`) and §6 (refresh and node resolution move out of the CLI). Related: ADR-009, ADR-011, ADR-014 (§14), ADR-016, ADR-020.

## Context

- Every client type needs the same things: resolve a node, connect to its services, follow events, and read facts locally. Today that means CLI, TUI and bevy apps. The MDMA 101 remote (Pi Zero, std Linux) is the first new consumer.
- Today the CLI alone has the full node resolution: `--node`, `MDMA_NODE`, local IPC, the saved selection, then discovery. It lives in `mdma-cli` `node.rs` and `ctx.rs`. The TUI knows only `--node`/`MDMA_NODE`, so on a laptop it fails with "Library connect failed: ipc:///run/mdma/library.sock: Connection refused".
- `components/mdma_client` already exists and is used by the TUI, bevy-app and bdd. `bases/client` holds `ClientConfig`, and a component must not depend on a base.
- ADR-021 put the replica, the snapshot and the sync throttle in `mdma-cli`. Only the CLI depends on `library_projection`, so no other client can reuse the replica.
- A polylith profile picks one implementation per interface for every project in the build.
- No embedded target exists. A future ESP32 (Embassy) target is unplanned. NNG would need rethinking for it (possibly a pure-Rust NNG drop-in Joakim writes, not now).

## Decision

### 1. `mdma_client` is the one client library

- **Decided (Joakim):** every client type uses `mdma_client`: the CLI, the TUI, the MDMA 101 remote and the bevy apps. It is extended, not wrapped in a new façade or split into a new component.
- `mdma_client::Node` holds what the CLI's `Ctx` holds, minus hicli. That is the lazy `Target` and the library, playback, source, admin, gateway and event-gateway connections. It is built from `NodeOptions { node, library_socket, playback_socket, sources_dir }`.
- `mdma_client::node` gets the resolution, the node cache, `Identify` and the error texts from the CLI's `node.rs`. The wording and the pick prompt move unchanged.
- `ClientConfig` moves from `bases/client` into `mdma_client`, and `bases/client` is deleted.
- Errors are a `thiserror` `NodeError`, because a component cannot return `color_eyre`. Its texts are byte-identical to today's. No variant has `#[source]` or `#[from]`, so no message prints twice.
- `mdma_client::events::subscribe` replaces both the TUI's `events.rs` and the CLI's `subscribe.rs`.
- The shared fact types live in `mdma_client::facts`: `Urgency`, `NameField`, `GroupField`, `FactsStatus` and `FactsError`.
- What stays in the CLI: hicli argument newtypes, the `Globals -> NodeOptions` conversion, completion, the thin `node` and `replica` commands, rendering, the write table and the detached spawn (`detach.rs`).

### 2. One resolution order for every terminal client

- Every terminal client resolves its node the same way: (1) `--node`; (2) `MDMA_NODE`; (3) local IPC, meaning sockets given or `/run/mdma/library.sock` exists; (4) the saved selection, verified with `Identify`; (5) discovery.
- With several nodes and no saved selection, the client prompts as the CLI does. It prompts only on a TTY, and a picked node is saved.
- A full-screen client resolves eagerly (`node.target()?`), before raw mode or the alternate screen, so the prompt and its notes reach a normal terminal.
- Socket flags have no default value. A default would count as "given" and force local IPC. `MDMA_NODE` is read by `Node`, not by the argument parser.

### 3. Local facts are a build-time interface

- Local facts sit behind a polylith interface, `client-facts`. The implementation is chosen per profile, at build time, with no trait on top.
- Interface: `ClientFacts { open, names, view, sync, request_refresh, status, reset }` and `FactsView { tracks, track, marked, group_counts, plays, catch_up }`.
- **Now:** one implementation, `client_facts_replica`. It is the ADR-021 replica, moved unchanged out of `mdma-cli`: probe, pick, sync, reach, connect, snapshot and the throttle.
- **Deferred (Joakim):** `client_facts_remote`, which would answer by asking the library, and an `embedded.profile` selecting it. There is no embedded target to justify them. The interface stays swappable so they can be added without touching clients.
- Naming: `client_facts_*`.
- CI builds every profile, so an implementation that does not satisfy the interface fails the build.

### 4. All views derive from `library_projection`

- **Decided (Joakim):** one fold. Every view a client shows comes from `library_projection`, which the library service uses too: tracks, groups, marks and plays. Clients never re-aggregate or re-count facts.
- The pure parts the service still holds move into `library_projection` first: track info and album covers, track fields with the hidden filter, the live projection and the plays fold.
- This amends ADR-021 §4. The dependency on `library_projection` moves from `mdma-cli` into `client_facts_replica`. `mdma-cli` no longer depends on `fact_replica` or `library_projection` directly.

### 5. One throttle, injected executor

- There is one sync throttle, ADR-021 §6's `meta.json` timing, backoff and `sync.lock`. It lives in the implementation, not in each client.
- `ClientFacts::request_refresh(now, urgency, spawn)` decides whether to sync, and the caller provides how. The CLI passes a detached `setsid` spawn. The TUI passes a send to a worker thread. Both share the meta throttle and the flock.
- **Decided (Joakim):** TUI live updates come from events plus a 30 s timer. The worker subscribes to `acid/facts/`, catches up on each event or tick and sends `AppEvent::FactsChanged`.
- This amends ADR-021 §6: refresh is still always detached from the user's path, but the executor is injected, so the throttle serves more than the CLI.

### 6. Read-your-writes without silent fallback

- A view that must reflect the client's own write (ADR-011) gets its answer from the library. Otherwise the local view is shown as stale until the replica catches up.
- **Decided (Joakim):** a stale replica shows as stale. No client falls back silently to the remote node to hide staleness.

### 7. The console does not use `client-facts`

- `mdma-console` is a web service on the node and reads from the library. It gets no replica.

## Why

- One library means one resolution order, one set of error texts and one event subscription. The TUI's laptop failure goes away without any TUI-specific resolution code.
- A build-time interface keeps clients unaware of where facts come from. A remote-only build can come later as a new profile, not a client rewrite. This is how polylith swaps implementations; a runtime trait would add nothing.
- A single fold means client and library cannot disagree on counts, groups or plays. ADR-014 §14's split-point test covers it once.
- An injected executor keeps the throttle's state and locking in one place while letting each client choose a detached process or a thread.
- Showing staleness is honest. A silent remote fallback would mask a broken sync.

## Alternatives considered

- **A new client component beside `mdma_client`.** Rejected. `mdma_client` already has the consumers, and two client libraries would split the same concern.
- **Keep node resolution in the CLI and give the TUI its own.** Rejected. Two resolution orders drift, and the TUI bug is exactly that drift.
- **A runtime trait for local facts.** Rejected. Polylith swaps implementations at build time, and a trait adds a layer no build needs.
- **Build the remote implementation and `embedded.profile` now.** Deferred by Joakim. There is no embedded target, and the ESP32 transport is unsettled.
- **Let each client aggregate what it shows**, for example the TUI counting genres or plays itself. Rejected. It forks the fold. The TUI's genre N+1 (`TrackInfo` had no genre) is a symptom of this.
- **Fall back to the remote node when the replica is stale.** Rejected by Joakim. It hides sync failures.
- **Name the components `local_facts_*`.** Rejected by Joakim in favour of `client_facts_*`.

## Consequences

- `bases/client` is deleted. `dj-workspace`, `mdma-cli` and `mdma-tui` depend on `mdma_client` instead.
- mdma-cli keeps a base: `bases/cli` (`cli-base`) holds process setup (SIGPIPE, color-eyre).
- `mdma_client` depends on `node-discovery`, so `mdns-sd` and its threads become part of what it pulls in. bevy-app and bdd become heavier. Measure with `cargo_tree` and consider a feature gate if it matters.
- `TrackInfo` gains genre, label and styles (serde default), so the TUI can group by them without per-track lookups.
- **Risk, implementation drift:** with only one implementation, nothing checks that a second one would fit the interface. CI building every profile catches this only once a second profile exists.
- **Risk, folding cost:** opening the replica folds about 7 MB of log. Doing that per CLI command may be too slow. Measure before adding a cached projection.
- **Risk, lock contention:** a long-running TUI and CLI shells share `sync.lock`. A TUI that syncs often can make CLI syncs exit as busy. That is correct, but it can delay a refresh the CLI asked for.
- **Risk, remote plays:** a future remote implementation needs a `LibraryRequest::Plays`, or `FactsView::plays` answers `Unsupported`. That is open until the remote implementation is built.
- Play history in the CLI (#99) already shipped in 0.38.0, built directly on `library_projection`'s `LineageLog` (then `PlayLog`) via the CLI's replica. When phase 3 lands it moves onto `FactsView::plays`. That is a move, not new work, and clients still never count facts themselves.

## Phasing

- **0a.** Add genre, label and styles to `TrackInfo` and fill them in the service and stub. Fix the TUI's genre grouping.
- **0b.** Extract the remaining pure folds into `library_projection`.
- **0c.** `mdma-cli`, `mdma-tui` and bdd use `mdma-client = { workspace = true }`.
- **1.** `Node`, resolution and `ClientConfig` into `mdma_client`, and `bases/client` deleted. **1b:** the TUI on `Node`.
- **2.** `client_facts_replica` as a pure move out of `mdma-cli`.
- **3.** `FactsView`. Play history (#99, shipped in 0.38.0 on `library_projection`'s `PlayLog`, now `LineageLog`) moves onto `FactsView::plays`; this is a move, not new work.
- **4.** The TUI on `FactsView`, with live updates (events plus the 30 s timer).
- **Deferred.** `client_facts_remote`, `embedded.profile`, CI for that profile.

0a and 0b go together and 0c runs in parallel with them. Then 1, 2 and 3 run in order. After 3, phase 4 can run. Code is moved without reshaping; the only deliberate reshape is `NodeError`.

## Related

- ADR-009 (node identity)
- ADR-011 (read-your-writes)
- ADR-014 (§14, shared fold)
- ADR-016 (per-node replicas)
- ADR-020 (client-owned fact sources)
- ADR-021 (§4 and §6, amended)
