# Implementation plan

A phased build plan for `otf-release` v1, derived from the build order in `plan.md` §8. Each
phase is independently testable and lists explicit **acceptance criteria**. The Cargo workspace
and module skeletons already exist (Phase 0 ✅); every command function is currently a
`todo!()`.

> **Legend:** ✅ done · 🚧 in progress · ⬜ not started. Module paths are `crates/<crate>/src/<file>`.

## Status at a glance

| Phase | Scope | Status |
| --- | --- | --- |
| 0 | Workspace scaffold + types + CLI surface | ✅ |
| 1 | npm adapter | ✅ |
| 2 | Changelog parser/rewriter | ✅ |
| 3 | Graph: topo sort + cascade | ✅ |
| 4 | Strict preflight | ✅ |
| 5 | `version` command | ✅ |
| 6 | `publish` command | ✅ |
| 7 | `init` command | ⬜ |
| 8 | Hardening: docs, tests, release-of-self | ⬜ |

Dependency order: 1 → 2 → 3 → 4 → 5, then 6, then 7. (5 needs 1–4; 6 needs 1+3; 7 needs 1.)

---

## Phase 0 — Scaffold ✅

**Done.** Workspace (`core`, `adapters`, `cli`), domain types (`Pkg`, `Bump`, `DepKind`,
`InternalDep`), the `Adapter` trait, command-module stubs, and the clap CLI surface. `cargo
build` and `otf-release --help` both work.

---

## Phase 1 — npm adapter ✅

**Goal:** a fully working `Adapter` impl so every later phase has real data.
**Files:** `adapters/npm/mod.rs`, `adapters/npm/manifest.rs`.

**Done.** All eight `Adapter` methods implemented, 13 unit tests green. Two design choices
worth noting for later phases:
- **Format-preserving edits** (`manifest.rs`) — a small JSON span locator rewrites only the
  target value's bytes, so manifests stay byte-stable except the intended change. Reads use
  `serde_json`.
- **`CommandRunner` seam** (`mod.rs`) — `is_published` / `publish` / `update_lockfile` shell
  out through a trait, so they're tested with a fake runner (no live npm/registry). Phases 5–6
  can reuse this seam for `git` / `gh`.

Tasks:
1. `discover_packages` — read workspace globs from the root `package.json`, parse each member's
   `package.json` (name, version, `private`), resolve `changelog_path`, and extract
   `internal_deps` (only edges to other discovered packages) across `dependencies`,
   `peerDependencies`, `devDependencies`.
2. `write_version` / `update_dep_range` — edit manifests **preserving formatting** (key order,
   indentation, trailing newline). Prefer a format-preserving JSON edit over full re-serialize.
3. `format_range` → `^{version}` (done in stub).
4. `dependent_bump` → peerDep mirrors, else patch (done in stub).
5. `resolve_workspace_links` — rewrite `workspace:*` / linked ranges to concrete versions.
6. `update_lockfile` — refresh `package-lock.json` (e.g. `npm install --package-lock-only`).
7. `is_published` — `npm view <name>@<version> version`; `Ok(true)` on success, `Ok(false)` on
   the registry's not-found, `Err` on anything else.
8. `publish` — `npm publish --access public --no-workspaces`; attach `staged_assets` when present.

**Acceptance:**
- `discover_packages` on a fixture monorepo returns correct names/versions/`publishable` flags
  and exactly the internal edges (no external deps).
- Manifest writes are byte-stable except the intended change (golden-file test).
- `is_published` is exercised against a stubbed/mocked `npm` (no live registry in unit tests).
- npm gotchas hold: `--no-workspaces`, `--access public`, link resolution (see
  [adapters/npm.md](./adapters/npm.md)).

---

## Phase 2 — Changelog parser/rewriter ✅

**Goal:** read and rewrite Keep a Changelog files.
**Files:** `core/changelog.rs`.

**Done.** Parse/rewrite logic is pure (`&str → String`) so golden tests need no file IO; the
public `parse_unreleased` / `release_unreleased` / `dated_section_notes` are thin file
wrappers. Section boundaries are level-≤2 ATX headings, so `### Added`/`### Fixed` stay inside
the body. `is_empty` ignores whitespace **and** HTML comments. 8 unit tests green.

Tasks:
1. `parse_unreleased` — capture the body between `## [Unreleased]` and the next `## ` heading;
   `is_empty` treats whitespace/comments-only as empty.
2. `release_unreleased` — move `[Unreleased]` → `## [x.y.z] - YYYY-MM-DD`, leave a fresh empty
   `[Unreleased]`, insert the `_Dependency updates._` stub when `stub_if_empty`.
3. Helper to extract a dated section's body (for GH Release notes in Phase 6).

**Acceptance:**
- Round-trip golden tests: messy real-world changelogs parse and rewrite without clobbering
  unrelated sections.
- Empty/whitespace/comment-only `[Unreleased]` reports `is_empty() == true`.
- Date is injected (caller-supplied, for testability).

---

## Phase 3 — Graph: topological sort + cascade ✅

**Goal:** ordering for publish and the bump cascade for version.
**Files:** `core/graph.rs`.

**Done.** `Graph::build` indexes by name and validates edges (rejects duplicate names and
edges to unknown packages); `topo_order` is Kahn's algorithm with deterministic (index-order)
output and a cycle error naming the involved packages; `cascade` is a max-merge worklist that
propagates transitively, mirrors peerDeps, and never bumps private leaves. 5 unit tests green
(diamond order, cycle, validation, transitive/max cascade, private-selection guard).

Tasks:
1. `Graph::build` — index packages by name; validate that every internal edge points at a known
   package.
2. `topo_order` — dependencies before dependents; **error on cycle** (report the cycle).
3. `cascade` — worklist over selected bumps: for each bumped package, for each dependent, apply
   `adapter.dependent_bump`, **merge with max**, re-enqueue changed dependents (transitive),
   and **stop at private packages**.

**Acceptance:**
- Topo sort correct on a diamond graph; cycles produce a clear error.
- Cascade: transitive propagation, max-bump on multi-path, peerDep mirrors, private leaves never
  appear in the bump map. Unit tests use a fake `Adapter`.

---

## Phase 4 — Strict preflight ✅

**Goal:** the all-or-nothing gate.
**Files:** `core/preflight.rs`, `core/git.rs`.

**Done.** Repo access is behind a `RepoState` trait (`git.rs`): `GitRepo` resolves the highest
`name@x.y.z` tag (numeric semver order) and counts commits since it **scoped to the package
dir** (`rev-list --count <tag>..HEAD -- <relpath>`). `preflight::check` applies the rule table,
collects all violations, treats a missing changelog as empty, and skips private packages;
`format_violations` renders the abort block. Tested with a fake `RepoState` for the rule engine
plus a **real-git** test proving path scoping (root lockfile change ⇒ 0 commits). 4 unit tests.

Tasks:
1. Resolve each non-private package's last tag `name@x.y.z`.
2. `git log <tag>.. -- <pkg path>` scoped to the package dir → "has commits since tag".
3. Apply the rule table from [preflight.md](./preflight.md); collect **all** `Violation`s.
4. First-release handling (no tag + publishable).

**Acceptance:**
- Given fixtures (commits-without-notes, selected-but-empty, compliant, private-with-commits),
  the exact expected violation set is returned.
- Path scoping: a change to a root lockfile does **not** mark a package dirty.
- Any violation → non-zero exit, no writes (enforced by the `version` integration test).

---

## Phase 5 — `version` command ✅

**Goal:** the full local flow end to end.
**Files:** `core/version.rs`, `core/summary.rs`, `core/prompt.rs`, `core/forge.rs`,
`core/date.rs`, `core/git.rs` (GitOps), CLI wiring.

**Done.** `version::orchestrate` is the testable core; side effects are injected as traits
(`Prompt`, `GitOps`, `Forge`) and `today` is injected for determinism. `run` wires the real
`StdinPrompt`/`GitRepo`/`GhForge`. Flow: discover → preflight → pending → select+bump →
cascade → compute versions/ranges → `summary::render` → dry-run/confirm → branch guard
(clean + on `main`) → apply (versions, ranges incl. private apps, changelogs, lockfile) →
commit → push → `gh` PR. Covered by core unit tests (apply_bump, change_note, summary) plus
two CLI integration tests on a real temp git+npm repo (npm runner faked, push to a local bare
remote): a full release landing on `release/*` with main untouched, and a dry-run that writes
nothing.

Tasks:
1. Orchestrate: discover → preflight → prompt (multi-select + per-pkg bump) → cascade → compute
   versions & range updates → `summary::render` → confirm.
2. Branch guard: clean tree + on `main`; `git checkout -b release/<…>`.
3. Apply: `write_version` (publishable), `update_dep_range` (incl. private apps),
   `release_unreleased`, `update_lockfile`.
4. Commit (`chore(release): …`), push, open PR via `gh`.
5. `--dry-run` (print plan, write nothing) and `--first-release`.

**Acceptance:**
- `--dry-run` on a fixture prints the three-block summary and leaves the tree untouched.
- A real run (in a temp git repo, `gh`/`npm` stubbed) creates a `release/*` branch with correct
  manifests, ranges, changelogs, and lockfile — and **never** commits to `main`.
- Private apps: ranges updated, no version bump, no changelog dated section.

---

## Phase 6 — `publish` command ✅

**Goal:** CI publish, stateless and resumable.
**Files:** `core/publish.rs`, `core/git.rs` (tag ops), `core/forge.rs` (release), CLI wiring.

**Done.** `publish::orchestrate` discovers, topo-sorts, filters to publishable & not-already-
published, then per package: `resolve_workspace_links` → `publish` (staged assets attached
only if `<artifacts-dir>/<pkg>/` exists on disk) → `create_tag`/`push_tag` → optional
`create_release` from the dated changelog section. First failure propagates and halts (no
rollback); a re-run skips already-published via `is_published` and resumes. `GitOps` gained
`create_tag`/`push_tag`; `Forge` gained `create_release`. Two CLI integration tests with a
registry-modelling fake runner prove topo order + idempotency and halt-on-failure +
forward-resume (dependent of the failed package is never attempted).

Tasks:
1. Discover → filter (`publishable` && !`is_published`) → `topo_order`.
2. Per package: `resolve_workspace_links` → `publish(pkg, staged_assets)` where
   `staged_assets = <artifacts-dir>/<pkg>/` iff that dir exists → tag `name@x.y.z` (+ optional
   GH Release from the dated changelog section).
3. **Halt on first failure**; re-run resumes forward via `is_published`.
4. `--artifacts-dir`, `--dry-run`.

**Acceptance:**
- Idempotency: a second run after a full success publishes nothing.
- Resume: after an injected mid-run failure, re-running skips the already-published and
  continues; dependents of the failed package are **not** published in the first run.
- Asset attach is driven purely by directory presence on disk.

---

## Phase 7 — `init` command

**Goal:** generate the single `release.yml`.
**Files:** `core/init.rs`, an embedded YAML template.

Tasks:
1. Detect ecosystems (npm).
2. Multi-select asset packages; prompt target triples (default set + `# edit me`).
3. Emit `release.yml`: `build-matrix` (iff asset packages) → `publish` (`needs:`), artifact
   download to `.artifacts/`, `otf-release publish`, correct secrets.
4. Idempotent overwrite (`--force`); never re-manage after generation.

**Acceptance:**
- Generated YAML matches a golden file for (a) libs-only and (b) libs+assets repos.
- Re-run without `--force` warns and does not overwrite; with `--force` it replaces.

---

## Phase 8 — Hardening

- End-to-end test on a sample monorepo fixture (version → simulated merge → publish).
- Fill in module docs to match `docs/`; keep `docs/` and code in sync.
- `otf-release` releasing **itself** as `@opentf/release` (dogfood).
- CI for the tool's own repo (fmt, clippy, test).

---

## Cross-cutting concerns

- **Error reporting** — `anyhow` with context; preflight/cascade aggregate rather than fail-fast
  where the spec says "print all".
- **External commands** (`git`, `gh`, `npm`) — wrap behind small runner traits so they can be
  faked in tests; no live network in unit tests.
- **No persisted state** — verify nothing writes a config file; disk + registry + git only.
- **Idempotency & atomicity** — preflight aborts before writes; publish resumes forward.

## Requirements traceability

All 14 requirements from `plan.md` §10 map onto these phases:

| Req | Summary | Phase(s) |
| --- | --- | --- |
| 1 | Monorepo multi-package publish | 1, 6 |
| 2 | CLI-driven flow | 0, 5–7 |
| 3 | Registry-agnostic, npm v1 | 0, 1 |
| 4 | Notes = manual `[Unreleased]` | 2, 4 |
| 5 | `version` multi-select + per-pkg bump | 5 |
| 6 | Summary/confirm before apply | 5 |
| 7 | Adapter-decided dependent bump | 1, 3 |
| 8 | Rust single binary | 0 |
| 9 | Local → `release/*` → PR, never `main` | 5 |
| 10 | Matrix-gated multi-target publish | 6, 7 |
| 11 | Private pkgs = leaves | 3, 5 |
| 12 | Strict preflight | 4 |
| 13 | `init` ecosystem-aware, idempotent | 7 |
| 14 | Single `release.yml`, stateless, topo | 6, 7 |
