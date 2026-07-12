# `otf-release upgrade`

**Regenerates `.github/workflows/release.yml` from the existing `release.toml`.**

```
otf-release upgrade [--force]
```

| Flag | Effect |
| --- | --- |
| `--force` | Overwrite `release.yml` without prompting. |

Implemented in `crates/core/src/upgrade.rs`. Does **not** edit `release.toml` — only the generated
workflow.

## Why it exists

`init` writes both `release.toml` and `release.yml`, but the workflow is a scaffold that evolves
with the CLI. After upgrading `otf-release` itself, or after editing workflow-baked settings in
[`config`](./config.md) (such as `tag_format` or `github_release_notes`), run `upgrade` to pick up
new CI pipeline features without re-running the full setup wizard.

From the changelog:

- **v0.1.0** — Added `upgrade` so repos can adopt new generated workflow behavior (for example the
  `check-release` job that guards expensive cross-compilation) without hand-editing YAML.
- **v0.14.0** — Regenerated npm publish jobs now use the repo's **detected package manager** instead
  of always falling back to `npm ci`, fixing Bun/pnpm/Yarn repos without `package-lock.json`.
- **v0.17.0** — Regenerated workflows emit the delegated [`check`](./check.md) gate with
  `fetch-depth: 0` so tags are present for the release decision.

## What it does

1. Load `release.toml` from the workspace root.
2. Re-render `.github/workflows/release.yml` with the same generator [`init`](./init.md) uses.
3. If `release.yml` already exists and `--force` was not passed, prompt before overwrite; cancel
   leaves the file unchanged.

The generated file remains yours to edit afterward — `upgrade` does not try to manage it on every
run. See [ci-workflow.md](../ci-workflow.md).

## When to run it

- After installing a newer `otf-release` CLI and you want the workflow to match.
- After `otf-release config` changes that affect generated jobs (tag format, GitHub Release notes
  source, package build matrix entries, and similar).
- When onboarding a feature shipped in a recent release (for example `otf-release check` replacing
  hand-rolled bash in `check-release`).

## See also

- [init.md](./init.md) — first-time setup that writes both config and workflow.
- [config.md](./config.md) — interactive `release.toml` editor; points here when workflow regen is
  needed.
- [ci-workflow.md](../ci-workflow.md) — the single `release.yml` model and what gets generated.