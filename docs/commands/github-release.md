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
5. **Renames the staged binaries** — the `bin/<stage_as>/<bin>[.ext]` tree each build leg uploaded —
   into OS/arch-named assets: `<bin>-<os>-<arch>[.ext]`, mapping `darwin`→`macos`, `win32`→`windows`,
   `x64`→`x86-64` (e.g. `esrun-linux-x86-64`, `esrun-windows-x86-64.exe`, `esrun-macos-arm64`).
6. **Creates the Release** on `main` with those assets attached.

## Packaging (archives & checksums)

By default each staged binary is attached as a raw, renamed file. A build-only package can instead
ship archives and a checksums file — the features the hand-written release scripts used — via
[`release.toml`](../configuration.md):

```toml
[[package]]
name      = "esrun"
mode      = "build-only"
matrix    = true
bin_name  = "esrun"
archive   = "auto"                    # tar.gz for Unix targets, zip for Windows
checksums = true                      # attach a combined checksums.txt (SHA-256)
include   = ["README.md", "LICENSE", "types/*.d.ts"]   # bundled inside each archive
```

- **`archive`** — `"tar.gz"`, `"zip"`, or `"auto"`. Each target becomes `<bin>-<os>-<arch>.tar.gz`
  (or `.zip`); the binary keeps its own name inside the archive, with its executable bit preserved.
- **`include`** — extra files bundled beside the binary inside every archive, each keeping its
  repo-relative path (so `types/*.d.ts` stays under `types/`). Globs are expanded from the repo root.
- **`checksums`** — writes a `sha256sum`-style `checksums.txt` (`<hex>  <asset>`) covering every
  attached asset and adds it to the release.

The generated workflow does not change when you enable these — the binary reads them from
`release.toml`, so the release job stays the same thin call.

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
