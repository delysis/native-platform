# ADR-015: Local-first macOS release candidates

## Status

Accepted 2026-08-14. Supersedes ADR-014 for pre-user release-candidate work.

## Context and current evidence

The target first-party monorepo contains shared platform crates and several products with different release cadences. The products have no external users yet. Release work must preserve data and shutdown behavior without turning internal candidates into a compliance process or making GitHub availability part of local development.

Phase one established the useful pattern: exact main heads, protected annotated tags, lock hashes, workflow definitions, data-schema baselines, ancestry-preserving promotions, explicit negative evidence, and a release-manifest-style seal. It also archived the fiction repository and removed it from the active release train. One monorepo must not imply one synchronized product release or force publication of every internal crate.

## Decision

Components release independently from the shared repository using component-scoped semantic versions and annotated protected tags:

```text
platform-v0.x.y
fte-desktop-v0.x.y
mom-llama-v0.x.y
loom-v0.x.y
```

macOS is the only supported packaging target until a real user or distribution
need justifies another platform. `scripts/release-macos.sh` builds one component
locally and emits one receipt beside the zipped `.app`. The receipt records the
clean source revision, lockfile hashes, observed bundle identity, minimum macOS
version, executable and archive hashes, signing mode, and exactly which local
checks ran. It is evidence about one build, not a second source of truth.

The local Mac is the release authority. A clean commit and its component tag
identify stable source; the adjacent receipt contains the content digest for
one concrete artifact. GitHub Actions may independently rebuild and upload a
distinct remote candidate, but that artifact does not inherit the local
package's digest or packaged-app smoke evidence. GitHub Actions is not required
to create or test a local candidate.
Local candidates use ad-hoc signing. Developer ID signing and notarization are
required only when the artifact is actually distributed through a channel that
needs them. Model files are discovered at runtime and are never release inputs
or bundle contents.

The same command has an explicit `stable` mode for that distribution boundary.
It requires the exact annotated component tag at `HEAD`, an available Developer
ID Application identity, and a local `notarytool` Keychain profile. It waits for
notarization, staples and validates the ticket, asks Gatekeeper to assess the
app, and runs the exact-archive two-launch smoke. The operational procedure is
documented in `docs/releases/macos.md`.

Internal crates are not independently published unless a real external
consumer and release boundary are demonstrated.

Semantic versioning describes each component's supported public/product contract. Because there are no existing users, the initial migration need not preserve generic API or behavior compatibility. It must preserve data recovery, evidence lineage, and truthful migration/rollback boundaries.

## Alternatives

1. **One repository-wide version and release train.** Rejected because product cadence, platform risk, and acceptance evidence differ.
2. **Two-phase candidate and final manifests.** Rejected because they add commit and tag choreography without improving the product or shortening feedback loops at the current scale.
3. **Publish every internal crate.** Rejected because it creates unsupported public contracts and version churn without consumers.
4. **Use mutable branches or floating dependency references.** Rejected because they cannot reproduce or safely roll back a release.

## Migration

1. Run the component's focused source-level migration, reopen, and shutdown tests locally.
2. Build its macOS `.app`, verify its signature, and emit the adjacent receipt.
3. Launch, quit, and relaunch the packaged app with an isolated test store
   before accepting it as a local candidate.
4. Before calling a release stable, exercise an active operation through the
   exact packaged app and verify applicable backup/restore rollback.
5. Tag the accepted clean commit with the component version.
6. Build the stable Developer ID-signed, notarized package locally.
7. Upload the artifact asynchronously if distribution is useful.
8. Keep former repositories readable at their protected phase-one tags until
   the corresponding component migration and rollback proof is accepted.

## Rollback

Select the prior tag and package, restore the data backup made before migration,
and verify the package against its receipt before launch. An older binary must
not open a forward-migrated store. A corrected release receives a new version;
an existing tag is never moved. The current packaged builds exercised this
procedure with `scripts/product-state-backup.mjs`: each current binary mutated
the original state, and the prior binary recovered the pre-migration marker
from a fresh restored root.

## Acceptance

- The local release command works without GitHub or network access once dependencies are cached.
- The release command rejects dirty source; its receipt records package hashes and signing state.
- Focused component checks cover source-level migration/reopen and
  active-operation shutdown; the separate offline state tool establishes
  packaged rollback.
- A local candidate has an isolated packaged-app launch, quit, and relaunch smoke.
- A stable release additionally has active packaged-operation evidence and
  applicable packaged backup/restore rollback evidence.
- Evidence distinguishes ad-hoc local acceptance from Developer ID/notarized distribution.
- Archived or diagnostic-only repositories are absent from the active package graph.
- Live paid-provider spend is not a release acceptance requirement; any hosted behavior claim remains limited to the evidence actually run.

## Consequences

Products can release independently without waiting for CI. The receipt is
small enough to inspect and discard for failed candidates. The tradeoff is
intentional: it does not attempt to prove every transitive input or turn an
internal build into a notarized public distribution. Real migration, shutdown,
launch, and rollback behavior remain the release gates.
