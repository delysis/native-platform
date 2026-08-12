# Mom Wave 1 shutdown-envelope receipt

Recorded: 2026-08-12

This candidate consumes `platform-contracts-v0` at immutable revision
`da22fa893ac183c5d9df972a7e67215c0d92b383`. Its test-only adapter converts
the result returned by an actual `AppRuntimeHandle::shutdown` invocation into
`ClosedSummaryV0`. The tests construct the real application handle, use its
real application-work registry and gateway finalizer, invoke composed
shutdown, and then convert the returned success or failure facts. They do not
construct `AppShutdownSummary` or `AppShutdownError` literals.

The application records the native host's resident slot count after app work
and the gateway have drained but before final native shutdown. That is the
expected worker count at the terminal drain boundary. The adapter preserves it
separately from the joined-worker count returned by
`ProcessExitJoinedNativeHost`; mismatches fail canonical validation. If the
native finalizer fails, the pre-finalizer resident count remains visible rather
than being replaced with the joined count.

## ADR-003 boundary

This is not acceptance of the shared operation-lifecycle model. Mom's current
application registry exposes admission directly as a live `AppWorkLease`, a
command name, an internal occurrence number, and optional cancellation state.
It does not expose:

- distinct Reserved, Queued, Running, Terminal, and Released states;
- public operation-ID uniqueness or a separate attempt identity;
- a consumer ticket whose drop requests cancellation while an executor lease
  retains identity;
- executor-owned authoritative terminal and final/progress projections;
- stale-release generation checks;
- bounded progress capacity or waiter-timeout observations;
- a retained-task count or panic terminal record.

Implementing `OperationModelAdapter` over those missing facts would require a
shadow lifecycle and would not test the real Mom registry. The full shared
model suite therefore remains open. This candidate proves only the composed
shutdown-envelope conversion and existing product-specific admission/drain
tests.

No `platform-contract-testkit` dependency or `OperationModelAdapter`
implementation is added by this candidate.

## Verification

- exact Rust 1.92.0 Mom app tests with `unstable-w1-contracts`: 44 passed;
- exact Rust 1.92.0 Mom app all-target Clippy with warnings denied: passed;
- exact Rust 1.92.0 formatting: passed;
- architecture, workflow-policy, and diff checks: passed.
