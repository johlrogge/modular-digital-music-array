---
name: conventional-commits
description: Conventional Commits specification adapted for the MDMA project. Defines commit message format, types, scopes, and breaking change conventions.
# NOTE: This skill is project-owned — it contains MDMA-specific scopes and examples.
# Do NOT run `metadev install conventional-commits`; it would overwrite this with the generic template.
---

# Conventional Commits for MDMA

All commits in this repository follow the [Conventional Commits](https://www.conventionalcommits.org/) specification.

## Format

```
<type>(<scope>): <description>

[optional body]

[optional footer(s)]
```

## Types

| Type | Purpose |
|------|---------|
| `feat` | New feature or capability |
| `fix` | Bug fix |
| `refactor` | Code restructuring without behavior change |
| `docs` | Documentation only |
| `test` | Adding or updating tests |
| `ci` | CI/CD pipeline changes |
| `chore` | Maintenance (deps, versions, tooling) |
| `perf` | Performance improvement |
| `build` | Build system or cross-compilation changes |

## Scopes

Scopes map to workspace members and infrastructure:

| Scope | Area |
|-------|------|
| `export` | Audio export / transcoder (`audio_transcoder`, export command) |
| `cli` | CLI interface (`mdma_cli`) |
| `playback` | Playback engine and server (`playback_engine`, `mdma_playback`) |
| `beacon` | Provisioning beacon (`beacon`) |
| `library` | Library service (`mdma_library`, `library_ipc_client`) |
| `gateway` | Gateway routing (`mdma_gateway`) |
| `bandcamp` | Bandcamp source (`mdma_bandcamp`) |
| `console` | Web management console (`mdma_console`) |
| `pkg` | Package building and distribution |
| `ci` | CI workflows and scripts |
| `search` | Library search (`library_search`) |
| `protocol` | IPC protocols (`media_protocol`, `source_protocol`, `event_protocol`) |
| `release` | Version bumps and release process |

Omit scope for changes that span many areas or don't fit a single scope.

## Breaking Changes

Mark breaking changes with `!` after the scope:

```
feat(protocol)!: change library response format to v2
```

Or use a `BREAKING CHANGE:` footer:

```
feat(protocol): change library response format to v2

BREAKING CHANGE: LibraryResponse now uses tagged enum variants
```

## Rules

1. **Imperative mood** in description: "add feature" not "added feature"
2. **Lowercase** description start: `feat(cli): add export command` not `feat(cli): Add export command`
3. **No period** at the end of the description
4. **Body** explains *why*, not *what* (the diff shows what)
5. **72 characters** max for the first line
6. **Single scope** per commit — split multi-scope work into multiple commits

## Examples

From this project's history:

```
feat(export): rewrite audio transcoder to use ffmpeg subprocess
feat(cli): add shell completion generation (bash/zsh/fish/elvish/powershell)
fix(export): reorder AIFF chunks so ID3 appears before SSND
feat(export): preserve cover art in AIFF exports
chore(pkg): bump mdma-console to 0.2.2
ci: split test and build-publish into separate jobs
docs(beacon): update provisioning workflow in README
refactor(library): extract search parser into component
test(export): add format category classification tests
build: add gitflow to devenv packages
chore(release): bump to 0.3.0
```
