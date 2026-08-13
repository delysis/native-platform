# native-platform

This repository is the Delysis first-party Rust platform monorepo. Wave 3
imported the accepted `llama-native-kit` history beneath `crates/native`.
Wave 4 imported the accepted Free Token Energy history beneath `products/fte`
and moved its packages into the root Rust and pnpm workspaces.

The native import preserves all 45 accepted source commits without squashing.
`migration/ledger.json`, the two import receipts, and their commit maps bind
each source, deterministic rewrite, ancestry-preserving merge, and path
cutover. Other first-party products remain exact Git integration inputs; their
source and release manifests have not been moved.

## Workspace policy

- Rust 1.92.0, edition 2024, resolver 3.
- One root `Cargo.lock` and one root `pnpm-lock.yaml`.
- Central dependencies and lints in `Cargo.toml`.
- Explicit package groups: portable, platform, product, research, diagnostic,
  and real-hardware.
- `xtask policy` authenticates the workspace, migration evidence, and copied
  ADRs.

Run the local gates with:

```text
./scripts/check-shell-policy.sh
```

`tests/integration-current` consumes accepted product revisions through exact
Git dependencies while resolving native crates from `crates/native`. It runs
shared contract and vertical-authentication checks and records the precise
split-graph boundary in `tests/integration-current/COVERAGE.md`.
