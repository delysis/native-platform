# Mom Wave 1 lifecycle receipt

Recorded: 2026-08-12

This candidate consumes `platform-contracts-v0` and
`platform-contract-testkit` at immutable revision
`cbab33555ab9355a6ac453d659c55ec9e0666821` (`w1-contracts-v0-2026-08-12-r3`).

Mom now owns a production `OperationSupervisor` used by its long-running
commands. Public operation IDs and monotonic attempt IDs identify registry
entries; consumer tickets request cancellation on drop while executor leases
retain lifecycle authority. The supervisor enforces the exact transition
chain, one authoritative terminal/final record, bounded progress, stale-lease
rejection, admission quiescence, and owned thread reaping. Executor panics are
caught and published as failed terminals before release. Shutdown waits for
both operation release and join and returns the exact admitted and joined
worker-ID sets.

`AppRuntimeHandle::shutdown` preserves the production supervisor's actual
phase, active-operation count, retained-task count, operation-worker counts,
and worker identities in its cached shutdown result. The contract adapter
converts those returned facts; it does not supply test-owned zeros or maintain
a shadow lifecycle. Native-host worker counts remain separate and are composed
only at the application shutdown boundary.

## Verification

- immutable r3 compositional manifest: all 11 ownership suites and all 18
  lifecycle invariants passed;
- `cargo test --workspace --all-features --locked`: 188 passed, 14 ignored
  because they require explicitly supplied real model assets;
- `cargo clippy --locked -p mom-llama-app --features unstable-w1-contracts
  --all-targets -- -D warnings`: passed;
- formatting, architecture, contracts, persona/product UX, workflow policy,
  JavaScript syntax, and diff checks: passed.
