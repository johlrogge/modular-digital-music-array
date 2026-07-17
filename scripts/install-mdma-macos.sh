#!/usr/bin/env bash
# install-mdma-macos.sh — download and install MDMA macOS binaries from CI artifacts
#
# Usage: install-mdma-macos.sh [tag]
#   tag  — Git tag to install, e.g. v0.25.0. Defaults to the most recent vN.N.N tag.
#
# Requirements: gh CLI, authenticated (gh auth login).
# Environment:
#   MDMA_INSTALL_DIR — override install directory (default: /usr/local/bin).
#                      If the directory is user-writable, sudo is skipped.

set -euo pipefail

REPO="johlrogge/modular-digital-music-array"
WORKFLOW_NAME="Build and Publish Packages"
INSTALL_DIR="${MDMA_INSTALL_DIR:-/usr/local/bin}"

# --------------------------------------------------------------------------
# Helpers
# --------------------------------------------------------------------------

die() {
    echo "error: $*" >&2
    exit 1
}

info() {
    echo "==> $*"
}

# --------------------------------------------------------------------------
# Cleanup
# --------------------------------------------------------------------------

TMPDIR_WORK=""

cleanup() {
    if [ -n "$TMPDIR_WORK" ] && [ -d "$TMPDIR_WORK" ]; then
        rm -rf "$TMPDIR_WORK"
    fi
}

trap cleanup EXIT

# --------------------------------------------------------------------------
# Check dependencies
# --------------------------------------------------------------------------

if ! command -v gh >/dev/null 2>&1; then
    die "gh CLI not found. Install it from https://cli.github.com/"
fi

if ! gh auth status >/dev/null 2>&1; then
    die "gh CLI is not authenticated. Run: gh auth login"
fi

# --------------------------------------------------------------------------
# Resolve tag
# --------------------------------------------------------------------------

if [ "${1:-}" != "" ]; then
    TAG="$1"
    info "Using requested tag: $TAG"
else
    info "Resolving latest tag..."
    TAG="$(gh api "repos/${REPO}/tags" \
        --jq '[.[] | select(.name | test("^v[0-9]"))] | .[0].name')"
    if [ -z "$TAG" ]; then
        die "Could not resolve any tag matching v[0-9]* from ${REPO}"
    fi
    info "Latest tag: $TAG"
fi

# --------------------------------------------------------------------------
# Resolve tag -> commit SHA
# --------------------------------------------------------------------------

info "Resolving commit SHA for $TAG..."
SHA="$(gh api "repos/${REPO}/git/ref/tags/${TAG}" \
    --jq '.object.sha' 2>/dev/null || true)"

# Annotated tags point at a tag object; peel to commit
if [ -z "$SHA" ]; then
    die "Tag ${TAG} not found in ${REPO}"
fi

# If the object type is "tag" (annotated), follow it to the commit
OBJ_TYPE="$(gh api "repos/${REPO}/git/ref/tags/${TAG}" --jq '.object.type')"
if [ "$OBJ_TYPE" = "tag" ]; then
    SHA="$(gh api "repos/${REPO}/git/tags/${SHA}" --jq '.object.sha')"
fi

if [ -z "$SHA" ]; then
    die "Could not resolve commit SHA for tag ${TAG}"
fi

info "Commit SHA: ${SHA}"

# --------------------------------------------------------------------------
# Find latest successful workflow run for this commit
# --------------------------------------------------------------------------

info "Looking for successful '${WORKFLOW_NAME}' run at ${SHA}..."
RUN_ID="$(gh run list \
    --repo "${REPO}" \
    --commit "${SHA}" \
    --workflow "${WORKFLOW_NAME}" \
    --status success \
    --json databaseId \
    --jq '.[0].databaseId')"

if [ -z "$RUN_ID" ] || [ "$RUN_ID" = "null" ]; then
    echo "error: No successful '${WORKFLOW_NAME}' run found for ${TAG} (commit ${SHA})." >&2
    echo "" >&2
    echo "Artifacts expire after ~90 days. To rebuild, trigger a new run at:" >&2
    echo "  https://github.com/${REPO}/actions/workflows/build-and-package.yml" >&2
    echo "(use 'Run workflow' button on the workflow page)" >&2
    exit 1
fi

info "Found run ID: ${RUN_ID}"

# --------------------------------------------------------------------------
# Download artifacts
# --------------------------------------------------------------------------

TMPDIR_WORK="$(mktemp -d)"
CLI_DIR="${TMPDIR_WORK}/cli"
TUI_DIR="${TMPDIR_WORK}/tui"
mkdir -p "$CLI_DIR" "$TUI_DIR"

info "Downloading mdma-cli-macos-arm64..."
gh run download "${RUN_ID}" \
    -n mdma-cli-macos-arm64 \
    -R "${REPO}" \
    -D "${CLI_DIR}"

info "Downloading mdma-tui-macos-arm64..."
gh run download "${RUN_ID}" \
    -n mdma-tui-macos-arm64 \
    -R "${REPO}" \
    -D "${TUI_DIR}"

# --------------------------------------------------------------------------
# Strip quarantine and make executable
# --------------------------------------------------------------------------

CLI_BIN="${CLI_DIR}/mdma"
TUI_BIN="${TUI_DIR}/mdma-tui"

for BIN in "$CLI_BIN" "$TUI_BIN"; do
    if [ ! -f "$BIN" ]; then
        die "Expected binary not found after download: ${BIN}"
    fi
    xattr -d com.apple.quarantine "$BIN" 2>/dev/null || true
    chmod +x "$BIN"
done

# --------------------------------------------------------------------------
# Install
# --------------------------------------------------------------------------

if [ ! -d "$INSTALL_DIR" ]; then
    echo "Install directory does not exist: ${INSTALL_DIR}" >&2
    echo "Create it or set MDMA_INSTALL_DIR to an existing directory." >&2
    exit 1
fi

if [ -w "$INSTALL_DIR" ]; then
    USE_SUDO="no"
else
    USE_SUDO="yes"
fi

echo ""
echo "Installing to ${INSTALL_DIR}:"
echo "  mdma        (CLI — library control, search, playlists, export)"
echo "  mdma-tui    (TUI — terminal user interface)"
echo ""

if [ "$USE_SUDO" = "yes" ]; then
    echo "(sudo will be used because ${INSTALL_DIR} is not user-writable)"
    echo ""
    sudo install -m 755 "$CLI_BIN" "${INSTALL_DIR}/mdma"
    sudo install -m 755 "$TUI_BIN" "${INSTALL_DIR}/mdma-tui"
else
    install -m 755 "$CLI_BIN" "${INSTALL_DIR}/mdma"
    install -m 755 "$TUI_BIN" "${INSTALL_DIR}/mdma-tui"
fi

# --------------------------------------------------------------------------
# Verify installed versions
# --------------------------------------------------------------------------

CLI_VERSION="$("${INSTALL_DIR}/mdma" --version 2>/dev/null || echo "${TAG}")"

echo ""
echo "Installed:"
echo "  ${INSTALL_DIR}/mdma      ${CLI_VERSION}"
echo "  ${INSTALL_DIR}/mdma-tui  ${TAG}"
echo ""
echo "Done. Set MDMA_NODE to your Pi hostname to start using it:"
echo "  export MDMA_NODE=mdma-yourname.local"
echo "  mdma ping"
