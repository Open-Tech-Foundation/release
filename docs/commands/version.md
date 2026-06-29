# `otf-release version`

**Interactive. Run locally. Produces a release PR — never publishes, never writes to `main`.**

```
otf-release version [--dry-run] [--first-release]
```

| Flag | Effect |
| --- | --- |
| `--dry-run` | Compute and print the plan ([summary](#plan-and-final-review)), write nothing. |
| `--first-release` | Permit publishable packages with no prior tag matching `tag_format`. Curated mode still requires release notes for packages you want to release. |

Implemented in `crates/core/src/version.rs`.

## What it does, step by step

1. **Discover** packages via the adapter; build the internal dependency graph.
2. **Strict preflight** ([preflight.md](../preflight.md)) — abort the entire run on *any*
   violation, **before mutating anything**. All violations are printed at once.
3. **Parse `[Unreleased]`** for each package; flag those with content as *pending*.
4. **Prompt** — multi-select the packages to release, then pick a bump
   (major / minor / patch) per selected package.
5. **Cascade** — for each bumped package, walk its dependents. Each dependent's bump is
   `adapter.dependent_bump(dep_bump, kind)`. This is **transitive** (every newly bumped
   dependent is re-fed into the walk) and takes the **max** bump when a package is reached by
   multiple paths. The cascade **terminates at private packages** — they are graph leaves and
   are never versioned or published. See [graph](../architecture.md#data-flow).
6. **Compute** new versions and the internal dependency-range updates
   (`adapter.format_range`).
7. **Plan** — render the computed version and range changes. `--dry-run` stops here and writes
   nothing.
8. **Branch** — assert a clean working tree and that you are on `main`, then
   `git checkout -b release/<date-or-versions>`. Release changes are **never** committed onto
   `main` directly (CI publish triggers on `main`).
9. **Apply** on the branch:
   - `adapter.write_version` for every affected **publishable** package.
   - `adapter.update_dep_range` for every changed internal range — **including private apps**
     (so they stay buildable) — but private apps get **no version bump and no publish**.
   - **Changelog rewrite**: move `[Unreleased]` → `## [x.y.z] - YYYY-MM-DD`, leaving a fresh
     empty `[Unreleased]`. Packages that were auto-bumped *only* (no curated notes) get the
     stub `_Dependency updates._`. See [changelog-format.md](../changelog-format.md).
   - `adapter.update_lockfile` — refresh the lockfile in the **same commit**, or a CI install
     will drift.
10. **Final review / confirm** — print the actual `git diff --stat`, then ask
    whether to commit, push, and open the PR. On cancel, generated release-branch changes are
    discarded and the command returns to the original branch.
11. **Commit** (`chore(release): …`), **push**, and **open a PR** via `gh`.

Merging that PR is what triggers CI [`publish`](./publish.md).

## Plan And Final Review

The plan is shown by `--dry-run` and is included in the final review:

```
Version Bumps (Direct & Indirect):
  Package       | Old   | New   | Reason
  @opentf/core  | 1.2.0 | 2.0.0 | major, selected

Internal Range Updates:
  Consumer    | Dependency   | Old    | New    | Notes
  playground | @opentf/core | ^1.2.0 | ^2.0.0 | private app (not published)

Changed Files:
  Cargo.toml      | 2 +-
  CHANGELOG.md    | 8 +++++++-

```

Three blocks: explicitly **selected** packages, **auto-bumped** dependents (with the reason),
and **internal range updates** (private apps flagged "range updated, NOT published").

## Invariants

- No release commit is created and nothing is pushed before the final diff confirmation.
- If the user cancels at the final confirmation, generated release changes are discarded.
- Private apps: ranges updated, **never** bumped or published.
- The working tree must be clean and on `main`; all release writes land on `release/*`.
- Preflight runs to completion (and can abort) before the first prompt.

## See also

- [preflight.md](../preflight.md) — the gate that runs in step 2.
- [changelog-format.md](../changelog-format.md) — the rewrite rules in step 9.
- [publish.md](./publish.md) — what the merged PR triggers.
