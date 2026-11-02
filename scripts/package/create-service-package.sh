#!/usr/bin/env bash
# Generic Void package creator for MDMA services.
#
# void-packages/srcpkgs/<pkg>/ is the single source of truth for everything
# except the binary and the version (which comes from Cargo.toml):
#
#   template              depends, short_desc, conf_files (sourced)
#   INSTALL, REMOVE       copied verbatim when present
#   files/<d>/            every dir containing a `run` is copied whole to
#                         etc/sv/<d>/ (down, log/run, ... preserved); a default
#                         log/run is generated only when absent
#   files/rootfs/**       copied verbatim into the package root
#
# Usage:
#   create-service-package.sh \
#     --srcpkg DIR          void-packages/srcpkgs/<pkg> (relative to the project root)
#     [--bin NAME]          binary name; the package ships it as usr/bin/<pkgname>
#                           (every MDMA binary is named like its package)
#     [--cargo-toml PATH]   manifest holding the version (relative to project
#                           root); default: projects/<pkg> or projects/mdma-<pkg>
#
# Always runs from the project root, whatever the caller's cwd.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/package/lib-srcpkg.sh
source "$SCRIPT_DIR/lib-srcpkg.sh"
cd "$LIB_REPO_ROOT"

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
SRCPKG=""
BIN=""
CARGO_TOML=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --srcpkg)      SRCPKG="$2";     shift 2 ;;
        --bin)         BIN="$2";        shift 2 ;;
        --cargo-toml)  CARGO_TOML="$2"; shift 2 ;;
        *) echo "Unknown argument: $1" >&2; exit 1 ;;
    esac
done

if [[ -z "$SRCPKG" ]]; then
    echo "Error: --srcpkg is required" >&2
    exit 1
fi
SRCPKG="${SRCPKG%/}"
if [[ ! -d "$SRCPKG" ]]; then
    echo "Error: srcpkg directory not found: $SRCPKG" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# Metadata from the template (single source of truth)
# ---------------------------------------------------------------------------
srcpkg_load "$SRCPKG"
SVC="$TPL_PKGNAME"
if [[ -z "$SVC" || "$SVC" != "$(basename "$SRCPKG")" ]]; then
    echo "Error: template pkgname '$SVC' does not match directory $(basename "$SRCPKG")" >&2
    exit 1
fi
if [[ -z "$TPL_SHORT_DESC" ]]; then
    echo "Error: short_desc missing in $SRCPKG/template" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# Paths — all relative to the project root
# ---------------------------------------------------------------------------
BIN="${BIN:-$SVC}"
if [[ "$BIN" != "$SVC" ]]; then
    echo "Error: binary '$BIN' must be named like the package '$SVC' (shipped as usr/bin/$SVC)" >&2
    exit 1
fi
BINARY="target/aarch64-unknown-linux-gnu/release/${BIN}"
PACKAGE_DIR="build/package-${SVC}"
PACKAGES_DIR="build/packages"

echo "Creating ${SVC} Void package..."

if [[ ! -f "$BINARY" ]]; then
    echo "Error: Binary not found at $BINARY" >&2
    exit 1
fi

if [[ ! -f "$SRCPKG/files/${SVC}/run" ]]; then
    echo "Error: $SRCPKG/files/${SVC}/run not found (every package needs its service)" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# Package directory structure
# ---------------------------------------------------------------------------
rm -rf "$PACKAGE_DIR"
mkdir -p "$PACKAGE_DIR/usr/bin"

# ---------------------------------------------------------------------------
# Binary
# ---------------------------------------------------------------------------
echo "  -> Copying ${BIN} binary..."
cp "$BINARY" "$PACKAGE_DIR/usr/bin/"
chmod +x "$PACKAGE_DIR/usr/bin/${BIN}"

# ---------------------------------------------------------------------------
# rootfs overlay (files/rootfs/** -> package root)
# ---------------------------------------------------------------------------
ROOTFS="$SRCPKG/files/rootfs"
if [[ -d "$ROOTFS" ]]; then
    echo "  -> Copying rootfs overlay..."
    cp -a "$ROOTFS/." "$PACKAGE_DIR/"
    # Normalised: 755 if the source has the user x-bit, else 644.
    while IFS= read -r -d '' rel; do
        chmod "$(data_file_mode "$ROOTFS/$rel")" "$PACKAGE_DIR/$rel"
    done < <(cd "$ROOTFS" && find . -type f -printf '%P\0')
fi

# ---------------------------------------------------------------------------
# Runit service directories (files/<d>/ with a `run` -> etc/sv/<d>/)
# ---------------------------------------------------------------------------
while read -r name; do
    [[ -n "$name" ]] || continue
    src="$SRCPKG/files/$name"
    if [[ ! -f "$src/run" ]]; then
        echo "Error: $src has no run script (not a service dir?)" >&2
        exit 1
    fi
    dest="$PACKAGE_DIR/etc/sv/$name"
    echo "  -> Copying service dir $name -> etc/sv/$name/"
    mkdir -p "$dest"
    cp -a "$src/." "$dest/"
    if [[ ! -f "$dest/log/run" ]]; then
        echo "     (generating log/run)"
        mkdir -p "$dest/log"
        printf '#!/bin/sh\nexec svlogd -tt /var/log/%s\n' "$name" > "$dest/log/run"
    fi
    # runit-executed files (run, log/run, finish, check, control/*) are 0755;
    # anything else (e.g. `down`) is 755 only if the source has the x-bit.
    while IFS= read -r -d '' rel; do
        if service_file_is_exec "$rel"; then
            chmod 755 "$dest/$rel"
        elif [[ -f "$src/$rel" ]]; then
            chmod "$(data_file_mode "$src/$rel")" "$dest/$rel"
        fi
    done < <(cd "$dest" && find . -type f -printf '%P\0')
done < <(srcpkg_service_dirs "$SRCPKG")

# ---------------------------------------------------------------------------
# INSTALL / REMOVE (verbatim)
# ---------------------------------------------------------------------------
for hook in INSTALL REMOVE; do
    if [[ -f "$SRCPKG/$hook" ]]; then
        echo "  -> Copying $hook..."
        cp "$SRCPKG/$hook" "$PACKAGE_DIR/$hook"
        chmod 755 "$PACKAGE_DIR/$hook"
    fi
done

# ---------------------------------------------------------------------------
# Version from Cargo.toml
# ---------------------------------------------------------------------------
CARGO_TOML="${CARGO_TOML:-$(cargo_toml_for "$SVC" || true)}"
if [[ -z "$CARGO_TOML" || ! -f "$CARGO_TOML" ]]; then
    echo "Error: Cargo.toml for ${SVC} not found (got '${CARGO_TOML}'); pass --cargo-toml" >&2
    exit 1
fi

VERSION="$(cargo_version "$CARGO_TOML")"
echo "  Version from ${CARGO_TOML}: ${VERSION}"
if [[ -z "$VERSION" ]]; then
    echo "Error: Could not extract version from $CARGO_TOML" >&2
    exit 1
fi
if [[ "$TPL_VERSION" != "$VERSION" ]]; then
    echo "Warning: template version ($TPL_VERSION) differs from Cargo.toml ($VERSION); using Cargo.toml" >&2
fi

REVISION="$TPL_REVISION"
FULLVERSION="${VERSION}_${REVISION}"
echo "  -> Package version: ${FULLVERSION}"

# ---------------------------------------------------------------------------
# xbps-create
# ---------------------------------------------------------------------------
mkdir -p "$PACKAGES_DIR"
PACKAGE_DIR_ABS=$(realpath "$PACKAGE_DIR")
PACKAGES_DIR_ABS=$(realpath "$PACKAGES_DIR")

# Drop packages of older versions so stale files never reach the repository.
rm -f "$PACKAGES_DIR"/"${SVC}"-[0-9]*.aarch64.xbps

echo "  -> Directory contents:"
ls -lR "$PACKAGE_DIR_ABS" | head -60

cd "$PACKAGES_DIR_ABS"

echo "  -> Running xbps-create..."

XBPS_ARGS=(
    -A aarch64
    -n "${SVC}-${FULLVERSION}"
    -s "$TPL_SHORT_DESC"
    -H "https://github.com/johlrogge/modular-digital-music-array"
    -l MIT
    -m "Joakim Ohlrogge <joakim.ohlrogge@agical.se>"
)

if [[ -n "$TPL_DEPENDS" ]]; then
    # One -D argument holding the whole space-separated list.
    XBPS_ARGS+=(-D "$TPL_DEPENDS")
fi

if [[ -n "$TPL_CONF_FILES" ]]; then
    XBPS_ARGS+=(--config-files "$TPL_CONF_FILES")
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
