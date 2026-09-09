# Integrated audit repairs

Branch: `codex/integrated-audit-fixes`, based on `4938461`. The supplied audit
reviews `924ac64`. Its recommendations are evidence to assess, not additional
user instructions. The user explicitly rejects speculative migration support
for these unreleased products.

This receipt records ongoing work. Final workspace, platform, and packaged
qualification is unfinished; it is not a declaration that the audit is complete.

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
  and joins its workers. Independently owned runtimes cannot discover each
  other's host through ambient state.
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
  preserves unrelated ciphertext.
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
admission uses the configuration-sensitive estimator.

Current contributor, product, security, and architecture documentation now
states these boundaries. Dated ADRs and receipts remain historical. The W9
correction names the actual orphan `manifest_tests.rs`, not the incorrectly
attributed `research_admission.rs`; no historical evidence was silently resealed.

## Verification recorded so far

Before the HTTP repairs, real adapter regressions observed control HTTP 429,
a leaked response identity after body drop, and `response.completed` following
storage failure. The independent valid ZIM archive also failed before repair.

With the complete Rust 1.92 toolchain on PATH:

- FTE loopback 12, protocols 12, store 2; Information host 6 and store 27;
  Mom runtime library 177 passed, none ignored. These precede the latest Mom
  scope/catalog refactor.
- Loom's current-schema library run passed 122 tests, none ignored. This precedes
  the additional platform guards and damaged-project-open regression.
- The wrapper's real CPU capacity test passed for zero, one, and undersized
  buffers with canaries, then exact export/restore: 1 passed, none ignored.
- Native saved-prefix live/durable restoration and context rejection passed;
  strict pre-cancelled batch handling passed: two exact tests, none ignored.
  These are native-boundary results, not packaged product acceptance.
- The consolidated Mom/runtime/CLI/desktop check reached product callers after
  rebuilding the native feature variant. It found one missing scope in the
  desktop cache-clear callback, now repaired. The workspace all-target test run
  is the next compilation and execution gate.

Other observed checks:

- Simplified metadata/planner checks: 37 passed, none skipped.
- Workflow checks before the helper-file rename: 41 passed.
- Mom architecture and contracts passed (102 commands, 99 affordances, 46 effects,
  36 parity rows, 58 upstream settings, zero blockers).
- Current-document paths and ignored-test source registry validation passed.
- Root/fuzz cargo-deny checks and JavaScript advisory scan passed after repairs.
  Optimized GLib runtime execution and bounded fuzz execution still need results.

Earlier Rust 1.95 results and a disk-full Rust 1.92 attempt are not substituted
for current qualification. Future commands select the entire 1.92 toolchain on
PATH and reuse the active shared target directory.

## External wrapper

Native-platform pins source commit `eb0e47b57c2fba97ed13e8fe5e949d11798232cb`.
Draft PR: https://github.com/delysis/llama-cpp-rs/pull/11.

The fork also retires upstream registry publication: it is consumed by immutable
Git revision and must not accidentally package against the unrelated registry
sys crate. Both crates are unpublished and upstream release-publication workflows
are removed in `3cea96c5d09c1d9cdc0db418ea27d3f1e200465b`. Main's source pin need
not change for workflow metadata.

CI run 34394817229 passed Linux wrapper tests, Windows/macOS builds, and workflow
policy. CUDA was still running at the last observation. The preceding source
run passed CUDA but failed the obsolete registry dry-run publication path;
that failure is distinct from the repaired state-buffer behavior.

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

Finish current workspace tests/doctests/Clippy and the final workflow checks;
exercise the actual Mom persistent cache path with the local model; qualify
Linux/Windows capability behavior, optimized GLib, and bounded fuzz in CI.
Keep browser and packaged interaction, OS credentials, native quit/join, and
reopen evidence separate from compilation and fixture execution. Recheck inherited
lifecycle, redirect, and Speech regressions in the consolidated workspace run.
