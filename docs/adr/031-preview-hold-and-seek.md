# ADR-031: Preview hold and seek in split playback

## Status

Accepted, 2026-11-17 (Joakim decided)

Builds on ADR-017 (every playback command names a deck; the gateway parses and re-serialises `Command`), ADR-026 (playback holds only full hashes), ADR-016 (marks and the console), ADR-029 (the console's narrowing). Related: ADR-030 (the client replica; no play facts are involved here).

## Context

Joakim wants a click on a playlist-workbench row to let him hear that track at once, without losing what was playing, and to move around inside a track (timeline, beat steps). Reading the code showed:

- Seek is plumbed only at the bottom. `PlaybackEngine::seek` takes an interleaved sample index; `StreamCommand` has no seek; mdma-playback refused `Command::Seek` as "not supported in split-architecture mode"; nothing sent it.
- The audible position is known (`StreamTrackInfo.position_ms`) and published as `PositionUpdate` about once a second, but only while Playing. No command returns it, so UIs guessed paused-versus-playing.
- `PlaybackState` is `Idle | Playing | Paused`. The source holds one track plus a preload. Loading another track discards the paused one, so going back means reload, seek, play.
- `play_queue`, `skip` and `stop` write `TrackStarted`/`TrackStopped`, and `library_projection` folds them into plays (#99). A preview sent through `play_queue` would count as a play.
- The library already carries what a timeline needs: `TrackInfo.memory_cues`, `beat_grid` and `duration`.
- The gateway re-serialises `Command`, so any new or changed playback command needs gateway and playback deployed together.

## Decision

### A1. The preview hold (phase S2)

- **One held slot.** Preview X captures where the audible track is (hash, position, playing or paused), loads X and plays it. Previewing again replaces the preview and leaves the held track alone: a preview of a preview is not a level you return to. `BackTo` holds a track or nothing and cannot hold a preview, so nested stacks cannot be built.
- **Back** reloads the held track, seeks to the held position, and plays it only if it was playing. It is the undo: no confirm, no separate undo. Preview from Idle holds nothing; Back returns to Idle.
- **Automatic return.** When the preview ends by itself, playback pops: it restores the held transport exactly.
- **Previews are not plays.** No `TrackStarted`/`TrackStopped` for a preview, and no session start. The held track's play continues: it is paused, not stopped, and the pop writes nothing. Revisit with ADR-017's `PlayStarted.via` once that lands.
- **During a preview:** Pause, Resume and Seek act on the preview. Play resumes a paused preview (as it resumes a paused track). Stop ends everything: the hold is dropped, `TrackStopped(OnRequest)` is written for the held track and `PreviewEnded { resumed: None }` is published. **Skip and PlayQueue are refused** ("Previewing: go back first"), whatever the queue holds (an empty queue is refused the same way, not as "Queue is empty"), an explicit refusal rather than a guess; in the UI Back takes Next's place. PlayQueue on a paused track that is not a preview still resumes it. Queue edits stay allowed.
- **Preview needs a position.** A preview over a playing or paused track is refused ("Cannot preview: the source does not say where the playing track is") unless the source has just said which track it plays and where, and that track is the one playback has as audible. That covers an unreadable position, a source that cannot be asked, and a gapless switch landing between asking and previewing. Nothing is guessed: going back to a wrong position would lose the listener's place.
- **A late `Finished` is not this track's.** The poll loop counts a `Finished` only when the track the source reports equals the track playback has as audible, so a late `Finished` for a preview that just ended cannot end the restored track.
- **Up next is untouched.** Preview is not a queue action; it does not go through `QueueAction`, which means "what plays later".
- **Where it lives.** In playback, so it survives a page reload and is shared by CLI, TUI and console. It is lost on a playback restart, like everything else playback holds.

### A2. Types (phase S2)

- `PositionMs(u64)` in `playback_primitives` (done in S1).
- `Transport { Playing, Paused }`, `BackTo { Nothing, Track { hash, at, transport } }` and `PlaybackState::Previewing { hash, transport, back_to }` in `playback_state.rs`. Transitions: `preview(hash, observed: Option<Observed { hash, at }>)` from every state, refused with `Refusal::PositionUnknown` when a playing or paused track is not observed; `end_preview` gives `LoadAt { hash, at, then }` for a held track, or `StopEngine` for nothing; Pause and Resume toggle `transport` inside `Previewing`; `skip` and `play_queue(next: Option<..>)` return `Err(Refusal)` (`Previewing`, `QueueEmpty`) instead of effects: a refusal is a result, not an effect, and the queue front is popped only for an `Ok` with effects. `LoadAt` is the playback effect that composes the source's Load, Seek and then Play (only if `then` is Playing), the same seek as `Command::Seek`: one way to start a track at a position, no special Load variant. Track{Paused,Resumed} events name the audible track, which may be a preview.

### A3. Protocol

Every new command carries a required `deck: Deck` (ADR-017 section 1.1).

- `Seek { deck, position_ms: PositionMs }` replaces `Seek { deck, position: usize }` (S1). Nothing sent the old form; it no longer parses. `PositionMs` is serde-transparent, so the JSON is a plain number. A position past the end of the track is kept to the track's duration by the engine (one rule for every client). `Seek` answers with `ResponseData::Transport` (below), so the caller learns where the track actually went, not just what it asked for.
- `GetTransport { deck }` answers `ResponseData::Transport(TransportStatus)` (S1). The shape makes illegal states unrepresentable: `Idle` has no track and no position; `Loaded { transport: Playing|Paused, now, position_ms, duration_ms, preview }` always has a track (`position_ms` and `duration_ms` are `None` only when the source cannot say). On the wire it is internally tagged: `{"state":"idle"}` or `{"state":"loaded","transport":"playing","now":...}`. `preview` is `None` unless the audible track is a preview; then it is `Some(PreviewStatus { back_to: Option<Held { hash, at, transport }> })`. The held track, its position and its transport are one value, so they cannot disagree; `back_to` is `None` when the preview started over nothing (going back stops). This is the "transport status command" the console reserved; it answers paused-versus-playing directly, so the console's guessing cache can go.
- `Preview { deck, hash }` (the hash is in `hashes_held()`, so `check_canonical` covers it) and `EndPreview { deck }`, a no-op when not previewing (S2). `EndPreview` answers `ResponseData::NowPlaying` with the track audible afterwards, so a client can say where it went back to. `Play { deck }` is documented as "resume a paused track or preview"; `PlayQueue` as "resumes when paused; refused while previewing".
- **A failed return leaves the state untrue.** `LoadAt` can fail (the held track left the library, the source is gone). The state machine has already moved to the held track, so until the next command the state says Playing or Paused while the source plays nothing or the preview. The same holds today for a failed `LoadAndPlay`. A user's Back answers the error; the automatic return after a natural end only logs a warning, because there is no event to carry an error. Proposal, kept small and not done: one `PlaybackEvent::Error { message }` published when an effect fails with no caller to answer, which UIs show as a toast; the state would then be corrected by the next `GetTransport`.
- Events `PreviewStarted` and `PreviewEnded` (S2). `PositionUpdate` is also emitted while a preview plays.
- `NowPlaying` keeps its meaning: the audible track, which is the preview while one runs.
- `StreamCommand::Seek { position_ms: PositionMs }` (S1): the source protocol gains `Seek`, still deckless, so the intent of ADR-017 §1 item 9 (playback maps a deck to its source instance; the source protocol names no deck) holds. An old source answers "invalid command"; playback passes that on as the error and changes nothing.

### A4. Seek plumbing (S1)

1. **Engine.** `PlaybackEngine::seek_ms(PositionMs)` converts milliseconds to the interleaved sample index inside the engine, where channel count and source rate are known (`SourceFormat`, which cannot hold a zero channel count or rate). The engine's existing sample-index seek stays. The conversion truncates to a whole frame. A position past the end is kept to the track's duration here, once, and `seek_ms` returns the position it went to (a track of unknown duration is not kept to anything).
2. **mdma-audio** handles `Seek` by calling `seek_ms` and then `play()` again if it was playing: the engine leaves the track stopped after a seek, so "the play state stays as it was" is done by the source. Idle answers "nothing loaded". The engine restarts the track under a new id, so the source moves the track's metadata to the new id (`carry_meta`) before it prunes; without that `Loaded` reports nothing after a seek, which would silence position updates and `GetTransport`'s position and stop auto-advance and gapless detection. A test loads a real file through the real engine to show `Loaded` still describes the track after a seek (it needs the PipeWire output the engine opens on load).
3. **Playback** seeks the audible track and keeps its transport: playing stays playing, paused stays paused, Idle answers "nothing loaded". It publishes one `PositionUpdate` right away so UIs rebase their clocks, and answers with the transport as the source reports it after the seek.
4. **The seek-epoch trap.** The engine drops the preload on seek (`start_fresh` drops everything), but the poll loop only resent a preload when the audible track changed (`last_current`). After any seek gapless playback would silently stop working until the next track change. Fix: playback keeps a `SeekEpoch` counter, bumped after the source acknowledged a seek, and the poll loop's preload bookkeeping (`PreloadBook`: last sent, the hashes the source may switch to, and the epoch it saw) forgets what it *sent* whenever the track or the epoch changes. On a seek the set of hashes the source may switch to is kept: clearing it would let a switch the source had already committed to look like an unknown track and be played twice. The bump comes after the acknowledgement so that a preload the loop sent in between is not taken for one that survived. A preload that is sent again while the source still has it is a no-op there. One window remains: `Server::seek` takes the hash it reports from the state machine while the source seeks whatever is audible, so a gapless switch landing between the two makes the immediate `PositionUpdate` name the old track; the next poll corrects it. The poll tick is one function (`poll_once`) that the loop and the regression test both call; the test shows the preload is resent once per seek.
5. **Beat math in Rust, shared.** `music_primitives::BeatGrid` (not methods on `BeatGridInfo`, which is a wire type in `library_ipc_protocol`; the grid has no protocol dependencies and `BeatGridInfo::grid()` is the one way from the wire form, and answers `None` for zero beats per bar): `beat_ms(n)` (saturating), `nearest_beat(ms)`, `step(from_ms, beats, duration_ms)` (snap to the beat, step, clamp to the track) and `bars(duration_ms)`. It is a constant-tempo grid: one anchor and one BPM per track, as stored today. Beats before the start of the track snap to 0.
6. **Orchestration in `mdma_client`**, shared by the CLI, TUI, console and a future 101 bridge: `PlaybackBackend::seek(deck, position)`, `skip_beats(playback, library, deck, beats)` (reads `GetTransport` for `now` and position, reads `GetTrack(now).beat_grid`, steps, sends `Seek`) and one entry point for placing a time, `seek_to(playback, library, deck, ms, Placement::{Snap, Free})`. `Snap` goes to the nearest beat when the track has a grid and to the time asked when it has none; `Free` goes where asked and never connects to the library. A typed time, such as the CLI's, is free placement: the equivalent of Alt on the timeline. Timeline and seek clicks in the console are `Snap` unless Alt is held. `skip_beats` stays separate because it is a different operation. With no grid a beat step fails with `SeekError::NoBeatGrid`: there is no fallback to a guessed BPM-only grid. Playback stays fact-free and never learns about grids. Both return the transport `Seek` answered, so a client says where the track went.

### A5. Where "only the workbench" lives

Preview is a general, always-reversible playback capability: CLI, TUI and console can all use it, and the server does not need to know the page, because nothing about preview loses what was playing. "Offered only in the workbench" is console view composition (which buttons a page renders), not a domain rule. No route is restricted to the workbench, because that restriction could not be enforced honestly. Rule 9 of the console rules therefore reads:

> A click never interrupts playback irreversibly. The one interruption is preview, which always offers Back to what was playing; the console offers it only in the playlist workbench.

### Decisions (Joakim, 2026-11-17)

1. One held slot: previewing again replaces the preview.
2. When a preview ends by itself, go back automatically and resume as before.
3. Previews are not plays.
4. Skip is refused while previewing.
5. A timeline click snaps to the nearest beat when the track has a grid; Alt places freely. The snap rule lives in Rust (`BeatGrid`); the browser only passes a flag.
6. Beat step n cycles through 1, 4, 8, 16 and 32 and defaults to 8. The setting is UI-side, per device.
7. Seek is also exposed in the main player, not only in the workbench.
8. Later: clicking a workbench row previews it, clicking it again toggles pause/play, and Back ends the preview.

## Phasing

- **S1: seek** (implemented): `PositionMs`, `seek_ms`, `StreamCommand::Seek`, `Command::Seek { deck, position_ms }`, `GetTransport`, `BeatGrid`, `mdma_client` `seek`/`skip_beats`/`seek_to`, gateway passthrough test, `mdma playback seek <TIME|+Nb|-Nb>` (under `playback`, next to stop, pause, resume and skip), the seek epoch.
- **S2: preview hold in playback** (implemented): `Transport`, `BackTo` and `PlaybackState::Previewing`; `LoadAt` (load, seek, play if it was playing) as the one way to start a track at a position; `Command::Preview` and `EndPreview`; `TransportStatus::Loaded.preview` filled; `PreviewStarted` and `PreviewEnded` events; no preload while previewing and a preview that ends by itself goes back (the preview's start and end bump the seek epoch); gateway passthrough test; `mdma_client`/`media_client` `preview` and `end_preview`; `mdma playback preview HASH|--back`. Pause and Resume while previewing emit `TrackPaused`/`TrackResumed` for the audible (preview) track, and the TUI ignores Track* and `PositionUpdate` events that name a track other than the one it shows and re-reads `GetTransport` on `PreviewStarted`/`PreviewEnded`.
- **S3: console preview** (implemented): `POST /api/preview {hash}` (400 without a hash, 422 for an unknown one, 409 with playback's words when it refuses, 502/503 per the usual policy), `POST /api/preview/back`, `POST /api/seek {to_ms, placement: "snap"|"free"}` or `{by_beats}` (through `mdma_client::seek_to` and `skip_beats`; 409 when idle, 422 without a beat grid), all answering like `GET /api/player`. `ApiPlayer` is built from `GetTransport` and gains `position_ms`, `duration_ms` and `preview: null | {back_to, back_at_ms, back_transport}`; `TransportCache`, the console's guess at paused versus playing, is gone. The seek bar is on (`SEEK_ENABLED`): the main player's scrubber snaps to the beat on release and Alt places freely, and the page only passes `placement`. In the workbench a click on a row previews it, a click on the previewing row pauses or resumes it (so does Space), and Back (the Back button where Next sits, `B`, or the toast's Back action) ends the preview; `P` previews the focused row and Shift+Up/Down the row above or below. The bar and the full player show a "Previewing" kicker, the preview's title and "Back to <held title> · m:ss", and Back takes Next's place. `PreviewStarted` and `PreviewEnded` refresh the player.
- **S4: timeline** in the workbench: cues, bar lines, the beat step selector.

Deploy order: the new commands and the changed `Seek` need **playback, mdma-audio and the gateway deployed together**. The gateway re-serialises `Command`, so an old gateway drops `GetTransport` and the new `Seek` as an unknown variant, and an old mdma-audio answers "invalid command" to `Seek`. Clients (CLI, console) come after.

## Consequences

- Seeking and the beat arithmetic have one home each; clients only choose a target.
- The poll loop's preload belief is a named, tested struct instead of three loose variables, and any future command that makes the source drop its preload bumps the same epoch (S2's preview start and end do; the bump is unconditional, so a no-op `EndPreview` costs one preload the source treats as already held).
- An old node answers "invalid command" (or an upgrade hint) to the new commands; nothing is guessed. In the console an old gateway drops `GetTransport`, so `/api/player` answers 502 and the page shows offline: consistent with the no-fallback rule.
- Every console transport route (pause, resume, preview, back, seek) uses one failure policy: a refusal by playback is a 409 with its words, a gateway that does not know the command or an answer of the wrong kind is a 502, an unreachable node a 503. `nng_transport::NngClientError::Refused` carries the refusal apart from `Service`.
- Beat steps are only as good as the single constant-tempo anchor; tracks with drifting tempo need a richer grid, which would be a new type.
