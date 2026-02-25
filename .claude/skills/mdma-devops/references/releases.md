# MDMA Release Process

All releases use git-flow commands. Never use manual merge/tag for releases.

## Release Workflow

### 1. Finish pending features

```bash
git flow feature finish <name>
```

Merges feature to `develop` with `--no-ff`, deletes feature branch.

### 2. Update ROADMAP.md

Mark completed priorities on `develop`. Commit.

### 3. Start release

```bash
git flow release start <version>
```

Creates `release/<version>` from `develop`.

### 4. Release branch work

On the release branch:
- Bump `workspace.package.version` in root `Cargo.toml`
- Bump `version` in any base `Cargo.toml` that has its own version (e.g., `mdma-console`)
- Run `cargo build` to regenerate `Cargo.lock`
- Run `cargo test` to verify
- Update changelogs or release notes if applicable
- Commit: `chore(release): prepare <version>`

### 5. Finish release

```bash
git flow release finish <version>
```

This:
- Merges release branch to `master`
- Tags `v<version>` on master
- Merges release branch back to `develop`
- Deletes the release branch

Tag message format: `v<version>: <summary>`

### 6. Push

```bash
git push github master develop --tags
```

### 7. Verify CI

Check GitHub Actions:
- Test job passes on all branches
- Build-and-publish job runs on master: builds .xbps packages, publishes to GitHub Pages repo

### 8. Deploy (optional)

If not already deployed during development:
```bash
just deploy-dev
```

### 9. Smoke test

Run the test agent to verify services on the Pi.

## Hotfix Workflow

```bash
git flow hotfix start <version>
# fix the issue, commit
git flow hotfix finish <version>
git push github master develop --tags
```

## Rules

- **Always** use `git flow` commands for feature/release/hotfix branches
- **Never** manually merge to master or develop
- **Never** manually create release tags
- Production branch: `master`
- Development branch: `develop`
