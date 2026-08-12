# ADR-006: Artifact publication outcomes

## Status

Accepted — 2026-08-12. Steward review is recorded in `../W1-ADRS-RECEIPT.json`.

## Context and current evidence

Attachment and Information cross a hostile-bytes boundary, but only Information and other artifact producers publish caller-visible files. The phase-one audit found that a write may become visible before parent-directory synchronization completes. Reporting that state as an undifferentiated failure invites a retry to overwrite or conflict with an artifact that was already committed. The accepted publication contract already requires private same-filesystem staging, exact length and digest verification, no-clobber publication, file synchronization, parent synchronization, and idempotent recovery.

There are no existing users whose API shape must be preserved. There are, however, durable artifacts and receipts whose identity and recovery semantics must remain truthful.

## Decision

Use the typed state machine:

```text
UntrustedBytes -> PrivateStagedArtifact -> VerifiedArtifact -> PublishedArtifact
```

Publication has exactly three externally meaningful outcomes:

- `NotPublished(error)`: no caller-visible artifact was committed;
- `Published(artifact)`: the exact verified artifact is visible and file and parent synchronization succeeded;
- `PublishedDurabilityUnknown { artifact, error }`: the exact artifact is visible, but durability could not be established.

Publication uses a private sibling on the destination filesystem, validates exact digest and length before commit, synchronizes the file, performs a no-clobber publish, then synchronizes the parent. Retry inspects an existing destination: an exact digest-and-length match is idempotent success; any mismatch is a conflict. Cleanup may delete only private staging objects it owns. Attachment parsing stays separate from publication and does not acquire filesystem or network authority merely by sharing artifact envelopes.

## Alternatives

- Return one generic `Result`: rejected because it erases whether commit occurred.
- Overwrite on retry: rejected because it destroys evidence and destination authority.
- Publish directly to the destination: rejected because callers could observe partial bytes.
- Share Attachment and Information implementation wholesale: rejected because archive parsing, network acquisition, URI policy, and indexing are different trust boundaries.

## Migration

1. Introduce versioned artifact, publication-result, and commit-receipt types.
2. Adapt Information acquisition/publication paths to private staging and no-clobber commit.
3. Preserve exact artifact identity, length, destination identity, visibility, synchronization facts, and recovery result in receipts.
4. Convert legacy ambiguous failures using destination inspection; never infer `NotPublished` without checking visibility.
5. Add fault injection before and after every irreversible boundary and idempotent-retry tests.

## Rollback

Revert callers to the prior implementation only while retaining the new inspection/recovery tool and typed receipt data. Do not delete a visible artifact or relabel `PublishedDurabilityUnknown` as `NotPublished`. If migration fails, quarantine the new path, inspect destination identity, and resume from the last verified state.

## Acceptance

- Fault tests distinguish pre-commit failure, committed-and-durable success, and committed-with-unknown-durability.
- No caller observes partial bytes and no retry silently overwrites a mismatch.
- Exact-match retry is idempotent.
- Receipts bind digest, length, destination identity, visibility, file sync, parent sync, and recovery status.
- Information product recovery tests preserve existing durable artifacts.

## Consequences

Callers must handle a third outcome and cannot equate every error with absence. The additional state is intentional: it makes recovery deterministic, prevents destructive retries, and lets common artifact contracts remain small without collapsing service-specific authority.
