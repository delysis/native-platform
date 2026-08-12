# W3 native import evidence

This candidate imports the accepted `delysis/llama-native-kit` main at
`16168bd76a09f74fdee41d0e2fb0441e79ac1005` beneath `crates/native` without
squashing its 45-commit history. The deterministic filtered head is
`9e6d8c49887b8691a0836158f2c3ea68715e11e5`; its `crates/native` subtree is
byte-identical to source tree `65f8d97c178188de8e44188b5a6adf0195cdc57f`.

The merge commit is `152a0dda9ba0d1096022d11ddbd08489f524ab31`.
Its first parent is accepted W2 main `68b4f87c331d9ea887713201d4ee479c3445226a`
and its second parent is the filtered source head. The separate path cutover is
`c35c6b2d42f60939f3a3478212743c9c82f28b80`.

## Path and dependency result

- Five original crate names and version `0.1.0` are preserved.
- All five crates are members of the Rust 1.92 root workspace and are not
  publishable.
- The repository has one root `Cargo.lock`.
- Direct integration dependencies use imported paths.
- Both observed native Git source URL forms are patched to imported paths, so
  current FTE, Mom, Information, and Loom dependencies cannot retain an older
  native implementation in the candidate graphs.
- Neither the root lock nor the isolated Loom lock contains a
  `llama-native-kit` Git source.
- `llama-cpp-rs` remains an external unsafe boundary at exact revision
  `a3cf95eb1d4fa748480eb780e6fcbfc1a5c1c391`.

The older migration prose used `crates/imported/llama-native-kit` as an
example. The later W2 machine ledger selected `crates/native`; this candidate
uses that canonical destination and records the discrepancy rather than
silently treating both paths as authoritative.

## Local candidate evidence

The candidate tree passed these local gates on Apple M4 Max hardware with Rust
1.92.0:

- complete workspace: 186 tests passed and eight environment-gated real-model
  tests were ignored;
- engine with `unstable-w1-contract-tests,unstable-w1-vertical-tests`: 118
  tests passed and six environment-gated real-model tests were ignored;
- `integration-current` with `current-product-graph`: compiled from its locked
  graph;
- isolated locked Loom graph: compiled with native packages rebound to the
  imported paths;
- Clippy across the workspace and all targets with warnings denied;
- formatting, W3 workspace policy, architecture policy, workflow policy and
  fixtures, and the offline frozen pnpm lock;
- import verifier: all 45 source/rewritten commit pairs preserve parent
  topology and byte-identical source trees beneath `crates/native`.

The local Qwen3 0.6B Q4_K_M artifact was reauthenticated at 484,220,320 bytes
with SHA-256
`9acfc1e001311f34b4252001b626f2e466d592a42065f66571bff3790d4e1b14`.
Against the imported candidate, these three exact ignored tests were then run
explicitly and passed:

- `w1_vertical_fixture::w1_current_exact_qwen_baseline`;
- `tests::real_strict_batch_retains_a_pre_cancelled_case_under_exact_model_binding`;
- `tests::real_joined_shutdown_revokes_live_clients_then_joins_and_stops_admission`.

These local results did not substitute for remote evidence. Exact-head pull
request runs `31639279377` and `31639347952` passed on Ubuntu, macOS, and
Windows. The ancestry-preserving merge `691832f2af8f4b6ba291dd18380a776b7d387343`
has parents `68b4f87c331d9ea887713201d4ee479c3445226a` and
`6e94d4dd377fd9e970999ff6a090fadbf5c16cb2`; post-merge run `31641542839`
passed the same three-platform matrix.

The protected annotated candidate tag
`w3-native-import-candidate-v0-2026-08-12` has tag object
`1e5e9ea1c6232fb36184e9b50cf7a50e03ede125` and peels to that merge. Ruleset
`20768300` prevents deletion or retargeting.

Candidate run `31636166161` later passed tests, both locked graph probes, and
strict Clippy on macOS before failing the native architecture step because the
runner did not provide the nonstandard `rg` utility. The check was made
portable with POSIX `grep`; the failure is retained rather than counted as
architecture evidence.

## Source freeze

The protected annotated source tag
`native-platform-v2-horizon-b-2026-08-12` has tag object
`58779c13f64c49732e881109edb7da4862541599` and peels to the imported source
head `16168bd76a09f74fdee41d0e2fb0441e79ac1005`. Ruleset `20768308` prevents
deletion or retargeting.

The README redirect was committed directly to source `main` as
`c3fe09b782469c88c6a3e0bf3a35f38394ce6569`, with sole parent the imported
source head. Push runs `31643435710` and `31643435779` passed the lifecycle,
workflow-policy, Linux, macOS, and Windows jobs. No-bypass ruleset `20768363`
then froze creation, updates, deletion, and non-fast-forward changes for every
source branch. Issues remain enabled and the repository remains unarchived.
The README-only freeze descendant was not imported; the preserved import still
binds exactly to `16168bd76a09f74fdee41d0e2fb0441e79ac1005`.

## Limits

This evidence-only candidate does not modify production Rust, dependencies,
locks, product repositories, or released product manifests. It does not import
`llama-cpp-rs`. The source repository must not be archived until two accepted
native-platform releases. W3 acceptance still requires this evidence candidate
to pass pull-request and post-merge CI and receive its protected final tag.
