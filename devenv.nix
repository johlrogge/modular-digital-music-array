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
  # Gateway mode: single address routes to all services
  env.MDMA_GATEWAY         = "tcp://mdma-909.local:5555";
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
      # Check gateway (single external port)
      nc -z -w2 mdma-909.local 5555 2>/dev/null \
        && echo "  ✓ gateway  :5555" \
        || echo "  ✗ gateway  :5555 (unreachable)"
      # Check console
      nc -z -w2 mdma-909.local 80 2>/dev/null \
        && echo "  ✓ console  :80" \
        || echo "  ✗ console  :80   (unreachable)"
      echo ""
      # Services via gateway
      if nc -z -w2 mdma-909.local 5555 2>/dev/null; then
        echo "Library:"
        mdma status 2>/dev/null | sed 's/^/  /' || echo "  (unreachable)"
        echo ""
        echo "Sources:"
        mdma source list 2>/dev/null | sed 's/^/  /' || echo "  (none)"
      fi
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
    echo "  gateway  $MDMA_GATEWAY"
    echo ""

    # Build mdma-cli and expose it directly as `mdma` on PATH
    cargo build -q --package mdma-cli 2>/dev/null \
      && export PATH="$MDMA_PROJECT_ROOT/target/debug:$PATH" \
      && echo "mdma CLI ready (gateway mode)" \
      || echo "mdma CLI not built — run: cargo build --package mdma-cli"

    echo ""
    echo "Commands:  mdma --help"
    echo "           mdma source list|sync|status|downloads"
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
