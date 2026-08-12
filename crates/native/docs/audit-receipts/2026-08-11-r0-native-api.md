# R0 native API and token-piece trace

## Scope

- Repository: `delysis/llama-native-kit`
- Audited base: `2d69f086e922ed7bdfd6236baf5a1ad0ed568360`
- Local branch: `codex/phase1-r0-native-api`
- Binding revision: `a3cf95eb1d4fa748480eb780e6fcbfc1a5c1c391`
- Status: steward candidate; downstream repins and remote CI are not claimed here

## Contract implemented

- `GenerationTicket` and `EmbeddingTicket` nonblocking waits now consume the
  ticket and return either `TryWaitOutcome::Pending(ticket)` or
  `TryWaitOutcome::Ready(output)`.
- Generic `NativeModelHandle::cancel(request_id, None)` now cooperatively
  cancels an active embedding request. A branch-qualified embedding cancel is
  rejected with a zero count because embeddings have no branch namespace.
- Strict generation captures raw token-piece bytes at the sampled-token decode
  site before UTF-8 display projection.
- The display decoder reserves from `encoding_rs`'s state-aware upper bound,
  checks consumed input and `CoderResult`, retries `OutputFull`, and finalizes
  before terminal events. Split UTF-8 scalars therefore reach display, event,
  and seal validation without dropped input.
- Raw trace allocation and piece copying are enabled only for a strict batch
  that retained authority evidence; ordinary and controlled-baseline
  generation do not pay that production memory cost.
- Each verified output retains one move-only `TokenPieceTrace` with a raw byte
  buffer and cumulative boundaries. The boundary vector begins at zero, is
  nondecreasing, contains one more entry than the generated-token vector, and
  ends at the raw byte length. Equal boundaries preserve zero-byte pieces.
- Seal minting checks every captured slice against the live model's sampled
  token ID and checks the complete byte stream against the retained display and
  event projections. The trace has no public constructor, `Clone`, or Serde
  implementation.
- Executor-owned request leases, strict artifact checks, and conservative
  non-Unix authority behavior are unchanged.

## Deterministic evidence

```text
cargo fmt --all -- --check
  PASS

cargo test --workspace
  PASS: 178 unit/integration tests
  PASS: 18 compile-fail doctests
  IGNORED: 8 explicitly hardware-dependent tests

cargo clippy --workspace --all-targets -- -D warnings
  PASS

./scripts/check-architecture.sh
  PASS: in-process native runtime boundary preserved

Focused R0 tests
  PASS: consuming_try_wait_pending_returns_ticket
  PASS: consuming_try_wait_ready_returns_output
  PASS: generic_cancel_cancels_embedding
  PASS: token_piece_boundaries_match_token_count
  PASS: zero_byte_piece_is_representable
  PASS: invalid_utf8_piece_is_preserved
  PASS: split_utf8_projection_reaches_verified_seal_validation
  PASS: tampered_trace_cannot_verify
```

An independent steward review rejected the first candidate because its display
decoder ignored partial consumption when an output buffer filled, and because
all generation modes copied traces they could not use. This final candidate
contains both corrections and the projection-to-seal regression above.

## Real-model evidence

Artifact:

```text
path: /Users/george/Documents/llama-native-kit/target/test-models/Qwen_Qwen3-0.6B-Q4_K_M.gguf
bytes: 484220320
sha256: 9acfc1e001311f34b4252001b626f2e466d592a42065f66571bff3790d4e1b14
host: Apple M4 Max
observed accelerator: Metal
```

The complete ignored real-engine selection passed 5/5 on the corrected final
candidate:

```text
controlled_runtime::tests::real_small_gguf_controlled_generation_proves_baseline_cfg_constraints_and_samplers
tests::real_completion_text_tokens_batch_and_capabilities
tests::real_in_process_prompt_smoke
tests::real_per_token_embeddings_preserve_generation_context
tests::real_strict_batch_retains_a_pre_cancelled_case_under_exact_model_binding
```

The strict batch test was extended to read the live seal and prove for both a
completed case and a pre-cancelled case that boundary count matches generated
token count, the terminal boundary equals raw byte length, and the raw byte
projection equals the verified output text. That exact test was rerun after the
assertions were added and passed 1/1. The strict test intentionally requests
CPU; other real tests in the 5/5 selection observed the M4 Max Metal backend.

## Evidence limits

- No GitHub branch, commit, pull request, or remote workflow was changed by
  this receipt.
- Windows CI has not yet observed this successor, so existing conservative
  Windows strict-authority language remains in force.
- FTE, Mom, and Loom compatibility requires successor pinning and adaptation
  of consumers of the now-consuming polling API. Those are separate promotion
  gates, not implied by this native-only receipt.
