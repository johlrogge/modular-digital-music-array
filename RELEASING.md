# MDMA Release Process

This document describes the release workflow for MDMA, including feature development,
regular releases, and hotfixes.

---

## Versioning Rules

MDMA uses [Semantic Versioning](https://semver.org/):

| Increment | When |
|-----------|------|
| **Major** | Breaking changes to IPC protocols or public APIs |
| **Minor** | New user-facing features, new CLI commands, new services |
| **Patch** | Bug fixes, performance improvements, internal refactors |

### Where versions live

- **Workspace version** — `[package] version` in root `Cargo.toml` (since cargo-polylith 0.10.0,
  workspace package metadata lives here, not in `Polylith.toml [workspace.package]`). Applies to
  all packages that use `version.workspace = true`. Bump this on every release.
- **Independently versioned packages** — `bases/*/Cargo.toml` entries that have a hardcoded
  `version = "x.y.z"` (not `version.workspace = true`) must be bumped individually based on
  what changed in that package.
- **xbps templates** — `void-packages/srcpkgs/*/template` each has its own `version=` field.
  This must match the corresponding `Cargo.toml` version (workspace or independent).
- **Package revision** — reset `revision=1` whenever `version=` is bumped. Increment revision
  (e.g., `_2`) when the template changes but the code version does not (e.g., fixing a run
  script or INSTALL hook).


---

## Feature Workflow

Normal development happens on feature branches off `develop`.

Use `mcp__git-write__git_flow` with `action: "feature start"` / `"feature finish"`.

Dispatch agents in this order for each feature:

1. **glenn-c** — validate the feature aligns with the roadmap and product vision
2. **code-minion** — implement the feature (uses `mcp__rust-codebase__hygiene_report` before reporting)
3. **architect** — review all changed files for duplication, fragility, missed reuse
4. If architect finds issues → **code-minion** to fix, re-run architect until clean
5. **commit** — commit with conventional commit message

Push develop:
```bash
git push github develop
```

---

## Release Checklist

### 1. Start the release branch

Use `mcp__git-flow-release__gitflow_release_start` with the target version.

### 2. Bump the workspace version

Edit root `Cargo.toml` (since cargo-polylith 0.10.0, workspace package metadata lives here, not in `Polylith.toml`):

```toml
[package]
version = "<new-version>"
```

### 3. Bump independently versioned packages

For each `bases/*/Cargo.toml` and any component with a hardcoded `version = "x.y.z"`,
bump the version individually based on what changed in that package. Only packages using
`version.workspace = true` inherit the workspace version automatically.

### 4. Bump xbps template versions

Update `version=` in each template under `void-packages/srcpkgs/*/template`. The value
must match the corresponding `Cargo.toml` version. Reset `revision=1` on every version bump.

Packages to check:

```
void-packages/srcpkgs/beacon/template
void-packages/srcpkgs/mdma-library/template
void-packages/srcpkgs/mdma-audio/template
void-packages/srcpkgs/mdma-playback/template
void-packages/srcpkgs/mdma-gateway/template
void-packages/srcpkgs/mdma-bandcamp/template
void-packages/srcpkgs/mdma-console/template
void-packages/srcpkgs/mdma-acid/template
```

### 5. Update documentation

Dispatch **documenter** to update all README.md files with the new version.

### 6. Commit the version bumps

Dispatch **commit** with message:

```
chore(release): bump to <version>
```

### 7. Finish the release

Use `mcp__git-flow-release__gitflow_release_finish` with the version and a tag annotation
message summarising what the release contains. This merges to `master` and `develop` and
creates the tag.

### 8. Push everything

```bash
git push github master develop --tags
```

### 9. CI builds and publishes

GitHub Actions triggers on the `master` push:

1. Runs tests across all workspace members
2. Cross-compiles all service binaries for `aarch64-unknown-linux-gnu`
3. Builds `.xbps` packages via `xbps-src`
4. Signs packages
5. Publishes to GitHub Pages package repository at:
   `https://johlrogge.github.io/modular-digital-music-array/aarch64`

Monitor the Actions run and verify it completes without errors before proceeding.

### 10. Deploy and verify on Pi

Dispatch **devops** to update packages (use `mcp__ssh__ssh_run` with host `pi`):

```
sudo xbps-install -Su
```

After install, services do NOT restart automatically. Restart in dependency order using
`mcp__ssh__ssh_run`:

```
sudo sv restart mdma-library
sudo sv restart mdma-audio
sudo sv restart mdma-playback
sudo sv restart mdma-gateway
sudo sv restart mdma-bandcamp
sudo sv restart mdma-console
```

Verify installed versions:

```
xbps-query -l | grep mdma
```

### 11. Smoke test

Dispatch **test** agent to verify the deployed version is functional.

---

## Hotfix Checklist

For urgent fixes that cannot wait for a normal release cycle.

### 1. Start the hotfix branch

Use `mcp__git-flow-release__gitflow_hotfix_start` with the patch version.
Hotfix branches cut from `master`, not `develop`.

### 2. Implement the fix

Dispatch **code-minion** with a focused, minimal fix. Then dispatch **architect** to review.

### 3. Bump versions

Same as release checklist steps 2–4: bump root `Cargo.toml` (`[package] version`), any independently versioned
packages that changed, and the corresponding xbps templates.

### 4. Commit

Dispatch **commit** with a conventional commit message describing the fix:

```
fix(<scope>): <description of the fix>
```

Followed by a second commit for version bumps:

```
chore(release): bump to <version>
```

### 5. Finish the hotfix

Use `mcp__git-flow-release__gitflow_hotfix_finish` with the version and a tag annotation
message. Merges to `master` AND `develop`, creates the tag.

### 6. Push and deploy

```bash
git push github master develop --tags
```

Wait for CI to build and publish, then dispatch **devops** to deploy as per steps 10–11
of the release checklist.

---

## Agent Dispatch Summary

| Step | Agent | MCP / Tool |
|------|-------|------------|
| Planning | **glenn-c** | Validate feature scope against roadmap |
| Implementation | **code-minion** | `mcp__rust-codebase__hygiene_report` |
| Review | **architect** | Read-only; repeat until clean |
| Documentation | **documenter** | Update READMEs with new version |
| Commit | **commit** | `mcp__git-write__git_commit` |
| Deploy | **devops** | `mcp__just-deploy__just_run`, `mcp__ssh__ssh_run` |
| Packaging (CI) | **ci** | Triggered automatically on `master` push |

---

## Branch Model Reference

| Branch | Purpose |
|--------|---------|
| `master` | Production — CI builds packages from here |
| `develop` | Integration branch — all features merge here |
| `feature/<name>` | Feature development, branches from `develop` |
| `release/<version>` | Release preparation, branches from `develop` |
| `hotfix/<version>` | Urgent fixes, branches from `master` |

Remote name is `github` (not `origin`).

---

## Commit Message Format

All commits follow [Conventional Commits](https://www.conventionalcommits.org/).
See `.claude/skills/conventional-commits/SKILL.md` for MDMA-specific scopes and examples.

```
<type>(<scope>): <description>

# Examples:
feat(playback): add crossfade between tracks
fix(library): handle empty inbox on first boot
chore(release): bump to 0.9.0
docs(beacon): update provisioning workflow
```

---

## Troubleshooting

### Services not responding after deploy

Services must be restarted manually after `xbps-install -Su`. Use `mcp__ssh__ssh_run` with
host `pi` in dependency order: library → audio → playback → gateway → bandcamp → console.

### Package build fails in CI

Check the GitHub Actions run log. Common causes:
- xbps template `version=` does not match `Cargo.toml` version
- `checksum=` field outdated (set `checksum="SKIP"` for development builds)
- Cross-compilation target misconfigured

### Version mismatch between Pi and expected

Via `mcp__ssh__ssh_run` with host `pi`:

```
xbps-query -S mdma-playback   # show installed version and details
xbps-install -S               # force re-sync repository index
xbps-install -Su              # update to latest
```

### Rollback a bad deploy

Via `mcp__ssh__ssh_run` with host `pi`:

```
sudo xbps-install -f mdma-playback-0.7.0_1
```
