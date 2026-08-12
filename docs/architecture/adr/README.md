# Delysis architecture decision record set

Status: **Accepted for W1 on 2026-08-12**

This directory is the pre-shell authority for the fourteen decisions required
by `W1-ADRS`. It is intentionally held in the sealed phase-one control packet:
`delysis/native-platform` does not exist yet, and creating that repository
before W1 contracts and vertical baselines pass would invert the task graph.

During W2 these files must be imported byte-for-byte into
`docs/architecture/adr/` in the new repository. The W2 history ledger must
record each source SHA-256; changes after import require a superseding ADR, not
an unrecorded rewrite of this accepted set.

## Decision index

| ADR | Decision | Scope owner |
| --- | --- | --- |
| [ADR-001](ADR-001-first-party-monorepo.md) | First-party monorepo | systems steward |
| [ADR-002](ADR-002-owner-handle-and-shutdown-hierarchy.md) | Owner/handle and shutdown hierarchy | systems steward |
| [ADR-003](ADR-003-operation-lifecycle-v2.md) | Operation lifecycle v2 | systems steward |
| [ADR-004](ADR-004-final-result-versus-progress.md) | Final result versus progress | systems steward |
| [ADR-005](ADR-005-evidence-tiers-and-claim-naming.md) | Evidence tiers and claim naming | systems steward |
| [ADR-006](ADR-006-artifact-publication-outcomes.md) | Artifact publication outcomes | systems steward |
| [ADR-007](ADR-007-one-modern-gateway.md) | One modern Gateway | FTE and Mom product owner |
| [ADR-008](ADR-008-product-store-separation.md) | Product store separation | FTE, Mom, Loom, Information product owner |
| [ADR-009](ADR-009-speech-sibling-service.md) | Speech sibling service | systems steward |
| [ADR-010](ADR-010-loom-writing-and-research-modes.md) | Loom writing versus research modes | Loom product owner |
| [ADR-011](ADR-011-frontier-diagnostic-quarantine.md) | Frontier diagnostic quarantine | Loom product owner |
| [ADR-012](ADR-012-rust-baseline-and-resolver.md) | Rust baseline and resolver | systems steward |
| [ADR-013](ADR-013-credential-storage.md) | Credential storage | FTE and Mom product owner |
| [ADR-014](ADR-014-component-release-and-versioning.md) | Component release and versioning | all product owners |

## Authority and review

The repository owner explicitly authorized all phase-one remediation and the
subsequent gated architecture work, stated that the software has no existing
users, and removed compatibility ceremonies that do not protect real data or
evidence. That authority permits breaking internal APIs and schemas when the
migration is explicit and tested. It does not permit false runtime claims,
credential disclosure, destructive loss of recoverable product data, or
topology work before its gate.

The systems-steward and product-boundary acceptance is recorded in
`../W1-ADRS-RECEIPT.json`. Acceptance of this set authorizes `W1-CONTRACTS`; it
does not authorize the W2 monorepo shell or source imports.

## Controlling evidence

- `R8-FINAL-RECEIPT.json`
- `PHASE1-LOCK-MANIFEST.json`
- `PHASE1-BASELINE-LEDGER.json`
- `PHASE1-TAG-LEDGER.json`
- `PHASE1-WORKFLOW-EXPORTS.json`
- `contracts/operation-lifecycle-v2.md`
- `contracts/publication-protocol.md`
- `contracts/evidence-and-claims.md`
- `contracts/repository-boundaries.md`
