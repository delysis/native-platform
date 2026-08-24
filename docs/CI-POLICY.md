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
entry binds the exact Cargo test ID to a workspace package, source file, exact
Cargo target identity, and explicit platform availability. Target records are
validated against locked Cargo metadata by package, target name, target kinds,
target source, and manifest. Entries also state their prerequisite and evidence
class and explicitly record what that test cannot promote. The registry has 37
tests: 37 are available on macOS, 36 on Linux, and 33 on Windows.

The structural validator runs in ordinary policy CI. Ignored tests must use the
canonical private `fn` or `async fn` form, an explicit `#[test]` or
`#[tokio::test(...)]`, and `#[ignore = "reason"]`. Conditional, public, and
macro-generated ignore syntax is rejected. Comments and strings are tokenized
as non-code, not counted as test declarations. Workspace manifests may not
configure custom Cargo harnesses. Crate-level `no_main`, custom test framework,
test-runner, generated harness-main, `include!`, and workspace proc-macro paths
are rejected, including raw-identifier and conditional forms. Every workspace
build script is bound to an explicitly reviewed SHA-256.

Every Rust source, Cargo manifest or lock, build script, proc-macro source,
toolchain file, and Cargo configuration change selects exact reconciliation.
The blocking pull-request job is a Linux, macOS, and Windows matrix, and full CI
also reconciles on all three operating systems. Each lane builds test harnesses
with locked `cargo test --no-run --message-format=json-render-diagnostics`
through `rustup run 1.92.0`, independent of the ambient `cargo` on `PATH`.
Compiler identity is recorded. Effective Cargo configuration and environment
overrides for compilers, wrappers, bootstrap, targets, linkers, loaders,
Rustflags, and Rustdoc flags are rejected. Source, manifests, build scripts,
lock, toolchain, registry, and Cargo configuration are hashed before the build
and must remain unchanged afterward.

Cargo test-profile artifacts must be regular nonsymlink files inside Cargo's
metadata target directory. They are hashed, invoked from an empty temporary
working directory with only `--ignored --list`, bounded by a 30-second timeout,
hard-terminated on timeout, then hashed again. The resulting full test ID and
real Cargo target tuple is reconciled against the expected current-platform
subset. This gate records that
Cargo requested rustc test mode and that the validator supplied list arguments;
it does not claim cryptographic proof of stock libtest or that trusted compiler,
build-script, proc-macro, linker, or loader code cannot misbehave. It never
requests an ignored test body and cannot promote runtime or product evidence.

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
