# TUI configuration coverage

Every supported `release.toml` schema field is editable through `release config`. The tables below
are the inventory checked alongside persistence, editor reachability, and workflow-generation tests.
After changing generated workflow behavior, run `release upgrade`.

| Repository configuration | TUI location |
| --- | --- |
| `provider`, `default_branch` | Repository → Provider / Default branch |
| `tag_format`, `legacy_tag_formats`, `snapshot_tag` | Repository → Tag format / Legacy tag formats / Snapshot tag |
| `otf_release_version` | Repository → Tool version |
| `secrets.npm`, `secrets.cargo` | Registry secrets → npm token secret / Cargo token secret |
| `changelog_scope`, `changelog_strategy`, `github_release_notes` | Changelog → Scope / Strategy / GitHub Release notes |
| `adapters`, `skip_publish` | Ecosystems → Enabled / Never publish |
| `discovery.npm` | Ecosystems → npm package directories |
| `publish.ignore_paths` | Ecosystems → Publish ignore paths; also each package screen |
| `hooks.pre_version`, `post_version`, `pre_publish`, `post_publish` | Hooks → corresponding command list |
| `setup` | Build setup → step list; add/remove steps |
| `package` | Packages → existing/discovered package; Add package for manual definitions |

| Package configuration | Package screen |
| --- | --- |
| `name`, `adapter`, `mode` | Name / Adapter / Mode |
| `command`, `artifacts` | Build command / Artifacts |
| `matrix`, `targets` | Matrix build / Build targets / Target details |
| `bin_name`, `compress`, `manifest` | Binary name / Compression / Manifest |
| `archive`, `include`, `executable` | Release assets → Archive format / Included files / Executable |
| `checksums`, `attest` | Release assets → Checksums / Build provenance |
| `provenance` | npm → Provenance |
| `tag_format`, `legacy_tag_formats`, `changelog` | Release identity → Tag format / Legacy tag formats / Changelog |
| `version_field`, `publish` | Generic adapter → Version field / Publish command |
| `setup` | Build setup → own/inherited step list; Use repository setup restores inheritance |
| `env` | Build environment (KEY=value entries) |

Release asset controls appear for effective build-only packages. npm provenance appears for effective
npm publish packages, including matrix npm packages. Generic version and publish fields appear for
generic packages. These conditions follow how the release engine uses the settings.

| Nested configuration | Detail screen |
| --- | --- |
| `targets[].name`, `arch` | Target details → OS name / Architecture |
| `targets[].triple`, `runner`, `stage_as`, `ext` | Rust triple / Runner / Stage directory / Extension |
| `targets[].cross`, `vm` | Cross compile / Build in VM; default retains registry behavior |
| `setup[].uses`, `with`, `run`, `targets`, `jobs` | Setup step → Action / Action inputs / Script / Targets / Jobs |

Known choices use pickers. Custom tag formats, target definitions, manifests, commands, and globs can
be entered without editing TOML manually. Entry lists preserve embedded commas, command order, and
multiline command strings. The target picker preserves configured target overrides.

TUI saves compare the old and new schema values and patch the original TOML syntax tree. Unchanged
comments, quote styles, formatting, and extension keys are retained; edited values may be formatted
by the TOML editor. A no-op save preserves the document exactly.
