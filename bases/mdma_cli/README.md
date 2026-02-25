# mdma_cli

Command-line interface for MDMA. Connects to the gateway and lets you search the library, control playback, export tracks, and more. The binary is named `mdma`.

Shell completions are available for bash, zsh, fish, elvish, and PowerShell.

## Build

```bash
cargo build --package mdma-cli
```

## Run

```bash
# The gateway address is derived from MDMA_NODE (set in devenv shell)
mdma --help

# Generate shell completions
mdma completions bash > ~/.bash_completion.d/mdma
```

## Back to workspace

[Workspace README](../../README.md)
