    mkdir -p /run/mdma/sources

    # Create mdma user if doesn't exist
    if ! id mdma >/dev/null 2>&1; then
        useradd -r -s /sbin/nologin -d /music -c "MDMA Service User" mdma || true
    fi

    # Set ownership
    chown -R mdma:mdma /run/mdma 2>/dev/null || true
