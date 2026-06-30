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

to regenerate `.github/workflows/release.yml`.

## Editable Areas

- lifecycle hooks;
- enabled ecosystems;
- configured package build fields;
- generic package fields;
- global settings: provider, snapshot tag, tag format, changelog scope/strategy, and GitHub
  Release notes.

`github_release_notes` controls the body of GitHub Releases created for `build-only` packages:
`auto-generate`, `curated-changelog`, or `semantic-commits`.
