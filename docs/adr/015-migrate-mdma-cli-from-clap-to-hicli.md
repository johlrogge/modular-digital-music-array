# ADR-015: Migrate mdma-cli from clap to hicli

## Status

Accepted, 2026-11-04

## Context

mdma-cli was a monolithic clap derive CLI with a central dispatch. hicli 0.2 (johlrogge/hicli, on bpaf 0.9)
models a command as a documented fn, a group as a documented module, an argument as a type (hicli ADR-006),
and adds curried groups and default commands (hicli ADR-011) — the two shapes mdma-cli needs
(`source <NAME> <cmd>`, `search [QUERY] | search fact-values-for`). Phase 1 split mdma-cli into one module
per future group with `fn x(args…, ctx: &Ctx) -> color_eyre::Result<()>` commands, argument newtypes and
positionals last.

## Decision

1. mdma-cli's parser is hicli 0.2; clap and clap_complete are removed.
2. **Parse-time validation for everything** ("make illegal states unrepresentable"): every command argument
   is a domain type constructed — validated — while parsing, so a command never receives an unconstructable
   value and bad input is a usage error (exit 2) before anything connects. Validate what the domain type
   requires and invent no rules: names (playlist names, item ids, output names) may be lenient; dates,
   hashes, ranges, files are strict. Values whose validity depends on another argument (sort -a/-d, view
   columns, playlist contains flags, fact field+value) are combined at parse time too.
   FromStr lives on the CLI newtype and delegates to the owning component's constructor/parser; a component
   is changed only when the rule itself belongs there. A typed hash on argv follows the same rule as a hash
   read from stdin. Concrete rules:
   - A track hash on argv follows the stdin rule: at least 8 lowercase hex characters, optional `sha256:`
     prefix; anything else is a usage error.
   - `--node` is parsed strictly as `host[:port]` by a `NodeSpec` type in the node_discovery component; a
     bad port is a usage error instead of silently becoming part of the host. MDMA_NODE uses the same
     parser but is not a usage error (see Mapping).
   - `--energy` is bounded to 1–10, the range of the existing `EnergyLevel` domain type.
3. **Standard option parsing only.** No special case for values starting with `-` (clap's
   `allow_hyphen_values` on `--started/--stopped/--added` is removed). A leading-dash value is written
   `--added=-7`; date expressions have plenty of non-dash forms.
4. **Accepted losses:** elvish and PowerShell completions; `mdma help <cmd>`; the "mdma source list" hint
   after a bad source; `--added -7` as two words. Also accepted: `--version` prints `Version: x`; help text
   is no longer clap's (semantically equivalent, re-baselined); `source NAME --help` shows the source
   group's page (hicli ADR-011 deferred); installed completion scripts must be regenerated.
5. `queue edit` / `playlist edit` use a unique temp file (separate change).

### Layout

- main.rs: `#[group] mod <g>;` per group, each carrying the group's doc comment; `fn app()` built with
  `app!{ globals: globals(), commands: [...] }` in today's order; `main` returns `app().run(mk_ctx)`.
- Root commands (ping status list get facts sort bookmark bookmarks view) are `#[command]` fns in the plain
  module tracks.rs, registered by path (`tracks::ping`); likewise subscribe::subscribe, transfer::upload,
  transfer::export, completions::generate_completions (hidden).
- Nested groups are file groups: library/{track,identity,dedup,archive}.rs; admin's service_mode is inline.

### Mapping

- Globals: `#[derive(Bpaf)] Globals` — socket/playback_socket `Option<String>` + env (None = defaulted,
  default applied in Ctx::new); sources_dir env + fallback; `--node` has no env (MDMA_NODE is read by
  node.rs, ADR-009). MDMA_NODE goes through the same NodeSpec parser but is validated lazily, only when a
  command resolves a node; a bad value is a runtime error naming the variable (exit 1), not a usage error,
  because it is environment rather than argv. `mk_ctx = |g| Ok(Ctx::new(g))`; the node stays lazily resolved.
- `source`: `#[group(args = SourceArg)]`, `list` a fixed sibling, per-source commands `#[command(curried)]`.
- Defaults: `search` → `query` (default) + `fact-values-for`; `library dedup` → `scan` (default, --apply) +
  `merge`; `library archive` → `list` (default) + `empty`. Prose of a default lives on its group.
- Completions: hidden `generate-completions <bash|zsh|fish>` printing `hicli::completion_script`.
- Errors: command errors map to exit 1 with the message chain; parse errors exit 2. Remaining
  `process::exit` calls are kept through the migration and converted afterwards.
- Argument type naming:
  - `<X>Arg` / `Opt<X>Arg` is a value parsed from a single argv token through `FromStr` (Opt wraps
    `Option<XArg>`, because hicli has no `Arg` impl for `Option`/`Vec`; every optional command parameter is an
    `Opt<X>Arg`, lists live in multi-value `*Args` structs).
  - Switches (`Yes`, `Force`, `Apply`, `ApplyUpdates`) and values combined from several arguments
    (`SortDirection`, `ContainsMode`, `ViewColumns`) are named for the domain concept.
  - `*Cli` structs are clap parse-only and disappear in P2.3; `*Args` structs carry several values and survive
    as Bpaf structs.
  - Free text with no domain rule stays `XArg(String)` with a `// Stays a String: <reason>` comment.

## Known hicli gaps we accept

hicli 0.3.0 fixed #19 (a default without options is shown in help), #20 and #21 (positional items in help,
usage lines name the binary), #27 and #28.

- #22, #23 (cosmetic help/test-macro issues).
- #26: errors do not name the flag.
- No per-source help page; a typo of a sibling before a default reads as the default's query.

## Alternatives considered

- **Stay on clap** — keeps the central dispatch and the stringly external_subcommand re-parse for sources.
- **Validate inside commands (status quo)** — rejected by decision 2: illegal values reach command bodies and
  fail after connecting.
- **Validating FromStr on ContentHash in music_primitives** — rejected: the full identifier is constructed
  infallibly by services; a typed prefix is a CLI concept.
- **Keep hyphen-leading values via a local bpaf helper** — rejected by decision 3: non-standard parsing is
  bad UX.

## Consequences

- Bad input exits 2 with the parser's message instead of custom text and exit 1.
- Commands are plain fns testable directly; parse tests use `cli_test!`/`App::parse`.
- bpaf's positionals-last rule is a parameter-order rule (checked at fold time in debug builds).
- node_discovery gains a NodeSpec type (the only component change).
