# ADR-005: Evidence tiers and claim naming

## Status

Accepted 2026-08-12. Set-level systems-steward review is recorded in `../W1-ADRS-RECEIPT.json`.

## Context and current evidence

Phase one retained several important distinctions: fixture-backed hosted-provider correctness did not become a live-provider claim; bounded local Attachment fuzzing did not become a scheduled-run claim; an initial Mom run labeled Qwen was rejected when the persisted conversation actually loaded SmolLM2; and a Loom diagnostic critic receipt could not authorize promotion. These cases show that evidence must state what happened, what did not happen, and what authority was actually present.

Operational success, reproducibility, research eligibility, and external attestation require different facts. Local hashes and persisted rows are useful evidence but cannot recreate process-local, one-use authority. There are no users requiring legacy claim names, so misleading names may be removed rather than aliased. Existing negative evidence and exact lineage must remain available.

## Decision

Use four cumulative evidence tiers:

### Operational

Validated request, truthful route and local/network/fixture facts, terminal result, bounded resources, typed error, and operation lifecycle.

### Reproducible

Operational evidence plus exact build/runtime/model/artifact identity, input and output digests, normalized parameters, seed where applicable, and timing or usage provenance.

### ResearchEligible

Reproducible evidence plus frozen source graph, exact prompt and model bindings, exact token IDs and token-piece bytes/boundaries where relevant, exact event lineage, live move-only process authority, role separation, contamination labels, live adoption, and an explicit one-use foreground decision where required.

### ExternallyAttested

Research-eligible or otherwise scoped evidence plus independent or hardware-backed attestation, externally anchored build/timestamp/signature, or reproducible-build verification under a stated threat model. Local hashes alone do not establish this tier.

Every claim uses the narrowest true name. For example, use `VerifiedForegroundCommand` unless physical user presence is actually proven, and `DiagnosticFrontierCriticReceipt` when remote model identity is not independently authenticated and the receipt has no promotion authority.

Persisted receipts, replay witnesses, and serialized records never mint or reconstruct live authority. Diagnostic critic output cannot authorize writer selection or research promotion. Every receipt names its tier, threat model, exact source, exact runtime or artifact, omitted claims, and negative evidence.

## Alternatives

### One generic `verified` flag

Rejected. It collapses materially different threat models and encourages overclaiming.

### Infer the tier from available receipt fields

Rejected. Missing fields, replay, and serialization can make accidental authority escalation appear plausible.

### Treat reproducible local hashes as external attestation

Rejected. Self-reported identity is not independent attestation.

### Persist live authority for later reuse

Rejected. One-use, session-bound authority would become forgeable and replayable.

### Keep broad legacy names as compatibility aliases

Rejected where they overclaim. With no existing users, narrow truthful names replace them directly; historical receipts remain interpretable through explicit schema/version metadata.

## Migration

1. Inventory receipt and claim types across native, Gateway, Speech, Information, Mom, Loom, Attachment, and diagnostic tooling.
2. Add explicit tier, threat model, exact source/runtime identity, omitted-claim, and negative-evidence fields to versioned envelopes.
3. Rename overbroad authority claims to the narrowest true names.
4. Keep live authority types nonserializable and move-only; persist only derived nonauthorizing receipts.
5. Add conversion adapters only where needed to read historical evidence. Do not preserve misleading public names merely for compatibility.
6. Add golden fixtures proving that fixture, local, network, research, and external-attestation flags cannot be silently promoted.
7. Preserve rejected and negative runs alongside replacement evidence.

## Rollback

If a consumer cannot yet read the versioned evidence envelope, retain its prior reader behind an import-only adapter while emitting the new narrow claim. Do not roll back by restoring a broader claim name, reconstructing live authority from persisted data, or deleting negative evidence. A product that cannot state its true tier must fail closed or report the lower tier.

## Acceptance

- Every receipt declares exactly one evidence tier and its threat model.
- Exact source and runtime/artifact identity are recorded where the tier requires them.
- Omitted claims and negative evidence are explicit.
- Fixture execution cannot claim live network behavior.
- Local hashes cannot claim external attestation.
- Persisted or replayed evidence cannot recreate live authority.
- Diagnostic frontier evidence cannot authorize writing or promotion.
- `VerifiedForegroundCommand` states only the trusted-host, focused, one-use command threat model.
- Research token spans require exact token-piece byte boundaries.
- Golden fixtures reject tier escalation caused by serialization, missing fields, or broad legacy names.

## Consequences

Receipts become more explicit and sometimes more verbose, but claims remain auditable across product, research, diagnostic, and release contexts. Honest negative results remain first-class evidence. UI and support tooling can distinguish operational success from reproducibility, research eligibility, and independent attestation without inventing authority.
