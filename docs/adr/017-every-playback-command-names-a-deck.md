# ADR-017: Every playback command names a deck; plays are unique values

## Status

Proposed, 2026-11-10

Amends ADR-014 §9 (play events are asserts-only and unpaired; a merge keeps them, as §9 says, and the code now does the same) and makes a one-off exception to ADR-001's append-only log (§6). Builds on ADR-001 (append-only log), ADR-004 (readers redeploy together), ADR-009 (gateway compatibility) and ADR-016 §5 (upgrade message for old nodes). Adopts the identity rule of stainless_facts ADR-001 (johlrogge/stainless_facts#15). Related: the separate change that makes MDMA's fold source-blind.

## Context

**Commands (media_protocol::Command, components/media_protocol/src/protocol.rs:14-83).**
- Carry `deck: Deck` (required): LoadTrack, Play, Stop, Pause, Resume, SetVolume, Unload, Seek, GetLength (15-46).
- Carry no deck and implicitly act on deck A: QueueNext, QueueAppend, QueueList, QueueClear, QueueRemove, QueueReplace, PlayQueue, NowPlaying, Skip, GetSession, ListAudioOutputs, SetAudioOutput, GetAudioOutput (47-82). The comments say so: "queue feeds deck A only" (47), "load on deck A" (67, 69).
- The deck-less commands without arguments are **unit variants**: QueueList, QueueClear, PlayQueue, NowPlaying, Skip, GetSession, ListAudioOutputs, GetAudioOutput (protocol.rs:58-82). On the wire they are bare strings such as `"skip"` (tests at 198-260).
- There is no fast-forward command.

**mdma-playback ignores the deck.**
- LoadTrack, SetVolume, Unload and Seek are rejected: "not supported in split-architecture mode" (projects/mdma-playback/src/server.rs:196-201).
- Play, Stop, Pause, Resume and GetLength match `deck: _` and act on the only deck (202-251).
- There is one `StreamClient` and one `PlaybackStateMachine` (server.rs:58, 63). PlayQueue, NowPlaying, Skip and GetSession use them directly (307-375). The audio-output commands act on the single `StreamClient`'s sink (376-408), which this ADR treats as the master output (§2.5).
- A request for deck B therefore silently acts on deck A.

**Transport.**
- The gateway wraps `Command` unchanged (`GatewayRequest::Playback { request: Command }`, components/gateway_protocol/src/lib.rs:34-35). It parses and **serializes it again** before forwarding (projects/mdma-gateway/src/main.rs:306, 325-326, `forward_typed` 126). Gateway and playback therefore share one `Command` definition.
- stream_source_protocol has no deck: one stream per source instance. Mapping decks to source instances belongs in playback.
- `PlaybackEvent` (components/event_protocol/src/lib.rs:48-71) carries `hash` but no deck. Subscribers: TUI now_playing.rs:56 and app.rs:301, CLI subscribe.rs:97, console templates/index.html:1460.

**Clients that build `Command`.**
- components/media_client/src/lib.rs:45-186
- components/mdma_client/src/playback.rs:38-186
- projects/mdma-cli/src/playback.rs:14-163 (hard-codes `Deck::A` at 23, 33, 43)
- projects/mdma-tui/src/input.rs:198, 338-353, 425-437 (hard-codes `Deck::A`)
- projects/mdma-console/src/main.rs:891-1177 (hard-codes `Deck::A` at 1081, 1100, 1109)
- projects/bdd/src/playback_simulator.rs:102-116 and steps/playback_steps.rs:10-47
- Queue commands: mdma-cli queue.rs and mdma-tui queue_pane.rs.

**Play facts today.**
- `TrackStarted(StartReason)` and `TrackStopped(StopReason { OnRequest, OnCompletion, OnSkip })` (components/music_facts/src/value.rs:8-70, 331-335).
- mdma-playback writes them through `write_fact` (server.rs:39-48), with effects from playback_state.rs:95-98, 116-119, 134-137, 158-161, 176-179, 200-203, 256-268 and 290-293. In practice `StartReason` is always `ByQueue`: playback never writes `OnRequest`.
- The values carry no time, deck or id, so every play of a track is the same value.
- The tags `Played` and `Skipped` are **taken**. `rewrite_legacy` maps them to `TrackStopped(OnCompletion)` and `TrackStopped(OnSkip)` (value.rs:381-396), and such lines exist in old logs (music_facts/tests/fixtures/legacy_facts.jsonl:4-7).
- Playback 0.2.0 wrote the event time as the value (`{"t":"TrackStarted","v":"2026-02-23T11:34:39.496504277Z"}`; `Played`/`Skipped` have the same form). These and any other unknown string are *guessed* as `OnRequest` (value.rs:32-33, 66-67).
- Older playback wrote short-hash entities (#86, 8 or 12 hex characters) and once the empty entity `""`. Readers resolve short hashes by unique prefix (service.rs:3205-3220).
- The only play retracts come from retire batches (`retract_all_entity_facts_locked`, service.rs:2359-2395), which the hard retract in `merge_into` uses. They re-serialise the parsed value, so a 0.2.0 value is retracted as `"OnRequest"`.
- Stage2 (2026-11-10): 1,466 play lines out of 22,959. 721 `TrackStarted` and 704 `TrackStopped` asserts (79 and 76 of them datetime-valued); 7 `Played` and 6 `Skipped`; 28 retracts, written by three dedup merges on 2026-11-04 (14 starts and 14 stops); 364 short-hash lines and 5 with `""`.
- The logs show that starts and stops cannot be paired. There are stops with no start, starts with no stop, and bursts of stops for tracks that started days earlier.

**Identity rule (stainless_facts ADR-001, adopted by MDMA).**
- An assertion's identity is (entity, value). Timestamp and source never match.
- A retract cancels the live (entity, value).
- Repeatable occurrences must therefore be distinct values. For plays, the time the event was sent makes each one distinct.

**Readers of play facts.**
- library_service:
  - `refresh_event_timestamps` (components/library_service/src/service.rs:3099-3230) keeps the latest start and stop per key, over the `Replaces` lineage, from the ACID fact timestamp. It ignores retracts (3110-3113, 3153; note at 79-81).
  - `apply_assert_scalar` (518-523).
  - `is_provenance_tracked` excludes plays (488-496).
  - `FactAggregator` skips plays (851-883), and play retracts are a no-op (840-842).
  - dedup carry skips plays (service/dedup.rs:105).
  - library_service_stub (src/service.rs:120-121).
- Derived fields `last_started` and `last_stopped` (library_search/src/eval.rs:26-27, 125-131), used by `mdma search --started/--stopped` (mdma-cli/src/search.rs:63-67) and sorting (mdma-cli/src/tracks.rs:150-153).
- `mdma facts` displays values generically (value.rs:444-445, 506-507).
- ADR-014 §9 plans a play count (the number of starts) and moves play events into the fold, removing `refresh_event_timestamps`. It says that only asserts count and that play events on retired losers fold through their `Replaces`.
- There is no play count yet. TUI and console show `PlaybackEvent`s, not play facts.

**Old readers skip lines they cannot parse**, without a word (service.rs:921-924, 1572-1578). A new tag is invisible to them, but it is not corruption.

## Decision

Points marked **Decided** were decided by Joakim. Everything else is **Proposed**.

### 1. Every playback command names a deck

1. **Decided:** every playback command names a deck: play, stop, pause, resume, skip, seek, fast-forward (when it exists), now-playing and play-queue. Commands that lack a deck today gain one. The exceptions are queue commands other than QueueNext (§2), the audio-output commands, which act on the master output (§2.5), and GetSession, which reports every deck (§2.7).
2. **Decided:** the new field is optional on the wire (`Option<Deck>`, serde default). An unspecified deck means `Deck::A`.
3. **Decided:** it will be tightened to required later. That is a future **breaking** protocol step (Phasing, phase 5).
4. Commands that already carry a required `deck` (Play, Stop, Pause, Resume, Seek, GetLength, LoadTrack, SetVolume, Unload) keep it required. Old clients always send it, so there is nothing to loosen.
5. **Bare-string compatibility.** Commands that are unit variants today and gain a deck (PlayQueue, NowPlaying, Skip) become struct variants: `{"skip":{"deck":"A"}}`. QueueNext is already a struct variant, so it only gains the optional field.
   - `Command` must keep accepting the old bare string `"skip"` as if it had no deck.
   - Use the pattern `MusicValue` already uses (value.rs:134-144, 370-376): a private derived mirror, plus a `Deserialize` that rewrites the legacy form first.
   - Serialization always emits the new form.
6. **The default is resolved in one place.** The server turns an absent deck into `Deck::A` once, at the command boundary (one function, e.g. `Command::deck_or_legacy_default()`). No handler sees `Option<Deck>`.
   - Log at debug when the default was used, so the tightening can be timed.
   - Alternative: the field is `deck: Deck` with `#[serde(default = ...)]`. That needs no `Option` in Rust, but absent callers can no longer be counted.
7. **New clients always send the deck explicitly.** They never rely on the default, so tightening it breaks no current client.
8. **No silent fallback.** Playback honours the deck it is given. Until a second deck exists, any deck other than A gets `Response::Err { "deck B is not available on this node" }`. It is never quietly played on A.
9. stream_source_protocol does not change. Playback maps a deck to its source instance.

### 2. Which commands take a deck: the queue is shared

1. **Decided:** the queue is **shared**. It is not per deck.
2. **Decided:** QueueNext names a deck: "play this next on deck X". The deck is optional on the wire and unspecified means `Deck::A`, as in §1.2.
   - The entry records the deck it was queued for. `QueueEntry` and the persisted `PersistEntry` (projects/mdma-playback/src/server.rs:34-37, 51-55) gain `deck: Option<Deck>`, with a serde default so queue files on disk stay readable.
   - Entries added by QueueAppend or QueueReplace name no deck.
   - Which entry a deck takes from the shared queue: see Open questions 1.
3. **Decided:** QueueAppend, QueueClear, QueueRemove and QueueReplace take **no** deck, because the queue is shared.
4. **Proposed:** these take a deck:
   - PlayQueue (which deck the next entry is loaded on);
   - Skip;
   - NowPlaying.
5. **Decided:** the audio output is the **master** output only. ListAudioOutputs, SetAudioOutput and GetAudioOutput take **no** deck. A mixer blends the decks into the master output, and the decks do not have outputs of their own. For where this goes next, see §4.4.
6. **Decided:** QueueList takes **no** deck. It lists the whole shared queue, with the deck each entry names, if any.
7. **Decided:** GetSession takes **no** deck. The session is per node (`SessionId` is node-wide today, playback_primitives/src/lib.rs:25-39), and GetSession reports every deck.

### 3. Play events name a deck and are unique values

1. **Decided:** play starts and stops name a deck.
2. New `MusicValue` variants. The tags `Played` and `Skipped` are taken by `rewrite_legacy` (value.rs:395-396), so these names are used:
   - `PlayStarted { deck: Deck, via: StartReason, at: DateTime<Utc> }`
   - `PlayStopped { deck: Deck, reason: StopReason, at: DateTime<Utc> }`
3. **Decided: events are isolated.** A stop does not reference or match a start. The log records that a start or a stop was sent, not that anything was playing. Readers do not pair events: there are no open plays, no durations and no orphan stops.
4. **Decided: `at` is the identity.** Under (entity, value) identity, (entity, tag, `at`) must be unique, and only per node:
   - Playback mints `at` when it sends the event, at full (nanosecond) precision. Two events of the same tag on one entity at the same instant do not happen.
   - Only playback writes play events, and it runs on the node. Offline replicas never write them (ADR-016), so per-node uniqueness is enough.
   - There is no id type and no new dependency.
   - Identity is compared on the parsed value, so `at` matches whatever its textual form.
5. **The writer mints `at`** in `write_fact` (projects/mdma-playback/src/server.rs:39-48). The state machine does not change. For now, playback always writes `Deck::A`.
6. **Time and ordering.**
   - `at` is when playback sent the event. `last_started` and `last_stopped` use `at`.
   - The fold still orders by (ACID timestamp, log order) (ADR-014 §2).
   - Migrated lines have `at` = their ACID timestamp (§6.5), so history shows the same times as today.
7. **Retracting an event** means retracting that exact `PlayStarted` or `PlayStopped` value. Only a writer that knows the full shape may retract it. It must retract the value exactly as read from the log, never a lossy copy it has serialised again (the legacy retire batches did that; see §6.6).
8. **Retracts are real cancellations** for `PlayStarted`/`PlayStopped`. The legacy "asserts only" rule (service.rs:3110-3113) is removed.
9. **Decided: plays survive a merge, as ADR-014 §9 says.** From this change on, `retract_all_entity_facts_locked`, the hard retract in `merge_into`, skips play events. A merged-away copy's history stays and folds through its `Replaces`. This holds until ADR-014 step 4 removes retire from merge and replace.
10. **Room for a queue identity.** No queue field now (§4). A later `queue: Option<QueueId>` on `PlayStarted`, with a serde default, is additive: old readers ignore unknown fields (`MusicValueDef` has no `deny_unknown_fields`).
11. **Decided: reasons are not guessed.** `StartReason` and `StopReason` gain a variant `NotRecorded`, meaning no reason was recorded. Only the migration writes it (§6.5); nothing new ever does. Both enums are read through the same tolerant pattern as the deck (§5.3): an unrecognised name is kept as unknown, never turned into `OnRequest`. Writers emit only known reasons.
12. **No legacy play shape survives.** The migration (§6) rewrites every legacy play line, so readers handle only the new shapes.
13. `PlaybackEvent::TrackStarted`, `TrackStopped`, `TrackPaused`, `TrackResumed` and `TrackEnded` gain `deck` (optional, with a serde default). Old subscribers ignore the extra field. Events carry no id.

### 4. Future work (record, don't build)

1. **Decided (future):** several queue and playback modes, and several queues active at once.
   - Example: party mode, where each user has a queue and an algorithm picks the next track by fair airtime.
2. The shapes leave room for this:
   - an optional queue identity on queue commands, PlayQueue and Skip, following §1's optional-then-required path;
   - an optional `queue` on `PlayStarted` (§3.10);
   - a later `StartReason` variant (for example a pick by the algorithm).
3. None of this is built now. No queue field now.
4. **Decided (future):** a mixer, a master output and a cue output.
   - The decks are blended by a mixer into the master output. The audio-output commands (§2.5) choose the master's device.
   - Later a **cue** (headphone) output may be added. It is a separate output, not an output per deck. Which decks are sent to it is a mixer setting.
   - When it comes, the audio-output commands gain an optional output selector (master or cue; unspecified means master). This follows the same optional-then-required path as the deck (§1).
   - The mixer, the cue output and the selector are not built now.

### 5. The `Deck` enum only grows

1. **Decided:** `Deck` is `{A, B}` today (playback_primitives/src/lib.rs:144-149). New variants are only ever added, and old readers see them as unknown.
2. **Commands:** an old playback that receives an unknown deck answers "invalid command" (server.rs:163-170). That is correct, because it cannot play that deck.
3. **Facts:** an unknown deck must not make the whole line unparseable, since old readers drop such lines silently (service.rs:1572-1578).
   - music_facts reads `deck` through a tolerant type that keeps an unrecognised name as unknown.
   - The pattern is like `StartReason`'s hand-written `Deserialize` (value.rs:26-36), but it keeps the value instead of guessing.
   - Writers emit only known decks.
   - This tolerance must ship with the first fact that carries a deck (phase 3), before any third deck exists.
   - Deck and the two reason enums should share one tolerant-enum helper, not three hand-written `Deserialize` impls.

### 6. Rewrite legacy plays in place (one-off migration)

1. **Decided:** legacy plays are rewritten in place, while MDMA has no other users. One play shape in the code beats keeping two forever.
2. **Scope.** Every line whose value tag is `TrackStarted`, `TrackStopped`, `Played` or `Skipped`, Assert and Retract alike. No other line changes. On stage2 that is 1,466 of 22,959 lines.
3. **One line in, one line out.** Each line keeps its entity, ACID timestamp, source, operation and position; only the value changes. Line count and order are unchanged, so every `line:<n>` position still points at the same fact. Nothing persists such a position today:
   - the library keeps `cursor` and `event_cursor` in memory, starting from None (service.rs:72-82), and ignores the legacy `facts.cursor` file (service.rs:4714-4717);
   - ACID reads the file on every request (fact_store_file/src/storage.rs:56-80);
   - there are no replicas yet (ADR-016 §2).

   Keeping positions keeps future replica cursors valid and makes the backup easy to diff.
4. **Entities are kept exactly as written**, including #86 short hashes and the empty entity `""`. Readers keep resolving short hashes as today. The migration repairs nothing.
5. **Asserts.** Each line is mapped on its own; there is no pairing (§3.3).
   - `TrackStarted(r)` becomes `PlayStarted { deck: A, via: r, at }`.
   - `TrackStopped(r)` becomes `PlayStopped { deck: A, reason: r, at }`.
   - `Played` becomes `PlayStopped { reason: OnCompletion, … }` and `Skipped` becomes `PlayStopped { reason: OnSkip, … }`, as `rewrite_legacy` reads them today.
   - **Decided:** datetime-valued lines (playback 0.2.0; on stage2 79 starts and 76 stops) get `via: NotRecorded` or `reason: NotRecorded` (§3.11). Today they are read as `OnRequest`, which is a guess. They are not guessed.
   - `deck: A` is true for history: deck A is the only deck that has ever played.
   - `at` is the line's ACID timestamp, not the datetime inside a legacy value. That is the time readers use today, so `last_started` and `last_stopped` do not move.
6. **Retracts.** A legacy retract carries no time, and legacy values are not unique. To find the value a retract cancels, the tool matches it to an assertion:
   - the earliest assertion on that entity, earlier in the log, whose legacy value reads equal (tag and reason after `rewrite_legacy` and the datetime-to-`OnRequest` reading) and which no earlier retract has claimed;
   - matching uses the parsed value, because retire batches re-serialised datetime values as `"OnRequest"`, and the exact entity string, because a short hash is a different entity;
   - a retract never takes its own line timestamp as the `at` of the value it cancels, because one batch stamps all its lines alike (stage2: three `TrackStarted` retracts on one entity at lines 22857, 22859 and 22861).

   A retract is rewritten to the exact value it cancels, except for merge retracts (next point).
7. **Decided: merge retracts cancel nothing.** The play retracts written by merges were never meant to remove history (ADR-014 §9, §3.9 above). On stage2 these are all 28 retracts, written by the three dedup merges on 2026-11-04:

   | Lines | Merged-away copy | Head (`Replaces`) | Retracts |
   |---|---|---|---|
   | 22857-22862 | `sha256:4609c124…` | `d57f2f28` | 3 starts, 3 stops |
   | 22886-22887 | `sha256:f3baa408…` | `e92172c4` | 1 start, 1 stop |
   | 22906-22927 | `sha256:ec9ce8d0…` | `4dfe138e` | 10 starts, 10 stops |

   - Each is rewritten with `at` = its own line timestamp, so it matches no assertion and cancels nothing. Matching (§6.6) still runs, to check that each of the 28 matched exactly one assertion.
   - The tool is given the merge batches per node, as line ranges. On stage2 these are the three above. Any other play retract is rewritten to the exact value it cancels (stage2 has none).
   - The merged-away copies' plays (14 starts, 14 stops) stay live and keep their history.
   - Uniqueness of `at` is checked on assertions only. Retract lines of one batch can share a value without harm.
8. **Effect.** No play leaves any history. `last_started` and `last_stopped` do not move, and play counts equal what the old reader sees, because it ignores retracts.
9. **Procedure**, per node (stage2 first, then prod):
   1. Stop ACID, library and playback. Writers that go through them fail meanwhile; nothing is lost.
   2. Back up `facts.jsonl` next to the original, and keep the backup.
   3. Run the migration tool, which writes a new file and verifies it:
      - same line count, and non-play lines byte-identical;
      - every new line parses;
      - (entity, tag, `at`) is unique among assertions;
      - every retract matched as reported (stage2: 28 of 28);
      - replaying old (today's reader) and new gives identical `last_started`/`last_stopped` per track;
      - live starts and stops per track equal today's, because no migrated retract cancels anything.

      It prints the counts and refuses on any other difference.
   4. Swap the files atomically (rename within the directory), then start the services with the new release (Phasing 3).
   - The tool is idempotent (already migrated lines are left alone) and refuses to run while ACID answers on its socket.
   - **Rollback** means restoring the backup together with the previous release. An old release skips every migrated play line silently (service.rs:921-924).
10. This is the only time MDMA rewrites its log. ADR-001's append-only rule otherwise stands.

## Why

- **One addressing rule for every command.** Today the deck is required on some commands, ignored on others and missing from the rest. A request for B that plays A is a charade.
- **An optional deck plus bare-string acceptance** lets old CLIs, TUIs and consoles keep working against a new node, and makes the later tightening one visible step.
- **Unique event values** follow from the adopted identity rule. An event can be retracted exactly, and history no longer depends on `FactSource`, which the source-blind fold ignores.
- **Isolated events** (decided) record only what happened: a start or a stop was sent. Legacy logs show why pairing is unsound: stops with no start, repeated starts without a stop, and bursts of stops for tracks that started days earlier.
- **`at` as identity** (decided) needs no id type and no dependency. Uniqueness is only needed per node, because only playback writes play events.
- **Plays survive a merge** (decided) because ADR-014 §9 says so, and a merge that erased a copy's history would lose what the user listened to. The 28 existing retracts are neutralised for the same reason.
- **`NotRecorded`** (decided) is honest. A guessed `OnRequest` claims something nobody recorded.
- **Rewriting legacy plays** (decided) means one shape in the code. It is cheap now, with no users besides Joakim, and the one-to-one rewrite keeps every log position.
- **New tags, not new fields on `TrackStarted`/`TrackStopped`**: the migration can tell migrated lines from unmigrated ones, and `Played`/`Skipped` stay retired.

## Alternatives considered

- **Add `deck` and `at` to `TrackStarted`/`TrackStopped` with serde defaults.** Rejected: legacy values would read with a defaulted `at` and remain equal values.
- **A minted id (UUIDv7 or ULID) in each event, with stops referencing starts.** Rejected (decided): pairing is not wanted, and `at` is already unique per node. It would add a dependency for nothing.
- **The ACID line timestamp as identity.** Rejected: under the identity rule the line timestamp is not part of the value. `at` lives in the value, and for migrated lines it equals the line timestamp.
- **Let the merge retracts cancel** (rewrite them to the exact value, so the copies' plays leave their heads' history). Rejected (decided): it contradicts ADR-014 §9.
- **Keep reading datetime-valued legacy lines as `OnRequest`.** Rejected (decided): it is a guess. They get `NotRecorded`.
- **The tag name `Played`.** Rejected: `rewrite_legacy` already rewrites it (value.rs:395).
- **Keep `Deck` mandatory on new fields straight away.** Rejected by decision 1.2: it breaks prod's deployed clients.
- **New `...OnDeck` command variants beside the old ones.** Rejected: the protocol surface doubles, and the old variants live forever.
- **A deck on every queue mutation.** Rejected (decided): the queue is shared. Only QueueNext names a deck, because "play this next" is about a deck (§2.2).
- **One audio output per deck** (for headphone cueing). Rejected (decided): the decks go through a mixer into one master output. Cueing comes later as a separate cue output (§4.4), not as an output per deck.
- **Fold legacy plays as they are.** Rejected (decided): two shapes in every reader forever.
- **Append-only backfill.** Rejected: retracting equal legacy values is ambiguous under (entity, value) identity, and readers would still have to tell backfill retracts from legacy ones.

## Consequences

- **Protocol:**
  - `Command` gains an optional `deck` on QueueNext, PlayQueue, Skip and NowPlaying. The queue-mutation, QueueList, GetSession and audio-output commands do not change.
  - It accepts legacy bare strings.
  - `PlaybackEvent` gains an optional `deck`.
- **Queue file:** entries gain an optional `deck`. Old queue files load with no deck on any entry. An older playback reading a newer queue file ignores the field (serde ignores unknown fields), so it treats every entry as having no deck.
- **Fact schema:** adds `PlayStarted { deck, via, at }`, `PlayStopped { deck, reason, at }`, `NotRecorded` reasons, and tolerant read types for deck and reasons. `TrackStarted`, `TrackStopped` and the `Played`/`Skipped` rewrite are removed after the migration. No new dependency.
- **Readers of the new shapes**, which ship in the same node release as the writer and the migration:
  - `refresh_event_timestamps` (service.rs:3099-3230), and later the fold that replaces it (ADR-014 §9 step 3): latest `at` per kind, with retracts honoured;
  - `apply_assert_scalar` (518-523);
  - `is_provenance_tracked` (488-496), the `FactAggregator` skip (851-883) and the play-retract no-op (840-842);
  - the dedup carry rule (dedup.rs:105), while it still exists;
  - `retract_all_entity_facts_locked` (service.rs:2359-2395), which skips play events (§3.9);
  - library_service_stub, the `mdma facts` display (value.rs:403-541) and the BDD simulator;
  - a future play count: live `PlayStarted` events.
  - Consumers of `last_started`/`last_stopped` (library_search, `mdma search`, sorting) need no change.
- **Deploy order:**
  1. Node first: playback, gateway and library together. The gateway serializes `Command` again (main.rs:126), so gateway and playback must match. The library must parse the new tags before playback writes them; otherwise new plays are skipped until the library is upgraded, at which point its restart rescans from the beginning (`event_cursor` starts at None, service.rs:76-77). ACID does not change.
  2. Then the clients: CLI, TUI and console.
- **Compatibility:**
  - Old clients against a new node keep working (optional deck, bare strings).
  - A new client against an old node gets "Invalid request" or "invalid command". It shows ADR-016 §5's upgrade message, with no fallback.
- **Log:** rewritten once, for play lines only (§6), with the same line count and positions. Append-only otherwise. One line per start or stop, as today.
- **History:** no play leaves history. A merge keeps the merged-away copy's plays (§3.9), and the 28 existing merge retracts cancel nothing (§6.7). Old plays with no recorded reason read as `NotRecorded`, not `OnRequest`.
- **Migration downtime:** ACID, library and playback stop on each node while the tool runs. A backup of `facts.jsonl` is kept.
- **Tightening (phase 5) is breaking.** Clients that omit the deck, and bare-string commands, are rejected. Do it only after every deployed client sends the deck.

## Open questions

1. **Which entry does a deck take from the shared queue?** This follows from §2.2.
   - **Proposed:** a deck takes the first entry that names this deck or names none. An entry that names another deck is passed over and stays in place.
   - Until deck B plays this makes no difference: every entry is either for A or for no deck, so A takes the head as it does today.
2. **CLI and TUI surface for choosing a deck** (for example `--deck`, or the focused deck in the TUI). This is deferred until deck B plays. Until then, clients send `Deck::A` explicitly.

## Phasing

1. **Protocol.**
   - Add the optional `deck` to QueueNext and the deck-less commands in §2.4, accept the legacy bare strings, and add the default-resolution function.
   - Clients send `Deck::A` explicitly (media_client, mdma_client, CLI, TUI, console, BDD).
   - Deploy the node, then the clients.
2. **Playback honours the deck.**
   - Replace `deck: _` (server.rs:202, 219, 226, 233, 240) and the implicit A in QueueNext, PlayQueue, Skip and NowPlaying (252-363) with the resolved deck. The audio-output handlers (376-408) do not change.
   - A deck other than A gets an explicit error (§1.8).
   - Queue entries record the deck QueueNext names (persisted with a serde default). Taking an entry follows Open questions 1's rule once it is decided; with only deck A this behaves as today.
   - `PlaybackEvent` gains `deck`.
3. **New play facts.**
   - Add `PlayStarted`, `PlayStopped`, the `NotRecorded` reasons and the tolerant read types for deck and reasons.
   - Playback writes `at` and `Deck::A`.
   - In the same node release: the readers of the new shapes (see Consequences), `deck` on events, `retract_all_entity_facts_locked` skipping play events (§3.9) and the migration tool (§6). Legacy play code is removed.
   - Deploy per node: stop the services, migrate (§6.9), start the new release. Stage2 first, then prod.
4. **Readers move into the ADR-014 fold.** Play events fold as a union keyed by (origin entity, value), with the origin entity in the key so that two copies' events never merge. Retracts are honoured, and the play count is added.
5. **Later: tighten.** The deck becomes required, and bare-string and absent-deck forms are rejected. This is breaking and needs its own release note.

## Related

- ADR-001, ADR-004, ADR-009, ADR-014 (§9 amended), ADR-016 (§5 upgrade message; §2 replica cursors)
- stainless_facts ADR-001: assertion identity is (entity, value) (johlrogge/stainless_facts#15)
- The source-blind fold change (separate)
