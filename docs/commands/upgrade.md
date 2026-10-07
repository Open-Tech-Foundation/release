# `release upgrade`

**Regenerates `.github/workflows/release.yml` from the existing `release.toml`.**

```
release upgrade [--force]
```

| Flag | Effect |
| --- | --- |
| `--force` | Overwrite `release.yml` without prompting. Hand edits are still listed — as what was discarded. |

Implemented in `crates/core/src/upgrade.rs`. Does **not** edit `release.toml` — only the generated
workflow.

## Why it exists

`init` writes both `release.toml` and `release.yml`, but the workflow is a scaffold that evolves
with the CLI. After upgrading `release` itself, or after editing workflow-baked settings in
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
3. If the existing `release.yml` is byte-for-byte what would be written, say so and stop.
4. If it was edited by hand since it was generated, list the lines regenerating it would discard.
5. Without `--force`, ask before overwriting; cancel leaves the file unchanged.

## Hand edits

The first line of a generated workflow is a stamp holding a SHA-256 of what was generated. A file
that no longer matches its stamp was edited after generation, and `upgrade` prints the edited file's
lines that the regenerated workflow does not contain before it asks. With `--force` it overwrites
anyway and the list is the record of what was dropped. `release doctor` reports the same condition as
`workflow-hand-edited`.

The stamp is a hash, not a copy, so the list is "lines the new workflow does not have": it shows the
hand edits, and may also show lines a newer `release` generates differently. A file with no stamp —
hand-written, or generated before stamps existed — cannot be checked; `upgrade` says so and you
should review `git diff` afterwards.

A hand edit is a sign `release.toml` cannot express something yet. Move it there (`env`,
`[[package.setup]]` with `jobs`, …) rather than re-applying it after every upgrade. See
[ci-workflow.md](../ci-workflow.md).

## When to run it

- After installing a newer `release` CLI and you want the workflow to match.
- After `release config` changes that affect generated jobs (tag format, GitHub Release notes
  source, package build matrix entries, and similar).
- When onboarding a feature shipped in a recent release (for example `release check` replacing
  hand-rolled bash in `check-release`).

## See also

- [init.md](./init.md) — first-time setup that writes both config and workflow.
- [config.md](./config.md) — interactive `release.toml` editor; points here when workflow regen is
  needed.
- [ci-workflow.md](../ci-workflow.md) — the single `release.yml` model and what gets generated.