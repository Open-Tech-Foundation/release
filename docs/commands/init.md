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

1. **Choose adapters** (multi-select): `1) npm  2) crates.io`. The enabled set is recorded in
   `release.toml`; a polyglot repo can enable both.
2. **List publishable packages** (discovered across the enabled adapters), then **multi-select**:
   *"Which need a build step before publish?"*
3. For each selected package, prompt for:
   - **mode** — `publish` (build, then push to the ecosystem's registry) or **`build-only`**
     (build, then attach the artifacts to a **GitHub Release** — no registry push);
   - **adapter** (only asked if more than one is enabled);
   - **build matrix?** — if yes, the **target triples** (a default set, each marked `# edit me`);
   - the **build command** and the **artifacts** glob to stage.
4. **Persist `release.toml`** and **generate `release.yml`** from it. Both writes are guarded:
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

- a **`build-<pkg>`** job per package with a build step (a matrix when *build matrix* is yes);
- an **`npm-publish`** job when npm is enabled — runs `otf-release publish` (publishes only
  `publish`-mode packages);
- a **`cargo-publish`** job *only* if a cargo package opts into `mode = "publish"` (crates.io);
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
- [commands/publish.md](./publish.md) — the command the `npm-publish`/`cargo-publish` jobs run.
