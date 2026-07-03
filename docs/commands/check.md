# `check` — the CI release gate

A non-interactive helper that answers one question: **does this commit release anything?** It prints
`true` when at least one configured package has a real version whose tag doesn't exist yet, else
`false`. The generated `release.yml` uses it as the `check-release` job so an ordinary push to `main`
(a doc fix, a chore) doesn't spin up the cross-platform build matrix. Implemented in
`crates/core/src/check.rs`.

```
otf-release check
```

## Why it exists

`release.yml` triggers on every push to `main`, but most pushes aren't releases — a release is a
merge of the PR that [`version`](./version.md) opened, which bumps versions in manifests. The gate is
the single place that decides "is this one of those?" Without it, every trivial commit would run the
whole matrix build.

Crucially, `check` is a **thin delegate**, not a re-implementation. It reuses the exact primitives
[`publish`](./publish.md) uses:

- `discover_packages` — each package's current version, read from its manifest by the adapter (the
  same code `publish` versions with), so there is no hand-rolled `jq`/`node`/`cargo` read in YAML;
- `format_tag` — the `{name}@{version}` tag, from the same `tag_format`;
- `git tag` — whether that tag already exists.

Because the gate and the publish share one code path, the gate can never drift from what actually
ships.

## The decision, per package

A package makes the gate return `true` when **all** of these hold:

1. it is publishable — private apps and `skip_publish` packages are excluded (CI never releases them);
2. its version is not the `0.0.0` unreleased sentinel;
3. its `{name}@{version}` tag does **not** exist yet.

Every package is evaluated against *its own* version and *its own* tag, so a repo where only one
package bumped (while the others are unchanged and already tagged) still releases — the bug the
single-sentinel gate used to have. **Build-only** packages count too: unlike `publish`, which skips
them (they ship via the GitHub Release the same run creates), the gate must see their bump or a
build-only-only release would be skipped.

## In the workflow

```yaml
check-release:
  runs-on: ubuntu-latest
  outputs:
    should_release: ${{ steps.check.outputs.should_release }}
  steps:
    - uses: actions/checkout@v4
      with:
        fetch-depth: 0            # so the release tags are present to compare against
    - name: Install otf-release
      run: curl -fsSL .../install.sh | bash
    - id: check
      run: echo "should_release=$(otf-release check)" >> "$GITHUB_OUTPUT"
```

`fetch-depth: 0` matters: the tag comparison is against **local** tags, and a shallow checkout
carries none. Every downstream job is gated on
`if: needs.check-release.outputs.should_release == 'true'`.

## See also

- [publish.md](./publish.md) — the per-package idempotent publish the gate mirrors.
- [ci-workflow.md](../ci-workflow.md) — the generated workflow shape and its dependency DAG.
- [configuration.md](../configuration.md) — `tag_format`, `skip_publish`, and package modes.
