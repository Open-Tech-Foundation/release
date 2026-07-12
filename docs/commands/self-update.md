# `otf-release self-update`

**Checks GitHub Releases and reinstalls when a newer CLI version is available.**

```
otf-release self-update
```

Implemented in `crates/cli/src/self_update.rs`.

## What it does

1. Query `https://api.github.com/repos/Open-Tech-Foundation/release/releases/latest` for the
   newest release tag.
2. Compare that version to the running binary.
3. If already up to date, print a message and exit.
4. Otherwise rerun the official install script:
   - **macOS / Linux** — `install.sh` via `curl | bash`
   - **Windows** — `install.ps1` via `irm | iex`

This is the same install path documented in the root [`README.md`](../../README.md#-quick-start).

## See also

- Root [README](../../README.md) — install commands for first-time setup.
- [upgrade.md](./upgrade.md) — refresh the repo's generated workflow after updating the CLI (separate
  from updating the binary itself).