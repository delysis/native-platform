# Native Loom frontend parity — reviewed reference

Reference: main `90349a54061790954cf8a88160e4e29a8a325d4d` combined locally with
`a43231626f5deb4ad8f6f08beb36dca40a236270` (draft Mine PR 47, unmerged upstream).
The titlebar fragment is unchanged by that integration. Reviewed App/CSS hashes are in
`build.rs`. This inventory supersedes the September 5 interface inventory;
retired controls are not requirements. See [REFERENCE-AUDIT.md](REFERENCE-AUDIT.md).

The goal remains incomplete. Source identity, contracts, offscreen pixels and
historical runtime receipts must not be reported as current product acceptance.
The current-source configured-pane smoke is recorded in
[NATIVE-REVIEW-2026-09-15.md](NATIVE-REVIEW-2026-09-15.md); it establishes only the
specific native interactions listed there.
The subsequent splitter check is in
[NATIVE-RESIZE-REVIEW-2026-09-16.md](NATIVE-RESIZE-REVIEW-2026-09-16.md), including
an unresolved stray-key observation that prevents full focus-isolation acceptance.
The later [focus review](NATIVE-FOCUS-REVIEW-2026-09-16.md) adds native evidence for
Shift-Tab and focused splitter input isolation, plus source-level fixes for selector
dismissal, format-palette dismissal and accessibility action validation. Its
separate control-activation observation remains unresolved at the native boundary.

The September 16 integration brings PR 50's provider, import/context ownership
and cache fixes into the source checkout. It preserves Mine's Rust-owned config,
encrypted sidecars and frozen profiles alongside explicit source-version-bound
materials. Historical native review bundles precede this integration; they do
not establish runtime acceptance of the combined application services.

| Area | Current authority/reference | Native state and remaining acceptance |
| --- | --- | --- |
| Titlebar | App.svelte canvas controls; app.css | Current fixed SVGs and geometry ported. No EASL label or retired titlebar controls. Configured right/bottom pane toggles use the current SVGs. Record/Ghost/Loompad disabled. Enabled states, title truncation, drag behavior, theme and real interaction remain open. |
| Workspace/panes | WorkspacePane.svelte, PaneDivider, App.svelte pane slots; loom-config | Multiple native views now share the active Markdown source/history while retaining pane-local geometry, scroll, mode and caret. Rendering, edits, IME isolation and saving have synthetic regression coverage. Existing-document auxiliary editor and independent Wry pane retained. Configured main/right/bottom slots, toggles, named references, selector keyboard isolation and distinct pane identities are wired. Outline/right/bottom splitters implement bounded dragging, arrow/Home/End keys and accessible values. Enabled controls and editors now have stable focus identities, Tab traversal and selector-focus restoration. Full native activation, long-outline traversal, search/tree outline, chat, terminal and preview adapters remain incomplete. |
| Configuration | loom-config; workspace_template | One shared Rust snapshot used by Tauri and native. Current precedence and conflict tests pass. Native consumes theme mode and pane configuration; full themes, settings refresh and visible errors incomplete. No separate experiment preference authority. |
| Project/document lifecycle | Current Tauri project commands; loom-store | Same ordinary-folder store entrypoint and shared no-clobber Untitled creation. Async open/create/select/save with stable identities. Rename/delete/export/reveal, folder warnings, large folders and watcher interaction incomplete. Prototype has 16-document/1 MiB bounds. |
| Source fidelity | loom-document/store; source editor and verse contracts | Prose and verse retain exact bytes. Saving does not rewrite source or add undo. Source/visual mapping and composition contracts tested. Real OS IME and complete structural editing acceptance open. |
| Markdown | LoomEditor.svelte and current visual editing helpers | Lossless native model, headings/emphasis, quotes/nested lists, code, input rules, mappings and atomic undo implemented. Source/verse Shift-Tab outdents literal source atomically or releases focus on unindented lines. Reviewed Source and Format keyboard shortcuts are routed. The paragraph/emphasis/list controls retain revision-bound pane selections, active states and accessible names. The Link destination/Link/Remove row is wired with independent literal input, history, pointer/keyboard/IME routing, horizontal scroll and text-input accessibility. Image controls, unsupported structural edits, native activation and broader adversarial corpus remain incomplete. |
| Typography | Current editor CSS and retained Pretext reference | Parley/Vello Rust text stack, paragraph roles, grapheme navigation, caret/selection geometry and corpus tests exist. Browser differential, full rich-item parity, long-document incremental layout and current display-latency evidence incomplete. |
| EASL text library | User ecosystem requirement; shared service TEXT-LIBRARY.md | Importable EASL modules own composition, editing/history/preedit, fallback/page selection, glyph placement, caret/hit geometry, navigation and styles. Narrow native primitives supply fonts, shaping/rasterization, immutable UTF-8, Unicode/input facts and GPU execution. Loom consumes shared editing, styles, navigation and final placement while retaining native line/caret layout and rasterization. Studio and its host/dependencies are removed; runtime tests and standalone specimens execute independently. See TEXT-LIBRARY.md for bounded capabilities and remaining atlas-widget/native acceptance gaps. |
| Persistence/recovery | Current loom-store | Worker owns lease; exact-source conditional saves, journals, immutable checkpoints, captured pending requests and partial acknowledgements tested. Tauri/native adapter orchestration still needs consolidation. Stale-draft/reconciliation UI, external changes and fatal-worker recovery incomplete. |
| Private storage | Current desktop-vault/store policy | New macOS projects use encrypted private history; ordinary manuscripts/config remain readable; existing plaintext remains unchanged. Current signed native launch/relaunch acceptance open. |
| Models/generation | Current shared native generation/model services | Existing authorities retained. Native model setup, loading, verified downloads/cancellation and state presentation incomplete. |
| Ghost text/Loompad | Current App.svelte and generation services | Current assets extracted. Source revision/blob and UTF-8 boundary capture is shared between Tauri Weave admission and the native project worker; tests reject draft, stale-source and wrong-project requests. Native selection/presentation binding, model submission, rendering, cancellation, promotion and durable alternatives remain incomplete. |
| Context/imports/attachments | Current shared context/import and attachment services | No invented context file. Existing document auxiliary editor supported. Configured editor references resolve through the shared Rust document inventory. Context/import/drop, image/audio inputs and receipts remain incomplete. |
| Recording/speech | Current speech_input and microphone_capture | Services retained. Native record/stop/cancel/retry controls, authoritative target binding and transcript insertion incomplete. |
| EASL Studio | User exclusion | Removed together with its host adapter, Cast and Hollow. It is outside this stack and is not a parity deliverable. |
| Coexistence/accessibility | Wry/AccessKit/native host | Historical native/Wry interaction exists. Corrected-baseline focus/keyboard/IME and combined accessible child tree acceptance remain open. |
| Shutdown | Current close coordinators and worker registries | Native storage close drains accepted requests and joins. Full model/pane/session coordination and recovery remain incomplete. |

Application reference paths are under `products/loom/apps/loom/src`; Tauri
adapters are under `products/loom/crates/tauri-plugin-loom/src`.
`loom-markdown` owns source/semantic editing; `easl-text` now supplies the native
editor's shared EASL selection/replacement, pointer-hit, navigation and rich-style policy, and `easl-native-text` supplies
native shaping, line/caret geometry and rasterization. The general EASL atlas and native input APIs have
separate standalone coverage and are not yet this experiment's rendering path.
Shared Rust services own application/configuration behavior. EASL owns the
view. Shared settings, creation and store consumers are a start, not a
complete extraction of the current application session.

The pointer bridge uses the current shaped layout and indexed line, streaming
caret stops to EASL in batches of 256. CPU integration covers Markdown source and
undo, independent pane selections, rich text/insets, wrap/bidi affinity, empty
lines, whole graphemes and granular word/line drags. This leaves fonts, current
titlebar assets and painter unchanged. Shared EASL policy now also chooses
vertical/page and logical line/document navigation destinations. Native line facts
and the same hit tester preserve each pane's preferred column; Markdown retains
source/history. The standalone EASL widget shares page policy, including progress
through paragraph gaps. Horizontal/word navigation, granular selection rules and
OS/accessibility acceptance remain open EASL integration work.

Rich view style resolution now also runs in the shared EASL library. The Loom
adapter supplies semantic roles and inline flags; EASL applies ordered field
masks and coalesces equal spans. Current heading, body, code, bold, italic, link,
rule and source styles retain the reviewed role data, including shaping settings
and spacing. Each pane caches styles by document revision and source mode,
invalidating on appearance changes. CPU integration verifies cache reuse through
resize/caret movement, refresh after edits, independent pane styles, exact source
and undo, IME preedit isolation, empty-heading metrics and invalid-style recovery.
No source/history/config authority moves into this resolver. This is actual Loom
policy adoption; Parley still supplies shaping, line and caret geometry. The general EASL
line consumer now also applies rich size/spacing, feature settings, mixed-size
baselines, colored ink and font-metric decorations through `rich.easl`. Its CPU
and offscreen fixtures are component evidence. The standalone contextual widget
now carries rich spans through wrapped paragraphs, variable line heights, colored
ink/decorations and shared caret/viewport geometry. Style changes invalidate its
single-paragraph shape cache; failed layout preserves completed arrays. Rich
fallback, multi-paragraph cache retention, Loom's atlas/line-layout integration,
latency measurements and native interaction acceptance remain open.

The actual renderer now uses reusable EASL paint policy for manuscript text,
labels and paragraph markers. It supplies shaped visual glyphs and font/style
facts in bounded batches; EASL computes final glyph positions and inline
decoration geometry. Native font instances, Vello rasterization, clipping and
display scaling remain in the host. A bounded geometry cache survives unchanged
repaints and host-origin/display-scale changes. CPU comparisons retain exact
native positions/pixels across mixed scripts, sizes, clipping and three scales;
both-evaluator policy tests reject invalid late input before output. Current
Loom renderer checks exercise both appearances, cache reuse and configured
panes. This is actual paint-policy adoption, not completion of the independent
EASL atlas widget, native line-layout replacement or OS/performance acceptance.

The editor now reuses shaped runs through width, alignment and paragraph-box
changes, including active preedit. Fresh-layout comparisons cover glyph, caret
and hit geometry, and the actual Loom adapter checks shaping/style-cache reuse.
The offscreen resize measurements in [PERFORMANCE.md](PERFORMANCE.md) exclude
painting and window input; edits still reshape the document.

The formatting palette now retains its exact document, revision and pane selection
while chrome has focus. It follows selection movement only in the focused owner;
source-mode changes, composition, document replacement and unrelated source edits
invalidate the capture. Successful commands renew it and return focus to the
editor. EASL paints the nine existing controls' active states, and AccessKit gets
explicit accessible names and toggle states. Renderer visits to sibling panes
preserve pending typing marks. Boundary typing is checked against the installed
current web editor, including inclusive emphasis and noninclusive links. The
native palette now includes the reviewed link input/Link/Remove row, with the
reference's centered 13px command labels, bold B and italic I. Its transient
single-line input uses shared EASL editing, hit testing and scroll policy while
retaining independent source/history. Newlines from paste are stripped as in the
reference input; invalid link commands preserve the captured manuscript. The
first selected link initializes the destination; subsequent owner selection
changes preserve the typed destination. Link and Remove enablement follows the
reference. Unlink also removes marks from selected edge spaces. After the input
loses focus, its pending IME owner remains until commit/disable so late URL
commits cannot be redirected to a manuscript. CPU event/geometry checks and
offscreen pixels do not establish native control or OS IME acceptance.

Acceptance must identify the intended reference and implementation revisions,
executable, bundle, assets and process before testing actual native interactions.
Use synthetic projects, preserve active user windows and private manuscripts,
and list unmet requirements explicitly. Do not promote old receipts into current
acceptance or mark the goal complete while required behaviors remain unfinished.
