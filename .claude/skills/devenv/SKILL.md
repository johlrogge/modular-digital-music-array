---
name: devenv
description: Development environment for the MDMA project. Use when you need to know what tools are available, how to enter the devenv shell, how to add packages, what environment variables are set, or how to run builds and CI locally. Covers Nix/devenv setup, just recipes, cross-compilation toolchain, and reproducible environment conventions.
---

# MDMA Development Environment (devenv)

The MDMA project uses [devenv](https://devenv.sh) for a reproducible Nix-based development environment. All agents should prefer devenv-provided tools over system tools.

## Entering the Shell

```bash
devenv shell
```

On entry, the shell prints tool versions, the target Pi address, and confirms the `mdma` CLI is ready on `PATH`.

**Note for agents:** The devenv shell is already active when you are running inside it. You do not need to prefix commands with `devenv shell --`. All tools listed below are already on `PATH`.

## Tools Provided

All of these are available in the devenv shell without manual installation:

| Tool | Purpose |
|------|---------|
| `rustc` / `cargo` | Rust stable toolchain |
| `zig` | Zig compiler (used by cargo-zigbuild for cross-compilation) |
| `cargo-zigbuild` | Cross-compile Rust binaries for aarch64 without a full GCC toolchain |
| `just` | Task runner — all project recipes live in `justfile` |
| `bacon` | Background Rust checker |
| `clang` / `libclang` | Required for bindgen |
| `pkg-config` | Build-time library discovery |
| `alsa-lib` | ALSA audio headers for building playback components |
| `pipewire` | PipeWire headers for building playback components |
| `nmap` | Network scanning (Pi discovery via `just pi-scan`) |
| `sshpass` | SSH with password (used by `just deploy-dev`) |
| `gh` | GitHub CLI |
| `git` | Version control |
| `helix` | Text editor |
| `claude` | Claude Code CLI |
| `socat` | Used for Claude Code sandboxing |

### Rust Target

The `aarch64-unknown-linux-gnu` target is pre-installed by devenv. Cross-compilation to Raspberry Pi works out of the box with `cargo-zigbuild`.

## Environment Variables

These are set automatically by devenv in every shell session:

| Variable | Value | Purpose |
|----------|-------|---------|
| `MDMA_NODE` | `mdma-909.local` | Target Pi hostname; CLI derives gateway from this |
| `MDMA_SSH_KEY` | `/home/johlrogge/.ssh/mdma_pi` | SSH key for Pi access |
| `MDMA_PROJECT_ROOT` | `$PWD` at shell entry | Live checkout path (not the Nix store) |
| `LIBCLANG_PATH` | Nix store path | Required by bindgen |
| `PATH` | includes `$MDMA_PROJECT_ROOT/target/debug` | `mdma` CLI available directly |

**Important:** Never export or set `MDMA_GATEWAY` manually. The CLI derives the gateway address from `MDMA_NODE` automatically.

## devenv Scripts

These are shell scripts defined in `devenv.nix` and available as commands:

| Command | Purpose |
|---------|---------|
| `mdma-volume <0-1>` | Set audio sink volume on the Pi via PipeWire |
| `mdma-status` | Show service reachability and active PipeWire streams |

## Key `just` Recipes

Run `just --list` to see all available recipes grouped by category.

### Build

```bash
just watch              # Watch mode: check → test → build → clippy (recommended for development)
just build              # cargo build (all workspace members)
just beacon-cross       # Cross-compile beacon for aarch64 using cargo-zigbuild
just beacon-native      # Cross-compile beacon using native gcc toolchain
just console-cross      # Cross-compile mdma-console for aarch64
just library-cross      # Cross-compile mdma-library for aarch64
just gateway-cross      # Cross-compile mdma-gateway for aarch64
just bandcamp-cross     # Cross-compile mdma-bandcamp for aarch64
just playback-cross     # Cross-compile mdma-playback for aarch64 (sets up sysroot first)
just cli-cross          # Cross-compile mdma CLI for aarch64
```

### Test

```bash
just bdd                # Run BDD/Cucumber tests
cargo test              # All inline unit tests
cargo test --package X  # Tests for a single crate
cargo clippy            # Lint
```

### Deploy (dev iteration)

```bash
just deploy-dev         # Cross-compile beacon and deploy to welcome-to-mdma.local
just deploy-library     # Cross-compile and deploy mdma-library to mdma-909.local
just deploy-console     # Cross-compile and deploy mdma-console to mdma-909.local
just deploy-playback    # Cross-compile and deploy mdma-playback to mdma-909.local
just deploy-gateway     # Cross-compile and deploy mdma-gateway to mdma-909.local
just deploy-bandcamp    # Cross-compile and deploy mdma-bandcamp to mdma-909.local
just deploy-cli         # Cross-compile and deploy mdma CLI to mdma-909.local
```

### Package Building

```bash
just pkg-build-all      # Build all .xbps packages and index the repository
just pkg-beacon         # Build beacon .xbps package only
just pkg-library        # Build mdma-library .xbps package only
just pkg-console        # Build mdma-console .xbps package only
just pkg-playback       # Build mdma-playback .xbps package only
just pkg-gateway        # Build mdma-gateway .xbps package only
just pkg-bandcamp       # Build mdma-bandcamp .xbps package only
```

### CI

```bash
just ci-simulate        # Run the full CI pipeline locally (build + strip + package + test)
just ci-build-beacon    # CI-style beacon cross-compilation
just ci-build-library   # CI-style library cross-compilation
just ci-build-console   # CI-style console cross-compilation
just ci-build-playback  # CI-style playback cross-compilation
just ci-build-gateway   # CI-style gateway cross-compilation
just ci-build-bandcamp  # CI-style bandcamp cross-compilation
```

### Network / Pi Discovery

```bash
just pi-scan            # Full nmap scan for Raspberry Pi devices (~30s)
just pi-connect         # Find and auto-SSH to first Pi found
just pi-ssh             # SSH to provisioned Pi (mdma-909.local)
just pi-ssh-beacon      # SSH to beacon Pi (welcome-to-mdma.local)
just pi-wait            # Poll until a Pi appears on the network
just pi-check <IP>      # Check if a specific IP is a Raspberry Pi
```

## Adding New Packages to devenv

Edit `devenv.nix` and add the package to the `packages` list:

```nix
packages = with pkgs; [
  # ... existing packages ...
  your-new-package
];
```

Then restart the devenv shell to pick up the change:

```bash
exit
devenv shell
```

Agent definitions also live in `devenv.nix` under `claude.code.agents`. Never edit `.claude/agents/*.md` files directly — they are regenerated from `devenv.nix` when the shell restarts.

## Cross-Compilation Notes

- **Preferred method:** `cargo zigbuild` via `just beacon-cross` (or equivalent per-service recipes)
- **ZIG_GLOBAL_CACHE_DIR** must be set before running zigbuild; the justfile recipes handle this automatically
- The `aarch64-unknown-linux-gnu` Rust target is pre-installed — no `rustup target add` needed
- For playback (which links against PipeWire): use `just playback-cross`, which calls the setup-sysroot script first

## Running CI Locally

```bash
just ci-simulate
```

This runs the full legacy CI pipeline: cross-compile beacon, strip binary, package as tar.gz, verify ARM64 binary. It matches what GitHub Actions runs.

For the full xbps package pipeline (what the GitHub Actions `build-and-package.yml` workflow runs):

```bash
just pkg-build-all
```

## Reproducibility Principle

All agents must use devenv-provided tools. Do not install tools with `cargo install`, `npm install -g`, `pip install`, or system package managers to work around missing tools. If a tool is missing, add it to `devenv.nix`.
