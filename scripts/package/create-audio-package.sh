#!/usr/bin/env bash
# Create Void package for mdma-audio
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/create-service-package.sh" \
    --bin mdma-audio \
    --desc "MDMA file playback stream source" \
    --cargo-toml projects/mdma-audio/Cargo.toml \
    --deps "pipewire>=0 wireplumber>=0 libspa-alsa>=0 alsa-pipewire>=0" \
    --extra-install "$SCRIPT_DIR/install/mdma-audio.sh"
