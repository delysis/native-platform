# R4 exact token-piece evidence receipt

Date: 2026-08-11
Task: `R4-LOOM-BYTES`
Evidence tier: verified live local inference plus persisted diagnostic replay; not cross-platform certification

## Immutable inputs

- Loom base: `79fb322c8c950cea8cc0659019cae660270369c8`
- Native successor: `f7a69316c64d857b99bd847dd44cd852fc5b4ca4`
- Candidate code-diff SHA-256 before this receipt: `88bfb383e9c67c5976c766f5fe060c4db29081a6966d4d2ebff6bcc043b86251`
- Qwen artifact: 484,220,320 bytes, SHA-256 `9acfc1e001311f34b4252001b626f2e466d592a42065f66571bff3790d4e1b14`
- Gemma artifact: 4,954,576,032 bytes, SHA-256 `aa0a9a03993440f45176f19f8189a2e84c210ff8628ec13dc6edf42d017f7670`
- Observed host for real-model tests: Apple M4 Max

The native successor was resolved through a local Git URL rewrite because the
commit was not published at verification time. The checked-in dependency still
names the canonical GitHub repository and exact revision. R4 is therefore not
remotely buildable or promotable until R0 is published.

## Claim and threat model

For ordinary verified native base-writer calls, Loom now persists the exact raw
bytes emitted for every sampled token together with the cumulative boundary
vector. Migration 10 stores both values as immutable content-addressed blobs.
The trace is committed by a versioned call fingerprint, replay validates its
shape and terminal boundary, and `exact_token_span` slices those recorded bytes
without decoding or retokenizing them.

The adversary considered here may alter stored rows or blobs, substitute a new
tokenization with the same displayed text, split a UTF-8 scalar across pieces,
emit an empty piece, or preserve stop/endpoint bytes that are absent from the
displayed projection. Those substitutions fail replay unless the exact v2
commitment still matches.

Historical schema-9 receipts contain no trace. Their byte-identical v1 call
fingerprints remain replayable, but replay does not invent boundaries and those
calls cannot answer `exact_token_span`. Controlled-generation seals in the
pinned native API also contain no token-piece trace; Loom rejects injected trace
material on that path and makes no byte-exact controlled-call claim.

## Verification

- `cargo test --workspace --all-targets`: passed after the final fixes. The
  store suite included 146 passing unit tests; known external/live tests remained
  ignored unless separately invoked.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: passed.
- Frontend: 29 Vitest files / 173 tests passed; Svelte check reported zero
  errors and warnings; Vite production build passed with its existing chunk-size advisory.
- `cargo fmt --all -- --check` and `git diff --check`: passed.
- Exact schema-9 v1 receipt replay after migration 10 and reopen: passed.
- Exact trace store/reopen regression: passed for a split multibyte scalar,
  invalid UTF-8 piece bytes, a zero-byte piece, endpoint/stop bytes, completed
  and cancelled terminals, exact span slicing, and persisted retokenization tampering.
- Real Qwen native-writer bridge: passed with live in-process inference and
  exact evidence admission.
- Real Gemma 4 E2B base Q8 raw family: passed with two branches, generated
  tokens, shared-prefix evidence, and the corrected buffered-event drain.
- Independent steward review initially blocked three defects: legacy v1 digest
  drift, event receiver loss after move-only `Ready`, and permissive controlled
  trace pairing. All were fixed and the candidate was then approved.

## Omitted claims and remaining gate

This receipt does not claim that displayed UTF-8 equals raw token-piece bytes,
that old or controlled calls have exact boundaries, that replay reconstructs
live authority, or that Windows and other hardware backends are certified. The
real Gemma backend test is not a launched desktop acceptance. The separately
identified corrected bundle completed literal-tab persistence, visible caret
suggestion, promotion, loaded-model quit, and immediate-relaunch checks recorded
in [2026-08-11-r4-quiet-editor-ux.md](2026-08-11-r4-quiet-editor-ux.md). That is
one designated-machine product slice, not portable certification.
