# Native focus work — 2026-09-16

This is partial interaction evidence and a source-level repair receipt. It does
not establish full focus isolation, a completed Loom frontend, or working native
Ghost text/Loompad. The active parity goal remains unfinished.

## Native observation before the final repairs

The uniquely named `Loom EASL Focus Review.app` was built from local integration
commit `9cd4366d62908a56980e81b5496a177c732cb3bc` plus uncommitted focus work.
Its reference was main `90349a54061790954cf8a88160e4e29a8a325d4d` plus Mine
`a43231626f5deb4ad8f6f08beb36dca40a236270`. This executable predates the final
selector, palette-dismissal and accessibility-dispatch repairs below.

| Identity | Value |
| --- | --- |
| Bundle ID | `app.delysis.loom.easl.focus.review.20260916` |
| Observed PID | `98049` |
| Executable SHA-256 | `14299e978da26f6591d9eaeb193244c749e259a069224d60a2fb78601137a8b4` |
| Bundled EASL SHA-256 | `6027ffb459eed22df1eb6827102232fe751592497791ccaa01cb1abfca7518fe` |

Local identity and fixture records are under
`target/easl-integration/focus-review-20260916/`. The executable is inside that
directory's `Loom EASL Focus Review.app/Contents/MacOS/`.

Native accessibility and pixels showed Shift-Tab moving focus from the
unindented manuscript to the `Hide Proof` button, with a visible focus ring.
With the outline open, Shift-Tab moved to `Resize documents`; typing an ordinary
letter retained splitter focus and left the manuscript unchanged.

A separate sequence involving `Show Reader` was followed by an ordinary letter
appearing at the manuscript start. Focus ownership between that activation and
the keypress was not established. Computer-use state-change warnings also
occurred during this investigation, so this observation does not isolate an OS,
host, accessibility or automation cause. The edit was undone and saved using
separate, refreshed-state actions. A subsequent read-only filesystem check
verified both manuscripts and `.mine.toml` against all three original hashes.
`preservation-check.json` records that result and confirms that the running
bundle executable is unchanged. No review window was driven, replaced or closed
during the continuation that made the final repairs.

## Final source changes and checks

The Rust host now separates focused controls/splitters from the retained editing
target. EASL draws the visible focus ring and hides editor carets during control
focus. Stable numeric control IDs also identify accessibility nodes; inserting
an outline row or changing a button label does not redirect an existing control.
Enabled buttons, selectors, document rows, editors and splitters participate in
Tab traversal. Enter/Space activate buttons through the existing EASL click
reducer. Native text, clipboard editing and IME admission remain excluded while
controls own focus.

Three consequential focus routes were repaired:

- A selected popup option disappears when the selector closes. Focus now returns
  to its surviving selector for pointer selection, Enter and Escape. Tab closes
  the popup, rebuilds the uncovered scene and traverses from that selector.
- Clicking a pane toggle outside the Format palette emits both palette dismissal
  and pane activation. Dismissal now preserves the newly focused control instead
  of clearing it and allowing subsequent typing into the manuscript.
- Text accessibility dispatch now accepts only advertised action/payload pairs.
  Unrelated actions and mismatched payloads cannot implicitly focus or replace
  the manuscript. This is a verified dispatch defect; it is not proof of the
  cause of the earlier native stray-key observation.

The reviewed Command/Control-Shift-M and Command/Control-Shift-F shortcuts now
reach Source and Format. Source/verse Shift-Tab calls the shared Rust Markdown
model's exact-source outdent, preserving UTF-8, mixed line endings, selection
direction, tracked pane selections and one-step undo. An unindented selection
adds no history and releases Shift-Tab to focus traversal.

The final consolidated run passed **174 tests**, with **2 existing manual tests
excluded**, across `loom-easl-interface`, `loom-markdown`, `loom-text-session`
and `easl-native-text`. Strict Clippy passed for both changed Rust packages,
including their tests. Logs: `target/easl-integration/focus-final-tests.log` and
`focus-final-clippy.log`. The regression tests evaluate the actual EASL program
and native Markdown/selector state; they do not substitute for native keypress
or accessibility acceptance of this final source.

Optional `LOOM_EASL_FOCUS_TRACE` records bounded event metadata to a new file for
the next controlled native investigation. It excludes authored text, key text,
clipboard data and composition payloads. It was not enabled in the observed
running bundle above.

## Remaining integration boundary

The separate Loom task reports a single Ghost/Loompad icon and quieter default
sidebar in commit `6ee740a` on `codex/loom-quiet-defaults`, submitted as a PR.
That report is coordination information, not evidence that the change is merged
or part of this reviewed reference. Review and integrate its final source before
the next native interface comparison. No Svelte/CSS reference was edited here.

Full native activation/focus isolation, long-outline scrolling and traversal,
native/Wry focus, OS IME, formatting-selection ownership and link/image controls
remain open. Model/completion, speech and the remaining application adapters are
still absent. Preserve the running synthetic review and ordinary Loom windows;
the experiment must remain clearly identified as incomplete.
