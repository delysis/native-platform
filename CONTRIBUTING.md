# Contributing

Use safe, idiomatic Rust, explicit ownership, and workspace dependencies and
lints. Select the pinned toolchain as described in README.md. Keep package groups
complete so dependency-aware CI can select affected consumers.

These products are unreleased. Prefer one current schema and clear rejection of
incompatible data to upgrade machinery without a consumer. Never erase user
prose or credentials as a side effect of a schema change. Remove obsolete runtime
paths and their migration-only tests; retain tests of current constraints.

Reproduce a defect through its actual adapter, store, or worker before repairing
it. Test behavior and observable effects, including failure and cancellation.
During development, run focused checks; after edits settle, run the affected
packages and their consumers once. Reuse the same Cargo target directory and
prune stale generated caches before disk pressure interrupts a build.

```sh
cargo run --locked -p xtask -- policy
node scripts/ci/cargo-group.mjs test <group>
node scripts/ci/cargo-group.mjs clippy <group>
cargo fmt --all -- --check
```

Run relevant frontend checks for UI changes and doctests for public Rust APIs.
Full CI, selected real-model execution, and platform qualification are described
in [the CI policy](docs/CI-POLICY.md). A fixture, listed test, or successful build
establishes only that result. Packaged acceptance requires the actual interaction.

Opt-in tests belong in `ci/ignored-tests.json` with exact identities and real
prerequisites. Unsupported platforms must return a typed failure before mutation;
keep portable tests enabled and test the unsupported boundary on that platform.

Historical migration ledgers, receipts, and seals are provenance, not instructions
for current development. Update current contracts when behavior changes; do not
rewrite archived evidence or build new evidence machinery without a consumer.
