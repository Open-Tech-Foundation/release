# cargo adapter

The Rust / crates.io adapter. Implemented in `crates/adapters/src/cargo.rs`, mirroring the
[npm adapter](./npm.md) over `Cargo.toml` and using `toml_edit` for **format-preserving** edits.
This is an **initial** implementation — usable for crates with concrete versions; see
[limitations](#limitations).

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

## Limitations

- **Inherited versions.** A crate using `version.workspace = true` is **read** (discovery resolves
  it from `[workspace.package] version`) but **cannot be written** — `write_version` errors,
  because independent per-package versioning needs a concrete `[package] version`. **Lockstep**
  workspace versioning is a deferred follow-up. This is exactly why the tool can't yet release
  its *own* crates (they inherit their version) — see [roadmap](../roadmap.md).

## See also

- [adapters/overview.md](./overview.md) — the trait these methods implement.
- [adapters/npm.md](./npm.md) — the sibling adapter this mirrors.
- [roadmap.md](../roadmap.md) — what's left for cargo and other ecosystems.
