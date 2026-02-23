# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

MDMA (Modular Distributed Music Architecture) is a distributed DJ system for Raspberry Pi 5. The goal is to move the music experience from a phone to a dedicated device with professional playback capabilities.

**Target Platform:** Raspberry Pi 5 running Void Linux with NVMe storage via M.2 HAT

## Build Commands

```bash
# Development
just watch              # Watch mode: check → test → build → clippy
cargo build             # Build all workspace members
cargo test              # Run all tests
cargo clippy            # Lint

# Cross-compile beacon for Raspberry Pi
just beacon-cross       # Uses cargo-zigbuild (recommended)
just beacon-native      # Uses aarch64-linux-gnu-gcc

# Deploy to running Pi
just deploy-dev         # Build and deploy to welcome-to-mdma.local

# Package building
just pkg-build-all      # Build xbps packages
just ci-simulate        # Full CI pipeline locally

# Find Pi on network
just pi-scan            # Network scan for Raspberry Pi
just pi-connect         # Find and auto-SSH to Pi
```

## Architecture

### Workspace Structure

- **bases/** - Binary entry points
  - `beacon` - Provisioning server with web UI (main focus)
  - `mdma_playback` - Audio playback server
  - `library_crawler` - Music library indexing
  - `mdma_cli` - Command-line interface
  - `mdma_console` - Web management console

- **components/** - Shared libraries
  - `playback_engine` - Real-time audio (Symphonia + PipeWire)
  - `music_primitives` - BPM, Key, Mode types
  - `playback_primitives` - Volume, Deck types
  - `media_protocol` - Command/Response protocol
  - `storage_primitives` - Type-safe ByteSize

### Beacon Provisioning Pipeline

The beacon provisions Raspberry Pi systems through 7 stages with type-safe chaining:

```
stage0_safety → stage1_validate → stage2_partition → stage3_format →
stage4_install → stage5_configure → stage6_finalize
```

Each stage implements the `Action` trait with `plan()` and `apply()` methods. The compiler enforces correct stage sequencing through the type system.

**Key files:**
- `bases/beacon/src/provisioning/mod.rs` - Stage orchestration
- `bases/beacon/src/provisioning/stage*.rs` - Individual stages
- `bases/beacon/src/routes/` - Axum web handlers

### Type-Safe Primitives

The codebase uses strongly-typed wrappers to prevent API misuse:
- `ByteSize` - Storage sizes with human-readable display
- `Volume` - dBFS values with linear conversion
- `Deck` - A/B deck enum
- `Bpm`, `Key`, `PitchClass` - Music theory types

## Development Environment

Uses Nix/devenv for reproducible environment. Enter with `devenv shell`.

Provides: Rust, Zig, cargo-zigbuild, PipeWire libs, nmap, sshpass, just

## Deployment

### Raspberry Pi

The provisioned Pi is at **mdma-909.local**.

**IMPORTANT: Never wipe or delete `/music` on the Pi unless explicitly instructed. It contains the music library and downloaded tracks.**

### SSH Access

Always use `/home/johlrogge/.ssh/mdma_pi` as the SSH key when connecting to the Pi.

**Unprovisioned Pi (beacon mode):**
```bash
ssh root@welcome-to-mdma.local  # password: voidlinux
```

**Provisioned Pi:**
```bash
ssh -4 -i ~/.ssh/mdma_pi admin@mdma-909.local  # key-based auth, -4 forces IPv4
```

### Service Access

All services are behind the gateway. Only port 5555 is exposed externally.

- **Gateway TCP (remote):** `tcp://mdma-909.local:5555` — routes to all services
- **Library IPC (local):** `ipc:///run/mdma/library.sock`
- **Playback IPC (local):** `ipc:///run/mdma/playback.sock`
- **Source sockets (local):** `/run/mdma/sources/*.sock` (auto-discovered by gateway)

MDMA_GATEWAY is already set in the devenv shell. Never export or prefix commands with it.

**Bandcamp username:** `johlyroger`

**Web UI:** `http://welcome-to-mdma.local` (beacon) or `http://mdma-909.local` (provisioned)

**Package repo:** GitHub Pages at `https://johlrogge.github.io/modular-digital-music-array/`

## Testing

Tests are inline with `#[cfg(test)]` modules:
```bash
cargo test                    # All tests
cargo test --package beacon   # Single package
cargo test provisioning       # Filter by name
```

## Coordinator Rules

You are a **coordinator**. You talk to Joakim and dispatch work to agents. You do NOT do work yourself.

- **NEVER** write, edit, or create code files — use code-minion
- **NEVER** SSH to the Pi or deploy — use devops
- **NEVER** explore the codebase deeply (more than a quick glance) — use Explore agents
- **NEVER** commit — use commit agent
- **NEVER** run cargo build/test/clippy — agents do this
- **DO** summarize agent output concisely for Joakim
- **DO** suggest which agent to use next based on context
- **DO** suggest new agents when no existing agent fits
- **DO** after minion-herder reports back, dispatch rust-architect to review all changed
  files for duplication, inconsistencies, and other code quality concerns before relaying
  results to Joakim
- **NEVER** edit `.claude/agents/*.md` files directly — agent definitions live in
  `devenv.nix`; changes there regenerate the agent files on devenv shell restart

Workflow:
1. Joakim states intent → dispatch to glenn-c for planning
2. Plan approved → dispatch to minion-herder with the approved plan
3. minion-herder executes via subagents, reports back
4. Dispatch rust-architect to review changed files — duplication, inconsistencies, missed
   reuse, fragility; if issues found, dispatch minion-herder to fix, then re-run until clean
5. You relay results to Joakim

## Git Commit Guidelines

- Do NOT include "Generated with Claude Code" or similar references in commit messages
- Do NOT include "Co-Authored-By: Claude" or similar in commit messages
- Write commit messages as if written by the developer directly
