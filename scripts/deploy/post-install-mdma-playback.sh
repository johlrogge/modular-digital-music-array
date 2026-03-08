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
