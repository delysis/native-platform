# Configured native panes: limited runtime check

Application reference: `a43231626f5deb4ad8f6f08beb36dca40a236270`, the unmerged
Mine integration containing main `8ce8948f3bcfc24df87ed0346801dedc7402feb0`.
Implementation: the uncommitted `codex/loom-easl-current` worktree, Rust 1.92.0,
optimized `native-view` profile. No original application source, store or config
parser was changed in this pane-wiring pass.

The signed review bundle was `Loom Current EASL Review 2.app`, bundle ID
`app.delysis.loom.easl.current.review.20260915T2343r2`, PID `62011` at observation.
Executable SHA-256:
`b7353705e8b2913d49bfa60480e16697e5909df7ec536e67a6fbc2afbf10d81c`.
The development signing script supplied its stable signing identity. The review
bundle uses an isolated, explicitly plaintext synthetic fixture; this does not
test production encrypted project creation or relaunch.

Local artifacts are under
`target/easl-integration/configured-panes-review-20260915T2343/`: `identity.json`,
`implementation-sha256.json`, and `interaction-receipt.json`. The executable and
bundled EASL source were hashed before interaction. Accessibility exposed the
current-only configured controls `Hide Reader` and `Hide Proof` before visual
evaluation.

Observed through the actual native macOS window:

- Current titlebar assets and main/right/bottom text views rendered.
- The pane selector consumed a typed `x`; Down/Return selected Reference without
  changing the manuscript behind it.
- `Open Reading notes` switched the existing active document.
- Typing in the reference editor appeared in all three views. Command-S saved
  the edit to `Reading notes.md`; the saved bytes were checked independently.
- Hiding the focused reference editor returned focus to the main manuscript.
  Showing it restored the configured pane.

Desktop state changed during the check. The receipt retains the observed saved
synthetic source verbatim, including trailing spaces; this interaction is not
an exact-byte replay test. Source-fidelity claims remain bounded by the separate
automated editing and persistence tests.

The first launch failed to paint because a new selector-arrow SVG used a
disallowed attribute. A renderer test reproduced the failure. The asset was
corrected, icon initialization made atomic, and validation moved before window
creation. The first launch identity remains in `failed-launch-identity.json`;
it is not successful rendering evidence.

Final regular checks: 159 tests passed, two opt-in tests remained ignored;
strict Clippy, current-document validation and ignored-test inventory passed.
The synthetic appearance test was also run explicitly. Its earlier PNGs precede
the selector-arrow correction; use the observed second native launch for that
affordance, not those PNGs.

This is limited runtime evidence, not interface acceptance. Pane resizing,
original-webview pixel comparison, long-title ellipsis, chat/terminal/preview,
model controls, complete lifecycle/config refresh, OS IME, combined Wry
accessibility, and encrypted relaunch remain open. See `PARITY.md`.
