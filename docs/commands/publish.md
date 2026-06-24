# `otf-release publish`

**Non-interactive. Run in CI. Stateless. Idempotent and resumable.**

```
otf-release publish [--artifacts-dir <DIR>] [--dry-run]
```

| Flag | Effect |
| --- | --- |
| `--artifacts-dir <DIR>` | Root of staged binary artifacts (`.artifacts/`). Per-package assets live in `<DIR>/<pkg>/`. |
| `--dry-run` | Resolve and print the publish plan, but do not publish or push tags. |

Implemented in `crates/core/src/publish.rs`. Triggered by a merge to `main` (see
[ci-workflow.md](../ci-workflow.md)).

## What it does, step by step

1. **Discover** packages; build the graph.
2. **Filter** to the publishable set:
   - `!pkg.publishable` → **skip** (private apps are always excluded).
   - `adapter.is_published(pkg, pkg.version)` is `true` → **skip** (already published →
     idempotent / resumable).
3. **Topological sort** over the internal graph — dependencies before dependents. **Error on
   cycles.**
4. For each package, in order:
   - `adapter.resolve_workspace_links(pkg)` — inject concrete published versions for any
     `workspace:*` / linked internal deps.
   - `adapter.publish(pkg, staged_assets)` — where `staged_assets` is `<artifacts-dir>/<pkg>/`
     **if that directory exists on disk**, else `None` (registry-only). **State comes from
     disk, not config.**
   - On success: push the git tag `name@x.y.z` and (optionally) create a GitHub Release from
     the package's new changelog section.

## Failure model — halt, never roll back

Publishing is **not atomic** and is **irreversible**. If a package fails to publish:

- **Stop immediately.** Do not publish its dependents.
- There is **no rollback.** A previously published package stays published.
- **Re-running resumes forward**: `is_published` skips everything already shipped and the run
  continues from where it stopped.

This is why the gating happens upstream — a failed build matrix means `publish` never runs at
all (see [ci-workflow.md](../ci-workflow.md)).

## Why stateless matters

There is no manifest of "what to publish" handed to this command. It re-derives everything:

- the package set, from manifests on disk;
- what is already shipped, from the registry (`is_published`);
- which packages get binaries, from the **presence of `.artifacts/<pkg>/`** on disk.

So a re-run after a partial failure, or a manual re-trigger, always does the right thing
without remembering anything from the previous run.

## Invariants

- Private apps are never published.
- Already-published versions are skipped (idempotent).
- Dependencies publish before dependents (topological order); cycles are a hard error.
- First failure halts the run; forward-resume only, no rollback.

## See also

- [ci-workflow.md](../ci-workflow.md) — the `release.yml` that gates and invokes this.
- [adapters/npm.md](../adapters/npm.md) — the publish mechanics and npm gotchas.
