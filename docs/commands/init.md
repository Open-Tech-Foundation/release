# `otf-release init`

**Interactive setup. Writes `release.toml` (the source of truth) and generates one
`.github/workflows/release.yml` from it.**

```
otf-release init [--force]
```

| Flag | Effect |
| --- | --- |
| `--force` | Overwrite existing `release.toml` / `release.yml` without prompting. |

`init` takes **no `--adapter` flag** — it asks. Implemented in `crates/core/src/init.rs`.

## What it does, step by step

1. **Choose adapters** (spacebar multi-select): `npm`, `crates.io`, `generic`. The enabled set is
   recorded in `release.toml`; a polyglot repo can enable several.
2. **Auto-configure npm packages** (the tool owns the build; no prompt). For each publishable npm
   package, `init` reads its `package.json`: if it declares a `scripts.build`, the package gets an
   **inline-build publish entry** (`npm run build` runs in the package's own publish job — no
   separate build job or artifact staging), and npm's pack/publish lifecycle hooks (`prepublish`,
   `prepublishOnly`, `prepack`, `prepare`) are **stripped** from `package.json` (with a printed
   notice) so npm can't re-run a build behind the pipeline. See
   [adapters/npm.md](../adapters/npm.md). npm workspace manifests that are not release packages
   (for example fixture or benchmark folders without a `version`) are skipped and listed with the
   reason.
3. **List publishable cargo packages**, then **multi-select**: *"Which packages need built
   artifacts before publish?"* (npm packages are handled in step 2, so they are not offered here.)
   For each selected package, prompt for:
   - **mode** — `publish` (build, then push to the ecosystem's registry) or **`build-only`**
     (build, then attach the artifacts to a **GitHub Release** — no registry push);
   - **build matrix?** — if yes, the **target triples** (a default set, each marked `# edit me`);
   - the **build command** and the **artifacts** glob to stage.
4. **For the generic adapter** (if enabled): `init` **scans the repo** for recognized manifests that
   carry a version and presents them in a multi-select to **import** — so you don't hand-type
   manifest paths (single project or monorepo). Generic is the *custom-way* path, so the scan spans
   **all** project types (`Cargo.toml`, `package.json`, `deno.json`, `pyproject.toml`, …), not just
   ones lacking a native adapter. Per imported package you supply only the optional build/artifacts
   and publish command; you can also **add packages by hand**. See
   [adapters/generic.md](../adapters/generic.md).
5. **Persist `release.toml`** and **generate `release.yml`** from it. Both writes are guarded:
   re-running warns before overwrite (`--force` to replace).
6. **Choose a global git tag format** from common options: `v{version}`, `{version}`,
   `{name}@{version}`, or `{name}@v{version}` (plus custom input). `init` inspects existing local
   tags and marks the matching pattern as suggested when it can. With no tags, multi-package repos
   default to `{name}@{version}` to avoid tag collisions. If you edit a detected pattern to migrate
   schemes, the detected pattern is saved as `legacy_tag_formats` so release history still works.
7. **Choose where release notes are maintained**: one root `CHANGELOG.md`, or per-package
   `CHANGELOG.md` files.
8. **Choose what GitHub Release descriptions contain** for `build-only` packages:
   auto-generated GitHub notes, curated changelog notes, or a semantic-style commit list since the
   previous matching configured tag. In package-level changelog scope, curated GitHub Release
   notes combine the released sections from all configured packages.

## `release.toml`

The committed source of truth. Every other command (`version`, `publish`) reads it instead of
taking an `--adapter` flag. See [configuration.md](../configuration.md) for the full schema.

```toml
adapters = ["crates.io"]
changelog_scope = "root"

[[package]]
name      = "opentf-release"
adapter   = "crates.io"
mode      = "build-only"          # artifacts -> GitHub Release, no registry push
matrix    = true
targets   = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin", "x86_64-pc-windows-msvc"]
command   = "cargo build --release -p opentf-release --target ${{ matrix.target }}"
artifacts = "target/${{ matrix.target }}/release/otf-release*"

# "auto-generate" | "curated-changelog" | "semantic-commits"
github_release_notes = "auto-generate"
```

## Generated workflow shape

From the config, `init` emits jobs:

- a **`check-release`** job that decides whether downstream jobs should run. It is a one-liner —
  `should_release=$(otf-release check)` — delegating to the binary like every other job, so it can't
  drift from what actually ships. `check` returns `true` if **any** configured package has a real
  version whose tag doesn't exist yet (`publish`/`github-release` are per-package idempotent and skip
  the rest); it needs `fetch-depth: 0` so the tags are present to compare against;
- a **`build-<pkg>`** job per package with a build step (a matrix when *build matrix* is yes);
- a single **`publish`** job when registry publishing is enabled — runs `otf-release publish`
  once, and the CLI loops the enabled adapters internally;
- a **`github-release`** job when any package is `build-only` — attaches its staged artifacts to a
  GitHub Release tagged from `tag_format`, idempotently. The default `GITHUB_TOKEN` +
  `contents: write`. Its release body follows the global `github_release_notes` setting.

For npm repos, generated jobs detect the package manager from the root lockfile: `bun.lockb` /
`bun.lock` use Bun, `pnpm-lock.yaml` uses pnpm, `yarn.lock` uses Yarn, and otherwise npm is used.

## Explicit caveats (surfaced to the user)

- **Matrix triples can't be fully inferred.** `init` writes a sensible default plus a
  `# edit me` marker; tuning them is your job.
- **Repo-specific build steps are yours to refine.** `init` wires the DAG, secrets, and your
  build command/artifacts; the exact runner-per-target and version-discovery line carry
  `# edit me` markers.
- **npm workspace discovery only imports real packages.** A workspace `package.json` must have a
  string `name` and `version` to become a release package. Missing fields are reported as skipped;
  malformed JSON is still treated as a broken manifest and stops the scan.

## Relationship to the single-workflow model

`init` is the generator; [ci-workflow.md](../ci-workflow.md) is the shape of what it produces
and *why*. Read them together when setting up a repo.

## See also

- [configuration.md](../configuration.md) — the `release.toml` schema.
- [ci-workflow.md](../ci-workflow.md) — the generated workflow, explained.
- [commands/publish.md](./publish.md) — the command the `publish` job runs.
