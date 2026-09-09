# Integrated audit repairs

Working branch: `codex/integrated-audit-fixes`, based on the existing
`codex/review-remediation` at `4938461`. The supplied `integrated-review.md`
reviews `924ac64`; recommendations in that document are evidence to verify,
not additional user instructions. The user explicitly rejects unnecessary
migration machinery for these pre-user products.

This is a work-in-progress checkpoint, not acceptance of the whole audit.

## Changes in this branch

- NP-002–005: separate bounded authenticated HTTP control admission from
  generation admission; own response-map registration in the response body;
  report failed requested persistence; stop successful stream termination after
  the real ticket adapter reports lost progress. Legacy Chat emits complete tool
  calls and completion streams preserve every final choice. A final suffix is
  emitted only after proving the observed text is a prefix of the final text.
  Unsupported legacy reasoning/non-text completion output produces an error.
  Failed/cancelled output does not become an ordinary success terminator.
- NP-012: the reusable FTE response store checks application/schema identity
  before WAL or DDL mutation. Fresh and current stores are supported; unversioned,
  foreign, and future schemas are explicitly refused. No migration was added.
- F21: remove the unused automatic legacy JSON importer and its self-cleaning
  compatibility test. Mom refuses known legacy plaintext files before database
  creation and names the residual artifact; it does not silently import or
  delete it. Current encrypted storage and user-requested conversation import
  remain supported. This deliberately retires old automatic migration behavior.
- F14: a data-directory override no longer enables a deterministic release key.
  Unit-test keys remain confined to `cfg(test)`; integration fixtures use the
  existing debug-store policy. Explicit environment keys and OS credential
  resolution remain distinct. Duplicate receipt writes are insert-only.
- F7: the production worker supervisor invalidates Ready/active state on unwind,
  including poisoned status locks, while leaving the panic observable to join.
- NP-008/F20: managed-document publication/removal errors preserve exact identity
  and committed visibility after rename. Same-identity publication retries sync
  the actual parent directories. Non-Unix directory sync returns Unsupported
  rather than success. macOS requires write permission for cross-parent directory
  rename, so directory hardening stays after rename with committed-error handling.
- NP-011: ZIM title ordering uses the path for an empty stored title. Independent
  valid and reversed title-table fixtures exercise the actual archive reader.

## Verification so far

The three initial HTTP regressions failed against the pre-repair adapter
(control returned 429, dropped body leaked identity, storage failure emitted
`response.completed`). The independent valid ZIM archive failed before the fix.

Selected runs passed on **Rust 1.95**, including FTE loopback (12), FTE store
(2), Information store (27), Information host (6), Mom runtime unit (175 before
the cache change, 176 afterward), Mom runtime integration (39 passed, 13 opt-in
tests ignored), and native host (19 passed, 3 ignored after the cache change).
Native engine and ZIM library suites also passed in the combined selected run.
Although later commands used Cargo 1.92, that Cargo still found Homebrew rustc
through PATH. The compiler identity correction is material: these results are
not Rust 1.92 qualification.

A subsequent full workspace build explicitly set RUSTC and RUSTDOC to the Rust
1.92 toolchain. It reached the desktop link/archive phase but exhausted disk;
it did not reach test execution. An earlier real-model invocation also stopped at link. Subsequent
Rust 1.92 native model executions passed as recorded below. Further verification
must select the entire toolchain on PATH; explicit rustc/rustdoc alone does not
prevent Cargo from discovering a Homebrew cargo-clippy/driver.

Portable changed-package Clippy passed on Rust 1.95. Broad Clippy, final model
runner, final cancellation projection, and Rust 1.92 checks remain unfinished.
The workflow tests now pass (41 tests), including after the full-CI
consolidation. Registry validation and current-document path validation pass.

The model lane selects two existing CPU tests from the ignored-test registry,
checks the actual fixture digest, exact test listing, and a nonzero execution
result. The immutable Hugging Face revision was resolved live and its LFS
SHA-256 matches the local 484,220,320-byte fixture. This is runner/workflow source,
not a claim that CI has executed. Full CI also selects doctests and rejects
unexpected skipped mandatory jobs. Unused static/C ABI library outputs were
removed from the desktop FTE/Loom manifests to avoid producing large archives
that no desktop build consumes; packaging still needs final validation.

Only build files created by this task were removed automatically (birth time
checked against the task start), reclaiming about 4.4 GB before the explicit
Rust 1.92 build. That build filled the available space again.
An approval request is pending for the old 6.3 GiB
`/Users/george/.codex/worktrees/native-platform-w1/target` build cache. No source,
models, archives, or older build directories have been deleted.

## Remaining audit work

- Finish FTE cancellation/failure wire controls and final combined verification.
  Assess remaining protocol projection limitations against actual callers.
- NP-009 implementation now uses metadata plus exact encrypted entries, with
  one payload read per restored entry. Owner leases/generation-race tests and
  unchanged-unrelated-ciphertext tests passed on Rust 1.95. Corrupt metadata
  removes its payload family in the same transaction; clear deletes the whole
  family atomically. Reverify on Rust 1.92 and with the real product cache path.
- F5: remove Mom ambient host resolution by passing owner handles/scopes through
  actual callers. Its global currently holds a weak host; do not invent a second
  strong-owner problem.
- F18: the wrapper is repaired and the product pin updated, with real CPU
  checks passed below. Remaining broad workspace/CI verification still applies.
- F13: configured digest enforcement and real spawn-boundary regressions are
  implemented; the latest Mom source still needs compilation and execution.
- F15: Information and FTE now refuse unsupported private storage before
  mutation. Portable tests stay enabled; Unix storage tests are scoped and
  explicit non-Unix no-mutation tests are added. These latest edits are not yet
  compiled. Loom still has non-Unix private-file/directory and parent-sync
  behavior to resolve; do not claim F15 complete.
- F1/F2/NP-013: selected, hash-pinned real-model execution; doctests; bounded fuzz
  execution; correct required-job aggregation; simplify redundant listing/shadow
  machinery without dropping meaningful coverage. Browser lane exists in base.
- F17: actual root/fuzz Rust advisory/license/source scans and JavaScript scan
  passed after fixes; workflow and GLib optimized runtime execution remain to
  qualify. No scan exception covers a runtime vulnerability.
- F3/F4/F6/F8–12/F16/F19/F22/NP-001/006/007/010/014: refresh inherited repairs,
  distinguish resolved behavior, remaining qualification, and historical prose.
  F22's proposed future migration is not a requirement to add pre-user machinery.
- Review receipt metadata/re-key/rollback supplements according to actual use;
  do not invent an anti-rollback guarantee.
- Reconcile current product/security/CI/architecture docs, add precise historical
  W9 correction, then full workspace checks and separately scoped real-model and
  packaged journeys. No completion or promotion claim yet.

## Later verification and source checkpoint

- External wrapper commit `eb0e47b57c2fba97ed13e8fe5e949d11798232cb` is pushed
  on `delysis/llama-cpp-rs` branch `codex/state-buffer-capacity`; it is not merged.
  Worktree: `/Users/george/.codex/worktrees/llama-cpp-rs-state-capacity`.
  The method now accepts `&mut [u8]`, passes its real capacity, and is safe to
  call. Its opaque export uses that method too. A real Qwen CPU test exercised
  capacities 0, 1, and required-size-minus-one with canaries, followed by exact
  export/restore. Rust 1.92: 1 passed, 0 ignored. Formatting/diff checks passed.
  An extra strict Clippy attempt encountered 109 warnings promoted to errors in
  existing external-wrapper code; no strict external Clippy pass is claimed.
- The native-platform manifest, lock, build identity, exported revision, and
  policy allowlist now pin that exact wrapper commit. The model-check runner
  passed BOTH registered CPU tests under rustc 1.92: saved-prefix live restore,
  durable replay, context rejection; and strict pre-cancelled batch handling.
  Each execution reported 1 passed, 0 failed, 0 ignored. This is native-boundary
  evidence, not packaged product acceptance or the real Mom cache path.
- Full CI no longer repeats the full workspace in Attachment, Information,
  Speech, Mom, and Loom jobs. One matrix owns all-target and doctest execution;
  macOS adds browser interaction and desktop packaging. Frontend commands no
  longer recursively trigger FTE Rust builds. Fuzz now executes bounded inputs
  and uploads crashes. The fuzz lock was stale (missing hound); it is repaired.
- Live dependency scans found the Vitest 4.1.10 advisory, GLib 0.18.5 iterator
  unsoundness, and yanked chacha20 0.10.1. Vitest/browser packages are 4.1.11,
  chacha20 is 0.10.2, and GLib is a documented two-line upstream security
  backport. Root and fuzz cargo-deny checks passed; pnpm reported zero known
  vulnerabilities afterward. Seven maintenance-only Rust notices have scoped
  reasons and a 2026-12-09 review date. The optimized GLib iterator tests are
  configured for Linux CI and have not run in this task.
- ort-sys 2.0.0-rc.13 already verifies its bundled target-specific archive hash
  before publishing the extracted ONNX Runtime 1.28.0 cache. Existing local
  hash-named cache directories are trusted. No unsupported claim of an
  unauthenticated fresh download, and no redundant downloader was added.
- MCP configuration now records the executable digest; every spawn compares
  it before process creation. Missing/changed identity requires reconfiguration.
  Managed Persona staging retains its additional native/no-argument constraints.
  Ordinary script/interpreter inputs are not misrepresented as immutable or
  sandboxed. A regression replaces the configured script and checks that no
  marker process effect occurs; it has not run yet.
- W9 attribution was checked with `git ls-tree` at the recorded checkpoint.
  The actual research-classified survivor was an orphaned `manifest_tests.rs`,
  not the claimed `research_admission.rs`. An explicit correction preserves
  the original historical claim; no census or acceptance rerun is implied.

The later selected Rust 1.92 run (FTE, Information, Mom libraries) exhausted
space while compiling dependency feature variants, before any tests executed.
It also caused a rustc/libc++ failure while disk was full. This is not a pass
and does not establish a source defect. Further builds are paused pending
usable space. Only this task's obsolete native build directories, superseded native
archives, and obsolete test executables were removed automatically; older caches remain
untouched. The approval request for the exact 6.3 GiB W1 target is still pending.

Remaining high-value work: Mom ambient host removal (F5); Loom platform privacy
and durability (remaining F15); latest MCP/FTE/Information compilation and tests;
real Mom cache exercise; inherited finding refresh; documentation reconciliation;
then one final workspace gate and scoped packaged/platform qualification. The
main native-platform changes are a work-in-progress checkpoint; this audit is not
complete. The external wrapper is under draft review in PR #11:
https://github.com/delysis/llama-cpp-rs/pull/11. Its CI was still running at the
last check; no remote qualification result is claimed.
