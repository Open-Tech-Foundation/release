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

## Keys

**Esc goes back.** It abandons whatever prompt is open and returns to the menu above it, writing
nothing — the edit in progress is dropped, not saved. Menus that show a *Back* row treat Esc as
that row.

At the root menu there is nothing above to go back to, so Esc arms the exit and says so; press it
again to leave. A stray press on the way out of a submenu therefore cannot end the session, and
leaving never requires hunting for the *Exit* row.

Ctrl-C still quits immediately, from anywhere.

## Editable Areas

- lifecycle hooks;
- enabled ecosystems;
- configured package build fields;
- generic package fields;
- global settings: provider, snapshot tag, skip-publish packages, publish ignore paths, tag
  format, changelog scope/strategy, and GitHub Release notes.

## New packages show up under *Packages*

The *Packages* menu re-reads the repo every time it opens. A package the enabled adapters find that
has no `[[package]]` block is listed alongside the configured ones, marked `[new]`:

```
? Which package?
> @opentf/esrun-postgres
  @opentf/esrun-redis
  es-runtime-cli
  es-runtime-lsp [new]
  Back
[new] = in this repo but not yet in release.toml — pick one to release it or skip it for good
```

Listing is all it does. Nothing is written to `release.toml`, and no manifest is touched, until you
pick the `[new]` entry and answer:

- **Release it** — writes its block, with the build command its adapter detects (an npm package
  declaring `scripts.build` gets `command = "npm run build"`, and npm's own pack/publish lifecycle
  hooks are stripped so they cannot re-run the build behind the pipeline), or identity-only when its
  publish needs no build. The pick then falls through into the normal field editor, so mode, build
  targets and the rest are set in the same visit.
- **Skip it** — records it in `skip_publish`. This repo will not version or publish it, and it stops
  being offered here.
- **Back** — decides nothing. It is still `[new]` next time.

Opening the menu, or backing out of it, therefore leaves the file byte-for-byte as it was: a package
that is not ready to release cannot be adopted by looking at a list.

Packages already in `skip_publish`, and ones their manifest marks unpublishable (`publish = false`,
`"private": true`), are never offered — the repo has already answered for those.

## Enabling an ecosystem configures its packages

Confirming the *Ecosystems* menu is the bulk path: it reconciles every block at once, adopting each
package the enabled adapters discover and reporting every change. Without this, enabling an
ecosystem left a repo it could not release from — the packages were discovered but had no blocks, so
no build step ran and there was nowhere to scope a per-package `tag_format`, which is what makes two
independently versioned packages collide on one tag.

Blocks that already exist are never rewritten, by either path: they hold decisions this cannot
re-derive, such as a build matrix or a scoped tag format. Re-running is therefore a no-op. Removal
is deliberately narrow — a block goes only when its ecosystem is switched off or the package moves
into `skip_publish`, never merely because one discovery run came back without it, so a transiently
empty discovery cannot delete a hand-tuned build matrix.

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
