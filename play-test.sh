#!/usr/bin/env bash

# Cleanup function
cleanup() {
    echo "Cleaning up..."
    # Kill the server if it's running
    if [ ! -z "$SERVER_PID" ]; then
        kill $SERVER_PID 2>/dev/null
    fi
    # Remove the socket file
    rm -f /tmp/mdma-commands
}

# Set up cleanup on script exit
trap cleanup EXIT

# Ensure any previous instance is cleaned up
cleanup

# Start the playback server in the background
cargo run --bin playback-server &
SERVER_PID=$!

# Wait a moment for the server to start
sleep 2

# Play the downloaded song
cargo run --bin media-ctl -- load \
    --library ~/music \
    --artist "Rick Astley" \
    --song "Rick Astley - Never Gonna Give You Up (Official Music Video)" \
    --channel A

cargo run --bin media-ctl -- play --channel A

# Wait for user input to stop
echo "Press Enter to stop playback"
read

# Stop playback (cleanup will handle server shutdown)
cargo run --bin media-ctl -- stop --channel A
