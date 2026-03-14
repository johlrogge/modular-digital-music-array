# mdma-tui

Terminal UI for MDMA. Dual-pane track browser and queue manager with live playback feedback.

Version: 0.3.0

[Back to root README](../../README.md)

---

## What it does

- Dual-pane layout: browser (left) and queue (right) by default
- Modal keybindings: Normal mode for navigation, Playback mode (`p`) for transport controls
- Command palette (`:` prefix): `play`, `pause`, `stop`, `next`, `clear`, `shuffle`, `quit`, pane switching
- `q` appends the selected track to the queue; `Q` inserts it next
- `?` opens a help overlay
- Live queue sync: subscribes to the event bus and refreshes queue panes automatically on `QueueChanged`
- Intelligent column compression: in narrow terminals, BPM and key columns are dropped first, then duration, then artist; the ` — ` separator between artist and title is hidden when artist is not shown

---

## How to run

```bash
# Build
cargo build -p mdma-tui

# Run (MDMA_NODE must point to a running Pi or localhost)
export MDMA_NODE="mdma-909.local"
mdma-tui
```

The TUI connects to the gateway at `tcp://$MDMA_NODE:5555` and the event bus at `tcp://$MDMA_NODE:5556`.

---

## Keybindings

| Key | Action |
|-----|--------|
| `j` / `k` | Move down / up |
| `q` | Append to queue |
| `Q` | Insert next in queue |
| `p` | Enter Playback mode |
| `:` | Open command palette |
| `?` | Help overlay |
| `Tab` | Switch active pane |
| `Esc` | Return to Normal mode |
