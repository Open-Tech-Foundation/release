# Changelog

All notable changes to **otf-release** are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/), and this project
adheres to [Semantic Versioning](https://semver.org/). Work in progress lives under
`[Unreleased]` until it ships.

## [Unreleased]

### Added

- **npm adapter** — workspace discovery, format-preserving `package.json` edits (version &
  dependency ranges), `workspace:` link resolution, lockfile refresh, and `is_published` /
  `publish` behind a testable command runner. Keeps the npm gotchas: `--access public`,
  `--no-workspaces`, idempotent `npm view`.
- **cargo adapter** — `Cargo.toml` discovery and format-preserving edits via `toml_edit`,
  `resolve_workspace_links` that injects concrete versions onto path deps for publish,
  `cargo update --workspace` lockfile refresh, and `cargo info` / `cargo publish -p` registry
  calls. Cargo has no peerDep concept, so internal dependents take a patch.
- **Lockstep workspace versioning (cargo)** — crates that inherit `version.workspace = true` are
  bumped by writing the shared `[workspace.package] version` in the root manifest (every
  inheriting crate moves together) and share a **single root `CHANGELOG.md`**. Crates with a
  concrete `[package] version` are still versioned independently.
- **cargo binary-release workflow** — `init --adapter cargo` generates a workflow that
  cross-compiles a target matrix (each on a matching runner) and attaches the binaries to a
  **GitHub Release** `vX.Y.Z`, idempotently — **no crates.io**. This is how `otf-release` ships
  itself; `crates/core` and `crates/adapters` are marked `publish = false` so only the binary is
  released.
- **`--adapter npm|cargo`** selector on the CLI (the `init`-generated workflow passes it
  explicitly; a repo can use both ecosystems).
- **Changelog engine** — Keep a Changelog parser/rewriter: read `[Unreleased]`, move it under a
  dated `## [x.y.z] - YYYY-MM-DD` section, leave a fresh `[Unreleased]`, stub auto-bumped-only
  packages with `_Dependency updates._`.
- **Dependency graph** — topological sort (Kahn, cycle-reporting) and a transitive, max-merged
  bump cascade that mirrors peerDeps and terminates at private leaves.
- **Strict preflight gate** — derives state from the last `name@x.y.z` tag, counts commits since
  it scoped to the package directory, and aborts (all-or-nothing) when changed/selected/
  first-release packages have an empty `[Unreleased]`.
- **`version` command** — interactive local flow: discover → preflight → select + bump →
  cascade → summary → dry-run/confirm → branch guard (clean + on `main`) → apply (versions,
  ranges incl. private apps, changelogs, lockfile) → commit → push → open PR. Side effects are
  behind `Prompt` / `GitOps` / `Forge` traits.
- **`publish` command** — non-interactive CI flow: topological, idempotent (`is_published`
  skip), halt-on-failure with forward-resume; tags and optional GitHub Releases from the dated
  changelog section. Staged binaries attached only when `<artifacts-dir>/<pkg>/` exists.
- **`init` command** — generates a single `.github/workflows/release.yml`, adapter-aware: the
  **npm** shape feeds a registry `publish` job, the **cargo** shape feeds a GitHub-Release job.
  A `build-matrix` job appears only when asset packages are selected. Overwrite guarded by
  `--force`.
- **Docs** — `docs/` reference tree (architecture, commands, adapters, changelog format,
  preflight, CI workflow, roadmap) and a phased implementation plan.
- **CI** — `.github/workflows/ci.yml` running `cargo fmt --check`, `clippy -D warnings`, and the
  test suite.

### Notes

- Nothing has been released yet; the first tagged release will move these notes into a dated
  section. The tool ships **its own binary** via a GitHub Release (cross-OS artifacts), generated
  by `init --adapter cargo` and tagged on merge — **not** published to crates.io. See
  [docs/ci-workflow.md](docs/ci-workflow.md).
