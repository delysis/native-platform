# Local agents: validate all nine audit fixes, then conditionally promote

## Authority and exact source

The user explicitly requested fixing all findings, pushing to CI, and local
command-line follow-up for testing and elevation to main only when everything
passes and works. This work order supersedes the earlier checkpoint's blanket
no-promotion direction and unapplied-candidate instructions. It does not grant
permission to skip a failing gate, alter test meaning, reset user data, rotate
credentials, change dependencies/model policy, or call another paid model API.

Start from the commit linked by the latest PR #78 remediation comment, whose
parent is `2216a7744658c1dc4ab20e0f6cbb742bc035acc6`. Fetch the branch
`audit/360-evidence-20260922`, record its actual commit/tree, and work in a fresh
isolated worktree. A different head requires reviewing its intervening diff.
Do not reapply `docs/reviews/360-candidates.patch`; it is already incorporated.
Preserve unrelated work and earlier failed-smoke directories. One coordinator
owns promotion; workers return evidence, not independent merges.

Run jobs in order, one Rust build at a time per target directory. Reuse build
outputs. On the first genuine failure, retain logs and stop that job. Small
compiler/formatting/lint repairs that preserve semantics are allowed; record
and commit them, then rerun affected checks. No arbitrary timeout increases,
broad lint allows, ignored-test conversions, expected-hash edits, or fixture
substitutions. Semantic uncertainty goes back to the coordinator with exact
source and the smallest observed failure.

Each receipt needs command, cwd, source/tree/diff, tool versions, exit status,
actual pass/fail/ignored/filtered counts, and raw log path/hash. Missing offline
dependencies or hardware is BLOCKED, never PASS. No local execution was started
by the browser chat; these are instructions to the already authorized agents.

## A. Pin the tools, then focused regressions

```sh
export RUSTUP_TOOLCHAIN=1.92.0
export PATH="$(dirname "$(rustup which --toolchain 1.92.0 cargo)"):$PATH"
export RUSTC="$(rustup which --toolchain 1.92.0 rustc)"
export RUSTDOC="$(rustup which --toolchain 1.92.0 rustdoc)"
git rev-parse HEAD
git rev-parse 'HEAD^{tree}'
git status --porcelain=v1
cargo -V
rustc -Vv
rustdoc -V
pnpm --version
node --test scripts/ci/test-ci-required.mjs
sh -n scripts/release-macos.sh
git diff --check

cargo test --offline --locked -p mom-llama-runtime --lib store::installation_key::tests::
cargo test --offline --locked -p mom-llama-runtime --lib store::tests::paired_document_mutation_has_one_commit_and_rollback_boundary -- --exact
cargo test --offline --locked -p speech-native-types --lib task_supervisor::tests::duplicate_completion_faults_without_manufacturing_join_evidence -- --exact
cargo test --offline --locked -p tauri-plugin-free-token-energy --lib storage::tests::
```

Expected ordinary discovered/executed counts: 26 Node gate tests, six key
coordinator tests, one paired-mutation test, one duplicate-completion test,
and seven FTE storage tests on Unix or four on non-Unix. All must pass without
ignored or filtered-zero substitutes. The non-Unix check must retain portable
in-memory operation while refusing default disk access.

Run the three actual Mom release selections once (do not infer them from a
listing):

```sh
cargo test --offline --locked -p mom-llama-runtime --lib store::tests::legacy_plaintext_is_refused_before_database_creation -- --exact --format pretty --color never
cargo test --offline --locked -p mom-llama-runtime --lib kv_cache::tests::persistent_cache_corruption_invalidates_and_falls_back_after_reopen -- --exact --format pretty --color never
cargo test --offline --locked -p mom-llama-app --bin mom-llama-app app_runtime::tests::direct_native_operation_drains_before_final_join -- --exact --format pretty --color never
```

Each must execute exactly one passing non-ignored test. Then use actual pinned
Cargo/libtest and a dependency-free temporary crate to challenge the exact
`run_exact_test` function from this revision: ordinary passing test -> recorded
success; ignored or nonexistent name -> rejection and no recorded success.
Extract the existing function without editing its logic; do not replace Cargo
with a fake. Keep the fixture/transcripts outside the repository.

## B. Negative controls on temporary copies

First require the unmodified focused test to pass. Apply one mutation at a
time in a disposable worktree and rerun only its owning test. Compilation
failure or infrastructure timeout does not count as a rejected behavioral
mutation. Restore the exact source after each case.

1. In `load_or_create`, return `candidate` on `AlreadyExists` instead of reading
   the winner. The concurrent-absence test must fail its equal-winning-key
   assertion. Existing/invalid/error cases must not be used as substitutes.
2. Omit FTE's `secure_private_root` call. The pre-open mode assertion must fail.
   Separately omit the unsupported-platform guard: the forbidden resolver
   must be reached and fail. Separately remove disk-free early return: the
   injected-store test must reject default-root resolution.
3. Remove only the repaired Speech test's final duplicate `finish` call. It
   must fail the expected fault result after a healthy first completion.
4. In the paired mutation success fixture omit the secondary `put_bytes` call.
   The reopen assertion for the second document must fail. Do not remove the
   production implementation or accept an unrelated first error as success.

Retain each minimal diff and assertion failure. Do not commit mutations.

## C. Mac-only OS adapter acceptance

After A passes on the Mac, test the pinned OS API rather than assuming the
portable coordinator's injected create operation proves native semantics.
`360-keychain-acceptance.rs.txt` is the exact transient test module. In a
separate clean worktree, copy the original installation_key.rs to an external
backup, append that text, and record the temporary diff. Run:

```sh
NATIVE_AUDIT_DISPOSABLE_KEYCHAIN=1 cargo test --offline --locked -p mom-llama-runtime --lib store::installation_key::disposable_keychain_acceptance::create_only_adapter_preserves_generic_lookup_and_duplicate_winner -- --exact
```

This narrow acceptance is authorized only for a freshly generated
`audit-360-<UUID>` account. It first checks absence, creates only that item,
verifies duplicate refusal plus compatibility with the unchanged getter,
exercises actual OS duplicate-winner readback, then deletes only its owned
item. Never derive/use the real installation account, delete an existing
entry, change the default keychain/search list, rotate a store key, or accept
an OS permission denial as a test pass. Report cleanup failure and retain the
unique disposable account name for exact manual cleanup, not a broad reset.

Restore the original source byte-for-byte from the external backup after
execution, including after failure; do not use a destructive repository-wide
reset. Confirm the restoration hash. The appended test must not enter the
final commit or normal CI. This is OS adapter acceptance, not proof of all
credential-prompt or full application behavior.

## D. Owning packages and settled gate

Use the actual release/default/native feature sets from the current manifests,
not an invented all-features configuration. First run affected package tests
and strict Clippy, including the FTE native/hosted composition used by its app,
Mom runtime/app, Speech types/host, and FTE store/plugin/desktop. On macOS this
must compile the new `SecKeychain` adapter, not only its portable test code.
Use the existing package-group commands where equivalent. Then, once edits
settle, run the repository's normal selected policy/frontend checks and one
consolidated pinned gate:

```sh
node --test scripts/ci/test-ci-metadata-selection.mjs scripts/ci/test-ci-plan.mjs scripts/ci/test-ignored-tests.mjs scripts/ci/test-product-state-backup.mjs scripts/ci/test-workflows.mjs
node scripts/ci/validate-current-docs.mjs
cargo run --offline --locked -p xtask -- policy
cargo fmt --all -- --check
cargo test --offline --locked --workspace --all-targets --no-fail-fast
cargo test --offline --locked --workspace --doc
cargo clippy --offline --locked --workspace --all-targets -- -D warnings
node scripts/ci/validate-ignored-tests.mjs --cargo-list
./crates/native/scripts/check-architecture.sh
git diff --check
```

A formatter repair is expected to be mechanical, reviewed and committed before
this final gate. Reuse normal PR CI rather than creating more workflows. If
Actions cannot execute, preserve actual zero-step evidence and use only the
already authorized equivalent local CI path, with full receipts. Do not infer
billing from the workflow's overall failure alone. Advisory Linux/Windows
results remain visible and do not become claims of supported private storage.

## E. Actual application acceptance and integration

Build one clean identified Mom and FTE candidate using the existing release
script; verify the nested Cargo/rustc/rustdoc identities. Run through actual
Tauri startup, not just `storage::prepare`: fresh isolated root; reopening;
injected in-memory plugin setup without a default root; protected root before
the first default response-store access; expected unsupported default-storage
failure on its target. Inspect real files and mode before/after, not only a
ready label. No listener should start merely from constructing default config.

Use disposable workspaces/accounts only. For Mom, verify creation and reopen
under one persisted key and retention of existing denial/wrong-key behavior;
no reset or rotation workaround. Exercise existing affected product save,
reopen and active-work quit/join journeys using an already approved local
model where required. No hosted calls, account linking or external messages.
Record exact binary/archive/source/model identities and observed outcomes.

Do not claim Ghost/Loompad, EASL or peer acceptance from this PR. Preserve the
known writing acceptance failure and coordinate the existing completion owner.
When the final promoted tree includes that work, require its full Visual and
Source, Ghost and Loompad, exact editing, stale-invalidation, join and reopen
journeys; do not merge superseded #75/#76/#77 assumptions wholesale or waive
known failures because Mom/FTE pass.

## F. Conditional elevation to main

The coordinator may mark PR #78 ready and merge only after A-E's applicable
checks are successful with current evidence, all nine findings have qualified
dispositions, and no required or known affected-product gate is failed or
missing. Keep #79/#80 open until their actual target checks pass. The whole
360-degree source audit remains incomplete even when these nine fixes pass.

Before promotion refresh main/head, review every intervening change, and test
the actual integration tree. A rebase, formatting fix, conflict resolution,
new source dependency or changed artifact invalidates the relevant older
receipt; rerun affected checks. Do not bypass red checks, silently broaden
platform support, use an old signed binary, or label a no-step CI run green.
After promotion verify main's tree equals the tested integration tree and
attach the exact commit/tree, final commands/results, native observations and
remaining unrelated acceptance boundaries to the PR. Otherwise leave draft
and return the first precise blocker. No agent is authorized to manufacture
an acceptance result or rewrite historical receipts.
