{ pkgs, lib, config, inputs, ... }:

{
  # Development packages
  packages = with pkgs; [
    git
    helix             # Your editor
    just              # Task runner
    bacon             # Rust watch mode
    bubblewrap        # Sandboxing for Claude Code
    
    # Rust build dependencies
    clang             # For bindgen
    llvmPackages.libclang  # For bindgen
    
    # Audio development (for local testing)
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

  # Custom scripts
  scripts.hello.exec = ''
    echo "🎵 MDMA Development Environment"
  '';
  
  scripts.claude-safe.exec = ''
    # Get the git repository root (works from any subdirectory)
    REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
    
    echo "🔒 Sandboxing command in: $REPO_ROOT"
    echo "Running: $@"
    echo ""
    
    bwrap \
      --unshare-all \
      --share-net \
      --die-with-parent \
      --ro-bind /usr /usr \
      --ro-bind /lib /lib \
      --ro-bind /lib64 /lib64 \
      --ro-bind /bin /bin \
      --ro-bind /sbin /sbin \
      --ro-bind /nix /nix \
      --proc /proc \
      --dev /dev \
      --tmpfs /tmp \
      --tmpfs /run \
      --ro-bind /etc/resolv.conf /etc/resolv.conf \
      --ro-bind-try /etc/ssl /etc/ssl \
      --ro-bind-try /etc/static/ssl /etc/static/ssl \
      --ro-bind-try /etc/ca-certificates /etc/ca-certificates \
      --bind "$REPO_ROOT" /workspace \
      --chdir /workspace \
      --setenv HOME /workspace \
      --setenv TMPDIR /tmp \
      --setenv TEMP /tmp \
      --setenv TMP /tmp \
      --setenv PATH "$PATH" \
      --setenv LIBCLANG_PATH "$LIBCLANG_PATH" \
      --setenv CARGO_HOME /workspace/.cargo \
      --setenv RUSTUP_HOME /workspace/.rustup \
      "$@"
  '';

  # Shell hook
  enterShell = ''
    hello
    echo "Rust: $(rustc --version)"
    echo "Helix: $(hx --version | head -1)"
    echo "Bubblewrap: $(bwrap --version | head -1)"
    echo "Clang: $(clang --version | head -1)"
    echo ""
    echo "📦 Available commands:"
    echo "  just --list      - Show available tasks"
    echo "  bacon           - Watch mode for Rust"
    echo "  hx              - Helix editor"
    echo "  claude-safe     - Sandboxed command execution"
  '';

  # Tests
  enterTest = ''
    echo "Running tests"
    git --version | grep --color=auto "${pkgs.git.version}"
    rustc --version
    hx --version
    bwrap --version
    clang --version
  '';
}
