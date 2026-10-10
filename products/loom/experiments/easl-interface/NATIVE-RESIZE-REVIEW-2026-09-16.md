# Native pane resizing: limited runtime check

Reference: `a43231626f5deb4ad8f6f08beb36dca40a236270`, the unmerged Mine
integration containing main `8ce8948`. Implementation was the uncommitted
`codex/loom-easl-current` checkout, Rust 1.92.0, optimized `native-view` profile.
The titlebar and PaneDivider fragments were separately compared with upstream
`90349a54061790954cf8a88160e4e29a8a325d4d` and were unchanged. That upstream
revision was not integrated into this executable.

The stable-development-signed bundle was `Loom EASL Resize Review.app`, bundle
ID `app.delysis.loom.easl.resizing.review.20260916`, observed PID `70595`.
Executable SHA-256:
`7720c20021cc2bdc8d8d817f2ce6183be5cbf0a0390bb5ad5be7c469f67fec8a`.
Its executable, bundled EASL and implementation source hashes were recorded
before interaction. Accessibility exposed the new numeric pane splitters.
Artifacts, including `identity.json` and `interaction-receipt.json`, are under
`target/easl-integration/pane-resize-review-20260916/`.

Actual macOS interactions established:

- Right and bottom panes accepted accessible numeric values. Arrow keys moved
  by 10 logical pixels, Shift-arrow by 40; Home/End honored current limits.
- Dragging resized the right, bottom and outline panes. The bottom stopped at
  its 60% bound; the right pane re-clamped as the outline grew.
- Escape returned focus to the manuscript. Resize operations did not enter its
  undo history: one undo removed the only observed text edit after resizing.
- After undo/save, SHA-256 hashes of both original synthetic manuscripts and
  authored `.mine.toml` matched their original bytes exactly.

An initial batched `x`, Command-S, Escape after focusing the maximum-width right
splitter unexpectedly inserted one character in the synthetic heading. Controlled
retries with separately observed bottom, right and outline focus did not reproduce
it. The cause is unproven. This is an unresolved observation, not a repaired or
accepted focus-isolation path. Later geometry changed between observations; the
review window was left untouched to preserve possible user interaction.

Final owned-code checks: 44 host-view tests passed, two manual tests remained
ignored; strict Clippy passed. A synthetic appearance check also passed, before
the final hover/project-reset adjustments; the signed native bundle includes
those adjustments. Source/caret/scroll/history/config preservation through
reflow is additionally covered by the renderer integration test.

This receipt establishes the listed interactions only. Full keyboard traversal,
OS IME, unexplained key routing, original-webview pixel comparison, encrypted
creation/relaunch and complete interface parity remain open. See `PARITY.md`.
