# Changelog format

Release notes follow **[Keep a Changelog](https://keepachangelog.com/)**. The hand-written
`[Unreleased]` section is the **source of truth** for what ships — it is **never inferred from
commits**. Parsing/rewriting lives in `crates/core/src/changelog.rs`.

## Shape of a `CHANGELOG.md`

```markdown
# Changelog

All notable changes to this package are documented here.

## [Unreleased]

### Added
- New `--foo` flag.

### Fixed
- Crash when the manifest had no `version`.

## [1.2.0] - 2026-05-01

### Added
- Initial public API.
```

`release.toml` chooses the changelog scope:

- `changelog_scope = "root"` uses the root `CHANGELOG.md` for every package.
- `changelog_scope = "package"` uses each package's adapter-discovered `CHANGELOG.md`
  (`Pkg.changelog_path`).

## How `version` rewrites it

When a release is applied ([version step 9](./commands/version.md)), for each affected package:

1. The `[Unreleased]` body is moved into a new dated section:
   ```
   ## [x.y.z] - YYYY-MM-DD
   ```
2. A **fresh empty `[Unreleased]`** is left at the top for the next cycle.
3. A package that was **auto-bumped only** (reached purely via cascade, with no curated
   `[Unreleased]` notes of its own) gets a stub body:
   ```
   _Dependency updates._
   ```

The same dated section is what [`publish`](./commands/publish.md) can lift into a GitHub
Release body.

## How `preflight` reads it

The [strict gate](./preflight.md) treats `[Unreleased]` as a compliance signal:

- **Empty/missing** `[Unreleased]` in the configured changelog but there are **commits since the
  last tag** (scoped to the package directory) → **abort**.
- Selected for a bump but `[Unreleased]` is empty → **abort**.

"Empty" means no meaningful content (whitespace/comments only) — see
`changelog::Unreleased::is_empty`.

## Rules of thumb

- Use root scope for lockstep/product releases; use package scope for independently released
  monorepo packages.
- Private apps are **not** required to keep a changelog (preflight allows their commits).
- Don't hand-edit dated sections that `version` already wrote — they are the published record.

## See also

- [preflight.md](./preflight.md) — the gate that enforces `[Unreleased]` discipline.
- [commands/version.md](./commands/version.md) — the rewrite step.
