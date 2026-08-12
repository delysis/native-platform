# Wave 1 FTE contract adapter

The `fte-router` test target can opt into `unstable-w1-contract-tests`. The
feature compiles a test-only adapter against the immutable
`w1-platform-contracts` revision recorded in `w1-contracts.env` and
`Cargo.lock`. The dependency is optional and absent from the default production
feature graph; it adds no provider authority.

The adapter validates only surfaces that the production Gateway actually
exposes:

- repeated shutdown of an empty real `Gateway`, through the testkit's generic
  lifecycle assertion;
- local-only routing decisions against the exact privacy envelope;
- exact capability envelopes derived from `Gateway::backend_snapshots`; and
- exact service-error envelopes derived from a real privacy rejection.

FTE does not expose the testkit's reservation, attempt identity, queued state,
progress projection, or retained-task counters. Those lifecycle operations are
intentionally unsupported by this adapter. Existing production Gateway tests
remain the evidence for duplicate request rejection, consumer-drop
cancellation, authoritative terminal handling, request draining, and backend
worker joins.

The checks use inventory-only fixture backends. No hosted request, provider
credential, network call, or claimed hosted-provider acceptance is required.
