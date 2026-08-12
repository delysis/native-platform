# W1 current-code Gemma vertical

Date: 2026-08-12

## Exact runtime acceptance

- Source baseline: `949c0006b57f416190e2ae8ab84dc3a944d6b4d1`
- Model: `gemma-4-E2B-base-Q8_0.gguf`
- Bytes: `4954576032`
- SHA-256: `aa0a9a03993440f45176f19f8189a2e84c210ff8628ec13dc6edf42d017f7670`
- Production path: `LlamaBackend::start_exact_continuation`
- Prompt: `Mara pressed her palm to the cold brass, and`
- Cases: seeds 41 and 42, one exact shared prompt, raw completion mode
- Result: both cases completed with non-empty generated token IDs, positive shared-prefix metrics, distinct branch identities, and at least four lexical output tokens
- Capability result: architecture `gemma4`, backend `cpu`, chat unsupported, completion supported
- Fail-closed probe: a fill-in-middle request through the raw-continuation entry point was rejected with `raw continuation requires PromptMode::Completion`
- Teardown: the loaded model slot was released and the one production runtime worker was joined

The exact replay passed with:

```text
LOOM_GEMMA4_E2B_BASE_PATH=/Users/george/.cache/fiction-harness/models/gemma-4-E2B-base-Q8_0.gguf \
  cargo test --locked -p loom-backend-llama \
  --features unstable-w1-vertical-tests \
  --test w1_current_exact_gemma \
  w1_current_exact_gemma_baseline -- --ignored --exact
```

The authenticated portable test, strict test-target clippy, fixture SHA-256 ledger, workspace formatting, W1 pin policy, and diff checks also passed locally.

## Linux executable launch race

Loom's post-merge Linux CI exposed a transient `ETXTBSY` while launching a newly copied fake Codex CLI. Command spawning now retries only Linux raw OS error 26, at most eight times with a 5 ms delay. All other spawn errors remain immediate failures. A Linux-only regression holds a test executable open for writing, releases it after 10 ms, verifies successful bounded recovery, and separately verifies that a missing executable still fails.

The first W1 pull-request run also exposed an ownership race: a fast desktop worker could publish terminal state before its worker reservation was attached. Production generation and the deliberately panicking lifecycle adapter now gate worker execution until attachment succeeds. An attachment failure cancels the native owner, opens the gate, joins the returned desktop worker, and joins the native backend before returning the failure. The focused production-order and contract lifecycle regressions each passed ten consecutive local runs; remote Linux remains the required cross-platform gate.

## Boundaries

The exact Gemma replay does not establish desktop rendering, suggestion promotion, persistence, relaunch behavior, Metal acceleration or performance, or behavior on other hardware and operating systems. Those claims remain explicitly omitted. CI authenticates the checked-in fixture and source binding but cannot replay the external 4.95 GB model artifact.
