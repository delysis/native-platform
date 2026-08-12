# ADR-009: Speech as a sibling service

## Status

Accepted — 2026-08-12. Steward review is recorded in `../W1-ADRS-RECEIPT.json`.

## Context and current evidence

Speech carries streaming PCM, decoder state, installed-platform voices, model-backed transcription/synthesis, peer cancellation, and backend-specific capabilities. Those semantics do not fit text-generation requests or events. Phase one established typed Speech ownership, cancellation, task reaping, bounded active state, real Parakeet evidence, and launched-app Apple TTS evidence. The architecture also requires capability discovery to remain side-effect free and execution to revalidate availability at admission.

There are no existing users requiring preservation of any text-shaped speech facade. Audio correctness, cancellation isolation, lifecycle accounting, and stored artifact recovery remain invariants.

## Decision

Speech remains a sibling platform service with `SpeechOwner` and cloneable `SpeechHandle`, separate from the text/model Gateway. Speech shares platform operation identity, lifecycle semantics, error envelopes, privacy policy, capability envelopes, diagnostics, and shutdown summaries. It retains distinct request payloads, audio sinks, event streams, final results, and backend capability detail.

Mom may install Speech directly. FTE may expose optional audio protocol edges that delegate to `SpeechHandle`. Loom may later use dictation or read-aloud without inheriting hosted text-provider policy. The Gateway does not construct, own, or final-shut Speech.

Speech supervision must reap completed tasks during process lifetime, retain bounded panic/error summaries, close admission before shutdown, cancel active operations, await active zero, and return a truthful joined summary.

## Alternatives

- Encode audio in Gateway text DTOs: rejected because it loses stream, timing, buffer, and backend semantics.
- Make Speech a Gateway-owned subsystem: rejected because ownership and shutdown ordering become ambiguous.
- Give each product a speech implementation: rejected because cancellation and backend behavior would drift.
- Treat build support as runtime availability: rejected; discovery and execution evidence are distinct.

## Migration

1. Stabilize versioned speech types and adapters against the shared lifecycle contract.
2. Keep Apple and Parakeet discovery side-effect free and revalidate at operation admission.
3. Route product calls through `SpeechHandle`; make any FTE protocol edge an adapter, not an owner.
4. Preserve existing audio artifacts and operation receipts through versioned conversion.
5. Freeze real and model-free peer-cancellation, synthesis, transcription, reaping, and shutdown fixtures before repository import.

## Rollback

Revert a product adapter to its previous speech integration while leaving the sibling service and stored audio artifacts intact. Do not transfer final-shutdown authority to Gateway and do not revive an implementation that accumulates completed tasks. Preserve failed and partial-operation receipts for diagnosis.

## Acceptance

- Dependency tests forbid Speech core from depending on Gateway text request DTOs or product stores.
- Peer cancellation cannot cancel an unrelated operation.
- Completed task state is bounded by concurrency, not historical request count.
- Apple launched-app and real Parakeet gates remain separately named and reproducible.
- Application shutdown closes admission, drains Speech, and reports joined backend work before process exit.

## Consequences

Products use an explicit additional service handle and adapters must translate protocol-specific audio edges. In return, audio remains type-correct, independently testable, and free from text-routing policy and ownership ambiguity.
