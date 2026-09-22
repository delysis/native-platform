# Completion hydration/admission continuation

Base: `ddc5c7679f6a44bdb4c6d0e3602f8192b9326261`.
Base tree: `7d76c7d40c9fd7d267433151db8cc79b46fd00e5`.
Native product acceptance is **OPEN**. Final Mac qualification and promotion belong to the local agent.

Read the original root `handoff.md` for the complete prior record and approved model.
This continuation supersedes the one-word-preview premise in the older PR #75
observation document. The user explicitly requires full streaming previews.

## What is proved, and what is not

The old smoke's raw store, run IDs and AX logs were not available to the browser-chat
runtime. The supplied path and Library searches did not expose those bytes. The
reported eight runs and stage counts are inherited observations, not a replay here.
Do not claim that the specific second family has been attributed yet.

Three independent source defects are repaired:

1. `refreshBranchesFor` publishes ready branch summaries before awaiting immutable
   body hydration, retaining the old bounded live partials during that transition.
   The family evaluator treated a nonempty but unusable partial as a final rejection.
   Its exhausted disposition could spend the existing one-retry budget and request
   four replacement runs before the candidate body had been verified. The evaluator
   now records pending body authority independently of live-text usability. A complete
   valid live family remains visible; an incomplete one waits. SHA-verified unusable
   output still receives the existing bounded recovery behavior. No budget, timer,
   generation guard or model was changed.
2. The native observer searched for the exact label `Manuscript editor`. The real App
   supplies `Untitled, manuscript editor` for this smoke's fixed `Untitled.md` document.
   It now requires one uniquely named AX text area, using title/description rather
   than editor contents as its label. Different or ambiguous editors remain rejected.
   The observation records the last witness before editor lookup and bounded observed
   editor labels, so a selector failure no longer hides the family state. The helper
   remains specific to the initial Untitled Visual smoke; it is not a generic Source
   or arbitrary-document acceptance driver.
3. PR #75 retained a one-word renderer and allowed observed bytes shorter than the
   selected preview. Those contradict the user requirement. The shared preview now
   preserves the complete authorized editor-safe text and the observer requires equal
   byte counts. Exact DOM text/key, ancestry, geometry, focus, scope, durable-event
   correlation and post-observation terminal checks remain required.

Compatible full-streaming changes from PR #76 were reconciled by their exact file
preimages, not by a wholesale branch merge: immutable-prefix growth after acceptance,
Loompad full-tail display, the live-session browser harness, full-text expectations
and the existing streaming regressions. No Rust, App.svelte, Tauri schema, dependency
lock, model policy, user document, database or historical receipt is modified.

Eight runs alone do not distinguish concurrent double admission from the product's
existing bounded replacement of an unusable family. The fix proves that hydration
pending is not grounds for replacement; the old native trace must still establish
which situation actually occurred. Do not increase the four-run guard to make it pass.

## First local action: preserve and inspect the failed run

Use the original failed smoke, before running another candidate:

```sh
set -eu
smoke=/var/folders/t0/4s921_v11fv9vlymtx6g5qgm0000gn/T/delysis-loom-smoke.XXXXXX.99o0hBJfyk
test -d "$smoke"
db=$(find "$smoke" -type f -name loom.sqlite3)
test -f "$db" # Fails rather than choosing arbitrarily when zero/multiple stores exist.
out=$(mktemp -d -t loom-admission-evidence.XXXXXX)
sqlite3 -readonly "$db" ".backup '$out/loom.sqlite3'"
sqlite3 -readonly "$out/loom.sqlite3" \
  < products/loom/docs/completion-trace.sql > "$out/completion-trace.json"
for name in launch-1-live-stream-diagnostics.json launch-1-live-stream-monitor.failure.json \
  launch-1-generation-family-guard.failure.json launch-1-generation-family-guard.stderr.log \
  launch-1.stderr.log; do
  test ! -f "$smoke/$name" || cp -p "$smoke/$name" "$out/$name"
done
printf 'Preserved diagnostic evidence: %s\n' "$out"
```

The query is a diagnostic, not a gate. It preserves command IDs, request fingerprints,
run/branch IDs, admission order, revision/blob/target, terminal times and status,
candidate blob identities and raw event payloads. It reports explicit truncation
above 32 runs or 4096 events per run. Never label a truncated result complete. Keep
native manuscript/model text in the local evidence package, not in Git history.

For the two families, compare command identity, document/revision/target/model,
first/last delta times, terminal times and candidate output. Determine whether the
second admission overlaps active first-family work, follows a failed/cancelled
family, or occurs after summaries become terminal but before renderer verification.
Check the raw fourth stream and all four terminal bodies, not merely their count.
The database alone cannot establish renderer hydration timing: correlate the AX/DOM
failure record and app logs, and say when that timing was not recorded. A real
all-terminal unpresentable family can legitimately exhaust; that is different from
this reproduced pending-hydration race.

## Focused local qualification

Fetch the pushed continuation into a clean isolated worktree. Do not reset user work,
reapply the old patch, or merge PR #75/#76 wholesale. This branch already reconciles
the selected compatible streaming changes while retaining PR #75 observer changes.

```sh
pnpm --filter @delysis/loom exec vitest run --maxWorkers=4 \
  src/lib/completionHydration.test.ts src/lib/inlineSuggestionFamily.test.ts \
  src/lib/inlineGhostObservation.test.ts src/lib/ghostText.test.ts \
  src/lib/completionStreaming.test.ts src/lib/completionController.test.ts \
  src/lib/completionSession.test.ts
pnpm --filter @delysis/loom check
pnpm --filter @delysis/loom exec vitest run --config vitest.browser.config.ts \
  src/lib/appCompletion.browser.test.ts src/lib/ghostObservation.browser.test.ts \
  src/lib/completionController.browser.test.ts src/lib/editorInteractions.browser.test.ts \
  src/lib/loompadStreaming.browser.test.ts src/lib/Loompad.browser.test.ts
helpers=$(mktemp -d -t loom-observers.XXXXXX)
rustup run 1.92.0 cargo run --offline --locked -p xtask -- macos-smoke-support "$helpers"
"$helpers/start_loom_live_streaming_monitor" --self-test
git diff --check
```

The real-App test injects only native transport. `weave_status` now returns its real
`WeaveStarted` shape instead of a completion snapshot. The added terminal transition
holds the fourth live partial unusable and delays real SHA body verification. It
requires pending hydration, then full DOM output and no second admission. The
Source-mode unit test separately executes the production retry planner while the
body remains pending; no sleep shorter than the retry timer is used as evidence.

If any focused check fails, fix the actual failing boundary and keep the counterexample.
Do not substitute the portable adapters for the pinned suites. Once settled, run the
one consolidated pinned gate from root `handoff.md`, including separate doctests and
all affected frontend/browser checks, then one clean macOS release candidate.

Run the unchanged real-model smoke against the new exact artifact, followed by BOTH
full Visual and Source journeys: pre-terminal multi-frame Ghost, stable distinct
four-choice Loompad, cycling with unchanged manuscript/run count, exact insertion,
separate completion unconsume and ordinary undo, stale-scope invalidation, active-work
joined quit, preference-preserving relaunch and exact persisted bytes. The observer
is still Visual-only: its pass cannot certify omitted Source/Loompad journey steps.
Record executable/archive/model hashes and toolchain/SDK/backend/signing identity.
A failing or missing native journey blocks promotion. The local pinned gate remains
promotion authority while GitHub billing prevents Actions from executing.

## Executed here

Linux Node 22.16.0 and global TypeScript were used, not the repository's pinned frontend
installation. The checked-in TypeScript test registration was mapped to `node:test`;
production functions and assertions were unchanged. Source-mode hydration tests use
throwing sentinels for unavailable Visual/ProseMirror imports; no Visual parser was
executed in that run. All these are component results, never native acceptance.

- Hydration/retry: 7 cases pass. The initial race reproduction failed 3 cases; a
  further truly empty verified-body case also failed before its correction.
- DOM observation: 8 synthetic-geometry cases pass. No WebKit layout is certified.
- Existing immutable-prefix streaming controller: 12 cases pass.
- Compiled portable Swift observer: 38 assertions pass. AppKit/AX code was not compiled.
- Five deliberate mutations are detected: premature retry, endless hydration, trusting
  a different blob proof, accepting a truncated native preview, and the retired name.
- Pure observer strict TypeScript check and modified TS/Svelte-script syntax pass.
- macOS-target Swift syntax parsing passes, not framework typechecking/linking.
- Diagnostic SQL compiles against the product schema, executes on an empty read-only
  store, rejects a write, and leaves its database bytes unchanged. No real runs were
  inspected by that query here.
- `rustup run 1.92.0 cargo --version` is unavailable here (`rustup` not installed).

Pinned Vitest, Svelte compilation, WebKit, Rust workspace checks, native helper linking,
release building, model execution and full product acceptance remain with the local
agent. The delivery bundle contains raw logs and exact before/after source hashes.

## Remaining geometry audit note

A synthetic probe also demonstrates that PR #75's shared observer uses the union
rectangle rather than the first wrapped fragment. The caret itself must still be in
view. That separate geometry change is not included in this narrow admission/name
repair and must not be claimed closed. Reproduce it in real WebKit before changing
visibility semantics; preserve exact-text and offscreen-caret checks. Its exploratory
counterexample is retained separately in the delivery bundle, not counted as a passed
regression or a native failure.
