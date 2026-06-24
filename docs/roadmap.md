# Roadmap — deferred & out of scope for v1

v1 is deliberately narrow: **npm only**, hand-curated notes, local `version` → manual PR. The
items below are explicitly out of scope now. The architecture leaves room for them; none are
implemented.

## Additional adapters

The [`Adapter`](./adapters/overview.md) trait isolates ecosystems. **npm** and **cargo** are
implemented (see [adapters/cargo.md](./adapters/cargo.md)); **PyPI** and others are further out.

The cargo adapter is an **initial** implementation with one known gap still open:

- `version.workspace = true` (inherited versions) **breaks independent per-package
  versioning**. The adapter reads inherited versions but **refuses to write** them — independent
  bumps need a concrete `[package] version`. **Lockstep** workspace versioning (bump the whole
  workspace together) is the deferred follow-up, needed before the tool can release *its own*
  crates, which currently inherit their version.

Already handled: `cargo publish` needs a concrete `version` on path deps (done in
`resolve_workspace_links`); cargo has no peerDep concept so all internal dependents take a
`Patch`; crates.io is source-only so staged binaries are ignored on publish.

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
