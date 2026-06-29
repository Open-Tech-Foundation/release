# OTF Release — Documentation

Reference documentation for `otf-release`, the manual-bump, changelog-aware release CLI for
polyglot monorepos.

> **Status:** The implemented command surface includes `init`, `version`, `publish`, `snapshot`,
> `config`, `upgrade`, and `self-update`. The implemented adapters are **npm**, **cargo**, and
> **generic**. GitHub is the only fully implemented forge provider.

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
| [commands/config.md](./commands/config.md) | Interactive editor for `release.toml`. |
| [configuration.md](./configuration.md) | The `release.toml` schema — the committed source of truth. |
| [adapters/overview.md](./adapters/overview.md) | The `Adapter` trait and domain types. |
| [adapters/npm.md](./adapters/npm.md) | The npm adapter — rules, gotchas, commands. |
| [adapters/cargo.md](./adapters/cargo.md) | The cargo adapter — rules and Rust-specific limits. |
| [adapters/generic.md](./adapters/generic.md) | The generic adapter — bring-your-own-commands (e.g. JSR). |
| [changelog-format.md](./changelog-format.md) | Keep a Changelog conventions and rewrite rules. |
| [preflight.md](./preflight.md) | The strict, all-or-nothing compliance gate. |
| [ci-workflow.md](./ci-workflow.md) | The single `release.yml` model. |
| [roadmap.md](./roadmap.md) | Known gaps and upcoming work. |
| [implementation-plan.md](./implementation-plan.md) | Historical phased build plan; useful context, not the current source of truth. |

## Glossary

- **Publishable package** — a library or asset package that gets versioned, tagged, and
  pushed to a registry.
- **Private app** — a non-publishable package; a **graph leaf**. Never versioned or
  published, but its internal dependency ranges are still updated so it stays buildable.
- **Asset package** — a publishable package that also ships prebuilt binary artifacts
  (cross-compiled in CI and attached at publish time). A first-class package, **not** a
  guarded special case.
- **Cascade** — propagating a bump from a package to its internal dependents, transitively.
- **Adapter** — the ecosystem-specific backend behind which registry and manifest knowledge
  lives.

## Conventions used in these docs

- The invoked binary is `otf-release`; the published npm package is `@opentf/release`.
- Code/identifier references point at `crates/<crate>/src/<module>.rs`.
- "Current" = npm + cargo + generic adapters, config-driven via `release.toml`, local
  `version` → release PR → CI `publish`.
