---
name: architect
description: "Expert Rust development and architecture guidance. Use when working on Rust code including: debugging compiler errors (especially lifetime issues), designing type-safe APIs, implementing async patterns with tokio, evaluating architectural tradeoffs, applying Rust patterns (newtype, typestate, builder), improving code through type-driven design (making illegal states unrepresentable), error handling with thiserror/eyre, exploring ECS architectures beyond games, embedded development with Embassy on ESP32/Raspberry Pi, implementing Polylith architecture in Rust, and setting up development workflows with bacon and just."
---

# Architect — MDMA project context

MDMA-specific additions to the architect skill. Generic Rust guidance is built into the architect agent.

## MDMA Architecture

- **Polylith layout**: `components/` (shared libs), `bases/` (runtime API crates), `projects/` (binaries)
- **Builds**: `cargo polylith cargo --profile dev <cmd>` — the polylith tool generates the workspace
- **Production profile**: acid only, uses `fact_store_file` instead of `fact_store_memory`
- **IPC**: NNG sockets — library at `ipc:///run/mdma/library.sock`, playback at `ipc:///run/mdma/playback.sock`, gateway at `tcp://mdma-909.local:5555`
- **Protocol pattern**: `LibraryRequest`/`LibraryResponse` enums with serde_json over NNG; `AcidRequest`/`AcidResponse` for fact store
- **Fact store**: ACID — append-only, content-addressed, immutable facts via `MusicValue` enum
- **Error handling**: `thiserror` in libs/components, `color-eyre` in binaries

## Review focus for MDMA

- New `LibraryRequest` variants must be handled in both `library_service` and `library_service_stub`
- When adding protocol variants, all services embedding that protocol must be redeployed together (gateway, library, console all embed `library_ipc_protocol`)
- Hash resolution: handlers that accept `ContentHash` from user input must call `self.resolve_hash()` — partial hashes are valid CLI input
- Facts are immutable and append-only — "un-doing" a fact requires a new compensating fact (e.g. `Unbookmarked`)
- `FactOrigin::User` for facts written by the user (CLI/TUI); `FactOrigin::FilesystemScan` for scanner-written facts
