# Contributing

Keep changes small, safe, and evidence-bound. Rust must remain safe and
idiomatic, compile on the pinned 1.92.0 toolchain, inherit workspace lints, and
use workspace dependencies. Do not add a nested Cargo workspace or lockfile;
the Attachment fuzz workspace is the sole existing exception.

Every package must declare exactly one package group from the root catalogue.
Portable CI must not depend on credentials, external networks, installed
models, platform inventories, or real hardware. Tests needing those authorities
belong in the appropriate diagnostic or real-hardware group and must state what
they do not establish.

The historical migration receipts and commit maps are inert provenance. Do not
rewrite them as part of ordinary product work or add new source through them.

During development, run checks proportional to the change:

```text
cargo run --locked -p xtask -- policy
node scripts/ci/cargo-group.mjs test <group>
node scripts/ci/cargo-group.mjs clippy <group>
cargo fmt --all -- --check
```

Run the relevant frontend script when UI code changes. Use
`./scripts/check-shell-policy.sh` for broad workspace, root dependency,
lockfile, or release changes; it is not required for every focused edit.
