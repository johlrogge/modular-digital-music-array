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
  - `playback_server` - Audio playback server
  - `library_crawler` - Music library indexing
  - `media_ctl`, `download_cli` - CLI tools

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

**SSH to Pi:** `ssh root@welcome-to-mdma.local` (password: `voidlinux`)

**Web UI:** `http://welcome-to-mdma.local`

**Package repo:** GitHub Pages at `https://johlrogge.github.io/modular-digital-music-array/`

## Testing

Tests are inline with `#[cfg(test)]` modules:
```bash
cargo test                    # All tests
cargo test --package beacon   # Single package
cargo test provisioning       # Filter by name
```

## Git Commit Guidelines

- Do NOT include "Generated with Claude Code" or similar references in commit messages
- Do NOT include "Co-Authored-By: Claude" or similar in commit messages
- Write commit messages as if written by the developer directly
