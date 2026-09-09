# Integrated audit repairs

Branch: `codex/integrated-audit-fixes`, based on `4938461`. The supplied audit
reviews `924ac64`. Its recommendations are evidence to assess, not additional
user instructions. The user explicitly rejects speculative migration support
for these unreleased products.

This receipt records the implemented repairs and their observed qualification.
The final cross-platform CI rerun and packaged acceptance remain separate. It is
not a declaration that compilation or fixtures establish product acceptance.

## Repairs

- NP-002–005: FTE separates bounded authenticated control admission from
  generation admission; response bodies own response-map registration. Requested
  persistence failures and lost progress cannot produce successful completion.
  Legacy Chat preserves tool calls; completion preserves every final choice and
  emits a final suffix only when observed text is its prefix. Unrepresentable
  output produces an error. Cancellation does not become a success terminator.
- NP-012/F22: FTE and Loom inspect database identity before WAL/schema mutation.
  Fresh/current formats are supported; incompatible formats are refused. Loom
  has one current schema, with writing constraints, indexes, and triggers intact;
  the fourteen-step migration chain and retired research tables are removed.
  Opening a damaged project cannot create a replacement database. Existing
  incompatible data is preserved; no speculative migration or backup framework
  was introduced.
- F21/F14: Mom no longer automatically imports plaintext legacy JSON. Known
  residual artifacts are refused before key/database creation. Explicit import
  remains supported. A data-directory override cannot enable a deterministic
  release key. Wrong keys do not rotate themselves; receipt insertion is unique.
- F5: Mom callers receive an explicit runtime-owned operation scope. The global
  host lookup and duplicate ambient-call wrappers are removed. A production
  owner constructs its host, binds configuration, cancels its scope on drop,
  and joins its workers. Native-host access does not recursively lock the
  operation registry during Persona admission. Independently owned runtimes cannot discover each
  other's host through ambient state.
- Native empty-output and cancellation receipts preserve the actual invocation
  fact from the owning stream lifecycle. Cancellation before admission remains
  distinct from cancellation after the engine ran.
- Loom avoids redundant editor property updates. Ordinary navigation removes
  the completion decoration and its parent authority; Option word movement uses
  WebKit's native word boundary and commits that selection to ProseMirror before
  another refresh. This fixes a caret trapped next to a recently removed widget.
  Real modifier-key rollback and refocus scenarios verify caret/document state.
- F7: the production native worker supervisor invalidates Ready/active state on
  unwind, including poisoned status locks, and preserves the failed join.
- NP-008/F20: Information publication/removal errors retain exact committed
  identity after rename. Same-identity retries sync the actual parent directories.
  Unsupported synchronization cannot return success.
- F15: Information, FTE, and Loom refuse unsupported private-storage capabilities
  before mutation. Actual storage tests are Unix-scoped; portable logic remains
  enabled and explicit non-Unix no-mutation regressions cover refusal.
- NP-009: persistent cache metadata and encrypted entries are separate. Restoring
  an entry reads that payload once. Owner generations are revalidated at live
  promotion; corrupt metadata clears its family atomically. Updating one entry
  preserves unrelated ciphertext. Mom conversation checkpoint lookup now supplies
  the exact conversation owner, matching persistence; the old unowned lookup
  could never reuse its own checkpoint.
- NP-011: ZIM title ordering uses the path for an empty title, with independent
  valid and reversed title-table fixtures.
- F13: MCP configuration records executable SHA-256; every spawn checks it.
  A replaced/missing executable requires reconfiguration. Managed Persona tools
  retain native/no-argument restrictions. Scripts/interpreter inputs are not
  represented as immutable or sandboxed. A replaced-script regression checks
  that no marker process effect occurs.
- F18: the pinned llama.cpp wrapper takes a destination slice and passes its real
  length. Native export no longer needs an unsafe call. State import remains the
  documented narrow unsafe boundary.
- F17: Vitest/browser packages were updated to 4.1.11, chacha20 to 0.10.2, and the
  GTK-compatible GLib 0.18.5 has a documented two-line upstream security backport.
  Seven maintenance-only notices have scoped reasons and a 2026-12-09 review
  date. No runtime vulnerability is waived. ort-sys already verifies the hash of
  its downloaded archive; no redundant downloader was added.

## Simpler development process

Full CI executes one workspace matrix, doctests, and Linux Clippy. macOS adds
browser interaction and packaging; frontend commands do not repeat Rust builds.
The fuzz lane executes bounded inputs and retains crashes. A hash-pinned CPU
model lane runs exact registered tests and rejects zero execution. Advisory
checks scan the actual root and fuzz locks.

PR selection uses Cargo's resolved and declared local dependency edges plus
explicit non-Cargo rules. Unknown paths or unavailable metadata select full
coverage. The old path planner, Mom overlay, and shadow comparison are removed.
The obsolete W8/W9 `xtask lean` census is retired. Git retains its history.

Mom's Consult-to-Persona migration and repair machinery is removed. Current
builtin catalog updates still preserve user edits. The unused file-size-only
native memory estimator and tests of its obsolete formula are removed;
admission uses the configuration-sensitive estimator. Memory budget mode is
explicit; the heuristic migration from historical default values is removed.

Current contributor, product, security, and architecture documentation now
states these boundaries. Dated ADRs and receipts remain historical. The W9
correction names the actual orphan `manifest_tests.rs`, not the incorrectly
attributed `research_admission.rs`; no historical evidence was silently resealed.

## Verification recorded so far

Before the HTTP repairs, real adapter regressions observed control HTTP 429,
a leaked response identity after body drop, and `response.completed` following
storage failure. The independent valid ZIM archive also failed before repair.

With the complete Rust 1.92 toolchain on PATH:

- Consolidated workspace execution at `1135259` passed 1,591 tests in 63
  harnesses, with 43 fixture-dependent tests ignored. This includes inherited
  lifecycle, redirect, Speech, FTE, Information, and Mom/Loom store regressions.
- Workspace doctests passed 20 cases across 44 harnesses, none ignored.
- After the later cancellation/cache-owner fixes, the actual Mom library passed
  172 tests, and FTE desktop/loopback activity passed 10 tests. The updated real
  product cache scenario passed on Metal with the hash-verified Qwen fixture:
  encrypted checkpoint creation, cold/warm reuse, explicit clearing, and cache
  off were executed. This is product-runtime evidence, not packaged UI evidence.
- Strict workspace/all-target Clippy passed after the final cache-owner and
  CI runner corrections.
- The wrapper's real CPU capacity test passed for zero, one, and undersized
  buffers with canaries, then exact export/restore: 1 passed, none ignored.
- Native saved-prefix live/durable restoration/context rejection and strict
  pre-cancelled batch handling passed locally and in CI: two exact CPU tests.

Other observed checks:

- Full WebKit interaction suite: 74 passed after the native word-navigation fix;
  the formerly intermittent rollback also passed in isolation. Svelte check
  reports zero errors and warnings. The previous setProps-only repair passed
  locally but failed in CI and was insufficient.
- Loom unit suite passed 448 tests. Unit workers are capped at four; contention
  with native CPU inference had exceeded the compiler test's five-second limit.
  The limit was not increased.
- Simplified metadata/planner checks: 37 passed. Consolidated workflow/planner,
  ignored-registry, and backup checks: 120 passed. The changed workflow/registry
  checks subsequently passed 63 cases.
- Mom architecture and contracts passed (102 commands, 99 affordances, 46 effects,
  36 parity rows, 58 upstream settings, zero blockers).
- Current-document paths and ignored-test source registry validation passed.
- Root/fuzz cargo-deny checks and JavaScript advisory scan passed after repairs.
- CI `34401258044` passed frontend, policy, both bounded fuzz targets, and the
  exact CPU model scenarios. Linux workspace tests/doctests/Clippy passed, but
  its optimized GLib command could not test a non-workspace dependency. A small
  Linux workspace integration target now exercises the real GLib string iterator
  under release optimization. Windows compiled but correctly refused a private
  loopback token in a test that assumed Unix support; desktop activity remains
  portable, while the real private-token case is Unix-only. macOS compiled and
  passed Rust tests, then reproduced the now-repaired native caret failure.
  These failures are preserved, not relabeled as a green full run.

Earlier Rust 1.95 results and the disk-full Rust 1.92 attempt are not substituted
for current qualification. Commands select the complete 1.92 toolchain on PATH
and reuse the active shared target. CI retains dependency caches on test failure;
failed behavior still fails the aggregate. The model lane now includes Mom's
registered product-cache integration test and rejects zero execution.

## External wrapper

Native-platform pins source commit `eb0e47b57c2fba97ed13e8fe5e949d11798232cb`.
Draft PR: https://github.com/delysis/llama-cpp-rs/pull/11.

The fork also retires upstream registry publication: it is consumed by immutable
Git revision and must not accidentally package against the unrelated registry
sys crate. Both crates are unpublished and upstream release-publication workflows
are removed in `3cea96c5d09c1d9cdc0db418ea27d3f1e200465b`. Main's source pin need
not change for workflow metadata.

CI run 34394817229 passed Linux wrapper tests, Windows/macOS builds, workflow
policy, and CUDA. The CUDA check took nearly its ninety-minute allowance because
it built a multi-architecture binary. Commit `4c000bd5992da2a95beffe3b6777216f2a5fea67`
pins Rust 1.92 and limits that compile check to representative CUDA architecture
75 with a thirty-minute cap. This is compile coverage, not GPU runtime acceptance;
its CI rerun is `34403567854`.

## Local cache maintenance

The user's September 9 instruction supersedes per-cache approval. The preference
is saved. A dependency-free safe Rust helper checks Cargo identity, a fourteen-day
age threshold, active builds, Cargo locks, symlinks, and open files. It only prunes
reproducible Cargo output subdirectories; model fixtures and source stay intact.
A real filesystem regression passed. A daily 04:00 automation runs the helper
quietly, notifying only on a failure requiring intervention.

The automatic sweep removed 228,332,165,314 bytes, in addition to the obsolete W1
and W9 targets. Available space was approximately 193 GiB after resumed builds.
The disk blocker and old approval request are resolved.

## Remaining qualification

Complete the cross-platform CI run with the corrected
GLib target, Windows fixture scope, native caret path, and registered Mom cache
scenario. Keep packaged interaction, OS credentials, native quit/join, and reopen
evidence separate from compilation and controlled fixture execution. No merge
or distribution is implied by this receipt.
