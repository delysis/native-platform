# Frozen-Model Residual Training

## Experiment Card

- title: Explicit native terminal-token or response-span residual extraction and bounded contrast-subspace training
- hypothesis: Native owner-thread execution can learn residual directions from training contrasts and optimize their signed continuation likelihood without updating model weights or reading held-out examples during fitting.
- data slices: Exposed authored, non-personal smoke contrasts with disjoint training and validation token sequences; locally cached Gemma 4 E4B-it and 31B-base GGUFs for the paired runtime protocol, not new behavioral confirmation.
- modality inputs: Exact token IDs, selected post-block hidden states, unsampled teacher-forced logits, and model fingerprints.
- proxy variables used and why: None. The generic API has no demographic or personality-label vocabulary.
- invariance checks: Unchanged model bytes; explicit layer indexing; zero-vector versus no-vector equivalence; positive and negative interventions; held-out examples excluded from initialization and search; context isolation from normal generation.
- privacy review: Local in-process inference only. Callers own storage and data access. Upstream tests and examples contain no private person records or corpus text.
- evaluation metrics: Mean continuation log likelihood, signed pole margins, symmetric softplus loss, vector-stack norm, training-only optimization trace, validation-only final evaluation, and cancellation/owner-join evidence.
- ablations: Zero control, contrast-initialized control, optimized layer gains, reversed sign, repeat/no-op execution, matched-schedule terminal-token versus response-span-mean extraction, and the original V2 terminal reference on the same exposed pairs and two frozen models.
- failure criteria: Missing or unsupported hidden tensors; non-finite values; mismatched dimensions or model fingerprints; overlapping training/validation examples; zero-norm contrast axes; cancellation; a failed artifact-identity check. No learned model is returned on failure.
- release decision: Generic research primitive. Successful execution does not promote a personality axis or establish steering generalization.

## Boundaries

The model is frozen. The native training method estimates each selected layer's
direction from the mean paired hidden-state difference, then performs bounded,
deterministic coordinate search over the layer gains. Its objective is symmetric
positive/negative teacher-forced continuation likelihood with L2 regularization.
It does not implement autograd through llama.cpp, update quantized weights, or
claim numerical equivalence to a PyTorch Adam optimizer over every hidden
coordinate. Context-gated nonlinear actuator training is a separate algorithm.

Admission currently requires the `gemma4` architecture. A generic decoder flag
is insufficient: some diffusion architectures expose decode without causal KV
memory. Extending this allowlist requires architecture-specific runtime evidence.
Base and instruction-tuned weights use the same native architecture contract;
qualification of one model does not establish training quality on another.
The opt-in runtime protocol will also be exercised unchanged against the cached
Gemma 4 31B base Q8_0, with layer 20 and the same search hyperparameters. This
second model is an implementation check, not a replication of a personality claim.

Before fitting, the first training pair is scored with no control and explicit
zero control twice, followed by cleared-control replay, using the same execution
mode in all comparisons. Maximum per-token log-probability
drift must not exceed `1e-5`; token errors cannot cancel in a mean. Gain updates
require more than `4e-5` absolute loss improvement, a computational noise floor,
not statistical confidence. Zero gains always use disabled-control semantics,
including validation. Zero-sweep and zero-radius runs must equal their baselines.
The cvec layer interval is the smallest interval covering the selected blocks;
unselected rows within that interval are zero. An initial Metal run enabled
zero controls across every block and failed this numerical gate before fitting.
Restricting the interval avoids unnecessary zero-add operations outside the
intervention. The original tolerance is retained.
This probe covers only those target tokens, not the full vocabulary or every
possible input. Fixed-range controls may still add zero operations in gaps
between selected blocks. Passing the probe is not a universal error bound.

Training cases and validation cases are separate inputs. The latter are only
scored at the no-op baseline and once after training; they cannot determine
directions, search steps, stopping, or checkpoint selection. Exact token overlap
is rejected but semantic near-duplicate detection remains the caller's duty.

Residual extraction and scoring run on the model's existing owner worker using
fresh temporary contexts, never a shared generation KV cache. The intervention
begins at the last prefix token and continues through teacher forcing. Prefix
tokens before that position remain unmodified. Native layer/tensor semantics
are recorded explicitly rather than inferred from another framework's index.
Temporary contexts use one sequence and at most 4096 context tokens, independent
of a larger resident chat context. Extraction temporarily holds both capture and
scoring contexts; callers must allow additional KV/compute memory. The API bounds
vector allocation and token work, not elapsed time or total model/backend RAM.

The terminal-token V2 method batches those unmodified prefix tokens into chunks no larger than
`min(n_batch,64)` without requesting unused logits. Capture is disabled until the
final pole token; that token and the intervention boundary remain singleton
decodes. Controlled teacher forcing also remains singleton. The V1 singleton
reference is immutable commit `9218348`; changing batch size to one in V2 does
not recreate V1 because V2 suppresses unused prefix logits. Batch-shape changes
can change numerical outputs, so V2 has a distinct method label and build identity.

Local V2 E4B-it Q8_0 checks on 2026-09-16 passed at resident batch limits 16
and 64. Saved smoke directions, gains, losses and evaluation cases matched across
those two runs. Against V1, direction cosine was 0.9999993713 and the selected
gain remained 0.25, but heldout final loss moved from 0.9419453407 to 0.9418227574.
No-op drift was zero. These are numerical receipts on two authored contrasts,
not an equivalence guarantee or trait replication. The batch-16 test additionally
exercised capture with two different prefixes longer than 129 tokens. Both tests
exercised multi-chunk zero/cancelled-sum evaluation. Timings were contended with
another model run and the test workload changed; no speedup is established.
Exact chunk-edge sweeps, cancellation during prefill, and V2 31B-base nonzero-gain
optimization remain additional live checks. V1's 31B receipt is not V2 acceptance.

### Response-Span Mean Specification

`ResidualPooling::TerminalToken` preserves that V2 extraction schedule and method
label. `ResidualPooling::ResponseSpanMean` is an explicit alternative with method
label `paired_response_span_mean_direction_bounded_coordinate_search_prefix_batches_max64_v3`.
It clears controls and KV state per pole, disables capture during the whole
neutral prefix, then decodes each pole token singly with capture enabled. Only
the state **after** that pole token contributes; no prefix token, padding, or
additional EOS token is included. An EOS already present in caller-supplied pole
tokens is part of the pole. Exact-token boundary selection remains the caller's
responsibility; the runtime neither retokenizes nor infers response spans.

`ResidualPooling::TerminalTokenMatched` is the V3 causal-ablation companion with
method label `paired_terminal_token_matched_direction_bounded_coordinate_search_prefix_batches_max64_v3`.
It uses exactly the same prefix-only prefill, enabled capture, singleton pole
decodes, requested outputs, and per-token capture validation as `ResponseSpanMean`.
It replaces its retained representation after every token and returns only the
final one. Earlier states are discarded, not skipped or retained in a token array.
Both modes fail on malformed/nonfinite intermediate captures even when the final
capture would be usable. Their only extraction difference is the reduction over
those captures; neither unit-normalizes a token or pole representation.

For layer `l`, pair `i`, and positive/negative token counts `T+` and `T-`:

```text
p[l,i] = sum_t h[l,i,positive,t] / T+
n[l,i] = sum_t h[l,i,negative,t] / T-
m[l]   = sum_i (p[l,i] - n[l,i]) / number_of_training_pairs
d[l]   = m[l] / ||m[l]||_2
```

All sums and divisions before the final unit-direction cast use f64. Each pole
has equal weight regardless of token count, then each pair has equal weight.
There is no per-token or per-pole unit normalization, no normalization across
layers, and no change to gain fitting, likelihood scoring, or model weights.
One-token poles have the same decode schedule and representation in all three modes.
The no-op and signed scoring schedules remain identical across modes.

Capture retains one bounded f32 selected-layer snapshot, not a sequence of
hidden states. Pole sums, the previous pole mean, and paired sums each have at
most 1,048,576 f64 values (8 MiB each). The wrapper additionally owns its bounded
f32 capture storage and the current returned snapshot. These bounds exclude
context/model/backend memory. Cancellation is checked before and after every
captured token; incomplete poles, wrong layer order/shape, nonfinite values, and
capture failures abort rather than produce a partial direction.

Span extraction changes pole decode batch shape relative to V2 terminal
extraction: V2 prefills every token before the final pole token in bounded chunks;
span mode must capture every pole token singly. Preserving the V2 reference means
this comparison is **not** guaranteed to isolate pooling from batch-shape
numerics. Pure arithmetic tests isolate the averaging rule, not kernel behavior.
The matched-terminal V3 versus mean V3 comparison removes that schedule confound.
Use V2 versus matched V3 only as a separate batch-shape numerical check; do not
substitute V2 for the matched pooling ablation.

Historical V1/V2 receipts do not establish acceptance of response-span extraction.
On 2026-09-16 the focused V3 runtime protocol completed eight zero-search fits
per model, at layer 20, batch 64, on the same exposed authored smoke pairs:

| Model | Matched terminal / mean cosine | V2 terminal / matched cosine |
| --- | ---: | ---: |
| Gemma 4 E4B-it Q8_0 | 0.5377560489 | 0.9999993232 |
| Gemma 4 31B base Q8_0 | 0.7127156295 | 0.9999999245 |

On both models, all three modes produced exactly equal directions on single-token
poles. Replacing only held-out pairs left directions, gains, and training history
exactly unchanged in both V3 modes. Each owner joined one expected worker. All 17
write-time artifact hashes per run were independently rechecked. Comparison-file
SHA-256 values (model hashes appear in the historical receipts below):

- E4B-it: `35c598f18baa1e9f6ad5b62cc54ed87168cb1c9ca7fbec1df3aadef4682dd2b4`
- 31B base: `2b852a54592f1278ec263dcd34626d2187b89289164a0fbaa725cc521314f8d0`

These zero-search fits establish extraction and isolation behavior, not nonzero
steering, better held-out loss, unsupervised trait discovery, or controller
qualification. All gains remained zero and final metrics equaled baselines by
design. A zero/near-zero direction remains a failure, not a reason to select a
new pair or silently fall back to terminal mode. Nonzero gain optimization and
behavioral comparisons require separate receipts; neither cosine nor runtime
test duration measures quality or throughput.

The full nonzero-gain E4B-it integration protocol then passed separately for both
V3 modes (one search sweep, gain radius 0.5, step 0.25; two train/two held-out
authored pairs). Both selected gain 0.25 and had zero measured no-op drift:

| E4B-it extraction | Train loss | Held-out loss | Held-out signed utility |
| --- | --- | --- | ---: |
| Matched terminal | 1.092831486 -> 1.080992381 | 0.950441138 -> 0.941856971 | 0.016792745 |
| Response mean | 1.092831486 -> 1.081044922 | 0.950441138 -> 0.943208098 | 0.014166155 |

Both-sign polarity agreement remained zero in both runs. Mean pooling was worse
on this tiny exposed held-out set; do not infer a general ranking. Both passed
heldout mutation independence, zero-sweep/radius identity, ordinary-generation
isolation, signed evaluation, cancellation, and owner joining. Output SHA-256:

- Matched terminal: `f927f66aa18fd65ce12362579188e57b9044b0b0a2598028b743008ebf16f6d4`
- Response mean: `469968407ec86f84bc3469c4bef3698deac320a70e03bb70ed9f4adf8eaa3006`

On the single eight-token E4B generation probe, both signs in both modes produced
exactly the zero-control token IDs and hit the token limit. Mean reference KL on
the supplied continuation was about 0.000069/0.000068 for matched terminal and
0.000102/0.000095 for span mean (positive/negative). Nonzero distribution shifts
therefore did not change this greedy rollout; neither outcome establishes task
preservation or independently observed personality behavior.

Both full V3 modes also passed on 31B base, unchanged layer/search settings and
the same authored token pairs. Both selected gain 0.25 and had zero no-op drift:

| 31B base extraction | Train loss | Held-out loss | Held-out signed utility |
| --- | --- | --- | ---: |
| Matched terminal | 0.891883426 -> 0.889379033 | 0.936227743 -> 0.934705407 | 0.003042821 |
| Response mean | 0.891883426 -> 0.889449335 | 0.936227743 -> 0.934392220 | 0.003708324 |

Output SHA-256:

- Matched terminal: `89e0dd5d73a86c1a6673df199d7236f63241898b6753ccd4e759856dfff6c9e6`
- Response mean: `b69b77859d864e3a0b35d745a81602034ac752b9c8ff9783b7b40b29822a494b`

The 31B checks passed the same isolation, signed evaluation, cancellation and
owner-join assertions. Both-sign polarity agreement remained zero, and all four
eight-token greedy cases matched their zero-control case. For each model, saved
requests differed only in pooling; model/execution fingerprints and all baseline
metrics matched exactly. Mean pooling's small held-out advantage on 31B reverses
the E4B ordering. There is no replicated pooling winner or behavioral controller
claim from these exposed two-pair tests.

The final three-mode source passed 245 tests across the four native packages,
20 doctests, strict native all-targets Clippy, workspace formatting, architecture,
policy, and diff checks. New tests cover raw f64 means, unequal pole lengths,
finite shape/count bounds, prefix exclusion, identical singleton schedules,
cancellation, training-only extraction, and mode/request/method tampering.

## API And Data Contract

`ResidualTrainingRequest` contains model/request identities, mandatory typed
`pooling` (`terminal_token`, `terminal_token_matched`, or `response_span_mean`), unique interior
zero-based block indices, exact-token positive/negative pairs in separate train
and validation partitions, and bounded search/loss parameters. Each pair has a
nonempty prefix; the final prefix token predicts the first continuation token.
Clients should use native tokenization of complete sequences and explicit token
boundaries rather than concatenating separately tokenized text fragments.

The output carries model and temporary-context fingerprints, a canonical request
hash, unit directions in requested-layer order, scalar gains, per-sweep training
metrics, baseline/final held-out metrics, intervention semantics, and the no-op
diagnostic. Mean likelihoods exclude prefix tokens. Unit directions are retained
even for a no-op fit; their gains remain zero. All outputs are local research
artifacts and may encode information from the source data.

The output also requires `pooling`. `validate_for_request` checks this against
both the request mode and its canonical hash, while receipt validation checks the
mode-specific training-method label. The request hash domain is now
`llama-native.residual-training-request.v2` with a fixed mode tag; even terminal
requests get new hashes. This is a request-encoding version, not the extraction
method version. Missing or unknown modes are rejected; there is no serde default
or automatic rewriting of archived JSON. Model fingerprints continue to identify
the model/context rather than the extraction rule. Receipt binding combines those
fingerprints with the mode-bound request hash and method. Serialized receipts are
audit consistency evidence, not signatures or independently verifiable execution.

`handle.train_residual(request)?.wait()?` returns a move-only
`VerifiedResidualTraining`. Its `output()` is serializable but is only an audit
record, not a rehydratable execution credential. After `owner.shutdown_joined()`,
`belongs_to_joined_model` checks exact worker-instance identity. Dropping a ticket
requests cancellation; request IDs stay reserved until the executor releases
them. Cancellation is checked between bounded prefix chunks and controlled token
steps, not inside a native kernel; chunk limits do not bound wall-clock latency.

No file, network, dataset, or personality-label authority is added to the runtime.
Applications enforce input byte limits before deserializing, retain their own
experiment/provenance files, and decide whether a fit warrants later evaluation.

## Focused Pooling Runtime Check

```sh
LLAMA_RESIDUAL_MODEL=/absolute/path/model.gguf \
LLAMA_RESIDUAL_POOLING_OUTPUT_DIR=/absolute/path/new-output-directory \
cargo test --locked -p llama-native-engine --test residual_training \
  local_pooling_matched_schedule_and_single_token -- --ignored --exact --nocapture
```

Run once per model, sequentially, after the downstream card is amended and frozen.
This test runs eight fits with two training pairs, one held-out pair, zero search
sweeps, layer 20 (override `LLAMA_RESIDUAL_LAYER`), GPU offload, context 512, batch
64, and a 64-token full-sequence admission cap. It does not run the older test's
generation or residual-evaluation grid. Cases are full poles in all three modes,
one-token poles in all three modes, and changed holdout in both V3 modes. All use
the same exposed smoke source pairs; single-token cases truncate each exact pole
to its first token. No confirmation data is opened.

Every fit still performs native no-op checks and baseline metrics; this is not
an extraction-only shortcut. `LLAMA_RESIDUAL_POOLING_TIMEOUT_SECONDS` defaults to
300 per fit, bounded to 1..=3600. Timeout requests cancellation and fails the test;
loading/joining remain cooperative with no hard time guarantee. No speedup or
direction superiority is asserted. The test requires exact all-mode one-token
direction equality and unchanged V3 directions/gains/history under holdout
replacement. Full-pole matched-versus-mean cosine and maximum coordinate delta
are reported, not thresholded. Zero sweeps do not establish optimizer efficacy.

The output directory must be new. It contains `config.json`, eight
`{full|single|holdout}-{terminal|matched|mean}-{request|output}.json` pairs for the
executed cases, and `comparison.json` with per-file SHA-256 and verified joined
owner checks. Files use create-new and sync; failed runs may leave partial
artifacts without a successful comparison receipt. Receipts remain audit-only.

## Full Opt-in Runtime Check

```sh
LLAMA_RESIDUAL_MODEL=/absolute/path/model.gguf \
LLAMA_RESIDUAL_POOLING=response_span_mean \
LLAMA_RESIDUAL_TEST_OUTPUT=/absolute/path/new-output.json \
LLAMA_RESIDUAL_TEST_REQUEST=/absolute/path/new-request.json \
cargo test --locked -p llama-native-engine --test residual_training \
  local_training_holdout_isolation_and_join -- --ignored --exact --nocapture
```

The two artifact paths are optional and must not exist. Relative paths resolve
from the crate test directory, not the workspace root. The test uses authored
non-personal contrasts, not research-corpus fixtures. It checks validation-only
mutation invariance, zero-sweep and zero-radius identity, ordinary-generation
isolation, cancellation, and owner joining. It does not assert held-out benefit.
The test constructs an explicit pooling field; its default is `terminal_token`,
and any unrecognized `LLAMA_RESIDUAL_POOLING` fails before loading a model. Run the
same test separately with each mode and new receipt paths to preserve the paired
comparison. Do not run the two large models concurrently or use timing from a
contended machine as throughput evidence.

## Falsification

If no-op changes the model's logits beyond declared numerical tolerance, if a
validation-only change changes fitted directions or gains, or if missing layers
are silently substituted with final embeddings, the implementation is invalid.
If held-out signed margins do not improve, the particular learned actuator has
not demonstrated transfer, even when all runtime checks pass.

## Historical V1 Local Verification: 2026-09-16

Binding: `b508f1c7652c751513c361c7e4fdeb090fbed577`, proposed in
[llama-cpp-rs PR 14](https://github.com/delysis/llama-cpp-rs/pull/14).
Native submodule remains `5f55650a78f92aff4d48d671423e888fac0469ff`.

On Apple M4 Max / Metal, Gemma 4 E4B IT Q8_0
(`fb8f0c032de00b18c710824af3c7e5777c71e5fb60b13f13575f0a9e92ddecd0`):

- Layer 20, one sweep, gain radius 0.5, step 0.25, two train/two held-out pairs.
- Fitted gain: `0.25`; train loss `1.092775678 -> 1.080971781`.
- Held-out loss `0.950405410 -> 0.941945341`; signed utility `0.016593238`.
- Both-sign polarity agreement remains **zero**: these small shifts do not
  reverse the model's preferred pole. Do not promote this as a usable faculty.
- No-op/replay maximum target-token log-probability delta: `0`.
- Held-out replacement left directions, gains, and history exactly unchanged.
- Zero-sweep and zero-radius outputs exactly equaled their baselines; ordinary
  greedy generation was unchanged; cancellation and joined-owner checks passed.
- Output-file SHA-256:
  `8dd0c147275b5bbc2a0c7f66b5c53c05357b8b3870a650bcfc59e79da1dbd3c6`.
- Exact-request-file SHA-256:
  `7203389bec8962235f25f5f083c7fa706640a406ad2a473b67e69a5541b70424`.

This is runtime and optimization smoke evidence on authored contrasts, not
population evidence, cross-domain generalization, or an LM psychometric claim.
The initial all-layer zero-control attempt failed before fitting. A subsequent
run passed all runtime assertions but failed only its optional receipt write
because a relative output path resolved from the crate directory. The recorded
successful run used new absolute output paths; no prior artifacts were replaced.

The same fixed protocol also passed on Gemma 4 31B **base** Q8_0
(`2b739f4d97c7559d0354bd87901b1571e839525108fc5c9747415982fc57400f`):

- Layer 20 gain `0.25`; train loss `0.891889243 -> 0.889376915`.
- Held-out loss `0.936247198 -> 0.934714370`; signed utility `0.003061191`.
- Both-sign polarity agreement again remains **zero**. Neither smoke fit is
  evidence of a usable, independently replicated personality faculty.
- No-op/replay maximum target-token delta `0`; all holdout mutation, no-op,
  generation isolation, cancellation, and joined-owner assertions passed.
- Output-file SHA-256:
  `9216edbd3c04bb32e04ea25e7546e670ba0624b44363e8f987def43e355ef192`.

Local native gates passed 224 package tests and 19 doctests, native-group strict
Clippy and formatting, architecture checks, and diff checks. The opt-in test is
registered separately in `ci/ignored-tests.json`; ordinary green CI does not
stand in for model execution. That registry also records the reviewed build-script
digest after the immutable binding revision update.
