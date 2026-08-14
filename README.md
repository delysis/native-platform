# native-platform

`native-platform` is the canonical Delysis first-party Rust monorepo. It owns
the Native llama boundary, Attachment, Information, Speech, Free Token Energy,
Mom Llama, and Loom.

The migration preserves each accepted source repository as ordinary Git
history under its destination path. The `migration/` receipts and commit maps
remain as archival provenance; they are not mutable build inputs or ordinary
policy gates. `delysis/llama-cpp-rs` remains the sole external Delysis Git
source: it is the separately reviewed unsafe upstream boundary.

## Workspace

- Rust 1.92.0, edition 2024, resolver 3.
- All 47 first-party packages are members of one root Cargo workspace.
- One root `Cargo.lock` resolves exact `rusqlite 0.39.0` and one
  `libsqlite3-sys 0.37.0` native link.
- One pnpm 11.16.0 workspace and root `pnpm-lock.yaml` own the FTE, Mom, and
  Loom applications. Each resolves the same pinned local Tauri CLI version.
- `ci/package-groups.json` assigns every package to exactly one primary group
  and optional secondary gates.
- The Attachment fuzz target is the only deliberately excluded auxiliary
  Cargo workspace.

Check the live repository invariants with:

```text
cargo run --locked -p xtask -- policy
```

Then test and lint only the affected package group during normal development:

```text
node scripts/ci/cargo-group.mjs test product-mom
node scripts/ci/cargo-group.mjs clippy product-mom
```

`./scripts/check-shell-policy.sh` remains the exhaustive local gate for broad
workspace, lockfile, and release changes. `cargo xtask lean verify` is an
explicit historical W8/W9 census check, not part of ordinary policy.

Lifecycle, migration, and SQLite identity checks live with the product or
service that owns the behavior. Product UI, real-model, and loaded-model
shutdown evidence remain explicit acceptance gates and are not inferred from
compilation.
