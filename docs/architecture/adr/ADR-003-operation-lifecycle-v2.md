# ADR-003: Operation lifecycle v2

## Status

Accepted 2026-08-12. Set-level systems-steward review is recorded in `../W1-ADRS-RECEIPT.json`.

## Context and current evidence

The native registry, Gateway, Speech, Mom application registry, and Loom host/campaign independently converged on the same lifecycle needs: stable public identity, executor-owned release, cancellation that survives consumer drop, self-reaping task supervision, truthful terminal state, and shutdown that waits for active work to end.

Prior defects included identity release tied to a consumer ticket, stale release removing a newer reservation, completed task retention until process shutdown, waiter timeout confused with worker failure, and unchecked sequence reuse. Phase-one repairs and receipts establish that these are correctness constraints, not product-specific conveniences.

## Decision

Adopt this shared semantic state machine:

```text
Reserved → Queued → Running → Terminal → Released
```

`Terminal` is exactly one of `Completed`, `Cancelled`, or `Failed`.

One public `OperationId` may be active at most once in a supervisor. Internal retries or fallback routes use distinct `AttemptId` values while status and cancellation remain anchored to the public operation. The public identity remains reserved until executor terminal accounting and authoritative final publication have been attempted.

The consumer ticket owns progress and final receivers, a cancellation handle, and public identity access. Dropping the ticket requests cancellation only. It does not release the registry entry, detach owned work, join a worker, or authorize shutdown.

The executor or task owns the operation lease, terminal accounting, final sender, and task-local cleanup. Stale releases are generation checked and cannot remove a newer reservation.

Supervisors move through:

```text
Running → Quiescing → Closed
```

Quiescing rejects new admission, requests cancellation, waits for active count zero, and joins owned execution. Retained task state is bounded by active concurrency, not historical request count. Checked monotonic allocation is used for nonces and sequences; exhaustion fails closed.

This is one semantic contract with execution-appropriate implementations such as a synchronous native owner registry and asynchronous supervisors for Gateway, Speech, Information, and product work. It is not a universal runtime trait.

## Alternatives

### Consumer-owned registry release

Rejected. Ticket drop can make an operation appear absent while its executor is still running.

### Per-service lifecycle semantics

Rejected. The products need common cancellation, terminal, and shutdown invariants, even though their request and event payloads differ.

### One public ID per retry

Rejected. It fragments cancellation and status. Attempts require their own identities beneath one public operation.

### Retain all `JoinHandle` values until shutdown

Rejected. Memory and shutdown work then grow with historical request count.

### Wrapping nonce allocation

Rejected. Reuse defeats stale-identity protection.

## Migration

1. Freeze the normative contract and model tests in the Wave 1 contracts/testkit package.
2. Implement a test adapter, not a shared universal runtime trait.
3. Run the same duplicate-ID, stale-release, ticket-drop, race, panic, shutdown, and emptiness tests against native, Gateway, Speech, Mom, and Loom implementations.
4. Add `AttemptId` beneath existing public operation identity where retries exist.
5. Move release leases to executors/tasks and make all async services self-reap.
6. Remove legacy lifecycle paths once their product verticals prove equivalent; no general compatibility alias is required.

## Rollback

Each implementation may roll back independently to its protected phase-one version while the shared contract remains unchanged. If an adapter cannot prove the contract, that repository does not import or cut over. Rollback must preserve executor-owned release, bounded task state, truthful terminal accounting, and joined shutdown.

## Acceptance

The shared contract suite proves for every implementation:

- duplicate public IDs are rejected;
- stale release cannot remove a newer reservation;
- ticket drop requests cancellation but does not release identity;
- cancel/complete and admit/quiesce races produce one terminal result;
- waiter timeout does not create worker terminal state and returns continued control;
- unread progress cannot block final result or shutdown;
- panic is captured as safe terminal and shutdown evidence where possible;
- repeated shutdown is deterministic;
- task and active registries are empty after successful shutdown;
- sequence exhaustion fails closed.

## Consequences

Products and services gain one vocabulary for admission, cancellation, terminal state, and shutdown without erasing payload-specific APIs. Implementations must maintain more explicit state and generation checks, but lifecycle correctness becomes reusable, comparable, and migration-testable.
