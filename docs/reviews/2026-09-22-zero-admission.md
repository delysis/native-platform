# Zero-admission and acceptance-store continuation

Base: `050672127b735df0fba6c0a55dd09de23dad79e8`
Base tree: `f244612be52e12cd3b31dae126cd4dfbc2182769`
Native acceptance: **BLOCKED**. This is a narrow source repair, not a native receipt.

Read the existing root `handoff.md` for the failed artifact and preserved evidence.
The browser-chat execution host could not access that Mac directory or screenshot.
It did not inspect their raw contents or reproduce the particular native incident.
Do not rewrite their receipts or attribute the old run to a cause not recorded there.

## Defects and changes

### Explicit enabling reads stale derived policy

`setSuggestionsEnabled` sets `suggestionsEnabled`, then synchronously calls model
preparation or `scheduleAutomaticSuggestions`. Both read the legacy reactive
`completionAutomationActive`. Before Svelte applies the assignment, that flag can
still be false. `armSuggestionSchedule` clears the schedule and returns. No schedule
remains for the lifecycle wake mechanism to resume. With an already-loaded writer,
there need not be a later `model_ready` transition to rescue the lost request.

The method now awaits Svelte's `tick()` after the native acknowledgement and local
policy update, then revalidates component lifetime, application phase and project/
session before requesting work. This is a reactive-state barrier, not a new timer,
retry or extended deadline. Repeating an already-enabled policy does not mint work.

The existing real-App WebKit test adds the missing warm path: finish a family,
disable suggestions, then enable again with the SAME loaded writer and unchanged
manuscript. It requires exactly one new explicit admission, a new run identity and
full rendered streaming text. Unexpected model loads, writes or hosted requests fail.
The existing delayed-hydration cases remain intact. This new browser case is unrun
in the authoring environment; it must be exercised locally.

### Window configuration did not reach the WebKit store

The previous isolation test asserted `WindowConfig.data_store_identifier`. Pinned
`tauri-runtime` 2.11.3's `From<&WindowConfig> for WebviewAttributes` never copies this
field; `WebviewBuilder::from_config` uses that conversion. A passing configuration
assertion therefore said nothing about actual WebKit isolation. Shared default
localStorage can retain navigation entries from distinct smoke directories.

Acceptance startup now defers only windows with `create: true`, then builds those
same configurations exactly once through `WebviewWindowBuilder::from_config(...)
.data_store_identifier(identifier).build()`. Normal startup is unchanged. Templates
with `create: false` remain uncreated. The existing directory-derived stable ID is
unchanged; the store is persistent, not incognito. No history filtering/deletion,
filesystem move, preference migration, dependency update or key store is introduced.

The Rust tests now describe what they actually check: window-creation ownership,
configuration preservation and application identity. They do NOT claim to prove
WebKit storage. The native A/B/A check below must verify that boundary. The explicit
store API requires macOS 14+; the target is macOS 15.6. Unsupported OS versions must
be recorded BLOCKED, not treated as isolated by a configuration test.

Previously polluted normal-profile history is not silently erased. New isolated
runs must not read it; existing writing and historical evidence stay untouched.

### The zero-run observer skipped its own diagnostics

The old observer continued at `family_pending` before walking AX or reading the
completion witness. With zero runs, its empty witness/editor records were inevitable,
not observations of the renderer. It now collects the same bounded AX diagnostics
and restores the unique editor's focus BEFORE waiting for the family. Zero runs
still cannot pass. All family, exact DOM bytes/key, scope, caret, durable-delta and
post-observation terminal predicates and all deadlines remain unchanged.

App adds `pre_admission` to the existing diagnostic witness. It reports exact
project/session/document/revision/blob identity, root, current context, lifecycle
reason, generation intent, schedule kind, edit/save versions, caret, surface/focus
and navigation count. These are direct projections of current state, never new
admission authority or a substitute for visible native completion.

## Source references

- Product sources at the exact base: `src/App.svelte` under
  `products/loom/apps/loom`, `src-tauri/src/lib.rs`, and the existing observer in
  `scripts/macos-smoke-support/start_loom_live_streaming_monitor.swift`.
- [Pinned conversion](https://github.com/tauri-apps/tauri/blob/tauri-runtime-v2.11.3/crates/tauri-runtime/src/webview.rs#L447-L505):
  Git blob `d7fb9c80f31185670d9f737a58529c37d743512d`.
- [Pinned WebviewBuilder conversion](https://github.com/tauri-apps/tauri/blob/tauri-v2.11.5/crates/tauri/src/webview/mod.rs#L391-L436):
  Git blob `b89d3c95d0e841992542afb97b8c5c9deb87b720`.
- [Explicit builder setter](https://github.com/tauri-apps/tauri/blob/tauri-v2.11.5/crates/tauri/src/webview/webview_window.rs#L1130-L1142):
  Git blob `c61b96e60307ca4e3038db3f5b26a1adf00cf77f`.
- [Svelte tick contract](https://svelte.dev/docs/svelte/lifecycle-hooks#tick).

## Executed here / not executed

A replay extracts the actual App scheduling and writer-request functions plus the
production controller/intent functions. It explicitly SIMULATES the reactive flush;
it is not a Svelte, WebKit, native transport or inference run. Eight cases pass after
the edit; three fail on the old code. Removing the flush, removing post-flush scope
checks, or rearming an already-enabled policy each fails its intended assertions.

The unchanged portable Swift observer contract passes 38 assertions. macOS-target
Swift parsing passes, but Apple-framework typechecking/execution is unavailable.
The safe worktree application utility passes nine temporary-repository tests.
TypeScript/Svelte-script syntax and exact patch pre/postimage checks are recorded
in the accompanying package. None of these is native product acceptance.

Rust, rustfmt, pinned pnpm/Svelte/Vitest/WebKit and the target Mac are unavailable
here. Rust window tests and the modified real-App browser case are UNRUN. Previous
local gate successes remain scoped to the previous tree.

## Local agent: one continuation, no branch mixing

Apply the accompanying exact-base patch in the fresh worktree prepared by its
`apply.mjs`; it verifies the base commit/tree and every changed-file hash. It does
not commit, push, reset, merge, touch unrelated work or claim acceptance. Do not
reapply earlier packets or merge the old PR #75/#76 branches wholesale.

First preserve the original failed smoke directory, including its database/WAL,
logs, failure JSON and screenshot, before another launch. For a stopped app a
read-only SQLite backup is preferable to copying an open database without its WAL.
The source replay does not retroactively supply missing native admission state.

Run the focused checks before building another artifact:

```sh
pnpm --filter @delysis/loom check
pnpm --filter @delysis/loom exec vitest run --config vitest.browser.config.ts \
  src/lib/appCompletion.browser.test.ts
rustup run 1.92.0 cargo fmt --all -- --check
rustup run 1.92.0 cargo test --locked -p loom-app
rustup run 1.92.0 cargo clippy --locked -p loom-app --all-targets -- -D warnings
helpers=$(mktemp -d -t loom-zero-admission-helpers.XXXXXX)
rustup run 1.92.0 cargo run --locked -p xtask -- macos-smoke-support "$helpers"
```

If these expose an integration error, repair it narrowly; do not change the test to
accept an absent family, one-word preview, wrong root, hidden widget or extra run.
Then run the consolidated pinned local gate from `handoff.md` ONCE on the settled
revision, commit it, and build one clean identified candidate. Record the final
tree and executable/archive hashes; do not label the packet's partial checkout a
full repository tree.

### Native store check: same artifact, A/B/A, no inference claim

Use macOS 14+ and two NEW acceptance directories A and B. Run only one instance of
the bundle at a time, with `DELYSIS_LOOM_ACCEPTANCE_DIR` set before launch. Retain all
state; do not clear localStorage or sanitize the directory list.

A1: require the actual writing root and visible root list to contain A only. Disable
suggestions through the normal Cmd+Shift+G policy control, read the control state,
and quit normally. B1: require B only, with no A or previous smoke/normal-profile
roots; quit normally. A2: require A's exact project/manuscript and disabled preference
to survive relaunch, without B's navigation history. Compare actual visible entries
with `pre_admission.project_root` and `workspace_folder_count`; metadata alone is
not a store or visible-product acceptance receipt. Preserve actual observations.
Do not run a separate Swift WebKit fixture and call it the product binding.

### Native generation check

Run the existing exact-archive smoke with the approved model, unchanged:

```sh
DELYSIS_ACCEPTANCE_SOURCE_SHA=$(git rev-parse HEAD) \
LOOM_SMOKE_GGUF_MODEL_PATH=/Users/george/.cache/fiction-harness/models/gemma-4-E2B-base-Q8_0.gguf \
./scripts/smoke-macos-app.sh loom "$archive" "$evidence/smoke.json"
```

`archive` must be the new clean release's `Loom.app.zip`, with its adjacent receipt;
`evidence` is a new retained output directory. Model size: `4954576032`. Model SHA:
`aa0a9a03993440f45176f19f8189a2e84c210ff8628ec13dc6edf42d017f7670`.

If it still has zero runs, inspect `last_witness.pre_admission` immediately. An
unarmed intent, missing schedule, model-unavailable state, stale save/blob, unmapped
caret, hidden surface, or wrong root is now distinguishable. Missing/unreadable
witness remains BLOCKED; do not call it ready. If it admits more than four, preserve
the original guard failure and trace both commands. Do not raise its budget.

Only real, correlated pre-terminal multi-frame output permits continuation through
the full Visual AND Source journeys: stable distinct Loompad W/A/S/D, no manuscript
mutation or hidden work while cycling, exact insertion, separate unconsume/ordinary
undo, stale-scope invalidation, active-work joined quit, and exact same-artifact
persisted-content/preference relaunch with fresh native completion. Promotion
requires that exact tested tree and all required native receipts, not this packet.
