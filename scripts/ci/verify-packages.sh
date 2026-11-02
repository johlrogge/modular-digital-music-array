#!/usr/bin/env bash
# Verify that built .xbps packages carry everything void-packages/srcpkgs/<pkg>/
# declares (the single source of truth), and that the declarations themselves
# are sane.
#
# Usage:
#   verify-packages.sh --static        no build needed: checks srcpkg declarations,
#                                      template version == Cargo.toml, run scripts
#   verify-packages.sh [--require-all] also checks every build/packages/*.aarch64.xbps
#                                      (--require-all: every srcpkg must have a package)
#
# Package checks (per .xbps, listed/extracted with tar):
#   - every files/<d>/** is present under etc/sv/<d>/ with identical content,
#     run, log/run (and finish/check) are 0755
#   - INSTALL and REMOVE are byte-identical to the srcpkg files, or both absent
#   - run_depends == normalised template depends
#   - conf_files == template conf_files
#   - version == template version == Cargo.toml version, props pkgver matches
#   - every files/rootfs/** file is present
#   - revision == template revision, short_desc == template short_desc
#   - usr/bin/<pkgname> is an executable aarch64 ELF (every binary is named like its package)
#   - run, log/run, finish, check, control/* are 0755; other files (and rootfs
#     files) are 0755 iff the source has the x-bit, else 0644
#   - the same checks run on build/repository/aarch64/ (the published artifact)
#   - nothing else is in the package: every file is the binary (usr/bin/<pkgname>),
#     xbps metadata (props.plist, files.plist), INSTALL/REMOVE, files/<d>/**
#     under etc/sv/<d>/, a generated log/run, or files/rootfs/** with identical content
#
# Needs: bash, tar (with zstd/xz support for the package), awk, sed, cmp, stat, sh.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"
# shellcheck source=scripts/package/lib-srcpkg.sh
source "$REPO_ROOT/scripts/package/lib-srcpkg.sh"

STATIC_ONLY=0
REQUIRE_ALL=0
for arg in "$@"; do
    case "$arg" in
        --static)      STATIC_ONLY=1 ;;
        --require-all) REQUIRE_ALL=1 ;;
        -h|--help)     sed -n '2,/^$/p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "Unknown argument: $arg" >&2; exit 2 ;;
    esac
done

SRCPKGS_DIR="void-packages/srcpkgs"
PACKAGES_DIR="build/packages"
REPOSITORY_DIR="build/repository/aarch64"
FAILURES=0

fail() { echo "  FAIL: $*" >&2; FAILURES=$((FAILURES + 1)); }
note() { echo "  $*"; }

# Names allowed inside a runit service directory (files/<d>/).
SERVICE_ENTRIES=" run down finish check log control "

# ---------------------------------------------------------------------------
# plist helpers (props.plist written by xbps-create)
# ---------------------------------------------------------------------------
unescape_xml() { sed 's/&gt;/>/g; s/&lt;/</g; s/&amp;/\&/g'; }

plist_string() {
    awk -v key="$1" '
        !f && index($0, "<key>" key "</key>") { f = 1; r = $0; sub(/.*<\/key>/, "", r); $0 = r }
        f && /<string>/ { s = $0; sub(/.*<string>/, "", s); sub(/<\/string>.*/, "", s); print s; exit }
    ' | unescape_xml
}

plist_array() {
    awk -v key="$1" '
        !f && index($0, "<key>" key "</key>") { f = 1; next }
        f && /<array\/>/ { exit }
        f && /<\/array>/ { exit }
        f && /<string>/ { s = $0; sub(/.*<string>/, "", s); sub(/<\/string>.*/, "", s); print s }
    ' | unescape_xml
}

sorted_words() {
    local - w
    set -f
    for w in $1; do echo "$w"; done | sort
}

# ---------------------------------------------------------------------------
# Static checks for one srcpkg dir
# ---------------------------------------------------------------------------
static_check() {
    local dir="$1" name entry d cargo_toml cargo_ver hook re
    name="$(basename "$dir")"
    echo "[$name] srcpkg"

    if ! srcpkg_load "$dir"; then
        fail "cannot read template"
        return
    fi

    [[ "$TPL_PKGNAME" = "$name" ]] || fail "template pkgname '$TPL_PKGNAME' != directory name '$name'"
    [[ -n "$TPL_SHORT_DESC" ]] || fail "template short_desc is empty"
    [[ -n "$TPL_VERSION" ]] || fail "template version is empty"

    [[ "$TPL_REVISION" =~ ^[0-9]+$ ]] || fail "template revision '$TPL_REVISION' is not a number"

    # depends: every normalised entry must be name<op>version. Exact pkgvers
    # (foo-1.0_1) would become a malformed 'foo-1.0_1>=0', so reject them.
    re='^[A-Za-z0-9._+-]+(>=|<=|>|<|=)[A-Za-z0-9._+:~-]+$'
    local pkgver_re='^.+-[0-9][A-Za-z0-9.+~]*_[0-9]+$'
    (
        set -f
        for d in $TPL_DEPENDS_RAW; do
            if [[ "$d" != *[\<\>=]* && "$d" =~ $pkgver_re ]]; then
                echo "  FAIL: dependency '$d' looks like an exact pkgver; use 'name' or 'name>=ver'" >&2
                exit 1
            fi
        done
    ) || FAILURES=$((FAILURES + 1))
    for d in $TPL_DEPENDS; do
        [[ "$d" =~ $re ]] || fail "malformed dependency '$d' in depends"
    done

    # conf_files: absolute, and shipped through the rootfs overlay
    for d in $TPL_CONF_FILES; do
        if [[ "$d" != /* ]]; then
            fail "conf_files entry '$d' is not an absolute path"
        elif [[ ! -f "$dir/files/rootfs/${d#/}" ]]; then
            fail "conf_files entry '$d' has no files/rootfs/${d#/}"
        fi
    done

    # rootfs programs must carry the x-bit in git (modes follow the source file)
    if [[ -d "$dir/files/rootfs" ]]; then
        while IFS= read -r -d '' d; do
            [[ -x "$dir/files/rootfs/$d" ]] || fail "files/rootfs/$d must be executable (chmod +x; git update-index --chmod=+x)"
        done < <(cd "$dir/files/rootfs" && find usr/libexec usr/bin usr/sbin usr/lib/mdma -type f -printf '%p\0' 2>/dev/null)
    fi

    # files/: service dirs (with run) and the rootfs overlay, nothing else
    if [[ -d "$dir/files" ]]; then
        for entry in "$dir"/files/*; do
            [[ -e "$entry" ]] || continue
            if [[ -f "$entry" ]]; then
                fail "stray file files/$(basename "$entry"): put it under files/rootfs/ or a service dir"
            fi
        done
    fi
    [[ -f "$dir/files/$name/run" ]] || fail "files/$name/run missing (every package ships its service)"
    while read -r d; do
        [[ -n "$d" ]] || continue
        if [[ ! -f "$dir/files/$d/run" ]]; then
            fail "files/$d has no run script (service dirs need one; rootfs files go in files/rootfs/)"
            continue
        fi
        head -1 "$dir/files/$d/run" | grep -q '^#!' || fail "files/$d/run has no shebang"
        if [[ -f "$dir/files/$d/log/run" ]]; then
            head -1 "$dir/files/$d/log/run" | grep -q '^#!' || fail "files/$d/log/run has no shebang"
        fi
        for entry in "$dir/files/$d"/*; do
            [[ -e "$entry" ]] || continue
            if [[ "$SERVICE_ENTRIES" != *" $(basename "$entry") "* ]]; then
                fail "files/$d/$(basename "$entry") is not a runit service file (move it to files/rootfs/)"
            fi
        done
    done < <(srcpkg_service_dirs "$dir")

    # INSTALL / REMOVE: shell scripts that parse
    for hook in INSTALL REMOVE; do
        if [[ -f "$dir/$hook" ]]; then
            head -1 "$dir/$hook" | grep -q '^#!/bin/sh' || fail "$hook must start with #!/bin/sh"
            sh -n "$dir/$hook" 2>/dev/null || fail "$hook has a shell syntax error"
        fi
    done

    # version: template == Cargo.toml
    if cargo_toml="$(cargo_toml_for "$name")"; then
        cargo_ver="$(cargo_version "$cargo_toml")"
        if [[ "$TPL_VERSION" != "$cargo_ver" ]]; then
            fail "template version '$TPL_VERSION' != $cargo_toml version '$cargo_ver'"
        fi
    else
        fail "no Cargo.toml found for $name (projects/$name or projects/mdma-$name)"
    fi
}

# ---------------------------------------------------------------------------
# Package checks for one .xbps
# ---------------------------------------------------------------------------
package_check() {
    local pkg="$1" base rev nv ver name dir x d rel src exp act mode hook cargo_toml cargo_ver pkgver
    base="$(basename "${pkg%.aarch64.xbps}")"
    rev="${base##*_}"
    nv="${base%_*}"
    ver="${nv##*-}"
    name="${nv%-*}"
    dir="$SRCPKGS_DIR/$name"
    echo "[$name] $(basename "$pkg")"

    if [[ ! -d "$dir" ]]; then
        fail "no srcpkg for built package '$name' ($dir)"
        return
    fi
    srcpkg_load "$dir" || { fail "cannot read template"; return; }

    x="$WORK/$name"
    rm -rf "$x"
    mkdir -p "$x"
    if ! tar -xpf "$pkg" -C "$x"; then
        fail "cannot unpack $pkg with tar (missing zstd/xz support?)"
        return
    fi
    [[ -f "$x/props.plist" ]] || { fail "props.plist missing in package"; return; }

    # versions
    cargo_toml="$(cargo_toml_for "$name" || true)"
    cargo_ver=""
    [[ -n "$cargo_toml" ]] && cargo_ver="$(cargo_version "$cargo_toml")"
    [[ "$ver" = "$TPL_VERSION" ]] || fail "package version '$ver' != template version '$TPL_VERSION'"
    [[ "$ver" = "$cargo_ver" ]] || fail "package version '$ver' != Cargo.toml version '$cargo_ver'"
    pkgver="$(plist_string pkgver <"$x/props.plist")"
    [[ "$pkgver" = "${nv}_${rev}" ]] || fail "props pkgver '$pkgver' != '${nv}_${rev}'"
    [[ "$rev" = "$TPL_REVISION" ]] || fail "package revision '$rev' != template revision '$TPL_REVISION'"

    # short_desc
    act="$(plist_string short_desc <"$x/props.plist")"
    [[ "$act" = "$TPL_SHORT_DESC" ]] || fail "short_desc '$act' != template '$TPL_SHORT_DESC'"

    # binary: exactly usr/bin/<pkgname>, an aarch64 ELF
    if [[ ! -f "$x/usr/bin/$name" ]]; then
        fail "usr/bin/$name missing from package"
    else
        [[ -x "$x/usr/bin/$name" ]] || fail "usr/bin/$name is not executable"
        if command -v file >/dev/null 2>&1; then
            file -b "$x/usr/bin/$name" | grep -q 'ELF.*ARM aarch64' ||
                fail "usr/bin/$name is not an aarch64 ELF: $(file -b "$x/usr/bin/$name")"
        else
            [[ "$(head -c 4 "$x/usr/bin/$name" | od -An -c | tr -d ' ')" = '177ELF' ]] ||
                fail "usr/bin/$name is not an ELF binary"
        fi
    fi

    # run_depends / conf_files
    exp="$(sorted_words "$TPL_DEPENDS")"
    act="$(plist_array run_depends <"$x/props.plist" | sort)"
    [[ "$exp" = "$act" ]] || fail "run_depends mismatch: expected [$(echo $exp)] got [$(echo $act)]"
    exp="$(sorted_words "$TPL_CONF_FILES")"
    act="$(plist_array conf_files <"$x/props.plist" | sort)"
    [[ "$exp" = "$act" ]] || fail "conf_files mismatch: expected [$(echo $exp)] got [$(echo $act)]"

    # service dirs
    while read -r d; do
        [[ -n "$d" ]] || continue
        if [[ -d "$dir/files/$d" ]]; then
            while IFS= read -r -d '' rel; do
                if [[ ! -e "$x/etc/sv/$d/$rel" ]]; then
                    fail "etc/sv/$d/$rel missing from package"
                elif [[ -f "$dir/files/$d/$rel" ]] && ! cmp -s "$dir/files/$d/$rel" "$x/etc/sv/$d/$rel"; then
                    fail "etc/sv/$d/$rel differs from srcpkg"
                fi
            done < <(cd "$dir/files/$d" && find . \( -type f -o -type l \) -printf '%P\0')
        fi
        for rel in run log/run; do
            [[ -f "$x/etc/sv/$d/$rel" ]] || fail "etc/sv/$d/$rel missing from package"
        done
        # modes: runit-executed files 0755, others 755 iff the source has the x-bit
        while IFS= read -r -d '' rel; do
            mode="$(stat -c %a "$x/etc/sv/$d/$rel")"
            if service_file_is_exec "$rel"; then
                exp=755
            elif [[ -f "$dir/files/$d/$rel" ]]; then
                exp="$(data_file_mode "$dir/files/$d/$rel")"
            else
                continue
            fi
            [[ "$mode" = "$exp" ]] || fail "etc/sv/$d/$rel mode is $mode, expected $exp"
        done < <(cd "$x/etc/sv/$d" 2>/dev/null && find . -type f -printf '%P\0')
        if [[ ! -f "$dir/files/$d/log/run" && -f "$x/etc/sv/$d/log/run" ]]; then
            grep -qx "exec svlogd -tt /var/log/$d" "$x/etc/sv/$d/log/run" ||
                fail "generated etc/sv/$d/log/run has unexpected content"
        fi
    done < <(srcpkg_service_dirs "$dir")

    # INSTALL / REMOVE
    for hook in INSTALL REMOVE; do
        if [[ -f "$dir/$hook" ]]; then
            if [[ ! -f "$x/$hook" ]]; then
                fail "$hook missing from package"
            elif ! cmp -s "$dir/$hook" "$x/$hook"; then
                fail "$hook differs from $dir/$hook"
            fi
        elif [[ -e "$x/$hook" ]]; then
            fail "package has a $hook but the srcpkg does not"
        fi
    done

    # Every file in the package must be accounted for by the srcpkg
    local -A known=()
    local f
    known[props.plist]=1
    known[files.plist]=1
    for hook in INSTALL REMOVE; do
        [[ -f "$dir/$hook" ]] && known[$hook]=1
    done
    while read -r d; do
        [[ -n "$d" ]] || continue
        known["etc/sv/$d/log/run"]=1
        while IFS= read -r -d '' rel; do
            known["etc/sv/$d/$rel"]=1
        done < <(cd "$dir/files/$d" && find . \( -type f -o -type l \) -printf '%P\0')
    done < <(srcpkg_service_dirs "$dir")
    if [[ -d "$dir/files/rootfs" ]]; then
        while IFS= read -r -d '' rel; do
            known["$rel"]=1
        done < <(cd "$dir/files/rootfs" && find . \( -type f -o -type l \) -printf '%P\0')
    fi
    while IFS= read -r -d '' f; do
        [[ "$f" = "usr/bin/$name" ]] && continue   # the binary
        [[ -n "${known[$f]:-}" ]] || fail "unexpected file in package, not declared by the srcpkg: $f"
    done < <(cd "$x" && find . \( -type f -o -type l \) -printf '%P\0')

    # rootfs overlay
    if [[ -d "$dir/files/rootfs" ]]; then
        while IFS= read -r -d '' rel; do
            if [[ ! -f "$x/$rel" ]]; then
                fail "rootfs file $rel missing from package"
                continue
            fi
            cmp -s "$dir/files/rootfs/$rel" "$x/$rel" || fail "rootfs file $rel differs from srcpkg"
            mode="$(stat -c %a "$x/$rel")"
            exp="$(data_file_mode "$dir/files/rootfs/$rel")"
            [[ "$mode" = "$exp" ]] || fail "$rel mode is $mode, expected $exp"
        done < <(cd "$dir/files/rootfs" && find . -type f -printf '%P\0')
    fi
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
shopt -s nullglob

SRCPKG_DIRS=("$SRCPKGS_DIR"/*/)
if (( ${#SRCPKG_DIRS[@]} == 0 )); then
    echo "Error: no srcpkgs found in $SRCPKGS_DIR" >&2
    exit 1
fi

for d in "${SRCPKG_DIRS[@]}"; do
    static_check "${d%/}"
done

# Verify the freshly built packages and, when present, the indexed repository
# copies (the published artifact).
verify_dir() {
    local pdir="$1" pkg nv pname
    local -A seen=()
    local -a pkgs
    echo "=== $pdir ==="
    pkgs=("$pdir"/*.aarch64.xbps)
    if (( ${#pkgs[@]} == 0 )); then
        fail "no packages in $pdir (run: just pkg-build-all)"
        return
    fi
    for pkg in "${pkgs[@]}"; do
        nv="$(basename "${pkg%.aarch64.xbps}")"
        nv="${nv%_*}"
        pname="${nv%-*}"
        if [[ -n "${seen[$pname]:-}" ]]; then
            echo "[$pname] $(basename "$pkg")"
            fail "second package for $pname (also ${seen[$pname]}); run: just pkg-clean"
            continue
        fi
        seen[$pname]="$(basename "$pkg")"
        package_check "$pkg"
    done
    if (( REQUIRE_ALL == 1 )); then
        for d in "${SRCPKG_DIRS[@]}"; do
            pname="$(basename "${d%/}")"
            [[ -n "${seen[$pname]:-}" ]] || { echo "[$pname]"; fail "no package for srcpkg $pname in $pdir"; }
        done
    fi
}

if (( STATIC_ONLY == 0 )); then
    WORK="$(mktemp -d)"
    trap 'rm -rf "$WORK"' EXIT
    verify_dir "$PACKAGES_DIR"
    if [[ -d "$REPOSITORY_DIR" ]]; then
        verify_dir "$REPOSITORY_DIR"
    elif (( REQUIRE_ALL == 1 )); then
        fail "$REPOSITORY_DIR missing (run: just pkg-build-all)"
    else
        note "skipping $REPOSITORY_DIR (not built)"
    fi
fi

echo ""
if (( FAILURES > 0 )); then
    echo "verify-packages: $FAILURES problem(s) found" >&2
    exit 1
fi
if (( STATIC_ONLY == 1 )); then
    echo "verify-packages: srcpkg declarations OK"
else
    echo "verify-packages: all packages OK"
fi
