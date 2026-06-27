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

## Commands

| Command | Usage | Description |
|---------|-------|-------------|
| **`init`** | `otf-release init` | Interactive setup: configure ecosystems, build matrices, and artifacts. Generates `release.toml` and `release.yml`. |
| **`version`** | `otf-release version` | Interactive local release: choose bumps, cascade dependencies, write changelogs, and automatically open a Release PR. |
| **`publish`** | `otf-release publish` | Non-interactive CI flow: publishes changed packages in topological order, attaching staged build artifacts. |
| **`upgrade`** | `otf-release upgrade` | Upgrades your local `release.toml` and regenerates your CI pipeline to match the latest CLI version features. |

## Workflow

1. **Init:** Run `otf-release init` once to configure ecosystems, build matrices, and instantly generate your `.github/workflows/release.yml` pipeline.
2. **Curate:** Write your release notes in each package's `[Unreleased]` changelog section as you develop features.
3. **Version:** Run `otf-release version` locally. It walks you through selecting bumps, safely cascades versions, and opens a curated Release PR.
4. **Merge:** Review the PR and merge it into `main`. The tool guards against empty changelogs to ensure no undocumented ships occur.
5. **Publish:** The generated `release.yml` GitHub Action triggers automatically, cross-compiles artifacts natively via the `release.toml` configuration, and publishes them.

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


