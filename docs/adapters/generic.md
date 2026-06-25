# generic adapter

The **bring-your-own-commands** adapter, for an ecosystem the tool doesn't natively support yet —
e.g. Deno's **JSR**. Instead of hardcoded registry knowledge, you describe the package in
[`release.toml`](../configuration.md) and the tool still gives you versioning, a curated
changelog, a release PR, and a publish/release workflow scaffold. Implemented in
`crates/adapters/src/generic.rs`.

## How it works

- **Version** lives in a manifest you name — `manifest` (e.g. `deno.json`) and `version_field`
  (defaults to `version`). The adapter reads it (this is the **git-tag source**) and bumps it
  **in place** with a targeted text replace, preserving the file's formatting. Works for any
  `"version": "x.y.z"` (JSON) or `version = "x.y.z"` (TOML) style manifest.
- **Publish** is an optional shell command — `publish` (e.g. `npx jsr publish`). When set, the
  package is `publish` mode and ships through `otf-release publish`, which runs your command and
  then tags + creates the GitHub Release. When omitted, the package is `build-only`.
- **Build** is optional — `command` + `artifacts`, like any other adapter, for staging files.
- **No dependency graph, lockfile, or ranges** — those trait methods are no-ops.

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

`init` asks for these interactively (name, manifest, version field, optional build command /
artifacts, optional publish command), or you can edit `release.toml` directly.

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
