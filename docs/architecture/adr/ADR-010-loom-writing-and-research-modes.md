# ADR-010: Loom writing and research modes

## Status

Accepted — 2026-08-12. Steward review is recorded in `../W1-ADRS-RECEIPT.json`.

## Context and current evidence

Loom serves two related but unequal workflows. Quiet writing needs a manuscript-first editor, local bounded suggestions, exact candidate identity, explicit caret-local acceptance, and ordinary operational or reproducible provenance. Research campaigns need frozen source and prompt graphs, controlled candidates, blind evaluation, exact model/runtime binding, exact token-piece bytes and boundaries, live process authority, role separation, contamination labels, and one-use promotion authority. Phase one repaired exact token-piece evidence and foreground promotion authority and established that serialized receipts cannot recreate live authority.

The existing product has no users requiring preservation of research-heavy navigation or legacy candidate UI. Manuscripts, revisions, promotion lineage, and negative research evidence are durable and must remain recoverable.

## Decision

Loom is one writing product with two explicit modes:

- Writing mode owns projects, documents, revisions, quiet local suggestions, source-aware context, explicit promotion, and operational/reproducible evidence.
- Research mode adds frozen experiments, budgets, blind evaluation, research-eligible evidence, diagnostic/admitted distinctions, and human promotion authority.

Research mode is additive; ordinary writing never depends on it. The active manuscript is authoritative. Model output remains a candidate until an exact presented candidate is accepted through a valid current command. Stale candidates are invalidated by edit, caret, focus, or IME changes and cannot overwrite the manuscript directly.

Research eligibility requires exact token IDs, raw token-piece bytes, boundary vectors, model/prompt bindings, terminal/events, and a live non-serializable owner-worker seal. Foreground promotion consumes a process-bound, one-use command proving only that the trusted application host accepted one focused command—not physical user presence. Diagnostic frontier output remains nonauthorizing unless a separate explicit promotion protocol admits it.

## Alternatives

- Make all writing a research campaign: rejected because it burdens the primary writing journey and confuses evidence tiers.
- Let research machinery write directly to the manuscript: rejected because it bypasses promotion authority and provenance.
- Treat serialized receipts as live authority: rejected because replay could forge adoption.
- Maintain Loom and Fiction as competing research authorities: rejected; Fiction is archived diagnostic history or a one-way input only.

## Migration

1. Preserve and version existing manuscript, revision, candidate, selection, and research records.
2. Separate writing-default UI/navigation from progressively disclosed research surfaces.
3. Keep shared domain and store identities while isolating research execution, evaluation, and diagnostic adapters.
4. Require exact byte-boundary evidence and live authority only on research-eligible adoption paths; retain ordinary truthful provenance for writing mode.
5. Import Fiction evidence one way, if needed, without converting historical diagnostics into Loom authority.

## Rollback

Disable research mode and diagnostic adapters while retaining writing mode, manuscripts, and immutable campaign evidence. A rollback may withdraw a research-eligible claim but may not promote diagnostic output, delete negative results, or reconstruct consumed foreground authority. Restore manuscript state only through recorded revisions and promotion lineage.

## Acceptance

- A fresh user can write and accept or dismiss local caret suggestions without entering research mode.
- Candidate presentation and accepted bytes match exactly; stale acceptance fails closed.
- Research fixtures prove frozen inputs, exact token-piece boundaries, role separation, and non-replayable live seals.
- Promotion receipts name the narrow foreground-command claim and consume authority once.
- Manuscript/revision migration and quit/relaunch tests preserve exact content and lineage.

## Consequences

Loom retains specialized research modules and evidence storage, but they no longer dominate ordinary writing UX or inflate routine claims. The product can remain ergonomic while preserving a strict, auditable path for research-grade generation and adoption.
