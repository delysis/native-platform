# EASL integration handoff for Chat Pro Astra

## State preserved for continuation

Branch: `codex/loom-easl-main-integration`  
Base: `5b34e054cb65a34dd8f44f28fcec94d1e535fe86`  
Packet: `/Users/george/Downloads/loom-easl-next-5b34e054`  
Packet validation: `node tools/preflight.test.mjs` passed (8/8), and the packet
preflight/export plus `--apply` both verified the exact base, source blobs,
unique fragments, patch hashes, and the 18 resulting candidate files.

The two supplied patches are applied in this dedicated feature branch as a
**takeover handoff**. This is intentionally not a promotable/main-ready
revision: retain the blockers below in the branch history and resolve them
before opening or merging a promotion PR.
Two mechanical validation repairs were necessary:

1. `src/accessibility_tests.rs`: use the pinned `accesskit::Node::is_expanded()`
   accessor (which returns `Option<bool>`) instead of nonexistent `expanded()`.
   This changes no product behavior.
2. `ui/loom.easl`: rename the `pane-toggle` parameter `position` to
   `pane-offset`. The installed EASL compiler rejects a function parameter that
   shadows its top-level `position` binding. This one rename cleared the shared
   `CantShadowTopLevelBinding("position")` error that had caused 53 review tests
   to fail at compile time.

The complete review-only test receipt is **not green**: 102 passed, 2 ignored,
and one failed after 107.58 seconds:

```text
document::tests::discard_waits_for_an_accepted_journal_and_leaves_only_the_saved_source
products/loom/experiments/easl-interface/src/document.rs:751
Storage response timeout
```

The packet does not modify `src/document.rs`; all newly reached current-chrome
and accessibility tests preceding this result passed. Reproduce the named test
once in isolation before deciding whether it is a test/runtime timing regression
or an environment flake. Do not mask it with a longer arbitrary timeout: inspect
the accepted-journal lifecycle and preserve its no-data-loss assertion.

Focused reproduction has passed once: `1 passed; 104 filtered out; 14.79s`.
That makes the full-suite failure timing/contention-sensitive rather than a
deterministic failure, but it remains a delivery gate until a clean complete
receipt is captured.

## Blocking work: native reference guard, do not bypass

The normal native build is presently blocked before application compilation:

```text
cargo check -p loom-easl-interface
error: Loom interface reference changed. Review the current view and its native
port together before updating the reference hash.
actual App.svelte SHA-256:   6667474039c166a5a33b1f4e2cd23e5cca79353962c73b1df4ac0e5c3552709a
build.rs expected SHA-256:   3d45a082bb51a6f11a06b865e5e6c5a198e1c836dbb58fe3ded8562d931a1725
```

The packet itself says the App/CSS guard hashes are intentionally unchanged and
is only an unqualified candidate. Do **not** replace the expected hash merely to
make CI green. First reconcile the current `products/loom/apps/loom/src/App.svelte`
with the EASL port and review whether the native experiment still represents the
current Loom interface. If and only if that review establishes an intentional
reference change, update the guard together with the port and add source-grounded
tests/receipt. Otherwise restore/rebase the appropriate reference source and
keep the guard strict.

## Required continuation gate and delivery

After resolving the guard on an intentional basis:

1. Focused reproduction of `document::tests::discard_waits_for_an_accepted_journal_and_leaves_only_the_saved_source`, followed by `cargo test -p loom-easl-interface --no-default-features --features review-only`
2. `cargo check -p loom-easl-interface` (native-default guard/build)
3. `cargo fmt --all -- --check`
4. `cargo clippy -p loom-easl-interface --no-default-features --features review-only --tests -- -D warnings`
5. Run the requested macOS native acceptance separately: launch the actual
   binary, exercise Add/pane controls/focus/IME, and verify the app/bundle/SHA
   identity. The packet explicitly supplies no native product qualification.
6. Only after the above passes, commit the candidate plus the two narrow fixes,
   push `codex/loom-easl-main-integration`, and dispatch/reuse its GitHub CI.

Known non-blocking warning seen during review compilation:
`crates/services/easl/vendor/harfrust/src/hb/face.rs:439` has unused `shape`.
It predates this candidate and was not altered.

## Review-only lint gate: seven required fixes

`cargo fmt --all` has been run and its follow-up `cargo fmt --all -- --check`
passes. `cargo clippy -p loom-easl-interface --no-default-features --features
review-only --tests -- -D warnings` still fails with these actionable findings:

1. `src/accessibility.rs:43` — `update_accessibility` is 108 lines; split into
   bounded helpers rather than suppressing `too_many_lines`.
2. `src/document.rs:822` — replace `Duration::from_secs(60)` with the clearer
   `Duration::from_mins(1)` (or an equivalent named timeout policy).
3. `src/interface.rs:352` — invert the `if kind != "control"` condition to the
   Clippy-preferred `if kind == "control" { None } else { ... }` form.
4. `src/theme.rs:70` — avoid direct equality of the `[f32; 9]` fixture; compare
   bit patterns or use an exact-domain assertion that documents why zero/one are
   safe values.
5. `src/lib.rs:376` — `perform` is 109 lines; extract coherent action families
   while retaining the current explicit authority boundaries.
6. `src/lib.rs:399` — add the suggested semicolon after `pointer_edit(kind, arg)?`.
7. `src/lib.rs:827` — `window_event` is 103 lines; extract event-specific helper
   paths, preserving native-window ownership and ordering.

Do not add blanket `allow(clippy::...)` attributes: this package opts into
pedantic Clippy and the requested delivery gate uses `-D warnings`.

## Local CI receipt on takeover commit `f2c1dc0c`

These commands were run locally on macOS after the takeover branch was pushed.
They are a substitute for unavailable/billing-gated remote capacity only; they
do not constitute macOS native product acceptance.

| Gate | Result | Evidence / consequence |
| --- | --- | --- |
| Packet preflight unit tests | PASS | `node /Users/george/Downloads/loom-easl-next-5b34e054/tools/preflight.test.mjs`: 8/8. |
| Packet exact-base validation and apply | PASS | Both read-only export and `--apply` verified base `5b34e054`, source blobs, fragments, recipes, and candidate hashes. |
| Current service documentation | PASS | `node scripts/ci/validate-current-docs.mjs` emitted `surfaces=2`, `documents=7`, `current_paths=5`. |
| PR policy/unit suite | FAIL | 130/132 passed. The changed `products/loom/experiments/easl-interface/build.rs` hashes to `ce2c6517b12743e88a38124630936b605ac41dc3de4a838db12ae563364a6cea`, but `ci/ignored-tests.json` records `7eae63b51f0272f1490c5408f932f332b5ca331438bec6a06724fb6d7abd181a`. Review the build script, then update its reviewed catalog digest and rerun the suite. The second failure is only the test cascade from this first catalog failure. |
| Local CI planner | PASS, full matrix selected | With base `c0bcc42d` and head `f2c1dc0c`, risk is `dependency`; it selects policy, root/native/gateway/attachment/information/speech/Mom/Loom Linux, selected Windows, frontend, macOS, ignored-test, dependency-graph, and fuzz jobs. This branch predates current `main`, so it cannot claim a Loom-only CI scope. |
| Full portable workspace test gate | FAIL | `cargo test --locked --workspace --all-targets --no-fail-fast` compiled broadly, then stopped in `loom-easl-interface` build.rs on the same strict App.svelte reference-hash mismatch documented above. No downstream workspace-test success claim is valid. |
| Review-only EASL suite | NOT PROMOTABLE | Complete receipt: 102 pass, 2 ignored, 1 journal timeout; focused rerun of that one test passed in 14.79s. Treat as an unresolved timing/contention defect until a clean full receipt exists. |
| Formatting | PASS | `cargo fmt --all -- --check`. |
| Review-only Clippy | FAIL | The seven findings listed in the preceding section remain. |

### Promotion order

1. Review and reconcile the current Loom App source with the native EASL port;
   update the strict reference hash only when that source review justifies it.
2. Independently review `build.rs`, update the reviewed build-script digest in
   the ignored-test catalog, and make the 132-test policy suite pass.
3. Fix the seven review-only Clippy findings without broad lint suppression.
4. Obtain a clean complete review-only suite, diagnosing the accepted-journal
   timeout rather than increasing its deadline.
5. Rerun the selected full local/remote matrix, then complete live macOS native
   UI, focus, IME, bundle, and identity acceptance before marking the PR ready.

## Local continuation receipt for packet `easl-native-next-fa931bf1`

Executed in a new detached worktree at `fa931bf13f788a0c093e977685f37c2d12efac06`
with Rust 1.92.0 (`rustc 1.92.0 (ded5c06cf 2025-12-08)`, Cargo
`1.92.0 (344c4567c 2025-10-21)`) and `--offline --locked --profile native-view`.
The 17-test packet preflight passed, then applied the packet with no source/hash
relaxation. `cargo fmt --all -- --check` and `git diff --check` pass.

Two narrow repairs were required by executable gates:

- `interface_fault_tests.rs` now uses the repository-proven unconditional
  infinite-loop budget fixture; the original input-conditioned loop returned
  normally and could not test VM poisoning.
- `chrome_state::ADD_LABELS` is `#[cfg(test)]`; it is test data and otherwise
  triggered the owned-crate `-D warnings` dead-code gate. Runtime behavior is
  unchanged.

### Command results

- Focused packet tests: document model **6/6**, input geometry **4/4**, VM
  faults **4/4**, frame state **2/2**, queued journal acknowledgement **1/1**.
- Full review-only EASL target: **120 passed, 0 failed, 2 ignored** (the two
  manual renderer tests).
- Review-only owned-code Clippy with `-D warnings`: **PASS**.
- Ignored-test registry: **24/24 PASS**; catalog count remains unchanged at 46.
- Repository CI policy/workflow/current-doc suite: **136/136 PASS**; current-doc
  validation reports `surfaces=2`, `documents=7`, `current_paths=5`,
  `retired_edges=2`.
- Reusable suite (`easl-native-text`, `easl-text`, `loom-markdown`,
  `loom-text-session`): **PASS** across all unit, integration and doc tests.
  Cargo reports only the pre-existing duplicate `specimen` example output-name
  warning for the two EASL packages, plus the pre-existing unused `harfrust`
  `shape` warning.

### Intentional remaining failure

The ordinary native-app gate was run separately:

```text
rustup run 1.92.0 cargo check --offline --locked --profile native-view \
  -p loom-easl-interface
```

It correctly rejects the unchanged strict interface reference:

```text
actual App.svelte: 6667474039c166a5a33b1f4e2cd23e5cca79353962c73b1df4ac0e5c3552709a
expected guard:     3d45a082bb51a6f11a06b865e5e6c5a198e1c836dbb58fe3ded8562d931a1725
```

This is an expected **native qualification failure**, not a reason to update the
hash. No app was launched, no native parity was claimed, and no Tauri adapter
was implemented. The packet's architecture slice remains a tested source
continuation only; the next owner must handle the separate native host, IME,
accessibility, signed-bundle and source-parity work already described above.

## Local receipt for `easl-tauri-next-f721ec2d` (candidate commit)

Applied in a fresh detached worktree at exact base `f721ec2d907916744d9c590f86b65c3bfb1264e8`.
Packet tooling passed **25/25** (`preflight`, icon, delivery); full-source
preflight and apply passed. Rust toolchain: `rustc 1.92.0 (ded5c06cf 2025-12-08)`,
Cargo `1.92.0 (344c4567c 2025-10-21)`, all Cargo commands used
`--offline --locked --profile native-view`, one process at a time.

Two demonstrated compile/lint repairs were made:

- `layout.rs`: matched the current `IOManager::record_compute` signature.
- `native/surface.rs`: handled Tao's non-exhaustive `MouseScrollDelta`.
- `build.rs` and `native/keys.rs`: fixed Clippy markdown diagnostics.

The new build-script digest was reviewed and updated in `ci/ignored-tests.json`
to `0244d518564753ca1f12b37f78e9db15a4c7089360a48fa87a13020233a20d3b`.
The registry test's reviewed-build-script count was updated from 8 to 9; the
ignored-test inventory remains **46** entries with no ignored test added.

### Executed gates

- `easl-tauri-probe` default: **16 passed** (7 unit + 9 integration).
- `easl-tauri-probe --features native-probe --all-targets`: **21 passed**
  (12 unit + 9 integration).
- Probe default and native-feature Clippy with `-D warnings`: **PASS**.
- Loom review-only: **120 passed, 0 failed, 2 ignored** (manual renderer tests).
- Loom review-only Clippy with `-D warnings`: **PASS**.
- Reusable `easl-native-text`, `easl-text`, `loom-markdown`, and
  `loom-text-session`: **PASS** across all tests and doctests.
- Repository CI metadata/planner/required/ignored-tests/backup/workflow/current-doc
  suite: **136/136 PASS**.
- `cargo fmt --all -- --check` and `git diff --check`: **PASS**.

Warnings are pre-existing: unused `harfrust::shape` and duplicate `specimen`
example output names. No foreground application was launched and no foreground
probe authorization was assumed. The native example remains unqualified:
IME/preedit/candidate-rectangle behavior, OS accessibility, production window
registration/close veto, WebView removal, Loom service/storage integration,
source parity, signed packaging, and visual/lifecycle acceptance remain open.

This candidate is suitable for draft-PR review/component promotion only. Do not
update Loom App/CSS hashes, mark PR #73 ready, merge, or promote to main from
these receipts. Next work order: authorized focused probe interaction and
lifetime evidence; real composition bridge; platform accessibility tree; then
reusable Tauri-managed surface integration with an existing Loom writing owner.

## Local continuation receipt for packet `easl-managed-next-a386e671`

Applied in a new clean detached worktree at exact base
`a386e6715e6276795734b844ed866b31b517b3b1`.

### Source correspondence

- Packet tooling: **33 passed, 0 failed, 0 skipped**.
- Read-only preflight and controlled `--apply`: **PASS**; 11 packet paths
  applied with no source-hash relaxation, three-way merge, lockfile
  regeneration, guard update, or packet mutation.
- The required mechanical/API repairs were retained in the candidate:
  `generate_context!` is expanded once per example binary; native close
  requests box the large `tauri::Window` enum variant; identical cleanup arms
  are merged.
- No foreground Loom/probe launch, PR mutation, merge, or source-reference
  update occurred.

### Rust/component results

Rust 1.92.0 (`rustc 1.92.0 (ded5c06cf 2025-12-08)`, Cargo
`1.92.0 (344c4567c 2025-10-21)`), always `--offline --locked
--profile native-view`, one Cargo process at a time.

- Managed lifecycle integration: **6/6 passed**.
- Default probe package: **22/22 passed** (7 unit, 6 managed lifecycle, 9
  editor integration).
- Native-feature all-targets package: **29/29 passed** (14 unit, 6 managed
  lifecycle, 9 editor integration; example target built with zero tests).
- Default and native-feature probe Clippy with `-D warnings`: **PASS**.
- `cargo fmt --all -- --check` and `git diff --check`: **PASS**.
- The only emitted warning is the pre-existing vendored
  `harfrust::shape` dead-code warning; it is outside this candidate.

### Hidden native lifecycle

The built binary was resolved from Cargo metadata and executed once with
`--check-native-lifecycle`; exit status **0**. Receipt validator: schema valid;
it explicitly does not claim to verify native execution.

- Evidence: `/Users/george/.codex/evidence/easl-managed-next-a386e671/`
- Binary SHA-256:
  `08ef2a0c091cad36f427292363128179005e42d7f8cab7ead3727738eefb808c`.
- Primary window: **3 attachments / 3 releases**, 4 presented frames, 2 exact
  source/selection/undo checks, destroyed callback and Manager removal observed.
- Peer window: **2 attachments / 2 releases**, 3 presented frames, 2 exact
  source/selection/undo checks, destroyed callback and Manager removal observed.
- Close veto, application-exit veto, renderer reattachment, positive
  presentation, retained editor state, and peer isolation all reported true.
- Evidence remains `qualified:false`; visual pixels, IME/preedit,
  accessibility, Loom integration, storage, packaging, and latency remain
  unexecuted.

### Still-unexecuted product/selected gates

The repository policy/current-doc/workflow/ignored-test suites, Loom review-only
and reusable-consumer suites, affected dependency consumers, and the expected
ordinary Loom native-app App.svelte guard failure were not replayed in this
continuation. The historical receipts above remain historical. PR #73 remains
draft and no promotion or product-acceptance claim is made.
