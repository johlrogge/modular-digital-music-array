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

  # Pi service sockets — available in every devenv shell automatically
  env.MDMA_LIBRARY_SOCKET  = "tcp://mdma-909.local:5555";
  env.MDMA_BANDCAMP_SOCKET = "tcp://mdma-909.local:5556";
  env.MDMA_PLAYBACK_SOCKET = "tcp://mdma-909.local:5557";
  env.MDMA_SSH_KEY         = "/home/johlrogge/.ssh/mdma_pi";
  env.MDMA_PI_HOST         = "mdma-909.local";

  # Git hooks
  git-hooks.hooks = {
    rustfmt.enable = true;
  };

  # Shell scripts for things that aren't in mdma-cli (Pi administration via SSH).
  # mdma itself is on PATH via enterShell — use it directly for all library/playback commands.
  scripts = {
    # Set the iFi DAC (or default sink) volume via wpctl on the Pi
    mdma-volume.exec = ''
      set -euo pipefail
      vol="''${1:-1.0}"
      echo "Setting sink volume to $vol on $MDMA_PI_HOST"
      ssh -4 -i "$MDMA_SSH_KEY" "admin@$MDMA_PI_HOST" \
        "sudo -u _pipewire PIPEWIRE_RUNTIME_DIR=/run/pipewire wpctl set-volume @DEFAULT_AUDIO_SINK@ $vol"
    '';

    # Show service reachability and current PipeWire stream on the Pi
    mdma-status.exec = ''
      echo "Pi: $MDMA_PI_HOST"
      echo ""
      for port in 5555 5556 5557; do
        label="library "
        [ "$port" = "5556" ] && label="bandcamp"
        [ "$port" = "5557" ] && label="playback"
        nc -z -w2 mdma-909.local $port 2>/dev/null \
          && echo "  ✓ $label :$port" \
          || echo "  ✗ $label :$port (unreachable)"
      done
      echo ""
      echo "PipeWire stream:"
      ssh -4 -i "$MDMA_SSH_KEY" "admin@$MDMA_PI_HOST" \
        "sudo -u _pipewire PIPEWIRE_RUNTIME_DIR=/run/pipewire wpctl status 2>/dev/null" \
        | grep -E 'Sink|Stream|mdma|vol' | sed 's/^/  /'
    '';
  };

  # Shell hook — sets MDMA_PROJECT_ROOT to the live checkout, builds mdma-cli,
  # and adds target/debug to PATH so `mdma` works directly in the shell.
  enterShell = ''
    # Must be set here (not via env.*) so it points to the live checkout,
    # not the read-only Nix store copy that toString ./. would produce.
    export MDMA_PROJECT_ROOT="$PWD"

    echo "MDMA Development Environment"
    echo "Rust: $(rustc --version)"
    echo "Zig:  $(zig version)"
    echo ""
    echo "Pi: $MDMA_PI_HOST"
    echo "  library  $MDMA_LIBRARY_SOCKET"
    echo "  bandcamp $MDMA_BANDCAMP_SOCKET"
    echo "  playback $MDMA_PLAYBACK_SOCKET"
    echo ""

    # Build mdma-cli and expose it directly as `mdma` on PATH
    cargo build -q --package mdma-cli 2>/dev/null \
      && export PATH="$MDMA_PROJECT_ROOT/target/debug:$PATH" \
      && echo "mdma CLI ready (talks to Pi via TCP)" \
      || echo "mdma CLI not built — run: cargo build --package mdma-cli"

    echo ""
    echo "Commands:  mdma --help"
    echo "           mdma-volume <0-1>  mdma-status"
    echo "           just --list"
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
