# Ghost rendering and acceptance contract repair

Base: `87939f3873977ef2670ddab9e869ca9d5d7d1739`  
Base tree: `7ae2286a855305b086b3d893b5c63e7613491a6c`  
Status: implementation for local qualification; **native product acceptance OPEN**.

## What the reported failure actually established

The retained run reported four completed candidates, 192 text-delta events and
654 unsuccessful accessibility observations. That is not sufficient evidence
that the editor painted no ghost. The old live observer compared the entire
candidate byte count with a suffix of the textarea's accessibility value.
The editor intentionally paints only the next word and marks that decoration
`aria-hidden`. Either difference can reject a correctly rendered suggestion.
The same disagreement existed in the idle/resume observer. The live observer
also stopped at terminal completion without recording whether terminal body
hydration eventually produced a visible suggestion.

This repair does **not** infer that native rendering was healthy. It removes
a demonstrated false-negative contract and supplies the missing observations.
The next native run must distinguish missing rendering from late rendering.
Neither model initialization nor a terminal candidate can substitute for it.

## Implementation

`inlineGhostObservation.ts` owns the shared one-word preview and DOM proof.
Both Visual and Source require exact actual text, matching DOM identity,
connected nodes, positive rendered area, an onscreen insertion boundary, and
visible ancestors through the containing app. A matching key on a blank,
substituted, zero-area or hidden widget no longer authorizes acceptance.
Source retains its independent mirror/insertion-edge geometry and its own
accepted separator bytes. Option uses the same visibility rule; the retired
exception allowing an invisible fan anchor is removed.

The Visual accessibility witness includes `visual.inline`, read from that DOM
proof. Layout/scroll reporting updates it even when the candidate key did not
change. This is DOM-derived evidence, not a candidate-buffer or ready-label
claim; it is not a screenshot or a claim against a compromised renderer.

The native live observer correlates that actual word with durable text-delta
bytes, the selected run, its exact family, the native editor/caret, and terminals
read **after** observation. AXValue may contain only the manuscript because the
decoration is accessibility-hidden. No arbitrary suffix is accepted. After all
runs terminate, five seconds are reserved only for diagnostic observation:
a terminal-only render still fails the pre-terminal gate.

Idle/resume reuses the same native observer in an explicitly separate
`--terminal-snapshot` mode. It retains the 75-second hidden/Finder interval,
durable candidate identity checks and no-new-generation guard. Its owned
observer processes are joined; it never kills the app. Failure logs are kept
per observation attempt, not overwritten by the next attempt.

The interaction runner uses existing Cmd+Shift+J/G shortcuts instead of
searching for retired Shuttle/autocomplete buttons. Option cycling, Shuttle,
Return, Tab and completion unconsume checks remain. Expected inserted bytes
are now read from SHA-verified immutable candidate blobs and compared exactly;
same-length substituted output cannot pass. This helper's root-level isolated
manuscript and trailing-whitespace baseline are checked, not guessed.
The controls-only helper likewise keeps the autocomplete-off/Shuttle
independence check but uses the current control and shortcut contracts.

## Regression boundaries and executed checks

- Eight DOM-observation unit cases executed with the same production source and
  assertion bodies, using Node's test scheduler in place of unavailable Vitest.
  Global TypeScript 5.8.3 was used, **not** the repository's pinned toolchain.
- The real Swift observer's portable `--self-test` executes 31 assertions; the
  insertion predicate's self-test executes nine. Swift 6.2.1 on Linux was used.
- Six negative controls reject substituted glyphs, zero-width glyphs, hidden
  containing apps, full-buffer/word confusion, forced AX inclusion, and
  length-only insertion validation. Each mutant compiled and failed its test.
- macOS-target Swift parsing checks syntax only. macOS frameworks were not
  compiled or executed here. Rust/rustup and the pinned frontend dependencies
  are unavailable in this execution host.

New browser coverage mounts the real editors and corrupts actual DOM glyphs.
`appCompletion.browser.test.ts` additionally mounts **App**, mocking only its
external Tauri transport. It exercises admission, staggered four-way snapshots,
wake-only events, actual DOM rendering, cycling, and delayed SHA-verified
terminal hydration. Unexpected native commands fail the test. These browser
tests are committed but **not executed here**. Their fixture outputs never
establish native model acceptance.

Existing unit/browser globs run these regressions; `xtask macos-smoke-support`
also executes both new Swift self-tests after compilation. No additional CI
service, redundant full build, source-string-only product gate, or receipt
rewriting is introduced.

## Local agent: execute in this order

1. Fetch this PR into a separate clean worktree. Preserve unrelated work. Read
   `handoff.md` for the settled gate and approved model identity. Do not reuse
   a prior artifact: old binaries lack the new DOM witness and must fail closed.
2. From `products/loom/apps/loom`, use the installed lockfile dependencies:

   ```sh
   npm run test -- src/lib/inlineGhostObservation.test.ts src/lib/ghostText.test.ts src/lib/sourceGhostText.test.ts
   npm run test:browser -- src/lib/ghostObservation.browser.test.ts src/lib/appCompletion.browser.test.ts
   npm run check
   ```

   Fix compilation/fixture errors without weakening assertions. The App test
   must not be replaced with a controller-only harness or direct ghost props.
3. From the repository root, compile the actual native observers:

   ```sh
   helpers=$(mktemp -d -t loom-ghost-observers)
   rustup run 1.92.0 cargo run --locked -p xtask -- macos-smoke-support "$helpers"
   ```

   This also runs observer/control/insertion self-tests. It is not product
   acceptance. Keep the exact compile log and toolchain identity.
4. Once edits settle, run the single consolidated local gate in `handoff.md`.
   Build one clean macOS candidate with `scripts/release-macos.sh loom candidate`.
   Use its actual printed archive path and adjacent release receipt; verify
   commit/tree and executable/archive hashes against this exact checkout.
   Every Rust command must use `rustup run 1.92.0 cargo ...`; do not substitute
   Homebrew cargo or rely on `RUSTUP_TOOLCHAIN` alone.
5. Run `scripts/smoke-macos-app.sh loom <new-archive> <new-receipt>` with
   `DELYSIS_ACCEPTANCE_SOURCE_SHA` set to the full clean HEAD and
   `LOOM_SMOKE_GGUF_MODEL_PATH` set to the approved Gemma path in `handoff.md`.
   Verify size `4954576032` and SHA-256
   `aa0a9a03993440f45176f19f8189a2e84c210ff8628ec13dc6edf42d017f7670`.
   Preserve the entire new smoke directory before cleanup.
6. Complete the approved **Visual and Source** journey on that same artifact:
   real in-caret Ghost, unchanged manuscript/run counts while cycling, four
   distinct stable Loompad W/A/S/D choices, exact insertion bytes, completion
   unconsume separately from ordinary editor undo, stale-scope invalidation,
   quit during active work/owned-worker joins, and same-artifact/project relaunch
   with exact persisted bytes and fresh native completion. A passing Visual
   observer or the old smoke script alone does not certify omitted journey steps.
7. Only the local agent may promote after that exact native qualification.
   Verify final main has the tested tree. Record raw logs, commit/tree, artifact,
   executable/model hashes, SDK/toolchain/backend/signing identity and blockers.
   Keep hosted credentials, microphones and other unexercised authorities
   explicitly separate from these results.

## Diagnose the first failed boundary, not the timeout

The live failure JSON now records `rejected_stages`, `last_witness`, exact run
IDs and `post_terminal_observation`. Use the first persistent boundary:

| Boundary | Next inspection |
|---|---|
| `family_pending` | Native admission/snapshot/store publication; do not start with CSS. |
| `witness_missing_or_ambiguous` | Correct artifact, renderer mount and AX note extraction. |
| `projection_controller` / `projection_family` | Active policy, current scope and four-way publication. |
| `projection_identity` / `projection_glyph` | App-to-editor binding and actual DOM word/key/geometry. |
| `editor_missing` / `editor_identity_or_caret` | Native focus, correct named surface, exact manuscript/caret. |
| `event_prefix_mismatch` | Durable run/sequence versus the actual displayed projection. |
| `render_observed_only_after_terminal` | Streaming publication/hydration latency; still FAIL. |
| `no_correlated_inline_render` | Inspect recorded projection state; never replace it with a ready flag. |

Do not extend waits, inject fixture completions, change the approved model,
reset the project, or remove a predicate merely to obtain a green receipt.
A red negative control must correspond to the repaired defect, not a syntax
error, missing import, failed build or deliberately unavailable service.
