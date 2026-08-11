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

Enabling **npm** for a repo that declares no `workspaces` of its own scans for `package.json` files
carrying a `name` and a `version`, lists them, and saves the ones you confirm to `[discovery] npm`
in `release.toml` — publishable packages start checked, private ones (apps, fixtures) do not.
Re-running it re-scans and starts from what is already declared, so a package added later shows up.
Repos whose root `package.json` already declares `workspaces` skip this: that declaration stays the
single source of truth. See the [npm adapter](../adapters/npm.md#repos-that-declare-no-npm-workspace).

Tag format editing offers the common patterns `v{version}`, `{version}`, `{name}@{version}`, and
`{name}@v{version}`, plus custom input.

`github_release_notes` controls the body of GitHub Releases created for `build-only` packages:
`auto-generate`, `curated-changelog`, or `semantic-commits`.

`publish.ignore_paths` is edited package-by-package from the global settings menu; the prompt stores
comma-separated glob patterns for the selected package without requiring manual TOML edits.
