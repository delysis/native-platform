# Base-model operations

Research and capability inspection, 2026-09-14. This note proposes operations; it
does not establish that they are implemented or that they improve this product's
writing. No models were run for this inspection.

The smallest useful foundation is raw continuation plus explicit conditional
token likelihood. Compose experiments from those operations instead of creating
a special agent or training subsystem for every paper. Keep generation, scoring,
selection, and modification of a document distinct.

## What the local runtime actually exposes

| Capability | Current evidence | Consequence |
| --- | --- | --- |
| Raw text and exact token prompts; bounded generation; explicit sampling | `llama-native-engine/src/lib.rs::inspected_capabilities`; `GenerationInput` and `SamplingConfig` in `llama-native-types/src/lib.rs` | Available below Loom. Default sampling is temperature 0.7, top-k 40, top-p 0.95; defaults are **not** an unmodified base-model sampler. |
| Controlled generation with frozen model/token identity | `NativeModelHandle::{controlled_model_identity, generate_controlled}` in `llama-native-engine/src/controlled_runtime.rs` | Existing route for generation plus distribution observations. `NativeHostRuntime::acquire_research_handle` in `loom-backend-llama/src/runtime.rs` obtains a revocable handle while preserving host ownership. |
| Raw, post-constraint, post-guidance, and post-sampler observations | `build_distribution_observation`, `build_stage_observation`, `log_normalization` in the controlled runtime | Selected token plus up to 256 ranked tokens. Normalization uses the whole available distribution, not just the returned top-k. Stages must remain distinct. Values are finite-precision observations of the loaded model, not calibrated correctness probabilities. |
| Ordinary Loom continuation | `loom-backend-llama/src/adapter.rs::{build_native_request, build_candidate_material}` | Uses ordinary generation, chooses a writer-specific raw/chat contract, and rejects legacy probability observations rather than relabeling them logprobs. It is not a generic raw neural-function API. |
| Per-token exponentiation | `GuidanceControl::PowerSampling`; `control_math.rs::power_temperature_transform` | Multiplies current logits by the exponent. This is token-level sharpening, equivalent to inverse temperature at that stage. Later sampling controls still apply. It is **not** whole-sequence power sampling. |
| Arbitrary continuation scoring | No public teacher-forced scoring request found | Selected-token/top-k observations cannot score an arbitrary supplied continuation. An absent top-k token is unknown, not zero probability. |
| End-of-generation probability | Controlled loop records the terminal token ID, then continues before appending its observation (`controlled_runtime.rs`, `model.is_eog_token` branch) | Returned observations omit the EOG factor. Summing them gives a generated-prefix score, not the probability of that terminated outcome. The evidence ledger hashes the observation but does not supply its numeric value to this consumer. |
| Cache reuse | Exact batching, shared prefixes, snapshot/restore are exposed | Useful for repeated prompts. No caller-directed, mid-decode particle resampling/reindexing API was found. Do not equate snapshot capability with an implemented SMC scheduler. |

Native source paths above are relative to
`crates/native/crates/`; Loom backend paths are relative to
`products/loom/crates/`.

## Two primitives and an ordinary selection operation

1. **`complete(prompt, model, sampling, limits) -> generation`.** Freeze exact
   prompt/token policy, model and tokenizer identity, sampling order, seed,
   stopping rule, and token budget. An explicitly raw operation must not silently
   apply the writer adapter's instruction template. Preserve output token IDs,
   displayed text, finish reason, and cancellation. Base versus instruction
   checkpoint identity is part of the operation, not inferred from its name.
2. **`log-likelihood(prompt_tokens, continuation_tokens) -> token_scores`.** Add a
   bounded owner-thread teacher-forced request. At each position, read raw logits,
   normalize over the full vocabulary in stable arithmetic, record the supplied
   token's log probability, and advance using that supplied token. No grammar,
   temperature, penalties, or sampling. Return per-token values, their f64 sum,
   exact token IDs, model identity, and the explicit inclusion/exclusion of a
   terminal token. Reuse decode/context ownership; do not expose mutable contexts
   to the document layer. A bounded requested-token next-step query is a smaller
   first slice if only classification labels are needed.
3. **`choose(candidates, scores, rule) -> selection`.** Distinguish maximum-score
   selection from categorical sampling. Over a fixed candidate list, softmax of
   `alpha * score` defines an exact finite categorical distribution, within
   numerical precision. Call it selection over this list, not sampling from the
   complete language-model sequence distribution.

Text wrappers must specify tokenizer boundaries. Independently tokenizing a
prefix and suffix need not equal tokenizing their concatenation. A score for one
tokenization is not the probability of an arbitrary Unicode string summed over
all tokenizations. Label comparisons must also record whitespace, label spelling,
and any termination/delimiter policy. Normalizing scores across supplied labels
means conditional preference **among those labels**, not universal confidence.

## Unsupervised elicitation: what would count

Wen et al.'s Internal Coherence Maximization (ICM) searches for labels that combine
logical consistency with mutual predictability. The latter conditions each label
on other labeled examples. Their full elicitation pipeline includes training on
the resulting labels; merely prompting a frozen model is not reproduction of
their trained-assistant result. Salience and long-context limitations matter:
coherence does not discover an arbitrary user's preferences. [Paper](https://arxiv.org/abs/2506.10139),
[authors' explanation](https://alignment.anthropic.com/2025/unsupervised-elicitation/).

The official implementation asks a base-model API for one generated token and
top-20 logprobs, with prefix caching recommended. It builds leave-one-out
demonstrations and searches label assignments with consistency repair and an
annealing schedule. Its practical score uses signed true/false log odds and an
inconsistency penalty; its `alpha` is a score coefficient, **not** the sequence
power exponent. [Repository](https://github.com/Jiaxin-Wen/Unsupervised-Elicitation),
[search implementation](https://github.com/Jiaxin-Wen/Unsupervised-Elicitation/blob/master/src/experiments/ICM.py).

The label extractor adds an epsilon to masses found in top-k and returns zero
when neither label is found. That is a specific approximation, not an exact
arbitrary-label scorer; do not copy it into a universal scoring API.
[Extractor](https://github.com/Jiaxin-Wen/Unsupervised-Elicitation/blob/master/src/model_querying/solution_extraction.py).

A small honest experiment here is an **ICM-inspired label search** over an explicit,
bounded set of short documents: named labels, frozen prompt examples, a supplied
consistency predicate, exact label scores, and an iteration/token budget. Export
assignments and objective history. Do not advertise unsupervised fine-tuning,
truth discovery, or preference alignment without the corresponding work and
evaluation. The score is an objective on assignments, not a normalized joint
probability over datasets.

## Sequence power: target first, algorithm second

For a precisely defined finite or terminated outcome space, define

\[
\ell(y\mid x)=\sum_t\log p(y_t\mid x,y_{<t}),\qquad
\pi_\alpha(y\mid x)\propto\exp(\alpha\ell(y\mid x)).
\]

Token temperature instead samples
`q_t(v) = p_t(v)^alpha / sum_u p_t(u)^alpha`. Its joint probability contains a
product of prefix-dependent normalizers, so it generally differs from
`pi_alpha`. Karan and Du use autoregressive block growth and Metropolis-Hastings
suffix proposals. Their reported gains are task/model experiments, not a promise
for Gemma writing. Finite chains approximate the target; mixing can be poor.
[Paper](https://arxiv.org/abs/2510.14901),
[official code](https://github.com/aakaran/reasoning-with-sampling).

Practical options, ordered by integration cost:

| Operation | Honest interpretation | Needed beyond current ordinary generation |
| --- | --- | --- |
| Token power / temperature | Locally sharpened proposal | Existing controls; avoid applying exponent and inverse temperature accidentally twice. |
| Finite candidate rescoring | Best-of-N or categorical choice within the realized list | Teacher-forced scores, or complete generated-outcome evidence if candidates share the exact scored prompt. |
| Importance resampling of independent proposals | Finite-particle approximation to a stated sequence target | Full `log p(y)` and actual `log q(y)`, including termination. Log weight is `alpha * log p - log q`, **not** just `alpha * log p`. For `q=p`, it is `(alpha-1) * log p`. Record ESS and candidate count; finite self-normalized estimates are biased. |
| Independence MH | Simple reference chain with a known stationary target | Same complete scores and support coverage. Accept using the change in `alpha*log p-log q`; with `q=p`, use `(alpha-1)*(log p(new)-log p(old))`. Record all transitions and finite budget; no claim of convergence. |
| Suffix MH | Reuses prefixes and makes smaller proposals | Teacher-forced forward/reverse proposal likelihoods, correct cut-point probabilities and length policy; cache reuse is an optimization. |
| Power-SMC | Weighted particles with sequential correction | Per-step raw/proposal log probabilities, EOS handling, synchronized particle advancement, cache ancestry/reindexing, ESS-triggered resampling. A new scheduler integration, not a sampler flag. |

Power-SMC uses incremental log weights `alpha*log p_t-log q_t`; its low-temperature
proposal reduces local weight variance. Resampling still produces a finite
particle approximation. The paper reports lower latency than MH on its tested
hardware; those multipliers do not establish local Metal performance.
[Power-SMC](https://arxiv.org/abs/2602.10273).

Scalable Power Sampling is another practical research direction: Monte Carlo
lookahead estimates future-dependent corrections, with jackknife bias reduction.
It is not ordinary low-temperature sampling and does not remove all finite-budget
approximation. It requires branching future continuations and more inference
orchestration than the initial two primitives.
[Ji et al.](https://arxiv.org/abs/2601.21590).

For unrestricted base-model targets, proposals must cover their support: top-k,
top-p, grammar restrictions, and greedy selection can exclude positive-mass
outcomes. Either remove those restrictions or explicitly define a restricted
target. Specify whether the outcome is a fixed-length prefix, an EOS-terminated
sequence, or a capped stopped process. A cap is not EOS, and omitted stop text is
not permission to omit its sampled token probabilities. Arbitrary length
normalization changes the target and must be named separately.

Likelihood favors what the model expects, not necessarily useful, correct, novel,
or preferred writing. Recent work reports that global sharpening can harm
downstream aggregation despite increasing correct trajectory mass, reinforcing
the need for same-budget comparisons and retained candidate diversity.
[Yang et al.](https://arxiv.org/abs/2608.14420).

## Integration order and evidence

Expose bounded raw completion first. Add raw teacher-forced scoring and complete
terminal-token observations before claiming sequence likelihood or sequence power.
Use finite candidate selection as the first executable composition; build bounded
ICM-inspired searches and an independence-MH reference only when their required
scores exist. Consider SMC after the simpler version demonstrates value at an
acceptable wall-clock cost.

Reuse the native host, revocable handles, cancellation, model identities, and
document evidence rather than introducing a second process or API service.
Keep run outputs separate until explicit acceptance. Verification should cover
one hand-computable finite distribution distinguishing temperature from sequence
power, token-alignment and EOS accounting, and a real-model smoke test that scores
known continuations. Then compare useful outputs at the same token and time
budget; avoid making a large benchmark suite a prerequisite for the primitives.
