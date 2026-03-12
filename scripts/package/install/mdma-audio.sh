    mkdir -p /run/mdma/streams

    # Create mdma user if doesn't exist
    if ! id mdma >/dev/null 2>&1; then
        useradd -r -s /sbin/nologin -d /music -c "MDMA Service User" mdma || true
    fi

    # Add mdma user to audio, video, _pipewire groups for PipeWire access
    usermod -a -G audio,video,_pipewire mdma 2>/dev/null || true

    # Ensure PipeWire is set up for headless operation:
    # - Stock pipewire runit service must be enabled
    # - WirePlumber launched via PipeWire context.exec drop-in
    if [ -f /usr/share/examples/wireplumber/10-wireplumber.conf ]; then
        mkdir -p /etc/pipewire/pipewire.conf.d
        ln -sf /usr/share/examples/wireplumber/10-wireplumber.conf \
            /etc/pipewire/pipewire.conf.d/ 2>/dev/null || true
    fi
    # Enable stock pipewire service if not already enabled
    if [ -d /etc/sv/pipewire ] && [ ! -e /var/service/pipewire ]; then
        ln -sf /etc/sv/pipewire /var/service/pipewire
        echo "pipewire service enabled"
    fi

    # Set ownership
    chown -R mdma:mdma /run/mdma 2>/dev/null || true
