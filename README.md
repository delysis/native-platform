# native-platform

This repository is the empty Wave 2 shell for Delysis first-party Rust platform
and product code. It establishes workspace policy, architecture records,
migration provenance, and CI before any production source or history is
imported.

The initial shell makes no product, runtime, hardware, model, provider, or
migration-equivalence claim. `migration/ledger.json` is the authority for future
imports; every `import_commit` and `path_dependency_cutover_commit` is currently
`null`.

## Workspace policy

- Rust 1.92.0, edition 2024, resolver 3.
- One root `Cargo.lock` and one root `pnpm-lock.yaml`.
- Central dependencies and lints in `Cargo.toml`.
- Explicit package groups: portable, platform, product, research, diagnostic,
  and real-hardware.
- `xtask policy` authenticates the shell, migration ledger, and copied ADRs.

Run the local gates with:

```text
./scripts/check-shell-policy.sh
```

The next authorized phase is W2 integration. Production import and history
movement remain later, separately reviewed operations.
