# Changelog

All notable changes to **otf-release** are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/), and this project
adheres to [Semantic Versioning](https://semver.org/). Work in progress lives under
`[Unreleased]` until it ships.

## [Unreleased]

### Changed
- **init** — Removed snapshot tag prompting and `snapshot.yml` generation from the setup flow;
  snapshot releases remain available through the dedicated `snapshot` command.
- **changelog config** — Added `changelog_scope` with strict root-level or per-package changelog
  modes, updated `init` to ask only where release notes are maintained, and made package-scope
  GitHub Release bodies combine notes from all configured package changelogs.

### Fixed
- **publish** — Made tag creation and GitHub Release creation idempotent so interrupted publish
  runs can be resumed without failing on already-created remote state.
- **cargo adapter** — Treated missing `cargo info` package results as unpublished and aligned the
  workspace MSRV to Rust 1.82.
- **generic adapter** — Tightened version-field matching so separators must directly follow the
  configured version key.
- **installers** — Prevented Unix and PowerShell install scripts from clobbering an already-running
  `otf-release` binary before the replacement download is ready.

## [0.4.0] - 2026-06-29

### Added
- **config/init** — Added `github_release_notes` to choose GitHub Release body content for
  build-only packages: GitHub-generated notes, the curated `CHANGELOG.md` release section, or a
  semantic-style commit list since the previous matching configured tag. The option is prompted
  during `init` and editable through `config`.

### Changed
- **config** — Normalized this repo's `release.toml` by writing default global settings
  explicitly and expanding build targets into standard TOML tables.

### Fixed
- **generic adapter** — Cleaned up Cargo manifest version-field matching to satisfy clippy without
  changing behavior.

## [0.3.0] - 2026-06-29

### Added
- **config** — Added global `tag_format` to `release.toml` (default `v{version}`) and exposed it
  in `init` and `config`, so preflight, publish, and generated GitHub Release jobs use the repo's
  configured tag convention instead of an implicit package-scoped format.

### Fixed
- **version** — Modified `git checkout -b` to `git checkout -B` so that release branch creation gracefully handles previously abandoned branches by resetting them instead of crashing.
- **version** — Removed the startup `gh` confirmation prompt and moved confirmation to a final
  review that shows the computed plan and changed-file stats before commit/push/PR.
- **init/npm** — npm workspace discovery now skips workspace manifests that are not release
  packages because they lack `name` or `version`, prints each skipped manifest with the reason,
  and still fails on malformed `package.json` files.
- **generic adapter** — `Cargo.toml` manifests with `version_field = "version"` now read and bump
  `[workspace.package].version` (or `[package].version`) instead of failing on root Cargo
  workspaces.

## [0.2.0] - 2026-06-28

### Added
- **version** — Added interactive pre-release channel selection (stable, alpha, beta, rc). Choosing a pre-release channel unlocks the new `prerelease` bump strategy for iterating tags, and automatically formats transitions from stable to pre-release (e.g., `1.0.0` to `1.1.0-beta.0`).
- **config** — Added global lifecycle hooks (`pre_version`, `post_version`, `pre_publish`, `post_publish`) to `release.toml`, allowing users to execute custom shell scripts across OS environments during critical release orchestration steps.
- **init** — Emits all known targets in the generated `.github/workflows/release.yml` matrix with unselected ones commented out, allowing users to easily toggle builds on and off.
- **core** — Added automated `install.sh` and `install.ps1` scripts for seamless downloads of GitHub Release assets.
- **docs** — Redesigned the `README.md` into a polished landing page with a commands table and a visual Mermaid workflow diagram.

### Changed
- **core** — Refactored CI target matrix configurations from Rust-specific strings to generic `Target` objects (`{ name, arch }`) making the schema fully language-agnostic.

## [0.1.0] - 2026-06-27

### Added
- **init** — The `generic` adapter wizard is now much smarter: it explicitly asks for the `mode` using a Select menu, prompts for the binary name for Rust projects, provides matrix-aware default commands (`rustup target add ...`), and automatically extracts versions from `.toml` files using `grep`.
- **init** — All `y/n` typing prompts have been replaced with arrow-key `Select` components for a smoother, typing-free UX.
- **upgrade** — A brand new top-level subcommand that updates configurations and regenerates `.github/workflows/release.yml` to match the latest CLI version, allowing you to easily adopt new CI pipeline features (like the new `check-release` job that guards expensive cross-compilation).

### Changed
- **project** — Renamed the workspace from `opentf-release` to `otf-release` across all `Cargo.toml` files, updated authors to "OTF Contributors", and updated the GitHub repository URL.

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
- **cargo binary-release workflow** — a `build-only` cargo package makes `init` generate a workflow
  that cross-compiles a target matrix (each on a matching runner) and attaches the binaries to a
  **GitHub Release** `vX.Y.Z`, idempotently — **no crates.io**. This is how `otf-release` ships
  itself; `crates/core` and `crates/adapters` are marked `publish = false` so only the binary is
  released.
- **`release.toml` — the committed source of truth.** A new `config` module persists which
  ecosystems are enabled and the per-package build steps. Every command reads it; there is **no
  `--adapter` flag**. See [docs/configuration.md](docs/configuration.md).
- **Per-package `publish` vs `build-only` mode.** `publish` builds then pushes to the ecosystem's
  registry; `build-only` builds then attaches the artifacts to a **GitHub Release** (no registry
  push). A polyglot repo can mix modes and adapters freely.
- **`generic` adapter** — a bring-your-own-commands ecosystem for registries the tool doesn't
  natively support (e.g. Deno's JSR). The version is read/bumped from a manifest you name
  (`manifest` + `version_field`, the git-tag source); an optional `publish` command (e.g.
  `npx jsr publish`) makes it `publish` mode, otherwise it's build-only. The generated workflow
  injects no toolchain for generic build steps and marks registry toolchain/secrets `# edit me`.
- **Generic package auto-discovery** — instead of hand-typing a manifest path, `init` scans the
  repo for recognized manifests carrying a version and presents them in a multi-select to import,
  inferring each package's name + version (single project or monorepo). Generic is the *custom-way*
  path, so the scan spans **all** project types — `Cargo.toml`, `package.json`, `deno.json`/
  `jsr.json`, `pyproject.toml`, `composer.json`, `gleam.toml`, `mix.exs` — not just ecosystems
  without a native adapter (cargo-workspace-inherited versions, build output, and hidden dirs are
  skipped). You can still add packages by hand. See `crates/core/src/discover.rs`.
- **Single unified `publish` job** — the generated workflow now emits one `publish` job (running
  `otf-release publish` once across all enabled adapters) instead of per-registry jobs, setting up
  only the toolchains the active registries need.
- **Modern interactive prompts** — `init` and `version` now use arrow-key select, spacebar
  multi-select, and confirm prompts (via `inquire`) instead of typing numbers.
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
  **Skips `build-only` packages** (they ship via the GitHub Release the workflow creates).
- **`init` command** — interactive setup (no flags): multi-select adapters (`npm`, `crates.io`),
  then per package its mode, build matrix, command, and artifacts. Persists `release.toml` and
  generates a single `.github/workflows/release.yml` from it — a `build-<pkg>` job per build step
  feeding an `npm-publish` / `cargo-publish` job (registry) and/or a `github-release` job
  (build-only). Both writes guarded by `--force`.
- **Docs** — `docs/` reference tree (architecture, commands, adapters, changelog format,
  preflight, CI workflow, roadmap) and a phased implementation plan.
- **CI** — `.github/workflows/ci.yml` running `cargo fmt --check`, `clippy -D warnings`, and the
  test suite.

### Notes

- Nothing has been released yet; the first tagged release will move these notes into a dated
  section. The tool ships **its own binary** via a GitHub Release (cross-OS artifacts), generated
  by `init --adapter cargo` and tagged on merge — **not** published to crates.io. See
  [docs/ci-workflow.md](docs/ci-workflow.md).
