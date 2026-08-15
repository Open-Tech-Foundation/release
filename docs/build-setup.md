# Build setup

**Steps run in every job of the generated workflow, before the work that job does.**
Configured as `[[setup]]` in [`release.toml`](./configuration.md); emitted by
`crates/core/src/init.rs`, modelled in `crates/core/src/config.rs`.

Some repos build and publish through tooling GitHub's runner does not ship and no adapter knows
about: a task runner, a bundler installed by its own `install.sh`, a private toolchain. `[[setup]]`
is the place to install it.

```toml
[[setup]]
uses = "./.github/actions/setup-tsr"
with = { esdev = "true" }

[[setup]]
uses = "./.github/actions/setup-esdev"
```

```yaml
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with:
          node-version: 24
      - uses: ./.github/actions/setup-tsr      # ← [[setup]] #1
        with:
          esdev: "true"
      - uses: ./.github/actions/setup-esdev    # ← [[setup]] #2
      - name: Build @scope/lib
        run: npm run build
```

## Why a step, and not a hook or a longer `command`

Hooks cannot do this. `pre_publish` is executed by `otf-release publish` at runtime, which is
*after* the build step in the same job, so nothing a hook does can provision what the build already
needed. It is also the wrong way round: the hooks are themselves written in the tool a setup step
installs.

Folding the install into a package's `command` does work — the string is emitted verbatim into
`run:` — but it is the wrong home twice over. `command` is also what `otf-release build` runs on a
contributor's machine, so an installer buried there executes outside CI. And everything crammed
into one `run:` block cannot use `$GITHUB_PATH`, whose writes only reach *later* steps, forcing an
inline `PATH=…` prefix on everything that follows.

A step of its own has neither problem. `$GITHUB_PATH` behaves normally and a composite action's
PATH exports reach what follows — the same mechanism that already works in a hand-written workflow,
which is why pointing at an action the repo already has is the preferred form. Re-spelling that
action's installer as `run` lines would fork the definition it exists to keep single.

## Fields

| Key | Meaning |
| --- | --- |
| `uses` | An action to run, written exactly as a workflow would: `./.github/actions/setup-tsr` for a local composite action, `owner/repo@v1` for a published one. |
| `with` | Inputs for `uses`, emitted as the step's `with:` block. Each is passed as a string — composite action inputs are strings even when declared `type: boolean`. |
| `run` | Shell lines run as one step, for a repo with no composite action to point at. Emitted as a single multi-line `run:` block. |
| `targets` | Target triples this step is for. Omit to run it on every row. See [Filtering matrix rows](#filtering-matrix-rows). |

`uses` and `run` are independent: either alone, or both — the action first, then the script.

## Why a list

Real release pipelines do not have one setup step. A polyglot repo's npm packages may build through
a task runner *and* a repo-local CLI, while the Rust matrix beside them needs only the first — two
actions in one job, a different pair in the next.

Folding each combination into a wrapper composite action works, but forks a definition per
combination, which is the thing pointing `uses` at an action the repo already has exists to avoid.
Steps run in the order the blocks appear, so an installer goes before anything that calls what it
installed.

A single `[setup]` table is still accepted and read as a one-step list, so a `release.toml` written
before the list existed keeps working unchanged. The first save from
[`config`](./commands/config.md) rewrites it as `[[setup]]`.

## Every job, once

Builds are not the only place the tool is needed. `pre_publish` / `post_publish` hooks and a generic
package's `publish` command are executed by `otf-release publish` inside a publish job, so a repo
whose hooks are `tsr test` would break if the steps were build-only. They are emitted at most once
per job — an inline-build publish job installs them before its build, and that same set serves the
publish that follows.

## Per package: replace, not append

A `[[package.setup]]` list replaces the repo-wide one in that package's own jobs (`build-`,
`matrix-`, `publish-`, `github-release-`). `setup = []` means "no step at all there". Jobs that
belong to no package — the release gate and the catch-all publish — always use the repo-wide list.

Replacing and not appending is deliberate. The commonest reason a package declares a list is that it
must run *less* than the repo does, and appending cannot express removal — it would leave every
package undoing a step it never asked for. So a repo installs what most of its packages need, and
the exceptions name their own subset:

```toml
[[setup]]                                  # the npm packages build through both
uses = "./.github/actions/setup-tsr"

[[setup]]
uses = "./.github/actions/setup-esdev"

[[package]]
name = "es-runtime-cli"
# … cargo builds need only the task runner
[[package.setup]]
uses = "./.github/actions/setup-tsr"
```

With that config, the generated workflow places the steps like this:

| Job | Steps |
| --- | --- |
| `check-release` (the gate) | belongs to no package → repo-wide list |
| `publish` (catch-all) | belongs to no package → repo-wide list |
| `publish-<npm-pkg>` (inline build) | inherits the repo-wide list |
| `matrix-`, `build-`, `github-release-` for `es-runtime-cli` | its own list |

## Filtering matrix rows

A repo-wide installer is not automatically runnable everywhere its packages build. A task runner
installed by a `curl | bash` script supporting Linux, macOS, and FreeBSD will *fail the job* on the
`windows-latest` leg of a Rust matrix build, at a step that build never needed.

`targets` confines the step to the rows it supports, instead of forcing the whole package to opt out
of a step its other legs want:

```toml
[[package.setup]]
uses = "./.github/actions/setup-tsr"
targets = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
]                                          # … and not the two windows rows
```

```yaml
      - uses: ./.github/actions/setup-tsr
        if: ${{ contains(fromJSON('["x86_64-unknown-linux-gnu", …]'), matrix.triple) }}
```

Because it compiles to a `matrix.triple` test, it selects **matrix rows**. A job with no matrix
builds no triple and has no `matrix.triple` to test, so a step naming `targets` is left out of those
jobs rather than emitted with a guard that could never be true. To drop a package's setup
everywhere, use `setup = []` rather than an empty `targets`.

### Combining with the VM guard

For a matrix package, setup is also gated to host-side rows: a VM target builds inside the guest,
which installs its own toolchain through the VM step's `prepare:`. When both apply the guards are
`&&`-ed:

```yaml
        if: ${{ !matrix.vm && contains(fromJSON('["x86_64-unknown-linux-gnu"]'), matrix.triple) }}
```

## What `doctor` checks

All four are silent in CI, which is why they are checked here. See
[`commands/doctor.md`](./commands/doctor.md).

| Code | Severity | What it catches |
| --- | --- | --- |
| `setup-action-missing` | error | A `uses: ./…` path with no `action.yml` in the repo. GitHub resolves it against the checkout and fails the job at startup, before doing any work. A published `owner/repo@v1` is resolved by GitHub, so it is not checked against disk. |
| `setup-targets-unknown` | warning | A `targets` triple that no package the step applies to builds. It never matches `matrix.triple`, so the step is skipped on every row and the build fails later, at the command that needed the tool. |
| `setup-targets-never-runs` | warning | A `targets` filter on a step that no matrix package receives. It selects matrix rows, and there are none, so the step is emitted in no job at all. |
| `setup-targets-redundant` | suggestion | A `targets` filter naming every triple those packages build, so it selects nothing. |

## Editing it

[`init`](./commands/init.md) asks for setup when some package has a build command — a package with
no build has no step for a setup step to precede — and keeps asking until you say there are no more
steps.

[`config`](./commands/config.md) → *Build setup* edits the list afterwards: one row per field per
step, numbered once there is more than one, with an *Add step* row at the end. Blanking a step's
action *and* its script removes it. On a package's screen the rows show what it inherits, labelled
`(repo default: …)`, until you edit one — at which point the package gets a list of its own, seeded
from what it was already receiving.

After a hand edit, run [`upgrade`](./commands/upgrade.md) to regenerate the workflow.

## See also

- [configuration.md](./configuration.md) — the full `release.toml` schema.
- [ci-workflow.md](./ci-workflow.md) — the single `release.yml` model and its job anatomy.
- [commands/doctor.md](./commands/doctor.md) — the audit that reports the four codes above.
