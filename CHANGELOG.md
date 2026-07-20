# Changelog

All notable changes to **otf-release** are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/), and this project
adheres to [Semantic Versioning](https://semver.org/). Work in progress lives under
`[Unreleased]` until it ships.

## [Unreleased]

- **github-release/config** — New `attest` field for build-only packages. `attest = true` adds an
  `actions/attest-build-provenance@v2` step to the generated release job and the `id-token: write` +
  `attestations: write` permissions it needs, signing every released asset with the workflow's OIDC
  identity. Consumers verify with `gh attestation verify <file> --repo <owner/repo>`.

  This closes the gap `checksums` cannot: a `checksums.txt` served from the same release proves an
  asset arrived **intact**, but an attacker who can replace the binary can replace the checksum
  beside it. Provenance is signed by GitHub and proves the asset was built by this repo's workflow
  from this commit — **authenticity**, not just integrity. The step's `subject-path` and the
  command's asset output directory come from one shared function (`github_release::assets_subdir`),
  so the glob cannot drift and silently sign nothing. Signing runs after the release is created, so
  a signing outage cannot block shipping.

  Off by default, because enabling it changes a workflow's permissions and `upgrade` must not do
  that silently. `init` proposes it (default yes) when configuring a build-only package.
- **install** — `install.sh` and `install.ps1` now verify what they download. The checksum from the
  release's `checksums.txt` is compared before install and a mismatch is fatal; build provenance is
  verified with `gh attestation verify` when the `gh` CLI is present. Set
  `OTF_RELEASE_REQUIRE_ATTESTATION=1` to make provenance mandatory instead of best-effort. Both
  checks run on the downloaded asset before it is unpacked, and both degrade cleanly against older
  releases that carry neither. Previously the only check was a magic-byte test, which distinguishes
  a binary from an HTML error page but accepts a malicious binary without complaint.
- **self** — This repo now publishes `checksums.txt` and provenance for its own releases.

- **github-release** — Fixed: the binary inside a release archive was **not executable**. v0.25.0's
  assets stored it as mode 644, so every extracted `otf-release` needed a `chmod +x` — the exact
  papercut archiving was meant to remove. The cause is upstream of the archiving code:
  `upload-artifact`/`download-artifact` zip the staged tree and drop POSIX permissions, so the
  binary always arrives at the release job non-executable no matter what the build produced. The
  archive writer now stores the binary member as mode `755` explicitly instead of inheriting it from
  disk. `include` files keep their own mode. Verified against both `.tar.gz` and `.zip`.
- **docs** — Corrected the "preserves the executable bit" claim in the README and docs, which was
  true of the archiving code in isolation but false of the actual CI pipeline.
- **install/README** — Documented FreeBSD in the install section and listed the published target set.

## [0.25.0] - 2026-07-20

- **install — required for 0.24.0 → next.** `install.sh` and `install.ps1` now unpack archive
  assets. Since 0.24.0 made archives the default, the next release publishes
  `otf-release-<os>-<arch>.tar.gz` / `.zip`, which the old installers could not fetch (they
  requested the bare name and 404'd) or validate (a gzip fails the ELF/PE magic check). Both scripts
  now try the archive name first, fall back to the bare name so older releases still install, and
  extract the binary before the existing "is this really an executable" guard runs. `self-update`
  reruns these scripts, so it is fixed by the same change. **Without this, the release after 0.24.0
  would have broken every `curl install.sh | bash`, including the tool's own CI jobs.**
- **install** — `install.sh` recognizes FreeBSD (`uname -s` = `freebsd`), so the FreeBSD asset is
  installable rather than published-but-unreachable.
- **self** — Added a FreeBSD x86_64 target to this repo's own `release.toml`, so the VM build path
  is exercised by a real release. aarch64 is left off: it is fully emulated and much slower.

## [0.24.0] - 2026-07-20

- **github-release — BREAKING.** Build-only binaries now ship as **archives by default**: a package
  that sets no `archive` key gets `"auto"` (`.zip` for Windows targets, `.tar.gz` elsewhere) instead
  of a raw, extensionless binary. Every asset therefore carries an extension, and the executable bit
  survives the download — a raw GitHub Release asset loses it, forcing a `chmod +x` on every install.
  Asset names change (`esrun-linux-x86-64` → `esrun-linux-x86-64.tar.gz`), so **any installer that
  downloads a release asset by name needs updating** before the next release. There is currently no
  way to opt back into raw binaries; an `archive = "none"` escape hatch can be added if one is
  wanted. `init` no longer offers "attach the raw binaries" — it asks only which format.
- **init** — `init` now offers `skip_publish` instead of leaving it to be hand-written. When a repo
  configures a `build-only` package but other discovered crates remain publishable, those crates are
  listed (pre-selected) and the answer is recorded. This closes a real footgun for binary-only Cargo
  workspaces: library crates carry no `publish = false`, nothing else stopped them, and the first
  `publish` run would have pushed them to crates.io. Skipped packages are still versioned in
  lockstep — only the registry push is suppressed. Repos that publish everything are never asked.

## [0.23.0] - 2026-07-20

- **targets** — Added FreeBSD build targets to the registry: `freebsd` / `x86_64`
  (`x86_64-unknown-freebsd`) and `freebsd` / `aarch64` (`aarch64-unknown-freebsd`), staged as
  `freebsd-x64` / `freebsd-arm64` and released as `<bin>-freebsd-<arch>`. Both are opt-in.

  GitHub hosts no FreeBSD runner, and cross-compiling from Linux does not work off the shelf —
  linking needs FreeBSD base-system libraries Rust does not ship, and `aarch64-unknown-freebsd` is
  tier 3 with no prebuilt `std`. So these targets build **natively inside a FreeBSD guest** on the
  Linux runner via `vmactions/freebsd-vm`, which makes every target the guest's host target and
  sidesteps both problems. `init` generates the full leg (boot, sync, `pkg install -y rust`, build,
  `copyback`). Note that the aarch64 leg is fully emulated and correspondingly slow.
- **targets** — New `vm` field on `[[package.targets]]` (and in the `matrix` JSON) marking a target
  that builds inside a VM guest rather than on the host runner. Host toolchain setup and cross prep
  are gated off for those rows. Generalizes to any `vmactions/<name>-vm` image; only FreeBSD ships
  in the registry today.
- **build** — New `otf-release build --stage-only` flag: skip the toolchain setup and build command
  and stage a binary an earlier step already produced. Required by VM targets (the compile happens
  in the guest, the staging on the host), and usable for any externally-built artifact — a container
  build, a Zig cross-compile, another action. A missing artifact now reports that the expected file
  was never produced instead of implying a build failure that never ran.
- **cargo** — Internal crates pinned in the root `[workspace.dependencies]` table (a `path` dep with
  an explicit `version`, referenced by members via `{ workspace = true }`) now have their version
  pins bumped in lockstep with the workspace version — previously only member `[dependencies]`
  sections were updated, so a workspace using the `[workspace.dependencies]` layout stranded stale
  pins that `cargo update`/publish could not resolve. Inherited `{ workspace = true }` member entries
  are also no longer given a conflicting `version` key. External pins (e.g. `serde`) are untouched.
- **targets** — Added musl (statically linked Linux) build targets to the registry: `linux-musl` /
  `x86_64` (`x86_64-unknown-linux-musl`) and `linux-musl` / `aarch64` (`aarch64-unknown-linux-musl`).
  Keyed under a distinct `linux-musl` OS name so they ship alongside the glibc `linux` targets with
  their own assets (`<bin>-linux-musl-<arch>`); `x86_64` builds self-contained, `aarch64` cross-links
  on the x64 runner. Both are opt-in (off by default) — select them in `init` or add a
  `name = "linux-musl"` target to `release.toml`.
- **github-release** — New first-class CI command `otf-release github-release` that owns the
  build-only release path end-to-end, so the generated `release.yml` no longer embeds inline
  `gh`/`awk`/`jq`/`sed` bash. It reads the package's version through its adapter (the same read
  `check`/`publish` use — never `cargo metadata | jq '.packages[0].version'`, which read whichever
  crate was first rather than the package's own), renders the tag from `tag_format`, builds the
  release body from `github_release_notes` (curated changelog / semantic commits / GitHub-generated,
  falling back to generated notes when a source is empty), renames the staged
  `bin/<stage_as>/<bin>` binaries into OS/arch assets (`<bin>-<os>-<arch>[.ext]`), and creates the
  Release idempotently. The generated `github-release-<pkg>` job is now a thin, stable call like the
  registry `publish` job — no `# edit me` version line. `otf-release upgrade` regenerates it into an
  existing workflow.
- **github-release/config** — Build-only packages can now ship the archives + checksums the old
  hand-written release scripts produced, via new `release.toml` fields: `archive` (`"tar.gz"` /
  `"zip"` / `"auto"` — `.zip` for Windows targets, `.tar.gz` elsewhere) packages each staged binary
  (preserving the executable bit), `include` bundles extra files (repo-relative paths or globs, e.g.
  `README.md`, `LICENSE`, `types/*.d.ts`) inside each archive, and `checksums = true` attaches a
  combined `sha256sum`-style `checksums.txt`. `init` prompts for these when configuring a build-only
  package. The generated workflow is unchanged — the binary reads them from `release.toml`.
- **init/discover** — Fixed package-name detection for a virtual Cargo workspace scanned from its
  own root: `init` passes `root = "."`, so the root `./Cargo.toml` had a parent of `.` with no
  `file_name()` and an unnamed workspace collapsed to the literal `package`. Discovery now
  canonicalizes to recover the real project directory name.

## [0.22.0] - 2026-07-18

- **publish** — `--exclude-package` (and `--package`) now keep the filtered package in the
  dependency graph as a known, resolvable node instead of dropping it before the graph is built. A
  dependent that references an excluded package — e.g. a JS package pinning a compiler published by
  its own job — no longer fails with `depends on unknown internal package`; the dependency resolves
  normally and the dependent publishes with it intact. The excluded package is simply skipped when
  choosing what to publish.

## [0.21.0] - 2026-07-18

- **workflow/init** — The generated catch-all `publish` job now waits on each dedicated
  `publish-<pkg>` job and gates on their results (`always() && … result != 'failure' && result != 'cancelled'`),
  so a dependent that exact-pins a package built by its own job (e.g. a JS package pinning a
  compiler) can no longer publish before — or despite a failed publish of — the package it pins.
  Added a top-level `concurrency: { group: release, cancel-in-progress: false }` so two quick pushes
  to `main` can't run two publish pipelines at once, and dropped the never-firing Windows install
  steps from every `ubuntu-latest` job (only the build matrix, which can run on Windows, keeps the
  PowerShell branch). `otf-release upgrade` regenerates these fixes into an existing workflow.
- **jsr** — Added native JSR ecosystem support. The tool can now auto-configure JSR/Deno packages, surgically update versions in `deno.json`/`deno.jsonc`/`jsr.json` and in internal workspace dependency ranges within the `"imports"` object, rewrite `workspace:*` specifiers before publication, query package publication state via registry API, and run JSR publishers. During `init`, if the JSR adapter is enabled but no JSR manifest exists, it prompts the user to scaffold a new `jsr.json` with smart default TypeScript entrypoint suggestions (`./src/index.ts`, `./mod.ts`, etc.) based on existing workspace files.

## [0.20.0] - 2026-07-18

- **init/npm** — Added `id-token: write` permission to the generated main release workflow
  when the npm adapter is active, allowing npm packages to publish with OIDC provenance.
- **internal** — Fixed the CI clippy failure (`clippy::too_many_arguments`, denied by `-D warnings`)
  by dropping two always-empty parameters (`needs`, `matrix_pubs`) and their now-dead
  artifact-download code from the workflow publish-job renderer — leftovers from the 0.18.0
  package-local refactor. The generated workflow is unchanged.

## [0.19.0] - 2026-07-16

- **npm/init** — The tool now owns the build for plain npm packages; npm just publishes. For a
  non-matrix npm publish package, `init` auto-detects its `package.json` `scripts.build` (no prompt)
  and generates a `publish-<pkg>` job that runs `npm run build` inline (scoped to the package
  directory) before `npm publish` — dropping the separate build job and cross-job artifact staging
  (`--artifacts-dir`). It also strips npm's pack/publish lifecycle hooks (`prepublish`,
  `prepublishOnly`, `prepack`, `prepare`) from `package.json` surgically, printing what it removed,
  so npm can't re-run a build behind the pipeline. Matrix npm packages (native binaries wrapped in
  npm) keep the build-job + staging path.

## [0.18.0] - 2026-07-12

- **docs** — Added dedicated command references for `upgrade`, experimental snapshot releases,
  and `self-update`, and linked them from the root and documentation indexes.
- **workflow** — Generated release workflows now gate, build, and publish configured packages
  independently. `check --package` and `publish --package` keep a package's matrix and publish job
  isolated, while the fallback publisher excludes packages owned by those local pipelines, so an
  unrelated release can neither trigger their builds nor be blocked by their skipped jobs.
- **docs** — Refactored root `README.md` into a longer landing page: centered title, highlighted
  core rule, quick start with install as step 1, role-based links into `docs/`, grouped
  command/adapter/capability tables with reference links, a collapsible doc index at the bottom,
  a broader tagline (single projects and monorepos, including polyglot setups), and removal of
  stale `--first-release` references.
- **docs** — Removed the status column from command and adapter tables in `README.md`.

## [0.17.0] - 2026-07-03

- **check** — Added `otf-release check`, the CI release gate. It prints `true` when any configured
  package has a real (non-`0.0.0`) version whose tag doesn't exist yet, else `false`, reusing the
  same version/tag logic `publish` ships with. The generated `check-release` job is now the one-line
  `should_release=$(otf-release check)`, replacing hand-rolled bash that read a single sentinel
  package: in a multi-package repo, a bump to any other package was skipped whenever the sentinel's
  tag already existed. Build-only packages are counted too, so a build-only-only release isn't
  missed. `init`/`upgrade` emit the delegated gate (with `fetch-depth: 0` so tags are present).

## [0.16.0] - 2026-07-02

- **internal** — Removed the crate-wide `#![allow(dead_code, unused_variables)]` from the core
  crate so drift warnings (and CI's `-D warnings`) are visible again; removed the one dead local it
  was masking.
- **internal** — Removed the same crate-wide allow from the adapters crate (scoping a test-only
  constructor to test builds), refreshed stale "npm is the only adapter" doc comments now that
  cargo and generic are real, and made `last_tag` prefer the stable tag over a same-core-version
  prerelease so the pick no longer depends on `git tag --list` order.
- **self-update** — Now compares versions semantically instead of by string equality, so a local
  dev build ahead of the latest release (e.g. `0.15.0` vs a `0.14.0` release) no longer "updates"
  and downgrades itself.
- **publish** — `cargo publish` now runs with `--allow-dirty`. `resolve_workspace_links` may edit
  `Cargo.toml` to inject concrete dependency versions right before publish; without this flag any
  such edit would dirty the tree and make cargo refuse to publish mid-run, after earlier crates in
  the graph had already shipped.
- **config** — Added a `default_branch` key to `release.toml` (defaults to `main`). `version` now
  starts a release from, and returns to, this branch, so repos on `master`, `trunk`, or a release
  train branch can use the tool.
- **version** — Iterating a prerelease now matches the channel on its exact first identifier
  instead of a prefix, so bumping channel `rc` against an existing `rc2.1` (or `beta` against
  `beta2`) correctly starts a fresh `rc.0` rather than treating `rc2` as the same channel.
- **version** — Cascade and lockstep-group bump merges are now prerelease-aware: a package reached
  by both a prerelease path (e.g. a peerDep mirroring a `PreMajor` beta) and a stable path no
  longer silently collapses to the stable bump — the prerelease intent wins, so a stable release
  can't ship with an internal range pointing at a `-beta` version. Two conflicting prerelease
  channels reaching one package are now a clear error instead of an order-dependent guess.

## [0.15.0] - 2026-07-02

- **workflow** — Bun-based npm publish jobs now still configure npm registry auth via
  `actions/setup-node`, so `otf-release publish` can run `npm publish` with `NPM_TOKEN`.

## [0.14.0] - 2026-07-02

- **upgrade** — Regenerated npm release workflows now use the repo's detected package manager
  instead of falling back to `npm ci`, fixing Bun/pnpm/Yarn repos without `package-lock.json`.
- **version** — Release review now groups selected packages, dependency-rule bumps, dependency
  range updates by package and dependency section, and the exact branch/commit created after
  confirmation.
- **version** — After creating and pushing the release branch, the command now prints a completion
  summary before PR and cleanup prompts so the next prompt has clear context.

## [0.13.0] - 2026-07-01

- **version** — Grouped bump prompts now print an explicit selected/skipped summary after each
  group, and changelog rewrite errors include the package name and changelog path.
- **version** — The grouped bump prompt now keeps an `Other release types` path so prerelease
  and graduate releases remain available after the changeset-style stable release selection.
- **version** — Auto-bumped packages with a missing changelog file or missing `[Unreleased]`
  section now receive the `_Dependency updates._` changelog stub instead of aborting during
  changelog rewrite; CLI errors also print their cause chain.

## [0.12.0] - 2026-07-01

- **version** — After pushing the release branch, the command now asks before switching back to
  `main`, pulling tags, and deleting the local release branch; declining still prints the manual
  post-release cleanup commands.
- **version** — The release review confirmation footer now shows explicit inline `Yes | No`
  choices instead of only key hints.
- **version** — Package bump selection now follows a changeset-style flow: choose all major
  packages, then minor, then patch, with a select-all option in each group.
- **npm** — Lockfile refresh now uses the repo's detected package manager instead of always
  running `npm install --package-lock-only`, fixing Bun/pnpm/Yarn workspaces that use
  `workspace:` ranges.

## [0.11.0] - 2026-07-01

- **version** — First releases no longer require a global `--first-release` override; publishable
  packages without prior tags are allowed when they have release notes, and `skip_publish` can
  exclude packages that should not be managed by the tool.

## [0.10.0] - 2026-07-01

### Fixed
- **release assets** — GitHub Release binaries are now named as public downloads with the binary
  name, OS, and architecture (for example `otf-release-linux-x86-64`) instead of leaking internal
  staging directories such as `darwin-arm64` or `win32-x64.exe`.
- **installer** — `install.sh` and `install.ps1` now request standardized public release asset
  names first, then fall back to legacy release asset names so existing published releases remain
  installable during the naming migration.
- **workflow** — Generated release workflows now use `install.sh` on Linux/macOS and `install.ps1`
  on Windows, so Windows runners no longer execute the Unix installer through Git Bash.

## [0.9.0] - 2026-07-01

### Fixed
- **installer** — `install.sh` and `install.ps1` now download deterministic release assets through
  GitHub's `/releases/latest/download/...` redirect instead of querying the unauthenticated
  GitHub API, avoiding CI failures from API rate limits.
- **init/config** — Tag-format prompts now present common formats as selectable options with
  custom input still available; `init` also suggests a format from existing repo tags and preserves
  the detected pattern as `legacy_tag_formats` when the user edits it to migrate schemes.
- **version** — Added `legacy_tag_formats` so repos can migrate tag schemes without rewriting old
  tags; preflight and generated changelog notes read both current and legacy formats while new
  release tags still use `tag_format`.

## [0.8.0] - 2026-06-30

### Fixed
- **workflow** — Generated matrix build jobs now omit the cross-toolchain install step when none
  of the selected targets need cross-compilation, avoiding a no-op step in native-only matrices.
- **version** — After pushing the release branch and opening or skipping PR creation, the command
  now prints next-step commands to return to `main` and delete the local release branch.
- **workflow** — Regenerated the repository release workflow and updated its matrix package
  config to the current `otf-release build` placeholders, so post-release pushes skip existing tags
  and real releases stage binaries correctly.
- **workflow** — Generated release workflows now use the npm package manifest path discovered
  during `init` to read versions directly, avoiding a large inline workspace-scanning script.
- **init** — Reworded the package build selection prompt around built artifacts so binary-backed
  packages are clearer without implying every package needs a generic build step.

## [0.7.0] - 2026-06-30

### Fixed
- **workflow** — Generated npm release workflows now detect Bun, pnpm, and Yarn lockfiles, use
  Node 24 for Node-based package managers, and use the matching setup/install command instead of
  always running `npm ci`.

## [0.6.0] - 2026-06-30

### Fixed
- **workflow** — An npm matrix package set to `mode = "build-only"` no longer publishes a
  binary-less package: `build-only` is meaningless for npm (its per-platform binaries ship inside
  the tarball, not as GitHub Release assets), so an npm + matrix package is now always routed
  through the publish job — which `needs` the build, merges the per-target artifacts into
  `.artifacts/<pkg>`, and runs `otf-release publish --artifacts-dir` — and gets no cosmetic
  GitHub Release.
- **workflow** — `check-release` now skips when the release tag already exists on the remote, so
  ordinary pushes to `main` (docs, chores) don't re-run the full cross-platform build.
- **workflow** — Removed the stray `# edit me: where the version lives` comment when the version
  read is a generated npm/cargo/generic-manifest command, and dropped a redundant
  `download-artifact` in the publish job when only matrix packages feed it.

### Changed
- **init** — Made the setup flow self-explanatory: a short intro, an inline hint under every prompt
  (what it means and its consequence), placeholders, and pre-filled defaults you can edit or accept
  with Enter. Also corrected the generic Rust/matrix command/artifacts defaults to use the
  `{triple}`/`{ext}`/`{bin}` placeholders (they still showed stale GitHub `${{ matrix.* }}` syntax).
- **init** — An npm package is no longer offered the `build-only` mode: its prebuilt binaries ship
  inside the npm tarball, so it is always `publish`. `build-only` (standalone binaries on a GitHub
  Release) is now only prompted for cargo/generic packages, and its label clarifies that.
- **init** — Default-selected build targets are now the five widely-supported platforms
  (`linux-x64/arm64`, `darwin-x64/arm64`, `win32-x64`); `win32-arm64` and 32-bit targets remain in
  the registry for explicit opt-in (they are rarely in a package's resolver set and need extra
  cross-setup).
- **version** — The final release review is now an interactive full-screen TUI (raw mode +
  scrollable, color-coded plan: green = publishing, yellow = cascade, dim = range-only/private)
  with `y`/`n` keys, replacing the static boxed text + line prompt.
- **ui** — Applied a consistent accent theme to every `inquire` prompt across `init`, `config`, and
  `version` (prompt markers, selected/highlighted options, checkboxes, help text).

### Fixed
- **installer** — `install.sh` derives the release tag and constructs the asset URL deterministically
  (robust to minified API JSON), so `self-update` no longer downloads the release API object and
  refuses to install. `install.ps1` uses a JSON parser and was unaffected.

## [0.5.0] - 2026-06-30

### Added
- **matrix/build commands** — Added `otf-release matrix` (emits the GitHub Actions build matrix
  from `release.toml`) and `otf-release build --package --target` (cross-compiles one target and
  stages its binary). The generated matrix workflow now drives both, so cross-compiled binary
  packages build and publish with no hand-edited YAML.
- **target registry** — `[[package.targets]]` now reconciles the Rust triple, the CI runner, and
  the Node `process.platform-process.arch` stage directory (`stage_as`), plus `ext`/`cross`. A
  hand-written file may list just `name`/`arch`; the built-in registry fills the rest. Added
  per-package `bin_name` and `compress` (brotli) fields.

### Changed
- **init** — The matrix workflow is regenerated as a dynamic `matrix-<pkg>` → `build-<pkg>` →
  `publish` DAG that calls `otf-release matrix`/`build`, removing the `# edit me` target list and
  the untemplated build command. Staged binaries land at `bin/<platform>-<arch>/<bin>[.br]`, the
  exact path an npm package's install-time resolver reads.
- **init** — Removed snapshot tag prompting and `snapshot.yml` generation from the setup flow;
  snapshot releases remain available through the dedicated `snapshot` command.
- **changelog config** — Added `changelog_scope` with strict root-level or per-package changelog
  modes, updated `init` to ask only where release notes are maintained, and made package-scope
  GitHub Release bodies combine notes from all configured package changelogs.

### Fixed
- **publish** — A `matrix` publish-mode package is now refused if its per-platform binaries were
  not staged under `--artifacts-dir`, replacing the removed `private:true` guard so a binary-less
  package can never reach the registry.
- **npm adapter** — A prerelease version publishes under its own dist-tag (`1.2.3-dev.<hash>` →
  `--tag dev`) instead of `latest`.
- **init** — Unified the npm auth secret to `NPM_TOKEN` across the release and snapshot workflows.
- **publish** — Made tag creation and GitHub Release creation idempotent so interrupted publish
  runs can be resumed without failing on already-created remote state.
- **cargo adapter** — Treated missing `cargo info` package results as unpublished and aligned the
  workspace MSRV to Rust 1.82.
- **generic adapter** — Tightened version-field matching so separators must directly follow the
  configured version key.
- **installers** — Prevented Unix and PowerShell install scripts from clobbering an already-running
  `otf-release` binary before the replacement download is ready.

## [0.4.0] - 2026-06-29

### Added
- **config/init** — Added `github_release_notes` to choose GitHub Release body content for
  build-only packages: GitHub-generated notes, the curated `CHANGELOG.md` release section, or a
  semantic-style commit list since the previous matching configured tag. The option is prompted
  during `init` and editable through `config`.

### Changed
- **config** — Normalized this repo's `release.toml` by writing default global settings
  explicitly and expanding build targets into standard TOML tables.

### Fixed
- **generic adapter** — Cleaned up Cargo manifest version-field matching to satisfy clippy without
  changing behavior.

## [0.3.0] - 2026-06-29

### Added
- **config** — Added global `tag_format` to `release.toml` (default `v{version}`) and exposed it
  in `init` and `config`, so preflight, publish, and generated GitHub Release jobs use the repo's
  configured tag convention instead of an implicit package-scoped format.

### Fixed
- **version** — Modified `git checkout -b` to `git checkout -B` so that release branch creation gracefully handles previously abandoned branches by resetting them instead of crashing.
- **version** — Removed the startup `gh` confirmation prompt and moved confirmation to a final
  review that shows the computed plan and changed-file stats before commit/push/PR.
- **init/npm** — npm workspace discovery now skips workspace manifests that are not release
  packages because they lack `name` or `version`, prints each skipped manifest with the reason,
  and still fails on malformed `package.json` files.
- **generic adapter** — `Cargo.toml` manifests with `version_field = "version"` now read and bump
  `[workspace.package].version` (or `[package].version`) instead of failing on root Cargo
  workspaces.

## [0.2.0] - 2026-06-28

### Added
- **version** — Added interactive pre-release channel selection (stable, alpha, beta, rc). Choosing a pre-release channel unlocks the new `prerelease` bump strategy for iterating tags, and automatically formats transitions from stable to pre-release (e.g., `1.0.0` to `1.1.0-beta.0`).
- **config** — Added global lifecycle hooks (`pre_version`, `post_version`, `pre_publish`, `post_publish`) to `release.toml`, allowing users to execute custom shell scripts across OS environments during critical release orchestration steps.
- **init** — Emits all known targets in the generated `.github/workflows/release.yml` matrix with unselected ones commented out, allowing users to easily toggle builds on and off.
- **core** — Added automated `install.sh` and `install.ps1` scripts for seamless downloads of GitHub Release assets.
- **docs** — Redesigned the `README.md` into a polished landing page with a commands table and a visual Mermaid workflow diagram.

### Changed
- **core** — Refactored CI target matrix configurations from Rust-specific strings to generic `Target` objects (`{ name, arch }`) making the schema fully language-agnostic.

## [0.1.0] - 2026-06-27

### Added
- **init** — The `generic` adapter wizard is now much smarter: it explicitly asks for the `mode` using a Select menu, prompts for the binary name for Rust projects, provides matrix-aware default commands (`rustup target add ...`), and automatically extracts versions from `.toml` files using `grep`.
- **init** — All `y/n` typing prompts have been replaced with arrow-key `Select` components for a smoother, typing-free UX.
- **upgrade** — A brand new top-level subcommand that updates configurations and regenerates `.github/workflows/release.yml` to match the latest CLI version, allowing you to easily adopt new CI pipeline features (like the new `check-release` job that guards expensive cross-compilation).

### Changed
- **project** — Renamed the workspace from `opentf-release` to `otf-release` across all `Cargo.toml` files, updated authors to "OTF Contributors", and updated the GitHub repository URL.

- **npm adapter** — workspace discovery, format-preserving `package.json` edits (version &
  dependency ranges), `workspace:` link resolution, lockfile refresh, and `is_published` /
  `publish` behind a testable command runner. Keeps the npm gotchas: `--access public`,
  `--no-workspaces`, idempotent `npm view`.
- **cargo adapter** — `Cargo.toml` discovery and format-preserving edits via `toml_edit`,
  `resolve_workspace_links` that injects concrete versions onto path deps for publish,
  `cargo update --workspace` lockfile refresh, and `cargo info` / `cargo publish -p` registry
  calls. Cargo has no peerDep concept, so internal dependents take a patch.
- **Lockstep workspace versioning (cargo)** — crates that inherit `version.workspace = true` are
  bumped by writing the shared `[workspace.package] version` in the root manifest (every
  inheriting crate moves together) and share a **single root `CHANGELOG.md`**. Crates with a
  concrete `[package] version` are still versioned independently.
- **cargo binary-release workflow** — a `build-only` cargo package makes `init` generate a workflow
  that cross-compiles a target matrix (each on a matching runner) and attaches the binaries to a
  **GitHub Release** `vX.Y.Z`, idempotently — **no crates.io**. This is how `otf-release` ships
  itself; `crates/core` and `crates/adapters` are marked `publish = false` so only the binary is
  released.
- **`release.toml` — the committed source of truth.** A new `config` module persists which
  ecosystems are enabled and the per-package build steps. Every command reads it; there is **no
  `--adapter` flag**. See [docs/configuration.md](docs/configuration.md).
- **Per-package `publish` vs `build-only` mode.** `publish` builds then pushes to the ecosystem's
  registry; `build-only` builds then attaches the artifacts to a **GitHub Release** (no registry
  push). A polyglot repo can mix modes and adapters freely.
- **`generic` adapter** — a bring-your-own-commands ecosystem for registries the tool doesn't
  natively support (e.g. Deno's JSR). The version is read/bumped from a manifest you name
  (`manifest` + `version_field`, the git-tag source); an optional `publish` command (e.g.
  `npx jsr publish`) makes it `publish` mode, otherwise it's build-only. The generated workflow
  injects no toolchain for generic build steps and marks registry toolchain/secrets `# edit me`.
- **Generic package auto-discovery** — instead of hand-typing a manifest path, `init` scans the
  repo for recognized manifests carrying a version and presents them in a multi-select to import,
  inferring each package's name + version (single project or monorepo). Generic is the *custom-way*
  path, so the scan spans **all** project types — `Cargo.toml`, `package.json`, `deno.json`/
  `jsr.json`, `pyproject.toml`, `composer.json`, `gleam.toml`, `mix.exs` — not just ecosystems
  without a native adapter (cargo-workspace-inherited versions, build output, and hidden dirs are
  skipped). You can still add packages by hand. See `crates/core/src/discover.rs`.
- **Single unified `publish` job** — the generated workflow now emits one `publish` job (running
  `otf-release publish` once across all enabled adapters) instead of per-registry jobs, setting up
  only the toolchains the active registries need.
- **Modern interactive prompts** — `init` and `version` now use arrow-key select, spacebar
  multi-select, and confirm prompts (via `inquire`) instead of typing numbers.
- **Changelog engine** — Keep a Changelog parser/rewriter: read `[Unreleased]`, move it under a
  dated `## [x.y.z] - YYYY-MM-DD` section, leave a fresh `[Unreleased]`, stub auto-bumped-only
  packages with `_Dependency updates._`.
- **Dependency graph** — topological sort (Kahn, cycle-reporting) and a transitive, max-merged
  bump cascade that mirrors peerDeps and terminates at private leaves.
- **Strict preflight gate** — derives state from the last `name@x.y.z` tag, counts commits since
  it scoped to the package directory, and aborts (all-or-nothing) when changed/selected/
  first-release packages have an empty `[Unreleased]`.
- **`version` command** — interactive local flow: discover → preflight → select + bump →
  cascade → summary → dry-run/confirm → branch guard (clean + on `main`) → apply (versions,
  ranges incl. private apps, changelogs, lockfile) → commit → push → open PR. Side effects are
  behind `Prompt` / `GitOps` / `Forge` traits.
- **`publish` command** — non-interactive CI flow: topological, idempotent (`is_published`
  skip), halt-on-failure with forward-resume; tags and optional GitHub Releases from the dated
  changelog section. Staged binaries attached only when `<artifacts-dir>/<pkg>/` exists.
  **Skips `build-only` packages** (they ship via the GitHub Release the workflow creates).
- **`init` command** — interactive setup (no flags): multi-select adapters (`npm`, `crates.io`),
  then per package its mode, build matrix, command, and artifacts. Persists `release.toml` and
  generates a single `.github/workflows/release.yml` from it — a `build-<pkg>` job per build step
  feeding an `npm-publish` / `cargo-publish` job (registry) and/or a `github-release` job
  (build-only). Both writes guarded by `--force`.
- **Docs** — `docs/` reference tree (architecture, commands, adapters, changelog format,
  preflight, CI workflow, roadmap) and a phased implementation plan.
- **CI** — `.github/workflows/ci.yml` running `cargo fmt --check`, `clippy -D warnings`, and the
  test suite.

### Notes

- Nothing has been released yet; the first tagged release will move these notes into a dated
  section. The tool ships **its own binary** via a GitHub Release (cross-OS artifacts), generated
  by `init --adapter cargo` and tagged on merge — **not** published to crates.io. See
  [docs/ci-workflow.md](docs/ci-workflow.md).
