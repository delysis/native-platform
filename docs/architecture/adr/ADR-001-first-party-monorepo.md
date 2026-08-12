# ADR-001: First-party monorepo

## Status

Accepted 2026-08-12. Set-level systems-steward review is recorded in `../W1-ADRS-RECEIPT.json`.

## Context and current evidence

The phase-one repair train required coordinated changes and immutable revision pins across the native runtime, Gateway, Mom, Loom, Speech, Information, and Attachment repositories. The accepted phase-one receipt, baseline ledger, tag ledger, workflow exports, and lock manifest establish the exact current repository heads, dependency edges, schemas, toolchain claims, workflows, and rollback tags.

The repositories contain distinct product domains, but the first-party Rust code does not have stable, independently versioned platform interfaces or independent consumer teams. Keeping platform and products in separate repositories would preserve the cross-repository SHA train that made atomic lifecycle and evidence changes difficult.

`llama-cpp-rs` is different: it is an upstream-tracking unsafe/FFI boundary with a separate review and publication cadence. Fiction has been archived and is historical diagnostic input, not active platform authority.

There are no existing users and therefore no general backwards-compatibility promise. Exact store recovery, evidence lineage, source history, and rollback invariants remain mandatory.

## Decision

Use three repository roles:

1. Keep `delysis/llama-cpp-rs` separate as the external upstream/unsafe binding fork.
2. Create `delysis/native-platform` as the single first-party repository for shared platform contracts, native runtime, modern inference Gateway, Attachment, Information, Speech, Tauri adapters, FTE desktop, Mom, Loom, frontends, and vertical integration tests.
3. Keep `delysis/fiction-autoresearch-harness` archived as historical diagnostic material. Any future use is a one-way import into diagnostic tooling and cannot create product or promotion authority.

The monorepo has one Cargo lockfile after path cutover, but products retain independent domains, stores, UI bundles, runtime owners, release tags, roadmaps, and feature sets. A monorepo is not permission to collapse trust boundaries into one crate.

## Alternatives

### Continue with the current repositories

Rejected. It retains moving cross-repository pins, prevents atomic platform/product changes, and duplicates integration proof across repositories.

### One platform repository plus separate product repositories

Deferred until platform APIs are stable, products have genuinely independent teams or external consumers, and compatibility windows are deliberate. Those conditions do not currently exist.

### One repository and one giant crate

Rejected. Unsafe/FFI, hostile-input, public serialization, native dependency, feature-isolation, and product-domain boundaries still require separate crates or modules.

### A permanent additional contracts repository

Rejected. Contracts and testkit belong in `native-platform`; no tenth production kit will be created.

## Migration

1. Complete and receipt all Wave 1 ADR, contract, FTE parity, credential migration, and vertical-baseline work before creating the shell.
2. Create the empty `delysis/native-platform` shell in Wave 2 with policy, ADRs, migration ledger, contracts/testkit, and CI only; import no production source in that commit.
3. Prove the exact frozen current repository SHAs together through the integration harness.
4. Import each repository history before consolidation, recording source identity, import command, old-to-new commit mapping, and tree hash.
5. Preserve crate names, APIs where useful, features, tests, stores, and schemas through parity.
6. Switch internal Git dependencies to paths only after each import proves equivalence.
7. Consolidate crates only after path-preserving import and the criteria in `07-CRATE-CONSOLIDATION-MAP.md` are met.
8. Freeze old first-party repositories after cutover; do not bidirectionally synchronize them.

## Rollback

Before a path cutover, abandon the failed monorepo import or integration branch and continue from the protected phase-one repository tags. No current repository is modified merely to make an import appear successful.

After a path cutover, roll back the affected component to its last parity-proven monorepo commit and restore the corresponding immutable source revision. Do not resume independent writes in both old and new repositories. If a future split is needed, generate it one-way from the monorepo.

## Acceptance

- ADR-001 through ADR-008 are accepted before the monorepo shell is created.
- All nine phase-one repositories have exact protected audit tags and sealed baseline evidence.
- The Wave 2 initial commit contains no imported production source.
- History and tree mappings are required for every later import.
- Product store and release independence is explicitly retained.
- `llama-cpp-rs` remains outside the first-party monorepo.
- Fiction remains archived and nonauthorizing.

## Consequences

Atomic platform/product changes, one dependency graph, one lockfile, and cross-product vertical tests become possible. CI and repository policy become more centralized. Product release cadence remains independent, while accidental divergence becomes harder. Migration must be staged carefully because a broad repository move cannot substitute for correctness, parity, or data-recovery proof.
