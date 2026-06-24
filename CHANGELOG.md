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
- **cargo adapter (initial)** — `Cargo.toml` discovery and format-preserving edits via
  `toml_edit`, `resolve_workspace_links` that injects concrete versions onto path deps for
  publish, `cargo update --workspace` lockfile refresh, and `cargo info` / `cargo publish -p`
  registry calls. Cargo has no peerDep concept, so internal dependents take a patch. Crates
  that inherit `version.workspace = true` are read but not written (independent versioning
  requires a concrete `[package] version`; lockstep is deferred). crates.io is source-only, so
  staged binaries are ignored on publish.
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
- **`init` command** — generates a single `.github/workflows/release.yml`; a `build-matrix` job
  appears only when asset packages are selected, and the `publish` job then downloads artifacts
  and runs `otf-release publish --artifacts-dir .artifacts`. Overwrite guarded by `--force`.
- **Docs** — `docs/` reference tree (architecture, commands, adapters, changelog format,
  preflight, CI workflow, roadmap) and a phased implementation plan.
- **CI** — `.github/workflows/ci.yml` running `cargo fmt --check`, `clippy -D warnings`, and the
  test suite.

### Notes

- Nothing has been released yet; the first tagged release will move these notes into a dated
  section. Releasing the tool's own crates to crates.io (via the cargo adapter) is the next
  step — see [docs/roadmap.md](docs/roadmap.md).
