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
