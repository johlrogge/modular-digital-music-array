# ADR-026: Playback state and play facts hold only canonical content hashes

## Status

Proposed, 2026-11-16

Builds on ADR-017 (play facts; its Context and §6.4 are corrected by this ADR), ADR-004 (readers and writers deploy together), ADR-009 (gateway compatibility). Related: ADR-011 (read-your-writes), ADR-013 (duplicate merge).

## Context

- A content hash has one canonical form: `sha256:` followed by 64 lowercase hex digits. Everything user-facing also accepts a prefix (8 or 12 hex characters, with or without `sha256:`); the library resolves it (`resolve_prefix`, reached through `get_track`).
- **Short hashes got into playback.** `mdma queue append 9fb4105e` sent `9fb4105e` to playback. Playback queued it verbatim (QueueNext, QueueAppend and QueueReplace in projects/mdma-playback/src/server.rs), persisted it in the queue file, and wrote it into `TrackStarted` and `TrackStopped` facts and into `PlaybackEvent`s. The 12-character hashes that `format_track_line` prints (and `queue edit` and `queue replace -` read back) did the same.
- **Damage:**
  - the web console shows "Unknown track" for a queue entry it cannot resolve;
  - exact-match QueueRemove fails between the short and the full form of one track;
  - plays are lost, and a short play key can later belong to a different track, because a prefix is only unique until another track shares it.
- **#86 is not history.** library_projection credits plays recorded under an 8- or 12-character key to the track they are a prefix of (the #86 workaround), and ADR-017 describes those entities as written by "older playback". That is wrong: the writer was still live. Every `queue append <short>` after the workaround went in wrote another short line, so the workaround treated a symptom of a bug that kept producing it.

## Decision

### 1. One resolver, in the client

1. **Decided:** a client that sends a hash to playback resolves it first through the library, the single resolver, with `get_track(hash)` and the returned `content_hash`. There is no second prefix-resolution implementation.
2. One helper does it for all clients: `library_ipc_client::resolve_full_hashes(hashes, get_track)`, returning `Vec<CanonicalHash>`. It resolves **all** hashes before any is returned and fails on the first that is unknown, ambiguous, or that the library answers with a hash that is not canonical (`ResolveError::NotCanonical`; legacy short-hash entities exist in the index). Nothing is sent until every hash resolves. Only QueueReplace and QueueRemove are atomic in playback; a client that sends QueueAppend or QueueNext once per hash relies on this up-front resolution to avoid a half-sent batch.
   - **Decided:** input that is already canonical is returned unchanged, without asking the library. Only prefixes go to `get_track`. A queue entry whose track the library has lost (for example one shown as unavailable in `queue edit`) therefore survives a round-trip, and a canonical hash costs no lookup.
3. Callers: every `mdma queue` subcommand that sends hashes (`next`, `append`, `remove`, `replace`, `edit`), the old web console's queue routes (append, remove, replace, playlist-to-queue), and the new console's `/api/queue/{next,last,edit}` and `/api/play-now` (once feature/console-redesign is merged). The BDD world resolves its tokens the same way (`library_hash`). `queue remove <short>` resolves too, so it matches the full hash in the queue. The console asks the library through the gateway client, not a direct library socket, so a fake gateway can stand in for both services.
4. Library-bound commands keep sending prefixes; the library resolves them on arrival. `ContentHash` stays one type. The hash argument newtypes in the CLI still accept a prefix.
5. The TUI takes hashes from `TrackInfo` values the library returned, but those can still be legacy short hashes, so it checks them too: every hash it sends to playback goes through the same helper (`LibraryBackend::resolve_for_playback`), which checks that it is canonical, and a failure is shown in the status line rather than dropped.

### 2. Playback validates shape only

1. **Decided:** `music_primitives::CanonicalHash` is `sha256:` plus exactly 64 lowercase hex digits, built only through `TryFrom<ContentHash>`. Playback state (`PlaybackState`, `PlaybackEffect`), `QueueEntry` and the play facts written by `write_fact` hold `CanonicalHash`, so a prefix cannot be represented there. Events convert to the wire `ContentHash` when they are emitted.
2. The commands that carry hashes to hold (QueueNext, QueueAppend, QueueReplace, QueueRemove) are checked once, at the command boundary, by one function (`media_protocol::require_canonical`). A hash that is not canonical gets `Response::Err`: "not a full content hash: <h>; resolve it through the library". QueueReplace is all or nothing: one prefix leaves the old queue as it was. The BDD playback simulator uses the same function.
3. **Playback has no library dependency.** It does not resolve; it only refuses. That keeps the single resolver in the library and playback usable when the library is down.
4. `load_queue` drops persisted entries that are not canonical, with a `warn!`, so a queue file written by an older playback does not put a prefix back into the new state. The wire type of every command and event is unchanged (`ContentHash`), so old clients still parse responses.

### 3. Deploy order: CLI first, then playback

An old CLI sends prefixes; a new playback refuses them. Roll out the clients (CLI, console, TUI) first. They resolve, which an old playback accepts as before. Then deploy playback. Gateway and playback need no ordering between them, since `Command` does not change.

## Consequences

- No new short-hash play lines are written. `TrackStarted`/`TrackStopped`/`PlayStarted`/`PlayStopped` entities from playback are full hashes from the playback release that carries this ADR.
- **The #86 reader workaround stays.** The log still holds short-key lines written before this change; ADR-017 §6.4 keeps their entities as written, and library_projection keeps crediting 8- and 12-character keys when exactly one full key starts with them. It no longer needs to grow: nothing writes new ones. Its doc comments say where the lines come from.
- A client that sends a prefix to the new playback gets an error that names the fix, instead of silent corruption.
- A prefix for a track the library no longer knows cannot be sent through `queue edit` or `queue replace`: resolution fails on the first, naming it. Remove that line and save again. A full hash passes through unchanged.
- The console answers 422 with the resolver's message when the library refuses the hash (unknown, ambiguous, or answering with a non-full hash), and 503 when the library cannot be reached (a transport failure), before playback is contacted. The CLI reports both as the error text.
- Persisted queue entries that were prefixes are dropped at the first start of the new playback, with a warning.
- `queue edit` writes the full hash for entries the library cannot show, so they survive the round-trip. When a save is rejected, the edited text is kept in a file whose path is in the error.
- ADR-017's claim that short-hash entities came from "older playback" is corrected in its Context and §6.4.

## Alternatives considered

- **Playback resolves prefixes itself** (calls the library). Rejected: a second resolver is a second place the rule can drift, and playback would gain a dependency and fail when the library is down.
- **Resolve in the gateway.** Rejected: the gateway forwards `Command` unchanged (ADR-017), and direct IPC clients bypass it.
- **Normalise a prefix in playback by zero-padding or guessing.** Rejected: a prefix is only unique with the library's whole track set in view.
- **Keep accepting prefixes and only widen the reader workaround.** Rejected: plays recorded under a prefix can move to another track later, so no reader can make them right.
- **Split `ContentHash` into a prefix type and a full type across the codebase.** Rejected for now: library-bound commands send prefixes on purpose. Only the places that must hold full hashes use `CanonicalHash`.
