    # Allow binding to privileged ports without root
    if command -v setcap >/dev/null 2>&1; then
        setcap 'cap_net_bind_service=+ep' /usr/bin/mdma-console
        echo "mdma-console: granted CAP_NET_BIND_SERVICE capability"
    else
        echo "WARNING: setcap not found, mdma-console may not bind to port 80"
    fi
