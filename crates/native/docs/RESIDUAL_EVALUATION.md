# Residual Evaluation

`NativeModelHandle::evaluate_residual` evaluates bounded residual interventions on
the loaded Gemma 4 model. This is a research API, not an endorsement of the supplied
profiles and not an extension of the promoted controlled-generation capabilities.

## Contract

`ResidualEvaluationRequest` binds the exact model SHA-256, source-digested unit
profiles, exact token cases, coefficients and actual composed-stack norm bound.
Each profile has one unit direction per selected interior post-block index. Its
case coefficient applies to each of those layers. For independently weighted
layers, supply separate profiles. Layer indices follow the training API.

Composition sums shared-layer contributions in f64, rounds once to f32, then
checks the norm of the resulting stack. Reinforcing directions can exceed a bound
even when coefficient norm does not. Opposing profiles can cancel exactly. The
runtime rejects excess norm, never rescales a request silently. A source digest
is caller provenance; it does not certify that the source exists or is meaningful.

All work stays on the resident model's owning worker. Evaluation allocates temporary
contexts; the generation KV cache and active generation controls are not modified.
The model artifact is verified before and after execution. A move-only
`VerifiedResidualEvaluation` binds a successful result to a joined owner. The
serializable DTO is audit evidence, not transferable live authority.

Teacher forcing and generation independently restart at the case prefix. The
control starts at its last token and persists through decoding. Prefix tokens
before that boundary are unmodified. Empty continuation skips teacher forcing;
zero new-token limit skips generation. At least one operation is required.

Teacher forcing returns mean continuation log probability and full-vocabulary
`KL(reference || controlled)` on the same token history. Neither metric includes
prefix predictions. KL is not computed on diverging generated histories, and a
small KL is not a task-preservation guarantee.

Generation uses raw-logit greedy argmax, lowest token ID on ties. The seed is
recorded but unused; this is not a configurable or stochastic sampler. EOG consumes
a selection slot but is not included in generated IDs. `token_limit` means exactly
the requested number of non-EOG IDs; no generation is `not_requested`. Generated
bytes preserve token spelling, including special tokens and possibly incomplete
UTF-8 at a token limit; consumers must not mistake lossy rendering for exact evidence.

## Bounds And Lifecycle

Requests allow at most eight profiles, 64 cases, 16 total profile-layer entries,
4096 context tokens, 256 new tokens per case, 32768 input tokens and 65536 estimated
decode tokens. Dense control storage is capped at 64 MiB. Contexts use one sequence
and at most 4096 tokens. Reference KV storage is omitted for generation-only work.
Inputs are validated before context allocation, including model vocabulary, layer
indices, direction widths and the actual norm of every case.

Dropping/cancelling a ticket requests cooperative cancellation. Decode operations
themselves are not interruptible; cancellation is checked between token steps.
Timeout is not an OS hard deadline. Owner shutdown cancels active and queued work
and joins the worker. Failed or cancelled evaluation returns no partial success DTO.

## Verification

Pure tests cover overlapping directions, reinforcement, cancellation, malformed
dimensions, hashes, actual-norm receipts, termination, finite full-vocabulary KL,
greedy tie handling and operation lifetime. The existing opt-in `residual_training`
integration test additionally exercises composition, zero identity, model mismatch,
cancellation and ordinary generation isolation on a supplied local GGUF:

```sh
LLAMA_RESIDUAL_MODEL=/absolute/path/model.gguf \
  cargo test --locked -p llama-native-engine --test residual_training -- --ignored --nocapture
```

`LLAMA_RESIDUAL_EVALUATION_OUTPUT` optionally specifies a new absolute JSON path.
No model downloads occur. A passed runtime test is not behavioral qualification.

## Local Runtime Evidence, 2026-09-16

On macOS/Metal, the authored integration probe passed with E4B-it Q8_0
(`fb8f0c032de00b18c710824af3c7e5777c71e5fb60b13f13575f0a9e92ddecd0`)
and 31B base Q8_0
(`2b739f4d97c7559d0354bd87901b1571e839525108fc5c9747415982fc57400f`).
Both exact-zero and opposing-profile cancellation had zero teacher-forced KL and
matching greedy IDs. Signed dose 0.25 produced finite full-vocabulary KL:

| Model | Positive KL | Negative KL |
| --- | ---: | ---: |
| E4B-it | 0.0000687048 | 0.0000673880 |
| 31B base | 0.00000153168 | 0.00000139854 |

The later 31B run also exercised reordered cases, alternate forced targets with
unchanged free-generation IDs/bytes, forced-only scoring equivalence, byte parity
with ordinary generation, and cancellation after worker activity began. Both
runs verified model-hash rejection, resident generation isolation and owner join.
The ordinary-generation comparison is specific to these execution settings;
singleton versus batched prefill need not be bit-identical on every backend.
Explicit zero-add graph drift is tested by the training API's separate no-op probe;
evaluation's exact-zero composition deliberately takes the disabled path.
Neither tiny probe establishes semantic control, useful dose ranges or composition
generalization. EOG/gapped-layer live coverage and strict-artifact mutation during
evaluation remain additional integration cases, not claimed as exercised here.
