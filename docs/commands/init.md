# `otf-release init`

**Interactive workflow generator. Emits exactly one `.github/workflows/release.yml`.**

```
otf-release init [--force]
```

| Flag | Effect |
| --- | --- |
| `--force` | Overwrite an existing `release.yml` without prompting. |

Implemented in `crates/core/src/init.rs`. **No config is ever persisted — the generated YAML
is the single source of truth.**

## What it does, step by step

1. **Detect** ecosystems present in the repo (v1: npm).
2. **List publishable packages**, then **multi-select**: *"Which need binary artifacts built
   before publish?"* — these become the **asset packages**.
3. For each asset package, **prompt for target triples** (a sensible default set, each marked
   with a `# edit me` comment).
4. **Emit `release.yml`** with:
   - a **`build-matrix` job** — only if any asset packages were selected — that cross-compiles
     the matrix and uploads artifacts;
   - a **`publish` job** — `needs: build-matrix`; downloads artifacts into `.artifacts/`; runs
     `otf-release publish`, which releases the **whole topological set** (libraries *and* asset
     packages), attaching staged binaries where present;
   - the right **secrets** per ecosystem (`NODE_AUTH_TOKEN` for npm; `CARGO_REGISTRY_TOKEN`
     later).
5. **Idempotent**: re-running warns before overwrite (`--force` to replace). The generated
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
