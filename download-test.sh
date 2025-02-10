#!/usr/bin/env bash

# Create output directory
mkdir -p ~/music

# Build and run download-cli
cargo run --bin download-cli -- -o ~/music "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
