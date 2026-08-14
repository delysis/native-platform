# ADR-014: Component release and versioning

## Status

Accepted 2026-08-12. Steward review is recorded in `../W1-ADRS-RECEIPT.json`.

## Context and current evidence

The target first-party monorepo contains shared platform crates and several products with different release cadences. A single repository revision is necessary but insufficient to identify a shipped product: releases also depend on the external `llama-cpp-rs` revision, component version, selected package graph, lockfile, schemas, workflows, platform, binary bytes, signing state, migration receipts, and accepted vertical evidence.

Phase one established the useful pattern: exact main heads, protected annotated tags, lock hashes, workflow definitions, data-schema baselines, ancestry-preserving promotions, explicit negative evidence, and a release-manifest-style seal. It also archived the fiction repository and removed it from the active release train. One monorepo must not imply one synchronized product release or force publication of every internal crate.

## Decision

Components release independently from the shared repository using component-scoped semantic versions and annotated protected tags:

```text
platform-v0.x.y
fte-desktop-v0.x.y
mom-llama-v0.x.y
loom-v0.x.y
```

Every release uses a two-phase component manifest. A committed candidate
manifest freezes the source and all build inputs. Artifacts are built and
signed from that exact candidate revision, and immutable acceptance receipts
are produced. A final manifest commit then binds the candidate source revision,
artifact/signing identities, and receipt hashes. The protected release tag
peels to that final manifest commit. The final manifest records at minimum:

- repository revision and component tag;
- component name and semantic version;
- exact external binding revisions, especially `llama-cpp-rs`;
- exact internal package graph and workspace lockfile SHA-256;
- Rust/toolchain and workflow-definition identities;
- database/schema and migration range;
- target platform and minimum OS;
- binary/package hashes and signing/notarization facts;
- required vertical, migration, backup/recovery, active-operation quit, relaunch, and rollback receipts;
- claims established, claims explicitly not established, and retained negative evidence.

The manifest is the release composition authority; a repository tag alone is not. Tags are immutable and protected against deletion and non-fast-forward updates. Internal crates are not independently published unless a real external consumer and release boundary are demonstrated.

Semantic versioning describes each component's supported public/product contract. Because there are no existing users, the initial migration need not preserve generic API or behavior compatibility. It must preserve data recovery, evidence lineage, and truthful migration/rollback boundaries.

## Alternatives

1. **One repository-wide version and release train.** Rejected because product cadence, platform risk, and acceptance evidence differ.
2. **Release from a commit SHA without a manifest.** Rejected because the SHA does not bind artifacts, external dependencies, schemas, signing, or evidence.
3. **Publish every internal crate.** Rejected because it creates unsupported public contracts and version churn without consumers.
4. **Use mutable branches or floating dependency references.** Rejected because they cannot reproduce or safely roll back a release.

## Migration

1. Define and validate the manifest schema in the repository before the first component cutover.
2. Import the phase-one heads, tags, locks, schema baselines, workflow exports, and negative evidence as the migration baseline.
3. Assign component boundaries and version namespaces; identify genuinely external crates separately.
4. For each component, freeze its package graph and vertical baselines, run data
   migration and recovery tests, and commit a candidate manifest naming the
   exact build-source revision and intended component version.
5. Build, package, and sign from that candidate revision; verify installed
   binary hashes and exact running identity; and produce content-addressed,
   immutable acceptance receipts.
6. Commit the final manifest containing those artifact, signing, and receipt
   hashes. The diff from candidate source to final manifest commit may contain
   only manifest and receipt material, never production code or dependencies.
7. Create the protected annotated component tag at the final manifest commit.
8. Keep former repositories readable at their protected phase-one tags until
   the corresponding component migration and rollback proof is accepted.

## Rollback

Select the previous accepted component manifest and protected tag, restore its signed package and the compatible data backup, and verify its recorded hashes before launch. If a schema is forward-only, use the manifest's explicit reverse/export recovery procedure rather than launching an older binary against incompatible data. Rollback never moves an existing tag and never rewrites or deletes the failed release receipt; a corrected release receives a new version.

## Acceptance

- A schema validator rejects missing revisions, floating references, absent hashes, inconsistent component/tag versions, and unsupported claims.
- Each release tag is annotated, server-protected, and peels to the manifest revision.
- Clean builders reproduce the package graph and verify the recorded lock, workflow, and artifact hashes.
- Component verticals cover package/sign, install/launch, active-operation quit,
  immediate relaunch, data migration from each of the previous two releases,
  backup/recovery, and rollback.
- Release evidence distinguishes unsigned local acceptance from signed/notarized distribution.
- Archived or diagnostic-only repositories are absent from active component manifests.
- Live paid-provider spend is not a release acceptance requirement; any hosted behavior claim remains limited to the evidence actually run.
- The W1 ADR set-level steward receipt confirms product owners agree on component, store, migration, and release boundaries.

## Consequences

Products can release independently while sharing atomic source changes and one workspace resolution. Releases gain reproducible composition and explicit rollback inputs, at the cost of manifest tooling and per-component evidence work. Internal refactors stay cheap because unconsumed crates do not acquire artificial public versions. A tag or green CI run alone can no longer be mistaken for a distributable release.
