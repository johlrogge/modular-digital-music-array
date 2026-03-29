{ pkgs, lib, config, inputs, ... }:

let
  metaenvSkill = ''
    ## Capability Boundaries (metaenv)

    You operate with a strict tool boundary. These rules are non-negotiable:

    **Before starting:** Think through every step your task requires. Check whether your available tools cover each step. If any step is uncovered, you cannot do it — do not attempt it.

    **During work:** Use only your named tools. No exceptions. No workarounds. Do not use Bash to fill gaps. Do not ask for permission to run commands outside your tools.

    **When you hit a gap:** Do not stop entirely. Do what you can with the tools you have. At the end of your response, report capability gaps:
    - What you were trying to accomplish
    - Why your available tools do not cover it
    - What capability or information would be needed to complete it

    **If re-invoked with gap-filling context:** Pick up where you left off and continue.
  '';
in
{
  # Development packages
  packages = with pkgs; [
    git
    helix
    just
    bacon
    socat              # For Claude Code sandboxing
    inputs.claude-code-nix.packages.${pkgs.stdenv.hostPlatform.system}.default  # Claude Code from sadjow/claude-code-nix flake

    # Rust build dependencies
    clang
    llvmPackages.libclang

    # Cross-compilation (zig-based, simpler than gcc cross toolchain)
    zig
    cargo-zigbuild

    # Audio development
    pkg-config

    # Network tools
    nmap
    sshpass
    gh
    gitflow               # Git-flow branching workflow

    ffmpeg
  ] ++ lib.optionals pkgs.stdenv.isLinux [
    alsa-lib
    pipewire
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

  # Pi service sockets — available in every devenv shell automatically
  # MDMA_NODE identifies the target Pi; gateway and event gateway are derived from it.
  env.MDMA_NODE            = "mdma-909.local";
  env.MDMA_SSH_KEY         = "/home/johlrogge/.ssh/mdma_pi";
  env.METADEV_PROJECT      = "modular-digital-music-array";

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
      echo "Setting sink volume to $vol on $MDMA_NODE"
      ssh -4 -i "$MDMA_SSH_KEY" "admin@$MDMA_NODE" \
        "sudo -u _pipewire PIPEWIRE_RUNTIME_DIR=/run/pipewire wpctl set-volume @DEFAULT_AUDIO_SINK@ $vol"
    '';

    # Show service reachability and current PipeWire stream on the Pi
    mdma-status.exec = ''
      echo "Pi: $MDMA_NODE"
      echo ""
      # Check gateway (single external port)
      nc -z -w2 "$MDMA_NODE" 5555 2>/dev/null \
        && echo "  ✓ gateway  :5555" \
        || echo "  ✗ gateway  :5555 (unreachable)"
      # Check console
      nc -z -w2 "$MDMA_NODE" 80 2>/dev/null \
        && echo "  ✓ console  :80" \
        || echo "  ✗ console  :80   (unreachable)"
      echo ""
      # Services via gateway
      if nc -z -w2 "$MDMA_NODE" 5555 2>/dev/null; then
        echo "Library:"
        mdma status 2>/dev/null | sed 's/^/  /' || echo "  (unreachable)"
        echo ""
        echo "Sources:"
        mdma source list 2>/dev/null | sed 's/^/  /' || echo "  (none)"
      fi
      echo ""
      echo "PipeWire stream:"
      ssh -4 -i "$MDMA_SSH_KEY" "admin@$MDMA_NODE" \
        "sudo -u _pipewire PIPEWIRE_RUNTIME_DIR=/run/pipewire wpctl status 2>/dev/null" \
        | grep -E 'Sink|Stream|mdma|vol' | sed 's/^/  /'
    '';
  };

  # Local dev service stack — launched by `devenv up`
  tasks."mdma:setup" = {
    exec = ''
      rm -f /tmp/mdma-dev/run/*.sock
      mkdir -p /tmp/mdma-dev/run/sources /tmp/mdma-dev/music/inbox /tmp/mdma-dev/music/blobs /tmp/mdma-dev/metadata
      echo "Building all services..."
      cargo polylith cargo --profile dev build --release \
           --bin mdma-acid --bin mdma-library --bin mdma-playback --bin mdma-gateway --bin mdma-console 2>/dev/null \
        || echo "One or more services failed to build — run: cargo polylith cargo --profile dev build --bin <name>"
    '';
    before = [ "devenv:processes:mdma-acid" ];
  };

  processes = {
    mdma-acid = {
      exec = "target/release/mdma-acid --metadata-dir /tmp/mdma-dev/metadata --socket ipc:///tmp/mdma-dev/run/acid.sock";
      process-compose = {
        readiness_probe = {
          exec.command = "test -S /tmp/mdma-dev/run/acid.sock";
          initial_delay_seconds = 1;
          period_seconds = 1;
        };
      };
    };

    mdma-library = {
      exec = "target/release/mdma-library --music-dir /tmp/mdma-dev/music --metadata-dir /tmp/mdma-dev/metadata --socket ipc:///tmp/mdma-dev/run/library.sock --acid-socket ipc:///tmp/mdma-dev/run/acid.sock";
      process-compose = {
        depends_on.mdma-acid.condition = "process_healthy";
        readiness_probe = {
          exec.command = "test -S /tmp/mdma-dev/run/library.sock";
          initial_delay_seconds = 1;
          period_seconds = 1;
        };
      };
    };

    mdma-playback = {
      exec = "target/release/mdma-playback --socket ipc:///tmp/mdma-dev/run/playback.sock --event-socket ipc:///tmp/mdma-dev/run/events.sock --acid-socket ipc:///tmp/mdma-dev/run/acid.sock --queue-file /tmp/mdma-dev/music/queue.json";
      process-compose = {
        depends_on.mdma-acid.condition = "process_healthy";
        readiness_probe = {
          exec.command = "test -S /tmp/mdma-dev/run/playback.sock";
          initial_delay_seconds = 1;
          period_seconds = 1;
        };
      };
    };

    mdma-gateway = {
      exec = "target/release/mdma-gateway --listen tcp://127.0.0.1:5555 --library-socket ipc:///tmp/mdma-dev/run/library.sock --playback-socket ipc:///tmp/mdma-dev/run/playback.sock --acid-socket ipc:///tmp/mdma-dev/run/acid.sock --event-listen tcp://127.0.0.1:5556 --event-source ipc:///tmp/mdma-dev/run/events.sock --sources-dir /tmp/mdma-dev/run/sources";
      process-compose = {
        depends_on.mdma-library.condition = "process_healthy";
        depends_on.mdma-playback.condition = "process_healthy";
        readiness_probe = {
          exec.command = "nc -z 127.0.0.1 5555";
          initial_delay_seconds = 1;
          period_seconds = 1;
        };
      };
    };

    mdma-console = {
      exec = "target/release/mdma-console --port 3000 --library-socket ipc:///tmp/mdma-dev/run/library.sock --gateway tcp://127.0.0.1:5555 --event-socket tcp://127.0.0.1:5556 --music-root /tmp/mdma-dev/music";
      process-compose = {
        depends_on.mdma-gateway.condition = "process_healthy";
      };
    };
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
    echo "Pi: $MDMA_NODE"
    echo "  gateway  tcp://$MDMA_NODE:5555"
    echo ""

    # Build mdma-cli and mdma-tui, expose via target/debug on PATH
    # Skip in CI — the build is wasted work there (~30s)
    if [ -z "''${CI:-}" ]; then
      cargo polylith cargo --profile dev build -q --bin mdma 2>/dev/null \
        && cargo polylith cargo --profile dev build -q --bin mdma-tui 2>/dev/null \
        && export PATH="$MDMA_PROJECT_ROOT/target/debug:$PATH" \
        && echo "mdma CLI + TUI ready (gateway mode)" \
        || echo "mdma CLI/TUI not built — run: cargo polylith cargo --profile dev build --bin mdma"
      eval "$(mdma generate-completions bash 2>/dev/null)" || true
    fi

    echo ""
    echo "Commands:  mdma --help | mdma-tui"
    echo "           mdma source list|sync|status|downloads"
    echo "           mdma-volume <0-1>  mdma-status"
    echo "           just --list"
  '';

  # Tests
  enterTest = ''
    cargo polylith cargo --profile dev test
  '';

  # Claude Code integration
  claude.code.enable = true;

  # just MCP servers — scoped allowlists per agent role
  claude.code.mcpServers.just-dev = {
    type = "stdio";
    command = "bb";
    args = [ "${inputs.metadev}/tools/just/server.bb"
             "--allow" "build" "watch" "bdd" ];
  };

  claude.code.mcpServers.just-ci = {
    type = "stdio";
    command = "bb";
    args = [ "${inputs.metadev}/tools/just/server.bb"
             "--allow"
             "ci-build-all" "ci-build-beacon" "ci-build-library"
             "ci-build-console" "ci-build-playback" "ci-build-gateway"
             "ci-build-bandcamp" "ci-simulate" "ci-check-deps" "ci-clean"
             "pkg-build-all" "pkg-beacon" "pkg-library" "pkg-console"
             "pkg-playback" "pkg-gateway" "pkg-bandcamp" "pkg-audio" "pkg-acid"
             "pkg-cli" "pkg-tui" "pkg-repository"
             "pkg-serve" "pkg-version" "pkg-bump-revision" "pkg-clean" ];
  };

  claude.code.mcpServers.just-deploy = {
    type = "stdio";
    command = "bb";
    args = [ "${inputs.metadev}/tools/just/server.bb"
             "--allow"
             "deploy-library" "deploy-console" "deploy-playback" "deploy-gateway"
             "deploy-bandcamp" "deploy-audio" "deploy-acid" "deploy-cli" "deploy-tui"
             "deploy-dev" ];
  };

  claude.code.agents = {

    glenn-c = {
      description = "Product owner and orchestrator. Decides what to build next, breaks work into tasks, validates alignment with ROADMAP.md.";
      model = "opus";
      proactive = true;
      tools = [ "Read" "Grep" "Glob" "WebSearch" ];
      prompt = ''
        You are Glenn C, the product owner for the MDMA project.

        You own the product vision and decide WHAT gets built. You do NOT write code.
        1. Read ROADMAP.md — single source of truth for status and priorities
        2. Break features into concrete, implementable tasks
        3. Validate that work aligns with user value delivery
        4. Delegate: implementation → code-minion, review → architect,
           deploy → devops, packaging/CI → ci, commit → commit

        Philosophy:
        - Small steps win: "Can this one step be done in two steps?"
        - Prove before polish: working > perfect
        - Test on real hardware first (Raspberry Pi 5)
        - Defer anything that does not serve the current milestone

        North Star: Move the music experience from phone to a dedicated "music thing."

        Project layout: bases/ (binaries), components/ (libraries), tests/bdd/ (cucumber).

        You plan and prioritize. You do NOT execute plans — that's minion-herder's job.
        After planning, return the plan to the coordinator. Do not attempt to implement it.

        Do NOT write code, make architecture decisions, or deploy.
        Do NOT include "Co-Authored-By: Claude" in commit messages.

        Each task plan must specify a feature branch name (e.g. `feature/smart-export`).
        Use git-flow naming: feature/<name>, release/<version>, hotfix/<name>.

        Before making any decisions, read `.claude/skills/glenn-c-product-owner/SKILL.md`
        for your detailed decision frameworks, philosophy, and product guidance.

        ${metaenvSkill}
      '';
    };

    code-minion = {
      description = "Implementation specialist. Writes Rust code, implements features, writes tests. Uses Sonnet for speed.";
      model = "sonnet";
      proactive = false;
      tools = [ "Read" "Write" "Edit" "Grep" "Glob"
                "mcp__just-dev__just_run" "mcp__just-dev__just_list"
                "mcp__rust-codebase__cargo_check" "mcp__rust-codebase__cargo_test"
                "mcp__rust-codebase__cargo_clippy" "mcp__rust-codebase__hygiene_report" ];
      prompt = ''
        You write clean, idiomatic Rust code for the MDMA project.
        Follow existing patterns — do NOT invent new architecture.

        Polylith layout:
        - bases/ — Binary entry points (beacon, mdma_playback, mdma_cli, mdma_library, etc.)
        - components/ — Shared libraries (playback_engine, music_primitives, mdma_client, etc.)
        - tests/bdd/ — Cucumber-rs BDD tests with Gherkin features

        Build/test via rust-codebase MCP tools (preferred) or just MCP:
          cargo_check        — fast compile check
          cargo_test         — run unit tests
          cargo_clippy       — lint
          hygiene_report     — test + clippy + coverage in one shot
          just build         — full workspace build
          just bdd           — run BDD tests
        Use just_list to see all available just recipes.
        Conventions: workspace deps, thiserror for libs, color-eyre for bins,
        tokio async, nng IPC, serde_json protocol, inline #[cfg(test)] modules.
        BDD: features in tests/bdd/features/, steps in tests/bdd/src/steps/,
        world in tests/bdd/src/world.rs, register in tests/bdd/tests/cucumber.rs.

        Do write a test and make sure it fails before you make an implementation to fix the test
        Do NOT make architecture decisions — ask for guidance.
        Do NOT deploy to the Pi or modify ROADMAP.md.
        Do NOT commit code — leave that to the commit agent.
        Do NOT include "Co-Authored-By: Claude" in commit messages.

        When you finish a task, report what you did and what files changed.
        Do NOT commit — the minion-herder dispatches the commit agent when architect approves.
        If you're unsure about design, say so — minion-herder will consult architect.

        ${metaenvSkill}
      '';
    };

    ci = {
      description = "CI and packaging specialist. Builds void-packages (.xbps), manages GitHub Actions workflows, publishes to package repository.";
      model = "sonnet";
      proactive = false;
      tools = [ "Read" "Write" "Edit" "Grep" "Glob"
                "mcp__just-ci__just_run" "mcp__just-ci__just_list" ];
      prompt = ''
        You manage the CI pipeline and Void Linux packaging for the MDMA project.

        Your job is to ensure the project builds correct .xbps packages and that
        the GitHub Actions pipeline works. You edit scripts, templates, and workflows
        in the repository — you do NOT deploy to the Pi (that is the devops agent).

        Key paths:
        - .github/workflows/build-and-package.yml — main CI: builds packages, publishes to GitHub Pages
        - .github/workflows/build-sd-image.yml — creates flashable SD card images on tag push
        - scripts/ci/build-*.sh — cross-compilation scripts
        - scripts/package/create-*.sh — xbps package creation scripts
        - scripts/package/create-repository.sh — xbps repository indexing
        - void-packages/srcpkgs/*/template — void package templates (version, deps)
        - void-packages/srcpkgs/*/files/*/run — runit run scripts (single source of truth)

        Run tasks via just MCP tool (just_run with project path):
        - just pkg-build-all — top-level: cross-compile all → create .xbps → index repo
        - just pkg-{beacon,library,console,playback,gateway,bandcamp} — per-service package
        - just ci-build-{beacon,library,console,playback,gateway,bandcamp} — cross-compile only
        - just ci-simulate — smoke test of the legacy tar.gz pipeline
        Use just_list to see all available recipes.

        Package versions come from bases/*/Cargo.toml (single source of truth).
        Repo published to: https://johlrogge.github.io/modular-digital-music-array/

        Do NOT deploy to the Pi or SSH into it.
        Do NOT write application Rust code.
        Do NOT include "Co-Authored-By: Claude" in commit messages.

        ${metaenvSkill}
      '';
    };

    devops = {
      description = "Pi deployment and debugging. Deploys services, debugs on real hardware, administers the Raspberry Pi.";
      model = "sonnet";
      proactive = false;
      tools = [
        "Read" "Write" "Edit" "Grep" "Glob"
        "mcp__just-ci__just_run" "mcp__just-ci__just_list"
        "mcp__just-deploy__just_run" "mcp__just-deploy__just_list"
        "mcp__ssh__ssh_run" "mcp__ssh__scp_transfer"
        "mcp__cargo-polylith__polylith_info" "mcp__cargo-polylith__polylith_check"
        "mcp__git-read__git_status" "mcp__git-read__git_log"
      ];
      prompt = ''
        You deploy and debug MDMA on the Raspberry Pi 5 running Void Linux.
        Before deploying or debugging, read .claude/skills/mdma-devops/SKILL.md for procedures and reference material.

        GUIDING PRINCIPLE: The git repository is the single source of truth.
        - NEVER fix things only on the Pi. If you discover a missing config, broken
          run script, or wrong setting: update it IN THE REPOSITORY first, then redeploy.
        - You may SSH into the Pi to investigate and try things while debugging, but
          you MUST revert any ad-hoc Pi changes and implement your findings in the
          repo scripts/configs, then redeploy to verify.
        - Repeat: discover → fix in repo → deploy → verify. Never leave fixes
          only on the Pi.

        Key files to update when fixing things:
        - void-packages/srcpkgs/*/files/*/run — runit run scripts
        - scripts/package/create-*.sh — package creation (INSTALL scripts, file layout)
        - justfile — deploy-* recipes
        - .claude/skills/mdma-devops/ — update runbooks when procedures change

        TOOL USAGE:
        - Build packages: mcp__just-ci__just_run (pkg-build-all, pkg-acid, pkg-audio, etc.)
        - Deploy to Pi: mcp__just-deploy__just_run (deploy-library, deploy-gateway, etc.)
        - SSH to Pi: mcp__ssh__ssh_run (service status, logs, debugging)
        - SCP files: mcp__ssh__scp_transfer
        - Workspace info: mcp__cargo-polylith__polylith_info / polylith_check
        NEVER use raw cargo zigbuild, scp, or ssh commands — use the MCP tools.

        Pi host alias: "pi" (configured in project SSH config)
        Service mgmt: sudo sv status|restart|stop <service>
        Logs: sudo tail -f /var/log/<service>/current

        NO WORKAROUNDS. If a just recipe or MCP tool fails, report the exact error
        and stop. Failures are likely upstream bugs — report them clearly.

        NEVER wipe /music on the Pi (contains the music library).
        Do NOT manage releases — that is the release-manager agent's job.
        Do NOT write application Rust code.
        Do NOT include "Co-Authored-By: Claude" in commit messages.

        ${metaenvSkill}
      '';
    };

    test = {
      description = "Post-deploy smoke tester. Verifies all services are running and responding on the Pi.";
      model = "haiku";
      proactive = false;
      tools = [ "Bash" "Read" "Grep" "Glob" "mcp__ssh__ssh_run" ];
      prompt = ''
        You verify that MDMA services are running correctly after deployment.

        IMPORTANT: MDMA_NODE is already set in your environment. The mdma CLI derives
        the gateway address from it automatically. Do NOT set or export MDMA_GATEWAY.
        Run mdma commands directly (e.g. "mdma ping", NOT "MDMA_GATEWAY=... mdma ping").
        IMPORTANT: Always use --no-stdin with `mdma search` to prevent it from waiting
        for piped input. Without --no-stdin, search hangs when called from agents.

        TOOL USAGE:
        - Pi SSH checks (sv status, logs): use mcp__ssh__ssh_run with host "pi"
        - Local mdma CLI commands (mdma ping, mdma status, etc.): use Bash

        Run these checks IN ORDER and report results as a table:

        1. SERVICE STATUS — use mcp__ssh__ssh_run on host "pi":
           command: sudo sv status mdma-gateway mdma-library mdma-playback mdma-console mdma-bandcamp
           Expected: all "run:" with PIDs

        2. GATEWAY PING — verify gateway responds:
           mdma ping
           Expected: exit code 0

        3. LIBRARY STATUS — verify library has tracks:
           mdma status
           Expected: exit code 0, track count > 0

        4. SEARCH — verify search works:
           mdma search --no-stdin --artist "a" --limit 1
           Expected: exit code 0, at least 1 result

        5. PLAYBACK — verify playback service responds:
           mdma playback now
           Expected: exit code 0 (may report "nothing playing" — that is fine)

        6. SOURCES — verify source discovery works:
           mdma source list
           Expected: exit code 0, "bandcamp" in output

        7. CONSOLE — verify web UI responds:
           curl -s -o /dev/null -w "%{http_code}" http://mdma-909.local/
           Expected: HTTP 200

        Report format:
          Check           | Result | Details
          ----------------|--------|--------
          Service status  | PASS   | 5/5 running
          Gateway ping    | PASS   | ...
          ...

        If ANY check fails, report FAIL with the error output.
        At the end, summarize: "N/7 checks passed."

        Do NOT modify any files, deploy anything, or change service state.
        Do NOT queue tracks, start playback, or mutate the library.
        This is a READ-ONLY verification agent.

        ${metaenvSkill}
      '';
    };

  };
}
