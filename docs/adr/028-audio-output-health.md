# ADR-028: Audio output health is reported, not inferred

## Status

Accepted, 2026-11-16 (architect design)

Related: ADR-017 (§2.5 audio-output commands are exempt from naming a deck; §4.4 plans an optional master/cue selector).

## Context

- With no DAC connected, Play leaves everything reporting Playing, but PipeWire never runs the stream callback. `consumed_total` stays 0, `PositionUpdate` reports about 0 forever, and the track never finishes. The web UI's elapsed see-saws between 0 and 1 s. Confirmed on stage2.
- `PipewireOutput::new` returned `Ok` before PipeWire had done anything. Every failure happened inside its thread and was swallowed.
- Nothing watched the stream state or whether callbacks arrived, so the engine could not tell "playing" from "a stream that never runs".
- A sink that exists but nobody hears (Dummy-Driver, HDMI with no screen) runs callbacks normally. Liveness cannot detect it.

## Decision

### 1. The callback heartbeat is the source of truth

- `OutputShared` carries `callbacks: Arc<AtomicU64>`. `render()` increments it as its first statement, before the paused check. Paused streams still get callbacks, and a starved ring is an underrun, not a dead output.
- It is a callback count, not `consumed_total`.
- It lives in `OutputShared`, which the engine owns, so it survives output re-creation.
- `OutputWatch` (pure, in `playback_engine`) judges it: a changed count is `Running`; an unchanged count for `STALL_AFTER` (1000 ms) is `Stalled`; before either the verdict is `Starting`. Time is passed in, so tests use synthetic `Instant`s and never sleep.
- A new watch is seeded with the current count, not 0. The counter survives re-creation, so a seed of 0 would read the first look at the new stream as `Running`.
- The stream `state_changed` listener only logs. It supplies the reason in the log, never the verdict, and it does not touch the callback's user data (which is `&mut` on the RT thread).
- "The output" means the master output, which today is deck A's source. With several sources the health is still the master output's.

### 2. Startup failures are real errors

- `PipewireOutput::new` waits up to 1 s for the thread to report that the stream is connected. A missing daemon or socket makes `new()` return `Err(StartError { error, consumer })`, which the engine surfaces as `PlaybackError::NoAudioOutput(#[source] AudioOutputError)` instead of a stringly `AudioDevice("PipeWire error")`.
- The mixer ring consumer is owned on the PipeWire thread by a drop guard (`ConsumerHome`) that sends it home through a capacity-1 channel on every exit path: a failed setup, a normal shutdown, or a startup timeout. `StartError` carries it back, and the engine puts it back into `mixer_consumer`, so the next load tries again. A start never returns `Ok` without an output.

### 3. Health is reported separately from transport state

- `OutputHealth { Absent, Starting, Running, Stalled }` (snake_case on the wire) lives in `playback_primitives`.
- It travels on `StreamResponse::Loaded { info, output }` (`output` is optional; `None` means an older source), on `PlaybackEvent::OutputChanged { health }` (topic `playback/output`, published by `mdma-playback` only on change), and on `Command::GetOutputStatus` -> `ResponseData::OutputStatus(Option<OutputHealth>)`, answered from `mdma-playback`'s cache.
- Playback stays `Playing`. There is no automatic pause: when the DAC returns, callbacks resume and play continues from the frozen position.
- Old decoders skip unknown event variants.
- `mdma-playback` polls the source at 1 s while Idle as well as while Playing or Paused, because the output keeps running when nothing plays. Without this the cache would keep the last session's `Running` after the DAC is pulled.
- The cache is never cleared by a source that stops reporting health; the last known value stays until replaced.

### 4. Output commands and events name no deck

- ADR-017 §2.5 exempts audio-output commands from naming a deck. `GetOutputStatus` and `OutputChanged` follow it. When §4.4's master/cue selector arrives, the cue-output path will add an optional selector to these, not a deck.

### 5. A stalled output is re-created on the next load

- `ensure_output` re-creates the PipeWire stream when the verdict is `Stalled`, so re-plugging a DAC that WirePlumber does not re-link recovers on the next track load. The decision is one pure function, `needs_new_output(current_rate, target, verdict)`.
- Re-creation discards the residue in the mixer ring and counts it as consumed (as a serviced flush does). A stalled stream never played it, and up to about 3 s of the previous track would otherwise be heard first. A hot swap by `set_output` keeps its buffer.
- It also applies when the next track is the preloaded one: the cut-to-preload fast path falls back to a fresh load while the output needs re-creating.
- A load against a stalled output can block for the ring flush (100 ms), the PipeWire flush (100 ms), the shutdown (up to 1 s) and the startup (up to 1 s). mdma-audio handles requests sequentially, so this is accepted.

### 6. Fail-fast Play is deferred

- Making Play fail when there is no output needs the playback state machine to return to Idle on a failed load. Today it moves to Playing before the effects run, and a failed `LoadAndPlay` leaves Playing in place.
- Fail-fast needs `load_failed`/`play_failed` -> Idle and re-queueing first. Until then health is only reported.

## Consequences

- UIs can show "no sound" within about 1.2 s of a stall, and the cause is in the log.
- Case (c), a sink nobody hears, stays undetected. A future change can report the linked sink.
- The consumer is returned on a failed start, so a missing or restarting PipeWire daemon recovers on a later load. Only a PipeWire thread stuck past the timeout still keeps the consumer. A ring per output (a `MixerCommand::ReplaceOutput { producer }`) would close that gap and is a follow-up.
