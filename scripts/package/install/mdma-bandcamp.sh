    mkdir -p /run/mdma/sources /var/lib/mdma
    mkdir -p /music/downloads /music/inbox
    mkdir -p /etc/mdma

    # Install default conf if not already present
    if [ ! -f /etc/mdma/bandcamp.conf ]; then
        cp /etc/mdma/bandcamp.conf.example /etc/mdma/bandcamp.conf
        echo "mdma-bandcamp: installed default conf at /etc/mdma/bandcamp.conf"
        echo "  Edit MDMA_BANDCAMP_USERNAME to match your Bandcamp account"
    fi

    # Create mdma user if doesn't exist
    if ! id mdma >/dev/null 2>&1; then
        useradd -r -s /sbin/nologin -d /music -c "MDMA Service User" mdma || true
    fi

    # Set ownership
    chown -R mdma:mdma /music /run/mdma /var/lib/mdma 2>/dev/null || true
