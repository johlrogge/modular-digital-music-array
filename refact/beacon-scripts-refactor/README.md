# Beacon Package Pipeline - Scripts-Based Architecture

## 🎯 Clean, Testable, Maintainable

**All complex logic moved to bash scripts. Justfile just calls them!**

## 📁 Directory Structure

```
scripts/
├── ci/                          CI/CD scripts
│   ├── build-beacon.sh         Cross-compile beacon for ARM64
│   └── strip-beacon.sh         Strip beacon binary
├── package/                     Package building scripts
│   ├── install-xbps-tools.sh   Install xbps tools (one-time)
│   ├── create-package.sh       Create Void package
│   ├── create-repository.sh    Create and index repository
│   ├── serve-repository.sh     Serve repo locally
│   └── test-install.sh         Test install on Pi
└── utils/                       Utility scripts
    ├── get-version.sh          Show current version
    └── bump-revision.sh        Increment revision

justfile                         Thin wrapper calling scripts
```

## ✅ Benefits

### 1. No Heredoc Hell
```bash
# Before: Heredoc in justfile (error-prone)
just recipe:
    cat > file << 'EOF'
    exec 2>&1  # ← Just tries to parse this!
    EOF

# After: Script does it
just recipe:
    ./scripts/package/create-package.sh
```

### 2. Testable Independently
```bash
# Test scripts without justfile
./scripts/ci/build-beacon.sh
./scripts/package/create-package.sh
./scripts/package/create-repository.sh

# All scripts have proper error handling
# All scripts are idempotent where possible
```

### 3. Reusable Anywhere
```bash
# Use in CI/CD
- run: ./scripts/ci/build-beacon.sh

# Use in other scripts
./scripts/ci/build-beacon.sh
./scripts/ci/strip-beacon.sh

# Use manually
./scripts/package/test-install.sh 192.168.0.38
```

### 4. Easy to Review
```bash
# Just looks at actual bash code
cat scripts/package/create-package.sh

# No wrestling with justfile syntax
# No escaped quotes, no heredoc delimiters
# Plain bash!
```

### 5. Easier to Maintain
```bash
# Edit script directly
vim scripts/package/create-package.sh

# Test immediately
./scripts/package/create-package.sh

# No justfile syntax to worry about
```

## 🚀 Usage

### Local Development

```bash
# Build package
just pkg-build-all

# Behind the scenes, justfile calls:
# 1. ./scripts/ci/build-beacon.sh
# 2. ./scripts/ci/strip-beacon.sh
# 3. ./scripts/package/create-package.sh
# 4. ./scripts/package/create-repository.sh

# Serve for testing
just pkg-serve

# Test on Pi
just pkg-test-install 192.168.0.38
```

### Direct Script Usage

```bash
# You can also call scripts directly!
./scripts/ci/build-beacon.sh
./scripts/package/create-package.sh
./scripts/utils/get-version.sh
```

### CI/CD

```yaml
# GitHub Actions
- name: Build beacon
  run: ./scripts/ci/build-beacon.sh

- name: Create package
  run: ./scripts/package/create-package.sh

- name: Create repository
  run: ./scripts/package/create-repository.sh
```

## 📖 Script Reference

### CI Scripts

**`scripts/ci/build-beacon.sh`**
- Cross-compiles beacon for ARM64
- Outputs to: `target/aarch64-unknown-linux-gnu/release/beacon`
- No arguments needed

**`scripts/ci/strip-beacon.sh`**
- Strips beacon binary to reduce size
- Uses `aarch64-linux-gnu-strip` or falls back to `strip`
- No arguments needed

### Package Scripts

**`scripts/package/install-xbps-tools.sh`**
- Installs xbps-rindex for repository indexing
- One-time setup
- Tries sudo, falls back to ~/.local/bin
- No arguments needed

**`scripts/package/create-package.sh`**
- Creates Void Linux package from beacon binary
- Reads version from `void-packages/srcpkgs/beacon/template`
- Outputs to: `build/packages/beacon-VERSION.aarch64.xbps`
- No arguments needed

**`scripts/package/create-repository.sh`**
- Creates package repository with index
- Requires xbps-rindex (run install-xbps-tools.sh first)
- Outputs to: `build/repository/aarch64/`
- No arguments needed

**`scripts/package/serve-repository.sh`**
- Serves repository on http://localhost:8080
- Shows instructions for Pi configuration
- Press Ctrl+C to stop
- No arguments needed

**`scripts/package/test-install.sh`**
- Tests package installation on Pi
- Starts local server, configures Pi, installs package
- Usage: `./scripts/package/test-install.sh PI_HOST`
- Example: `./scripts/package/test-install.sh 192.168.0.38`

### Utility Scripts

**`scripts/utils/get-version.sh`**
- Shows current package version
- Reads from template or shows default
- No arguments needed

**`scripts/utils/bump-revision.sh`**
- Increments package revision number
- Creates backup before modifying
- No arguments needed

## 🔄 Workflow Comparison

### Old: Everything in Justfile
```
justfile (500+ lines)
├── Complex heredoc syntax
├── Nested quotes
├── Error-prone
└── Hard to test
```

### New: Scripts + Thin Justfile
```
justfile (100 lines - just calls scripts!)
├── Simple script calls
├── No complex syntax
└── Easy to read

scripts/ (8 files, ~200 lines each)
├── Pure bash
├── Independently testable
├── Reusable anywhere
└── Easy to maintain
```

## ✅ Integration Steps

### 1. Copy Files

```bash
cd ~/mdma-workspace

# Backup justfile
cp justfile justfile.backup

# Copy new structure
cp -r /path/to/beacon-scripts-refactor/scripts ./
cp /path/to/beacon-scripts-refactor/justfile ./
```

### 2. Verify Scripts Are Executable

```bash
# Check permissions
ls -la scripts/**/*.sh

# Should all be -rwxr-xr-x

# If not, make executable
chmod +x scripts/**/*.sh
```

### 3. Test Locally

```bash
# Test individual scripts
./scripts/ci/build-beacon.sh
./scripts/package/create-package.sh

# Or use justfile
just ci-build-beacon
just pkg-beacon
```

### 4. Update CI (if you have GitHub Actions)

```yaml
# Update .github/workflows/build-beacon-package.yml
- name: Build beacon
  run: ./scripts/ci/build-beacon.sh

- name: Strip beacon
  run: ./scripts/ci/strip-beacon.sh

- name: Create package
  run: ./scripts/package/create-package.sh

- name: Create repository
  run: ./scripts/package/create-repository.sh
```

## 📊 Script Error Handling

All scripts use:
```bash
#!/usr/bin/env bash
set -euo pipefail
```

This means:
- `-e`: Exit on any error
- `-u`: Exit on undefined variables
- `-o pipefail`: Exit on pipe failures

**Scripts are fail-fast and safe!**

## 🎉 Result

### Before
```bash
# Complex justfile recipe with heredocs
[group('package')]
pkg-beacon:
    #!/usr/bin/env bash
    cat > file << 'EOF'
    exec 2>&1  # ← Parse error!
    EOF
```

### After
```bash
# Simple justfile recipe
[group('package')]
pkg-beacon: ci-build-beacon ci-strip-beacon
    ./scripts/package/create-package.sh
```

**Much cleaner!** 🎊

## 📝 Notes

- All your existing justfile recipes remain unchanged
- Only package-related recipes moved to scripts
- Scripts can be called directly or via justfile
- Perfect for CI/CD (same scripts everywhere)
- Easy to test, maintain, and extend

## 🚀 Next Steps

1. ✅ Copy scripts/ directory to your workspace
2. ✅ Replace justfile with new version
3. ✅ Verify scripts are executable
4. ✅ Test: `just pkg-build-all`
5. ✅ Commit and push
6. ✅ Enjoy clean, maintainable build scripts!

---

**No more heredoc hell!** 🎉
