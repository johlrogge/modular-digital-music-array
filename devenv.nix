{ pkgs, lib, config, inputs, ... }:

{
  # Development packages
  packages = with pkgs; [
    git
    helix
    just
    bacon
    socat              # For Claude Code sandboxing
    inputs.claude-code-nix.packages.${pkgs.system}.default  # Claude Code from sadjow/claude-code-nix flake

    # Rust build dependencies
    clang
    llvmPackages.libclang

    # Cross-compilation (zig-based, simpler than gcc cross toolchain)
    zig
    cargo-zigbuild

    # Audio development
    pkg-config
    alsa-lib
    pipewire

    # Network tools
    nmap
    sshpass
  ];

  # Rust language support
  languages.rust = {
    enable = true;
    channel = "stable";
    targets = [ "aarch64-unknown-linux-gnu" ];
  };

  # Environment variables for bindgen
  env.LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

  # Git hooks
  git-hooks.hooks = {
    rustfmt.enable = true;
  };

  # Shell hook
  enterShell = ''
    echo "MDMA Development Environment"
    echo "Rust: $(rustc --version)"
    echo "Zig:  $(zig version)"
    echo ""
    echo "Available commands:"
    echo "  just deploy-dev      - Build and deploy beacon to Pi"
    echo "  just beacon-cross    - Cross-compile beacon for aarch64"
    echo "  just --list          - Show all tasks"
  '';

  # Tests
  enterTest = ''
    echo "Running tests"
    git --version | grep --color=auto "${pkgs.git.version}"
    rustc --version
    hx --version
    clang --version
    claude --version
  '';
}
