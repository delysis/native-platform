# 360-degree audit: executed checkpoint

Baseline: `87939f3873977ef2670ddab9e869ca9d5d7d1739` / tree `7ae2286a855305b086b3d893b5c63e7613491a6c`.
Published code: `74a00d2c04c794e63703f53637b0ef4b88268e95` / tree `8e7b9e59f119c120831dd5b4b74179b85a6d75d1`, draft PR #78.

**The full audit is not closed.** This checkpoint has five published gate fixes, two uncompiled test-repair candidates, and two open implementation issues. No main write, Rust build, real libtest run, model execution, credential operation, foreground app launch or native acceptance occurred here.

## Findings and dispositions

| ID | Priority | Finding | Disposition |
| --- | --- | --- | --- |
| A360-001 | P2 | `run_exact_test` could credit an ignored test. | Fixed: require exactly one executed passing libtest summary, preserve command failure, reject absent/ambiguous/zero results. Controlled-transcript regression executed; real-libtest job remains open. |
| A360-002 | P3 | Identical Loom promotion/reopen check ran twice. | Removed the duplicate, retained the original, added a duplicate-selection check. |
| A360-003 | P2 | Mom release still named a removed plaintext-migration test. | Select actual `legacy_plaintext_is_refused_before_database_creation`; no migration restored. |
| A360-004 | P2 | Nested pnpm/Tauri could inherit standalone Cargo instead of the pinned tools. | Pin actual Cargo/rustc/rustdoc and inherited environment before nested commands. Controlled-tool regression executed. |
| A360-005 | P2 | A full plan with omitted macOS matrix children could borrow a passing aggregate. | Independently check full matrix membership. No claim that the real planner produced such a malformed plan. Advisory platforms unchanged. |
| A360-006 | P2 | Mom's paired-transaction regression calls its own test-only mutation engine. | Exact candidate patch removes that 67-line duplicate and exercises production `mutate_documents`, including rollback and reopen. Not compiled or applied to remote Rust source. |
| A360-007 | P2 | Speech's duplicate-completion fixture faults on its first completion because no join handle exists. | Exact candidate patch first joins a real worker and asserts healthy completion, then delivers the duplicate. Not compiled or applied to remote Rust source. |
| A360-008 | P1 | Concurrent Mom first-open can overwrite the installation encryption key. | OPEN: issue #79. Source-confirmed mechanism, no observed loss or OS execution. |
| A360-009 | P2 | FTE default response persistence occurs before the desktop privacy gate. | OPEN: issue #80. Source-confirmed ordering, no native disclosure reproduced. |

The Rust candidates are the adjacent `360-candidates.patch`; bounded validation is specified in `360-local-ci.md`. Those files are work products, not evidence that the Rust changes have run. Other real production mutation/lifecycle tests remain valuable; the two misleading tests do not invalidate all existing coverage.

## Highest-priority implementation issue

Mom's `store.rs::load_or_create_macos_key` reads a shared account and uses `set_generic_password` after absence. The locked security-framework 3.7.0 implementation attempts `SecItemAdd`, then uses `SecItemUpdate` on a duplicate. Two processes can both see absence, cache different generated keys and write under them, while only the last Keychain value survives. The same-key concurrent store fixture does not cover this independent OS-key creation race.

Issue #79 requires atomic create-only behavior and duplicate-winner readback with existing namespace/denial semantics preserved. Do not add rotation, migrations, unsafe FFI or a new registry. A deterministic no-credential race regression and separately authorized disposable-keychain qualification are different checks. No actual user-data loss or credential access is claimed.

Primary dependency source: https://docs.rs/security-framework/3.7.0/src/security_framework/passwords.rs.html , source lines 158–176. The fetched latest page identified its version as 3.7.0; the current lock's version was independently checked.

## FTE integration issue

The plugin setup creates the root and opens `gateway-v2.db` before the desktop setup secures its directory and reaches the separately guarded `gateway.db`. `SqliteStore::open` has no early private-platform check. The plugin also creates a disk root unnecessarily when an in-memory store is supplied without loopback.

Pinned Tauri 2.11.5 source confirms plugin initialization during `Builder::build` and later application setup on Ready. Therefore the desktop database's refusal test cannot establish that earlier startup performed zero I/O. Potential Unix exposure depends on actual parent permissions and umask; no disclosure or Windows launch was observed. Issue #80 requires the default persistence owner to enforce the precondition and retain genuinely disk-free injected operation.

Primary framework source: https://docs.rs/tauri/2.11.5/src/tauri/app.rs.html , source lines 2440, 1423–1428 and 2521–2532. Latest 2.11.6 was not substituted for the pinned source.

## Executed checks

`node --test scripts/ci/test-ci-required.mjs`: **26 passed, 0 failed, 0 skipped**, Node 22.16.0/Linux. The existing CI test entrypoint imports the new release cases. Shell/Node syntax and `git diff --check` passed. All four published code postimages match the tested local hashes.

Before correction the relevant development stages recorded 3/8, 8/9, 9/11 and 25/26 passing. Those are successive counterexample stages, not independent full runtime campaigns. The tests exercise actual shell/Node logic with controlled tool boundaries, not Rust or Tauri. An additional five-case packet validator checked census accounting, status honesty and exact Rust patch applicability, not product behavior.

PR workflow `35787738113` failed before planner execution: job `106948534871` returned zero steps, and build/test jobs were skipped. No code failure or successful build can be inferred. Billing was not verified as this run's cause. Local CI work orders were posted; no local-agent execution receipt was received.

## Source coverage and remaining scope

A recovered 1,181-file archive was reconstructed to exact tree `a66ff9c7cfe6eb5d2e73a9f725c0c22a0893a075` at `6f61eb6e36ee2a1fb0be6d79c93f70b16653bdf3`. The authenticated compare to baseline has 62 modified and seven added paths, giving 1,188 baseline paths. Its 1,119 unchanged files have verified current bytes. Changed files were not silently treated as current.

The complete current tree was not reconstructed. The delivery's conservative execution reading ledger records 11 full-file reads, 18 partial reviews and 1,159 unreviewed paths. All 37 original finding identities/meanings were recovered; reconciliation is started, not completed, and old outcomes do not close new evidence gaps.

Native state-buffer live receipts, Speech worker ownership, FTE quota/store behavior and Attachment budget accounting received bounded source review. Their callers, platform paths and native behavior are not comprehensively qualified. No universal lifecycle framework or retired migration machinery was restored.

PR #77 at `f0490c463f75f76714520bcaead099ac2216f7be` remains the separate full-streaming completion continuation. Its native Visual/Source Ghost/Loompad acceptance stays open; no competing controller changes or wholesale branch merge were made. EASL, peer inference, Signal, convergence and research extensions remain separate.

Next acceptance authority comes from actual bounded local results, not this report. Issue #79 is the highest-priority new implementation task; issue #80 and the two test candidates follow. Full inventory reading, historical reconciliation, native/FFI validation and exact packaged writing acceptance remain open. No release or main promotion is authorized by this checkpoint.
