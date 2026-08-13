# Single-workspace vertical coverage

The W8 diagnostic harness exercises cross-product invariants inside the one
root Cargo graph. All 65 first-party packages are root workspace members and
all are represented exactly once in `ci/package-groups.json`.

The suite:

- runs the shared deterministic lifecycle reference implementation;
- authenticates all eighteen accepted W1 vertical manifests from the imported
  contract source;
- proves every primary-group package is path-local in the root lock; and
- permits exactly one Delysis Git boundary: the exact `llama-cpp-rs` revision
  used by the native engine.

The old `current-product-graph`, isolated Loom graph, nested locks, and
split-SQLite claim were transition probes. W8 removes them after standardizing
the repository on `rusqlite 0.39.0` and `libsqlite3-sys 0.37.0`.

This harness is architectural evidence. It does not substitute for product UI,
real-model, or shutdown/relaunch acceptance.
