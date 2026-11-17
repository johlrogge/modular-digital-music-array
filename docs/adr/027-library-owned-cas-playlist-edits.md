# ADR-027: Library-owned compare-and-swap playlist edits

## Status

Accepted, 2026-11-17 (Joakim decided)

Builds on ADR-026 (A2 PlaylistEntries, A3 files never canonicalised, A6 TUI save format, A7 status codes, A8 deploy), ADR-004 (successor repair on read; readers and writers deploy together, and the gateway, library and console all embed `library_ipc_protocol`, so the gateway decodes `LibraryRequest`). Related: ADR-013 (dedup rewrites playlists), ADR-009 (gateway compatibility).

## Context

- A playlist is a `.plist` text file under `<metadata>/playlists/`. Each entry line starts with a hash token (short or full) and may be followed by text: `format_track_line` writes `<hash12>  Artist - Title`, and the TUI writes `<token>  # <reason>` for an unresolved row (ADR-026 A6). Comment and blank lines may appear anywhere.
- **No writer checks a version.**
  - PlaylistReplace overwrites the file.
  - PlaylistAppend reads the file and then writes it back.
  - PlaylistRemove deletes it unconditionally.
  - When two clients edit one playlist, the last write wins and the other change is lost without a trace.
- **Three writers, none serialised**, all in `library_service`:
  - The IPC handlers: PlaylistNew, Append, Replace, Rename and Remove.
  - The on-read repair (`read_playlist` → `repair_playlist_content`, the ADR-004 successor repair). Any read can write.
  - The dedup rewrite (`rewrite_playlists_replace_hash`, ADR-013). It sweeps every playlist.
  - A repair or dedup pass that reads before a Replace and writes after it puts the old content back.
- **Writes are not atomic.** `std::fs::write` truncates and then writes, so a crash or power loss on the Pi leaves a truncated playlist.
- **The TUI rewrites the whole file** with PlaylistReplace after every edit. The `# reason` save format exists only so that unresolved lines survive that round trip.
- The web console's playlist workbench needs add, remove, reorder and undo, on playlists that the TUI, the CLI and dedup can also change.
- Joakim's decisions:
  - Playlist files keep the short or full IDs they were written with and are never canonicalised (ADR-026 A3). An edit does not rewrite lines it did not touch.
  - Web edits keep comments. Comment lines, blank lines and the trailing text of entry lines are kept byte for byte.
  - Duplicates are skipped on add. An explicit "Add again" adds them anyway.

## Decision

### 1. Version token

1. **Decided:** `PlaylistVersion` is the sha256 of the playlist file's bytes as stored, taken after on-read repair. It is a newtype, sent as a hex string on the wire. Clients treat it as opaque and only compare it for equality.
2. It is computed on every read and never stored. Any change by any writer, including the TUI's Replace or a hand edit over SSH, changes it. There is no counter to keep in step.

### 2. Snapshot

1. `LibraryRequest::PlaylistSnapshot { name }` answers `LibraryResponse::PlaylistSnapshot(PlaylistSnapshot { version, entries: Vec<PlaylistEntry> })`.
2. `entries` is the ADR-026 A2 read model: file order, duplicates kept, unresolved entries in place, one per entry line. The version and the entries come from the same read, taken under the lock (§7).
3. Positions in edits are 0-based indices into `entries`. Comment and blank lines cannot be addressed.

### 3. Edit ops

```rust
LibraryRequest::PlaylistEdit { name: PlaylistName, version: PlaylistVersion, ops: Vec<EditOp> }

enum EditOp {
    Insert  { at: usize, hashes: Vec<ContentHash>, duplicates: Duplicates },
    Remove  { at: usize },
    Move    { from: usize, to: usize },
}
enum Duplicates { Skip, Keep }
```

1. Ops apply in order. Each sees the result of the one before it, so its positions refer to that state. A position out of range refuses the whole edit (`ProtocolError::InvalidEdit { op, reason }`) and nothing is written. An empty `ops` list is the identity edit: it answers Applied with the same version and writes nothing.
2. **Insert** resolves every hash against the library's visible tracks with `library_projection::resolve_visible`, the ADR-026 A1 "add" set, by the same rule as `resolve_entry`. One unknown, ambiguous or non-canonical hash refuses the whole edit with the typed error (`TrackNotFound`, `AmbiguousHash` or `HashNotCanonical`; the console answers 422). New lines are written by `format_track_line`, the form the CLI and TUI already write (line breaks in titles become spaces). `at == len` appends after the last entry.
3. **Duplicates** are compared on resolved canonical hashes, on both the existing and the new side. Existing entries are resolved with `resolve_entry`, and an unresolved entry never counts as a duplicate. `Skip` leaves out hashes already in the playlist and repeats within the same Insert. `Keep` inserts every hash; it is what "Add again" sends.
4. **Remove** and **Move** act on the entry line only. A comment above an entry stays where it is. Move trades places among the entry lines' slots, so no comment or blank line moves.

### 4. Edits preserve lines

1. **Decided:** edits work on `PlaylistDoc`, a `Vec<Line>` with `Line::Entry(StoredLine)` or `Line::Other { raw }`, plus whether the file ends in a newline. `StoredLine` is a validated single entry line. Entry lines are classified by the same rule as `playlist_hashes`: one definition. The on-read repair (successor chain) and the dedup rewrite use the same parser: both go through `replace_tokens`, which changes only the hash token of the lines it names and returns nothing when no line changes. So they keep CRLF, comments and trailing text byte for byte, and a file that needs no repair is passed on as it was read.
2. `render(parse(bytes)) == bytes` for every input; an exhaustive test over all strings up to length 7 checks it.
3. An op touches only the lines it names. Every other line stays byte-identical, including short IDs, trailing text, `# reason` lines, comments and blank lines.
4. An insert into a CRLF file writes LF lines; readers trim the `\r`, so the mix is harmless.
5. A document with no lines renders as the empty file whatever it was before, so removing the only entry and then inserting gives the same bytes as doing both in one edit.

### 5. Outcomes and undo

```rust
LibraryResponse::PlaylistEdited(EditOutcome)
enum EditOutcome {
    Applied  { snapshot: PlaylistSnapshot, skipped: Vec<Skipped>, previous: Option<StoredPlaylist> },
    Conflict { current: PlaylistSnapshot },
}

LibraryRequest::PlaylistRevert { name: PlaylistName, version: PlaylistVersion, content: StoredPlaylist }
```

1. **Conflict is an outcome, not an error.** If the version does not match, nothing is written and the current snapshot comes back, so the client can redraw without another round trip. The console answers 409.
2. Each `skipped` item names a hash that `Skip` left out and the position of the entry already holding it. Positions are 0-based, in the state after the op that skipped it (not after the whole edit). The console offers "Add again", which resends that Insert with `Keep`.
3. **Undo is a revert to the whole previous file.** Playlists are small, so `Applied.previous` holds the exact bytes the file had before the edit (`StoredPlaylist`, a newtype over the text). To undo, the client sends `PlaylistRevert { name, version, content: previous }` with the version it last saw. The revert goes through the same version check as an edit and answers `PlaylistEdited(EditOutcome)`. If someone else edited in between, the answer is Conflict and nothing is written: undo never overwrites. The content is written verbatim.
4. The revert's own `Applied.previous` is the file before the revert, so redo needs no extra work. The client holds the undo stack as a list of `previous` values.
5. **`previous` is present only when the edit wrote** (the plan decides: the bytes differ). An edit that leaves the bytes as they are (everything skipped, no ops, a move onto itself) answers `Applied` with the same version and `previous: None`, so there is nothing to undo. The client pushes `previous` when it is there and compares no versions. This matches the queue routes, where `previous` is present exactly when Undo makes sense. The console leaves the field out of the JSON when it is `None`.

### 6. Create, rename, delete

1. **`PlaylistDelete { name, version }`** is a new variant. It answers `PlaylistDeleted`, or `PlaylistDeleteConflict { current }` (with the current snapshot) when the version does not match; the console answers 409. `PlaylistRemove` stays as it is, unconditional, for old clients and CLI `playlist rm`.
2. **Rename** keeps `PlaylistRename { from, to }` without a version. It does not change content, so it cannot lose an edit. The check that the target exists and the `rename(2)` happen under the lock. An editor still holding the old name gets PlaylistNotFound on its next edit (404).
3. **Create** keeps `PlaylistNew`. The existence check and the write happen under the lock, and the write is atomic (§7). An invalid name is refused by `PlaylistName::new` (422).

### 7. One lock, atomic writes

1. **Decided:** a single `playlists_lock` in the library service is held by every playlist writer and every read-modify-write:
   - the IPC handlers: New, Append, Replace, Edit, Revert, Delete, Remove, Rename;
   - the on-read repair: read, repair and write are one critical section, and Get, Append, Snapshot and Entries read under the lock (Append appends to the repaired content);
   - the dedup rewrite, for its whole sweep.
2. Lock order: `playlists_lock` before `tracks`, since repair and Insert resolution take `tracks` inside it.
3. **Decided:** every playlist write goes through one function, `write_playlist_atomic(path, bytes)`, built on `private_file::write_file_atomic(path, bytes, 0o644)`. It writes a sibling temp file, fsyncs it and renames it over the target. The temp names end in `.tmp`, so the `.plist` listing ignores them. `write_playlist_atomic` itself creates the playlists directory if it is missing (`write_file_atomic` does not). A read error other than "not found", including content that is not UTF-8, is `Internal`, not PlaylistNotFound.
4. Hand edits over SSH bypass the lock. The version check still catches them before an edit lands.

### 8. The `playlist_edit` component

1. A new pure component with no IO. It holds `PlaylistDoc` parse and render and `apply(doc, ops, tracks, to_info) -> Result<Edited { doc, skipped }, EditError>`. The caller passes the library's tracks (`&[T: HashedTrack]`) and a `T -> TrackInfo` for writing new lines; hashes resolve inside `apply` with `library_projection::resolve_entry_track` (the rule behind `resolve_entry`, over `resolve_visible`), so the service and the stub cannot drift apart. `apply` does not compute undo. A `plan` module builds on `apply` and answers each request from the file's current bytes: `entries`, `snapshot`, `plan_edit`, `plan_revert`, `plan_delete`, `appended` (the one rule of Append). A `Planned` is either an answer with nothing to write or bytes to write with the answer. The contract scenarios (`playlist_edit::contract`, one `#[test]` each through `playlist_contract_tests!`) are behind the `test-support` feature, and the service and the stub run them as dev-dependencies.
2. The wire types (`EditOp`, `Duplicates`, `StoredPlaylist`, `PlaylistVersion` with `of(bytes)`, `PlaylistSnapshot`, `EditOutcome`) live in `library_ipc_protocol`, because the gateway decodes them. `plan` decides whether anything changed: it answers with nothing to write when the rendered bytes equal the old ones, so the version stays the same. `StoredLine` (one validated entry line) is internal to `playlist_edit`, not on the wire. `playlist_edit` depends on the protocol, `library_projection` and `track_formatter`.
3. `library_service` and `library_service_stub` both call it, so console and BDD tests run the same rules. The stub holds its playlists in memory, and implements every playlist request: List, Get, Entries, Snapshot, New, Append, Replace, Edit, Revert, Delete, Rename and Remove. Its one mutex on the playlists plays the part of `playlists_lock` (taken before `tracks`).

### 9. Gateway, console, deploy

1. The gateway decodes `LibraryRequest` (ADR-004: it embeds `library_ipc_protocol`), so it needs the new variants.
2. Console routes:
   - `GET /api/playlists/{name}` reads PlaylistSnapshot and returns the version along with the entries.
   - `POST /api/playlists/{name}/edit {version, ops}` forwards the ops unchanged.
   - `POST /api/playlists/{name}/revert {version, content}` sends PlaylistRevert, the undo.
   - `DELETE /api/playlists/{name}?version=` sends PlaylistDelete.
3. Status codes follow ADR-026 A7, plus:
   - 200 Applied;
   - 409 Conflict, with `current`;
   - 422 for a refused hash, an invalid edit or an invalid name;
   - 404 for a missing playlist.
4. **Decided:** there is no fallback to PlaylistReplace. Against an old node, the request fails as an unknown variant and the console answers 502.

### 10. Queueing a playlist with unavailable lines

**Decided (Joakim, 2026-11-17):** queueing a playlist appends its playable entries after what is already in Up next, skips its unavailable entries and reports them: the count, and the 0-based position and error of each. It does not fail the batch, and it never clears the queue: with nothing playable, nothing is appended and the queue stays as it is. This amends ADR-026 A4. (CLI `playlist get | queue replace` is an explicit replace the user asks for, and is unchanged.)

The rule lives in Rust, in `library_ipc_client` next to `entries_for_playback`, which returns the playable canonical hashes in order (duplicates kept) together with the skipped entries. Every client that queues a playlist uses it. The browser holds no rule. The append is per entry (`QueueAppend` takes one hash) and not atomic: a failure partway leaves the earlier entries queued, and a retry queues them again. An atomic batch append is tracked in #104. Positions are 0-based on the wire; the browser adds 1 when it shows one.

### 11. Later: the TUI moves to ops

The TUI, and CLI `playlist` edits, keep PlaylistReplace for now. Later the TUI reads PlaylistSnapshot and sends PlaylistEdit with a version. Once it does, unresolved lines are never re-serialised by a client, and the A6 `# reason` save lines are no longer needed. This needs an issue.

## Consequences

- Deploy `mdma-gateway`, `mdma-library` and `mdma-console` together, as in ADR-026 A8 and ADR-004. An old node answers the new requests with 502. The console does not fall back to Replace, so a version check is never silently skipped.
- Concurrent edits from the console are safe. The loser gets 409 with the current playlist and can retry; nothing is overwritten.
- The repair and dedup writers can no longer interleave with an edit. Their writes change the version, so an edit computed before them conflicts instead of undoing them.
- A crash during a write leaves the old file or the new one, never a truncated one.
- Comments, short IDs and unresolved lines survive web edits unchanged. ADR-026 A3 holds for edits.
- Until the TUI migrates, a TUI save is still a whole-file Replace with no version. It changes the version, so a console edit made at the same time conflicts, but the TUI itself can still overwrite a console edit. The `# reason` save lines stay until then.
- Undo is exact: a revert to `previous` puts back the same bytes, and the version check means it never overwrites someone else's edit. The browser only stores and resends the `previous` values it was given (no JS test harness is needed).
- Queueing a playlist never fails because of one lost track; the skipped lines are reported to the user.

## Alternatives considered

- **Compare-and-swap in the console**, like Up next's `queue_edit` (QueueList, compare against `expect`, then QueueReplace). Rejected: the check and the write are two requests, so another writer can land between them. It is racy by construction, and it puts the rule outside the library, where the stub, the TUI and the CLI cannot share it. (Up next has the same race; worth an issue.)
- **Whole-file replace with a version** (`PlaylistReplace { name, version, content }`). Rejected: the client rebuilds the file, so it has to round-trip comments, short IDs and unresolved lines itself. That is the reason the `# reason` format exists. Duplicates and undo would be computed in each client.
- **An optional `version` on the existing `PlaylistRemove`.** Rejected: serde ignores unknown fields, so an old library would accept a versioned remove and delete without checking. A new variant makes an old node refuse it instead.
- **Undo as inverse ops** (Insert undone by Remove, Remove by a Restore of the removed line, Move by the reverse Move). Rejected: the inverses are not exact once comments and blank lines are in play, because a position among entry lines does not say where the non-entry lines around it were (the gap problem). A whole-file snapshot (`previous`) is simpler and always byte-exact; playlists are small.
- **A per-playlist counter stored next to the file.** Rejected: hand edits and old writers would not bump it. Hashing the bytes catches every change.
- **Fail the whole queue batch on the first unavailable line** (ADR-026 A4 as first written). Rejected by Joakim: one lost track should not block the rest of the playlist.

## Amendment 2026-11-17: queue actions in the console

Settles how the console queues a playlist or a list of tracks. It refines §10 and ADR-026 A4's amendment.

1. **Actions.** There are four, the same for a playlist (`POST /api/playlists/{name}/queue {action}`) and for a list of hashes (`POST /api/queue {action, hashes}`):
   - `next`: the set goes to the front of Up next, in the order given.
   - `last`: the set goes to the end of Up next, in the order given.
   - `replace`: the set becomes the whole of Up next, in one atomic `QueueReplace`. The track that plays keeps playing: playback replaces only its queue.
   - `now`: the set goes to the front and starts, only while nothing is loaded (409 `busy` otherwise, before any lookup).
2. **An empty set never changes Up next.** This holds for every action and both routes, and is structural: the console runs an action only on a non-empty `TrackSet`. A playlist with nothing playable answers 200 `{queued: 0, queue, skipped}` and sends nothing, `replace` included (it does not clear Up next). A hash list that is empty is a malformed request: 400. In particular `now` on nothing never sends `PlayQueue`, so it cannot start what was already queued.
3. **Names.** A playlist name in the path that no playlist can have is 400, and nothing is sent. A name the library refuses (`InvalidPlaylistName`) is 422.
4. **Resolution.** Hashes in `POST /api/queue` are resolved against the Add set of ADR-026 A1 (the library's visible tracks) for every action, `replace` included. This departs from A1, which lists "Replace/edit" as resolved against queue and library together (a stale queue entry survives, a new one must exist in the library). That rule exists so that an edit of the current queue can keep entries whose track has left the library. `replace` is not an edit: the browser names the whole new queue from tracks it is showing from the library, and nothing in it is kept from the old queue. So every entry must exist in the library, and a track the library has lost is refused (422) rather than put into Up next. `/api/queue/edit` keeps the A1 rule. A playlist's hashes are already resolved by the library.
5. **Undo.** `previous` is present exactly when the change can be undone: next, last and replace. `now` has none, because an Undo cannot stop the track it started. Nothing queued has none either. It is the queue as the console read it just before sending the change, like `previous` in §5. The browser undoes with a CAS edit from the returned `queue` back to `previous`, and never reads its own idea of the queue. Between the read and the change another writer can still move the queue; that change is then not in `previous`. This read-then-replace race is the same as `/api/queue/edit`'s and stays until playback offers an atomic replace that returns what it replaced. `/api/queue/edit` follows the same rule: it answers `{queue, previous}` when it changed the queue, and when the requested queue equals the current one (compared after the hashes are resolved, so a short hash naming the same track is no change) it sends no `QueueReplace` and answers `{queue}` with no `previous`. The browser sees "no `previous`" and says "Nothing changed"; it compares nothing itself.
6. **Two 409 shapes (known debt).** A stale Up next edit answers `{error: "stale", queue}`; a stale playlist write answers `{current}`. They are different because one is a queue of hashes and the other a snapshot with a version. They should be one shape; this needs an issue.
