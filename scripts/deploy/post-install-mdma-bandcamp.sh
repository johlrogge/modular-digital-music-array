#!/bin/sh
sudo mkdir -p /music/downloads /music/inbox /run/mdma/sources /var/lib/mdma /etc/mdma
sudo chown -R mdma:mdma /music /run/mdma /var/lib/mdma
if [ ! -f /etc/mdma/bandcamp.conf ]; then
    sudo install -Dm644 /tmp/conf /etc/mdma/bandcamp.conf
    echo "Installed default bandcamp.conf — edit MDMA_BANDCAMP_USERNAME if needed"
else
    echo "Skipping bandcamp.conf (already exists)"
fi
