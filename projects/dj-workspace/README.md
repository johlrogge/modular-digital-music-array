# dj-workspace

Bevy/egui GUI application for DJ session preparation. Laptop-only — not packaged for the Raspberry Pi.

[Back to workspace README](../../README.md)

---

## What it does

- **Playlist browser** — browse and manage MDMA playlists from the desktop
- **Candidate finder** — search the library with BPM, Key, Role, and Energy filters; results are sortable
- Connects to any MDMA node via the gateway; runs standalone on the laptop while the Pi handles playback

## Usage

```bash
# Connect to a named MDMA node (derives gateway and library socket automatically)
dj-workspace --node mdma-909.local

# Or set the node via environment variable
export MDMA_NODE=mdma-909.local
dj-workspace
```

## Build

```bash
# From workspace root (devenv shell)
cargo polylith cargo --profile dev build -p dj-workspace
```

dj-workspace is excluded from the Pi cross-compilation targets. Run it as a native binary on the laptop only.

## Architecture

Built on the `bevy-app` Polylith base (Bevy + bevy_egui). The `client` base provides `ClientConfig` for deriving gateway and library socket addresses from `--node`. The app talks to the library over the gateway — no direct Pi IPC required.
