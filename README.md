# OTF Release

> Manual-bump, changelog-aware release CLI for polyglot monorepos.

`otf-release` is a single Rust binary that helps a repo move from curated release notes to a
release PR, then to CI-driven publishing. It supports npm workspaces, Cargo workspaces, and
generic manifest-based packages through a committed `release.toml`.

The core rule is simple: humans choose what to release and how much to bump; the tool handles
dependency cascades, manifest edits, changelog updates, tags, publishing order, and generated
GitHub workflows.

## ⚙️ Installation

**macOS / Linux**

```bash
curl -fsSL https://raw.githubusercontent.com/Open-Tech-Foundation/release/main/install.sh | bash
```

**Windows PowerShell**

```powershell
irm https://raw.githubusercontent.com/Open-Tech-Foundation/release/main/install.ps1 | iex
```

**From source**

```bash
cargo install --git https://github.com/Open-Tech-Foundation/release
```

## 🧭 Command Surface

| Command | Status | What it does |
| --- | --- | --- |
| `otf-release init` | ✅ Supported | Interactive setup. Writes `release.toml`, `.github/workflows/release.yml`, and `.github/workflows/snapshot.yml`. |
| `otf-release version` | ✅ Supported | Interactive local release flow. Select packages, choose bumps, cascade dependents, update manifests/changelogs, push a release branch, and open a PR. |
| `otf-release publish` | ✅ Supported | CI-oriented publish flow. Publishes in dependency order, skips already-published versions, creates `name@version` tags, and creates package releases from notes. |
| `otf-release snapshot` | 🧪 Experimental | Creates hash-based prerelease versions such as `1.2.3-snapshot.a1b2c3d` and publishes them from CI. |
| `otf-release config` | ✅ Supported | Interactive editor for hooks, ecosystems, package build fields, generic package fields, provider, snapshot tag, and changelog strategy. |
| `otf-release upgrade` | ◐ Partial | Regenerates `release.yml` from the current `release.toml`. |
| `otf-release self-update` | ✅ Supported | Checks GitHub Releases and reruns the install script when a newer CLI version exists. |

## ✅ Feature Matrix

| Area | Supported now | Notes |
| --- | --- | --- |
| npm adapter | ✅ | Discovers npm workspaces, preserves dependency range operators, resolves `workspace:*`, checks `npm view`, publishes with `npm publish --access public --no-workspaces`. |
| Cargo adapter | ✅ | Discovers Cargo workspaces, supports concrete crate versions and `version.workspace = true`, updates path dependency versions, checks `cargo info`, publishes with `cargo publish -p`. |
| Generic adapter | ✅ | Versions a configured manifest field and optionally runs a configured publish command for registries such as JSR. Idempotency is tag-based. |
| Polyglot versioning | ✅ | `version` runs as one release transaction across all enabled adapters. |
| Polyglot publishing | ✅ | `publish` loops enabled adapters and publishes each ecosystem in dependency order. |
| Dependency cascades | ✅ | Adapter-owned rules. npm peer dependencies mirror the dependency bump; normal deps patch dependents. Cargo/generic dependents patch. |
| Private packages/apps | ✅ | Never versioned or published; internal ranges are still updated so apps remain buildable. |
| Curated changelog mode | ✅ | Uses each package's `[Unreleased]` section as the release-note source. |
| Generated changelog mode | ✅ | Builds notes from git commit messages since the last package tag and prepends generated notes to `CHANGELOG.md`. |
| Prereleases | ✅ | Supports stable bumps, channel entry (`alpha`, `beta`, `rc`), channel iteration, channel switching, and graduation to stable. |
| Build-only packages | ✅ | CI can build artifacts and attach them to a GitHub Release instead of publishing to a registry. |
| Lifecycle hooks | ✅ | `pre_version`, `post_version`, `pre_publish`, and `post_publish` run from `release.toml`. |
| GitHub workflow generation | ✅ | Generates release and snapshot workflows from `release.toml`; intended as editable scaffolds. |
| Git providers | GitHub only | Config has a `provider` field, but only GitHub PR/release behavior is implemented. |

## ⚠️ Known Gaps

| Gap | Impact |
| --- | --- |
| `snapshot` is experimental. | Multi-adapter semantics, generated notes, rollback expectations, and workflow polish need more hardening. |
| Only GitHub is implemented. | GitLab, Bitbucket, Gitea, and Codeberg are future work. |

## 🔁 Release Flow

```mermaid
flowchart TD
    Init["⚙️ Init<br/>Generate release.toml and workflows"]
    Curate["📝 Prepare notes<br/>Curated CHANGELOG or generated commits"]
    PreVersion{"pre_version hooks"}
    Version["🏷️ Version<br/>Choose bumps, cascade, update files"]
    PostVersion{"post_version hooks"}
    PR["🔍 Release PR<br/>Review and merge"]
    PrePublish{"pre_publish hooks"}
    Publish["🚀 Publish<br/>CI publishes registries and/or artifacts"]
    PostPublish{"post_publish hooks"}

    Init --> Curate --> PreVersion --> Version --> PostVersion --> PR --> PrePublish --> Publish --> PostPublish
```

## 📄 License

MIT. See [LICENSE](LICENSE).
