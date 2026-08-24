# CI policy

The shell gate is authority-free and deterministic. It checks formatting,
compilation, tests, strict Clippy, the pnpm lock, live workspace topology,
package-group coverage, and the external Git allowlist. Historical migration
receipts, seal hashes, and ADR snapshots remain readable provenance but do not
gate ordinary changes. The shell gate does not contact product repositories,
load models, use credentials, exercise real hardware, or establish a product
claim.

Every future package belongs to exactly one declared package group. Portable,
platform, and product checks may be required for ordinary pull requests.
Research, diagnostic, and real-hardware jobs must be separately named and may
not be treated as substitutes for operational acceptance.

## Ignored-test registry

`ci/ignored-tests.json` is the machine-readable inventory of opt-in tests. Each
entry binds the exact Cargo test ID to a workspace package and source file,
states its prerequisite and evidence class, and explicitly records what that
test cannot promote. The structural validator runs in ordinary policy CI. Full
macOS CI additionally runs the exact locked workspace `--ignored --list`
command and reconciles all 37 IDs without executing them.

## Reverse-dependency shadow

The PR plan computes local reverse dependencies from locked, no-dependency
Cargo metadata. That result is observational under the
`dependency_shadow` field: `selection_applied` and `promotion_allowed` remain
false even when it matches the current planner. Unknown paths fail closed to
the full metadata graph. The authoritative planner continues to own explicit
frontend/platform assets in `ci/ci-path-exceptions.json` and required job
selection until representative shadow results are reviewed and deliberately
promoted. During this interval, changes to Native, Attachment, Information,
Speech, or FTE contracts conservatively include Mom.

There is one root Rust toolchain declaration, one Cargo workspace, one root
Cargo lockfile, and one root pnpm workspace lockfile. CI uses locked dependency
resolution, rejects first-party Git dependencies, and pins the one permitted
external FFI dependency to its reviewed revision. It does not consult the
migration ledger.
