# Bounded local CI work orders — audit PR #78

Baseline repair: `74a00d2c04c794e63703f53637b0ef4b88268e95`, tree `8e7b9e59f119c120831dd5b4b74179b85a6d75d1`. A later documentation-only checkpoint may carry this file and the candidate patch. Use the exact head supplied by the coordinator; verify that the four repaired source postimages still match the execution receipt before using newer documentation.

**Work orders posted; no local agent start or execution is claimed.** Use the existing authorized local dispatcher. Do not create another autonomous repair service or bypass its quotas. No merge or main write.

## Shared preflight

Use a fresh isolated worktree, one job at a time when sharing a build directory. Preserve unrelated work and failed native-smoke captures. Record HEAD, `HEAD^{tree}`, clean status, host, tool versions, exact command, start/end, exit code, counts and raw log hashes. Never log credential values. Stop at a missing prerequisite or first unexpected failure and report it; do not repair assertions, loosen hashes, increase deadlines, download a model, change schema or reset a store.

Resolve the whole pinned toolchain before nested commands:

```sh
set -eu
TOOLCHAIN_CARGO=$(rustup which --toolchain 1.92.0 cargo)
TOOLCHAIN_BIN=$(dirname "$TOOLCHAIN_CARGO")
export PATH="$TOOLCHAIN_BIN:$PATH"
export CARGO="$TOOLCHAIN_BIN/cargo" RUSTC="$TOOLCHAIN_BIN/rustc" RUSTDOC="$TOOLCHAIN_BIN/rustdoc" RUSTUP_TOOLCHAIN=1.92.0
rustc -Vv
cargo -V
rustdoc -V
```

No foreground UI, model inference, OS credential operations, account linking, hosted requests, message sending or distribution publication is part of these jobs.

## A — existing policy machinery

```sh
node --test scripts/ci/test-ci-required.mjs
node --test scripts/ci/test-ci-metadata-selection.mjs scripts/ci/test-ci-plan.mjs scripts/ci/test-ignored-tests.mjs scripts/ci/test-product-state-backup.mjs scripts/ci/test-workflows.mjs
sh -n scripts/release-macos.sh
git diff --check
```

The first command must execute 26 passing tests. Record the actual counts for the second command; do not import a historical expected total. These checks are not native product acceptance.

## B — actual pinned Mom release targets

Run each once, separately, without credentials or models:

```sh
cargo test --offline --locked -p mom-llama-runtime --lib store::tests::legacy_plaintext_is_refused_before_database_creation -- --exact --format pretty --color never
cargo test --offline --locked -p mom-llama-runtime --lib kv_cache::tests::persistent_cache_corruption_invalidates_and_falls_back_after_reopen -- --exact --format pretty --color never
cargo test --offline --locked -p mom-llama-app --bin mom-llama-app app_runtime::tests::direct_native_operation_drains_before_final_join -- --exact --format pretty --color never
```

Each must execute exactly one passing, non-ignored test. Missing dependencies are BLOCKED, not permission to change locks or silently fetch prerequisites.

## C — exact shell gate against actual libtest

Create a disposable, dependency-free Rust fixture outside the repository. It needs one ordinary passing unit test and one `#[ignore]` unit test. Generate its local lockfile once with `cargo generate-lockfile --offline` before invoking the release helper, which deliberately uses `--locked`. Use the real `run`, `record_check` and `run_exact_test` functions extracted from the candidate release script without editing them, running from the fixture root with its real package name.

Run three cases through pinned real Cargo: the ordinary test, the ignored test and a nonexistent exact name. Only the first may pass and record a check. The other two must fail without recording one. Preserve fixture, extraction method and raw transcripts. Do not substitute a shell function for Cargo in this job. It qualifies the real harness-output contract, not any product.

## D — apply and validate the exact Rust test candidates

The adjacent `360-candidates.patch` is **not applied to the remote Rust source**. In the isolated candidate worktree, verify these preimages with `git hash-object` before applying:

| Path | Before | After |
| --- | --- | --- |
| `products/mom/crates/mom-llama-runtime/src/store.rs` | `0b9fcaf73401d6ccfcc01834cc611ea363709890` | `561b056e5871e693c995605357676c3b2ba35bb1` |
| `crates/services/speech/crates/speech-native-types/src/task_supervisor.rs` | `376ab2d1009645edccfdde19bdaa3f099dc3fb81` | `89f6493c54909f1b6bb50001400a614083d53368` |

Use `git apply --check` and then `git apply` on that patch. Verify both exact postimages. A mismatch is a stop, not permission for fuzzy reapplication. The packet check has already established clean application to those preimages, but no Rust check was run.

```sh
cargo test --offline --locked -p speech-native-types --lib task_supervisor::tests::duplicate_completion_faults_without_manufacturing_join_evidence -- --exact
cargo test --offline --locked -p mom-llama-runtime --lib store::tests::paired_document_mutation_has_one_commit_and_rollback_boundary -- --exact
cargo test --offline --locked -p speech-native-types --lib
cargo test --offline --locked -p mom-llama-runtime --lib store::tests::
cargo fmt --all -- --check
cargo clippy --offline --locked -p speech-native-types -p mom-llama-runtime --all-targets -- -D warnings
git diff --check
```

The first two commands each need one executed pass. Preserve broader counts, especially ignored tests. Do not add suppressions or silently reformat the entire tree. If rustfmt proposes a change, return its scoped diff for review.

In a second disposable worktree, apply the deliberate Speech negative control: remove only the explicit duplicate `supervisor.finish(worker_id, Ok(()));` call from the new test, leaving the real worker and its first completion intact. Its final fault assertion must fail, whereas the unmodified candidate passes. Restore by discarding only that disposable worktree; never patch the main working copy or count compile failure as a detected mutation. For Mom, retain the exact forced-rollback error assertion and commit/reopen assertions; do not replace production `mutate_documents` with a fake.

Return the resulting exact diff, candidate hashes and raw receipts. Do not push, merge or mark accepted from these instructions alone; the coordinator reviews the actual results and owns integration. The Rust candidates do not fix Keychain issue #79 or FTE issue #80.

## Review-only escalation, not an unconstrained CI task

Issue #79 requires an audited atomic create-only Keychain implementation with portable race tests; real credential testing needs a separately authorized disposable account. Issue #80 requires an audited production-bootstrap correction before disk access. These are not invitations for a low-capability agent to invent a credential system, storage migration, global registry or new platform support.
