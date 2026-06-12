{ pkgs, lib, ... }:

# CI base environment — the minimal closure needed to build, test, and
# package MDMA. This file IS the CI shell: when CI=true, devenv.nix adds
# nothing on top of it. The developer shell imports this and layers
# interactive tooling on top (see devenv.nix).
#
# Keeping this small shrinks the Nix closure CI must realize, which in
# turn shrinks the surface exposed to Cachix patch evictions — the cause
# of the recurring "path '…patch' is not valid" CI failures since v0.20.2.
{
  packages = with pkgs; [
    git
    just

    # Rust build dependencies (bindgen)
    clang
    llvmPackages.libclang

    # Cross-compilation (zig-based, simpler than gcc cross toolchain)
    zig
    cargo-zigbuild

    # Audio development
    pkg-config
    ffmpeg            # audio_decoder's build.rs generates FLAC test fixtures with it
  ] ++ lib.optionals pkgs.stdenv.isLinux [
    alsa-lib
    pipewire          # link-time dep; pulls ffmpeg transitively — accepted
    xbps              # Void Linux package tools (xbps-create, xbps-rindex)
  ];

  # Rust language support
  languages.rust = {
    enable = true;
    channel = "stable";
    targets = lib.optionals pkgs.stdenv.isLinux [ "aarch64-unknown-linux-gnu" ];
  };

  # Environment variables for bindgen
  env.LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

  # Tests
  enterTest = ''
    cargo polylith cargo --profile dev test
  '';
}
