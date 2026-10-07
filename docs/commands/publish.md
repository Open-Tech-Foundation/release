# `release publish`

**Non-interactive. Run in CI. Stateless. Idempotent and resumable.**

```
release publish [--package <NAME>] [--artifacts-dir <DIR>] [--dry-run]
```

| Flag | Effect |
| --- | --- |
| `--artifacts-dir <DIR>` | Root of staged binary artifacts (`.artifacts/`). Per-package assets live in `<DIR>/<pkg>/`. |
| `--package <NAME>` | Publish only this package; leave every other pending package untouched. |
| `--exclude-package <NAME>` | Exclude a package owned by a separate package-local CI pipeline. Repeatable. |
| `--dry-run` | Resolve and print the publish plan, but do not publish or push tags. |

Implemented in `crates/core/src/publish.rs`. Triggered by a merge to `main` (see
[ci-workflow.md](../ci-workflow.md)).

## What it does, step by step

1. **Discover** packages; build the graph.
2. **Filter** to the publishable set:
   - `!pkg.publishable` → **skip** (private apps and packages listed in `skip_publish` are always excluded).
   - `adapter.is_published(pkg, pkg.version)` is `true` → **skip** (already published →
     idempotent / resumable).
   - no dated `## [version]` section in its changelog → **skip with a warning**. Only
     `release version` writes that section, so a new package merged to `main` (or a version edited
     by hand) is held back instead of shipping as a side effect of the merge. `snapshot` is exempt:
     its per-commit versions never get a changelog section.
3. **Topological sort** over the internal graph — dependencies before dependents. **Error on
   cycles.**
4. For each package, in order:
   - `adapter.resolve_workspace_links(pkg)` — inject concrete published versions for any
     `workspace:*` / linked internal deps.
   - `adapter.publish(pkg, staged_assets)` — where `staged_assets` is `<artifacts-dir>/<pkg>/`
     **if that directory exists on disk**, else `None` (registry-only). **State comes from
     disk, not config.**
   - On success: push the git tag rendered from `release.toml`'s `tag_format` and (optionally)
     create a GitHub Release from the package's new changelog section.

## Failure model — halt, never roll back

Publishing is **not atomic** and is **irreversible**. Before a failure counts, each registry
publish is retried:

- **Rate limits** (HTTP 429) are waited out — for the time the registry names (crates.io sends
  `try again after <date>`), else 10 minutes — up to 12 times, at most an hour per wait. crates.io
  lets 5 new crates through and then one every 10 minutes, so a first release of 11 crates takes
  roughly an hour, unattended. GitHub's default 6-hour job timeout covers that; if you set
  `timeout-minutes` on the publish job, keep it above the wait.
- **Network failures and 5xx responses** are retried 4 times with a doubling backoff from 30s.
- Before every retry the registry is asked whether the version already landed (a response lost
  after the upload), and if so the package counts as published instead of being re-sent.
- Anything else (a name already taken, a missing token, a build error) fails at once.

If a package still fails to publish:

- **Stop immediately.** Do not publish its dependents.
- There is **no rollback.** A previously published package stays published.
- **Re-running resumes forward**: `is_published` skips everything already shipped and the run
  continues from where it stopped. In GitHub Actions that is **Re-run failed jobs** on the
  release run; nothing needs to be cleaned up first. A package that published but whose tag or
  GitHub Release was not created is finished off on the re-run without being published again.

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
