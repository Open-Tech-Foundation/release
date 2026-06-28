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
2. **List publishable packages** (discovered across the enabled npm/cargo adapters), then
   **multi-select**: *"Which need a build step before publish?"*
3. For each selected package, prompt for:
   - **mode** — `publish` (build, then push to the ecosystem's registry) or **`build-only`**
     (build, then attach the artifacts to a **GitHub Release** — no registry push);
   - **adapter** (only asked if more than one is enabled);
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

## `release.toml`

The committed source of truth. Every other command (`version`, `publish`) reads it instead of
taking an `--adapter` flag. See [configuration.md](../configuration.md) for the full schema.

```toml
adapters = ["crates.io"]

[[package]]
name      = "opentf-release"
adapter   = "crates.io"
mode      = "build-only"          # artifacts -> GitHub Release, no registry push
matrix    = true
targets   = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin", "x86_64-pc-windows-msvc"]
command   = "cargo build --release -p opentf-release --target ${{ matrix.target }}"
artifacts = "target/${{ matrix.target }}/release/otf-release*"
```

## Generated workflow shape

From the config, `init` emits jobs:

- a **`check-release`** job that decides whether downstream jobs should run;
- a **`build-<pkg>`** job per package with a build step (a matrix when *build matrix* is yes);
- a single **`publish`** job when registry publishing is enabled — runs `otf-release publish`
  once, and the CLI loops the enabled adapters internally;
- a **`github-release`** job when any package is `build-only` — attaches its staged artifacts to a
  GitHub Release `vX.Y.Z`, idempotently. The default `GITHUB_TOKEN` + `contents: write`.

## Explicit caveats (surfaced to the user)

- **Matrix triples can't be fully inferred.** `init` writes a sensible default plus a
  `# edit me` marker; tuning them is your job.
- **Repo-specific build steps are yours to refine.** `init` wires the DAG, secrets, and your
  build command/artifacts; the exact runner-per-target and version-discovery line carry
  `# edit me` markers.

## Relationship to the single-workflow model

`init` is the generator; [ci-workflow.md](../ci-workflow.md) is the shape of what it produces
and *why*. Read them together when setting up a repo.

## See also

- [configuration.md](../configuration.md) — the `release.toml` schema.
- [ci-workflow.md](../ci-workflow.md) — the generated workflow, explained.
- [commands/publish.md](./publish.md) — the command the `publish` job runs.
