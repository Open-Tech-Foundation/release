# Roadmap — deferred & out of scope for v1

v1 is deliberately narrow: **npm only**, hand-curated notes, local `version` → manual PR. The
items below are explicitly out of scope now. The architecture leaves room for them; none are
implemented.

## Additional adapters

The [`Adapter`](./adapters/overview.md) trait isolates ecosystems. **npm** and **cargo** are
implemented (see [adapters/cargo.md](./adapters/cargo.md)); **PyPI** and others are further out.

The cargo adapter handles both independent (concrete-version) crates and **lockstep workspaces**
(`version.workspace = true` → bump `[workspace.package] version`, single root `CHANGELOG.md`).
`cargo publish` needs a concrete `version` on path deps (done in `resolve_workspace_links`); cargo
has no peerDep concept so all internal dependents take a `Patch`. For a binary tool, the cargo
`init` workflow ships cross-OS binaries via a **GitHub Release** rather than crates.io.

## Pre-releases / snapshots

`-next`, `-canary`, and similar pre-release channels are **deliberately excluded** from v1.
Add later if a need appears.

## Release-PR bot

A changesets-style, auto-maintained release PR is **not** in v1. v1 is local `version` → a
manually opened PR. A bot that keeps a running release PR up to date could come later.

## First-release ergonomics

v1 requires `[Unreleased]` for a first release. A dedicated `--first-release` flag (sketched in
`version.rs`) will make first-time publishing explicit rather than implicit.

## See also

- [adapters/overview.md](./adapters/overview.md) — how to add an adapter when these land.
- [implementation-plan.md](./implementation-plan.md) — what is being built now.
