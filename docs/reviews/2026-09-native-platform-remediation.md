# September 2026 audit remediation

Baseline: PR #42 merged at `924ac64ef0a7512327406ec6490c5e017b5112a8`.
This review refreshes the prior audit against that source and adds two speech
lifecycle races found during independent review.

## Result

- Hosted provider redirects are refused; unknown quota/usage stays unknown.
- Mom retains conversation ownership across asynchronous dispatch and validates
  store identity before mutation. First creation publishes a complete store
  atomically without replacing a racing creator's file.
- Native state restore requires worker-local export provenance for opaque KV;
  other snapshots use checked token replay. Full preflight precedes mutation,
  and mutating failure invalidates cache authority. Memory estimates include
  requested context, KV geometry and scratch with explicit heuristic fallback.
- Terminal publication precedes identity release in Mom, Speech and FTE.
  Loom completed-worker diagnostics are bounded; live leases retain their state.
- FTE desktop and authenticated loopback share terminal activity recording.
  Playground Stop survives admission races and failed workers release their slot.
  SQLite v1 upgrades preserve nullable usage and tolerate a completed racing upgrade.
- Speech cancellation survives first admission; project close revokes pending
  microphone starts before draining capture.
- Full CI runs Loom's WebKit browser suite. Setup-only Information work is
  removed. FTE inherits workspace lints. Current documentation has an explicit
  architectural supersession map; fifteen sealed ADRs remain unchanged.

## Local verification

The combined implementation at `848aaaf` and the source-identical merge
`d5ba835` passed on macOS with pinned Rust 1.92:

| Check | Result |
| --- | --- |
| cargo test --offline --locked --workspace --all-targets | 1,602 passed, 0 failed, 43 ignored; 63 suites |
| cargo clippy --offline --locked --workspace --all-targets --all-features -- -D warnings | Passed |
| FTE npm test and module-boundary check | Passed |
| Combined CI, Mom and FTE Node tests | 157 passed |
| Loom WebKit browser tests | 74 passed in five files |
| Ignored-test registry | 43 source entries match registry |
| Explicit real-GGUF native restore/replay controls | Passed |

The real-model checks used Qwen3 0.6B Q4_K_M, SHA256
`9acfc1e001311f34b4252001b626f2e466d592a42065f66571bff3790d4e1b14`,
with CPU inference. They cover same-worker restore, durable replay, altered
payload, preflight rejection and stale authority after mutating failure.
Negative-control regressions were run against the unfixed behavior.

## Acceptance limits

Durable snapshots can replay tokens instead of accelerating through opaque KV.
Memory estimates are not hard RSS or Metal allocation limits. Historic v1 token
zeros cannot retrospectively distinguish unknown usage. These local checks do
not certify physical microphones, live hosted credentials, signed bundles, or
cross-platform runtime behavior. Remote CI and promotion are separate evidence.
