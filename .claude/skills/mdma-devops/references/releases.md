# Release Process

## Overview

MDMA uses git-flow for releases. The workspace version in the root `Cargo.toml` is the single source of truth for the release version.

## Release Workflow

### 1. Start Release Branch

```bash
git flow release start <version>
# e.g., git flow release start 0.3.0
```

### 2. Bump Workspace Version

Edit `Cargo.toml` at the workspace root:
```toml
[workspace.package]
version = "<new-version>"
```

### 3. Bump xbps Template Versions

Update each template in `void-packages/srcpkgs/*/template`:
```bash
version=<new-version>
revision=1  # Reset revision on version bump
```

### 4. Update Documentation

Dispatch the **documenter** agent to update all README.md files with the new version.

### 5. Commit Version Bumps

```
chore(release): bump to <version>
```

### 6. Finish Release

```bash
git flow release finish <version>
# This merges to main and develop, creates a tag v<version>
```

### 7. Push Everything

```bash
git push origin main develop --tags
```

### 8. CI Builds and Publishes

GitHub Actions on the `main` push:
1. Runs tests
2. Cross-compiles all services for aarch64
3. Creates .xbps packages
4. Signs packages
5. Publishes to GitHub Pages repository

### 9. Verify on Pi

```bash
ssh -4 -i ~/.ssh/mdma_pi admin@mdma-909.local
sudo xbps-install -Su
# Verify new versions installed
xbps-query -l | grep mdma
```

## Hotfix Workflow

For urgent fixes that can't wait for a normal release:

```bash
git flow hotfix start <version>
# Make fix, bump patch version
git flow hotfix finish <version>
git push origin main develop --tags
```

## Version Scheme

- **Major:** Breaking changes to protocols or APIs
- **Minor:** New features, new CLI commands
- **Patch:** Bug fixes, performance improvements

Package revision (`_1`, `_2`) is used when the package template changes but the code version doesn't (e.g., fixing a run script).
