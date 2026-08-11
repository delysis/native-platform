# R0 downstream consumer compatibility

Date: 2026-08-11  
Native implementation: `f7a69316c64d857b99bd847dd44cd852fc5b4ca4`  
Audited native base: `2d69f086e922ed7bdfd6236baf5a1ad0ed568360`

## Accepted local claim

The exact R0 native API implementation compiles and executes through the
current local FTE, Mom, and Loom remediation candidates with one native-kit
source identity per consumer. This closes the local downstream-adaptation gate.
It does not claim that the unadvertised native or FTE commits can be resolved
by a clean remote runner, and it does not claim candidate remote CI.

## FTE

- Package revision: `1b4cc9c830cf5593e73b3ca9349ce9ac77d7bf5a`
- Evidence head: `a08ca359f5e1e053f027b0f58a680059639ffe56`
- Lock SHA-256:
  `44cbb25b1e027d4b353043c6b2c9e906e76dcb57842f7be130b7853131df090c`
- All four native packages pin exact R0; no audited-base native source remains.
- Rust 1.88 workspace: 102 passed, two declared real-GGUF ignores.
- Rust 1.88 full clippy, frontend, pin, boundary, workflow, formatting, and
  diff gates passed.
- Real Qwen adapter executed in process on CPU with stable-prefix reuse.
- The shared desktop Gateway route executed the same model on Apple M4 Max
  Metal and passed drain, native join, and `ggml_metal_free` deallocation in
  37.19 seconds.
- Exact packaging-only bundle:
  `com.delysis.fte.r7.1b4cc9c.acceptance`, executable SHA-256
  `dab8089ed3420886005b17ea47cdbf9c99ef2daac6a7deaeb1feec0344704dfd`.

## Mom

- Package revision: `4084d1120f63f7bf561e7381da7daec947cbcd44`
- Evidence head: `201c5142deea25b25e581920dfc4aa063d688322`
- FTE dependency: `1b4cc9c830cf5593e73b3ca9349ce9ac77d7bf5a`
- Lock SHA-256:
  `e2f1a4bf856e1c10e704025cc93b88495f4332f379d64ae012b482d9ad3871fd`
- All four direct native packages and every resolved native package use exact
  R0; no audited-base native source remains.
- Mom's sole polling consumer transfers ticket ownership to the consuming
  `try_wait` API and retains `TryWaitOutcome::Pending(ticket)` without changing
  supervised cancellation, event drain, timeout, or terminal handling.
- Default and Rust 1.88 gates passed: 13 CLI tests plus one declared ignore,
  129 runtime tests plus 13 declared ignores, 35 app tests, full clippy,
  architecture/contracts/UX checks, formatting, metadata, and diff checks.
- Real Qwen base completion passed on CPU, proved the real engine ran, and
  invoked no fixture.
- Exact packaging-only bundle:
  `com.delysis.mom-llama.r1.4084d11.acceptance`, executable SHA-256
  `7b010c7c4a76ae6fac66c2fce67100d7dca399f954f50f66e69a46a0ff11c157`.

## Loom

- Combined R5 implementation: `e78111cbc754fa285153623ab8791e3923494460`
- Evidence head: `a5b28840280eab05ba16cd4e36ff9119f794fd55`
- Lock SHA-256:
  `1ac941227854c2835e45aaa66c3e1a30d0cae11dcf670f3e484374498565b524`
- Every direct and resolved native package uses exact R0; no audited-base
  native source remains.
- Combined workspace gates passed: 630 Rust tests with eight declared
  environment-dependent ignores, 177 frontend tests, Svelte check, frontend
  production build, full clippy, Rust 1.88 check, workflow policy, formatting,
  and diff checks.
- R4's exact R0-backed real-Gemma writing bundle passed token-piece evidence,
  caret-local completion, literal-tab behavior, promotion, quit, and relaunch.
  The combined R5 research-promotion launched check remains a separate R5/R8
  gate and is not implied by this compatibility receipt.

## Remaining promotion boundary

The native implementation and FTE package revision are not advertised by their
GitHub remotes. Remote-only dependency resolution, candidate CI on required
platforms, review, merge, and phase-one tags remain open. Publication must be
ordered native R0 first, then FTE, then Mom; Loom also requires the published
native revision. No moving branch reference may replace these immutable pins.
