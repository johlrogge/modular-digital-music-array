#!/usr/bin/env bash
# Shared helpers for reading void-packages/srcpkgs/<pkg>/ (the single source of
# truth for package metadata, service files, INSTALL/REMOVE and the rootfs
# overlay). Sourced by create-service-package.sh and scripts/ci/verify-packages.sh.
#
# Sourced only (never executed), so it needs no exec bit.

LIB_REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# normalize_words "  a   b " -> "a b" (whitespace-normalise a template list)
normalize_words() {
    local - w out=()
    set -f
    for w in $1; do out+=("$w"); done
    echo "${out[*]-}"
}

# normalize_deps "a b>=1 c" -> "a>=0 b>=1 c>=0"
# xbps-create -D wants a version constraint on every dependency.
normalize_deps() {
    local - dep out=()
    set -f
    for dep in $1; do
        case "$dep" in
            *[\<\>=]*) out+=("$dep") ;;
            *) out+=("${dep}>=0") ;;
        esac
    done
    echo "${out[*]-}"
}

# srcpkg_load DIR
# Sources DIR/template in a subshell (so nothing leaks into the caller) and sets:
#   TPL_PKGNAME TPL_VERSION TPL_REVISION (default 1) TPL_SHORT_DESC
#   TPL_DEPENDS_RAW TPL_DEPENDS (normalised) TPL_CONF_FILES (whitespace-normalised)
srcpkg_load() {
    local dir="$1" tpl out
    tpl="$dir/template"
    if [[ ! -f "$tpl" ]]; then
        echo "Error: $tpl not found" >&2
        return 1
    fi
    out="$(
        # shellcheck disable=SC1090
        . "$tpl" || exit 1
        printf 'TPL_PKGNAME=%q\n' "${pkgname-}"
        printf 'TPL_VERSION=%q\n' "${version-}"
        printf 'TPL_REVISION=%q\n' "${revision-1}"
        printf 'TPL_SHORT_DESC=%q\n' "${short_desc-}"
        printf 'TPL_DEPENDS_RAW=%q\n' "${depends-}"
        printf 'TPL_CONF_FILES=%q\n' "${conf_files-}"
    )" || {
        echo "Error: failed to source $tpl" >&2
        return 1
    }
    eval "$out"
    TPL_DEPENDS="$(normalize_deps "$TPL_DEPENDS_RAW")"
    TPL_CONF_FILES="$(normalize_words "$TPL_CONF_FILES")"
}

# Files in a service dir that runit executes: always 0755.
# service_file_is_exec REL (path relative to the service dir)
service_file_is_exec() {
    case "$1" in
        run|log/run|finish|check|control/*) return 0 ;;
        *) return 1 ;;
    esac
}

# data_file_mode SRC -> 755 if the source file has the user x-bit, else 644
data_file_mode() {
    if [[ -x "$1" ]]; then echo 755; else echo 644; fi
}

# cargo_toml_for PKGNAME -> path of the crate manifest that owns the version.
cargo_toml_for() {
    local name="$1" cand
    for cand in "projects/${name}/Cargo.toml" "projects/mdma-${name}/Cargo.toml"; do
        if [[ -f "$LIB_REPO_ROOT/$cand" ]]; then
            echo "$cand"
            return 0
        fi
    done
    return 1
}

# cargo_version CARGO_TOML (path relative to the repo root, or absolute)
# Prints the package version, following `version.workspace = true`.
cargo_version() {
    local toml="$1" raw
    [[ "$toml" = /* ]] || toml="$LIB_REPO_ROOT/$toml"
    [[ -f "$toml" ]] || return 1
    raw="$(grep '^version[. =]' "$toml" | head -1)"
    if echo "$raw" | grep -q 'workspace = true'; then
        awk '/^\[workspace\.package\]/{flag=1; next} /^\[/{flag=0} flag && /^version/{gsub(/[" ]/,""); split($0,a,"="); print a[2]; exit}' "$LIB_REPO_ROOT/Cargo.toml"
    else
        echo "$raw" | sed 's/version *= *"\(.*\)"/\1/'
    fi
}

# srcpkg_service_dirs DIR -> names of files/<d> directories that are runit
# service directories (everything under files/ except the rootfs overlay).
srcpkg_service_dirs() {
    local d
    for d in "$1"/files/*/; do
        [[ -d "$d" ]] || continue
        d="${d%/}"
        [[ "$(basename "$d")" = rootfs ]] && continue
        basename "$d"
    done
}
