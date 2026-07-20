# `github-release` — attach build-only binaries to a GitHub Release

A non-interactive helper the generated workflow runs in **CI** for `build-only` packages. It is the
binary twin of [`publish`](./publish.md): where `publish` pushes a package to a registry,
`github-release` attaches a package's cross-compiled binaries to a GitHub Release. It exists so the
generated `release.yml` never embeds a wall of inline bash — the `github-release-<pkg>` job is a
thin, stable call, exactly like the registry `publish` job. Implemented in
`crates/core/src/github_release.rs`.

```
otf-release github-release [--package <name>] [--artifacts-dir <dir>] [--dry-run]
```

- `--package` — which build-only package to release. Optional when the repo has exactly one; the
  generated workflow always passes it.
- `--artifacts-dir` — root of the staged-artifact tree (`.artifacts/`) the build jobs uploaded.
  Omit for a package with no build step (a Release with no attached assets).
- `--dry-run` — resolve the plan and print it; create nothing.

## What it does

For each selected build-only package, in order:

1. **Reads the version** from the package's manifest via its adapter — the *same* read `check` and
   `publish` use, so the tag can never drift. (This replaces the old inline
   `cargo metadata --no-deps | jq '.packages[0].version'`, which read whichever crate happened to be
   first, not the package's own.)
2. **Renders the tag** from `release.toml`'s `tag_format`.
3. **Skips idempotently** if a release for that tag already exists — a re-run after a partial
   failure fills in nothing it already shipped.
4. **Builds the release body** from the global `github_release_notes` setting:
   - `curated-changelog` → the package's dated `## [version]` section from its `CHANGELOG.md`
     (root file in root scope, the package's own in package scope);
   - `semantic-commits` → the commit list since the package's previous matching tag;
   - `auto-generate` → GitHub-generated notes.
   A curated/semantic source that turns up empty falls back to GitHub-generated notes rather than
   shipping an empty body.
5. **Packages the staged binaries** — the `bin/<stage_as>/<bin>[.ext]` tree each build leg uploaded —
   into OS/arch-named assets, mapping `darwin`→`macos`, `win32`→`windows`, `x64`→`x86-64`. By
   default each becomes an archive: `esrun-linux-x86-64.tar.gz`, `esrun-macos-arm64.tar.gz`,
   `esrun-windows-x86-64.zip`.
6. **Creates the Release** on `main` with those assets attached.

## Packaging (archives & checksums)

**Build-only binaries ship as archives by default** — `archive = "auto"` is assumed when the key is
absent, so every asset carries an extension and extracts ready to run. (A raw GitHub Release asset
arrives non-executable, forcing a `chmod +x` on every install.) Set the key only to pin one
format for every target, and add `checksums`/`include` as needed, via
[`release.toml`](../configuration.md):

```toml
[[package]]
name      = "esrun"
mode      = "build-only"
matrix    = true
bin_name  = "esrun"
archive   = "auto"                    # the default — tar.gz for Unix targets, zip for Windows
checksums = true                      # attach a combined checksums.txt (SHA-256)
include   = ["README.md", "LICENSE", "types/*.d.ts"]   # bundled inside each archive
```

- **`archive`** — `"tar.gz"`, `"zip"`, or `"auto"` (the default). Each target becomes
  `<bin>-<os>-<arch>.tar.gz` (or `.zip`); the binary keeps its own name inside the archive, with its
  stored mode `755` so it extracts ready to run — the staged binary reaches this job through
  `upload-artifact`, which drops POSIX permissions, so the bit is set here rather than inherited.
  `include` files keep their own mode. There is currently no way to attach a raw, extensionless
  binary.
- **`executable`** — overrides that mode decision. Omit it: the default is "executable unless
  `compress` is set", since a brotli-staged `.br` is data the install step decompresses rather than
  a program. Set `executable = false` for a build-only package whose artifact is a payload
  (`.wasm`, `.jar`) instead of something you run, or `true` to force the bit on.
- **`include`** — extra files bundled beside the binary inside every archive, each keeping its
  repo-relative path (so `types/*.d.ts` stays under `types/`). Globs are expanded from the repo root.
- **`checksums`** — writes a `sha256sum`-style `checksums.txt` (`<hex>  <asset>`) covering every
  attached asset and adds it to the release.

The generated workflow does not change when you adjust `archive`/`include`/`checksums` — the binary
reads them from `release.toml`, so the release job stays the same thin call. `attest` is the
exception: it adds a step and two permissions (below).

## Supply chain: checksums vs. provenance

These solve different problems, and the difference matters:

| | Answers | Defeats |
| --- | --- | --- |
| `checksums = true` | "did this arrive intact?" | truncation, corruption, a partial upload |
| `attest = true` | "was this really built by *you*?" | a swapped asset, a compromised release |

A `checksums.txt` served from the same release is **integrity, not authenticity** — an attacker who
can replace the binary can replace the checksum file beside it. Only provenance is signed by GitHub
with the workflow's OIDC identity, so it cannot be forged by replacing files.

```toml
[[package]]
name      = "esrun"
mode      = "build-only"
checksums = true
attest    = true
```

`attest = true` makes `init` add to the generated workflow:

```yaml
permissions:
  contents: write
  id-token: write
  attestations: write       # sign build provenance for release assets
```

```yaml
      - name: Attest build provenance
        uses: actions/attest-build-provenance@v2
        with:
          subject-path: .artifacts/.flat-<slug>/*
```

That `subject-path` is the directory `github-release` writes its finished assets to. Codegen and the
command derive it from one shared function, so the glob cannot drift and silently sign nothing.
The step runs **after** the release, so a signing outage can't block shipping — the attestation goes
to GitHub's attestation store, not onto the release, so ordering doesn't change what consumers see.

Consumers verify with:

```bash
gh attestation verify otf-release-linux-x86-64.tar.gz --repo Open-Tech-Foundation/release
```

> **Off by default.** Provenance needs the two extra scopes above, so enabling it silently on
> `upgrade` would change a workflow's permissions without asking. `init` proposes it (default yes).
> It is supported on public repositories; private repos depend on your GitHub plan.

> **Upgrading from before archives were the default:** asset names gain an extension
> (`esrun-linux-x86-64` → `esrun-linux-x86-64.tar.gz`). Anything that downloads a release asset by
> name — an install script, a README `curl` line, a Dockerfile — must be updated, and must now
> unpack the archive.

## In the workflow

```yaml
github-release-<pkg>:
  needs: [check-release, build-<pkg>]
  if: needs.check-release.outputs.release_<pkg> == 'true'
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4
      with:
        fetch-depth: 0            # previous tags present for semantic-commit notes
    - uses: actions/download-artifact@v4
      with:
        path: .artifacts
    - name: Install otf-release
      run: curl -fsSL .../install.sh | bash
    - name: Create GitHub Release
      env:
        GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
      run: otf-release github-release --package <pkg> --artifacts-dir .artifacts
```

Auth: the default `GITHUB_TOKEN` with `contents: write`. The tag is created by the Release, on the
merge commit.

## See also

- [publish.md](./publish.md) — the registry twin of this command.
- [matrix-build.md](./matrix-build.md) — how the per-target binaries are built and staged.
- [ci-workflow.md](../ci-workflow.md) — the generated workflow shape.
- [configuration.md](../configuration.md) — `tag_format`, `github_release_notes`, and package modes.
