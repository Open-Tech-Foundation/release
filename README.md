<div align="center">

# OTF Release

</div>

> Curated-changelog, manual-bump release CLI for polyglot monorepos.

A single-binary release tool for the OTF monorepo. You write the release notes
(in each package's `[Unreleased]` changelog section), you pick the bumps — `release`
handles the rest: dependency-aware version cascades, internal range updates, topological
publishing, and a matrix-gated GitHub release in one `release.yml`.

Unlike commit-driven tools, your hand-written `[Unreleased]` notes are the source of
truth — never inferred from commits. Unlike npm-locked tools, the publishing backend is
adapter-based: **npm today, cargo and others later**.

## What it does

- **`version`** (local) — lists packages, you multi-select and choose bumps; cascades to
  internal dependents, updates dep ranges, moves `[Unreleased]` → a dated section, then
  opens a release PR. Never touches `main` directly.
- **`publish`** (CI) — publishes changed packages in dependency order, idempotent and
  resumable, attaching prebuilt binaries when a workflow stages them.
- **`init`** — generates one ecosystem-aware `release.yml`, asking which packages need
  binary artifacts built before publish.

## Principles

- **You curate, it ships.** Notes and bumps are human decisions; mechanics are automated.
- **Strict by default.** Commits since the last tag with an empty `[Unreleased]` abort the
  release — no undocumented ships.
- **Stateless & config-light.** The generated `release.yml` is the single source of truth.
- **Dependency-correct.** peerDep dependents mirror the bump; encapsulated ones get a patch.
  Private apps stay buildable but are never published.
