    mkdir -p /music/inbox /music/blobs /metadata /run/mdma

    # Create mdma user if doesn't exist
    if ! id mdma >/dev/null 2>&1; then
        useradd -r -s /sbin/nologin -d /music -c "MDMA Service User" mdma || true
    fi

    # Set ownership
    chown -R mdma:mdma /music /metadata /run/mdma 2>/dev/null || true
