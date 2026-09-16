# Frozen-Model Residual Training

## Experiment Card

- title: Native last-token residual capture and bounded contrast-subspace training
- hypothesis: Native owner-thread execution can learn residual directions from training contrasts and optimize their signed continuation likelihood without updating model weights or reading held-out examples during fitting.
- data slices: Authored, non-personal behavioral contrasts with disjoint training and validation token sequences; a locally cached Gemma 4 E4B GGUF for integration evidence.
- modality inputs: Exact token IDs, selected post-block hidden states, unsampled teacher-forced logits, and model fingerprints.
- proxy variables used and why: None. The generic API has no demographic or personality-label vocabulary.
- invariance checks: Unchanged model bytes; explicit layer indexing; zero-vector versus no-vector equivalence; positive and negative interventions; held-out examples excluded from initialization and search; context isolation from normal generation.
- privacy review: Local in-process inference only. Callers own storage and data access. Upstream tests and examples contain no private person records or corpus text.
- evaluation metrics: Mean continuation log likelihood, signed pole margins, symmetric softplus loss, vector-stack norm, training-only optimization trace, validation-only final evaluation, and cancellation/owner-join evidence.
- ablations: Zero control, contrast-initialized control, optimized layer gains, reversed sign, and repeat/no-op execution.
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
zero control twice, followed by cleared-control replay, using identical singleton
batches. Maximum per-token log-probability
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

## API And Data Contract

`ResidualTrainingRequest` contains model/request identities, unique interior
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

`handle.train_residual(request)?.wait()?` returns a move-only
`VerifiedResidualTraining`. Its `output()` is serializable but is only an audit
record, not a rehydratable execution credential. After `owner.shutdown_joined()`,
`belongs_to_joined_model` checks exact worker-instance identity. Dropping a ticket
requests cancellation; request IDs stay reserved until the executor releases
them. Cancellation is checked at token boundaries, not inside a native kernel.

No file, network, dataset, or personality-label authority is added to the runtime.
Applications enforce input byte limits before deserializing, retain their own
experiment/provenance files, and decide whether a fit warrants later evaluation.

## Opt-in Runtime Check

```sh
LLAMA_RESIDUAL_MODEL=/absolute/path/model.gguf \
LLAMA_RESIDUAL_TEST_OUTPUT=/absolute/path/new-output.json \
LLAMA_RESIDUAL_TEST_REQUEST=/absolute/path/new-request.json \
cargo test --locked -p llama-native-engine --test residual_training -- --ignored --nocapture
```

The two artifact paths are optional and must not exist. Relative paths resolve
from the crate test directory, not the workspace root. The test uses authored
non-personal contrasts, not research-corpus fixtures. It checks validation-only
mutation invariance, zero-sweep and zero-radius identity, ordinary-generation
isolation, cancellation, and owner joining. It does not assert held-out benefit.

## Falsification

If no-op changes the model's logits beyond declared numerical tolerance, if a
validation-only change changes fitted directions or gains, or if missing layers
are silently substituted with final embeddings, the implementation is invalid.
If held-out signed margins do not improve, the particular learned actuator has
not demonstrated transfer, even when all runtime checks pass.

## Local Verification: 2026-09-16

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
