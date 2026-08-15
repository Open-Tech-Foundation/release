# `otf-release doctor`

**Audit the release setup. Read-only.**

```
otf-release doctor            # report; exits non-zero if any error was found
otf-release doctor --strict   # also exit non-zero on warnings
```

Every other command acts at one moment: `check` gates a push, `version` cuts a release, `publish`
ships it. The failures that hurt most are the ones none of them can see at that moment — a
`release.toml` that parses, generates a workflow, and runs green while quietly shipping nothing.

`doctor` inspects the committed setup against what the adapters actually discover on disk, and
reports what breaks and how to fix it. It writes no files and touches no registry.

## Severity

Severity is about **consequence**, not confidence.

| Level | Meaning |
| --- | --- |
| **error** | A release will silently ship the wrong thing, or not ship at all. Exits non-zero. |
| **warning** | A release will work, but something is off that will bite later. |
| **suggestion** | A practice worth adopting; nothing is broken. |
| **info** | The resolved facts — every package, its tag, and how it ships. |

## Checks

| Code | Level | What it catches |
| --- | --- | --- |
| `tag-collision` | error | Two or more released packages share a `tag_format` with no `{name}`. They format to the same tag as soon as their versions meet, and [`github-release`](./github-release.md) then skips the second as already-shipped — attaching no assets, with no error. |
| `tag-collision-now` | error | Two packages already resolve to the same tag at their current versions. Only the first to reach the forge ships. |
| `missing-package-block` | error | A released package has no `[[package]]` block, so it has no build step and nowhere to scope its tag format or changelog. |
| `stale-package-block` | error | A block names a package no enabled adapter discovers. Its generated jobs release nothing. |
| `unbuilt-publish` | error | A package whose manifest declares a build script has no `command` in its block, so it is published without ever being built — for a package whose `files` points at `dist/`, an empty tarball. |
| `discovery-matches-nothing` | error | A `[discovery] npm` entry matches no package. The list *is* the member set, so whatever it named is not released at all — and a glob matching nothing raises no error anywhere else. |
| `missing-changelog` | warning | Released packages have no changelog file. Under the curated strategy their `[Unreleased]` is empty by definition, so they are never offered for release. Reported once, listing every package. |
| `shared-changelog` | warning | Packages at different versions all write notes into one file, interleaving them under a single heading. A lockstep group sharing one changelog is *not* flagged — that is the point of lockstep. |
| `matrix-without-targets` | warning | A `matrix = true` package has no `[[package.targets]]`, so its build fans out to nothing. |
| `inert-tool-pin` | error | `otf_release_version` predates `v0.26.0`, the first release whose installer reads `OTF_RELEASE_VERSION`. The workflow fetches that older script, which always downloads the *latest* release — so CI does not build with the pinned version at all. |
| `unparseable-tool-pin` | warning | `otf_release_version` is not a version tag. |
| `old-tool-pin` | suggestion | CI builds with an older tool than the binary you are running locally. |
| `no-checksums` | suggestion | A build-only package ships assets with no `checksums.txt`, so a download cannot be verified as intact. |
| `no-attestation` | suggestion | A build-only package ships assets with no signed provenance. A checksum can be replaced by whoever replaced the asset; an attestation cannot. |
| `stale-workflow` | error | `.github/workflows/release.yml` has no job for a configured package. It will be versioned and tagged, then build nothing. Run `otf-release upgrade --force`. |
| `shared-legacy-tag-format` | warning | A repo-wide `legacy_tag_formats` entry has no `{name}` while several packages are released, so every package reads the same release history. |
| `setup-action-missing` | error | A `[[setup]]` step points at `uses: ./…` with no `action.yml` in the repo. GitHub resolves the path against the checkout and fails the job at startup, before doing any work. A published `owner/repo@v1` is resolved by GitHub, so it is not checked against disk. See [build-setup.md](../build-setup.md). |
| `setup-targets-unknown` | warning | A `setup.targets` triple that no package the step applies to builds. It never matches `matrix.triple`, so the step is skipped on every row and the build fails later, at the command that needed the tool. |
| `setup-targets-never-runs` | warning | A `setup.targets` filter on a step no matrix package receives. The filter selects matrix rows and there are none, so the step is emitted in no job at all. |
| `setup-targets-redundant` | suggestion | A `setup.targets` filter naming every triple those packages build, so it selects nothing. |
| `empty-ignore-paths` | suggestion | One or more `publish.ignore_paths` entries have an empty glob list, which does nothing. Reported once for all of them. |

## In CI

`doctor` exits non-zero on any error, so it works as a gate:

```yaml
- run: otf-release doctor
```

Use `--strict` to fail on warnings too, once the repo is clean enough to hold that line.

## See also

- [configuration.md](../configuration.md) — every setting `doctor` reads.
- [preflight.md](../preflight.md) — the changelog/tag gate that runs *during* a release.
