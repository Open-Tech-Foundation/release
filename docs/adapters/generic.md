# generic adapter

The **bring-your-own-commands** adapter — for handling a project *your own way* instead of an
adapter's predefined behavior. That's orthogonal to project type: reach for it when the cargo/npm
adapters' built-in flow doesn't fit, or for an ecosystem with no native adapter at all (e.g. Deno's
**JSR**). Instead of hardcoded registry knowledge, you describe the package in
[`release.toml`](../configuration.md) and the tool still gives you versioning, a curated changelog,
a release PR, and a publish/release workflow scaffold. Implemented in
`crates/adapters/src/generic.rs`.

## How it works

- **Version** lives in a manifest you name — `manifest` (e.g. `deno.json`) and `version_field`
  (defaults to `version`). The adapter reads it (this is the **git-tag source**) and bumps it
  **in place** with a targeted text replace, preserving the file's formatting. Works for any
  `"version": "x.y.z"` (JSON) or `version = "x.y.z"` (TOML) style manifest.
  For a root `Cargo.toml`, the legacy `version_field = "version"` setting also reads/writes
  `[package].version` or `[workspace.package].version`; explicit paths such as
  `workspace.package.version` work too.
- **Publish** is an optional shell command — `publish` (e.g. `npx jsr publish`). When set, the
  package is `publish` mode and ships through `otf-release publish`, which runs your command and
  then tags + creates the GitHub Release. When omitted, the package is `build-only`.
- **Build** is optional — `command` + `artifacts`, like any other adapter, for staging files.
- **No dependency graph or ranges** — those trait methods are no-ops.
- **Lockfile** is normally a no-op. If a generic package versions a root `Cargo.toml` and
  `Cargo.lock` exists, `otf-release version` runs `cargo update --workspace` so the lockfile is
  refreshed in the release commit.

> ### ⚠️ Not for a Cargo workspace with internal path dependencies
>
> "No dependency graph or ranges" is the catch. If your root `Cargo.toml` pins internal crates —
>
> ```toml
> [workspace.dependencies]
> my-core = { path = "crates/core", version = "0.9.0" }
> ```
>
> — the generic adapter bumps `[workspace.package] version` and **leaves those pins at the old
> version**, because `update_dep_range` is a no-op here. The workspace then fails to resolve at all:
>
> ```
> error: failed to select a version for the requirement `my-core = "^0.9.0"`
> candidate versions found which didn't match: 0.10.0
> ```
>
> Use the [**cargo** adapter](./cargo.md) instead — it reconciles those pins. You do **not** have to
> publish to crates.io to use it: set `mode = "build-only"` on the package, and list any library
> crates in `skip_publish` (`init` offers this automatically). The generic adapter is the right
> choice for a Cargo project only when it has no internal path dependencies.

## `release.toml`

```toml
adapters = ["generic"]

# A JSR library: build, then publish with a manual command.
[[package]]
name = "my-lib"
adapter = "generic"
mode = "publish"
manifest = "deno.json"      # holds the version (the tag source)
version_field = "version"
command = "deno task build" # optional
artifacts = "dist/*"        # optional
publish = "npx jsr publish" # the manual publish command
```

`init` doesn't make you type the manifest path. When the generic adapter is enabled it **scans the
repo** for recognized manifests that carry a version and infers each package's name + current
version (single project or a monorepo of many), then presents them in a multi-select to **import** —
for each you supply only the optional build/artifacts and publish command. Because generic is about
handling a project *your own way* (not a specific registry), the scan spans **all** project types,
not just unsupported ones:

| Manifest | Detected as |
| --- | --- |
| `Cargo.toml` | Rust / Cargo |
| `package.json` | Node / npm |
| `deno.json` · `deno.jsonc` · `jsr.json` | Deno / JSR |
| `pyproject.toml` | Python / PyPI |
| `composer.json` | PHP / Packagist |
| `gleam.toml` | Gleam / Hex |
| `mix.exs` | Elixir / Hex |

A `Cargo.toml`/`package.json` shows up here too — pick the generic adapter when you want custom
commands for it instead of the cargo/npm adapter's built-in flow. (A crate that inherits
`version.workspace = true` has no literal version and is skipped.) You can still **add packages by
hand** for anything the scan misses, or edit `release.toml` directly; for a root Cargo workspace
manifest, `manifest = "Cargo.toml"` with `version_field = "version"` tracks
`[workspace.package].version`. Scanning skips `node_modules`, `target`, hidden dirs, and other
build output. Implemented in `crates/core/src/discover.rs`.

## In the generated workflow

- The generic `build-<pkg>` job (only if a `command` is set) injects **no language toolchain** —
  your command brings its own — runs it, and uploads `artifacts`.
- If a `publish` command is set, the unified `publish` job runs `otf-release publish` (which runs
  your command). The toolchain/secret your registry needs can't be inferred, so those steps carry
  `# edit me` markers.
- If there's no `publish` command, the package is build-only and its artifacts go to the
  `github-release` job; the version is read from your `manifest`.

## See also

- [configuration.md](../configuration.md) — the `release.toml` schema.
- [adapters/overview.md](./overview.md) — the `Adapter` trait these methods implement.
- [ci-workflow.md](../ci-workflow.md) — the generated workflow's shape.
