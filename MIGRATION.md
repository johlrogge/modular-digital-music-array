# MIGRATION.md — mdma Polylith Migration (Current State)

Last reviewed: 2026-03-18

## Status: COMPLETE (2026-03-18)

The polylith migration is done. All projects exist under `projects/` with `main.rs`
entry points. All bases are correct lib crates. All three violations previously reported
by `cargo polylith check` are resolved. Phase 4 was evaluated and rejected — `mdma_client`
is a proper component, not a facade.

---

## Current Architecture (Verified)

```
Cargo.toml                          # Root dev workspace
components/  (25 crates, all lib.rs)
  acid_client, acid_protocol
  audio_metadata, audio_transcoder
  bandcamp_api
  date_expression
  event_protocol
  gateway_client, gateway_protocol
  inbox_utils
  library_ipc_client, library_ipc_protocol
  library_search, library_service     ← stays in components (BDD dep)
  mdma_client                         ← proper component, kept
  media_client, media_protocol
  music_facts, music_primitives
  nng_transport
  playback_engine, playback_primitives
  source_protocol, storage_primitives
  stream_source_protocol
bases/  (5 crates)
  service/        lib.rs ✓   — NNG IPC daemon skeleton
  http_server/    lib.rs ✓   — Axum HTTP server skeleton
  tui/            lib.rs ✓   — ratatui/crossterm terminal skeleton
  client/         lib.rs ✓   — NNG client connection skeleton (gateway + event)
projects/  (10 crates, all main.rs ✓)
  mdma-acid/      → service base ✓
  mdma-audio/     → service base ✓
  mdma-bandcamp/  → service base ✓
  mdma-beacon/    → http_server base ✓
  mdma-console/   → http_server base ✓
  mdma-library/   → service base ✓
  mdma-playback/  → service base ✓
  mdma-tui/       → tui base + client base ✓
  mdma-cli/       → client base ✓
  mdma-gateway/   → service base ✓
```

---

## Violations (`cargo polylith check`)

All violations resolved as of 2026-03-18.

Previously tracked:

| # | Violation | Kind | Location | Resolution |
|---|-----------|------|----------|------------|
| 1 | Base has main.rs | warning | `bases/mdma_library/` | Moved to `projects/mdma-library/`, base removed |
| 2 | Project has no base dep | warning | `projects/mdma-cli/` | Wired to new `bases/client/` |
| 3 | Project has no base dep | warning | `projects/mdma-gateway/` | Wired to `bases/service/` |

---

## Phase 1: Fix `bases/mdma_library` — convert to lib + create project

**Status: COMPLETE (2026-03-18)**

**Problem:** `bases/mdma_library/` has `src/main.rs`. It is the library daemon binary.
It wires together: `service` base + `library_service` component + `clap` CLI args.
There is no `projects/mdma-library/` yet.

**Decision from prior brainstorm:** `library_service` is that project's implementation,
not a reusable component. It should move into the project, not stay in `components/`.

### Step 1.1 — Create `projects/mdma-library/` workspace

Create `projects/mdma-library/Cargo.toml` as a `[workspace]` with a single binary crate.

The project depends on:
- `service` base via `path = "../../bases/service"`
- `library-service` component via `path = "../../components/library_service"`
- `clap`, `tokio`, `color-eyre`, `tracing`, `tracing-subscriber`

Move all code from `bases/mdma_library/src/main.rs` into the project's `src/main.rs`.
The `main.rs` should be thin: parse CLI args, build `ServiceConfig`, call `service::run()`.

**Validate:**
- `cd projects/mdma-library && cargo build` succeeds
- Binary runs and handles NNG requests identically to the old `mdma_library` base binary
- Smoke test: start the binary, confirm it registers IPC socket and responds

### Step 1.2 — Assess: keep or remove `bases/mdma_library`

After the project exists, ask: does `bases/mdma_library` expose any API reused by other
projects? If not, remove it entirely. The `service` base already covers the NNG daemon
skeleton. A dedicated library base is only warranted if multiple projects need a
library-daemon-specific skeleton beyond what `service` provides.

**Most likely outcome:** remove `bases/mdma_library` entirely.

### Step 1.3 — Decide on `components/library_service`

`library_service` is only used by the library daemon. Options:

**Option A (recommended):** Move `components/library_service/` code into
`projects/mdma-library/src/`. Remove from `components/` and root workspace.
This reflects the architectural truth: library_service is this project's impl, not a
reusable interface.

**Option B:** Keep it as a component if there's any chance another project will need it.
Leave it in place but accept it's currently orphaned (flagged by check as unreachable).

Do not over-engineer. If no other project ever needs library logic, Option A is correct.

**Outcome: Option B was chosen.** `tests/bdd` directly imports `library_service` to
construct `LibraryService` in-process for BDD tests. Moving it into the project would
have broken the BDD test suite, which depends on it as a component. It remains in
`components/library_service`.

**Validate:**
- `cargo polylith check` no longer reports "Base has main.rs" for mdma_library
- Root workspace builds cleanly (`cargo build`)
- `projects/mdma-library && cargo build` still succeeds

---

## Phase 2: Add a base to `projects/mdma-cli`

**Status: COMPLETE (2026-03-18)**

**Problem:** mdma-cli has no base dependency. It uses `nng-transport` for IPC connections
to the gateway but doesn't use the `service`, `http_server`, or `tui` bases.

**Decision from prior brainstorm:** Create `bases/client/` — a CLI/TUI client connection
skeleton exposing the NNG Req0 gateway connection setup.

### Step 2.1 — Create `bases/client/` lib crate

Extract from `projects/mdma-cli/src/main.rs` the shared client connection skeleton:
- `--node` argument pattern and gateway address derivation
- NNG Req0 connection setup with timeouts and reconnect policy
- Event subscription (Sub0) setup
- `color_eyre::install()` and tracing setup

**Actual API implemented:**

```rust
pub struct ClientConfig {
    pub node: String,       // e.g. "mdma-909.local"
    pub gateway_port: u16,
    pub event_port: u16,
}

impl ClientConfig {
    pub fn gateway_addr(&self) -> String { ... }
    pub fn event_addr(&self) -> String { ... }
}
```

**Do not** put clap `Args` structs in the base. Each project defines its own CLI.

**Validate:**
- `cargo build -p client` succeeds
- No `src/main.rs` in the crate
- `cargo polylith check` does not flag it

### Step 2.2 — Wire `projects/mdma-cli` to use `client` base

Add `client = { path = "../../bases/client" }` to `projects/mdma-cli/Cargo.toml`.
Remove the connection setup code that was extracted into the base.

**Validate:**
- `cd projects/mdma-cli && cargo build` succeeds
- All CLI commands still work identically
- `cargo polylith check` no longer warns for mdma-cli

### Step 2.3 — Consider wiring `projects/mdma-tui` similarly

mdma-tui already uses the `tui` base. It also does NNG client connections.
If the `client` base covers the connection setup, mdma-tui can depend on both `tui` and
`client` bases — multiple base dependencies are valid polylith.

**Outcome:** mdma-tui was wired to both `tui` and `client` bases. The connection setup
is shared through `bases/client/`.

---

## Phase 3: Add a base to `projects/mdma-gateway`

**Status: COMPLETE (2026-03-18)**

**Problem:** mdma-gateway has no base dependency. Its NNG topology is unique:
- Rep0 frontend (receives routed requests from clients)
- Multiple Req0 backends (proxies to each service daemon)
- Pub0/Sub0 event bridge

This is different from the `service` base's simple Rep0 request-response loop.

**Decision point:** Two paths:

**Path A — Use `service` base for the Rep0 frontend only.**
The gateway's Rep0 loop is structurally similar to other services. The backend Req0
connections and event bridge are project-specific wiring on top. Wire mdma-gateway to
`service` base for the request-handling skeleton, add the backend connections as
project code.

**Path B — Gateway is a legitimate exception.**
The `cargo polylith check` warning for "project has no base dep" is advisory (exit 0).
If the gateway's topology genuinely doesn't fit any base, accepting the warning is valid.
Document the exception explicitly.

**Recommended approach:** Try Path A. If the `service` base API needs small changes to
accommodate the gateway's frontend (e.g. async handlers, event emission), evolve the base.
If the contortion is too large, choose Path B and add a suppression comment.

**Outcome: Path A was chosen.** `service::create_sockets()` is used for the Rep0 frontend
and Pub0 event publisher. TCP addresses work fine — the ipc:// directory creation guard
in the service base is a no-op for tcp:// addresses. The gateway's `connect_backend()`
intentionally diverges from `nng_transport::connect()`: it uses reconnect/dial_async
behavior that differs from the standard transport connect. This divergence is intentional
and preserved as project-specific code.

**Validate (Path A):**
- `cd projects/mdma-gateway && cargo build` succeeds
- Gateway routes requests to services identically
- `service` base API change (if any) does not break existing service projects
- `cargo polylith check` no longer warns for mdma-gateway

---

## Phase 4: Dissolve `components/mdma_client` (optional cleanup)

**Status: REJECTED**

**Reason:** `mdma_client` is not a fat facade. On audit it contains 735 lines of real
gateway-or-direct dispatch logic: `LibraryBackend`, `PlaybackBackend`, and `SourceClient`
implement the client transport strategy (direct IPC vs. routed gateway) used by mdma-cli,
mdma-tui, and BDD tests. This is substantive shared logic, not a thin wrapper hiding
dependencies. Dissolving it would duplicate this strategy across three callers.

`mdma_client` is kept as-is in `components/` as a proper component.

---

## Rollback Strategy

Each phase is independently deployable:
- Old binary code remains until the new project workspace is proven
- Root workspace and individual project workspaces are separate — removing a base from
  the root does not break project workspaces that path-dep it directly
- Do not delete a base crate until its replacement project passes a smoke test

---

## How to Run `cargo polylith check`

From the mdma repo root:
```bash
cargo polylith check
```

A clean migration produces zero errors (exit 0) and zero warnings.
Target state after all phases:
```
✓ No violations found  (as of 2026-03-18)
```

---

## Feeding Back to cargo-polylith

As violations are resolved, note any gaps in `cargo polylith check` detection:
- Does it correctly detect `main.rs` in bases/?
- Does it correctly detect project workspaces under `projects/` (vs root workspace members)?
- Does `cargo polylith deps` trace cross-workspace path deps?

File issues or improvements against `~/projects/cargo-polylith` as discovered.

---

## Phase 5: cargo-polylith metadata cleanup

**Status: PENDING**

`cargo polylith check` now reports 4 violations — 2 pre-existing orphans (acceptable, see §5a
of REFACTORING.md) and 2 new ones introduced by the stub work. This phase fixes them and
adds interface metadata so the tool understands which components are swappable alternatives.

### Current check output

```
[orphan]           audio-metadata — pre-existing, do not act
[orphan]           library-ingestion — pre-existing, do not act
[no-base]          mdma-bdd has no base dependency
[not-in-workspace] component 'library-service' at components/library_service_stub
                   not in root workspace members
```

### 5a. Rename the library-service stub

`components/library_service_stub/Cargo.toml` currently declares `name = "library-service"`,
which collides with the real component. Change it:

```toml
[package]
name = "library-service-stub"   # was "library-service"
version = "0.1.0"
edition = "2021"
```

Leave all `[dependencies]` unchanged.

**Validate:** `cargo build -p library-service-stub`

### 5b. Add the stub to root workspace members

**Root `Cargo.toml`** — add to `members`:

```toml
"components/library_service_stub",
```

**Validate:** `cargo build --workspace`

### 5c. Re-wire projects/bdd after the rename

`projects/bdd/Cargo.toml` currently declares:

```toml
library-service = { path = "../../components/library_service_stub" }
```

After the rename this no longer resolves (`library-service-stub` ≠ `library-service`).
Fix with the `package` key:

```toml
library-service = { path = "../../components/library_service_stub",
                    package = "library-service-stub" }
```

Cargo resolves `library-service-stub` from the given path and aliases it as
`library_service` inside the BDD crate. No change to any BDD test code.

**Validate:** `cd projects/bdd && cargo test -- -vv`

### 5d. Mark mdma-bdd as a test project

Add one table to `projects/bdd/Cargo.toml`:

```toml
[package.metadata.polylith]
test-project = true
```

This suppresses the `[no-base]` warning. `cargo polylith check` recognises it and
treats `mdma-bdd` as a development/test project rather than a deliverable.

**Validate:**

```bash
cargo polylith check
# expect: only the two pre-existing [orphan] warnings remain
```

### 5e. Add interface metadata to library-service components

**`components/library_service/Cargo.toml`** — add:

```toml
[package.metadata.polylith]
interface = "library-service"
```

**`components/library_service_stub/Cargo.toml`** — add:

```toml
[package.metadata.polylith]
interface = "library-service"
```

Since the real component's `name` matches the interface name, it is the default
implementation. The stub is registered as an alternative. No `[ambiguous-interface]`
warning will be emitted.

**Effect on tooling:**
- `cargo polylith info` will group and display both under the `library-service` interface label
- `cargo polylith edit` will show `library-service` in the interface column of the grid

**Validate:**

```bash
cargo polylith info
# Components section shows:
#   library-service   library-service
#                     library-service-stub

cargo polylith check
# Only the two expected [orphan] warnings — clean otherwise
```

### Future stubs

When a new stub component is created for any other interface (e.g. `audio-output-stub`),
apply the same pattern:

1. Give it a distinct package name: `name = "audio-output-stub"`
2. Add to root workspace members
3. Add `[package.metadata.polylith] interface = "audio-output"` to **both** the real
   component and the stub
4. In any project using the stub, wire via `package` key:
   ```toml
   audio-output = { path = "../../components/audio_output_stub",
                    package = "audio-output-stub" }
   ```
