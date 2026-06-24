# Architecture

`otf-release` is a single static binary (Rust) split into a registry-agnostic **core** and
one or more **adapters**. The core orchestrates a release; an adapter knows how one ecosystem
(npm, in v1) reads manifests, formats version ranges, talks to a registry, and publishes.

## Design rules

1. **Core never reads a manifest.** No `package.json`, no `Cargo.toml`. The core only ever
   talks to an [`Adapter`](./adapters/overview.md). This is what keeps it polyglot.
2. **The adapter owns ecosystem policy**, including the cascade rule (`dependent_bump`) and
   range syntax (`format_range`) — not a shared config file.
3. **Stateless / config-light.** Nothing is persisted between runs. State is derived from
   disk (manifests, changelogs, `.artifacts/`) and the registry/git (tags). The generated
   `release.yml` is the single source of truth for CI.
4. **Notes are curated, never inferred.** The hand-written `[Unreleased]` changelog section
   is the source of truth for release notes. Bumps are chosen by a human.

## Crate layout

```
Cargo.toml                      # workspace
crates/
  core/      opentf-release-core         # ecosystem-agnostic orchestration (lib)
    src/
      lib.rs
      adapter.rs    # Adapter trait + domain types (Pkg, Bump, DepKind, InternalDep)
      graph.rs      # dependency graph: topo sort + bump cascade engine
      changelog.rs  # Keep a Changelog parse/rewrite
      preflight.rs  # strict compliance gate
      summary.rs    # confirmation / dry-run rendering
      version.rs    # `version` command orchestration
      publish.rs    # `publish` command orchestration
      init.rs       # `release.yml` generator
  adapters/  opentf-release-adapters     # registry adapters (lib)
    src/
      lib.rs
      npm.rs        # the only implemented adapter in v1
  cli/       opentf-release              # binary `otf-release` (clap)
    src/
      main.rs
```

### Dependency direction

```
cli ──▶ core ◀── adapters
  └───────────────▶ adapters
```

- `core` defines the `Adapter` trait and all domain types. It depends on nothing internal.
- `adapters` depends on `core` (it implements the trait).
- `cli` depends on both: it constructs the concrete `NpmAdapter` and hands it to the core
  command functions as `&dyn Adapter`.

This direction means a new adapter is added **without touching `core`**.

## Domain types

Defined in `crates/core/src/adapter.rs`:

- **`Pkg`** — a discovered package normalized to ecosystem-agnostic terms (name, version,
  manifest/changelog paths, `publishable` flag, internal deps).
- **`Bump`** — `Patch < Minor < Major`, ordered so `max()` picks the strongest bump when a
  package is hit by several cascade paths.
- **`DepKind`** — `Dep | PeerDep | DevDep` (adapter-specific set; npm-flavored in v1).
- **`InternalDep`** — an edge to another package in the same monorepo, with its declared range.

## Data flow

### `version` (local)

```
discover ─▶ preflight ─▶ prompt ─▶ cascade ─▶ summary/confirm
   ─▶ branch ─▶ apply (versions, ranges, changelogs) ─▶ lockfile
   ─▶ commit ─▶ push ─▶ open PR
```

### `publish` (CI)

```
discover ─▶ filter (publishable & !is_published) ─▶ topo sort
   ─▶ for each: resolve_workspace_links ─▶ publish ─▶ tag + GH Release
   (halt on first failure; re-run resumes forward)
```

See [`commands/version.md`](./commands/version.md) and
[`commands/publish.md`](./commands/publish.md) for the step-by-step contracts.

## Why a workspace (not one crate)

Splitting `core` from `adapters` enforces rule #1 at the **compiler** level: `core` literally
cannot depend on the npm adapter, so it cannot reach into a `package.json` by accident. The
`cli` crate is the only place that names a concrete adapter.
