#!/usr/bin/env bash
# install-mdma.sh — install the MDMA CLI (mdma) and TUI (mdma-tui) from a GitHub release.
#
# Supports macOS arm64 and Linux x86_64 (OS detected automatically).
# Needs only curl, tar and shasum (macOS) or sha256sum (Linux). No gh CLI, no login.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/johlrogge/modular-digital-music-array/master/scripts/install-mdma.sh | bash
#   curl -fsSL .../install-mdma.sh | bash -s v0.39.0
#   bash install-mdma.sh [vX.Y.Z]
#
#   No argument installs the latest release (resolved once to a concrete tag);
#   vX.Y.Z installs that release.
#
# Environment:
#   MDMA_INSTALL_DIR — install directory (default: /usr/local/bin).
#                      sudo is used only if the directory is not user-writable.
#
# COMPATIBILITY CONTRACT (keep in sync with .github/workflows/build-and-package.yml
# and RELEASING.md). This script is always fetched from master, so the master
# version must be able to install EVERY published tag. A release must therefore:
#   - attach assets named mdma-<os>-<arch>.tar.gz
#     (mdma-macos-arm64.tar.gz, mdma-linux-x86_64.tar.gz) plus SHA256SUMS
#     ("<sha256>  <asset name>" lines);
#   - put the binaries `mdma` and `mdma-tui` at the tarball root.
# Changing any of that means teaching this script both the old and new layout.
#
# The whole body is inside main() and only invoked on the last line, so a
# truncated `curl | bash` download cannot run a partial script.

set -euo pipefail

REPO="johlrogge/modular-digital-music-array"

die() {
    echo "error: $*" >&2
    exit 1
}

info() {
    echo "==> $*"
}

TMPDIR_WORK=""
cleanup() {
    if [ -n "$TMPDIR_WORK" ] && [ -d "$TMPDIR_WORK" ]; then
        rm -rf "$TMPDIR_WORK"
    fi
}

fetch() {
    # fetch <url> <output file>
    curl --proto '=https' --tlsv1.2 -fsSL -o "$2" "$1"
}

print_next_steps() {
    # print_next_steps <install dir> <uname -s> <login shell path, may be empty>
    # Prints instructions only; never writes rc or completion files.
    local INSTALL_DIR="$1" OS="$2" SHELL_NAME="${3##*/}"
    local CMD="mdma"

    echo "Done."
    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*) ;;
        *)
            CMD="${INSTALL_DIR}/mdma"
            echo ""
            echo "${INSTALL_DIR} is not on your PATH. Add it, or run ${CMD} by full path."
            ;;
    esac

    echo ""
    echo "Next steps:"
    echo "  ${CMD} node select    # find MDMA nodes on the network and remember one"
    echo "  ${CMD} ping"
    echo ""
    echo "'node select' asks you to pick when several nodes are found;"
    echo "'${CMD} node select <name>' picks one directly."
    echo ""
    echo "Shell completions (optional):"
    echo ""
    case "$SHELL_NAME" in
        bash)
            if [ "$OS" = "Darwin" ]; then
                echo "  (on macOS this needs bash-completion@2, e.g. brew install bash-completion@2)"
            fi
            echo "  mkdir -p ~/.local/share/bash-completion/completions"
            echo "  ${CMD} generate-completions bash > ~/.local/share/bash-completion/completions/mdma"
            ;;
        zsh)
            echo "  mkdir -p ~/.zfunc"
            echo "  ${CMD} generate-completions zsh > ~/.zfunc/_mdma"
            echo ""
            echo "  Then add these two lines to ~/.zshrc if not already present"
            echo "  (before any existing compinit call):"
            echo ""
            echo "  fpath=(~/.zfunc \$fpath)"
            echo "  autoload -Uz compinit && compinit"
            ;;
        fish)
            echo "  mkdir -p ~/.config/fish/completions"
            echo "  ${CMD} generate-completions fish > ~/.config/fish/completions/mdma.fish"
            ;;
        *)
            echo "  Run '${CMD} generate-completions <bash|zsh|fish>' for your shell's completion script."
            ;;
    esac
    echo ""
    echo "Restart your shell afterwards."
}

main() {
    local INSTALL_DIR="${MDMA_INSTALL_DIR:-/usr/local/bin}"
    local OS ARCH ASSET TAG BASE_URL

    trap cleanup EXIT

    # ----------------------------------------------------------------------
    # Platform and tools
    # ----------------------------------------------------------------------

    OS="$(uname -s)"
    ARCH="$(uname -m)"
    case "${OS}/${ARCH}" in
        Darwin/arm64)
            ASSET="mdma-macos-arm64.tar.gz"
            ;;
        Linux/x86_64)
            ASSET="mdma-linux-x86_64.tar.gz"
            ;;
        *)
            die "no prebuilt binaries for ${OS}/${ARCH} (available: macOS arm64, Linux x86_64). Build from source instead."
            ;;
    esac

    command -v curl >/dev/null 2>&1 || die "curl not found"
    command -v tar >/dev/null 2>&1 || die "tar not found"

    local SHA_CMD
    if command -v shasum >/dev/null 2>&1; then
        SHA_CMD="shasum -a 256"
    elif command -v sha256sum >/dev/null 2>&1; then
        SHA_CMD="sha256sum"
    else
        die "neither shasum nor sha256sum found"
    fi

    # ----------------------------------------------------------------------
    # Resolve the release tag (a concrete vX.Y.Z, also for "latest")
    # ----------------------------------------------------------------------

    if [ "${1:-}" != "" ]; then
        TAG="$1"
    else
        info "Resolving the latest release..."
        local FINAL_URL
        FINAL_URL="$(curl --proto '=https' --tlsv1.2 -fsSLI -o /dev/null -w '%{url_effective}' \
            "https://github.com/${REPO}/releases/latest")" \
            || die "could not resolve the latest release of ${REPO}"
        TAG="${FINAL_URL##*/}"
    fi
    case "$TAG" in
        v[0-9]*) ;;
        *) die "expected a release tag like v0.39.0, got: ${TAG}" ;;
    esac

    BASE_URL="https://github.com/${REPO}/releases/download/${TAG}"
    info "Installing ${TAG} (${ASSET})"

    # ----------------------------------------------------------------------
    # Download and verify
    # ----------------------------------------------------------------------

    TMPDIR_WORK="$(mktemp -d)"

    info "Downloading SHA256SUMS..."
    fetch "${BASE_URL}/SHA256SUMS" "${TMPDIR_WORK}/SHA256SUMS" \
        || die "could not download ${BASE_URL}/SHA256SUMS (does release ${TAG} exist?)"

    info "Downloading ${ASSET}..."
    fetch "${BASE_URL}/${ASSET}" "${TMPDIR_WORK}/${ASSET}" \
        || die "could not download ${BASE_URL}/${ASSET}"

    local EXPECTED ACTUAL
    EXPECTED="$(awk -v f="$ASSET" '$2 == f || $2 == "*" f {print $1; exit}' "${TMPDIR_WORK}/SHA256SUMS")"
    [ -n "$EXPECTED" ] || die "${ASSET} is not listed in SHA256SUMS"

    # shellcheck disable=SC2086  # SHA_CMD is a fixed command with a flag
    ACTUAL="$($SHA_CMD "${TMPDIR_WORK}/${ASSET}" | awk '{print $1}')"
    if [ "$EXPECTED" != "$ACTUAL" ]; then
        echo "error: checksum mismatch for ${ASSET}" >&2
        echo "  expected: ${EXPECTED}" >&2
        echo "  actual:   ${ACTUAL}" >&2
        exit 1
    fi
    info "Checksum OK (${ACTUAL})"

    local EXTRACT_DIR="${TMPDIR_WORK}/extract"
    mkdir -p "$EXTRACT_DIR"
    tar -xzf "${TMPDIR_WORK}/${ASSET}" -C "$EXTRACT_DIR"

    local CLI_BIN="${EXTRACT_DIR}/mdma"
    local TUI_BIN="${EXTRACT_DIR}/mdma-tui"
    local BIN
    for BIN in "$CLI_BIN" "$TUI_BIN"; do
        [ -f "$BIN" ] || die "expected binary missing from ${ASSET}: $(basename "$BIN")"
        if [ "$OS" = "Darwin" ]; then
            # Files fetched with curl are not quarantined; this is a harmless safety step.
            xattr -d com.apple.quarantine "$BIN" 2>/dev/null || true
        fi
        chmod +x "$BIN"
    done

    # ----------------------------------------------------------------------
    # Install
    # ----------------------------------------------------------------------

    if [ ! -d "$INSTALL_DIR" ]; then
        echo "Install directory does not exist: ${INSTALL_DIR}" >&2
        echo "Create it or set MDMA_INSTALL_DIR to an existing directory." >&2
        exit 1
    fi

    echo ""
    echo "Installing to ${INSTALL_DIR}:"
    echo "  mdma        (CLI: library control, search, playlists, export)"
    echo "  mdma-tui    (TUI: terminal user interface)"
    echo ""

    if [ -w "$INSTALL_DIR" ]; then
        install -m 755 "$CLI_BIN" "${INSTALL_DIR}/mdma"
        install -m 755 "$TUI_BIN" "${INSTALL_DIR}/mdma-tui"
    else
        command -v sudo >/dev/null 2>&1 \
            || die "${INSTALL_DIR} is not writable and sudo is not available. Re-run as root or set MDMA_INSTALL_DIR to a writable directory, e.g. MDMA_INSTALL_DIR=\$HOME/.local/bin"
        echo "(sudo will be used because ${INSTALL_DIR} is not user-writable)"
        echo ""
        sudo install -m 755 "$CLI_BIN" "${INSTALL_DIR}/mdma"
        sudo install -m 755 "$TUI_BIN" "${INSTALL_DIR}/mdma-tui"
    fi

    # ----------------------------------------------------------------------
    # Verify installed versions
    # ----------------------------------------------------------------------

    local CLI_VERSION TUI_VERSION
    CLI_VERSION="$("${INSTALL_DIR}/mdma" --version 2>/dev/null </dev/null || echo "unknown")"
    # Newer CLIs print "Version: x.y.z"; keep just the version.
    CLI_VERSION="${CLI_VERSION#Version: }"
    TUI_VERSION="$("${INSTALL_DIR}/mdma-tui" --version 2>/dev/null </dev/null || echo "${TAG}")"
    TUI_VERSION="${TUI_VERSION#Version: }"

    echo ""
    echo "Installed ${TAG}:"
    echo "  ${INSTALL_DIR}/mdma      ${CLI_VERSION}"
    echo "  ${INSTALL_DIR}/mdma-tui  ${TUI_VERSION}"
    echo ""
    print_next_steps "$INSTALL_DIR" "$OS" "${SHELL:-}"
}

main "$@"
