# ADR-011: Frontier diagnostic quarantine

## Status

Accepted 2026-08-12. Steward review is recorded in `../W1-ADRS-RECEIPT.json`.

## Context and current evidence

The current Loom frontier adapter invokes a separately installed ChatGPT/Codex-style executable for controlled research diagnostics. The phase-one audit found that this can be useful for critique and comparison, but the process identity, requested model name, remote model identity, and returned text do not create live writer or promotion authority. A serializable transcript also cannot recreate the move-only runtime authority used by native inference or a one-use foreground promotion command.

Phase-one evidence already separates diagnostic frontier receipts from real local-model evidence and retains negative results. The archived fiction harness is not an alternative research authority; its surviving relationship is one-way diagnostic export/import. Live paid-provider spending was explicitly removed as an acceptance requirement. Strict fixtures may establish protocol behavior, but they do not establish that a hosted request ran or authenticate a particular remote model.

## Decision

Frontier execution is quarantined behind an opt-in `frontier-diagnostic` feature and is excluded from default product and platform dependency graphs.

The adapter must receive an explicit configuration manifest containing the executable path, executable hash and available signature/version facts, requested model, environment allowlist, network/provider policy, time and output bounds, and redaction policy. It runs under ordinary supervised operation lifecycle rules and emits a `DiagnosticFrontierCriticReceipt` containing inputs, output identity, limits, termination, and provenance sufficient for replay and audit.

Frontier outputs are nonauthorizing data. They may be admitted as diagnostic evidence but may not directly:

- mutate a manuscript or canonical research state;
- mint a verified native inference seal;
- select or promote a candidate;
- claim authenticated remote-model identity;
- be relabeled as locally generated evidence.

Any promotion of information derived from a frontier diagnostic must pass through the same explicit admission, evaluation, and one-use foreground decision used for other external evidence. Receipts remain serializable and cloneable precisely because they carry no live authority.

## Alternatives

1. **Keep the adapter in the default core.** Rejected because an optional hosted diagnostic would become an ambient build, runtime, privacy, and authority dependency.
2. **Treat frontier output as verified critic authority.** Rejected because executable invocation and a requested model string do not independently authenticate remote execution or model identity.
3. **Ban frontier diagnostics.** Rejected because bounded, provenance-rich external critique remains useful when its claim boundary is explicit.
4. **Require a live paid-provider call for acceptance.** Rejected because it adds credential and spend requirements without strengthening the architectural contract; fixtures and nonauthorizing receipts cover the accepted phase-one boundary.

## Migration

1. Inventory frontier invocation paths, manifests, environment variables, and persisted records.
2. Move the adapter and its integration tests behind `frontier-diagnostic`; keep diagnostic DTOs in the diagnostic boundary, not core inference types.
3. Add manifest validation, executable identity capture, redaction, time/output bounds, cancellation, terminal accounting, and receipt schema tests.
4. Reject direct store mutation or promotion from the adapter at compile-time/module-boundary checks and transaction tests.
5. Migrate historical frontier rows to explicit diagnostic classification without rewriting their original bytes, timestamps, or negative outcomes.
6. Preserve the archived fiction material through one-way diagnostic import only.

No generic backwards compatibility is required because there are no existing users. Historical data recovery and evidence provenance remain mandatory: migration must retain original records or a content-addressed backup and a reversible mapping receipt.

## Rollback

Disable the feature and remove the adapter from active manifests. Restore the prior exact component tag only if required to recover historical diagnostic records. Do not convert diagnostic receipts into authority during rollback. If a schema migration has run, restore the pre-migration backup or use the recorded reverse mapping while retaining both evidence identities.

## Acceptance

- Default builds and product tests contain no frontier diagnostic executable or
  frontier adapter dependency. Ordinary hosted-provider adapters governed by
  ADR-007 remain permitted and do not gain diagnostic promotion authority.
- Feature-enabled fixture tests cover manifest validation, bounds, cancellation, redaction, terminal accounting, and malformed output.
- Module and transaction tests prove frontier output cannot directly mutate or promote canonical state.
- Diagnostic receipts identify the executable and requested model while explicitly declining authenticated remote-model and live-provider claims.
- Historical diagnostic and negative evidence survives migration and recovery testing.
- Acceptance requires no live paid-provider spend.
- The W1 ADR set-level steward receipt reviews this decision and its interactions with ADR-005, ADR-008, and ADR-010.

## Consequences

The default platform remains local-first and does not silently acquire a hosted dependency. Diagnostic integrations remain possible, reproducible within their stated limits, and plainly weaker than native runtime or foreground-command evidence. Feature builds and receipt schemas add maintenance cost, and frontier results require an additional explicit promotion step.
