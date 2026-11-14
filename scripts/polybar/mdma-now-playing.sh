#!/usr/bin/env bash
#
# mdma-now-playing.sh — Polybar module for MDMA now-playing display
#
# Shows the currently playing track in your polybar status bar.
# Subscribes to MDMA pub/sub events for real-time updates.
#
# USAGE
# =====
#
#   mdma-now-playing.sh <node>
#
#   Example: mdma-now-playing.sh mdma-909.local
#
# INSTALLATION
# ============
#
# 1. Copy this script somewhere accessible:
#
#      cp scripts/polybar/mdma-now-playing.sh ~/.config/polybar/scripts/
#      chmod +x ~/.config/polybar/scripts/mdma-now-playing.sh
#
# 2. Add the following module to your polybar config:
#
#      [module/mdma]
#      type = custom/script
#      exec = /path/to/mdma-now-playing.sh mdma-909.local
#      tail = true
#      click-left = /path/to/mdma --node mdma-909.local playback start
#      click-middle = /path/to/mdma --node mdma-909.local playback skip
#      click-right = /path/to/mdma --node mdma-909.local playback stop
#
# 3. Add 'mdma' to your bar's modules list:
#
#      modules-right = ... mdma ...
#
# REQUIREMENTS
# ============
#
# - mdma CLI (build with: cargo polylith cargo --profile production build -p mdma-cli)
# - jq for JSON parsing
#
# CLICK ACTIONS
# =============
#
# Left click:   Start playback (play from queue)
# Middle click: Skip to next track
# Right click:  Stop playback

set -euo pipefail

NODE="${1:?Usage: mdma-now-playing.sh <node>}"

export PATH="$HOME/.cargo/bin:$PATH"
export MDMA_NODE="$NODE"

MAX_LEN=50

# Resolve a content hash to "Artist - Title"
resolve_track() {
    local hash="$1"
    local info
    info=$(echo "$hash" | mdma search 2>/dev/null | head -n 1) || true
    if [[ -n "$info" ]]; then
        # Pipe format: {8-char_hash}  {artist} - {title}  [{duration}]
        local rest="${info#*  }"
        rest="${rest%  \[*\]}"
        echo "$rest"
    else
        # Fallback: show short hash
        echo "${hash:7:8}"
    fi
}

# Truncate string to max length with ellipsis
truncate() {
    local str="$1"
    if (( ${#str} > MAX_LEN )); then
        printf '%s\u2026\n' "${str:0:$((MAX_LEN - 1))}"
    else
        echo "$str"
    fi
}

# Output a line for polybar to display
display() {
    local text="$1"
    if [[ -n "$text" ]]; then
        truncate "$text"
    else
        echo " "
    fi
}

# Query current playback state
init_state() {
    local hash
    hash=$(mdma playback now 2>/dev/null) || true
    if [[ -n "$hash" && "$hash" == sha256:* ]]; then
        resolve_track "$hash"
    fi
}

main() {
    # Show initial state
    display "$(init_state)"

    # Subscribe to playback events (pipe mode outputs one JSON object per line)
    mdma subscribe --topic playback/ 2>/dev/null | while IFS= read -r line; do
        local event_type
        event_type=$(echo "$line" | jq -r '.type // empty' 2>/dev/null) || continue

        case "$event_type" in
            TrackStarted)
                local hash
                hash=$(echo "$line" | jq -r '.hash // empty' 2>/dev/null) || continue
                if [[ -n "$hash" ]]; then
                    display "$(resolve_track "$hash")"
                fi
                ;;
            TrackEnded|TrackStopped)
                display ""
                ;;
        esac
    done
}

main
