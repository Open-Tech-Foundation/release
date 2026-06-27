<div align="center">

# OTF Release

</div>

> Curated-changelog, manual-bump release CLI for polyglot monorepos.

A single-binary release tool for the OTF monorepo. You write the release notes
(in each package's `[Unreleased]` changelog section), you pick the bumps — `release`
handles the rest: dependency-aware version cascades, internal range upgrades, topological
publishing, and a matrix-gated GitHub release in one `release.yml`.

Unlike commit-driven tools, your hand-written `[Unreleased]` notes are the source of
truth — never inferred from commits. Unlike npm-locked tools, the publishing backend is
adapter-based: **npm, cargo, and a `generic` (bring-your-own-commands, e.g. JSR) today, others later**.

## What it does

- **`version`** (local) — lists packages, you multi-select and choose bumps; cascades to
  internal dependents, upgrades dep ranges, moves `[Unreleased]` → a dated section, then
  opens a release PR. Never touches `main` directly.
- **`publish`** (CI) — publishes changed packages in dependency order, idempotent and
  resumable, attaching prebuilt binaries when a workflow stages them.
- **`init`** — interactive setup: asks which adapters to enable and, per package, its mode
  (`publish` to a registry, or `build-only` → GitHub Release artifacts), build matrix, command,
  and artifacts. Persists `release.toml` and generates one `release.yml` from it.
- **`upgrade`** — instantly upgrades configurations and the `.github/workflows/release.yml` CI pipeline to match the latest CLI version.

## Principles

- **You curate, it ships.** Notes and bumps are human decisions; mechanics are automated.
- **Strict by default.** Commits since the last tag with an empty `[Unreleased]` abort the
  release — no undocumented ships.
- **One committed config.** `release.toml` (written by `init`) is the source of truth; the
  generated `release.yml` and the other commands are derived from it.
- **Dependency-correct.** peerDep dependents mirror the bump; encapsulated ones get a patch.
  Private apps stay buildable but are never published.

## Installation

You can easily install `otf-release` using our automated installation scripts:

**macOS / Linux:**
```bash
curl -fsSL https://raw.githubusercontent.com/Open-Tech-Foundation/release/main/install.sh | bash
```

**Windows (PowerShell):**
```powershell
irm https://raw.githubusercontent.com/Open-Tech-Foundation/release/main/install.ps1 | iex
```

Alternatively, you can compile from source using Cargo:
```bash
cargo install --git https://github.com/Open-Tech-Foundation/release
```

## Documentation

Reference docs live in [`docs/`](./docs/) — start at [`docs/README.md`](./docs/README.md).

- [Architecture](./docs/architecture.md) · [Adapters](./docs/adapters/overview.md) ([npm](./docs/adapters/npm.md) · [cargo](./docs/adapters/cargo.md) · [generic](./docs/adapters/generic.md)) · [Configuration](./docs/configuration.md)
- Commands: [version](./docs/commands/version.md) · [publish](./docs/commands/publish.md) · [init](./docs/commands/init.md)
- [Changelog format](./docs/changelog-format.md) · [Preflight gate](./docs/preflight.md) · [CI workflow](./docs/ci-workflow.md)
- [Implementation plan](./docs/implementation-plan.md) · [Roadmap](./docs/roadmap.md)

## Status

v1 is **functionally complete**: all three commands (`version`, `publish`, `init`) and the **npm**
and **cargo** adapters are implemented and tested (CI on fmt + clippy + test). Setup is
config-driven: `init` is interactive and writes [`release.toml`](./docs/configuration.md) (the
source of truth, no `--adapter` flag), with a per-package **`publish`** vs **`build-only`** mode.
The cargo adapter supports **lockstep** workspaces and a **GitHub-Release binary** distribution
(cross-OS artifacts, no crates.io) — which is how `otf-release` ships itself. See the
[implementation plan](./docs/implementation-plan.md) for the phase-by-phase breakdown. Further
ecosystem adapters (PyPI), pre-releases, and a release-PR bot remain on the
[roadmap](./docs/roadmap.md).
