# ADR-001: MDMA-101 panel wire protocol and component boundaries

## Status

Accepted — 2026-04-22

Supersedes none. Relates to the MDMA-101 hardware spike plan
(`/home/johlrogge/.claude/plans/i-would-like-to-tidy-pumpkin.md`) and the
eventual hardware panel referenced in `ROADMAP.md`.

## Context

MDMA-101 is an iPod-inspired physical control surface: a multi-directional
rotary encoder, Kailh Choc switches in a diode matrix, and a display
(Sharp Memory LCD 2.7" target, TFT 2.8" fallback depending on supply).
Firmware runs on an ESP32-S3 in Rust (`esp-hal` + `embassy`, `no_std`).
The host service runs on `mdma-909` in std Rust and talks to the rest of
MDMA through the existing gateway and event bus.

The panel and the host exchange two streams of messages over USB CDC
serial: input events up, render commands down. We need to nail down
(a) what those messages look like on the wire, (b) where the trait
boundaries sit, and (c) what is shared between the firmware and the
host vs. kept on one side.

## Decision

### Wire encoding

Use **`postcard` + COBS framing** over USB CDC serial.

- Both ends are Rust. `serde`-derived types in a single shared crate
  compile to both targets. No schema language, no code generation step.
- Postcard produces compact (~3–5× smaller than JSON), allocation-free
  encodings. Trivial cost on the ESP32.
- COBS gives us self-synchronising framing for free: `0x00` is the frame
  delimiter, so chunk-safe reads fall out of the codec — no hand-rolled
  length-prefix or newline-scanning state machine.
- Postcard is the standard choice in the Ferrous Systems / Embassy
  ecosystem for this exact shape of problem (host ↔ embedded over
  serial).

### Component boundaries

Four new polylith components, one base, one project, one firmware crate
outside the polylith workspace.

- **`components/panel-protocol`** — `no_std`-capable. Holds the
  `InputEvent` and `RenderCommand` types and the postcard+COBS codec.
  Shared: the firmware depends on it as `no_std`, the host depends on
  it as `std`. Gated by a `std` Cargo feature (`default = ["std"]`,
  firmware builds `--no-default-features`).

- **`components/panel-ui`** — pure state machine. Takes `InputEvent`,
  emits `Vec<RenderCommand>`, holds no IO. `std` (has allocations, uses
  `heapless` for no_std-compatible collections where relevant). Does
  not depend on the gateway or media_protocol.

- **`components/panel-transport`** — host-side trait. Abstracts "send
  a `RenderCommand`, receive an `InputEvent`" over whatever concrete
  transport we end up with. `std`-only; the firmware does not implement
  this trait (it has its own direct IO tasks).

- **`components/panel-transport-fake`** — mpsc-channel fake
  implementing `PanelTransport`. Lets the whole stack run and be tested
  without any hardware. Not just a test fixture — it's the default
  transport for `projects/mdma-panel` until the real USB-CDC impl
  lands.

- **`bases/panel-host`** — wires `panel-ui` + `panel-transport` into
  a `run()` loop. Knows nothing about the gateway yet; that binding is
  a later layer.

- **`projects/mdma-panel`** — the host-side binary that will eventually
  run on mdma-909. Selects the transport impl via `[workspace.dependencies]`.

- **`firmware/mdma-panel-fw`** (not yet created) — separate Cargo
  workspace. References `panel-protocol` via a path dep. Not a polylith
  member — polylith reasons about the std root workspace.

### The gateway ↔ UI mapping belongs in `panel-host`

`panel-ui` stays pure: `InputEvent in, RenderCommand out`. The translation
from UI intent (PLAY, NEXT, queue-select) to concrete
`media_protocol::Command` calls against the gateway belongs in
`panel-host`, or in a future `panel-controller` component once that gains
enough weight to justify its own crate. `panel-ui` must not grow a
dependency on `gateway_client` or `media_protocol`.

## Why

- **One type definition, two targets.** Keeping `InputEvent` and
  `RenderCommand` in a shared crate avoids wire drift between firmware
  and host. This is worth a `no_std` feature gate.
- **Host-side trait, not firmware-side.** The firmware is a pure IO
  bridge; it does not benefit from a trait abstraction over its own
  peripherals at this stage. The host-side trait, on the other hand,
  is what makes the fake transport possible — and therefore what makes
  the whole UI testable without hardware, which is Joakim's explicit
  goal for this spike.
- **Fake as first-class.** `panel-transport-fake` isn't a test mock
  smuggled into production code. It's the transport the demo binary
  uses until the real one is written. Fake-first matches the
  hardware-spike reality: we can build, exercise, and iterate on the UI
  before the Sharp LCD breakout lands.
- **Postcard over alternatives.** Protobuf adds a schema language and
  codegen; CBOR and MessagePack don't target the embedded↔host use
  case as cleanly; JSON is wasteful and has no framing. Postcard + COBS
  is a well-trodden path for this exact shape of system.

## Alternatives considered

- **Newline-delimited JSON over CDC.** Rejected. Wasteful on wire,
  allocating parser, no natural framing on a byte-stream transport.
  Retained briefly in the initial plan, replaced before the first
  commit.
- **Protobuf.** Rejected. Adds a `.proto` schema and build step we do
  not need when both sides are Rust.
- **USB HID (not CDC).** Considered. HID reports are awkward for
  variable-sized render-command lists. CDC gives us a clean byte stream
  and trivial debugging (`cat /dev/ttyACM0`).
- **Wi-Fi / NNG from the panel directly to the gateway.** Deferred.
  The panel protocol is transport-agnostic; we can lift it onto NNG
  later without a re-design. For the spike, CDC sidesteps Wi-Fi
  reliability questions.
- **Full framebuffer push (host renders, sends bitmap).** Rejected.
  400×240 mono is 12 kB per frame; a command list for a menu screen is
  well under 1 kB, and lets the panel side exploit partial-update
  friendliness (Sharp) or dirty-rect blits (TFT).
- **Firmware as a polylith member.** Rejected. `no_std` + Xtensa/RISC-V
  target breaks `cargo check` on the std workspace. Firmware lives in
  its own Cargo workspace and path-depends on `panel-protocol` only.

## Consequences

- `panel-protocol` must stay small and dependency-lean. Adding a std-
  only crate to it breaks the firmware build. Enforce this by keeping
  `cargo check -p panel-protocol --no-default-features` green (and
  wire into CI when the firmware crate lands).
- `panel-ui` must not grow gateway / media_protocol dependencies. If
  pressure arrives — and it will when we start translating UI intent to
  playback commands — the mapping goes into `panel-host` or a new
  `panel-controller` component, not into `panel-ui`.
- The `String<64>` cap in `RenderCommand::Text` will bite on long
  track/artist names. UI layer is responsible for truncation with
  ellipsis. Revisit if the cap proves too tight once real metadata is
  wired in.
- `PanelTransport` uses `async fn` in trait (`#[allow(async_fn_in_trait)]`).
  Acceptable while the trait is sealed and single-impl-per-binary. If we
  ever need `dyn PanelTransport`, switch to `async_trait` or
  `impl Future + '_`.
- The base is currently called `panel-host`. Once the firmware side
  arrives, the name "host" becomes ambiguous (host = mdma-909 side, but
  also could mean "runs the UI"). Accept the ambiguity for now; rename
  deferred.
