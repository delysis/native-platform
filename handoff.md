# Current entry point

Start with the [24 September integration map](docs/reviews/2026-09-24-integration-map.md)
and [development workflow](docs/browser-development.md). The continuation below
is retained as historical evidence; do not reset to its older commits or reapply
its already-integrated patches. Paid GitHub Actions are unavailable: use local
source-bound validation without claiming unexecuted native or platform checks.

# Native-platform audit remediation handoff

Date: 2026-09-22  
Repository: `delysis/native-platform`  
The genuine implementation stack has been promoted to `main`; do not claim Ghost/Loompad product acceptance until the remaining presentation failure is repaired and exercised in the exact macOS artifact.

## Promotion status

PRs #64, #66, #67, #68, #69, #70, #71, and #72 were merged in order using the green local gate as authority while GitHub Actions were payment-blocked. Final promoted main commit: `f38a842d8ddea0a97a69b2f3d3efbd5d39ffa27d`. Final main tree: `412fee9338828f9a179ab0275039511318257dd0`, exactly matching the tested and reviewed integration tree.

## Mac continuation after Astra's `b73e2e22`

Astra's implementation and receipt commits were fast-forwarded and independently exercised on the target Mac. The genuine navigation and current-control changes are retained. Focused current-tree checks passed: 507 frontend unit tests, Svelte check with zero diagnostics, 21 Swift completion-control contract assertions across 18 compiled helpers, and pinned Rust 1.92 `loom-app` tests plus strict Clippy.

One acceptance-only regression was found during integration: Astra selected an incognito WebView to isolate acceptance localStorage, but a nonpersistent store cannot prove the required preference-preserving relaunch. The follow-up replaces it with a stable, directory-scoped custom WebKit data-store identifier. Normal launches continue to use the default store; separate acceptance directories cannot share navigation preferences; an acceptance relaunch can retain them. This needs confirmation in the exact packaged two-launch journey.

The serious remaining work is intentionally narrow:

1. `scripts/macos-smoke-support/exercise_loom_completion_controls.swift` still encodes retired autocomplete/Shuttle controls. Rewrite it around the exact `Ghost text`/`Loompad` control and Cmd+Shift+G policy shortcut, or retire the obsolete no-model assertion if its ownership is fully covered elsewhere.
2. Build one clean candidate from the settled tree and run the real-model journey. Prior evidence reached Metal initialization but created zero `generation_runs`; diagnose writer load -> admission -> event -> presentation from the preserved logs and fresh bounded diagnostics.
3. Require actual Ghost and four-word Loompad rendering in both Visual and Source, exact accept/unconsume/ordinary undo, stale invalidation, joined active-work quit, and exact-content relaunch. If native generation still stalls, preserve model/backend/application logs plus database and accessibility state and hand that bounded failure to Pro; do not redesign unrelated systems.
4. Use local CI as promotion authority while GitHub Actions are payment-blocked. Iterate with focused tests, then run one consolidated pinned 1.92 gate asynchronously. Promote the stack only from the exact tested tree.

### Exact real-model failure after the Mac continuation

Commit `4299647a24b39ae6f1100e9753db50fc6f156815` built a clean, ad-hoc-signed candidate and passed the complete local gate inherited from `24682e62` plus the focused post-gate workflow/shell checks. The approved Gemma model loaded on Metal and the repaired control automation proved `Ghost text`, actionable and suggestions enabled. The exact family created four runs, 192 `text_delta` events, four nonempty candidates, and four `completed` terminals with no generation-guard or project-busy failure.

The product journey still failed: after 654 accessibility polls, the monitor reported `family_terminal_before_live_witness`. No correlated pre-terminal WYSIWYG ghost was observed before all four runs became terminal. This is now a presentation/observation problem, not the earlier selector or writer-load failure.

Preserved evidence:

- local gate: `/tmp/native-platform-local-ci-24682e62-v2/` (`FAILURES=0`)
- release: `/tmp/native-platform-4299647a-acceptance/release-loom-candidate.log`
- smoke: `/tmp/native-platform-4299647a-acceptance/smoke-real-model.log`
- retained isolated run: `/var/folders/t0/4s921_v11fv9vlymtx6g5qgm0000gn/T/delysis-loom-smoke.XXXXXX.0mtLzZBF7E`
- failure receipt: `launch-1-live-stream-monitor.failure.json`
- full diagnostic: `launch-1-live-stream-diagnostics.json`

For Pro: first determine whether the terminal ghost actually renders after hydration and the monitor exits too early, or whether `InlineFamilyEvaluation` never publishes it. Add a bounded post-terminal accessibility/DOM observation before changing product behavior. If the final ghost is absent, trace the exact four run IDs from generation event -> terminal candidate -> durable hydration -> family evaluation -> editor presentation key. If it is present only after terminal, retain that proof and separately decide whether live streaming is a required product contract or an over-strict smoke assertion. Then exercise Ghost and Loompad in both editor modes; do not touch executor/cache/sampler systems without evidence that this presentation failure originates there.

## Current stack

The implementation is a linear, pushed PR stack based on refreshed `origin/main` at `c0bcc42d8e274c9f428d3e19508709d7fa6e23ae`:

| PR | Branch | Tip | Scope |
|---|---|---|---|
| #64 | `maintenance/phil-cache-identity-20260916` | `45b85579` | canonical cache fingerprint prerequisite |
| #66 | `codex/audit-loom-family` | `59c377fe` | G01 unified family evaluation |
| #67 | `codex/audit-loom-distinct-words` | `d7b730a7` | G02 bounded four-word Loompad contract |
| #68 | `codex/audit-native-executor` | `5bdb2cc7` | executor lifetime and cancellation |
| #69 | `codex/audit-cache-linearization` | `27f2180b` | cache ordering, outcomes, common-prefix reuse |
| #70 | `codex/audit-sampling-compatibility` | `53320b94` | sampling validation/transforms and exact template dispatch |
| #71 | `codex/audit-ci-privacy` | `f8588b3e` before this handoff commit | CI selection and material-retention privacy |

The integrated source tree tested before this handoff was:

- commit: `f8588b3eace27e0e81fc41c32e2ac15857abbc6a`
- tree: `0f9ef58b0b81787d4f9a93f89dad73b4051b43c1`

GitHub Actions are failing before product steps because the account has no CI credits. George explicitly authorized local CI as promotion authority. Keep local CI asynchronous; do not wait idly for GitHub.

## Immediate blockers found in the exact macOS candidate

The current integrated tree is **not promotable**, regardless of component-test results.

1. The sidebar visibly presents two entries named `writing`: a workspace-root label plus a nested folder row. Determine the exact filesystem/project-root relationship and ensure the UI presents one canonical writing location without ambiguous duplicate labels. The default native root is `app_local_data_root/writing`; current format explicitly permits writing directly at the chosen root. The repository smoke script still assumes the retired `writing/manuscript/Untitled.md` hierarchy, while the current app creates `writing/Untitled.md`.
2. The titlebar offers `Collapse main pane` even when no right-side chat pane exists to occupy the space. Collapsing then leaves a void. In `products/loom/apps/loom/src/App.svelte`, the fallback button is currently rendered under `{#if project && !mainPane}`. It must only be offered when a visible replacement pane can fill the main area; add a browser regression for the no-chat/no-replacement case.
3. The release candidate renders no ghost text in either Ghost mode or Loompad/WASD mode, including while Option is held. This is the motivating feature and a hard acceptance failure. Reproduce through the real native writer, inspect model/admission/event/presentation state, and require an actually rendered ghost or four-word Loompad page—not merely a ready label or controller state.
4. `./scripts/release-macos.sh loom candidate` builds and signs, but refuses its receipt because Tauri regenerates these checked-in files:
   - `products/loom/apps/loom/src-tauri/gen/schemas/acl-manifests.json`
   - `products/loom/apps/loom/src-tauri/gen/schemas/desktop-schema.json`
   - `products/loom/apps/loom/src-tauri/gen/schemas/macOS-schema.json`
   The generated diff is preserved at `/tmp/native-platform-f8588b3e-acceptance/build-generated-schema.diff`. Regenerate/review/commit the authoritative schemas, then prove a clean-tree release build.
5. `scripts/smoke-macos-app.sh` hard-codes `writing/manuscript/Untitled.md` at the journey paths around lines 1191, 1603, and 1624. The current default document is `writing/Untitled.md`. Update all path assumptions and add a source-level or shell regression so the smoke contract follows the actual format.
6. A temporary smoke copy with only the manuscript path corrected reached native model load but spent more than five minutes in stale `set_loom_completion_toggle` automation looking for `Turn autocomplete off/on`; the real control is now labeled `Ghost text`/`Loompad`. It was capped rather than accepted. The retained database contained zero `generation_runs`; the manuscript remained exact `hello `. Repair the helper, then diagnose why the approved E2B load never completes. Preserve this as a failure, not a pass.

User screenshots for blockers 1-3 were supplied in the task and are also available during this session at:

- `/var/folders/t0/4s921_v11fv9vlymtx6g5qgm0000gn/T/codex-clipboard-264836ff-95e3-4ece-b933-0c7231a2374a.png`
- `/var/folders/t0/4s921_v11fv9vlymtx6gqm0000gn/T/codex-clipboard-78c50c17-f3a4-4e99-9683-a0aaa1d4229d.png` (if this path has expired, use the task attachment)

## Local verification already completed

Use the pinned toolchain explicitly. `/opt/homebrew/bin/cargo` is standalone Rust 1.95 and `RUSTUP_TOOLCHAIN=1.92.0` alone is not sufficient. Run commands as `rustup run 1.92.0 cargo ...` or prepend `/Users/george/.rustup/toolchains/1.92.0-aarch64-apple-darwin/bin` to `PATH`.

Raw local-CI logs: `/tmp/native-platform-final-ci.5Riovl/`.

Passed on exact `f8588b3e`:

- toolchain identity: rustc/cargo 1.92.0
- policy/workflow Node suites: 134/134
- current-doc tests: 4/4 and validator
- no-Python enforcement: 6/6
- `xtask policy`
- `cargo fmt --all -- --check`
- release/smoke shell syntax and macOS smoke-helper compilation
- full workspace all-target test: 1,895 passed, 0 failed, 44 ignored, 69 suites
- separate full-workspace doctests
- ignored-test inventory

Strict workspace all-target Clippy and the native architecture check also passed. The runner then entered the redundant focused native-test rerun and was intentionally interrupted for this handoff. Do not count that interrupted rerun separately; the native packages were already exercised by the passing workspace all-target run and their doctests by the passing workspace doctest run. Read `/tmp/native-platform-final-ci.5Riovl/results.tsv` and the individual logs for the exact recorded gate set.

The workspace run confirms the focused preserved contracts for H01-H07 and V06, including hosted redirect refusal, Mom ownership/atomic storage, terminal-before-release ordering, current FTE no-migration database behavior, controlled speech cancellation/revocation, WAL retry, private loopback-token directories, import cancellation, and schema preservation. Boundaries remain:

- local fixtures, not live hosted credentials;
- injected microphone implementations, not a physical microphone;
- the old FTE v1 upgrade was intentionally retired by `6d49d148`; current behavior rejects prior schemas without modifying the database.

## Artifact/model evidence

Release/acceptance logs: `/tmp/native-platform-f8588b3e-acceptance/`.

- approved writer: `/Users/george/.cache/fiction-harness/models/gemma-4-E2B-base-Q8_0.gguf`
- model size: `4954576032`
- model SHA-256: `aa0a9a03993440f45176f19f8189a2e84c210ff8628ec13dc6edf42d017f7670`
- model identity matches `writer-gemma4-base-v2.json`
- macOS accessibility permission was available
- environment: macOS 15.6 arm64, Xcode/SDK 26.2
- Metal initialized, which proves backend initialization only—not a completed writer or generation run
- candidate build reached compile/sign, but the repository release gate failed on generated-schema dirt
- current product acceptance is failed/blocked by the six items above; do not label it accepted

Key files:

- `/tmp/native-platform-f8588b3e-acceptance/release-loom-candidate.log`
- `/tmp/native-platform-f8588b3e-acceptance/build-generated-schema.diff`
- `/tmp/native-platform-f8588b3e-acceptance/smoke-current-layout.diff`
- `/tmp/native-platform-f8588b3e-acceptance/smoke-current-layout.log`
- `/tmp/native-platform-f8588b3e-acceptance/smoke-loom-real-model.log`

## Important implementation boundaries

- Do not merge PR #59 or `codex/loom-distinct-words` wholesale. Only the audit-scoped word-choice work was ported.
- R06 was intentionally not mechanically consolidated: ticket ownership, authority, and error semantics differ.
- Cross-process cache guarantees remain owned by and tested through the injected product store.
- Protected material retention fails before plaintext binding/evidence/temp publication because no encrypted storage boundary exists. Ordinary material retains the documented Unix-permission contract.
- No hosted inference fallback, project reset, historical-receipt rewrite, new key store, or silent database migration.
- Preserve exact acceptance/unconsume/ordinary-undo distinctions and frozen family authority.
- Loompad intentionally suppresses inline ghost text, but must show the complete four-choice overlay; Ghost mode must show true in-caret ghost text.

## Recommended continuation

1. Let the baseline local gate finish and save its final results, but do not promote `f8588b3e`.
2. Work in a fresh branch/worktree based on `codex/audit-ci-privacy`; do not overwrite the generated schema diff before reviewing it.
3. Add negative controls first for the duplicate-root presentation, no-replacement main-pane collapse control, current smoke path, and real completion visibility state.
4. Fix the release/schema and smoke-path failures, then reproduce the missing completion through the exact native event chain. Do not paper over it with fixture text.
5. Run focused frontend/browser tests and native tests, then one consolidated local gate on the new exact clean tip.
6. Build a new identified candidate and run both Visual and Source journeys with the approved model: rendered ghost, cycling, four stable distinct Loompad words, exact accept/unconsume/undo, stale invalidation, graceful active-work quit, and exact-content relaunch.
7. Only after that passes, rebase each stacked PR onto the preceding merged revision and admin-merge #64, #66, #67, #68, #69, #70, #71 in order. Verify the final `origin/main` tree matches the tested tree.
8. Produce a finding-to-evidence receipt for all 37 entries F01-F14, G01-G02, R01-R06, V01-V08, H01-H07. Mark missing external authority blocked, never passed.

## Working-tree warning

The active worktree may show the three generated Tauri schema files as modified while builds run. They are build-generated evidence from the release defect, not unrelated user edits. Do not discard them blindly. Review against `/tmp/native-platform-f8588b3e-acceptance/build-generated-schema.diff`, then either commit the authoritative generated output with its source change or regenerate it deterministically on the repair branch.

## Astra continuation: PR #75 qualification state (2026-09-22)

This section records the latest exact qualification state. It supersedes no
acceptance requirement above; native product acceptance remains OPEN.

### Tested source and artifact

- PR #75 source was fetched from GitHub into a clean separate worktree.
- Qualification worktree: `/Users/george/.codex/worktrees/native-platform-pr75-ghost`.
- Original PR tip: `2fb08be8846cc00c3b58d4e3fbefecc37e26352f`.
- Latest local qualification commit: `e98e6cdc57be9e336ece5803282e63db10ee6fb7`.
- Latest local qualification tree: `90feed84a62bd72d5dd318f4960ff191604964c3`.
- The local commit only repairs browser-test fixtures exposed by the new
  production identity checks: `onGhostPresentationRejected`, the exact
  `weave-${commandId}` request identity, and the expected `weave_status` read.
- Clean candidate directory:
  `/Users/george/.codex/worktrees/native-platform-pr75-ghost/dist/macos/loom-v0.1.0-e98e6cdc57be-20260922T191416Z`.
- Candidate archive:
  `/Users/george/.codex/worktrees/native-platform-pr75-ghost/dist/macos/loom-v0.1.0-e98e6cdc57be-20260922T191416Z/Loom.app.zip`.
- Candidate receipt:
  `/Users/george/.codex/worktrees/native-platform-pr75-ghost/dist/macos/loom-v0.1.0-e98e6cdc57be-20260922T191416Z/release-receipt.json`.

### Green qualification evidence

- Focused frontend unit suite: 73 tests passed.
- Full frontend unit suite during release: 70 files, 515 tests passed.
- Prescribed browser suite: 19 files, 148 tests passed.
- `svelte-check`: 0 errors, 0 warnings.
- Pinned `rustup run 1.92.0 cargo run --locked -p xtask -- macos-smoke-support`
  passed 9 insertion assertions, 31 live-observer assertions, and compiled
  18 macOS helpers.
- Clean macOS Tauri build and ad-hoc signing completed successfully.
- These are component/build receipts only. They do not establish native
  product acceptance.

### Latest real-model failure and preserved evidence

The exact archive smoke was run with:

- `DELYSIS_ACCEPTANCE_SOURCE_SHA=e98e6cdc57be9e336ece5803282e63db10ee6fb7`
- `LOOM_SMOKE_GGUF_MODEL_PATH=/Users/george/.cache/fiction-harness/models/gemma-4-E2B-base-Q8_0.gguf`

Preserved smoke directory:

`/var/folders/t0/4s921_v11fv9vlymtx6g5qgm0000gn/T/delysis-loom-smoke.XXXXXX.99o0hBJfyk`

The run proves model/backend activity but fails native acceptance:

- Metal initialized.
- `generation_run_count` reached 8; the guard permits at most 4.
- The first four-run family IDs were recorded in
  `launch-1-live-stream-diagnostics.json`.
- A fifth generation was admitted while the first ghost/cache family was in
  use; the guard reports `asynchronous_guard_failed`.
- `last_witness` and `post_terminal_observation` are both `{}`.
- Rejected stages were `family_pending: 325` and `editor_missing: 41`.
- No correlated pre-terminal WYSIWYG ghost was observed.
- No product acceptance, model acceptance, or promotion claim is allowed.

Relevant files in that directory:

- `launch-1-live-stream-diagnostics.json`
- `launch-1-live-stream-monitor.failure.json`
- `launch-1-generation-family-guard.failure.json`
- `launch-1-generation-family-guard.stderr.log`
- `launch-1.stderr.log`

### Unresolved to-dos for Astra

1. Start from the exact latest qualification commit/tree above and inspect
   the preserved smoke directory before editing code.
2. Trace why the app admits a second four-run family before the first family
   has produced a correlated visual/source witness. Establish whether this is
   duplicate automatic scheduling, family teardown/retry, stale scope
   invalidation followed by an unauthorized retry, or another admission path.
   Do not raise the generation guard and do not suppress the guard failure.
3. Correlate each of the eight run IDs through admission, durable event
   publication, terminal candidate, family evaluation, and editor projection.
   Use source revision/blob/cursor identity, not labels or readiness flags.
4. Explain the `editor_missing` observations from the live diagnostics. Verify
   the packaged app is attached to the intended named editor and native caret;
   do not replace this with a controller-only or screenshot-only assertion.
5. Determine whether a ghost is painted after terminal hydration. If yes,
   retain the proof and separately decide whether streaming-before-terminal is
   the intended product contract. If no, fix the actual projection/presentation
   path and preserve the negative controls.
6. After the narrow fix, rerun focused unit/browser tests, pinned observer
   self-tests, and one clean release. Then rerun the exact archive smoke with
   the approved Gemma model and preserve the complete smoke directory.
7. If the smoke reaches a live witness, complete both Visual and Source
   journeys: true in-caret Ghost, four distinct stable Loompad W/A/S/D choices,
   unchanged manuscript while cycling, exact accept/unconsume/ordinary undo,
   stale-scope invalidation, active-work quit with owned-worker joins, and
   same-artifact/project relaunch with exact persisted bytes and fresh native
   completion.
8. Only promote after the exact tested tree passes those journeys and all
   artifact/model/source hashes are recorded. Keep hosted credentials,
   microphones, and unexercised authorities explicitly out of the result.

### Explicit non-goals

- Do not weaken or remove a predicate to obtain a green receipt.
- Do not extend waits as a substitute for diagnosing the duplicate admission.
- Do not inject fixture completions into the native smoke.
- Do not change the approved Gemma model or add hosted inference fallback.

## Pro handoff: latest native smoke remains genuinely broken (2026-09-22)

The local agent stopped after the exact approved-Gemma smoke remained visually
empty and the supplied screenshot showed an ugly repeated list of temporary
`/private/var/folders/.../delysis-loom-...` directories in the app's folder
search UI. This is a bounded failure handoff, not a product acceptance receipt.

### Exact attempted artifact

- Tested commit before the smoke: `034f8f94474d66aa09c027a526c96653b0b75403`.
- Tested tree before the smoke: `4625df45ab85fffc90a5f292b8cd541ccb1c52b5`.
- Candidate:
  `/Users/george/.codex/worktrees/native-platform-pr75-astra-20260922/dist/macos/loom-v0.1.0-034f8f94474d-20260922T203135Z`.
- Smoke evidence:
  `/var/folders/t0/4s921_v11fv9vlymtx6g5qgm0000gn/T/delysis-loom-smoke.XXXXXX.p23c0Dgk0k`.
- User screenshot:
  `/var/folders/t0/4s921_v11fv9vlymtx6g5qgm0000gn/T/codex-clipboard-67bbf759-95c1-4bb1-b8a9-1cae94e661a8.png`.

### Exact result

The run was bounded and stopped at the user's direction; it did not reach the
four-run guard. The monitor observed 2,226 polls with:

- `reason: no_correlated_inline_render`
- `generation_run_count: 0`
- `family_run_ids: []`
- `last_witness: {}`
- `post_terminal_observation: {}`
- `observed_editor_labels: []`
- `rejected_stages: { family_pending: 2226 }`
- control state: `Ghost text`, enabled, suggestions enabled

This latest artifact did not reach durable generation admission before the live
monitor failed. It does not prove the cached model is bad; it does prove the
packaged product path is still not rendering or admitting a completion in this
run. Do not call this a model-loading pass.

### Pro-owned next actions

1. Inspect the preserved smoke directory and application stderr first. Explain
   why the latest run has zero `generation_runs` despite the cached model and an
   enabled `Ghost text` control.
2. Explain the repeated temporary-folder entries visible in the screenshot.
   Determine whether acceptance directories are exposed as project/search
   results, whether launches share or leak the wrong persistent WebKit store, or
   whether this is a separate filesystem-root presentation defect. Fix only with
   direct evidence.
3. Reconcile this zero-admission result with the earlier artifact, which had two
   command families and eight durable runs. Do not infer that the hydration repair
   caused or fixed the old second family without a command/run/event trace.
4. Re-run one bounded exact artifact smoke after diagnosis. Require actual
   pre-terminal multi-frame Ghost and then the full Visual and Source/Loompad
   journeys already specified above.

### Stop conditions

- Do not spend time on broad redesign or old PR rebases in this handoff.
- Do not raise the four-run guard, extend deadlines, inject completions, or use
  hosted fallback.
- Missing evidence is `BLOCKED`, never `PASS`.
- Native product acceptance and promotion remain OPEN.
- Do not reset the project, rewrite historical receipts, or perform a silent
  database migration.
- Do not claim acceptance from the green browser suite, native self-tests,
  Metal initialization, generation-run creation, or a ready label.
