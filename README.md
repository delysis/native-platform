# native-platform

`native-platform` is the canonical Delysis first-party Rust monorepo. It owns
the Native llama boundary, platform contracts, Attachment, Information,
Speech, Free Token Energy, Mom Llama, and Loom.

The migration preserves each accepted source repository as ordinary Git
history under its destination path. `migration/ledger.json`, the commit maps,
and the generic `migration/seal-manifest.json` bind the accepted revisions and
evidence. `delysis/llama-cpp-rs` remains the sole external Delysis Git source:
it is the separately reviewed unsafe upstream boundary.

## Workspace

- Rust 1.92.0, edition 2024, resolver 3.
- All 65 first-party packages are members of one root Cargo workspace.
- One root `Cargo.lock` resolves exact `rusqlite 0.39.0` and one
  `libsqlite3-sys 0.37.0` native link.
- One pnpm 11.16.0 workspace and root `pnpm-lock.yaml` own the FTE and Loom
  frontends.
- `ci/package-groups.json` assigns every package to exactly one primary group
  and optional secondary gates.
- The Attachment fuzz target is the only deliberately excluded auxiliary
  Cargo workspace.

Run the local repository policy with:

```text
./scripts/check-shell-policy.sh
```

Run a package group with:

```text
node scripts/ci/cargo-group.mjs test core
```

Cross-product lifecycle, source-lock, imported-contract, and SQLite identity
checks live in `tests/vertical`. Product UI, real-model, and loaded-model
shutdown evidence remain explicit acceptance gates and are not inferred from
compilation.
