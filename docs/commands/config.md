# `otf-release config`

**Interactive editor for `release.toml`.**

```
otf-release config
```

The command updates the committed config file only. If you edit settings that are baked into
generated workflows, such as `tag_format` or `github_release_notes`, run:

```
otf-release upgrade --force
```

to regenerate `.github/workflows/release.yml`. See [upgrade.md](./upgrade.md).

## Editable Areas

- lifecycle hooks;
- enabled ecosystems;
- configured package build fields;
- generic package fields;
- global settings: provider, snapshot tag, skip-publish packages, publish ignore paths, tag
  format, changelog scope/strategy, and GitHub Release notes.

## Enabling an ecosystem configures its packages

Confirming the *Ecosystems* menu reconciles `[[package]]` blocks with what the enabled adapters
discover, and reports every change. A package that this repo releases but has no block gets one:
with the build command its adapter detects (an npm package declaring `scripts.build` gets
`command = "npm run build"`, and npm's own pack/publish lifecycle hooks are stripped so they cannot
re-run the build behind the pipeline), or identity-only when its publish needs no build.

Without this, enabling an ecosystem left a repo it could not release from — the packages were
discovered but had no blocks, so no build step ran and there was nowhere to scope a per-package
`tag_format`, which is what makes two independently versioned packages collide on one tag.

Blocks that already exist are never rewritten: they hold decisions this cannot re-derive, such as a
build matrix or a scoped tag format. Re-running is therefore a no-op. Removal is deliberately
narrow — a block goes only when its ecosystem is switched off or the package moves into
`skip_publish`, never merely because one discovery run came back without it, so a transiently empty
discovery cannot delete a hand-tuned build matrix.

Re-open *Ecosystems* and confirm after adding a package to the repo, to pick it up.

Enabling **npm** for a repo that declares its members nowhere — neither a root `workspaces` field
nor `pnpm-workspace.yaml` — scans for `package.json` files
carrying a `name` and a `version`, lists them, and saves the ones you confirm to `[discovery] npm`
in `release.toml` — publishable packages start checked, private ones (apps, fixtures) do not.
Re-running it re-scans and starts from what is already declared, so a package added later shows up.
Repos that already declare their members skip this: that declaration stays the single source of
truth. See the [npm adapter](../adapters/npm.md#repos-that-declare-no-npm-workspace).

Tag format editing offers the common patterns `v{version}`, `{version}`, `{name}@{version}`, and
`{name}@v{version}`, plus custom input.

`github_release_notes` controls the body of GitHub Releases created for `build-only` packages:
`auto-generate`, `curated-changelog`, or `semantic-commits`.

`publish.ignore_paths` is edited package-by-package from the global settings menu; the prompt stores
comma-separated glob patterns for the selected package without requiring manual TOML edits.

Under *Packages*, alongside the build fields, each package has a **Tag format** and a **Changelog**
of its own — for a package that must not share the repo's tag line or changelog scope. Every
publishable package has a `[[package]]` block, so all of them are reachable here, including ones
with no build step. Each prompt names the repo-wide value it would otherwise inherit, so leaving it
blank visibly means "whatever the repo does"; answers are validated before saving. See
[configuration.md](../configuration.md#scoped-release-identity).
