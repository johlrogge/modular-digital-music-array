#!/bin/sh
sudo usermod -a -G audio,video,_pipewire mdma 2>/dev/null || true
# Ensure PipeWire WirePlumber drop-in is configured
sudo mkdir -p /etc/pipewire/pipewire.conf.d
sudo ln -sf /usr/share/examples/wireplumber/10-wireplumber.conf /etc/pipewire/pipewire.conf.d/ 2>/dev/null || true
# Ensure stock pipewire service is enabled
if [ -d /etc/sv/pipewire ] && [ ! -e /var/service/pipewire ]; then
    sudo cp -a /usr/share/examples/sv/pipewire /etc/sv/pipewire
    sudo ln -sf /etc/sv/pipewire /var/service/pipewire
    sleep 3
fi
sudo chown -R mdma:mdma /run/mdma

# Set default audio sink volume to 100% via WirePlumber config drop-in.
# WirePlumber's built-in default is 0.064 (~-24dBFS), which causes audio to
# be silent after reboot. This drop-in overrides it to 1.0 (0dBFS = 100%).
sudo mkdir -p /etc/wireplumber/wireplumber.conf.d
sudo tee /etc/wireplumber/wireplumber.conf.d/99-mdma-volume.conf > /dev/null << 'WPCFG'
# MDMA: Set default audio sink volume to 100%
wireplumber.settings = {
  device.routes.default-sink-volume = 1.0
}
WPCFG

# Allow PipeWire graph to switch between common sample rates natively.
# Without this, the graph is locked to 48000 and every 44.1 kHz source
# (MP3, much of the Bandcamp library) is resampled, causing choppy playback.
sudo mkdir -p /etc/pipewire/pipewire.conf.d
sudo tee /etc/pipewire/pipewire.conf.d/10-mdma-rates.conf > /dev/null << 'PWCFG'
# MDMA: allow PipeWire to switch to common audio sample rates natively.
# Without this override, the graph is locked to 48000 and every 44.1 kHz
# source (MP3, much of the bandcamp library) gets resampled. Include
# 44.1 kHz and its 2x/4x relatives plus the standard 48/96 kHz.
context.properties = {
    default.clock.allowed-rates = [ 44100 48000 88200 96000 ]
}
PWCFG

# Restart PipeWire and WirePlumber so the new configs take effect.
# (The volume drop-in also requires a restart to apply on reinstall.)
sudo sv restart pipewire 2>/dev/null || true
sudo sv restart wireplumber 2>/dev/null || true
