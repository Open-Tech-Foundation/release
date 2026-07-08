# Strict preflight gate

An **all-or-nothing** compliance check that runs **before any prompt and before any mutation**
in [`version`](./commands/version.md). Its job: refuse to release if the changelog discipline
has slipped, while still allowing the flow to continue with warnings for commits that touched only
configured ignored paths. Implemented in `crates/core/src/preflight.rs`.

## Principle

> No undocumented ships. If a package changed since its last release in a release-relevant way,
> its `[Unreleased]` section must say so — or the whole run aborts.

Every hard violation is collected and printed at once; the process exits non-zero **before** any
`release/*` branch is created or any file is written. Warning-only cases are printed and the flow
continues. There is never partial state from a hard failure.

## State derivation

For every **non-private** package not listed in `skip_publish`, state comes from its last git tag
matching `release.toml`'s global `tag_format` or any configured `legacy_tag_formats`:

```
git log <tag>.. -- <pkg path>
```

The default format is `v{version}`. Repos that want package-scoped tags can set
`tag_format = "{name}@{version}"`. Repos migrating tag schemes can keep writing new tags with
`tag_format` while reading old history with `legacy_tag_formats = ["{name}@{version}"]`.

The diff is **scoped to the package directory** so shared root files (lockfile, CI config)
don't falsely mark a package as changed. Repos may also configure package-specific
`[publish.ignore_paths]` globs in `release.toml`; when every changed file for a package matches
those globs, an empty `[Unreleased]` is only a warning.

## Rules

| Condition | Result |
| --- | --- |
| Commits since last tag (scoped to pkg path) **but** configured `[Unreleased]` empty/missing | ✗ **ABORT** |
| Commits since last tag, `[Unreleased]` empty/missing, and **all** changed files match configured `publish.ignore_paths` | ⚠ **WARN** and continue |
| Selected for a bump **but** configured `[Unreleased]` empty | ✗ **ABORT** |
| No last tag **and** publishable (first release) with configured `[Unreleased]` empty/missing | ✗ **ABORT** |
| No last tag **and** publishable (first release) with configured `[Unreleased]` notes | ✓ OK |
| `[Unreleased]` present **with** commits | ✓ OK |
| Commits in a **private** package | ✓ OK — no changelog demanded |
| Commits in a package listed in `skip_publish` | ✓ OK — the tool does not version or publish it |

## Example abort output

```
release aborted — preflight violations:

  core: 3 commits since v1.2.0 but [Unreleased] is empty
  cli:  selected for bump but [Unreleased] is empty

no release/* branch created, no files written.
```

## Example warning output

```
preflight warnings:

  docs-site: 2 commit(s) since docs-site@1.4.0 but [Unreleased] is empty; only ignored paths changed
```

## Why before the prompt

Running the gate first means the user never spends time selecting packages and bumps only to
have the apply step fail. A failing repo is rejected up front, atomically.

## See also

- [changelog-format.md](./changelog-format.md) — what a compliant `[Unreleased]` looks like.
- [commands/version.md](./commands/version.md) — where this runs (step 2).
