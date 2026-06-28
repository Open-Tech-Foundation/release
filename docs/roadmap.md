# Roadmap and Known Gaps

This page tracks gaps in the current implementation. The root
[`README.md`](../README.md) is the product-facing overview; this file is the short engineering
roadmap.

## Highest priority

| Area | Gap | Recommendation |
| --- | --- | --- |
| Generated CI | `release.yml` is editable scaffolding and still needs stronger validation. | Add workflow shape tests and ensure required setup/install steps are generated. |

## Next

| Area | Gap | Recommendation |
| --- | --- | --- |
| Snapshot releases | `snapshot` exists, but its multi-adapter behavior and failure model need a clear contract. | Keep it experimental until documented and tested end to end. |
| Generic manifests | Generic version parsing is simple text matching. | Use structured JSON/TOML parsing when the file type is known. |

## Later

| Area | Direction |
| --- | --- |
| Additional adapters | PyPI, Maven, Go, and other ecosystems can fit behind the existing `Adapter` trait. |
| Additional forges | GitLab, Bitbucket, Gitea, and Codeberg need provider-specific PR/release implementations. |
| Release PR automation | A bot-maintained release PR could be added later; the current flow is local `version` to PR. |
