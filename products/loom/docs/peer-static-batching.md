# Independent peer batches and native qualification

Implementation base: `cdd47396881cd1ae4dd78fecaa01cab7a6e7196a` (PR #63).
This change is PR #65, stacked on that hardening branch, not a replacement for
PR #49's cabals/Signal integration. Work was performed in ordinary Chat.

## Implemented boundary

`ComputeHost` may collect up to four independently authorized jobs for 20 ms
from the first acceptance. At most two may belong to one peer. Collection closes
under the admission lock before the worker drains the accepted channel. New
jobs receive Busy while that static batch executes; there is no waiting queue
behind it and no admission between native decode steps.

The production `NativeExecutor` batches only nonempty, byte-identical raw text
without media, in one cabal membership epoch and under the exact same model
claim. Seeds may differ; the native random-seed sentinel is not eligible for
this class. Other raw prompts, functions and media are not silently coerced into
a batch. A single admitted job retains its prior execution path.

The key remains `(authenticated peer, job UUID)`, never UUID alone. Every job
retains its original grant, input fingerprint, source document, revision,
artifact identities, cancellation token and durable receipt history. A batch
mapping records their relationship before native dispatch. Unknown, duplicate
or missing adapter output identities fail validation; output vector order is
not authority.

The native batch has one foreground-preemptible idle lease. That lease is not a
child of any request's cancellation token. Request cancellation targets only its
native case; foreground takeover cancels the whole batch and waits for its
execution to return. Dropping a per-call native owner cancels and waits rather
than abandoning its ticket. Request completion does not unload the resident
model. The cancellation-safe enclosing supervisor joins from PR #63 remain.

Each request has a monotonic deadline starting at acceptance, including
collection and execution. Revocation, expiry, cancellation before dispatch,
shutdown and persistence failure are rechecked independently. Exact retries
recover the same signed receipt and do not spend another allowance.

### Resource and completion limits

Acceptance reserves bounded collection and ledger capacity. It does **not**
yet reserve a tokenized KV allocation before accepting the request. The new
`IndependentRawBatch` prepares exact prompt tokens using the resident worker,
then submits one native `GenerationBatchRequest`; the native exact-token path
checks aggregate cell capacity before mutating KV. An execution-admission failure
after durable acceptance becomes a failed job and consumes the accepted grant
unit under the existing allowance semantics.

This is a static completion barrier: per-job final receipts are published after
the batch returns. A short case can finish decoding before its siblings but
still wait for the batch; its deadline includes that wait. Cancellation does
not free an admission slot for a later request in the running batch. Continuous
admission and earlier per-sequence settlement require a different worker state
machine, not a larger channel.

Identical input is the initial cache authorization restriction. The exact-token
submission does not implicitly reuse the preceding resident request's prefix.
Within the batch, both requesters supplied the entire shared prompt themselves.
This is not a general cross-tenant cache-isolation design. Arbitrary-prompt peer
batching, trust-scoped caches and heterogeneous media remain unimplemented.

Host retention remains the existing durable ledger plus private writing store;
this change does not implement expiry, garbage collection or proof of deletion.
It establishes no throughput, economic or foreground-latency result.

## Evidence is not interchangeable

The normal single-family manuscript validator is unchanged. Independent peer
sources are not fabricated as branches of one manuscript. The new native path
retains original requests, tokenized inputs, the actual native batch ID and case
indexes, model fingerprint, outputs and per-job provenance wrappers.

Its result is explicitly `operational_native` evidence, not a reconstructed
`VerifiedGenerationBatch`, hardware attestation or proof about a remote host.
The whole-batch strict seal is not used as a prerequisite for preserving a
completed sibling: a cancelled case can end between UTF-8 token pieces and fail
that stricter whole-batch text contract. The existing signature-versus-execution
proof distinction is unchanged.

## Executable native component qualification

From a clean, committed checkout on a supported Unix native-model machine:

```sh
node scripts/qualify-peer-native.mjs /absolute/model.gguf /absolute/new-receipts
```

This is explicit opt-in execution, not an ignored test silently skipped in an
ordinary CI run. The runner creates an isolated worktree at the exact source
SHA and requires exactly one match for the reviewed native generation decode
site. It inserts a diagnostic observation **after successful `context.decode`**,
not at admission or at a fixture callback. It leaves the working checkout's
source unchanged and records the instrumentation patch and its digest.

The compiled diagnostic provider uses the production `IndependentRawBatch` and
resident runtime behind real cabal grants and authenticated local QUIC. Two
independent device identities deliberately use the same job UUID, with distinct
seeds and separately persisted requester intent. Qualification requires:

1. Both case identities appear in one successful native decode call.
2. One requester persists and sends cancellation; a later decode contains only
   the surviving sequence, whose token count continues increasing.
3. The first job has its own signed cancellation outcome, while the second
   completes with text matching its actual native result.
4. Exact retries preserve both terminal hashes and spent allowances; requester
   reopen preserves cancellation intent and its receipt.
5. Handles acquired before and after address the same resident worker, and the
   native runtime and listeners drain before a passing receipt is written.

A missing shared decode, separate callbacks, different native batch IDs, duplicate
sequence IDs, stalled survivor, resurrected cancelled case, unrelated output,
changed model fingerprint or missing receipt fails qualification. JSONL readers
ignore an incomplete trailing record while the parent is still appending it.

The private receipt directory includes the source and instrumentation hashes,
executable hash, model fingerprint, native request/output mapping, signed peer
receipts, raw decode trace, logs and qualification summary. It is created with
private permissions and contains input/output provenance; do not publish it as
anonymous telemetry. Trace output perturbs timing, so this diagnostic build is
not a production throughput benchmark.

**Scope:** the diagnostic provider and three endpoints run locally. This does
not exercise the packaged Tauri `NativeExecutor`, UI paths, two physical devices,
forced relay traversal or Internet availability. The receipt explicitly records
`instrumented_build: true`, `packaged_tauri_adapter: false`,
`physical_networks: false` and `continuous_admission: false`.

## Regression coverage and executed validation

New ordinary Rust tests cover protocol batch identity/cancellation/recovery,
collection capacity and scope refusal, pre-dispatch revocation, output identity
bijections, independent raw-request bounds, distinct source preparation,
foreground admission refusal and rejecting invalid decode traces. Existing
single-job, shutdown, receipt-history and grant tests remain in place. No
ignored-test inventory exception or duplicate workflow was added.

The existing product `--all-targets` gate compiles the qualification example and
runs its trace-parser tests. A Rust integration test also invokes the two Node
instrumentation/parser regressions, keeping those checks in the same product
gate rather than introducing a separate workflow.

Executed in this Chat environment on September 16, 2026:

```text
node --test scripts/ci/test-peer-native-qualification.mjs
2 passed; 0 failed; 0 skipped
```

The locally tested copies matched the connected GitHub blob identities:

```text
scripts/qualify-peer-native.mjs
  d88a8e93550904bcf76625f6f1a9ddf1a0369f3d
scripts/ci/test-peer-native-qualification.mjs
  ef7041442533316c18ca20b3d4c25e46709a6bfe
```

These two checks test instrumentation placement and log parsing with fixtures.
They do not establish Rust compilation or native execution. No local Cargo,
Clippy, rustfmt, real-model, packaged-app or physical-network gate was run.
Actions run `35159749005` at `19315b8e5aea3d8207cb4b110f623c2aae29d779`
failed before runner assignment (`steps: []`, `runner_id: 0`); it provided no
Rust validation. Later head-specific CI observations belong in the PR record.

## Still required before broader service claims

First obtain passing formatting, compilation, strict Clippy and component tests
at the final revision. Then execute the instrumented native component gate and
qualify the actual packaged adapter with the same ownership/cancellation cases.
Separately exercise direct and forced-relay paths across physical machines.

Continuous admission remains unimplemented: a second request must arrive after
a first is already decoding and enter that worker before the first completes,
with chunked prefill, aggregate KV reservations, stable per-sequence samplers,
independent terminal ownership and shutdown drain. Neither the current static
batch nor its fixture concurrency is evidence of that capability.
