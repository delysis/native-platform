# W1 contract boundary audit

The `unstable-w1-contract-tests` feature is pinned to immutable contract commit
`cbab33555ab9355a6ac453d659c55ec9e0666821`
(`w1-contracts-v0-2026-08-12-r3`). It binds only facts owned by Loom production
types. It does not satisfy the full W1 lifecycle manifest.

## Compositional lifecycle coverage

Declared implementation: `loom / interactive-generation-registry`.

| Suite | Status | Production owner |
| --- | --- | --- |
| waiter control | passing | `GenerationRegistry` |
| transition chain | gap | no one owner exposes the contract's five phases |
| registry identity | gap | no checked attempt-sequence allocator or stale lease |
| attempt hierarchy | gap | branches are admitted atomically, not started through an attempt owner |
| consumer cancellation | gap | no consumer ticket whose `Drop` cancels while an executor lease survives |
| terminal authority | gap | durable terminal authority belongs to `loom-store`, outside this registry |
| admission/quiesce/shutdown | gap | process quiescence and worker joins belong to the Tauri application owner |
| progress/shutdown | gap | registry owns no progress receiver or supervised task handle |
| panic/shutdown | gap | registry owns no executor catch/join boundary |
| stable shutdown | gap | registry has no worker-owning shutdown operation |
| task reaping | gap | registry tracks routes, not retained task handles |

The passing waiter suite exercises a real `GenerationRegistry` family. Its
bounded zero-duration wait returns without mutating the active route; the same
ticket then routes cancellation through `cancel_run`; and `complete_family`
releases the real route after the simulated worker-side terminal-persistence
boundary. `OperationSnapshot` is only a projection of those production facts.
It does not store or advance lifecycle state in the adapter.

The test also feeds its single typed evidence item to
`LifecycleCoverageManifest::accept` and requires rejection as incomplete. This
prevents the PR from accidentally claiming all eleven suites or all eighteen
normative invariants.

## Other bound production facts

Additional focused tests retain evidence for:

- bounded family and branch reservation;
- duplicate live request, run, and branch rejection;
- project/session cancellation routing, including cancellation retained before
  a backend handle is attached;
- active route and branch counts;
- terminal-persistence failure visibility; and
- family release after durable terminal persistence.

`BuildModelPolicy` remains a closed allow-list whose real policies all declare
`InferenceBoundary::LocalOnly` and `HostedFallback::Forbidden`. The adapter
maps those declarations to the W1 local-only privacy envelope. It performs no
hosted call, carries no credentials, and grants no network authority.

## Required production refactors for full acceptance

Full lifecycle acceptance requires an application-level supervisor adopted by
the real interactive generation path. It must own distinct consumer and
executor identities, exact phase transitions, one authoritative terminal/final
projection, bounded progress, task handles, quiescence, panic conversion, and
actual worker joins. A test-only state machine or a wrapper that merely assigns
the Loom marker would be shadow state and is explicitly rejected.
