# Roadmap — deferred & out of scope for v1

v1 is deliberately narrow: **npm only**, hand-curated notes, local `version` → manual PR. The
items below are explicitly out of scope now. The architecture leaves room for them; none are
implemented.

## Additional adapters (interface only, no impl)

The [`Adapter`](./adapters/overview.md) trait isolates ecosystems, but only npm exists. Known
constraints for the next likely adapter, **cargo**:

- `version.workspace = true` (inherited versions) **breaks independent per-package
  versioning** — the adapter must either forbid it or accept lockstep versioning for Rust.
- `cargo publish` **requires a concrete `version`** on path dependencies (mirrors npm's
  `resolve_workspace_links`).
- Cargo has **no peerDep concept**, so the cascade rule (`dependent_bump`) likely makes **all**
  internal dependents `Patch`.

PyPI and others are further out.

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
