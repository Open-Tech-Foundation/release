# OTF Release — Documentation

Reference documentation for `otf-release`, the curated-changelog, manual-bump release CLI
for the OpenTF monorepo.

> **Status:** v1 is implemented and tested — `version`, `publish`, and `init`, with the **npm**
> and **cargo** adapters. Setup is config-driven: `init` writes [`release.toml`](./configuration.md)
> (the source of truth), with a per-package `publish` vs `build-only` mode. The cargo path supports
> lockstep workspaces and a GitHub-Release binary distribution (how `otf-release` ships itself).
> [`implementation-plan.md`](./implementation-plan.md) tracks the phase-by-phase build.

## Start here

- **New to the tool?** Read the root [`README.md`](../README.md) for the elevator pitch, then
  [`architecture.md`](./architecture.md).
- **Setting up a repo?** [`commands/init.md`](./commands/init.md) → [`configuration.md`](./configuration.md) → [`ci-workflow.md`](./ci-workflow.md).
- **Cutting a release?** [`commands/version.md`](./commands/version.md) (local) then
  [`commands/publish.md`](./commands/publish.md) (CI).
- **Writing a new adapter?** [`adapters/overview.md`](./adapters/overview.md).

## Contents

| Doc | What it covers |
| --- | --- |
| [architecture.md](./architecture.md) | Crate layout, the core/adapter seam, data flow. |
| [commands/version.md](./commands/version.md) | The interactive, local `version` command. |
| [commands/publish.md](./commands/publish.md) | The non-interactive, CI `publish` command. |
| [commands/init.md](./commands/init.md) | Interactive setup: writes `release.toml`, generates `release.yml`. |
| [configuration.md](./configuration.md) | The `release.toml` schema — the committed source of truth. |
| [adapters/overview.md](./adapters/overview.md) | The `Adapter` trait and domain types. |
| [adapters/npm.md](./adapters/npm.md) | The npm adapter — rules, gotchas, commands. |
| [adapters/cargo.md](./adapters/cargo.md) | The cargo adapter — rules and Rust-specific limits. |
| [adapters/generic.md](./adapters/generic.md) | The generic adapter — bring-your-own-commands (e.g. JSR). |
| [changelog-format.md](./changelog-format.md) | Keep a Changelog conventions and rewrite rules. |
| [preflight.md](./preflight.md) | The strict, all-or-nothing compliance gate. |
| [ci-workflow.md](./ci-workflow.md) | The single `release.yml` model. |
| [roadmap.md](./roadmap.md) | Deferred / out-of-scope items. |
| [implementation-plan.md](./implementation-plan.md) | Phased build plan with acceptance criteria. |

## Glossary

- **Publishable package** — a library or asset package that gets versioned, tagged, and
  pushed to a registry.
- **Private app** — a non-publishable package; a **graph leaf**. Never versioned or
  published, but its internal dependency ranges are still updated so it stays buildable.
- **Asset package** — a publishable package that also ships prebuilt binary artifacts
  (cross-compiled in CI and attached at publish time). A first-class package, **not** a
  guarded special case.
- **Cascade** — propagating a bump from a package to its internal dependents, transitively.
- **Adapter** — the ecosystem-specific backend (npm in v1) behind which all registry and
  manifest knowledge lives.

## Conventions used in these docs

- The invoked binary is `otf-release`; the published npm package is `@opentf/release`.
- Code/identifier references point at `crates/<crate>/src/<module>.rs`.
- "v1" = the current milestone: **npm + cargo** adapters, config-driven via `release.toml`, no
  pre-releases, local `version` → manual PR.
