{ pkgs, lib, config, inputs, ... }:

{
  # Development packages
  packages = with pkgs; [
    git
    helix
    just
    bacon
    socat              # For Claude Code sandboxing
    claude-code        # The actual package!
    
    # Rust build dependencies
    clang
    llvmPackages.libclang
    
    # Audio development
    pkg-config
    alsa-lib
    pipewire
    
    # Network tools
    nmap
  ];

  # Rust language support
  languages.rust = {
    enable = true;
    channel = "stable";
  };

  # Environment variables for bindgen
  env.LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

  # Git hooks
  git-hooks.hooks = {
    rustfmt.enable = true;
  };

  # Shell hook
  enterShell = ''
    echo "🎵 MDMA Development Environment"
    echo "Rust: $(rustc --version)"
    echo "Helix: $(hx --version | head -1)"
    echo "Clang: $(clang --version | head -1)"
    echo "Claude: $(claude --version)"
    echo ""
    echo "📦 Available commands:"
    echo "  claude               - Start Claude Code"
    echo "  just --list          - Show available tasks"
    echo "  bacon                - Watch mode for Rust"
    echo "  hx                   - Helix editor"
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
