# ADR-012: Rust baseline and resolver

## Status

Accepted 2026-08-12. Steward review is recorded in `../W1-ADRS-RECEIPT.json`.

## Context and current evidence

The sealed phase-one baseline records mixed Rust policies: most repositories declare or test Rust 1.88, Information requires Rust 1.92, some workflows use moving stable, and no repository currently owns a `rust-toolchain` file. The future first-party platform workspace must support atomic cross-component changes with one dependency resolution and one lockfile. Preserving the lower version would require backporting the Information implementation and maintaining split language/library assumptions before migration yields any value.

Phase-one also exposed the cost of mixed tool binaries: one excluded Clippy attempt combined Cargo/rustc 1.88 with a Homebrew 1.95 `clippy-driver` and produced invalid evidence. A workspace-wide exact toolchain is therefore part of reproducibility, not a convenience.

## Decision

The first-party `native-platform` workspace standardizes on:

```text
Rust 1.92
edition 2024
Cargo resolver 3
one workspace Cargo.lock
```

The repository root pins the complete Rust 1.92 toolchain, including rustfmt and Clippy. Workspace package manifests inherit the edition, Rust version, lints, and shared dependency versions from the root. CI invokes tools from that pinned toolchain and records their versions.

The separate `llama-cpp-rs` FFI/upstream-tracking repository keeps its own release and toolchain policy; compatibility is tested at its exact pinned revision. Diagnostic or archived non-Rust repositories are not forced into this workspace policy.

## Alternatives

1. **Retain Rust 1.88.** Rejected because the current Information implementation already establishes Rust 1.92 as the highest required first-party baseline; backporting creates work without a user compatibility obligation.
2. **Use moving stable.** Rejected because formatter, lint, resolver, and lockfile behavior would drift between developers and CI.
3. **Permit per-crate editions and resolvers.** Rejected because a unified workspace needs one dependency-resolution and language-policy boundary.
4. **Adopt nightly.** Rejected because no accepted production requirement needs unstable features; the Attachment fuzz workspace may retain an exact nightly toolchain solely for fuzzing.

## Migration

1. Create the root toolchain and workspace policy before importing production source.
2. Import contract/testkit packages first and verify Rust 1.92, edition 2024, resolver 3, format, Clippy, and lockfile policy.
3. For each component import, update manifests to inherit workspace fields and replace local dependency versions with reviewed workspace entries.
4. Regenerate the single lockfile only through an explicit, reviewed dependency-resolution change; record the old and new hashes.
5. Run package tests, reverse-dependency tests, platform lanes, and product verticals before deleting the source repository copy.
6. Keep exact phase-one tags and lock hashes as the recovery baseline.

There are no existing users requiring generic compiler compatibility. Data formats, evidence formats, and migration receipts do not change merely because the compiler baseline changes.

## Rollback

Before source cutover, revert the workspace-policy commit. After component import, restore the component's exact phase-one Git pin and lockfile from the component release manifest. A rollback to Rust 1.88 is allowed only if all imported packages compile and pass their accepted contracts there; it must not be claimed from manifest edits alone. Never hand-edit a lockfile to imitate the prior resolution.

## Acceptance

- Root policy pins Rust 1.92 and its rustfmt/Clippy components.
- Every production workspace package uses edition 2024 and declares or inherits `rust-version = "1.92"`.
- The root workspace uses resolver 3 and exactly one production `Cargo.lock`.
- CI proves tool binary versions, locked builds, formatting, warnings-denied Clippy, package tests, reverse dependencies, and platform contracts.
- Attachment fuzzing may use only its separately pinned nightly lane and cannot affect the production lock or compiler claim.
- Phase-one lock hashes remain retrievable for rollback and evidence comparison.
- The W1 ADR set-level steward receipt confirms the baseline and any explicit exception.

## Consequences

The unified workspace gains deterministic tool behavior, current language features, and one dependency graph. Contributors and CI must install Rust 1.92. Downstream environments requiring older compilers are not supported unless a later ADR establishes a real consumer and tested compatibility policy. The explicit exception for fuzz nightly remains narrow and auditable.
