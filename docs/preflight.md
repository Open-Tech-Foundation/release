# Strict preflight gate

An **all-or-nothing** compliance check that runs **before any prompt and before any mutation**
in [`version`](./commands/version.md). Its job: refuse to release if the changelog discipline
has slipped. Implemented in `crates/core/src/preflight.rs`.

## Principle

> No undocumented ships. If a package changed since its last release, its `[Unreleased]`
> section must say so — or the whole run aborts.

Every violation is collected and printed at once; the process exits non-zero **before** any
`release/*` branch is created or any file is written. There is never partial state.

## State derivation

For every **non-private** package, state comes from its last git tag matching
`release.toml`'s global `tag_format`:

```
git log <tag>.. -- <pkg path>
```

The default format is `v{version}`. Repos that want package-scoped tags can set
`tag_format = "{name}@{version}"`.

The diff is **scoped to the package directory** so shared root files (lockfile, CI config)
don't falsely mark a package as changed.

## Rules

| Condition | Result |
| --- | --- |
| Commits since last tag (scoped to pkg path) **but** `[Unreleased]` empty/missing | ✗ **ABORT** |
| Selected for a bump **but** `[Unreleased]` empty | ✗ **ABORT** |
| No last tag **and** publishable (first release) without `--first-release` | ✗ **ABORT** |
| No last tag **and** publishable with `--first-release` | Require release notes in curated mode |
| `[Unreleased]` present **with** commits | ✓ OK |
| Commits in a **private** package | ✓ OK — no changelog demanded |

## Example abort output

```
release aborted — preflight violations:

  core: 3 commits since v1.2.0 but [Unreleased] is empty
  cli:  selected for bump but [Unreleased] is empty

no release/* branch created, no files written.
```

## Why before the prompt

Running the gate first means the user never spends time selecting packages and bumps only to
have the apply step fail. A failing repo is rejected up front, atomically.

## See also

- [changelog-format.md](./changelog-format.md) — what a compliant `[Unreleased]` looks like.
- [commands/version.md](./commands/version.md) — where this runs (step 2).
