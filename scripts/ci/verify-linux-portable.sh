#!/usr/bin/env bash
# verify-linux-portable.sh — fail if a Linux x86_64 binary is tied to /nix
# or needs a newer glibc than the release promises.
#
# Usage: verify-linux-portable.sh <binary>...
#   MAX_GLIBC (default 2.31) — highest GLIBC_x.y symbol version allowed.
#
# Checks per binary: ELF x86-64, interpreter is /lib64/ld-linux-x86-64.so.2,
# no RPATH/RUNPATH into /nix, no /nix library or unresolved library in ldd,
# highest GLIBC symbol version <= MAX_GLIBC, and a warning on /nix/store
# strings. Needs file, readelf, ldd, sort -V, grep (plain ubuntu).

set -euo pipefail

[ "$#" -gt 0 ] || { echo "usage: $0 <binary>..." >&2; exit 2; }

MAX_GLIBC="${MAX_GLIBC:-2.31}"

fail=0
for bin in "$@"; do
    echo "== $bin"
    file "$bin"

    if ! file "$bin" | grep -q 'ELF 64-bit.*x86-64'; then
        echo "::error::$bin is not an x86-64 ELF"; fail=1; continue
    fi

    interp="$(readelf -l "$bin" | sed -n 's/.*Requesting program interpreter: \(.*\)\]/\1/p')"
    echo "interpreter: ${interp:-<none>}"
    if [ "$interp" != "/lib64/ld-linux-x86-64.so.2" ]; then
        echo "::error::$bin interpreter is '${interp}', expected /lib64/ld-linux-x86-64.so.2"; fail=1
    fi

    dyn="$(readelf -d "$bin")"
    if grep -E 'RPATH|RUNPATH' <<<"$dyn" | grep -q /nix; then
        echo "::error::$bin has an RPATH/RUNPATH into /nix"; fail=1
    fi

    if grep -qa '/nix/store' "$bin"; then
        # Only a warning: build-time paths can be embedded in panic/debug
        # strings without affecting whether the binary runs. The interpreter,
        # RPATH, ldd and glibc checks are the ones that decide portability.
        echo "::warning::$bin contains /nix/store strings:"
        grep -aoE '/nix/store/[^"[:space:]]{0,80}' "$bin" | sort -u | head -5 || true
    fi

    # Run ldd once; grepping its output directly under pipefail can fail on SIGPIPE.
    ldd_out="$(ldd "$bin" 2>&1 || true)"
    echo "$ldd_out"
    if grep -q /nix <<<"$ldd_out"; then
        echo "::error::$bin links a library from /nix"; fail=1
    fi
    if grep -q 'not found' <<<"$ldd_out"; then
        echo "::error::$bin has an unresolved shared library"; fail=1
    fi

    # Highest GLIBC_x.y version the binary requires (dynamic symbol versions).
    versions="$(readelf -V "$bin" | grep -oE 'GLIBC_[0-9]+\.[0-9]+(\.[0-9]+)?' | sort -u || true)"
    highest="$(sed 's/^GLIBC_//' <<<"$versions" | sort -V | tail -n 1)"
    echo "highest GLIBC symbol version: ${highest:-<none>} (limit ${MAX_GLIBC})"
    if [ -n "$highest" ]; then
        top="$(printf '%s\n%s\n' "$highest" "$MAX_GLIBC" | sort -V | tail -n 1)"
        if [ "$top" != "$MAX_GLIBC" ]; then
            echo "::error::$bin needs GLIBC_${highest}, above the allowed ${MAX_GLIBC}"; fail=1
        fi
    fi
done

exit "$fail"
