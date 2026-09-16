# Independent peer batches and native qualification

Implementation base: `cdd47396881cd1ae4dd78fecaa01cab7a6e7196a` (PR #63).
This is PR #65, stacked on that hardening branch, not a replacement for PR #49's
cabals/Signal integration. Changes were authored in ordinary Chat.

## Implemented boundary

`ComputeHost` collects up to four independently authorized jobs for 20 ms from
the first acceptance, with at most two from one peer. Collection closes under
the admission lock before the worker drains the accepted channel. New jobs
receive Busy while the static batch executes. There is no waiting queue behind
it and no admission between native decode steps.

The production `NativeExecutor` batches only nonempty, byte-identical raw text
without media, in one cabal membership epoch and under the exact same model
claim. Seeds may differ; the native random-seed sentinel is not eligible for
this class. Other prompts, functions and media are not silently coerced into a
batch. A single admitted job retains its previous execution path.

The key remains `(authenticated peer, job UUID)`, never UUID alone. Every job
retains its original grant, input fingerprint, source document, revision,
artifact identities, cancellation token and durable receipt history. An explicit
mapping records the independent sources before native dispatch. Unknown,
duplicate or missing adapter output identities fail validation; vector order
is not authority. Exact retries do not spend another allowance.

The native batch has one foreground-preemptible idle lease. That lease is not a
child of any request's cancellation token. Request cancellation targets only its
native case; foreground takeover cancels the whole batch and waits for native
completion. Dropping the per-call native owner cancels and waits rather than
abandoning its ticket. Request completion does not unload the resident model.
The cancellation-safe enclosing joins from PR #63 remain.

Each request has a monotonic deadline starting at acceptance, including
collection and execution. Revocation, expiry, pre-dispatch cancellation,
shutdown and persistence failure are checked independently.

### Resource, completion and cache limits

Acceptance reserves bounded collection and ledger capacity. It does **not** yet
reserve tokenized KV capacity before accepting the request. `IndependentRawBatch`
prepares exact prompt tokens on the resident worker, then submits one native
`GenerationBatchRequest`. The existing exact-token path checks aggregate cell
capacity before mutating KV. Admission failure at this execution boundary becomes
a failed accepted job and consumes its grant unit under the existing semantics.

This is a static completion barrier: per-job final receipts are published after
the batch returns. A short case can finish decoding before its siblings but
still wait for the batch; its deadline includes that wait. Cancellation does
not free a slot for a later request in the running batch. Continuous admission
and earlier per-sequence settlement require a different worker state machine,
not a larger channel.

Identical input is the initial peer cache restriction. Exact-token submission
disables implicit reuse of the preceding resident request's prefix. Within this
batch, both requesters supplied the entire shared prompt themselves. This is not
a general cross-tenant cache-isolation design. Arbitrary-prompt peer batching,
trust-scoped caches and heterogeneous media remain unimplemented.

Host retention remains the existing ledger plus private writing store; no
expiry, garbage collection or deletion proof is introduced. This change
establishes no throughput, economic or foreground-latency result.

## Evidence is not interchangeable

The manuscript-family validator is unchanged. Independent sources are not
fabricated as branches of one manuscript. The new native path retains original
requests, tokenized inputs, actual native batch ID and case indexes, model
fingerprint, outputs and per-job provenance wrappers.

Its result is explicitly `operational_native` evidence, not a reconstructed
`VerifiedGenerationBatch`, hardware attestation or proof about a remote host.
The whole-batch strict seal is not required to preserve a completed sibling:
a cancelled case can end between UTF-8 token pieces and fail that stricter
whole-batch text contract. Strict per-case sealing remains a separate obligation.
Signatures continue to authenticate host assertions, not prove model execution.

## Executable native qualification

From a clean, committed checkout on a supported Unix native-model machine:

```sh
node scripts/qualify-peer-native.mjs /absolute/model.gguf /absolute/new-receipts
```

The runner builds and executes this exact registered, opt-in libtest:

```text
peer_compute::batch::native_qualification::independent_peers_share_native_decode
```

This test uses the actual `NativeExecutor::from_state`, the real model-loading
command, a real private project store, resident native inference and authenticated
local QUIC. No fixture or diagnostic provider replaces the production executor.
A mock Tauri app supplies the application context; this is not packaged-app UI
acceptance. The superseded diagnostic-provider example was removed.

The runner creates an isolated worktree at the exact source SHA and requires one
match at each of two reviewed source boundaries. It records the original-to-case
mapping after native admission, and actual case/sequence membership **after a
successful `context.decode`**. It leaves the checkout's sources unchanged and
saves the instrumentation patch. Scheduler admission and callback counts cannot
stand in for the successful native decode observation.

Two independent requester identities deliberately use the same job UUID, with
distinct seeds and separately persisted intent. Qualification requires:

1. Both independently mapped cases occur in one successful native decode call.
2. One requester persists and sends cancellation; a later successful decode
   contains only the surviving sequence, whose token count keeps increasing.
3. The first job has its own signed cancellation outcome; the second completes
   with text and tokens bound to the host's actual retained native result.
4. Both original source documents/revisions remain independently bound to their
   native cases. No successful result document is created for the cancelled job.
5. Exact retries preserve terminal hashes and spent allowances; requester reopen
   preserves cancellation intent and its signed result.
6. Handles acquired before and after address the same resident worker. The active
   manuscript stays unchanged; networks drain and the real model is released
   before a passing receipt is written.

Missing shared decoding, different batches, duplicate sequence IDs, stalled
survivors, resurrected cancelled cases, unrelated outputs, changed fingerprints
or missing source bindings fail the gate. The runner also fails when the exact
test does not execute or does not produce its source-bound qualification receipt.
JSONL readers ignore an incomplete trailing record during concurrent appends.

The private receipt directory includes source and instrumentation hashes, the
actual Cargo-selected libtest executable hash, model fingerprint, native trace,
owner-to-case mapping, host/requester ledgers, production provenance, logs and
qualification summary. It contains prompt/output data; it is not anonymous
telemetry. Instrumentation perturbs timing, so it is not a throughput benchmark.

Receipts explicitly identify `production_peer_executor: true` and
`instrumented_build: true`, while `packaged_app`, `physical_networks` and
`continuous_admission` remain false. This test does not exercise separate physical
machines, forced relays, Internet availability or the native UI.

## Regression coverage and executed checks

New ordinary Rust tests cover protocol ownership/cancellation/recovery, bounded
collection and scope refusal, pre-dispatch revocation, result identity bijections,
independent raw-request limits, distinct source preparation, foreground refusal
and rejecting invalid decode traces. Existing single-job and history tests remain.

The product `--all-targets` gate compiles the new hardware test but does not run
it without explicit opt-in. Its prerequisite and promotion limits are registered
in `ci/ignored-tests.json`; the prior entries are unchanged. The ordinary trace
integration test invokes the Node tooling regressions within that same product
gate. No duplicate workflow or weakened CI gate was added.

Executed in Chat on September 16, 2026:

```text
node --test scripts/ci/test-peer-native-qualification.mjs
4 passed; 0 failed; 0 skipped
```

The two additional production-mapping/executable-selection tests were first run
against the previous runner and failed because that implementation was absent;
all four passed after implementation. The tested copies match these Git blobs:

```text
scripts/qualify-peer-native.mjs
  1bd90b9b8a8a354c958159a7f55c8fe498117114
scripts/ci/test-peer-native-qualification.mjs
  88955ea7421e03af4f6d921b9a53bde42711d0fe
```

These are instrumentation and parser tests, not native execution evidence.
**No local Cargo, rustfmt, Clippy, real-model, packaged-app or physical-network
gate was executed.** Actions run `35159749005` at
`19315b8e5aea3d8207cb4b110f623c2aae29d779` failed before runner assignment
(`steps: []`, `runner_id: 0`), yielding no Rust validation. Final-head CI status
is recorded separately in the PR; this document does not assert a passing build.

## Remaining acceptance

Obtain passing final-revision formatting, compilation, strict Clippy and component
tests, then execute the native qualification command. Separately qualify packaged
UI behavior and direct/forced-relay operation across physical machines.

Continuous admission remains unimplemented: a request arriving after an earlier
request starts decoding must enter that worker before the earlier request ends,
with chunked prefill, aggregate KV reservations, independent samplers/results and
shutdown drain. This static implementation and its fixture tests are not evidence
that continuous admission or physical-network qualification has been achieved.
