# `release.toml`

The committed **source of truth** for a repo's release setup. Written by
[`init`](./commands/init.md); read by [`version`](./commands/version.md) and
[`publish`](./commands/publish.md). There is **no `--adapter` flag** — the enabled ecosystems
live here. The file is plain, hand-editable TOML; parsed by `crates/core/src/config.rs`.

## Schema

```toml
# Ecosystems enabled for this repo (multi): "npm", "crates.io", "generic".
adapters = ["npm", "crates.io"]

# Global git tag format. Supports {version} and optional {name}.
tag_format = "v{version}"

# Older tag formats to read as release history while writing new tags with tag_format.
# Useful when migrating, e.g. from @scope/pkg@1.2.3 to @scope/pkg@v1.2.4.
legacy_tag_formats = ["{name}@{version}"]

# Publishable packages that otf-release should not version or publish.
skip_publish = ["@scope/internal-tool"]

# Optional: per-package globs that should warn instead of block when only these paths changed.
# Add one entry per package name.
[publish.ignore_paths]
"@scope/docs-site" = ["docs/**", "**/*.test.ts", "**/__tests__/**"]

# GitHub Release body source for build-only packages.
github_release_notes = "auto-generate"

# Global lifecycle hooks (optional). Array of shell commands executed in order.
[hooks]
pre_version = ["npm run lint", "node scripts/validate.js"]
post_version = ["python3 scripts/sync-docs.py"]
pre_publish = ["npm run test"]
post_publish = ["curl -X POST ..."]

# Zero or more packages that need built artifacts before publish/release.
# A publishable package with no entry here is published as-is by its adapter.
[[package]]
name      = "@opentf/web-compiler"  # the name the adapter discovers
adapter   = "npm"                   # which enabled ecosystem it belongs to
mode      = "publish"               # "publish" | "build-only"
matrix    = true                    # build across a target matrix?
command   = "cargo build --release --target {triple}"   # {triple}/{ext}/{bin} expand per target
artifacts = "target/{triple}/release/otfwc{ext}"        # the binary this target produced
bin_name  = "otfwc"                 # staged as bin/<stage_as>/otfwc<ext>[.br]  (matrix only)
compress  = "brotli"                # decompressed at install time            (matrix only)

# build-only release packaging (read by `github-release`):
archive   = "auto"                  # "tar.gz" | "zip" | "auto" — package each binary (build-only)
checksums = true                    # also attach a combined checksums.txt (SHA-256)
include   = ["README.md", "LICENSE"]  # extra files bundled inside each archive

# One [[package.targets]] table per platform. init fills every field from the built-in
# registry; a hand-written file may list just name/arch and the rest is looked up.
[[package.targets]]
name = "linux"
arch = "aarch64"
triple   = "aarch64-unknown-linux-gnu"
runner   = "ubuntu-latest"
stage_as = "linux-arm64"            # MUST equal Node's process.platform-process.arch
ext      = ""
cross    = true                     # installs the gcc cross linker on the runner

[[package.targets]]
name = "windows"
arch = "x86_64"

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
| `tag_format` | Global git tag format used by `version`, preflight, `publish`, and generated GitHub Release jobs. Must include `{version}`; may include `{name}` for package-scoped tags, e.g. `{name}@{version}`. |
| `legacy_tag_formats` | Optional older tag formats used only to find prior release history during `version`/preflight and generated changelog notes. New tags are still written with `tag_format`. |
| `skip_publish` | Package names never pushed to a registry, even when their manifests look publishable. They are still **versioned** in lockstep with the release — this only suppresses the publish. `init` fills this in automatically: when a repo has a `build-only` package alongside other discovered crates (a Cargo workspace's library crates, say, which carry no `publish = false`), it lists them and records your answer. |
| `publish.ignore_paths` | Optional per-package path globs. If a package has commits since its last tag, `[Unreleased]` is empty, and **every** changed file matches one of these globs, the release flow prints a warning and continues instead of aborting. |
| `changelog_scope` | Where curated release notes live: `"root"` uses the root `CHANGELOG.md` for every package; `"package"` uses each package's adapter-discovered `CHANGELOG.md`. |
| `github_release_notes` | GitHub Release body source for `build-only` packages: `"auto-generate"` lets GitHub generate notes, `"curated-changelog"` copies root notes in root scope or combines released sections from all configured package changelogs in package scope, and `"semantic-commits"` writes a commit list since the previous matching `tag_format` tag. `init` asks for this and `config` can edit it later. |
| `[[package]]` | A package with an explicit build step. |
| `name` | The package name as discovered by its adapter. |
| `adapter` | The owning ecosystem (`"npm"` / `"crates.io"` / `"generic"`). |
| `mode` | `"publish"` → build then push to the registry. `"build-only"` → build, then attach artifacts to a GitHub Release; **never** pushed to a registry. Generic packages can use either mode when a `publish` command is configured. |
| `matrix` | `true` builds across `[[package.targets]]` (multiple platforms); `false` is a single runner. |
| `command` | The build command CI runs. For matrix packages it is templated per target with `{triple}`, `{ext}`, `{bin}`, `{stage_as}`, `{arch}`, `{name}`. |
| `artifacts` | The built binary to stage (matrix: templated like `command`) / a glob to attach to the release. |
| `bin_name` | _(matrix only)_ the compiled binary's base name; staged as `bin/<stage_as>/<bin_name><ext>`. |
| `compress` | _(matrix only)_ `"brotli"` compresses each staged binary to `…<ext>.br` (decompressed at install time); omit to stage raw. |
| `archive` | _(build-only)_ how to package each staged binary: `"tar.gz"`, `"zip"`, or `"auto"`. **Defaults to `"auto"`** (`.zip` for Windows targets, `.tar.gz` elsewhere) — build-only binaries always ship as archives, so every asset carries an extension and the binary extracts ready to run (stored mode `755`). Read by [`github-release`](./commands/github-release.md). |
| `checksums` | _(build-only)_ `true` also attaches a combined `checksums.txt` (SHA-256 of every asset) to the GitHub Release. |
| `include` | _(build-only)_ extra files to bundle **inside each archive** beside the binary — repo-relative paths or globs, e.g. `["README.md", "LICENSE", "types/*.d.ts"]`. Each keeps its path within the archive. |

### Build targets (`[[package.targets]]`)

Each target reconciles the **three** naming systems that describe one physical binary — the Rust
**triple** (cargo), the CI **runner** (GitHub Actions), and the **`stage_as`** directory the Node
`extract.js` resolver reads (`process.platform`-`process.arch`, e.g. `linux-arm64`, `darwin-x64`,
`win32-x64`). Getting `stage_as` wrong is the one mistake that publishes a working-looking package
no install can use, so the tool owns this mapping.

| Key | Meaning |
| --- | --- |
| `name` / `arch` | Generic OS / architecture, e.g. `linux` / `aarch64`. The registry key. |
| `triple` | Rust target triple. Looked up from `name`/`arch` if omitted. |
| `runner` | GitHub-hosted runner OS. Looked up if omitted. |
| `stage_as` | Node `process.platform-process.arch` stage dir. Looked up if omitted. **Must** match what the package resolves at install time. |
| `ext` | Executable extension (`""` or `.exe`). Looked up if omitted. |
| `cross` | Whether the runner needs cross-compile prep (a non-host linker). Looked up if omitted. |
| `vm` | Whether the build runs natively inside a VM guest on the runner (`vmactions/<name>-vm`) instead of on the host. Looked up if omitted. Mutually exclusive with `cross` in practice — the guest brings its own toolchain. |

`init` writes every field; a hand-edited file may give just `name`/`arch` and let the built-in
registry (`crates/core/src/config.rs`) fill the rest.

**musl (static Linux):** use `name = "linux-musl"` with `arch = "x86_64"` or `"aarch64"` for a
statically linked binary that runs on any distro (Alpine included). It is keyed separately from the
glibc `linux` rows so both can ship side by side, with distinct assets
(`<bin>-linux-musl-x86-64` vs `<bin>-linux-x86-64`). `x86_64` links self-contained via
`rustup target add`; `aarch64` cross-links on the x64 runner. Both are opt-in (off by default), e.g.:

```toml
[[package.targets]]
name = "linux-musl"
arch = "x86_64"
```

A crate with C dependencies also needs a musl C toolchain on the runner (e.g. `apt-get install
musl-tools`); add it as a build step or in the package `command`.

**FreeBSD (built in a VM):** use `name = "freebsd"` with `arch = "x86_64"` or `"aarch64"`. Both are
opt-in (off by default):

```toml
[[package.targets]]
name = "freebsd"
arch = "x86_64"
```

GitHub hosts no FreeBSD runner, and cross-compiling from Linux does not work off the shelf: rustc
emits objects fine, but linking needs FreeBSD base-system libraries (`-lexecinfo`, `-lkvm`,
`-lprocstat`, …) that Rust does not ship, and `aarch64-unknown-freebsd` is a tier-3 target with no
prebuilt `std` at all.

So these targets carry `vm = true` and build **natively inside a FreeBSD guest** on the Linux
runner, via [`vmactions/freebsd-vm`](https://github.com/vmactions/freebsd-vm). `init` generates the
whole leg: the guest boots, the checkout syncs in, `pkg install -y rust` provides the toolchain, the
package `command` runs natively, and `copyback` returns the binary to the host, where
`otf-release build … --stage-only` stages it like any other target. Inside the guest every target is
the *host* target, so the tier-3 problem disappears.

> **aarch64 is fully emulated** on an x64 runner and is therefore much slower than the x86_64 leg —
> time a real run before depending on it. `cross` stays `false` for both: the GNU/Linux cross prep
> is the wrong toolchain here.

The same mechanism covers any OS with a `vmactions/<name>-vm` image; only FreeBSD ships in the
registry today.

## The `generic` adapter

For an ecosystem the tool doesn't natively support (e.g. Deno's JSR), enable `"generic"` and
describe the package yourself. The version lives in a **manifest you name** (`manifest` +
`version_field`, default `version`) — that's the git-tag source, bumped in place. A `publish`
command (e.g. `npx jsr publish`) is **optional**: set it for `publish` mode, omit it for
build-only. `init` asks for these (or edit `release.toml` directly). The generic-only fields are:

| Key | Meaning |
| --- | --- |
| `manifest` | File holding the version (e.g. `deno.json` for generic, or a discovered `package.json` path for npm workflow version reads). Required for a generic package. |
| `version_field` | Generic only: the version key inside `manifest` (default `version`). Dot paths like `workspace.package.version` are supported; for root `Cargo.toml`, `version` also maps to `[package].version` or `[workspace.package].version`. |
| `publish` | Optional shell command that publishes to the registry. Omit ⇒ build-only. |

See [adapters/generic.md](./adapters/generic.md).

## How the commands use it

- **`version`** acts on every enabled adapter as one release transaction — all publishable
  packages (both modes) are versioned, changelog-rolled, committed, pushed, and opened as one PR.
- **`publish`** acts on every enabled adapter but **skips `build-only` packages** — those ship
  via the GitHub Release the workflow creates, not through a registry.

## Publish Ignore Paths

`publish.ignore_paths` is keyed by package name, not adapter, so it applies even to packages
that do not have a `[[package]]` build entry. Use it for churn that should not force changelog
notes by itself, for example docs-only and tests-only edits:

```toml
[publish.ignore_paths]
"@scope/pkg-a" = ["docs/**", "**/*.md"]
"@scope/pkg-b" = ["**/*.test.ts", "**/*.spec.ts", "**/__tests__/**"]
```

This is a short-term escape hatch, not a release classifier. Mixed changes still fail if any
non-ignored file changed and `[Unreleased]` is empty.

## See also

- [commands/init.md](./commands/init.md) — the interactive flow that writes this file.
- [ci-workflow.md](./ci-workflow.md) — the workflow generated from it.
