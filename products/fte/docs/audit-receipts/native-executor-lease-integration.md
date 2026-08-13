# Native executor-lease integration receipt

## Identity

- FTE base: `cf9b7ea2f8d381601ba674179d26d4d56e80f3df`
- native-kit: `4dd744209ff85886be9dce7df46cd65eaa19c804`
- llama-cpp-rs: `152dabbd3492d8e35fdf7112e556685c6c75ec9a`

The four direct native dependencies move atomically. Cargo resolved one native
revision and one transitive binding revision; the checked-in pin gate rejects
the superseded native and binding checkpoints.

## Boundary changes

- `DuplicateActiveRequest` maps to a non-retryable 409 conflict with sanitized
  detail; native diagnostic text is not exposed.
- The gateway now admits only one active public `RequestId`. Internal route
  fallback remains sequential: a failed pre-ticket attempt drops its admission
  lease before the next route is tried.
- `GatewayTicket::with_admission_lease` remains unchanged. Its relay owns the
  gateway lease until the backend final resolves, independent of consumer
  ticket lifetime. A deterministic barrier test drops the consumer ticket,
  starts shutdown, and proves shutdown remains pending until the backend sends
  its authoritative final result.
- FTE local work already waits for the native ticket on its tracked blocking
  operation. It does not use bounded native waiter methods and therefore
  requires no lossy timeout adaptation.
- Borrowed native shutdown remains non-owning: FTE drains its operations but
  never invokes final host shutdown.

## Red evidence

The first all-target check against the new native revision failed exhaustively
at both native error mappings because `DuplicateActiveRequest` was not handled.
After adding the explicit mapping and regression, the workspace compiled.

The earlier published FTE follow-up `70f0990` moved the dependency backwards
from controlled-generation `6a82439` to shutdown-only `b71dfaa`. This successor
branches from canonical `cf9b7ea` and does not include that regressive pin
commit.

## Local acceptance

Rust test and lint commands used Rust 1.88.0 and the checked-in lockfile.

- Workspace all-feature/all-target tests: 124 passed; the one real-GGUF test
  remained intentionally ignored in the deterministic run.
- Workspace all-feature/all-target Clippy with `-D warnings`: passed. Two
  inherited provider `format!` calls were modernized rather than weakening the
  lint boundary.
- Frontend syntax and test: passed (1 test).
- `npm audit --audit-level=moderate`: no vulnerabilities.
- Native pin, module-boundary, workflow-policy, formatting, and diff checks:
  passed.
- Release Tauri build: produced the macOS app and DMG.

The ignored integration was then run explicitly against
`Qwen_Qwen3-0.6B-Q4_K_M.gguf` (484,220,320 bytes, SHA-256
`9acfc1e001311f34b4252001b626f2e466d592a42065f66571bff3790d4e1b14`).
It passed cold and warm stable-prefix chat, raw completion, single-resident
reuse, FTE-owned drain without host shutdown, and application-owned final
worker join. The model was configured for CPU execution; llama.cpp initialized
the Metal backend on the Apple M4 Max, but this receipt does not claim Metal
inference for the FTE test.
