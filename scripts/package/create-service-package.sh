#!/usr/bin/env bash
# Generic Void package creator for MDMA services
#
# Usage:
#   create-service-package.sh \
#     --bin NAME          binary name (e.g. mdma-library)
#     --svc NAME          service/package name, defaults to --bin value
#     --desc TEXT         xbps package description
#     --cargo-toml PATH   path to Cargo.toml (relative to project root)
#     [--deps TEXT]       xbps runtime dependencies
#     [--extra-dirs DIRS] space-separated extra dirs to create inside the package
#     [--extra-install FILE] path to shell snippet appended inside the post) block
#     [--extra-files SRC:DEST ...] extra files to copy into the package tree

set -euo pipefail

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
BIN=""
SVC=""
DESC=""
CARGO_TOML=""
DEPS=""
EXTRA_DIRS=""
EXTRA_INSTALL=""
EXTRA_FILES=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --bin)         BIN="$2";          shift 2 ;;
        --svc)         SVC="$2";          shift 2 ;;
        --desc)        DESC="$2";         shift 2 ;;
        --cargo-toml)  CARGO_TOML="$2";   shift 2 ;;
        --deps)        DEPS="$2";         shift 2 ;;
        --extra-dirs)  EXTRA_DIRS="$2";   shift 2 ;;
        --extra-install) EXTRA_INSTALL="$2"; shift 2 ;;
        --extra-files) shift
                       while [[ $# -gt 0 && "$1" != --* ]]; do
                           EXTRA_FILES+=("$1")
                           shift
                       done ;;
        *) echo "Unknown argument: $1" >&2; exit 1 ;;
    esac
done

# ---------------------------------------------------------------------------
# Validate required args
# ---------------------------------------------------------------------------
if [[ -z "$BIN" ]]; then
    echo "Error: --bin is required" >&2
    exit 1
fi
if [[ -z "$DESC" ]]; then
    echo "Error: --desc is required" >&2
    exit 1
fi
if [[ -z "$CARGO_TOML" ]]; then
    echo "Error: --cargo-toml is required" >&2
    exit 1
fi

# Default service name = binary name
SVC="${SVC:-$BIN}"

# ---------------------------------------------------------------------------
# Paths — all relative to project root (where the script is called from)
# ---------------------------------------------------------------------------
BINARY="target/aarch64-unknown-linux-gnu/release/${BIN}"
PACKAGE_DIR="build/package-${SVC}"
PACKAGES_DIR="build/packages"

echo "Creating ${SVC} Void package..."

# ---------------------------------------------------------------------------
# Verify binary
# ---------------------------------------------------------------------------
if [[ ! -f "$BINARY" ]]; then
    echo "Error: Binary not found at $BINARY" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# Create package directory structure
# ---------------------------------------------------------------------------
rm -rf "$PACKAGE_DIR"
mkdir -p "$PACKAGE_DIR/usr/bin"
mkdir -p "$PACKAGE_DIR/etc/sv/${SVC}/log"

# Extra dirs requested by the caller
if [[ -n "$EXTRA_DIRS" ]]; then
    for dir in $EXTRA_DIRS; do
        mkdir -p "$PACKAGE_DIR/$dir"
    done
fi

# ---------------------------------------------------------------------------
# Copy binary
# ---------------------------------------------------------------------------
echo "  -> Copying ${BIN} binary..."
cp "$BINARY" "$PACKAGE_DIR/usr/bin/"
chmod +x "$PACKAGE_DIR/usr/bin/${BIN}"

# ---------------------------------------------------------------------------
# Copy run script from void-packages (single source of truth)
# Fall back to a minimal generated script if not present.
# ---------------------------------------------------------------------------
RUN_SCRIPT="void-packages/srcpkgs/${SVC}/files/${SVC}/run"
if [[ -f "$RUN_SCRIPT" ]]; then
    echo "  -> Copying service script from void-packages..."
    cp "$RUN_SCRIPT" "$PACKAGE_DIR/etc/sv/${SVC}/run"
else
    echo "  -> Service script not found in void-packages, generating default..."
    cat > "$PACKAGE_DIR/etc/sv/${SVC}/run" <<RUNSCRIPT
#!/bin/sh
exec 2>&1
exec chpst -u mdma /usr/bin/${BIN}
RUNSCRIPT
fi
chmod +x "$PACKAGE_DIR/etc/sv/${SVC}/run"

# ---------------------------------------------------------------------------
# Log service
# ---------------------------------------------------------------------------
echo "  -> Creating log service script..."
cat > "$PACKAGE_DIR/etc/sv/${SVC}/log/run" <<LOGSCRIPT
#!/bin/sh
exec svlogd -tt /var/log/${SVC}
LOGSCRIPT
chmod +x "$PACKAGE_DIR/etc/sv/${SVC}/log/run"

# ---------------------------------------------------------------------------
# Extra files (--extra-files SRC:DEST)
# ---------------------------------------------------------------------------
for mapping in "${EXTRA_FILES[@]+"${EXTRA_FILES[@]}"}"; do
    src="${mapping%%:*}"
    dest="${mapping#*:}"
    dest_dir="$(dirname "$PACKAGE_DIR/$dest")"
    mkdir -p "$dest_dir"
    echo "  -> Copying extra file: $src -> $PACKAGE_DIR/$dest"
    cp "$src" "$PACKAGE_DIR/$dest"
done

# ---------------------------------------------------------------------------
# INSTALL script
# We always generate the boilerplate (log dir, service symlink, restart).
# Service-specific bits are read from --extra-install snippet if provided.
# ---------------------------------------------------------------------------
echo "  -> Creating INSTALL script..."

# Build the extra-install content once so we can embed it safely
EXTRA_INSTALL_CONTENT=""
if [[ -n "$EXTRA_INSTALL" ]]; then
    if [[ ! -f "$EXTRA_INSTALL" ]]; then
        echo "Error: --extra-install file not found: $EXTRA_INSTALL" >&2
        exit 1
    fi
    EXTRA_INSTALL_CONTENT="$(cat "$EXTRA_INSTALL")"
fi

# Write the INSTALL script using printf to avoid heredoc quoting surprises
{
    printf '#!/bin/sh\n'
    printf '# INSTALL script for %s package\n' "$SVC"
    printf 'case "${ACTION}" in\n'
    printf 'post)\n'
    printf '    # Create log directory for svlogd\n'
    printf '    mkdir -p /var/log/%s\n' "$SVC"
    printf '\n'
    if [[ -n "$EXTRA_INSTALL_CONTENT" ]]; then
        printf '%s\n' "$EXTRA_INSTALL_CONTENT"
        printf '\n'
    fi
    printf '    # Enable service\n'
    printf '    if [ ! -d /var/service ]; then\n'
    printf '        mkdir -p /var/service\n'
    printf '    fi\n'
    printf '    if [ ! -e /var/service/%s ]; then\n' "$SVC"
    printf '        ln -sf /etc/sv/%s /var/service/%s\n' "$SVC" "$SVC"
    printf '        echo "%s service enabled"\n' "$SVC"
    printf '    fi\n'
    printf '\n'
    printf '    # Restart if running\n'
    printf '    if sv status %s >/dev/null 2>&1; then\n' "$SVC"
    printf '        sv restart %s\n' "$SVC"
    printf '        echo "%s service restarted"\n' "$SVC"
    printf '    fi\n'
    printf '    ;;\n'
    printf 'esac\n'
} > "$PACKAGE_DIR/INSTALL"
chmod +x "$PACKAGE_DIR/INSTALL"

# ---------------------------------------------------------------------------
# Version from Cargo.toml
# ---------------------------------------------------------------------------
if [[ ! -f "$CARGO_TOML" ]]; then
    echo "Error: $CARGO_TOML not found!" >&2
    exit 1
fi

RAW=$(grep '^version[. =]' "$CARGO_TOML" | head -1)
if echo "$RAW" | grep -q 'workspace = true'; then
    REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
    VERSION=$(awk '/^\[workspace\.package\]/{flag=1; next} /^\[/{flag=0} flag && /^version/{gsub(/[" ]/,""); split($0,a,"="); print a[2]; exit}' "${REPO_ROOT}/Cargo.toml")
else
    VERSION=$(echo "$RAW" | sed 's/version *= *"\(.*\)"/\1/')
fi
echo "  Version from ${CARGO_TOML}: ${VERSION}"
if [[ -z "$VERSION" ]]; then
    echo "Error: Could not extract version from $CARGO_TOML" >&2
    exit 1
fi

REVISION="1"
FULLVERSION="${VERSION}_${REVISION}"
echo "  -> Package version: ${FULLVERSION}"

# ---------------------------------------------------------------------------
# xbps-create
# ---------------------------------------------------------------------------
mkdir -p "$PACKAGES_DIR"
PACKAGE_DIR_ABS=$(realpath "$PACKAGE_DIR")
PACKAGES_DIR_ABS=$(realpath "$PACKAGES_DIR")

echo "  -> Directory contents:"
ls -lR "$PACKAGE_DIR_ABS" | head -30

cd "$PACKAGES_DIR_ABS"

echo "  -> Running xbps-create..."

XBPS_ARGS=(
    -A aarch64
    -n "${SVC}-${FULLVERSION}"
    -s "$DESC"
    -H "https://github.com/johlrogge/modular-digital-music-array"
    -l MIT
    -m "Joakim Ohlrogge <joakim.ohlrogge@agical.se>"
)

if [[ -n "$DEPS" ]]; then
    # xbps-create takes one -D flag per dependency group or accepts space-separated
    # The original scripts passed the whole string as a single -D argument.
    XBPS_ARGS+=(-D "$DEPS")
fi

XBPS_ARGS+=("$PACKAGE_DIR_ABS")

if XBPS_TARGET_ARCH=aarch64 xbps-create "${XBPS_ARGS[@]}" 2>&1; then
    echo "  -> xbps-create succeeded"
else
    XBPS_EXIT_CODE=$?
    echo "Error: xbps-create failed with exit code: $XBPS_EXIT_CODE" >&2
    echo "  -> Directory contents:"
    ls -la
    exit 1
fi

cd - > /dev/null

# ---------------------------------------------------------------------------
# Verify output
# ---------------------------------------------------------------------------
EXPECTED="$PACKAGES_DIR/${SVC}-${FULLVERSION}.aarch64.xbps"
if [[ ! -f "$EXPECTED" ]]; then
    echo "Error: Package not created! Expected: $EXPECTED" >&2
    ls -la "$PACKAGES_DIR/"
    exit 1
fi

echo ""
echo "Package created: $EXPECTED"
ls -lh "$EXPECTED"
