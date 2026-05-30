# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

MDMA (Modular Distributed Music Architecture) is a distributed DJ system for Raspberry Pi 5. The goal is to move the music experience from a phone to a dedicated device with professional playback capabilities.

**Target Platform:** Raspberry Pi 5 running Void Linux with NVMe storage via M.2 HAT

## Environment

This project runs in an immutable Nix environment managed by devenv.
**Do NOT** run `pip install`, `npm install -g`, `cargo install`, `brew install`,
`apt-get install`, or any other imperative package manager.
If a tool or package is missing, add it to `devenv.nix` and re-enter the shell.
All tools, packages, hooks, and services are declared in `devenv.nix`.

## Build Commands

Agents use the `mcp__rust-codebase__*` MCP tools (polylith-aware):
```
cargo_check        # fast compile check
cargo_test         # run tests
cargo_clippy       # lint
hygiene_report     # test + clippy + coverage in one shot
```

Other tasks via `just` (use `just --list` or `mcp__just__just_list` to see all):
```bash
just watch              # Watch mode: check → test → build → clippy

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

- **projects/** - Binary entry points (deployable services and tools)
  - `mdma-beacon` - Provisioning server with web UI (main focus)
  - `mdma-playback` - Audio playback queue manager
  - `mdma-audio` - File playback source (wraps PlaybackEngine, speaks stream_source_protocol)
  - `mdma-cli` - Command-line interface
  - `mdma-console` - Web management console
  - `mdma-library` - Library service
  - `mdma-gateway` - Gateway router
  - `mdma-bandcamp` - Bandcamp source

- **bases/** - Abstract Polylith base traits (client, service, http_server, tui)

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
- `projects/mdma-beacon/src/provisioning/mod.rs` - Stage orchestration
- `projects/mdma-beacon/src/provisioning/stage*.rs` - Individual stages
- `projects/mdma-beacon/src/routes/` - Axum web handlers

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

The provisioned Pi is at **mdma-johlyroger.local**.

**IMPORTANT: Never wipe or delete `/music` on the Pi unless explicitly instructed. It contains the music library and downloaded tracks.**

### SSH Access

Always use `/home/johlrogge/.ssh/mdma_pi` as the SSH key when connecting to the Pi.

**Unprovisioned Pi (beacon mode):**
```bash
ssh root@welcome-to-mdma.local  # password: voidlinux
```

**Provisioned Pi:**
```bash
ssh -4 -i ~/.ssh/mdma_pi admin@mdma-johlyroger.local  # key-based auth, -4 forces IPv4
```

### Service Access

All services are behind the gateway. Only port 5555 is exposed externally.

- **Gateway TCP (remote):** `tcp://mdma-johlyroger.local:5555` — routes to all services
- **Library IPC (local):** `ipc:///run/mdma/library.sock`
- **Playback IPC (local):** `ipc:///run/mdma/playback.sock`
- **Source sockets (local):** `/run/mdma/sources/*.sock` (auto-discovered by gateway)

MDMA_NODE is already set in the devenv shell. The CLI derives the gateway address from it automatically. Never export or prefix commands with MDMA_GATEWAY.

**Bandcamp username:** `johlyroger`

**Web UI:** `http://welcome-to-mdma.local` (beacon) or `http://mdma-johlyroger.local` (provisioned)

**Package repo:** GitHub Pages at `https://johlrogge.github.io/modular-digital-music-array/`

## Testing

Tests are inline with `#[cfg(test)]` modules. Use `mcp__rust-codebase__cargo_test` (polylith-aware) to run them.

## Coordinator Rules

You are a **coordinator**. You talk to Joakim and dispatch work to agents. You do NOT do work yourself.

- **NEVER** write, edit, or create code files — use code-minion
- **NEVER** SSH to the Pi or deploy — use devops
- **NEVER** explore the codebase deeply (more than a quick glance) — use Explore agents
- **NEVER** commit — use commit agent
- **NEVER** run cargo build/test/clippy — agents do this
- **NEVER** edit `.claude/agents/*.md` files directly — agent definitions live in
  `devenv.nix`; changes there regenerate the agent files on devenv shell restart
- **DO** summarize agent output concisely for Joakim
- **DO** suggest which agent to use next based on context
- **DO** suggest new agents when no existing agent fits
- **DO** dispatch agents immediately — don't describe what you're about to do and ask
  "ready to proceed?" — just do it
- **DO** parallelize independent tasks — dispatch multiple code-minions in a single
  message when changes touch separate crates/files with no dependencies

Workflow:
1. Joakim states intent → dispatch to glenn-c for planning
2. Plan approved → dispatch code-minion(s) directly with the approved plan
   - For large plans: split into independent phases, dispatch one code-minion per phase in parallel
   - Give each code-minion a clear, self-contained prompt with all necessary context
   - Code-minions must run cargo build/test/clippy before reporting back
3. Code-minion(s) report back → dispatch architect to review all changed files
   for duplication, inconsistencies, missed reuse, and fragility
4. If architect finds issues → dispatch code-minion to fix, then re-run architect
   until clean
5. Dispatch commit agent
6. Dispatch devops to deploy → dispatch test agent for smoke tests
7. Relay results to Joakim

### Git-flow Branching

- **Feature branches**: `git flow feature start <name>` for new features
- **Release branches**: `git flow release start <version>` for version bumps
- **Hotfix branches**: `git flow hotfix start <name>` for urgent fixes
- All development happens on feature branches off `develop`
- Only releases and hotfixes merge to `master`
- CI builds packages only on `master` push; tests run on all branches

## Tool Usage Policy

**Always prefer MCP tools over Bash.** This project inherits MCP servers
from metadev (`git-read`, `git-write`, `gh-issues`, `gh-ci`, `gh-repo`,
`rust-codebase`, `just`, `devenv`, `cargo-polylith`, `adr`, `ssh`,
`mcp-test`) — plus project-scoped servers (`just-dev`, `just-ci`,
`just-deploy`). Use them first.

When no MCP covers the operation:
1. State in one sentence why you're using Bash (e.g. "no MCP tool for
   `git ls-remote`").
2. Consider whether the gap is worth a feature request against metadev
   (`gh_issue_create` with `repo: "johlrogge/metadev"`,
   `label: "enhancement"`) or the project's own MCP server.

When an MCP tool exists but misbehaves:
- **Do not fall back to Bash as a workaround.** File a bug and/or fix
  the root cause. A silent fallback hides the defect.

### Release Workflow

All release steps are owned by the **release-manager** agent. Dispatch it with the version number — it handles everything from branch creation to pushing.

1. Dispatch **release-manager**: `git flow release start <version>`
2. Dispatch **code-minion**: bump version in root `Cargo.toml` (`[package] version`) and `void-packages/srcpkgs/*/template`
3. Dispatch **documenter**: update all READMEs
4. Dispatch **commit** agent: `chore(release): bump to <version>`
5. Dispatch **release-manager**: `git flow release finish <version>` + push master, develop, tags to `github`
6. CI builds and publishes packages (triggered by master push)
7. Dispatch **devops** to deploy updated packages to Pi and verify

See `RELEASING.md` for detailed procedure.

## Git Commit Guidelines

- Do NOT include "Generated with Claude Code" or similar references in commit messages
- Do NOT include "Co-Authored-By: Claude" or similar in commit messages
- Write commit messages as if written by the developer directly
- All commits must follow Conventional Commits format (see `.claude/skills/conventional-commits/SKILL.md`)
- Format: `<type>(<scope>): <description>` (e.g., `feat(export): add smart format selection`)
