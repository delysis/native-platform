# Selection during a bounded remote diff

The optimized candidate gate at `d47089e` stopped in the frontend suite:
`moves a selection with text inserted before it` returned an empty selection
instead of `world`. The log is `/tmp/loom-release-d47089e.log`; that attempt did
not produce an accepted candidate. The same existing test passed in isolation.

The remote diff has a 16 ms deadline. When matching the root finished but the
deadline expired before recursion into a paragraph, its fallback replaced the
entire paragraph. That discarded useful unchanged text anchors, collapsing
selection and local undo mappings even for a simple insertion before the text.

A deterministic clock-controlled regression reproduces that specific deadline
boundary. Before the repair, it fails with the same empty selection:
`/tmp/loom-remote-selection-before-final.log`. The repaired fallback uses the
matching node's unchanged content prefix and suffix to bound the replacement.
Structural changes still use their existing replacement and recovery paths;
the time and edit-distance budgets are unchanged.

All five focused remote-editor cases now pass, including preserving the selected
word and undoing the local insertion while keeping a remote prefix. The passing
log is `/tmp/loom-remote-selection-after.log`. This is an editor-state regression
result, not evidence of physical simultaneous typing or IME acceptance.

The next release attempt at `ea0f11b` passed the remote-editor cases but stopped
in unrelated full-app Svelte compilation during test setup: 463 assertions
passed and 29 did not run. An isolated repeat confirmed that the compile hook
could take 38 seconds on this host, exceeding Vitest's default ten-second setup
limit (`/tmp/loom-app-reactivity-isolated.log`). That one preparation hook now
has a bounded 60-second budget; assertion timeouts and behavior are unchanged.
All 29 assertions passed afterward (`/tmp/loom-app-reactivity-setup-budget.log`).
Neither failed release attempt is treated as an accepted candidate.
