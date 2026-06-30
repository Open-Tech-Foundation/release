# `matrix` and `build` — the matrix CI helpers

Two non-interactive helpers the generated workflow uses to build a **matrix package** (one that
ships a cross-compiled binary per platform, e.g. a Rust binary wrapped in an npm package). They
exist so the workflow never hand-maintains a target list and the tool — not YAML — owns the
reconciliation between the Rust triple, the CI runner, and the Node stage directory. Implemented in
`crates/core/src/matrix.rs` and `crates/core/src/build.rs`.

## Why a CLI split

A single CLI run can't span OS runners, so the work is split into pieces the workflow wires up with
`needs:` and a matrix:

```
matrix-<pkg>   →   otf-release matrix --package <pkg>   (emit the matrix, once, on ubuntu)
build-<pkg>    →   otf-release build  --package <pkg> --target <name>/<arch>   (per runner)
publish        →   otf-release publish --artifacts-dir .artifacts             (gather + push)
```

## `otf-release matrix`

```
otf-release matrix [--package <name>]
```

Prints the GitHub Actions matrix as JSON, read straight from `release.toml`:

```json
{"include":[
  {"name":"linux","arch":"aarch64","triple":"aarch64-unknown-linux-gnu","runner":"ubuntu-latest","ext":"","cross":true,"stage_as":"linux-arm64"},
  {"name":"windows","arch":"x86_64","triple":"x86_64-pc-windows-msvc","runner":"windows-latest","ext":".exe","cross":false,"stage_as":"win32-x64"}
]}
```

`--package` is optional when exactly one matrix package exists. Each entry carries every reconciled
fact, so the build leg needs no further lookups. The workflow consumes it with
`strategy.matrix: ${{ fromJSON(needs.matrix-<pkg>.outputs.matrix) }}`.

## `otf-release build`

```
otf-release build --package <name> --target <name>/<arch>
```

Runs inside one matrix leg. It:

1. installs the Rust target (`rustup target add <triple>`) for cargo builds,
2. exports the cross linker env (`CARGO_TARGET_<TRIPLE>_LINKER`) when the target is `cross`,
3. runs the package's `command` with `{triple}`/`{ext}`/`{bin}` expanded for this target,
4. copies — brotli-compressing when `compress = "brotli"` — the built binary to
   `.artifacts/<package>/bin/<stage_as>/<bin><ext>[.br]`.

`<stage_as>` is the Node `process.platform-process.arch` directory the package's install-time
resolver reads. That path is the contract: get it right and every platform's binary lands exactly
where an install looks for it.

## How the pieces meet `publish`

Each leg uploads its `.artifacts/<package>` tree as a separate artifact. The `publish` job merges
them back into `.artifacts/<package>` (`download-artifact` with `merge-multiple: true`), then
`otf-release publish` copies that tree into the package before `npm publish`. A matrix package is
**only** published when its staged binaries are present — `publish` refuses a binary-less push (the
invariant that replaced the old `private:true` guard).

## See also

- [configuration.md](../configuration.md) — the `[[package.targets]]` schema and the target registry.
- [ci-workflow.md](../ci-workflow.md) — the generated workflow shape.
- [adapters/npm.md](../adapters/npm.md) — how staged binaries are packed into the tarball.
