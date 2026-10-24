# Release Process

## Overview

MDMA uses git-flow for releases. Versioning is split between the workspace and individual packages:

- **Workspace version** (`[workspace.package] version` in root `Cargo.toml`) applies to all packages that use `version.workspace = true`. This is bumped on every release.
- **Independently versioned packages** — bases with a hardcoded `version = "x.y.z"` in their own `Cargo.toml`, and any components with hardcoded versions (e.g. `library_service`), must be bumped individually based on what changed in that package.

## Release Workflow

### 1. Start Release Branch

```bash
git flow release start <version>
# e.g., git flow release start 0.3.0
```

### 2. Bump Versions

**Workspace version** — edit `Polylith.toml`:
```toml
[workspace.package]
version = "<new-version>"
```

**Independently versioned bases and components** — for each `bases/*/Cargo.toml` and any component `Cargo.toml` that has a hardcoded `version = "x.y.z"` (not `version.workspace = true`), bump the version individually based on what changed:
```toml
[package]
version = "<new-version>"
```

Only packages using `version.workspace = true` inherit the workspace version automatically.

### 3. Bump xbps Template Versions

Update each template in `void-packages/srcpkgs/*/template`. Each template's `version` field must match the version in its corresponding `Cargo.toml` (not necessarily the workspace version if that base is independently versioned):
```bash
version=<package-version>
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
# This merges to master and develop, creates a tag v<version>
```

### 7. Push Everything

```bash
git push github master develop --tags
```

### 8. CI Builds and Publishes

GitHub Actions on the `master` push:
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
git push github master develop --tags
```

## Version Scheme

- **Major:** Breaking changes to protocols or APIs
- **Minor:** New features, new CLI commands
- **Patch:** Bug fixes, performance improvements

Package revision (`_1`, `_2`) is used when the package template changes but the code version doesn't (e.g., fixing a run script).
