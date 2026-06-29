# npm adapter

Implemented in `crates/adapters/src/npm/`. The rules and gotchas below are baked into the npm
adapter so the core release flow can stay ecosystem-agnostic.

## Workspace discovery

The adapter expands the root `package.json` `workspaces` globs and treats a member as a release
package only when its `package.json` has string `name` and `version` fields. During `init`,
workspace manifests missing either field are skipped and printed with the reason, which keeps
fixture, benchmark, and tool-only folders from aborting setup.

Malformed JSON still fails discovery. That is a broken workspace manifest, not a non-release
package.

## Cascade rule (`dependent_bump`)

```
PeerDep  => mirror(dep_bump)   // a peerDep dependent takes the same bump as its dependency
else     => Patch              // Dep / DevDep dependents get a patch
```

A breaking change in a package forces a matching breaking bump in anything that lists it as a
**peer** dependency; everything else only needs a patch to pick up the new internal range.

## Registry check (`is_published`)

```
npm view <name>@<version> version
```

If the command **succeeds**, that exact version already exists → **skip** (this is what makes
[`publish`](../commands/publish.md) idempotent and resumable). A 404 → not published → publish it.

## Publish (`publish`)

```
npm publish --access public --no-workspaces
```

Two flags, both load-bearing:

- **`--access public`** — required for a **scoped** package's *first* publish (`@opentf/*`
  packages default to restricted otherwise).
- **`--no-workspaces`** — required because the **repo root is a private workspace**. Without
  this flag npm runs in workspace mode and **skips the package even when invoked from the
  package's own directory**.

## Workspace links (`resolve_workspace_links`)

Before publishing, rewrite `workspace:*` (and other linked internal deps) to the **concrete
published version**. npm does **not** do this automatically, so without it consumers would get
an unresolvable `workspace:*` range.

## Lockfile (`update_lockfile`)

After version writes, refresh `package-lock.json` so a CI `npm ci` does not drift from the
manifests. This runs in the **same commit** as the version changes (see
[version step 9](../commands/version.md)).

## Range syntax (`format_range`)

```
1.2.3  →  ^1.2.3
```

## No `private:true` guard — and why

The current pre-tool workflow sets `private: true` on asset packages purely to **hide them
from `changeset publish`**, then flips the flag off to publish. `otf-release` understands asset
packages natively:

> Asset packages are **normal publishable packages** with a binary target. **No guard, no
> flip-off step.** Topological publish handles "asset package depends on freshly-published
> libraries" by ordering libs first, asset package after — in one run.

This is the single biggest behavioral difference from the changesets workaround. See
[ci-workflow.md](../ci-workflow.md).

## Gotchas summary

| Keep | Why |
| --- | --- |
| Idempotent `npm view` skip | Resumable publish after partial failure. |
| `--no-workspaces` | Private root workspace would otherwise skip the package. |
| `--access public` | Scoped package first publish. |
| Brotli compression via Node `zlib` | No dependency on a runner-side CLI. |

| Drop | Why |
| --- | --- |
| `private:true` guard flip | Only existed to dodge changesets' blindness to asset packages. |

## See also

- [adapters/overview.md](./overview.md) — the trait these methods implement.
- [commands/publish.md](../commands/publish.md) — how these methods are sequenced in CI.
