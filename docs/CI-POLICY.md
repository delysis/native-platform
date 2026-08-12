# CI policy

The shell gate is authority-free and deterministic. It checks formatting,
compilation, tests, strict Clippy, the pnpm lock, workspace topology, migration
null fields, and ADR byte identities. It does not contact product repositories,
load models, use credentials, exercise real hardware, or establish a product
claim.

Every future package belongs to exactly one declared package group. Portable,
platform, and product checks may be required for ordinary pull requests.
Research, diagnostic, and real-hardware jobs must be separately named and may
not be treated as substitutes for operational acceptance.

There is one root Rust toolchain declaration, one Cargo workspace, one root
Cargo lockfile, and one root pnpm workspace lockfile. CI uses locked dependency
resolution and rejects imported source before its ledger entry records an
authorized import commit.
