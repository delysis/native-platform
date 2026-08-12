# W1 contract boundary audit

The `unstable-w1-contract-tests` feature is pinned to immutable contract commit
`cbab33555ab9355a6ac453d659c55ec9e0666821`
(`w1-contracts-v0-2026-08-12-r3`). The dependency has one manifest owner,
`loom-host`, and is re-exported only while the test feature is enabled so the
Tauri composition test cannot drift to another contract revision.

## Accepted lifecycle manifest

Declared implementation: `loom / interactive-generation-registry`.

One `LifecycleCoverageManifest<LoomInteractiveLifecycle>` is accepted from
eleven passing compositional suites and all eighteen required invariants. The
evidence is assembled from the production owners below; the adapters do not
store a second lifecycle state machine.

| Suites | Production owner |
| --- | --- |
| transition chain, registry identity, attempt hierarchy, consumer cancellation, terminal authority | `loom_host::GenerationSupervisor` |
| waiter control | `loom_host::GenerationRegistry` |
| admission/quiesce/shutdown, progress/shutdown, panic/shutdown, stable shutdown, task reaping | Tauri `ApplicationPhase`, the shared `GenerationSupervisor`, and `GenerationWorkerRegistry` owning the actual outer `JoinHandle`s |

`start_weave` reserves the real family registry and the shared supervisor while
holding application admission, advances Reserved -> Queued -> Running, and
attaches the executor to the same supervisor. Pre-executor failures record an
explicit Failed terminal and Released projection. Poisoned lifecycle state is
returned as an error; it is never projected as a successful empty or Closed
state.

`loom-store` remains the durable terminal authority. The Tauri release boundary
requires exactly one persisted terminal for every run before releasing the
family registry and atomically recording the supervisor's matching terminal
and Released projection. Canonical store event sequence numbers feed the
supervisor's bounded lossy progress projection. A missing or poisoned
supervisor operation is an error, not permission to skip lifecycle release.

Worker evidence comes from request IDs captured when real outer workers are
attached and from the actual order in which `GenerationWorkerRegistry` joins
them. Panic tests use the same retained outer `JoinHandle` path as the native
Llama owner. Shutdown quiesces admission, cancels operations, joins every owned
worker, closes the supervisor only after its operation and worker sets drain,
and returns the same canonical shutdown facts on replay.

## Privacy boundary

`BuildModelPolicy` remains a closed allow-list whose real policies all declare
`InferenceBoundary::LocalOnly` and `HostedFallback::Forbidden`. The adapter
maps those declarations to the W1 local-only privacy envelope. It performs no
hosted call, carries no credentials, and grants no network authority.

## Verification

The dedicated workflow checks the immutable pin, runs the full `loom-host`
feature suite, runs the full `tauri-plugin-loom` feature suite containing the
accepted cross-crate manifest, and applies strict clippy to both crates and all
targets.
