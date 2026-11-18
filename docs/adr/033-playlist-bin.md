# ADR-033: Playlist bin

## Status

Accepted, 2026-11-18 (Joakim approved the plan and every default)

Amends ADR-027 §6.1 (delete) and ADR-032 §8 (rename and delete). Builds on ADR-027 (CAS edits, one lock, atomic writes), ADR-004 (successor repair on read; the gateway, library and console embed `library_ipc_protocol`), ADR-013 (dedup sweeps playlists). Related: ADR-016 (outbox).

## Context

- Deleting a playlist unlinks its `.plist` file. `remove_playlist_file` in `library_service` calls `fs::remove_file`, and nothing brings the file back.
- Two handlers delete:
  - `PlaylistDelete { name, version }` (ADR-027 §6.1) checks the version.
  - `PlaylistRemove { name }` has no version, and is kept for old clients and CLI `playlist rm`.
- A playlist is hand-made: comments, short IDs and an order the user chose. A mis-click in the console, or a stale `rm`, loses all of it.
- `PlaylistName` allows only `[A-Za-z0-9_-]`. `playlist_names` and the dedup sweep read only top-level `*.plist` files in `playlists/`.
- Joakim's decisions: deleting should be undoable, with no confirm dialog; there is one place for deleted playlists, and nothing is purged by itself.

## Decision

### 1. One concept: the Bin, with Restore

1. **Decided:** a deleted playlist goes to the **Bin**. From the Bin it can be restored or purged.
2. There is no separate archive and no auto-purge. A binned playlist stays until the user purges it.

### 2. Storage

1. **Decided:** binning renames the file, under `playlists_lock`, to `playlists/.bin/<name>.<unix-ms>.plist`. The bytes are never rewritten, so comments, short IDs and trailing text survive byte for byte. "Never rewritten" refers to the bin step itself: `PlaylistBin` and `PlaylistDelete` read the playlist first, so the binned bytes are the file after the on-read repair (ADR-004).
2. `BinId` is a newtype for `<name>.<unix-ms>`. It parses unambiguously, because a `PlaylistName` cannot contain `.`. Binning the same name twice is fine: the timestamps differ.
3. `.bin/` is a directory, so the top-level `*.plist` listing never sees it. List, Search and the dedup sweep need no new branch.
4. The directory is created when first needed. The rename is atomic (`rename(2)` within one directory tree).

### 3. Every delete bins

1. **`PlaylistBin { name, version }`** is a new request. It is CAS, through `plan_delete`, and answers `PlaylistBinned { id }` (or `PlaylistDeleteConflict { current }`, the ADR-027 shape). It is a new variant so that an old node refuses it as an unknown variant instead of really deleting.
2. **`PlaylistDelete` and `PlaylistRemove` also bin**, on the server. There is one path, and a stale caller loses nothing.
3. Only **`BinPurge { id }`** and **`BinEmpty`** unlink files.
4. **`BinList`** answers `BinEntries [{ id, entry_count }]`. The name and the binned time come from the id (`id.name()`, and the unix ms in `id.binned_at_ms()`), so an entry cannot contradict its id. `entry_count` counts entry lines of the stored bytes.

### 4. Restore

1. `PlaylistRestore { id, as: Option<PlaylistName> }` renames the file back to `playlists/<name>.plist` under `playlists_lock`. `as` gives another name.
2. On success it answers `PlaylistRestored { name }`, the name the playlist now has. If the target name is taken, it answers the existing `PlaylistAlreadyExists` (ADR-027's third 409 shape) and writes nothing. The console then offers "Restore as...". An id that is not in the bin answers `BinEntryNotFound { id }`, which the console maps to 404.
3. The library never picks a name by itself and never suggests one. The user types it.
4. Stale hashes need no special work. The dedup sweep skipped the binned file, so a hash that was merged away meanwhile is fixed by the normal on-read successor repair after Restore (ADR-004).

### 5. Bandcamp links (amends ADR-032 §8)

1. A binned playlist keeps its link, but the link is **inactive** and cannot sync.
2. Restore re-activates the link.
3. Restoring under a new name counts as a rename and updates the link.
4. Purge (and Empty) drops the link.
5. This replaces ADR-032 §8's "deleting drops the link".

### 6. Console

1. **Delete acts at once** and shows an **Undo** toast. Undo is Restore. There is no confirm, because the action can be undone.
2. A **Bin** section under Collections lists binned playlists with Restore and Purge.
3. **Purge and Empty bin do confirm**, because they cannot be undone.
4. Routes:
   - `GET /api/bin`
   - `POST /api/bin/{id}/restore` (body may carry `as`)
   - `DELETE /api/bin/{id}`
   - `DELETE /api/bin`
   - `DELETE /api/playlists/{name}` now bins.
5. Status codes follow ADR-027 §9: 409 for a version conflict (`{current}`) and for a restore name clash (`{error}`), 422 for an invalid name or id, 404 for a missing playlist or bin entry. An invalid bin id gives 422, parsed in the console route handler before the library is asked. An invalid playlist name on the existing `DELETE /api/playlists/{name}` keeps today's 400. A node without the new variants answers 502 (no fallback).

### 7. CLI

1. `mdma playlist rm` is kept and now bins.
2. New: `mdma playlist bin ls | restore <id> [--as <name>] | purge <id> | empty`. `empty` requires `--yes`.

### 8. No change for TUI and dj-workspace

They list through `PlaylistList`, which does not show the Bin.

## Phased plan

- **P0:** this ADR, the amendments to ADR-027 and ADR-032, and a ROADMAP entry.
- **P1:** protocol variants and the `BinId` newtype, with serde and parse round-trip tests.
- **P2:** library service and stub, sharing one contract suite in `playlist_edit::contract`:
  - bin keeps the bytes identical;
  - a stale version gives a conflict;
  - restore round-trips the bytes;
  - a restore clash gives AlreadyExists and writes nothing;
  - restore with `as` works;
  - purge and empty work;
  - binned playlists are absent from List, Search and dedup;
  - Delete and Remove bin.
- **P3:** gateway decoding and console routes, including 409 and 422.
- **P4:** console UI.
- **P5:** CLI (hicli).
- **P6:** stage2. Deploy gateway, library and console together.

## Consequences

- A delete never loses a playlist. A stale `rm` or a mis-click is undone with Restore, and the bytes come back exactly.
- Deploy `mdma-gateway`, `mdma-library` and `mdma-console` together (ADR-027 §9, ADR-004). An old node answers `PlaylistBin` and the Bin requests with 502; it never deletes for real.
- Disk is not reclaimed until the user purges. Playlists are small, so this is cheap.
- Binned playlists are invisible to every reader, so the dedup sweep leaves them alone and Restore relies on on-read repair.
- Old clients that send `PlaylistDelete` or `PlaylistRemove` get a bin instead of an unlink, with the same answer as before.
- Link state in `mdma-bandcamp` must learn "inactive" (ADR-032 phase 3).

## Alternatives considered

- **A flag inside the file** (a `# binned` line). Rejected: it means rewriting the file, and a filter in every reader (List, Search, dedup, repair).
- **A separate archive next to the Bin.** Rejected: two concepts for one need. One Bin with Restore is enough.
- **Auto-purge after N days.** Rejected: silent deletion is the thing this ADR removes.
- **An optional `version` on `PlaylistRemove`, or a flag on `PlaylistDelete` to bin.** Rejected for the reason in ADR-027's alternatives: serde ignores unknown fields, so an old library would really delete. A new variant refuses.
- **A confirm dialog on delete.** Rejected: Undo is cheaper for the user and the action is reversible.
- **Restore picks a free name** (`name-2`). Rejected: the library does not invent names. The user types the new one.
