#!/usr/bin/env bash
# Create Void package for mdma-playback
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/create-service-package.sh" \
    --bin mdma-playback \
    --desc "MDMA audio playback server" \
    --cargo-toml projects/mdma-playback/Cargo.toml \
    --deps "pipewire>=0 wireplumber>=0 libspa-alsa>=0 alsa-pipewire>=0" \
    --extra-install "$SCRIPT_DIR/install/mdma-playback.sh"
