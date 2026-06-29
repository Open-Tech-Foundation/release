# cargo adapter

The Rust adapter. Implemented in `crates/adapters/src/cargo.rs`, mirroring the
[npm adapter](./npm.md) over `Cargo.toml` and using `toml_edit` for **format-preserving** edits.
It supports both independent (concrete-version) crates and **lockstep workspaces** (see
[versioning](#versioning--lockstep)).

## Cascade rule (`dependent_bump`)

Cargo has **no peerDep concept**, so every internal dependent simply needs to pick up the new
version requirement:

```
any kind => Patch
```

## Registry check (`is_published`)

```
cargo info <name>@<version>
```

Success → already published → skip. A "could not find" / "not found" error → not published →
publish it. (Best-effort; the exact check may be refined when the real crates.io release is
wired up.)

## Publish (`publish`)

```
cargo publish -p <name>
```

Run from the workspace root. **crates.io is source-only**, so any `staged_assets` (prebuilt
binaries) are **ignored** — binaries are distributed out-of-band (e.g. GitHub Releases), not via
the registry.

## Path-dependency versions (`resolve_workspace_links`)

`cargo publish` **requires a concrete `version`** on path dependencies. Before publishing, the
adapter injects each internal dependency's current version into its entry:

```toml
# before
core = { path = "../core" }
# after resolve_workspace_links
core = { path = "../core", version = "1.4.2" }
```

## Range syntax (`format_range`)

A bare version in `Cargo.toml` already means `^version`, so the adapter writes the plain
version string (`1.2.3`), not `^1.2.3`.

## Discovery & edits

- **Discovery** expands `[workspace] members` globs, reads each member's `[package]` name and
  version, and keeps only dependency edges that point at another workspace member (across
  `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`).
- **`publish = false`** (or an empty `publish = []`) marks a crate as **not publishable** — the
  cargo analogue of npm's `private: true`.
- **Edits** (`write_version`, `update_dep_range`) go through `toml_edit`, preserving comments,
  key order, and spacing.

## Versioning — lockstep

A workspace that declares a shared `[workspace.package] version` and whose crates inherit it
with `version.workspace = true` is versioned in **lockstep**:

- **Discovery** resolves each inheriting crate's version from `[workspace.package] version`, and
  points its changelog at a **single root `CHANGELOG.md`** (not per-crate).
- **`write_version`** on an inheriting crate bumps the shared `[workspace.package] version` in
  the root manifest, so every inheriting crate moves together. A crate with its own concrete
  `[package] version` is still bumped independently in its own manifest.

This is how the tool releases **its own** binary: `crates/core` and `crates/adapters` are marked
`publish = false` (internal libraries), leaving `opentf-release` as the single publishable
package, and a `version` bump rolls the whole workspace from a root `CHANGELOG.md`.

> If two *publishable* crates both inherit the workspace version but are bumped to different
> versions in one run, the last write wins — lockstep assumes they move together. Give a crate a
> concrete `[package] version` to opt it out.

## Binary distribution (no crates.io)

`cargo publish -p <name>` ships **source** to crates.io and is used only when a cargo package
opts into `mode = "publish"` in [`release.toml`](../configuration.md). For a **binary** tool, the
default `mode = "build-only"` makes [`init`](../commands/init.md) generate a workflow that
cross-compiles a target matrix and attaches the binaries to a **GitHub Release** tagged from
`release.toml`'s `tag_format` — no registry involved. That is how `otf-release` itself is distributed:
download the artifact for your OS. See [ci-workflow.md](../ci-workflow.md).

## See also

- [adapters/overview.md](./overview.md) — the trait these methods implement.
- [adapters/npm.md](./npm.md) — the sibling adapter this mirrors.
- [roadmap.md](../roadmap.md) — what's left for cargo and other ecosystems.
