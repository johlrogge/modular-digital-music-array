#!/bin/sh
# zig wrapper for cargo-zigbuild (selected via CARGO_ZIGBUILD_ZIG_PATH).
#
# Newer rustc passes -Wl,--fix-cortex-a53-843419 when linking
# aarch64-unknown-linux-gnu. zig's bundled lld frontend rejects it
# ("unsupported linker arg"), and cargo-zigbuild 0.20.1 does not filter it.
#
# That flag enables the Cortex-A53 erratum 843419 workaround. The Pi 5 is a
# Cortex-A76, which is not affected, so dropping the flag is safe for our target.
#
# Remove this wrapper once cargo-zigbuild/zig in the pinned nixpkgs handle the flag.
for arg in "$@"; do
    shift
    case "$arg" in
        -Wl,--fix-cortex-a53-843419) ;;
        *) set -- "$@" "$arg" ;;
    esac
done
exec zig "$@"
