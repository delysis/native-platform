# Chat Pro next handoff: current Loom UI port and EASL productionization

Use ordinary ChatGPT **Chat** mode with a **Pro** model. Work mode is forbidden.

This is a remote source-review and patch-authoring task. Do not ask for George's
Mac, `/Users/george`, Downloads, a shell, Cargo, pnpm, SSH or a desktop
connector. Their absence is expected. Do not claim to run tests, edit the real
checkout, commit, push, launch a window or inspect local Git state. Read the
attached source, produce bounded reviewable patches, and give exact proposed Mac
commands for Codex to execute later.

## Current implementation checkpoint

Review target:
`5aaadf1e98433921b8a77c057780a83fadc9d5b8` on
`codex/loom-easl-main-integration`.

The retained-material privacy repair is complete at this checkpoint. It seals
private bindings and evidence through the existing vault boundary, preserves
plaintext content identities, validates material payloads during protected copy,
and adds 15 privacy regressions. Do not replace, unwind or silently migrate that
design. Read `VERIFICATION-2026-09-22.md` for executed Mac evidence and honest
limits.

The immediate blocker is the intentional native-interface guard. Current
`App.svelte` hashes to
`6667474039c166a5a33b1f4e2cd23e5cca79353962c73b1df4ac0e5c3552709a`; the
reviewed native port still expects
`3d45a082bb51a6f11a06b865e5e6c5a198e1c836dbb58fe3ded8562d931a1725`.
Do not change that expected hash until the current interface and native port have
been reviewed and changed together.

Studio, Cast, Hollow and `easl-native-host` source, assets, dependencies and
ancestry are forbidden. Historical policy/provenance strings are not imports.
EASL owns reusable presentation, text and editing policy; Loom/native-kit retains
documents, immutable history, configuration, credentials, model lifecycle,
generation, cancellation, grants, provenance and persistence.

## Required first response

Read the complete attached current `App.svelte`, `app.css`, `build.rs`,
`ui/loom.easl`, and every Rust file under the native experiment's `src/`. Then:

1. Produce `CURRENT-UI-MATRIX.md`, mapping every current titlebar control and
   relevant state to the exact Svelte source range, Rust state/action owner,
   EASL input/action, icon source and missing test.
2. Produce `0005-port-current-titlebar-and-panes.patch`. It should port the
   current Add menu, main/right/bottom pane controls, conditional Ghost/Loompad
   control, title text, theme geometry, keyed icons, focus and accessible state.
   Keep unavailable service actions visibly disabled with a reason. Do not fake
   generation, recording, menus or panes.
3. Add offscreen/state-machine tests that fail against the stale titlebar and
   cover narrow windows, light/dark themes, disabled/busy states, pane
   preservation, menu dismissal and conditional controls.
4. Leave the reference hash unchanged in this patch. State the exact acceptance
   evidence required before a later hash-only patch can be considered.
5. Produce `NEXT-TODO.md`, carrying every unfinished item below with exact files,
   dependencies, acceptance evidence and the source ranges you did not receive.

If the complete source proves that a safe patch must be split, return the first
independently useful patch and a precise boundary for the next. Missing local
execution is never a reason to stop source review.

## Full task ledger

Work top-down. Each response should finish at least one bounded patch or a
source-complete audit and update the ledger. Do not combine unrelated ownership
changes into a giant patch.

### P0 — current interface fidelity

1. Diff current titlebar DOM, conditions, labels, tooltips and order against the native scene.
2. Recover the exact current macOS leading inset and fullscreen inset behavior.
3. Port the real Add button as a menu trigger, not a renamed new-document action.
4. Port New document, Add files, Open library and Connect sources menu entries.
5. Match Add-menu Escape, outside-click, selection and focus-restoration behavior.
6. Preserve Add-menu enabled, disabled, busy and read-only reasons.
7. Port main-pane show/collapse state and its default-main fallback control.
8. Port right-pane show/collapse state without remounting its service owner.
9. Port bottom-pane show/collapse state without losing terminal/chat scroll or selection.
10. Preserve pane-local caret, selection, scroll and draft state while hidden.
11. Bind busy-pane transitions to the actual cancellation owner before hiding.
12. Port the single conditional Ghost/Loompad button, including both icons and labels.
13. Match Ghost/Loompad enable, switch, loading, unavailable and material-view states.
14. Port recording, stop, retry-save, recognition-cancel and unavailable states honestly.
15. Hide recording and suggestion controls in the same current presentation modes.
16. Port exact title clipping, ellipsis, font, weight, centering and drag regions.
17. Match 30 px titlebar, 24 px controls, 15 px icons, 5 px gaps and current insets.
18. Match hover, pressed, focus-visible, disabled and high-contrast control states.
19. Resolve current theme tokens through shared configuration, including light and dark modes.
20. Remove stale cream/green experiment defaults and any independent product palette.
21. Replace positional SVG extraction with keyed assets and explicit conditional branches.
22. Assert main, right and bottom pane icons independently; reject missing or duplicate keys.
23. Assert both Ghost/Loompad icon branches and every current Add-menu icon.
24. Preserve useful content area and control discoverability at narrow window widths.
25. Map current error banners, status text and progress indicators into native presentation.
26. Map material, library, import and setup presentation modes that alter the titlebar.
27. Audit current settings/model/download indicators and expose only service-owned facts.
28. Add a source-derived reference manifest with semantic selectors as well as hashes.

### P0 — Loom/native-kit ownership and adapters

29. Inventory every UI action crossing from EASL into Rust and name its sole authority owner.
30. Use typed action enums; reject unknown, stale, duplicate and out-of-scope actions.
31. Bind actions to project session, document, revision, pane and selection where applicable.
32. Add monotonic state revisioning so delayed scene actions cannot mutate newer state.
33. Make state projection deterministic and bounded; exclude secrets and raw private paths.
34. Preserve create, rename, delete, export, reveal and folder-warning service behavior.
35. Preserve watcher, external-edit, reconcile and stale-revision behavior.
36. Preserve setup/config-file precedence without letting the view write policy directly.
37. Preserve material/library grants as host capabilities, never serialized view authority.
38. Preserve model selection, download, residency and cancellation in shared native services.
39. Preserve generation request fingerprints and exact lost-acknowledgement replay.
40. Preserve terminal/chat/page-preview receipt and cancellation ownership.
41. Project recording/transcription states without exposing captured audio through a view path.
42. Route clipboard actions through bounded typed input, with platform ownership explicit.
43. Keep menu commands and keyboard shortcuts on one action path to prevent semantic drift.
44. Add negative tests for actions after project close, document switch and revision advance.
45. Add negative tests for cross-pane, cross-project and cross-window authority reuse.
46. Add panic, cancellation and shutdown tests that leave no false UI success state.
47. Keep configuration files, generation recipes and immutable history in modular native-kit crates.
48. Document the narrow view-only boundary in public crate APIs and experiment README.

### P0 — production text/editor integration

49. Identify one real Loom writing pane for first integration without creating a second document owner.
50. Define a typed snapshot from Loom document/revision to EASL text/style inputs.
51. Bind every asynchronous `EditPlan` to document, revision, pane and selection.
52. Revalidate an `EditPlan` immediately before applying it through Loom's existing transaction API.
53. Reject stale, foreign, mid-grapheme, malformed and over-budget edit plans atomically.
54. Preserve exact UTF-8, mixed CRLF/LF, trailing whitespace and untouched Markdown source.
55. Preserve tracked selections and undo/redo across native projection changes.
56. Preserve markdown entities, hidden definitions, code fences, links and image syntax.
57. Preserve list/quote indentation, container boundaries and one-command undo semantics.
58. Route formatting, links, indentation and structural commands through `loom-markdown`.
59. Connect EASL glyph geometry, hit testing, caret, selection and decoration to one layout snapshot.
60. Invalidate only affected paragraphs after edits while preserving previous valid drawable state on failure.
61. Maintain caret visibility and preferred horizontal intent during reflow and paging.
62. Keep preedit separate from committed source and cancel it correctly on blur/focus transfer.
63. Expose composition candidate geometry in physical pixels with scale-factor revisioning.
64. Add clipboard, word, line, paragraph, document and drag selection contracts.
65. Add pointer capture, double/triple click, autoscroll and out-of-bounds drag behavior.
66. Add exact-source save/checkpoint/reopen tests from the integrated pane.

### P1 — typography and font system

67. Document current shaping, line breaking, painting and EASL/native responsibility boundaries.
68. Complete font file loading with bounded size, face index and malformed-font rejection.
69. Add family/style/weight/stretch matching with deterministic fallback order.
70. Preserve variable-font axes across shaping, metrics, rasterization and cache identity.
71. Add script-aware and grapheme-safe fallback without splitting combining sequences.
72. Add color emoji support with explicit supported table formats and fallback behavior.
73. Add multi-page glyph atlases with immutable texture identities and bounded residency.
74. Add deterministic eviction that never invalidates a submitted display list.
75. Separate font-resource lifetime from document, pane, window and renderer lifetimes.
76. Cover ligature caret stops and clusters with logical and visual navigation.
77. Cover bidi isolation, embedding, neutral resolution and selection affinity.
78. Cover Arabic joining and contextual shaping across safe span boundaries.
79. Cover CJK line opportunities, kinsoku-style prohibitions and ideographic spacing.
80. Support soft hyphen policy without changing source when a break is not selected.
81. Add dictionary hyphenation with versioned language data and explicit opt-in policy.
82. Implement Knuth-Plass-style glue, penalties, fitness classes and demerits where justified.
83. Add widows, orphans and consecutive-hyphen controls as paragraph policy.
84. Add optical margin alignment and punctuation protrusion as explicit style options.
85. Add bounded microtypographic expansion/tracking with deterministic fallback.
86. Add first-line, hanging and negative indents with source-independent layout geometry.
87. Add tabs with paragraph-relative stops and mixed-direction coverage.
88. Add inline boxes, baseline alignment, superscript/subscript and decoration metrics.
89. Define unsupported typography explicitly; never silently normalize or discard source.
90. Compare terminology and defaults against Butterick and TeX without overstating parity.

### P1 — Pretext differential coverage

91. Pin the exact Pretext revision and record source/artifact hashes.
92. Map every retained Pretext option to an EASL option or an explicit unsupported result.
93. Compare line choices, not just total height or glyph bounds.
94. Compare shaped clusters, advances, offsets, bidi order and selected break penalties.
95. Compare rich runs, no-break spans, inline boxes, tabs and soft hyphens.
96. Separate browser-measured correspondence from native invariant tests.
97. Add retained falsifiers for every discovered divergence and provenance for every fixture.
98. Add multilingual corpora for Arabic, Hebrew, Indic, Myanmar, Thai, CJK and emoji.
99. Add variable-font and fallback corpora with exact font-file identities.
100. Add malformed-font, malformed-text and capacity-exhaustion rejecting cases.
101. Add adversarial long words, narrow measures, negative advances and zero-width controls.
102. Report accepted differences with an explicit product policy instead of smoothing the oracle.

### P1 — performance and resource bounds

103. Measure compiler parse/typecheck/lower time independently from runtime execution.
104. Measure VM/native-call transfer, shaping, line breaking, atlas, paint and composition separately.
105. Measure cold startup, first document, warm reopen and first glyph upload.
106. Measure typing-to-display p50/p95 under short, long and multilingual documents.
107. Measure caret navigation, pointer selection, scroll, resize and pane transitions.
108. Measure Add-menu and palette opening, repeated clicks and disabled-action latency.
109. Measure sustained editing with bounded history and retained paragraph caches.
110. Record allocations, retained bytes, atlas occupancy, cache hit rate and eviction counts.
111. Add work budgets for scalars, graphemes, runs, lines, glyphs, pages and display-list items.
112. Add overflow-safe arithmetic at every capacity and byte-size boundary.
113. Add damage tracking so unchanged text is not reshaped or repainted.
114. Add GPU composition only where measurements beat CPU repaint without losing determinism.
115. Preserve a deterministic offscreen renderer for review images and regression diffs.
116. Rename colliding `specimen` example binaries so Cargo no longer warns or risks overwrite.
117. Resolve the vendored `harfrust::hb::face::shape` dead-code warning without weakening lint scope.
118. Add CI thresholds that detect regressions while tolerating controlled machine variance.

### P1 — native input, focus and accessibility

119. Define a single focus owner across titlebar controls, panes and the native editor.
120. Match Tab and Shift-Tab order in every pane visibility combination.
121. Restore focus after Add-menu dismissal, pane toggle, dialog cancel and failed action.
122. Route platform shortcuts without turning AltGr or IME input into commands.
123. Cover key repeat, dead keys, composed accents, CJK IME and late composition events.
124. Cover focus loss during preedit and window close during an admitted edit.
125. Keep candidate rectangles correct across scroll, resize, Retina scale and window movement.
126. Define accessible roles, names, values, pressed/expanded/busy/disabled states for every control.
127. Expose text ranges, caret, selection, line bounds and editable actions to the OS tree.
128. Preserve accessibility identity when panes hide/show without remounting their owners.
129. Announce errors, progress and completion without leaking private text.
130. Add keyboard-only and switch-control navigation expectations.
131. Add reduced-motion, increased-contrast and system font-size behavior.
132. Test VoiceOver ordering and editable-text actions in separately authorized native acceptance.

### P2 — storage, security and recovery follow-through

133. Inventory every remaining direct private file writer outside the shared codec.
134. Audit authenticate-before-parse and bounds-before-allocation at every private reader.
135. Audit ancestor-directory replacement races and descriptor-relative alternatives.
136. Audit mutable binding compare/write races and state which writers are trusted.
137. Add crash-recovery policy for encrypted evidence staging without adopting unknown files.
138. Audit protected-copy schema validation after generic byte copying.
139. Audit every generation/terminal receipt path for material-plan and retrieval fidelity.
140. Audit idempotency keys against changed cursor, pane, model, materials, policy and sampling.
141. Add lost-vault, wrong-key, plaintext-downgrade and ciphertext-rebinding tests to new writers.
142. Preserve external source bytes and immutable history on every failure path.
143. Audit project close, worker panic and shutdown joins for editor/render tasks.
144. Add fuzz/property tests for edits, ranges, private envelopes, display lists and replay JSON.

### P2 — packaging and OS qualification

145. Identify the real standalone native experiment packager; do not assume the web Loom bundler.
146. Record source SHA, dirty-patch digest, executable hash, bundle ID and embedded asset hashes.
147. Verify signing identity, hardened runtime and declared entitlements.
148. Verify no forbidden application source or dependency enters the bundle or published ancestry.
149. Verify embedded EASL, fonts, icons and reference manifests match the reviewed source.
150. Add a current-only accessibility control bound to the launched PID and executable identity.
151. Prepare pointer/keyboard/pane/menu/editor scripts for separately authorized foreground testing.
152. Prepare signed encrypted save, quit, relaunch and exact-source recovery scenarios.
153. Prepare external-edit, lease contention, forced shutdown and model-cancellation scenarios.
154. Qualify platform-webview coexistence without sharing view authority or focus accidentally.
155. Qualify macOS first, then Linux and Windows separately; never transfer OS evidence.
156. Define failure reports that preserve diagnostics without manuscript or credential leakage.

### P3 — repository and release evidence

157. Re-run forbidden-source and ancestry scans on every proposed patch.
158. Explain every remaining forbidden-name string as policy, documentation or provenance.
159. Audit workspace members, features, permissions, generated schemas and lockfile changes.
160. Keep dependency additions separately justified, pinned and license-reviewed.
161. Keep exact command, target, feature union, toolchain and exit status in receipts.
162. Keep passed, failed, ignored, manual and not-run tests separate.
163. Keep specification, source correspondence, component tests, package tests and live acceptance separate.
164. Produce a machine-readable artifact manifest for every handoff bundle.
165. Produce a reviewer-facing change map from requirement to code, tests and evidence.
166. Update public EASL text/editor API docs with supported and unsupported behavior.
167. Add runnable examples that do not collide in target filenames.
168. Keep benchmark corpora and large generated outputs out of source history unless intentionally curated.
169. Define promotion criteria from experiment to native-kit component and from component to Loom default.
170. End each response with the next five executable source tasks and every remaining acceptance boundary.

## Proposed Mac verification for returned patches

Give exact commands but label them **not run by Chat Pro**. At minimum include:

- `git apply --check --whitespace=error` for each patch;
- Rust 1.92.0, `--offline --locked`, `--profile native-view`, one Cargo process;
- focused tests for every changed reducer/adapter/view module;
- offscreen current-interface comparisons without updating the guard first;
- the full EASL text/Markdown/native-view component suite;
- frontend check and unit tests when App fixtures or contracts change;
- strict Clippy for all changed crates and feature unions;
- `cargo fmt --all -- --check`, `git diff --check`, and `xtask policy`;
- forbidden-source/dependency/ancestry scans; and
- separately identified foreground, signed-bundle, IME and accessibility work
  that remains unexecuted until George authorizes it.

Return useful source work even when native execution remains pending. Never
describe a static screenshot, successful build, offscreen render or source hash
as live product acceptance.
