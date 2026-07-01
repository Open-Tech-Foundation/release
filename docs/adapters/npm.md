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
npm publish --access public --no-workspaces [--tag <pre-id>]
```

Flags, all load-bearing:

- **`--access public`** — required for a **scoped** package's *first* publish (`@opentf/*`
  packages default to restricted otherwise).
- **`--no-workspaces`** — required because the **repo root is a private workspace**. Without
  this flag npm runs in workspace mode and **skips the package even when invoked from the
  package's own directory**.
- **`--tag <pre-id>`** — added automatically for a **prerelease** version: the leading identifier
  of the prerelease becomes the dist-tag (`1.2.3-dev.<hash>` → `--tag dev`, `2.0.0-beta.1` →
  `--tag beta`). A normal release publishes under `latest`. This keeps an automated snapshot from
  ever becoming the default install.

### Staging matrix binaries

Before `npm publish`, the contents of `.artifacts/<package>/` are copied into the package. For a
matrix package that tree is `bin/<stage_as>/<bin><ext>[.br]`, where `<stage_as>` is the Node
`process.platform-process.arch` directory the package's install-time resolver reads (`linux-arm64`,
`darwin-x64`, `win32-x64`, …). `otf-release build` produces this layout per target and the workflow
merges every target's artifact back into `.artifacts/<package>/` before this step — so the published
tarball carries a binary for each platform under the exact path the resolver expects.

## Workspace links (`resolve_workspace_links`)

Before publishing, rewrite `workspace:*` (and other linked internal deps) to the **concrete
published version**. npm does **not** do this automatically, so without it consumers would get
an unresolvable `workspace:*` range.

## Lockfile (`update_lockfile`)

After version writes, refresh the npm lockfile so CI installs do not drift from the manifests.
This runs in the **same commit** as the version changes (see
[version step 9](../commands/version.md)). Generated release workflows use the repo's root
lockfile to choose the install command: Bun, pnpm, Yarn, or npm. The local version flow uses the
same lockfile detection when refreshing the lockfile, so Bun/pnpm/Yarn workspaces do not fall back
to `npm install --package-lock-only`.

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
| `--tag <pre-id>` for prereleases | A snapshot never lands on `latest`. |
| Brotli staging done by `otf-release build` | Compresses with the Rust `brotli` crate (max quality, window 22); the package decompresses with Node `zlib` at install — no runner-side CLI either way. |

| Drop | Why |
| --- | --- |
| `private:true` guard flip | Only existed to dodge changesets' blindness to asset packages. |

## See also

- [adapters/overview.md](./overview.md) — the trait these methods implement.
- [commands/publish.md](../commands/publish.md) — how these methods are sequenced in CI.
