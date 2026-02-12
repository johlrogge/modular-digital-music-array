# Rust Development Tooling: bacon and just

## bacon - Background Rust Code Checker

bacon runs cargo commands in the background and shows results immediately.

### Installation
```bash
cargo install bacon
```

### Basic Usage
```bash
bacon              # Run default job (usually check)
bacon test         # Run tests
bacon clippy       # Run clippy
bacon doc          # Build docs
```

### Configuration: bacon.toml

Place in project root:
```toml
# bacon.toml
[jobs.check]
command = ["cargo", "check", "--color=always"]
need_stdout = false

[jobs.clippy]
command = ["cargo", "clippy", "--color=always"]
need_stdout = false

[jobs.test]
command = ["cargo", "test", "--color=always"]
need_stdout = true
watch = ["tests"]

[jobs.run]
command = ["cargo", "run"]
need_stdout = true

[jobs.doc]
command = ["cargo", "doc", "--no-deps", "--open"]
need_stdout = true
on_success = "open"

# Custom job
[jobs.coverage]
command = ["cargo", "tarpaulin", "--out", "Html"]
need_stdout = true
```

### Advanced Patterns

#### Workspace-Specific Checks
```toml
[jobs.check-workspace]
command = ["cargo", "check", "--workspace", "--all-features"]

[jobs.check-package]
command = ["cargo", "check", "-p", "my-package"]
```

#### Different Profiles
```toml
[jobs.check-release]
command = ["cargo", "check", "--release"]

[jobs.clippy-strict]
command = ["cargo", "clippy", "--", "-W", "clippy::pedantic"]
```

#### Watch Specific Paths
```toml
[jobs.test-integration]
command = ["cargo", "test", "--test", "integration"]
watch = ["tests", "src"]
```

### Keyboard Shortcuts
- `c` - cargo check
- `t` - cargo test
- `r` - cargo run
- `d` - cargo doc
- `q` - quit
- `h` - help

### Tips
1. Keep bacon running in a terminal split
2. Use keybindings to switch between jobs
3. Combine with just for complex workflows
4. Configure per-workspace for monorepos

## just - Command Runner

just is a command runner like make, but simpler and cross-platform.

### Installation
```bash
cargo install just
```

### Basic justfile

Place `justfile` (or `Justfile`) in project root:
```just
# List all recipes
default:
    @just --list

# Run tests
test:
    cargo test

# Run tests with coverage
coverage:
    cargo tarpaulin --out Html

# Format code
fmt:
    cargo fmt

# Run clippy
lint:
    cargo clippy -- -D warnings

# Build release
build:
    cargo build --release
```

### Dependencies

Recipes can depend on other recipes:
```just
# Run linter before tests
test: lint
    cargo test

# Format and lint before building
build: fmt lint
    cargo build --release

# Chain multiple dependencies
ci: fmt lint test
    @echo "CI checks passed!"
```

### Recipe Parameters

```just
# Run specific test
test-one TEST:
    cargo test {{TEST}}

# Run package tests
test-package PACKAGE:
    cargo test -p {{PACKAGE}}

# Build with specific features
build-features FEATURES:
    cargo build --features {{FEATURES}}

# Optional parameters with defaults
run profile="dev":
    cargo run --profile {{profile}}
```

### Groups and Attributes

#### Mark recipes as private
```just
# Public recipe (shown in --list)
build:
    cargo build

# Private recipe (hidden from --list)
[private]
_internal-helper:
    echo "Internal use only"

# Use private recipe
deploy: _internal-helper
    cargo build --release
```

#### Group related recipes
```just
# Documentation group
[group: 'docs']
doc:
    cargo doc --no-deps

[group: 'docs']
doc-open:
    cargo doc --no-deps --open

# Testing group
[group: 'test']
test:
    cargo test

[group: 'test']
test-integration:
    cargo test --test integration

# Development group
[group: 'dev']
dev:
    bacon

[group: 'dev']  
fmt:
    cargo fmt
```

Now `just --list --groups` shows organized recipes.

### Advanced Patterns

#### Workspace Management
```just
# Check all workspace members
check-all:
    cargo check --workspace

# Test specific workspace package
test-workspace PACKAGE:
    cargo test -p {{PACKAGE}}

# Build all binaries
build-bins:
    cargo build --workspace --bins

# For each component (useful in Polylith)
test-components:
    #!/usr/bin/env bash
    set -euo pipefail
    for component in components/*; do
        echo "Testing $component"
        cargo test -p $(basename $component)
    done
```

#### Environment-Specific Builds
```just
# Development build
dev:
    cargo run

# Production build with optimizations
prod:
    RUSTFLAGS="-C target-cpu=native" cargo build --release

# Docker build
docker:
    docker build -t myapp:latest .
```

#### Conditional Execution
```just
# Only run if tests pass
deploy: test
    @echo "Deploying..."
    cargo build --release
    # deployment commands

# Check if file exists
check-config:
    @test -f config.toml || (echo "config.toml missing" && exit 1)

# Run based on feature
build-with-feature FEATURE:
    @if [ "{{FEATURE}}" = "experimental" ]; then \
        cargo build --features experimental; \
    else \
        cargo build --features {{FEATURE}}; \
    fi
```

#### Working with bacon
```just
# Start bacon in background
watch:
    bacon check

# Start bacon with specific job
watch-test:
    bacon test

# Parallel: bacon + cargo watch for other tasks
watch-all:
    just watch & cargo watch -x 'run --example server'
```

### Multi-Line Scripts
```just
# Complex setup
setup:
    #!/usr/bin/env bash
    set -euxo pipefail
    
    echo "Installing dependencies..."
    cargo install cargo-edit cargo-watch bacon
    
    echo "Setting up git hooks..."
    cp scripts/pre-commit .git/hooks/
    chmod +x .git/hooks/pre-commit
    
    echo "Setup complete!"

# Python script example
analyze:
    #!/usr/bin/env python3
    import subprocess
    import json
    
    result = subprocess.run(
        ["cargo", "metadata", "--format-version=1"],
        capture_output=True,
        text=True
    )
    metadata = json.loads(result.stdout)
    print(f"Workspace has {len(metadata['packages'])} packages")
```

### Variables and Substitution
```just
# Set variables
rust_version := "1.75"
project_name := "my-app"

# Use in recipes
build:
    @echo "Building {{project_name}} with Rust {{rust_version}}"
    cargo build

# Environment variables
build-prod:
    RUSTFLAGS="-C target-cpu=native" cargo build --release

# Command substitution
git_hash := `git rev-parse --short HEAD`

build-with-hash:
    @echo "Building version {{git_hash}}"
    cargo build
```

### Integration Patterns

#### Complete Development Workflow
```just
# Default: show all commands
default:
    @just --list

# === Development ===
[group: 'dev']
dev:
    bacon check

[group: 'dev']
run *ARGS:
    cargo run -- {{ARGS}}

[group: 'dev']
fmt:
    cargo fmt --all

# === Testing ===
[group: 'test']
test:
    cargo test

[group: 'test']
test-coverage:
    cargo tarpaulin --out Html --output-dir coverage

[group: 'test']
test-watch:
    bacon test

# === Quality ===
[group: 'qa']
lint:
    cargo clippy --all-targets --all-features -- -D warnings

[group: 'qa']
check-all: fmt lint test
    @echo "✓ All checks passed!"

# === Building ===
[group: 'build']
build:
    cargo build

[group: 'build']
build-release:
    cargo build --release --locked

[group: 'build']
build-optimized:
    RUSTFLAGS="-C target-cpu=native -C opt-level=3" cargo build --release

# === Documentation ===
[group: 'docs']
doc:
    cargo doc --no-deps

[group: 'docs']
doc-open:
    cargo doc --no-deps --open

# === Maintenance ===
[group: 'maintenance']
clean:
    cargo clean

[group: 'maintenance']
update:
    cargo update

[group: 'maintenance']
outdated:
    cargo outdated

# === CI/CD ===
[group: 'ci']
ci: check-all
    @echo "✓ CI checks passed!"

[group: 'ci']
ci-deploy: ci build-release
    @echo "Ready to deploy"
```

#### Polylith-Specific justfile
```just
# Show all components
list-components:
    @ls -1 components

# Test changed components since main
test-changed:
    #!/usr/bin/env bash
    set -euo pipefail
    
    changed=$(git diff --name-only main | grep "^components/" | cut -d/ -f2 | sort -u)
    
    for component in $changed; do
        echo "Testing component: $component"
        cargo test -p $component
    done

# Build affected projects
build-affected:
    #!/usr/bin/env bash
    set -euo pipefail
    
    # Find changed components
    changed=$(git diff --name-only main | grep "^components/" | cut -d/ -f2 | sort -u)
    
    # Find projects that depend on changed components
    for project in projects/*; do
        for component in $changed; do
            if grep -q "path.*components/$component" $project/Cargo.toml; then
                echo "Building $(basename $project)"
                cargo build -p $(basename $project)
                break
            fi
        done
    done
```

### Best Practices

1. **Use groups for organization**
   - Development tasks
   - Testing tasks
   - CI/CD tasks
   - Maintenance tasks

2. **Mark internal recipes as private**
   ```just
   [private]
   _helper:
       echo "internal"
   ```

3. **Use dependencies for workflows**
   ```just
   deploy: test lint build
       # deployment
   ```

4. **Document recipes with comments**
   ```just
   # Run tests with coverage report
   coverage:
       cargo tarpaulin
   ```

5. **Provide defaults for parameters**
   ```just
   build profile="dev":
       cargo build --profile {{profile}}
   ```

6. **Use shell scripts for complexity**
   ```just
   complex:
       #!/usr/bin/env bash
       # Multi-line bash script
   ```

7. **Combine with bacon for live feedback**
   ```just
   watch:
       bacon test
   ```

## Complete Setup Example

### Project Structure
```
my-project/
├── justfile
├── bacon.toml
├── Cargo.toml
├── src/
└── tests/
```

### justfile
```just
default:
    @just --list --groups

[group: 'dev']
dev:
    bacon check

[group: 'test']
test:
    cargo test

[group: 'qa']
ci: fmt lint test
    @echo "✓ Ready to commit"

[group: 'build']
build:
    cargo build --release
```

### bacon.toml
```toml
[jobs.check]
command = ["cargo", "check", "--all-targets"]

[jobs.test]
command = ["cargo", "test"]
need_stdout = true
```

### Workflow
```bash
# Terminal 1: Keep bacon running
just dev

# Terminal 2: Run just commands
just test
just ci
just build
```

This setup provides:
- ✅ Immediate feedback (bacon)
- ✅ Organized commands (just with groups)
- ✅ Clear workflows (just dependencies)
- ✅ Easy CI integration (just ci)
