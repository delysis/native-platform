# Loom completion stabilization handoff — 2026-08-22

## Scope and status

This checkpoint is intentionally **not** a completion-UX acceptance receipt. It preserves the
current scheduler/lifecycle repair, stronger macOS smoke assertions, and exact runtime evidence so
the next implementation pass can continue without reconstructing failed attempts.

The packaged macOS app has now visibly rendered streamed ghost text in the visual (WYSIWYG)
editor with the official Gemma 4 12B QAT GGUF and one native four-sequence inference context. The
full Option-Right/Option-Left reversal, four-choice selection, and Shuttle flows remain unaccepted.

## Architecture added in this checkpoint

- `completionLifecycle.ts` replaces a single large readiness boolean with explicit `inactive`,
  `waiting`, and `ready` phases and typed reasons. A scheduled completion survives transient model,
  projection, save, branch, and caret-proof waits; it is dropped only when its intent is inactive.
- Automatic schedules no longer require the model to have finished loading before they can be
  armed. Reactive readiness changes wake the retained schedule.
- Source and visual caret navigation explicitly schedule a replacement batch after the new exact
  boundary settles. A programmatic selection change in an unfocused ProseMirror view no longer
  impersonates user caret navigation.
- The titlebar autocomplete control restores the editor's current selection after it is pressed and
  exposes the exact lifecycle reason through accessibility state.
- Visual Markdown boundary proof now reports a bounded failure (`canonical_mismatch`,
  `selection_not_text`, and so on), plus non-content diagnostics for mismatch byte lengths and the
  first differing UTF-16 position.
- The macOS smoke requires one observed family of exactly four durable generation runs before it
  accepts ghost evidence. It also requires an exact collapsed AX caret at the sentinel end and
  verifies that Option reversal does not admit intermediate inference.

No backend schema or persisted project format changed.

## Exact local evidence

Built bundle:

`target/release/bundle/macos/Loom.app`

The bundle was rebuilt with `tauri build --bundles app`, ad-hoc signed, and passed strict deep
signature verification. It contains no embedded model weights. Runtime model discovery used the
official `google/gemma-4-12B-it-qat-q4_0-gguf` snapshot through an isolated hard link; llama.cpp
reported `n_seqs = 4`.

Observed runs:

1. A clean run durably admitted exactly four generation runs at byte 38, the end of the exact
   untouched sentinel manuscript. The smoke later reported `canonical_mismatch` while waiting for
   a visible ghost. This motivated the bounded proof diagnostics in this checkpoint.
2. The next rebuilt run visibly rendered visual-editor ghost text; the user independently witnessed
   it. Its first family was four runs at byte 38. Before the harness could perform its own reversal,
   the manuscript received a human-classified `space + Tab` edit and a second family was correctly
   scheduled at byte 40. Because that external interaction contaminated the run, it is evidence for
   visual ghost rendering but not for scheduler duplication or Option reversal.
3. A final untouched run was interrupted at the user's handoff request while Gemma was still
   preparing. It is not product evidence and must not be reported as a failure or pass.

The macOS smoke has therefore not yet emitted a passing real-completion receipt.

## Remaining failures and likely next work

1. Re-run the exact packaged smoke without human interaction and capture the enriched caret-proof
   diagnostic if ghost presentation stalls. Do not weaken the exact Markdown/source identity gate.
2. Prove that one completed four-run family remains cached while ghost text is visible. No second
   family may start before candidate exhaustion, a genuine manual edit, or incompatible caret move.
3. Make Option-Right consume one cached word and Option-Left immediately restore the exact prior
   manuscript bytes and ghost remainder, with the generation-run count unchanged.
4. Prove that holding Option keeps all four alternatives visible, Up/Down changes the selected
   candidate, and Return/Tab/Right invoke the documented operation without losing modifier state.
5. Prove Shuttle uses the same cached completion session, hides inline ghost while active, and is
   independent of normal autocomplete.
6. Only after those checks pass should the smoke proceed to formatting, relaunch, and release-gate
   acceptance.

The macOS editor is a Tauri webview. On macOS, Tauri's webview implementation is WKWebView, which
is Apple's WebKit-backed native view. References to WebKit accessibility in the smoke therefore
refer to the accessibility tree exposed by the Tauri WKWebView, not a separate browser window.

## Deterministic gates at handoff

- `svelte-check`: 0 errors, 0 warnings
- Loom unit tests: 41 files, 251 tests passed
- Loom browser tests: 1 file, 8 tests passed
- workflow contract tests: 33 passed
- `sh -n scripts/smoke-macos-app.sh`: passed
- `git diff --check`: passed
- packaged macOS build and strict signature verification: passed

These gates establish build/static/browser integrity. They do not supersede the unresolved native
completion interaction acceptance above.
