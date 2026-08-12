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

pnpm is the exception: it keeps its member list in `pnpm-workspace.yaml`'s `packages` key and
ignores `workspaces` entirely, so that file is read first and wins when it declares members — it is
what actually installs the repo. A `pnpm-workspace.yaml` holding only a `catalog:` declares no
members and falls through to `package.json`. Negated patterns (`!packages/private-app`), which npm,
pnpm, and bun all accept, exclude a directory the other patterns matched.

A repo with **no root `package.json` at all** declares no npm packages there, which is an empty
result rather than an error — a polyglot repo's root routinely belongs to another ecosystem, and
failing would take `check`/`version`/`publish` down for every other enabled adapter too.

### Repos that declare no npm workspace

This is only for repos that declare their members *nowhere* — if `pnpm-workspace.yaml` or a root
`workspaces` field exists, that is used and `[discovery]` stays out of it.

A polyglot monorepo often has no root `package.json` to put `workspaces` in: the root is a Cargo
workspace, and the JS packages are independent projects with their own lockfiles. Adding a root
`workspaces` key just to be discoverable is not a neutral edit — npm, pnpm, and bun all act on it,
hoisting every member into one root `node_modules` behind a single lockfile.

So the declaration can live in `release.toml` instead:

```toml
[discovery]
npm = ["packages/*", "types"]
```

Globs are relative to the repo root and name package **directories**, not manifest files. When the
list is non-empty it *is* the member set and the root `package.json` is never consulted — so it can
also narrow a repo to a subset of what its package manager treats as members.

`init` and `otf-release config` → *Ecosystems* write this list: they scan the repo, show every
`package.json` carrying a `name` and a `version`, pre-check the publishable ones, and save what you
confirm. The scan is a suggestion engine only. Discovery never walks the tree at release time,
because a walk finds test fixtures and scaffolding templates just as happily as real packages, and
a false positive there means a published package or a pushed tag.

Repos that *do* declare their members — `workspaces` or `pnpm-workspace.yaml` — are left alone:
`[discovery]` stays absent, and the repo's own declaration remains the single source of truth.

### Installing dependencies in CI

A root workspace installs once at the root: one lockfile, every member resolving through it. A repo
declaring `[discovery] npm` has no root workspace to install — there is no root `package.json` —
so the generated workflow installs **each package in its own directory**, with the package manager
that package's own lockfile implies (`bun.lock` → `bun install --frozen-lockfile`, and so on). Two
packages in one repo may therefore use different package managers. The catch-all publish job builds
nothing, so it installs nothing and only sets up node to reach the registry.

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

## Building before publish (the tool owns the build)

The tool owns *releasing*; npm owns *publishing*. For a plain (non-matrix) npm package, there is no
cross-job artifact staging — the build runs **inline in the package's own publish job**, on the same
runner, right before `npm publish` packs it.

At `init`, for each publishable npm package:

- **Auto-detect the build.** If `package.json` declares a `scripts.build`, `init` records an
  inline-build publish entry (`command = "npm run build"`) — no prompt. The generated
  `publish-<pkg>` job runs `npm run build` (scoped to the package's directory via
  `working-directory`) and then `otf-release publish --package <name>` with **no `--artifacts-dir`**.
  A package without a `build` script is published as-is by the catch-all `publish` job.
- **Strip pack/publish lifecycle hooks.** Because the pipeline runs the build itself, `init` removes
  npm's `prepublish`, `prepublishOnly`, `prepack`, and `prepare` scripts from `package.json`
  (surgically — every other byte is preserved) so npm can't re-run a build behind the pipeline, and
  prints which hooks were removed. Move any custom pre-publish logic into a `build` script or the
  `[hooks]` section of `release.toml`.

Matrix npm packages (a native binary wrapped in an npm package) are the exception: their per-platform
binaries are built on separate runners and **must** stage across jobs, so they keep the build-job +
artifact path described below.

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

When the root has **neither** a lockfile nor a `package.json`, the refresh is skipped: that is the
`[discovery]` layout, where each package is its own install with its own lockfile beside it, so a
root install would just fail and no root lockfile exists to have gone stale.

## Range syntax (`format_range`)

```
1.2.3  →  ^1.2.3
```

### What gets rewritten (`update_dep_range`)

When an internal dependency is bumped, every consumer's declared range is rewritten — including
consumers marked `private: true`, which are never versioned or published but still have to resolve
against the workspace.

Rewriting preserves the existing operator and only touches a **simple range**: one comparator
(`^`, `~`, `>=`, `<=`, `>`, `<`, `=`, or none) followed by a complete `x.y.z` version.

```
^1.2.3            →  ^2.0.0
>=1.2.3           →  >=2.0.0
workspace:^       →  workspace:^     (resolved at publish, not here)
*  latest  1.x    →  unchanged       (nothing to replace)
https://…/pkg-1.2.3.tgz              →  unchanged
file:../packages/pkg                 →  unchanged
git+ssh://git@github.com/o/r#v1.2.3  →  unchanged
npm:@scope/other@^1.2.3              →  unchanged
>=1.0.0 <2.0.0                       →  unchanged
```

Anything pinned by hand is left exactly as written. A tarball URL or git ref names a version that
exists *now*; the version being bumped to is not published until the `publish` step, so moving the
pin would break `install` — and the lockfile refresh runs before publish. The plan marks those rows
`pinned spec, left unchanged`. Repointing a pin at the new release is a manual follow-up, or a
`post_version` hook.

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
