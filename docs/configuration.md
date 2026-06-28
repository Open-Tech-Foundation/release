# `release.toml`

The committed **source of truth** for a repo's release setup. Written by
[`init`](./commands/init.md); read by [`version`](./commands/version.md) and
[`publish`](./commands/publish.md). There is **no `--adapter` flag** — the enabled ecosystems
live here. The file is plain, hand-editable TOML; parsed by `crates/core/src/config.rs`.

## Schema

```toml
# Ecosystems enabled for this repo (multi): "npm", "crates.io", "generic".
adapters = ["npm", "crates.io"]

# Global lifecycle hooks (optional). Array of shell commands executed in order.
[hooks]
pre_version = ["npm run lint", "node scripts/validate.js"]
post_version = ["python3 scripts/sync-docs.py"]
pre_publish = ["npm run test"]
post_publish = ["curl -X POST ..."]

# Zero or more packages that need a build step before publish/release.
# A publishable package with no entry here is published as-is by its adapter (no build).
[[package]]
name      = "web-compiler"        # the name the adapter discovers
adapter   = "crates.io"           # which enabled ecosystem it belongs to
mode      = "build-only"          # "publish" | "build-only"
matrix    = true                  # build across a target matrix?
targets   = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]  # only when matrix = true
command   = "cargo build --release -p otfw_cli"
artifacts = "target/*/release/otfwc*"

[[package]]
name      = "docs-site"
adapter   = "npm"
mode      = "publish"
command   = "npm run build"
artifacts = "dist/**"
```

## Fields

| Key | Meaning |
| --- | --- |
| `adapters` | Enabled ecosystems: `"npm"`, `"crates.io"`, `"generic"`. Drives which publish/release jobs `init` generates. |
| `[[package]]` | A package with an explicit build step. |
| `name` | The package name as discovered by its adapter. |
| `adapter` | The owning ecosystem (`"npm"` / `"crates.io"` / `"generic"`). |
| `mode` | `"publish"` → build then push to the registry. `"build-only"` → build, then attach artifacts to a GitHub Release; **never** pushed to a registry. Generic packages can use either mode when a `publish` command is configured. |
| `matrix` | `true` builds across `targets` (multiple platforms); `false` is a single runner. |
| `targets` | Cross-compile triples (only when `matrix = true`). |
| `command` | The build command CI runs. |
| `artifacts` | A glob of artifacts to stage for publish / attach to the release. |

## The `generic` adapter

For an ecosystem the tool doesn't natively support (e.g. Deno's JSR), enable `"generic"` and
describe the package yourself. The version lives in a **manifest you name** (`manifest` +
`version_field`, default `version`) — that's the git-tag source, bumped in place. A `publish`
command (e.g. `npx jsr publish`) is **optional**: set it for `publish` mode, omit it for
build-only. `init` asks for these (or edit `release.toml` directly). The generic-only fields are:

| Key | Meaning |
| --- | --- |
| `manifest` | File holding the version (e.g. `deno.json`). Required for a generic package. |
| `version_field` | The version key inside `manifest` (default `version`). |
| `publish` | Optional shell command that publishes to the registry. Omit ⇒ build-only. |

See [adapters/generic.md](./adapters/generic.md).

## How the commands use it

- **`version`** acts on every enabled adapter as one release transaction — all publishable
  packages (both modes) are versioned, changelog-rolled, committed, pushed, and opened as one PR.
- **`publish`** acts on every enabled adapter but **skips `build-only` packages** — those ship
  via the GitHub Release the workflow creates, not through a registry.

## See also

- [commands/init.md](./commands/init.md) — the interactive flow that writes this file.
- [ci-workflow.md](./ci-workflow.md) — the workflow generated from it.
