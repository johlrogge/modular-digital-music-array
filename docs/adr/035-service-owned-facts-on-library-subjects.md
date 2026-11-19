# ADR-035: Service-owned facts on library subjects

## Status

Accepted 2026-11-19. Not yet implemented.

- **Supersedes:**
  - the interim amendments of 2026-11-19 to ADR-032 and ADR-033, which prune orphan links on read;
  - ADR-032 §5.2–5.3, "playlists are not fact entities".
- **Builds on:** ADR-001, ADR-004, ADR-018, ADR-020, ADR-027, ADR-033.
- **Related:**
  - ADR-016, outbox and replicas;
  - ADR-034, the CLI-local rekordbox base, which this ADR does not affect.

## Context

- **Every fact names a track today.**
  - Readers parse `Fact<ContentHash, MusicValue, FactSource>`, and `MusicValue` is a closed enum.
  - ACID itself stores `Fact<String, Value, Value>`, so the log is schemaless but its readers are not.
  - A fact whose entity is not a track would fold into a ghost track, because an entity counts as live as soon as it has any provenance.
- **mdma-bandcamp keeps its playlist state in a private `playlist-links.json`.** For each playlist that is the link, base, pending items, match cache and Bandcamp id. These are keyed by playlist **name**.
- **The library performs bin, restore, rename and purge.** So the link lost track of which playlist it belonged to.
  - On stage2 a binned `chill-openers` left its link and stale base behind.
  - A new, empty `chill-openers` inherited both, and the next sync read every base item as a local deletion.
  - An interim fix had clients notify the source after each lifecycle change. It missed the TUI, `PlaylistRemove`, a source that was down, and a sync in flight.
  - The interim that shipped instead prunes orphan links on read. It still has gaps: see the ADR-032 and ADR-033 amendments.
- **A name is not an identity.**
- **Joakim (2026-11-19):** services should be able to attach whatever facts they like to library subjects, without the library knowing which service wrote them or what they mean: "one of the biggest benefits with schemaless". The library handles the facts' lifecycle as part of the playlist change.

## Decision

### 1. A playlist has an id for its whole life

1. **The id.** `PlaylistId`, a newtype over a UUID, identifies one playlist's life.
   - It is stored in the playlist file as a library-owned comment line: `# mdma-playlist-id: <uuid>`.
   - The fact entity is `playlist:<uuid>`.
2. **When the line is added.** The library adds it the first time a fact is written for that playlist, under `playlists_lock`.
   - That is the only place the line is created.
   - Adding it changes the playlist's version once.
3. **Writes keep the line.** Every write preserves the current file's id line.
   - Whole-content writes (`PlaylistReplace`, `PlaylistRevert`) drop any id line in the content the client sent and keep the current one.
   - Edit ops cannot address comment lines.
4. **Lifecycle operations need no fact writes.** Bin, restore (also `--as`) and rename move the file with `rename(2)` or a hard link, and the id travels with it.
5. **A reused name starts empty.** A playlist created under a name that was used before has no id line, so it has no facts.
6. **Hand-copied files.** If two live files carry the same id, the library gives the newer file a fresh id when it lists playlists.
7. **Which facts are served.** A subject is live when a top-level `.plist` carries its id, and only live subjects' facts are served.
   - Facts of a binned subject are kept, and served again after Restore.
   - Orphans are never served.

### 2. Service facts

1. **Shape.** A service fact is `(subject, ServiceKey, OpaqueValue, FactSource)`.
   - `ServiceKey` is `<namespace>.<name>`, with namespace matching `[a-z0-9-]+` and name matching `[a-z0-9_-]+`. Both are validated when parsed.
   - `OpaqueValue` wraps a JSON value of at most 64 KiB.
   - The wire value is `{"t":"Service","v":{"k":"<key>","v":<json>}}`.
2. **The library never interprets keys or values.** They are not in `TrackInfo`, search or `FactKind`.
3. **Ownership is source identity** (ADR-018, ADR-020).
   - There is no namespace registry.
   - By convention, each namespace is written by one source.
4. **Latest wins per (subject, key, source identity).**
   - An assert replaces the current value.
   - A retract equal to the current value clears it.
   - `Set` with an identical value writes nothing.
5. **Typing.** Playlist facts are typed `Fact<PlaylistId, ServiceValue, FactSource>`, so track attributes cannot be stated about a playlist.

### 3. Requests

1. **`PlaylistCommit { name, expect: Version(v) | Live, ops, facts: [Set | Clear | ClearNamespace], source }`.** Under `playlists_lock` it does the following, in order:
   - check `expect`;
   - apply the ops (ADR-027);
   - make sure the id line is present;
   - write the file atomically;
   - write the facts to ACID.

   The catch-up runs after the lock is released, so `cursor` is never taken under `playlists_lock`. The answer is ADR-027's `EditOutcome` plus `subject`, or `Conflict`.
2. **`Version(v)` also pins the playlist's life.** The version is a hash of the bytes, and the bytes include the id. A commit computed for a playlist that was then binned and recreated is a conflict.
3. **Reads select facts by key prefix.**
   - `PlaylistFacts { name, namespace }` answers `{version, subject, facts}` from one locked read.
   - `PlaylistsWithFacts { namespace }` answers the same for every live playlist.
4. **Purge and empty.** `BinPurge` and `BinEmpty` retract every live fact of each purged subject, each with its own source, by reading the subject with `ReadEntity` (the ADR-004 pattern).
5. **Crash ordering.** The file is written before the facts.
   - A crash in between leaves the facts unchanged. For a sync that means a stale base, which ADR-032 §6.5 already accepts.
   - A crash during a purge leaves facts on an id that no file carries, and those are never served.

### 4. Projection and replicas

1. **Sort lines before parsing.** `library_projection` decides by entity whether a stream line is a track fact, a playlist fact or unreadable, before parsing its value. A playlist line never becomes a track.
2. **One fold for subject facts.** A `SubjectFacts` fold, implementing §2.4, sits next to the track fold. The service, the stub and the replica all use it.
3. **Replicas.** These lines reach replicas as part of the log, and replicas fold them.
   - Older clients count them as unreadable (ADR-020 §7).
   - Upgrade the CLI and TUI together with the node.

### 5. Bandcamp

1. **Keys.** These `bandcamp.*` keys replace `playlist-links.json`:
   - `link`
   - `playlist-id`
   - `base`
   - `pending`
   - `matches`
   - `last-synced`

   They are mdma-bandcamp's vocabulary, not the library's.
2. **`pending_base` is dropped.** A sync commits its ops and the new base in one `PlaylistCommit` with `expect: Version`.
   - On Conflict, the sync re-reads the facts.
   - If `bandcamp.link` is gone, the sync stops with `PlaylistNotLinked`.
3. **Link and unlink.**
   - Linking is a commit that sets the link and the id and clears the rest, so it always starts fresh.
   - Unlinking is `ClearNamespace("bandcamp")`.
4. **The interim goes away.** Prune-on-read (`live_links`, `retain_existing`) is removed.
5. **Migration.** At startup, after the library has been upgraded, mdma-bandcamp:
   - commits each live link's state with `expect: Live`;
   - drops links whose playlist is missing;
   - renames the file to `playlist-links.json.migrated`.

   Running the migration twice writes nothing.
6. **Later.** `matches` may move to per-track `bandcamp.track-id` facts (via `MusicValue::Service`). It is not part of this ADR.

## Consequences

- **Links follow their playlist.**
  - Through bin, restore, `--as` and rename, from every client: the CLI, the console, the TUI.
  - Also while the source is down.
  - A reused name never inherits a link.
- **A losing sync cannot bring a link back.** If a sync loses a race with a bin and a recreate, it gets a conflict.
- **The base moves with the edit.** It is written if and only if the sync's edit is applied.
- **Any service can attach facts to a playlist** without a change to the library.
- **Playlist files change.** A file gains one library-owned comment line the first time it gets facts.
- **Hand-editing risks.**
  - A hand-made copy gets a new id.
  - Deleting the id line by hand orphans the facts. That is the safe direction: the link is lost, and re-linking is a first sync.
- **Log growth** is about 10 KB per sync that changes the base. An unchanged value writes nothing.
- **Deployment.** Deploy gateway, library, console and bandcamp together (ADR-004, ADR-027 §9).

## Alternatives considered

- **Name as the subject, with facts re-keyed on bin, restore and rename.** Rejected: a file rename and a fact append cannot be atomic, so a crash between them hands the facts to the next playlist with that name.
- **An xattr holding the id.** Rejected: editors that save via rename lose it, and so do rsync without `-X` and atomic writes.
- **Service state as comment lines in the plist.** Rejected: it puts service state into the user's text, and TUI Replace drops it.
- **Retract plus assert on every change.** Rejected: it doubles the log for large values. Latest wins was chosen (Joakim, 2026-11-19).
- **Clients tell the source after each lifecycle change.** Rejected: the TUI, `PlaylistRemove`, a source that is down and a sync in flight all miss the message.
