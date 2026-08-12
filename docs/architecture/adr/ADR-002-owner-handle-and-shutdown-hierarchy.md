# ADR-002: Owner/handle and shutdown hierarchy

## Status

Accepted 2026-08-12. Set-level systems-steward review is recorded in `../W1-ADRS-RECEIPT.json`.

## Context and current evidence

Phase-one work found lifecycle defects at application composition roots: hidden native-host ownership, work admitted during shutdown, retained asynchronous tasks, ambiguous terminal accounting, and processes exiting before owned workers joined. The accepted Mom, Speech, FTE, Loom, and native-runtime receipts now demonstrate the direction of repair: application-owned supervision, bounded task state, service drain before native final shutdown, and joined owner workers.

The target topology defines one application owner containing the product owner and only the service owners needed by that binary. Libraries must not mint process owners through globals. There are no existing users requiring preservation of legacy owner APIs, but shutdown receipts and recovery evidence must not be weakened.

## Decision

Each application binary constructs exactly one unique, non-`Clone` `AppRuntimeOwner`. It may mint cloneable, revocable `AppRuntimeHandle` values for Tauri-managed state and other callers.

Only `AppRuntimeOwner` can perform final process shutdown. Libraries may provide service owners and handles, but may not create hidden process owners with `OnceLock`, global singleton state, or equivalent implicit authority.

Ownership follows this shape as applicable:

```text
AppRuntimeOwner
├── ProductOwner
├── GatewayOwner
├── SpeechOwner
├── InformationOwner
├── AttachmentHandle
└── NativeLlamaOwner
```

Shutdown ordering is normative:

```text
close application admission
→ cancel and drain product operations
→ shut down and join Gateway, Speech, and Information work
→ final-shut and join resident NativeLlamaOwner workers
→ permit process exit
```

Owners construct handles. Handles request work, cancellation, and bounded service shutdown but cannot mint or clone final process-shutdown authority. Where a framework prevents consuming the owner directly, one internal terminal state machine mints one shutdown receipt; repeated callers observe the closed summary.

## Alternatives

### Cloneable owners

Rejected. Cloning obscures which value owns final join and allows competing shutdown authorities.

### Library globals or lazy singletons

Rejected. They hide construction and teardown order, impede isolated tests, and let libraries outlive application authority.

### Independent service shutdown in arbitrary order

Rejected. Native workers can be torn down while Gateway or product operations still depend on them.

### Process exit as cleanup

Rejected. Abrupt termination is not proof that buffers, stores, tasks, listeners, and resident model workers reached terminal state.

## Migration

1. Inventory every application command and background task by owning product or service.
2. Move owner construction into each binary composition root.
3. Replace hidden owner access with injected handles.
4. Route all admitted application work through the application lifecycle and the relevant service supervisor.
5. Make shutdown close admission first, then cancel/drain/join in dependency order.
6. Add aggregate shutdown receipts that report all handled resources and errors after cleanup attempts complete.
7. Delete compatibility aliases and hidden singleton paths once each product vertical is proven; no general legacy API window is required.

## Rollback

Rollback is to the last phase-one owner implementation whose real shutdown receipt proves service drain and native join. If a product cannot satisfy the hierarchy, it remains on its pre-import implementation and is not promoted or imported. Rollback must not reintroduce dual owners, detached work, or exit-before-join behavior.

## Acceptance

- Each binary has one explicit application owner and only the service owners it needs.
- Owner types are unique and non-`Clone`; handles are cloneable and revocable.
- Application admission closes before cancellation and drain.
- Services shut down and join before native final shutdown.
- Process exit follows the native owner join.
- Repeated shutdown is deterministic and returns one closed summary.
- Active registries and retained task state are empty at successful shutdown.
- Panic and cleanup errors are aggregated without skipping later resources.
- No library-owned process singleton remains.

## Consequences

Construction and teardown become explicit, testable, and dependency ordered. Callers use lightweight handles without acquiring shutdown authority. Some application wiring becomes more verbose, but ownership ambiguity, hidden lifetime extension, and teardown races become structurally harder to express.
