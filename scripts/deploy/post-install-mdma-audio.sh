#!/bin/sh
sudo usermod -a -G audio,video,_pipewire mdma 2>/dev/null || true
sudo mkdir -p /run/mdma/streams
sudo chown mdma:mdma /run/mdma/streams
