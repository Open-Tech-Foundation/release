# `otf-release init`

**Interactive workflow generator. Emits exactly one `.github/workflows/release.yml`.**

```
otf-release init [--adapter npm|cargo] [--force]
```

| Flag | Effect |
| --- | --- |
| `--adapter` | Which ecosystem the generated workflow targets (`npm` default, or `cargo`). |
| `--force` | Overwrite an existing `release.yml` without prompting. |

Implemented in `crates/core/src/init.rs`. **No config is ever persisted — the generated YAML
is the single source of truth.**

## What it does, step by step

1. **List publishable packages**, then **multi-select**: *"Which need binary artifacts built
   before publish?"* — these become the **asset packages**.
2. For each asset package, **prompt for target triples** (a sensible default set, each marked
   with a `# edit me` comment).
3. **Emit `release.yml`** in the shape for `--adapter`:
   - **npm** — a `build-matrix` job (asset packages only) feeding a **`publish` job** that runs
     `otf-release publish` over the whole topological set, with `NODE_AUTH_TOKEN`.
   - **cargo** — a `build-matrix` job that cross-compiles each target on a matching runner,
     feeding a **`release` job** that creates a **GitHub Release** `vX.Y.Z` with the binaries
     attached. **No crates.io publish**; auth is the default `GITHUB_TOKEN` with
     `contents: write`. The artifacts are how users install the binary per OS.
4. **Idempotent**: re-running warns before overwrite (`--force` to replace). The generated
   YAML is an **editable scaffold**, not a tool-managed file — re-running does not fight your
   edits.

## Explicit caveats (surfaced to the user)

- **Matrix triples can't be fully inferred.** `init` writes a sensible default plus a
  `# edit me` marker; tuning them is your job.
- **Repo-specific build steps are yours to add.** The scaffold wires up the DAG and secrets,
  not your project's exact build commands.

## Relationship to the single-workflow model

`init` is the generator; [ci-workflow.md](../ci-workflow.md) is the shape of what it produces
and *why* (one workflow, proper DAG, asset packages as first-class citizens, no `private`
guard hack). Read them together when setting up a repo.

## See also

- [ci-workflow.md](../ci-workflow.md) — the generated workflow, explained.
- [commands/publish.md](./publish.md) — the command the `publish` job runs.
