# MIGRATION.md -- mdma Polylith Architecture Migration

## Goal

Migrate mdma from a flat Cargo workspace with misidentified "bases" to a proper
polylith architecture with three true base archetypes, ten project workspaces,
and clean component composition.

### Target structure

```
Cargo.toml                  # Root workspace: components + bases only
components/                 # 26 components (lib crates, pure domain logic)
bases/                      # 3 reusable archetypes (lib crates)
  service/                  #   NNG IPC daemon skeleton
  http_server/              #   Axum web server skeleton
  client/                   #   CLI/TUI client connection skeleton
projects/                   # 10 deployable units (each its own Cargo workspace)
  mdma-acid/
  mdma-audio/
  mdma-bandcamp/
  mdma-gateway/
  mdma-library/
  mdma-playback/
  beacon/
  mdma-console/
  mdma-cli/
  mdma-tui/
```

### Key architectural decisions (from brainstorm)

- **Projects own `fn main()`**. Bases are lib crates exposing a composable API.
- **Each project is its own Cargo workspace** with path deps into `components/` and `bases/`.
  This gives independent lockfiles, affected-set analysis for selective rebuilding, and
  `[patch]`-based implementation swapping.
- **Components can depend on other components' public APIs**. This is legitimate polylith --
  the interface is the public API, not a separate trait layer.
- **One `service` base** for all six NNG daemons. Variation (source, media_source) is expressed
  through project-level component selection, not base specialization.
- **`mdma_client` dissolves**. Connection policy extracts into its own component.
  Each client base composes only the IPC client components it needs.
- **`library_service` moves into the `mdma-library` project**. It is that project's
  implementation, not a reusable component.

---

## Migration sequence

### Phase 1: Extract the `service` base from mdma-acid

mdma-acid is the simplest NNG service. Use it as the template.

#### Step 1.1: Create `bases/service/` lib crate

Create `bases/service/` as a new lib crate in the root workspace.

Extract the shared service skeleton from `bases/mdma_acid/src/main.rs`:
- `color_eyre::install()`
- `tracing_subscriber::fmt()` with `EnvFilter`
- NNG Rep0 socket creation and `listen()`
- NNG Pub0 event socket creation and `listen()` (optional)
- IPC socket directory creation
- The request-receive / response-send loop

The base should expose something like:

```rust
pub struct ServiceConfig {
    pub name: &'static str,
    pub socket_address: String,
    pub event_address: Option<String>,
}

pub fn run<H>(config: ServiceConfig, handler: H) -> Result<()>
where
    H: Fn(&[u8]) -> Vec<u8>,
```

The exact API shape will emerge during extraction. Start simple, widen as needed.

**Do not** put clap `Args` structs in the base. Each project defines its own CLI.

**Validate:**
- `cargo build -p service` succeeds
- `cargo test -p service` passes (unit tests for the skeleton, if any)
- No binary target -- this is a lib crate

#### Step 1.2: Create `projects/mdma-acid/` as its own workspace

Create `projects/mdma-acid/Cargo.toml` as a `[workspace]` containing a single binary crate.
The binary crate depends on:
- `service` base via `path = "../../bases/service"`
- `acid_protocol` component via `path = "../../components/acid_protocol"`
- `event_protocol` component via `path = "../../components/event_protocol"`
- External crates: `stainless-facts`, `serde_json`, `clap`, `chrono`

Move the acid-specific code from `bases/mdma_acid/` into the project:
- `Args` struct (clap definition)
- `AcidError` enum
- `handle_request()`, `write_facts()`, `read_stream()`, `publish_facts_written()`, `count_facts_lines()`
- All tests

The project's `main.rs` should be thin:
- Parse `Args`
- Build the `ServiceConfig`
- Call `service::run()` with a closure/handler that dispatches to `handle_request()`

**Validate:**
- `cd projects/mdma-acid && cargo build` succeeds
- `cd projects/mdma-acid && cargo test` passes
- The resulting binary runs and handles NNG requests identically to the old version
- Run a manual smoke test: start the binary, send a Ping via nng, confirm Pong response

**Gotcha:** The project workspace needs its own `Cargo.lock`. This is expected and correct.
It will be independent of the root workspace's lockfile.

#### Step 1.3: Remove `bases/mdma_acid/` from the root workspace

Remove `"bases/mdma_acid"` from the root `Cargo.toml` `[workspace].members`.
Delete `bases/mdma_acid/` directory.

**Validate:**
- `cargo build` at the root still succeeds (acid is no longer a member)
- `cd projects/mdma-acid && cargo build` still succeeds
- No other workspace member depended on `mdma-acid` (bases should not depend on bases)

---

### Phase 2: Validate the `service` base against a second service

Pick mdma-playback -- it is more complex than mdma-acid (tokio runtime, Pub0 events,
audio streaming, acid-client dependency). This will stress-test the base API.

#### Step 2.1: Create `projects/mdma-playback/` workspace

Same structure as mdma-acid's project. Binary crate depending on `service` base
plus playback-specific components: `playback_engine`, `acid_client`, `event_protocol`,
`stream_source_protocol`, etc.

**Expect the `service` base API to need changes.** mdma-playback uses a tokio runtime
(`Runtime::new()` + `runtime.block_on()`), while mdma-acid runs a synchronous loop.
The base needs to accommodate both patterns, or the base provides the runtime and
the handler is async.

This is the critical design moment. Options:
1. Base provides tokio runtime, handler is `async fn`
2. Base is sync, projects that need async bring their own runtime
3. Base provides both sync and async entry points

Let the code tell you which is right. Do not over-design upfront.

**Validate:**
- `cd projects/mdma-playback && cargo build` succeeds
- `cd projects/mdma-playback && cargo test` passes
- Playback binary runs and works identically to the old version
- `service` base API accommodates both acid and playback without contortion

#### Step 2.2: Remove `bases/mdma_playback/` from root workspace

Same as Step 1.3.

---

### Phase 3: Migrate remaining services

With the `service` base proven against two real services, migrate the remaining four
one at a time. Order by complexity (simplest first):

1. **mdma-bandcamp** -- source service, relatively simple
2. **mdma-audio** -- media source, has PipeWire/audio dependencies
3. **mdma-library** -- largest service; `library_service` component absorbs into this project
4. **mdma-gateway** -- the hub; most complex NNG topology (Rep0 frontend + multiple Req0 backends + Pub0/Sub0 event bridge)

For each:

#### Step 3.N.1: Create `projects/<name>/` workspace

Follow the same pattern as mdma-acid and mdma-playback.

#### Step 3.N.2: Validate

- Build and test in the project workspace
- Smoke test the running binary
- Confirm the `service` base API still holds (or evolve it if needed)

#### Step 3.N.3: Remove old `bases/<name>/` from root workspace

**Specific gotchas per service:**

- **mdma-library**: Move `components/library_service/` code into the project. Remove
  `library_service` from `components/` and the root workspace. Update any other crate
  that depended on `library_service` (there should be none -- it was only used by mdma-library).

- **mdma-gateway**: Has a unique NNG topology -- it is both a Rep0 server (frontend) and
  multiple Req0 clients (backends), plus a Pub0/Sub0 event bridge. The `service` base may
  not cover all of this. Gateway might use the base for the Rep0 frontend skeleton only,
  with the backend connections and event bridge as project-specific code. Alternatively,
  gateway might not use the `service` base at all if its skeleton is too different. Decide
  when you get there.

---

### Phase 4: Extract the `http_server` base from beacon

#### Step 4.1: Create `bases/http_server/` lib crate

Extract the shared HTTP skeleton from `bases/beacon/src/main.rs`:
- `color_eyre::install()`
- `tracing_subscriber::fmt()` with `EnvFilter`
- `#[tokio::main]` or tokio runtime setup
- Axum router creation and `axum::serve()` binding
- Port/address CLI argument pattern

The base exposes something like:

```rust
pub async fn run(config: HttpServerConfig, router: axum::Router) -> Result<()>
```

**Validate:**
- `cargo build -p http_server` succeeds
- Lib crate, no binary

#### Step 4.2: Create `projects/beacon/` workspace

Move beacon-specific code into the project: hardware detection, provisioning,
update checking, config, server routes, templates.

**Validate:**
- `cd projects/beacon && cargo build` succeeds
- `cd projects/beacon && cargo test` passes
- Beacon runs identically

#### Step 4.3: Migrate `mdma-console` to use `http_server` base

Create `projects/mdma-console/` workspace. mdma-console also uses NNG for IPC
to backend services, so it may depend on both `http_server` and have NNG client
connections as project-specific code.

**Validate:**
- Console runs identically
- Both beacon and console use the same `http_server` base

#### Step 4.4: Remove old `bases/beacon/` and `bases/mdma_console/` from root workspace

---

### Phase 5: Extract the `client` base from mdma-cli

#### Step 5.1: Create `bases/client/` lib crate

Extract the shared client connection skeleton from `bases/mdma_cli/src/main.rs`:
- `--node` argument pattern and gateway address derivation
- NNG Req0 connection setup with timeouts and reconnect
- Event subscription (Sub0) setup
- `color_eyre::install()`

This is also where the extracted connection policy from `mdma_client` lives -- or
that becomes a separate `connection_policy` component the base depends on.

**Validate:**
- `cargo build -p client` succeeds

#### Step 5.2: Create `projects/mdma-cli/` workspace

Move CLI-specific code into the project: clap subcommands, output formatting,
all command handlers. The project depends on `client` base plus only the IPC
client components it actually uses.

**Validate:**
- `cd projects/mdma-cli && cargo build` succeeds
- All CLI commands work identically

#### Step 5.3: Create `projects/mdma-tui/` workspace

Same pattern for the TUI. Different set of IPC client components.

**Validate:**
- TUI runs identically

#### Step 5.4: Remove old `bases/mdma_cli/` and `bases/mdma_tui/` from root workspace

---

### Phase 6: Dissolve `mdma_client`

This can happen during Phase 5 or after, depending on how the `client` base
shapes up.

#### Step 6.1: Extract connection policy into a component

Create `components/connection_policy/` (or add to `nng_transport`).
Move DNS lookup, caching, timeout configuration out of `mdma_client`.

#### Step 6.2: Update IPC client components

Ensure `acid_client`, `gateway_client`, `library_ipc_client`, `media_client`
each use the connection policy component directly instead of relying on
`mdma_client` for consistent connection behavior.

#### Step 6.3: Remove `mdma_client`

Remove from root workspace and delete directory.

**Validate:**
- All projects that previously used `mdma_client` still build and work
- Each project depends only on the specific IPC client components it needs
- `cargo polylith check` shows no unexpected dependency edges

---

### Phase 7: Clean up root workspace

After all migrations:

#### Step 7.1: Verify root workspace contents

The root `Cargo.toml` `[workspace].members` should contain only:
- `components/*` (minus `library_service` and `mdma_client`, which were removed)
- `bases/service`, `bases/http_server`, `bases/client`

No binaries. No `default-members`. No `[[bin]]` targets anywhere in the root workspace.

#### Step 7.2: Verify all projects build independently

```bash
for project in projects/*/; do
  (cd "$project" && cargo build && cargo test)
done
```

#### Step 7.3: Update CI

CI must now build and test each project workspace independently, not just
`cargo test` at the root. The root workspace build validates components and
bases compile. Each project build validates that the project assembles correctly.

---

## Rollback strategy

Each phase is independently deployable. If a phase breaks something:

1. The old code still exists in `bases/<name>/` until Step N.3 removes it
2. Do not remove the old base until the new project is proven in production
3. The root workspace can temporarily contain both the old base and the new project
   (they produce different binary names or the old one is removed from `default-members`)

---

## What cargo-polylith should learn to do

As this migration progresses, feed findings back to `cargo-polylith`:

- `cargo polylith check` should understand project workspaces under `projects/`
- `cargo polylith deps` should trace dependencies across workspace boundaries
- `cargo polylith affected <component>` should report which projects need rebuilding
  when a component changes
- `cargo polylith info` should show the three-tier structure: components, bases, projects
