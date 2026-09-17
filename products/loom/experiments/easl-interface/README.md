# Loom interface in EASL — experiment

An incomplete native view of current Loom. The application reference combines
main `90349a54061790954cf8a88160e4e29a8a325d4d` with Mine draft PR 47 at
`a43231626f5deb4ad8f6f08beb36dca40a236270`. Mine remains unmerged upstream.
The combined titlebar matches that main revision byte-for-byte. This experiment
does not establish frontend parity.
See [HANDOFF-2026-09-17.md](HANDOFF-2026-09-17.md) for the handoff and prioritized remaining work,
[PARITY.md](PARITY.md) for the current gaps, and
[REFERENCE-AUDIT.md](REFERENCE-AUDIT.md) for the obsolete baseline incident.

`ui/loom.easl` describes geometry, typography, controls and transient view state.
Native Rust handles shaping, source transactions and application services. Manuscript strings
never enter the EASL VM. The view compiles once and evaluates on view input;
pointer motion is coalesced and unchanged hover scenes skip painting.

The broader target includes a reusable text and editing library written in EASL,
over native font/shaping/drawing primitives. The shared `easl-text` service now
implements paragraph composition, atlas placement and basic editing/input policy
in EASL. This experiment consumes selection/replacement policy through `Editing`
and pointer-hit policy through `HitTesting`, using actual shaped caret stops.
`Navigation` supplies EASL vertical/page and logical line/document movement;
its painter has not yet adopted the general atlas library. See the
[EASL text library target](../../../../crates/services/easl/TEXT-LIBRARY.md).
The current VM boundary above describes this implementation, not a requirement
that all reusable view algorithms remain in Rust.

## Run

Use an isolated synthetic project and the optimized, debuggable profile:

```sh
rustup run 1.92.0 cargo run --locked --profile native-view -p loom-easl-interface -- --project /absolute/path/to/scratch-project
```

On macOS, `--webview` adds an independent Wry child webview beside the native
manuscript. `--ui /absolute/path/to/loom.easl` selects another view; F5 validates
that source before replacing the live interface. `--build-info` prints the
implementation metadata and the reviewed application reference. No project flag
uses the application-data `Loom EASL Experiment/Project` scratch folder.
This is a standalone experimental host, not the completed Tauri integration.

New macOS projects use the current store's encrypted private-history policy;
manuscripts and authored configuration remain readable. Existing plaintext
projects are not silently converted. Follow `products/loom/AGENTS.md` for stable
development signing before making a repeated-use application bundle.

## Responsibility boundary

| Responsibility | Owner |
| --- | --- |
| Geometry, typography, controls, presentation state | `ui/loom.easl` |
| Bounded display-list adapter, window, clipboard, IME, accessibility, optional webview | Experimental native host |
| Shaping, line/caret layout, font resources and rasterization | `easl-native-text` |
| Rich view styles, final glyph positions and inline decoration geometry | Shared `easl-text` EASL policy |
| Select-all, replacement and Unicode deletion policy | Shared `easl-text/library/editing.easl` |
| Pointer nearest-caret choice, including wrap/bidi affinity | Shared `easl-text/library/geometry.easl`, bounded native stop transport |
| Preferred columns, vertical/page and logical line/document navigation | Shared `easl-text/library/navigation.easl`, native line facts |
| Lossless Markdown/verse source, semantic edits and undo | `loom-markdown` |
| Project worker, draft/checkpoint operations, settings snapshot, document creation | `loom-text-session` |
| Exact saved source and UTF-8 boundary capture before completion | `loom-text-session::completion`, used by Tauri and the native worker |
| Authored settings and workspace interpretation | `loom-config` |
| Files, exact UTF-8 source, immutable revisions and conditional persistence | `loom-store`, `loom-document`, `loom-types` |
| Protected private payloads | Existing desktop vault and store integration |
| Generation, models, context, imports and speech | Existing native services; full view adapters remain incomplete |

`loom-text-session::configuration::read` is consumed by both the Tauri adapter
and native project worker. It retains current `.mine.toml` precedence, existing
`.loom.md` handling, parse errors and external-edit conflict rules. This
experiment has no separate preference file or `loom-preferences` authority.
The native view consumes theme mode and configured pane slots, titles and document
references. Custom themes, settings refresh and visible configuration-error
handling remain incomplete.

The Tauri adapter and native worker also share `lifecycle::create_untitled`.
Both persist through the current `loom-store`; their checkpoint/draft adapter
orchestration still needs consolidation. `Untitled.md` naming, historical reservations and
no-clobber file creation follow the current application. Both prose and verse
retain their exact bytes; saving does not normalize line endings or add an undo
transaction. No synthetic context manuscript is created. An auxiliary editor
must bind an existing document by its stable identity.

The bounded project worker owns the exclusive project lease. After startup,
storage work runs off the event loop. Replies update acknowledged baselines
without replacing newer typing. Journaling is scheduled after 750 ms and idle
checkpointing after 900 ms; these are scheduling intervals, not disk-latency
guarantees. Composition defers checkpoints, and uncommitted IME text is never
saved. Partial failures retain successful acknowledgements and pause automatic
retry. Close stops admission, drains accepted work and joins the worker.

## Current interface and acceptance boundary

The titlebar reference has outline and new-document controls on the left, and
configured pane toggles, recording and Ghost text/Loompad controls on the right.
It has no EASL label, co-writer, context, Shuttle, appearance or editor-mode
titlebar buttons. The build verifies the reviewed `App.svelte` and `app.css`
hashes and extracts the current SVG assets as JSON data. Reference changes must
be reviewed with the native port. This check proves source identity, not visual
or behavior parity.

The native view draws the current controls, including configured right/bottom
pane toggles. Recording, Ghost text and Loompad remain disabled. Pane selectors
support pointer and keyboard selection; named references open the existing
document through the normal transition. Chat, terminal and preview adapters
remain explicitly unavailable. The outline, right and bottom pane splitters now
support bounded pointer resizing, arrow keys, Shift for larger steps, Home/End,
and native accessibility values. Dimensions stay local to the window, including
across project changes; resizing never rewrites authored configuration.
Enabled buttons, pane selectors, document rows, editors and splitters now have
stable native focus identities and Tab/Shift-Tab traversal. Enter/Space activate
buttons; closing a pane selector restores its focus, and Tab continues into its
uncovered siblings. Focused controls consume typing without changing the last
editor selection. Outline search/tree behavior, scrolling through long outlines,
webview focus traversal and the full document lifecycle remain unfinished.
Do not use this shell as an accepted
replacement for Loom or infer completion from its titlebar.

Completion source admission is now shared: the Tauri Weave path and the native
project worker read and validate source through `loom-text-session::completion`.
The worker exposes acknowledged revision/blob identities and refuses completion
capture for uncheckpointed drafts or stale visible bytes. The EASL view still has
no inference/promotion adapter or rendered ghost text; source admission alone
does not make those controls functional.

The host now supports multiple native editor views of the active document, with
one Markdown source and undo history. Layout, scroll, source/visual mode, stored
formatting choices and caret
belong to each pane; source selections follow edits through the shared Rust model.
Rendering another pane does not commit or redirect IME composition, and save
still goes through the original document worker. Configured editor panes now use these views, with distinct view identities for
each configured pane. Hiding or changing a pane preserves its view state;
typing in an open selector never reaches the editor behind it.
Cross-pane reprojection currently moves a caret inside a decoded entity to its
nearest source edge. Preserving that interior caret is still required before
claiming full multiple-pane editing parity.

Native editing includes headings, emphasis, quotes, nested lists, code blocks,
input rules, source/visual mapping, clipboard operations, undo/redo and literal
verse. Command/Control-B and I format text; Command/Control-Shift-M switches
source/visual mode, and Command/Control-Shift-F opens the formatting palette,
matching the reviewed application shortcuts. In source/verse, Shift-Tab removes
one leading tab or up to four spaces from each selected line as one undoable
edit; on unindented lines it moves focus backwards. The formatting palette includes
paragraph styles, emphasis, lists and the Link destination/Link/Remove row. Its
literal URL field retains independent undo, scrolling and IME ownership. Native
activation, image controls and complete structural editing remain unfinished. The text-session prototype also has
a 16-document and 1 MiB-per-text limit that has not reached application parity.

The focus review and its unresolved native activation observation are recorded
in [NATIVE-FOCUS-REVIEW-2026-09-16.md](NATIVE-FOCUS-REVIEW-2026-09-16.md).
For a controlled synthetic native investigation, `LOOM_EASL_FOCUS_TRACE` can name
a new log file. It records at most 4,096 focus/action events, never manuscript or
keyboard text. Existing files are not overwritten; file creation failure leaves
tracing disabled. The final selector/accessibility fixes have contract coverage
but have not yet been exercised in the native window.

Historical same-window native/Wry interaction and renderer measurements are
recorded in their original evidence. They do not qualify this corrected
baseline. Current native interaction, OS IME, combined accessibility, encrypted
relaunch, complete application shutdown and cross-platform UI acceptance remain
open. See the text crate README for the exact Pretext comparison boundary and
[PERFORMANCE.md](PERFORMANCE.md) for the earlier measured slowdown.

## Checks

```sh
rustup run 1.92.0 cargo test --locked --profile native-view -p loom-text-session -p loom-markdown -p loom-easl-interface -p easl-native-text
rustup run 1.92.0 cargo test --locked -p tauri-plugin-loom --lib workspace_template::tests
rustup run 1.92.0 cargo test --locked -p tauri-plugin-loom --lib new_document_
```

The manual `render_appearance_review` and `profile_native_view` tests use
synthetic text. Set `EASL_REVIEW_DIRECTORY` to an isolated output folder for the
former. Their exact prerequisites and evidence limits are registered in
`ci/ignored-tests.json`. Offscreen pixels and timing do not establish native
interaction, display latency or product acceptance.
