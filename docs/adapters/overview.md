# Adapters — the `Adapter` trait

An **adapter** is the ecosystem-specific backend behind which *all* registry and manifest
knowledge lives. The [core](../architecture.md) never reads a manifest directly; it only calls
adapter methods. Two adapters are implemented — [npm](./npm.md) and [cargo](./cargo.md);
PyPI and others remain [deferred](../roadmap.md). Which adapters are active comes from
[`release.toml`](../configuration.md) (there is no `--adapter` flag); the CLI builds one adapter
per enabled ecosystem.

Defined in `crates/core/src/adapter.rs`; implemented in `crates/adapters/`.

## Domain types

```rust
enum Bump { Patch, Minor, Major }            // ordered: max() picks the strongest bump
enum DepKind { Dep, PeerDep, DevDep }        // adapter-specific set (npm-flavored in v1)

struct InternalDep { name: String, kind: DepKind, range: String }

struct Pkg {
    name: String,
    version: String,
    manifest_path: PathBuf,
    changelog_path: PathBuf,
    publishable: bool,                       // false => private app (graph leaf)
    internal_deps: Vec<InternalDep>,
}
```

`Bump` variants are deliberately ordered `Patch < Minor < Major` so the cascade can take
`max()` when a package is reached by several dependency paths.

## The trait

```rust
trait Adapter {
    fn discover_packages(&self) -> Result<Vec<Pkg>>;
    fn write_version(&self, pkg: &Pkg, new: &str) -> Result<()>;
    fn update_dep_range(&self, pkg: &Pkg, dep: &str, new_dep_version: &str) -> Result<()>;
    fn format_range(&self, version: &str) -> String;        // ecosystem range syntax (^x.y.z)
    fn resolve_workspace_links(&self, pkg: &Pkg) -> Result<()>; // inject concrete versions pre-publish
    fn update_lockfile(&self, root: &Path) -> Result<()>;   // refresh lockfile after version writes

    // cascade rule lives HERE, not in shared config
    fn dependent_bump(&self, dep_bump: Bump, kind: &DepKind) -> Bump;

    fn is_published(&self, pkg: &Pkg, version: &str) -> Result<bool>; // registry check
    fn publish(&self, pkg: &Pkg, staged_assets: Option<&Path>) -> Result<()>;
}
```

## Method contracts

| Method | Contract |
| --- | --- |
| `discover_packages` | Enumerate workspace packages, normalize to `Pkg`, populate `internal_deps` (edges to other packages in the repo only). |
| `write_version` | Write `new` as the package's version in its manifest, preserving formatting. |
| `update_dep_range` | Update `pkg`'s declared range for internal dep `dep` to track `new_dep_version`, across all relevant dep kinds. |
| `format_range` | Render a concrete version into the ecosystem's range syntax (npm: `^x.y.z`). |
| `resolve_workspace_links` | Replace workspace links (`workspace:*`, linked) with concrete published versions, immediately before publish. |
| `update_lockfile` | Refresh the lockfile after version writes so a CI install doesn't drift. Called in the same commit as the version writes. |
| `dependent_bump` | **The cascade policy.** Given a dependency's bump and the edge kind, return the dependent's bump. Owned by the adapter, never shared config. |
| `is_published` | Registry lookup: is this exact `version` of `pkg` already published? Makes publish idempotent. |
| `publish` | Publish the package. Attach binaries from `staged_assets` if present; otherwise registry-only. |

## Writing a new adapter

1. Add a module under `crates/adapters/src/` and `impl Adapter for YourAdapter`.
2. Encode the ecosystem's cascade policy in `dependent_bump` (see the cargo notes in
   [roadmap.md](../roadmap.md) — e.g. no peerDep concept likely means *all* internal
   dependents are `Patch`).
3. Implement registry checks and publish mechanics, including any ecosystem gotchas (the npm
   adapter documents several — [npm.md](./npm.md)).
4. Wire it into the CLI in `crates/cli/src/main.rs`. **Do not touch `core`.**

## See also

- [npm.md](./npm.md) — the reference implementation.
- [roadmap.md](../roadmap.md) — deferred adapters and their known constraints.
