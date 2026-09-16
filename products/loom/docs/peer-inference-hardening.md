# Peer compute hardening: lifecycle and receipts

## Scope

This change follows the September 16, 2026 report, *Making cooperative local
inference economically viable*. Its source baseline is the peer integration at
`a7daf722392cf9122bf7adbe95096ba6cf9f98f8`, from PR #49, not the report's older
`cd79b47a0fd8f10adbcf9d641544544bbd00930f` peer revision. It is a correctness
prerequisite for wider scheduling, not an implementation or qualification of
cross-request batching.

The reviewed boundaries are `ComputeHost`, the native peer adapter, the wire
reader, the two durable compute ledgers, and the enclosing network shutdown.
The edits are confined to `loom-cabal` and this note. No native execution,
model configuration, grant budget, retention policy, or manuscript authority
is broadened. This is not an exhaustive audit of every native-platform crate.

## Defects and contracts

| Boundary | Defect in the baseline | Contract after this change |
| --- | --- | --- |
| Host shutdown | Taking the join handle before awaiting it lets a cancelled shutdown future detach the supervisor; a later caller can report success prematurely. | The handle stays in its owner slot until joining completes. A later caller joins the same supervisor and observes its retained persistence failure. |
| Network shutdown | The enclosing anti-entropy supervisor has the same take-before-await ownership hazard. | A cancelled shutdown keeps the supervisor's join handle available to the next caller. |
| Receipt wire reader | Signed status replies receive weaker structural checks than durable client records. | All receipt readers use the same model, identity-shape, digest, timestamp, status/revision, and output-size validation. Signature verification and exact request binding remain separate requirements. |
| Observed history | Increasing revisions alone can accept an impossible path, such as Running/1 to Cancelled/2, or a cancellation terminal that skips a revision. | Missing replies are allowed only when the resulting history is reachable. Cancellation reasons and immutable identities cannot change. |
| Host ledger writes | A changed cancellation reason or oversized output can be inserted and only rejected on a later read. | Validate the proposed direct transition before starting the write transaction; rejecting it leaves the last valid receipt readable. |
| Host ledger reads | The unsigned SQL revision index is not checked against signed revisions, and missing acceptance history can escape detection when byte accounting is consistent. | Require signed/index agreement and every contiguous revision beginning at acceptance. Do not impose this completeness rule on a requester that may have missed replies. |

The wire and storage versions remain 4. Valid existing records retain their
meaning; malformed history is rejected without migration or silent repair.
A signed receipt still authenticates a host assertion, not the asserted model
execution, host confidentiality, or deletion of retained prompts.

## Regression coverage

The new tests are ordinary, non-ignored Rust tests:

- `tests/compute_shutdown.rs` submits through the public host and authenticated
  QUIC path. A fixture deliberately remains alive after observing cancellation.
  Abandoning one shutdown must not allow a second to report drained before the
  fixture is released. The terminal receipt must be HostStopping cancellation.
- `src/transport_shutdown_tests.rs` exercises the production network shutdown
  method with a parked supervisor in its owner slot. It distinguishes joining
  from merely dropping the caller future.
- `src/compute/record_tests.rs` covers valid receipt shapes, validly signed
  malformed records, and reachable versus impossible skipped observations.
- The existing wire test now rejects malformed signed Submit and Status replies.
- The existing ledger tests now cover rejected writes leaving readable history,
  tampered revision indexes, missing acceptance records, and an abandoned
  shutdown that must not erase an injected persistence failure.

`loom-cabal` is already in `ci/package-groups.json` under `product-loom`.
`scripts/ci/cargo-group.mjs test product-loom` uses `--locked --all-targets`, so
these tests enter the existing product CI, including its macOS lane. No new
workflow or ignored-test exception is needed.

Targeted commands for qualification are:

```sh
cargo fmt --package loom-cabal -- --check
cargo test --locked --package loom-cabal --all-targets
cargo clippy --locked --package loom-cabal --all-targets -- -D warnings
```

These are required commands, not executed-check receipts. The browser authoring
session did not run Cargo or real-model tests locally. Consult the PR's actual
Actions runs for compile, formatting, lint, and test results at the final SHA.
The lifecycle fixtures do not establish native execution, physical-device
transport acceptance, or an economic improvement.

## Report obligations still open

The host still admits one peer job, and its native adapter still yields to
foreground work. No independent-request batching or continuous admission is
claimed. Preserve per-job cancellation, grants, cache scopes, input/result
binding, and native completion ownership when that scheduler is implemented.
Its acceptance must instrument the real native decode boundary: two distinct
request owners share a batch, cancelling one leaves the other correct, and
the model is loaded once. A fixture callback count cannot replace that test.

Physical-device direct/forced-relay operation, foreground latency, native
model replacement and shutdown, sustained storage growth, resource exhaustion,
and measured useful-work economics remain separate qualification obligations.
Host retention remains explicit and bounded by refusal, not automatic erasure.
Do not convert these component hardening changes into a claim of hardware
attestation, confidential inference, global exactly-once execution, or a fully
qualified cooperative service.
