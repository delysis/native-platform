# EASL text library

Reusable view algorithms live in EASL. Paragraph composition is implemented in
[`library/paragraph.easl`](library/paragraph.easl). EASL selects line endings
across a paragraph, scores raggedness and discretionary/consecutive hyphens,
and applies a configurable last-line weight. The module has no host calls or
Loom dependencies. [`library/editing.easl`](library/editing.easl) owns selection
and replacement policy; [`library/input.easl`](library/input.easl) interprets
basic keyboard commands and composition events; [`library/atlas.easl`](library/atlas.easl)
places shaped glyphs and generates GPU quads. These are components of the broader
[text and editing library](../TEXT-LIBRARY.md), not yet a complete editable widget.
[`library/editor.easl`](library/editor.easl) adds independent editor state,
bounded undo/redo, composition and logical navigation over immutable text values.
[`library/geometry.easl`](library/geometry.easl) and
[`library/view.easl`](library/view.easl) add caret/hit geometry, selection rectangles,
pointer dragging and layout-based keyboard movement.

An EASL consumer imports the module and calls `text-compose` with a point count,
an inlined point reader, a line-measurement callback, width, `TextPolicy`, and
an inlined output function. [`examples/paragraph_policy.easl`](examples/paragraph_policy.easl)
executes that path with its own buffers. Reader/output functions use existing
EASL higher-order specialization; they are not per-element calls into Rust.
The current CPU VM does not support passing dynamic arrays as ordinary function
arguments. The callback API avoids that restriction and lets consumers supply
their own storage. Helpers and public types currently share EASL's import scope;
full module namespaces remain language work.

For native hosts, Rust `Composer` compiles the same library once and exchanges
typed `TextBreak`/`TextPolicy` records in bulk through `library/native.easl`.
`Composer::compose` uses `easl-native-text` for shaped opportunities and final
glyph layout. The native layer validates every selected boundary, mandatory
break and line measure before committing the layout; failures retain the
previous drawable text. Neither layer modifies manuscript bytes.

The measurement callback receives start/end point indices and a remaining work
budget. Return `TextLineMeasure { status, advance, work }`; ordinary shaped-prefix
consumers call `text-prefix-measure`, with zero extra work. `text-compose` requires
monotone measurements as the start moves earlier: it stops searching when a
candidate is too wide. `text-compose-contextual` examines earlier starts even
after an overwide suffix, because joining can make a longer Arabic span narrower.
Both share validation, work limits and output contracts. Explicit indivisible
overflow edges minimize the number of overflowing lines before typography cost;
they cannot beat a fitting solution. Invalid measurements and exhausted work
publish no selected breaks.

Composition permits 4,096 opportunities and at most one million work units:
one per candidate edge plus any traversal reported by its measurement callback.
`TextComposition.edges` reports that combined work; a rejected candidate can
report one beyond the policy limit, as before.
Scratch storage is bounded in EASL. No fitting solution, invalid input and work
exhaustion are explicit outcomes. Costs use normalized f32 values. Accumulated
f64 native advances cross as high/low f32 pairs, preserving small line widths
within long paragraphs. This is minimum-raggedness composition, not complete
Knuth–Plass glue/fitness, dictionary hyphenation or TeX microtypography.

Loom's experimental native editor now uses `Editing` for select-all, replacement,
grapheme deletion and word deletion. The shared EASL module chooses ranges;
`loom-markdown` retains semantic edits, exact source bytes and undo ownership.
`Editing::apply` is also usable with a standalone native text widget, whose
native buffer primitive provides validated transactions and undo. `plan_for`
lets other document models apply the same intent through their own transactions.
No text buffer is copied into the VM for these commands. Live preedit must be
committed or canceled before applying an external edit.

Loom also consumes the shared EASL nearest-caret policy through `HitTesting` for
clicks, shift-clicks and captured drags. `hit_editor` streams the indexed native
line's shaped caret stops in batches of 256, with one reusable transport buffer;
`hit_stops` accepts another layout engine's stops. Neither copies the manuscript
or stores geometry across edits/reflow. A query admits at most 2,097,154 stops and
finite coordinates within ±10³⁰. Results must be one of the supplied stops before
the native widget can apply them. Invalid input and VM failure leave its source,
selection and history unchanged. Live preedit must first be committed or canceled.

The EASL policy preserves both affinities at wraps and bidi boundaries. At a
shared edge it prefers the shaped cluster containing the pointer, then downstream
affinity; this preserves which word was clicked. Native stop generation excludes
inner grapheme edges and empty-line font struts. Word/line selection granularity,
horizontal/word navigation and rasterization still use the native editor. Its granular word
ranges now expand to complete graphemes before retaining a drag anchor, including
CRLF. This is incremental adoption of EASL view policy, not the final atlas painter.

`Navigation::apply` consumes [`library/navigation.easl`](library/navigation.easl)
for Up/Down, Page Up/Down, logical line start/end and document start/end. The host
supplies current line metrics and streams the selected line's stops through the
same `HitTesting` instance. EASL preserves a preferred column through short lines,
keeps one line of page overlap, forces progress through paragraph gaps, and clamps
to complete source edges. Shift retains the anchor. Preferred-column state lives
with each native view; no document text or history enters the policy VM. The
standalone EASL widget shares this page policy, including document-edge behavior.
Queries use indexed native line lookup and bounded stop batches, not a complete
document geometry upload. Active preedit must be resolved before navigation.

## Rich view styles in EASL

[`library/styles.easl`](library/styles.easl) resolves reusable rich-text styles
without depending on Markdown, Loom, font discovery or document storage.
`text-resolve-styles` takes immutable style/rule tables and ordered byte ranges
through readers, then emits coalesced `TextStyledSpan` records. A range selects a
base style and rule bits. Rules apply in order and replace only their named
properties, so an emphasis rule can change weight without losing the heading's
size, language, OpenType features or variation settings.

`TextRunStyle` carries family, size, line height, weight, italic, letter/word
spacing, features, variations, language, RGBA, underline, strike, keep-all and
wrapping. String-valued properties are IDs into immutable consumer tables;
language `0xffffffff` means unspecified. `TextStyleRule.properties` is a field
mask documented beside `text-style-patch`; boolean fields can be cleared as well
as set. Equal adjacent results coalesce, gaps remain gaps, and distinct empty
ranges retain insertion styles. This module resolves style values; font matching,
shaping, line composition and painting are separate operations.

The consumer validates resource IDs and UTF-8 scalar boundaries. EASL checks
style values, range ordering, rule selectors/references and selected flags before
publishing. The readers must remain stable and must not alias output. A dry pass
checks output capacity before the emitting pass. Status 0 means success, 1 invalid
input and 2 exhausted capacity/work. Limits are 1 MiB of source, 4,096 styles,
32 unique single-bit rules, 32,766 ranges and one million work units, counted as
`styles + rules + 2 * spans * (rules + 1)`. A selected rule's value read and field
patch are included in its unit, not counted separately. This bounds the algorithm;
it does not measure interpreter instructions or elapsed time.

Native hosts use `Styling::resolve` with typed `StyleInput`, `StyleRule` and
`StyleProperties` values. It compiles the same library once, interns strings for
bulk transport, and validates the returned spans before replacing a caller's
style list. Source bytes do not enter the VM. Invalid inputs remain recoverable;
compiler/runtime failures are reported explicitly.

Loom now uses this resolver for its actual Markdown and source-view styles.
The adapter maps semantic roles and inline flags to rules; EASL owns property
replacement and coalescing. Each pane retains a cache keyed by document revision
and source mode, invalidated by changes to its body or role styles. Resize and
caret repaint reuse it. Preedit retains the native editor's composition styles.
Integration tests cover current heading/code/inline roles, empty-heading carets,
source fidelity, shared undo, independent pane appearance and invalid-style
recovery. Both EASL evaluators check capacity/work rejection before publication.
Loom's native layout consumes the resolved spans. Its final glyph positioning
and decoration geometry now also run through EASL as described below. Parley
still supplies shaping, line layout and caret geometry; Vello rasterizes fonts.
These policy adoptions do not complete atlas integration, OS input, full
interface or interactive-latency acceptance.

## Rich glyph layout in EASL

[`library/rich.easl`](library/rich.easl) applies resolved styles to native shaping
results. Logical size, letter/word spacing, mixed-size baselines, underline and
strikeout geometry remain EASL policy. It does not choose fonts or alter source.

- `text-style-glyphs` scales offsets and advances from atlas pixels to logical
  pixels, then adds tracking per extended grapheme and word spacing for U+0020
  and U+00A0. Apply it once before measuring, caching or placing. Cluster IDs,
  byte ranges, mark order and shaping-safety flags remain unchanged.
- `text-tracking-features` disables optional `liga`/`clig` for nonzero tracking,
  unless explicitly set by the author. Required script ligatures stay enabled.
  It validates at most 64 distinct printable tags before emitting any settings.
- `text-rich-line` combines actual font metrics, logical size and half-leading
  into a shared baseline. The explicit strut preserves empty insertion styles.
- `text-rich-decorations` emits colored logical rectangles using independent
  font underline/strikeout offsets and thicknesses. Missing thickness falls back
  to 1/16 em. Display scaling and pixel snapping belong to the painter.
- `text-place-scaled-run-geometry` scales atlas ink while consuming already
  styled advances; its UVs still address the original raster. Quads and editable
  cluster cells therefore share one logical coordinate system.

Glyph styling validates the full input and work/capacity bounds before writing.
Readers must remain stable and must not alias output; native grapheme facts must
cover the source. Limits are 65,535 glyphs/facts, 1 MiB source and one million work
units. Each of two passes reserves two visits per glyph for validation/emission,
covered grapheme visits and 33 units per cluster for boundary lookup. These are
algorithm bounds, not elapsed time. Status 1 is invalid input, 2 exhausted
work/capacity and 4 a negative final
cluster advance, which the current prefix/caret contract cannot represent.

The existing [`line_layout.easl`](examples/line_layout.easl) consumer now accepts
whole-grapheme `TextStyledSpan` partitions through `line-layout-styled`, plus a
font-instance resolver and strut. It retains per-quad colors and per-run
decorations, uses flat feature-set ranges and borrowed language handles, and
keeps measurement and cells aligned with styled advances. Native identity runs
skip the extra transform. `line-close` releases its owned empty-language handle;
the caller owns language tables. Explicit rich spans select among three atlas
slots and currently bypass the default-style fallback search. The caller must
provide the matching face, variations, features and language, and invalidate
preparation after changing these tables.

Both evaluators exercise real mixed-size/variable-font lines, feature overrides,
combining-mark cells, RTL cluster placement, empty insertion styles, resource
cleanup and failure recovery. The styled offscreen scene uses this same consumer.
Rich multi-paragraph widget integration, fragmented grapheme style edges, a
general font pool and Loom's atlas painter remain open.

## Actual native drawing through EASL policy

[`library/painting.easl`](library/painting.easl) places already shaped,
visually ordered glyphs from their offsets and advances. It shares decoration
bounds with `rich.easl`, resolves font metrics against explicit decoration
overrides, and preserves independent colors and explicit zero thickness. Signed
advances and decoration widths remain available for overlapping tracking. Both
functions validate the complete input before emitting; stable readers must not
alias output. Glyph runs admit 65,535 records and reserve four work units per
glyph. Coordinates and accumulated pen positions are bounded to ±1e9 logical
pixels. Invalid input returns status 1; capacity/work exhaustion returns 2.

`GlyphPainting` is the typed Rust adapter used by the **actual Loom renderer**
for manuscript text, UI labels and list markers. It compiles the shared EASL
policy once, compiling only painting and shared decoration geometry rather than
the unrelated paragraph/editor modules. It transports runs in batches of 1,024 glyphs and retains the absolute
f32 pen between batches. A bounded cache stores at most 256 runs / 2 MiB
of input/output allocation capacity (plus bounded entry metadata). Unchanged draws, host-origin scrolling and display-scale
changes reuse geometry. Glyph IDs and geometry are validated again at the native
boundary. No manuscript bytes, history or configuration enter this policy.

`RasterSurface::text_with` keeps font instances, native rasterization, clipping
and display scale with the host. It stages all visible runs before changing the
raster command stream, so an invalid late result cannot partially draw text or
leave an unmatched clip. The legacy `text` entry point remains a native consumer
and comparison oracle. A text draw admits at most 1 Mi glyphs and 65,535 visible
runs. The EASL adapter is allowed to process runs larger than its cache; its batch
size does not impose a smaller document limit.

Checks compare actual shaped positions and pixels against that native path for
mixed sizes/scripts, combining marks, decorations, clipping and display scales
1, 1.5 and 2. Long runs cross multiple transport batches without changing f32
accumulation. Both EASL evaluators exercise capacity/work rejection, malformed
late input, distinct decoration colors and explicit overrides. Loom's current
renderer checks exercise both appearances and configured panes; unchanged
repaint reuses the EASL cache. Parley still owns Loom's line/bidi/caret layout,
and Vello remains its rasterizer. This is actual paint-policy integration, not
completion of the independent EASL atlas widget or native acceptance.

## Editor state in EASL

The general editor core owns selection, history, composition and resource lifetime
in EASL. It does not depend on Parley, `loom-markdown`, or a custom Rust text host.
Native `TextBuffer` primitives provide immutable, exact UTF-8 values and Unicode
boundary facts. Handles pass through ordinary EASL functions and struct fields;
editing does not copy the complete document into VM word arrays. Bulk arrays are
used when text enters from OS input or leaves for font shaping.

```lisp
(import "path/to/library/editor.easl")
(var document-editor: TextEditorState)
@cpu
(defn main []
  (let [initial (make-text "A page begins here.")
        addition (make-text "Café")]
    (text-editor-open document-editor initial)
    (text-editor-commit document-editor addition)
    (text-editor-history document-editor false) ; undo
    (text-editor-history document-editor true)  ; redo
    (text-release initial)
    (text-release addition))
  (text-editor-close document-editor))
```

Start each widget with a zero-initialized `TextEditorState`; pass it by mutable
reference rather than copying an owning state. `open` borrows the initial value
and retains its own handle. `close` releases current text, preedit and history.
The current/visible value is borrowed. `text-editor-copy` returns a new owned
selection value, which can be committed to another editor before release.
`text-retain` creates an independently releasable handle sharing immutable bytes.
Released handles fail explicitly and are never reassigned to a newer value.

`text-editor-edit` uses `TextEditCommand`; `commit` replaces the selection as one
undo entry. `preedit` keeps committed bytes and history unchanged, validates UTF-8
cursor offsets, and can be canceled independently. `text-editor-input` consumes
the existing `TextInputEvent` plus a borrowed payload from its exact byte range.
It rejects reversed ranges and mismatched payload lengths before changing state.
It handles commits, composition/focus loss, select-all, deletion, undo/redo and
logical horizontal/line navigation. Status 4 means the event was unhandled.
`view.easl` adds visual movement and pointer interaction over supplied layout cells.
Clipboard device I/O and accessibility still require widget integration. Word movement currently
uses adjacent UAX29 word boundaries; full platform-specific word behavior remains.

Each history stack keeps at most 64 snapshots and 8 MiB of UTF-8 payload. Native
values are capped at 1 MiB each, with 16,384 live handles and 128 MiB of conservatively
reserved payload per evaluator. Native replacement currently copies the edited
value; persistent rope storage is not implemented. Invalid ranges, oversized
values and exhausted resources fail before replacing existing text.

`make-text` takes an EASL literal; quoted strings retain literal backslashes.
Use `text-from-utf8` with a global `[u32]` byte array and start/end offsets for
runtime input. Assign `text-utf8` to an array global for output. `text-slice` and
`text-replace` use ordered UTF-8 character ranges. `text-boundary` modes 0–3 are
previous/next/floor/ceil grapheme, 4–5 previous/next UAX29 word boundary, 6–7 logical
line start/end, and 8 the floor UTF-8 character boundary, including the cursor.
Imports currently share a namespace, including the rule against parameter names
shadowing globals; use application-specific names for consumer globals.

[`examples/editor_specimen.easl`](examples/editor_specimen.easl) renders bytes
produced by an EASL edit/undo/redo sequence using the font atlas and EASL shaders.
This verifies the component connection; it is not an interactive widget review.
Loom retains its shared Rust Markdown transactions and history and continues to
consume the existing shared editing-policy module. Its painter has not yet been
replaced by this general editor/atlas path.

## Geometry and view interaction in EASL

`text-place-run-geometry` emits glyph quads and `TextClusterBox` records from the
same shaped advances. Ink overhangs and accents do not determine caret positions;
spaces retain editable advance geometry. Supply a line index, top/height band and
source offset when placing each run. Runs must already have their script,
direction and position resolved; this function does not resolve paragraph bidi.
The existing `text-place-run` delegates to that same placement pass.

`text-build-cells` reads the placed clusters and emits one `TextCell` per extended
grapheme. It validates source boundaries, finite geometry and output capacity
before emitting. Status 1 is invalid input; 2 is capacity/work exhaustion. A call
permits 65,536 clusters and 1,048,576 output cells. Readers must expose a stable
snapshot, and emitters must provide the declared capacity. Empty lines can be
represented by an empty source range with a positive line height.

Ligature advances currently divide evenly among extended graphemes. This is an
explicit fallback in EASL; font GDEF caret positions and script-specific caret
policies are still required. Combining/emoji sequences are not divided into
bytes or individual Unicode scalars.

`text-hit-test`, `text-caret-at`, `text-move-horizontal`, `text-move-vertical` and
`text-selection-rects` consume those cells. Caret affinity distinguishes a previous
grapheme's trailing edge (0) from the next grapheme's leading edge (1) at a wrap
or bidi run boundary. Highlights can have disjoint rectangles. Zero-advance
graphemes remain keyboard editing steps. Queries currently scan the supplied
cells; line indexes, viewport narrowing and large-layout performance work remain.

Use one `TextViewState` per view with the shared `TextEditorState`:

- Capture `(text-editor-visible editor)` as the **borrowed** layout identity.
  Independently retained handles are different identities even for equal bytes.
- Build the cells, then pass that identity and the stable cell reader into
  `text-view-pointer` and `text-view-input`.
- Rebuild after visible source or layout parameters change. Status 5 rejects
  stale source geometry; status 3 preserves active composition when pointer or
  layout navigation would otherwise alter it. Status 4 is unhandled.
- Vertical movement retains the preferred x coordinate across short lines;
  key release does not reset it. Physical arrows, shift selection, Home/End and
  macOS Command-Left/Right use the layout. Word/document commands use the core.

[`examples/view_specimen.easl`](examples/view_specimen.easl) is a standalone
single-line consumer with EASL selection/caret painting and preedit underlining.
Its shared [`examples/line_layout.easl`](examples/line_layout.easl) now combines
script itemization, per-line bidi ordering and run planning before shaping and
placing mixed-direction text. This single-line scene keeps Amiri as its explicit
font; the contextual multiline consumer enables the EASL cascade described below. This
scene stages replacement quads/cells, publishes them with their source identity
only after success, and caps scalars/runs/glyphs/cells at 4,096 each. Status 4
rejects tabs/hard breaks that need multiline flow. Other failures retain prior
text pixels, disable stale caret/selection geometry and preserve exact source
and undo. Undo back to the cached source also clears failed-layout state.
Its offscreen entries exercise a pointer-selected ligature and a composition
preview, checking exact committed/preview bytes and resource cleanup. The optional
`view-window` entry consumes native ordered input and mouse snapshots, rebuilding
on text changes. It has only been compiled, not reviewed interactively. The
multiline consumer described below adds wrapping and caret-reveal scrolling.
Rich editing, OS IME, clipboard/accessibility acceptance and Loom painter
integration remain open. Studio and its preview adapter have been removed;
standalone EASL entries and native runtime tests retain library coverage.

## Multiline flow and viewport geometry in EASL

[`library/flow.easl`](library/flow.easl) connects the atlas records to the
paragraph composer and editor geometry. The native `text-graphemes` primitive
returns an array of `TextGrapheme { start, end, scalar, break-after }`: exact
extended-grapheme UTF-8 ranges, first scalar and Unicode line-break facts.
Break-after is 0 (prohibited), 1 (opportunity) or 2 (hard separator). EOF alone
is an opportunity; CRLF is one grapheme. ICU4X 2.2 supplies line opportunities,
including its automatic complex-language segmentation; the existing Unicode
segmentation primitive supplies graphemes. These facts do not select line ends.

A consumer provides stable indexed callbacks and output storage:

1. `text-flow-spans` combines graphemes with a shaped logical-order LTR run.
   It keeps ligatures together, retains source ranges and represents hard
   separators without ink. Invalid ranges and insufficient capacity publish
   no spans. A tab has no atlas ink or fixed advance; a shaped cluster cannot
   cross it. Discretionary hyphens still return unsupported status 4.
2. `text-flow-points` takes the measure, an explicit emergency-wrap flag and
   `TextTabs`. It emits `TextFlowPoint` records and a separate `TextFlowTab` table.
   A point's `mark` contains its `TextBreak`; `tabs` and `trimmed-tabs` count tabs
   at the full and trimmed prefixes. Prefix advances exclude tab widths.
   Cache spans with source/font identity; points also depend on width and policy.
   Strict mode retains Unicode opportunities. Emergency mode splits only
   overlong unbreakable segments into maximal fitting groups of shaped clusters.
   Its two linear passes validate capacity before emitting any points. It does
   not add a candidate at every cluster boundary: a 5,000-cluster token can
   produce 500 lines within the existing 4,096-point bound.
3. `text-compose` selects complete paragraphs. Its point reader returns `mark`;
   its measurement callback calls `text-flow-measure` with the two flow points,
   the same tab settings, table capacity/reader and remaining work budget.
   Lines without tabs use an O(1) prefix difference. Other lines visit only
   their intervening tabs, charging one work unit each. Fitting segments keep their
   original opportunities and paragraph-wide composition. A cluster wider than
   the measure remains whole on an explicitly authorized overflow edge; the
   consumer includes its actual width in scroll extent. `TextBreak.overflow-start`
   identifies the sole allowed start for such an edge; `4294967295u` disables it.
   Native prepared-paragraph consumers keep their strict no-fit contract.
4. `text-flow-place` validates the complete chosen break list, then emits
   glyph quads, cluster boxes and `TextFlowLine` records. Trailing ASCII spaces
   and separators remain editable source positions with zero edge width.
   Empty text, blank lines and a final hard separator retain editable lines.
   Its emergency flag permits breaks at shaped-span boundaries; real hard
   separators must still end their lines. Supply the same policy used for points.
   Tab positions, caret cells and line widths use the same compensated global
   prefixes as measurement, including at fractional stop thresholds. Unrepresentable
   stops are rejected before placement emits any geometry.
5. `text-build-cells` supplies the existing editor's hit, selection and caret
   geometry from those same placed advances.

`TextTabs { interval, minimum }` defines uniform stops relative to each line's
start. Both values must be finite and nonnegative, `minimum <= interval`, and
`interval <= 10,000,000`. A positive interval advances to the next stop, skipping
it if the distance is below `minimum`; zero/zero gives zero-width tabs. Unlike
trailing ASCII spaces, tabs retain their width at line ends. A tab remains one
exact-byte, selectable grapheme; drawing never substitutes spaces or paints a
font's control/notdef glyph. The consumer uses four shaped space advances per
stop and a half-space minimum, calculated once during font setup. Other consumers
can supply their own settings. This is an explicit editor whitespace policy,
not a claim of complete CSS whitespace behavior.

Both CPU evaluators check stop geometry, fractional thresholds, retained trailing
tabs, absent control glyphs, ligatures/combining text/CRLF, narrow wrapping,
capacity, representability and work failures. Composition costs and placed widths
also match an independent exhaustive partition oracle over 24 tabbed cases.

Emergency wrapping never inserts hyphens or edits source bytes and never splits
an extended grapheme or shaped ligature. This follows the source-preservation
and grapheme requirements of [CSS overflow wrapping](https://www.w3.org/TR/css-text-3/#overflow-wrap-property),
with conservative whole-shaped-cluster boundaries. It is not a CSS intrinsic-size
implementation or paragraph-wide optimization of all emergency candidates.

Flow inputs are bounded to 65,535 graphemes/glyphs, 4,096 break opportunities
(including the initial point) and 4,096 output lines. Composition retains its
existing edge-work budget. Status must be checked at each stage; consumers must
stage replacement buffers and publish source/font/width identity together after
success. Callbacks must remain stable and outputs must not alias inputs.

[`library/viewport.easl`](library/viewport.easl) validates an index over cells
in consecutive, nonoverlapping line bands. `text-visible-lines` uses binary
searches to return a half-open visible range. `text-indexed-hit` searches line
bands, then only the selected line's cells. `text-scroll-reveal` computes a
clamped content-space offset for a caret/selection rectangle. Translate pointer
coordinates into content space and subtract the offset when painting; scrolling
does not invalidate shaping or the index. These are geometry policies, not
native wheel-event or focus handling.
The Rust `Viewport` binding runs the same `text-scroll-reveal` function over
native layout facts. Loom's transient single-line inputs use it for horizontal
caret reveal, with the existing EASL editing and hit-test bindings. It does not
own their text, history, clipboard or OS composition events. Invalid geometry
returns an error without replacing the caller's previous offset.

[`examples/flow_specimen.easl`](examples/flow_specimen.easl) is a standalone
multiline consumer. It preserves selection and source through two width changes,
with one shaping pass, and renders a cross-line pointer selection. Its scroll
proof reveals the final caret in a smaller viewport and draws only the indexed
visible glyph range. Line bands share edges even with fractional font metrics. Its fixture
buffers are disposable and errors abort the proof. This remains a Latin/LTR
flow; the contextual document consumer below integrates script/bidi runs and
reshaping at chosen line edges. Fallback, discretionary hyphens, rich styles,
incremental document layout and real OS interaction remain open.

## Bidirectional paragraph facts and line ordering

`text-directional-facts(TextBuffer, direction)` supplies paragraph-resolved
Unicode facts in original scalar/UTF-8 order. Direction is 0 (automatic), 1
(explicit LTR) or 2 (explicit RTL). Each `TextDirectionalFact` has eight u32
fields: `start`, `end`, `scalar`, `level`, `paragraph` (its starting byte), `base`
(0/1), `reset` and `script` (raw OpenType tag). Reset classes are 0 ordinary,
1 whitespace/isolate formatting, 2 segment/paragraph separator, and 3 retained
X9 formatting/boundary neutrals. Script zero means Common, Inherited or Unknown.
The primitive accepts the existing 1 MiB text bound and rejects unknown directions.
It owns no selection, line choice or document state. Cache its output with the
source and base-direction identity; line reordering does not repeat analysis.

The pinned `unicode-bidi` 0.3.18 dependency provides Unicode 16 paragraph levels.
Raw script tags now come from ICU4X 2.2's Unicode 17 properties, also used by
`text-script-facts`. The font primitive now uses HarfRust 0.12.0, the same
previously locked version used by Parley, with explicit resource-limit failures.
Property recognition alone still does not establish font coverage or complete
script-specific typography.

[`library/bidi.easl`](library/bidi.easl) owns the line-dependent policies from
[UAX #9 L1/L2](https://www.unicode.org/reports/tr9/#Reordering_Resolved_Levels):

- `text-bidi-order` takes a half-open range of scalar fact indices within one
  paragraph. It resets trailing whitespace and relevant controls, retains X9
  controls' source positions, then reorders `TextBidiSlot { fact, level }` records.
  The slots map visual positions to original logical fact indices. A consumer
  supplies one scratch array through read/write callbacks, separate from facts.
- `text-bidi-runs` coalesces that validated permutation into visual-order
  `TextBidiRun { start, end, level }` records. Every byte range remains in logical
  order. Pass logical substrings and the run direction to shaping; never reverse
  UTF-8 or combining characters to obtain an RTL string.

Preflight rejects malformed/cross-paragraph ranges, capacity failures and work
exhaustion before publishing a permutation. There are at most 65,535 scalars per
selected line and a one-million-unit ordering budget, reserving
`count * (4 + 2 * maximum_level)` units. Run collection separately preflights its
output capacity. Readers must remain stable except for explicit scratch writes.
The caller must choose actual line ends at valid grapheme/shaping boundaries;
scalar-index addressing does not authorize breaking a combining sequence.

Both CPU evaluators compare all output levels, permutations and level-run ranges
against the native implementation's independent L1/L2 path for 540 selected lines:
RTL/LTR/automatic paragraphs, numbers, nested embeddings, isolates, controls,
soft-line trailing whitespace and multiple paragraphs. A real Amiri shaping and
EASL geometry replay retains both visual caret positions at an English/Arabic
boundary, using logical substrings and three directional runs.

These are level runs, **not complete shaping runs**. Resolve script context in
logical paragraph order first, then intersect script/style/font spans with the
selected line's level runs. Shape each intersection in logical source order;
place its clusters according to its resolved direction. `runs.easl` now performs
this intersection, and both single-line and contextual multiline EASL consumers
use it. The original Amiri fixture remains a focused three-level-run regression.
The original `flow.easl` still assumes Latin/LTR. Host font discovery, rich styles
and integration into Loom's retained painter remain required for general
multilingual editor acceptance; the contextual consumer now selects explicit
fallback fonts through `fallback.easl`.

## Script context belongs to EASL

[`library/script.easl`](library/script.easl) implements script-run selection using
[UAX #24's rendering guidance](https://www.unicode.org/reports/tr24/tr24-39.html#Implementation_Notes).
It resolves Common/Inherited characters, intersects Script_Extensions sets, keeps
whole extended graphemes together and carries opening-bracket context across
script changes. Matching closing brackets use that context when possible;
quotation marks remain ordinary context-sensitive characters because their
opening/closing meaning requires language information. It does not reverse source.

`text-script-facts(TextBuffer)` is a bulk native property primitive, not a run
builder. Each `TextScriptFact` has `start`, `end`, `grapheme`, `grapheme-end`,
`scalar`, `primary`, `bracket`, `pair`, `extensions-low` and `extensions-high`.
The last two fields are `vec4u`; the complete record is 16 words. Ranges use
original UTF-8 bytes. `primary` and set bits use ICU's public UScriptCode values:
Common 0, Inherited 1, Unknown 103, Latin 25, Arabic 2. The two vectors encode
256 bits in increasing code order. Implicit values remain in the native facts;
the EASL policy decides how to interpret them. Bracket kind is 0 ordinary, 1 open,
2 close; `pair` is the canonical opening scalar, including U+2329/U+3008 equivalence.
The existing 1 MiB source bound applies. Future property data that exceeds the
checked mask representation fails explicitly instead of truncating scripts.

```clojure
(import "../library/script.easl")
(var script-facts: [TextScriptFact])
(var script-runs: [TextScriptRun])
(defn script-fact-at [i: u32]: TextScriptFact (script-facts i))
(defn emit-script-run [i: u32 item: TextScriptRun] (= (script-runs i) item))
; In the CPU callback, after source changes:
; (= script-facts (text-script-facts source))
; (= script-runs (zeroed-array (array-length script-facts)))
; (text-itemize-scripts (array-length script-facts) script-fact-at
;   begin end 25u (array-length script-runs) emit-script-run 1000000u)
```

`begin`/`end` select scalar fact indices in a paragraph scope, containing whole
graphemes and at most a terminal hard-break grapheme (CRLF stays together).
Cache analysis and itemized runs by source/scope/fallback identity; resizing alone
does not change script context. The fallback is a known concrete UScriptCode used
for otherwise unresolved text. Output is logical-order
`TextScriptRun { start, end, script-code }`. Convert the code with
`text-script-tag(run.script-code)` when calling `shape-font-run`; codes and
[OpenType tags](https://learn.microsoft.com/en-us/typography/opentype/spec/scripttags)
are different namespaces. Unsupported or implicit codes fail the native conversion.

The first explicit primary within a grapheme supplies its script constraints.
Otherwise the first constrained extension set supplies them. Leading weak text
acquires subsequent compatible context; trailing weak text stays with its run.
An ambiguous extension set prefers the current primary, then the caller's fallback
when permitted, then the lowest remaining code. This is a documented rendering
policy, not language detection or proof of a suitable fallback font.

Status is 0 success, 1 invalid range/facts, or 2 capacity/nesting/work exhaustion.
Bounds are 65,535 selected scalars, 128 pending bracket openings and 1,000,000
work units. Validation, two deterministic passes, scalar visits, bracket searches,
pending-bracket fixups and bounded script selection share that budget. Stable
readers and nonaliasing output are required. The first pass proves success before
any run is published; malformed input and even late exhaustion preserve output.

Both evaluators exercise nested/mismatched brackets, Japanese/Syriac shared
characters, Indic/Hebrew combining marks, emoji, Kawi, paragraph scopes and work
limits. Every Unicode scalar's property set is checked against the transport.
Real Shantell Sans shaping of Latin/Cyrillic runs feeds EASL atlas placement and
20 exact-source caret cells, with hit checks at script boundaries. That is a
component replay, not font fallback, mixed-direction multiline or Loom acceptance.
The Unicode fixtures also exposed and fixed scalar-only vector bitwise emission
in the VM and grapheme-based delimiter scanning in the compiler's lexer.

## Script, direction and font/style intersections

[`library/runs.easl`](library/runs.easl) supplies `text-plan-runs`. Its inputs are:

- The immutable paragraph `TextBuffer` and the selected line's visual-order
  `TextBidiRun` records from successful `text-bidi-order`/`text-bidi-runs` calls.
  The planner trusts that permutation; it validates ranges and boundaries but
  does not independently re-run the bidi algorithm.
- Logical `TextScriptRun` records covering the entire paragraph, resolved before
  choosing the line. Rewrapping must not change script context.
- Logical `TextFontRun { start, end, font, style }` records covering the same
  paragraph. Font/style IDs index the caller's immutable table. EASL chooses
  these spans; they are not native file handles or automatic fallback results.
- Output capacity, an emission callback and a work budget.

Every output `TextShapeRun` has `start`, `end`, `script-code`, `font`, `style`,
`level`, `context-start` and `context-end`. Runs are in visual order; each source
range stays logical. Odd-level intersections are visited backwards without
reversing scalars or marks. Shape a context slice bounded by the output's context
range, passing `start/end - context-start` to `shape-font-span`. This preserves
joining across font/style changes but stops it at actual line and level edges,
as required by [UAX #9 shaping](https://www.unicode.org/reports/tr9/#Shaping).
Reuse the context slice for adjacent intersections of the same level run when
useful. Add `run.start` once when placing span-relative glyph/caret ranges.

The planner validates logical partitions and whole-grapheme boundaries, then
makes a dry pass before publishing. It permits at most 65,535 records in each
input/output and a one-million-unit budget. Validation reserves one unit per
input; each pass reserves 33 per level run for two binary searches and one per
output. Thus successful reported work is `scripts + fonts + levels +
2 * (33 * levels + outputs)`. These are algorithmic work units, not
wall-clock time or a bound on native segmentation/shaping. Readers must remain
stable and output cannot alias inputs. Status 0 succeeds, 1 rejects malformed
partitions/ranges, and 2 reports capacity/work exhaustion; failures emit nothing.

Status 3 reports a directional level edge inside an extended grapheme. For
example, U+0600 followed by Latin `a` is one grapheme but has two bidi levels.
Correct fragmented-grapheme shaping/caret support remains open; the planner does
not silently normalize levels or create an extra editor caret. Font/style edges
inside a grapheme are likewise unsupported and rejected as malformed input.

Both CPU evaluators exercise visual intersections, selected-line contexts,
malformed/capacity/work failures and 4,096 style edges. A two-font test feeds real
Latin/Cyrillic/Arabic shaping into atlas placement and 27 exact-source caret
cells. Arabic style subdivisions retain the independently pinned medial glyph;
a chosen line containing only that character produces its isolated glyph. Earlier offscreen integration exercised mixed-direction commit, combining-mark preedit,
unsupported-layout retention and undo recovery without modifying the EASL source
file. The contextual document consumer below reuses this planner. Loom's native
painter remains unchanged.

## Contextual widths and multiline layout

[`library/measure.easl`](library/measure.easl) accumulates shaped runs into
compensated logical prefix widths with a safety bit at every grapheme boundary.
`text-measure-run` consumes complete native grapheme facts and logical-cluster-order
glyphs; different runs can arrive in visual order. Its two passes validate ranges,
coverage overlap, capacity and work before modifying staging slots. Initialize
`TextMeasureSlot` storage to zero, with one slot per grapheme plus the final edge.
Glyph advances may be signed; each complete cluster must have a nonnegative
advance for reusable logical prefixes.
`text-measure-prefixes` checks complete coverage and finite accumulated widths
before publishing any `TextShapedPrefix`. Status 1 rejects malformed input;
status 2 reports capacity/work exhaustion. Native fact/glyph readers must stay
stable; the slot reader and writer intentionally share staging storage.

An interior boundary is reusable only when it begins a complete cluster without
the shaper's unsafe-to-break flag. A boundary inside a ligature stays unsafe.
Widths between two safe edges are constant-time prefix differences. This permits
reflow of unchanged shaping; it does not permit reuse across edits or changes to
font, size, script, direction, context, language or features. The source edges
are safe for the same complete paragraph. Missing glyphs do not establish font
coverage or fallback.

[`examples/contextual_layout.easl`](examples/contextual_layout.easl) integrates
these measurements with script/bidi run planning, contextual line composition,
atlas placement and source cells. It prepares the paragraph once, reuses complete
safe clusters, and reshapes unsafe candidate and selected line edges with their
exact line context. Ordinary Unicode opportunities are tried first, followed by
grapheme emergency wrapping. It minimizes overflow before raggedness, so an
allowed oversized character cannot displace a fitting contextual line. Selected
line measurement and placement use the same cache decision and must agree.
CRLF remains one source cell; blank and trailing empty paragraphs retain carets.
Trailing ASCII spaces keep zero-width cells at the visual trailing edge. All
document buffers publish together after line and cell validation succeeds.

The consumer accepts the explicit font cascade below, at most 4,096 scalars per paragraph,
4,095 graphemes per document, 4,096 glyph quads/lines and 8,192 cells. Composition
and selected-line planning share a one-million-unit document budget. Native
analysis/shaping and preparation have their own bounds; this is not a wall-clock
deadline. Single-paragraph width reflow retains shaping. Multi-paragraph cache
retention, contextual tabs, discretionary hyphens, host font discovery, rich styles
and fragmented-grapheme geometry remain open. The Latin/LTR tab-capable path
remains available in the same editor.

Both CPU evaluators test real Amiri safe spans against independently reshaped
substrings, malformed/overlapping/incomplete measurement input, exact source
cells, wrap affinity, CRLF/empty paragraphs and width reflow without new shaping.
The Arabic fixture `لملم` at 14.3px must become two fitting `لم` lines; the isolated
letters are wider. Exhaustive small nonmonotone partitions check fitting versus
unavoidable overflow and last-line weighting. These are bounded component
checks, not complete shaping or multilingual editor certification.

## Font fallback belongs to EASL

[`library/fallback.easl`](library/fallback.easl) selects fonts from actual
`FontShapedGlyph` records. Initialize one `TextFontChoice` per native grapheme
with `text-font-unset`, then call `text-font-consider` for contextual spans in
candidate preference order. A complete grapheme stays in one font, including
combining marks. Prefer the candidate with the fewest `.notdef` glyphs; ties keep
the earlier candidate. This score is glyph coverage, not a quality judgment about
emoji sequences, language, ligatures or typography. If no font covers a grapheme,
keep the best available choice and preserve its source and caret.

Each consideration checks exact contiguous span coverage, whole-grapheme cluster
edges, capacities and work before writing choices. `text-font-runs` validates
complete choices and coalesces them into the logical `TextFontRun` partition used
by `text-plan-runs`. A `style-at(index) -> u32` callback supplies the style ID for
each complete grapheme; equal fonts coalesce only when the styles also match.
Styles remain caller-owned and immutable during planning. Its `missing` result
counts unresolved graphemes. Facts must
be the immutable native grapheme partition; only the choice reader may alias its
writer. Output font runs must not alias their inputs. Per-call limits are 65,535
graphemes/glyphs, font IDs below 64, 1 MiB source offsets and a one-million-unit
inspection budget. Native shaping has independent glyph/operation bounds.

The contextual consumer uses Amiri, Shantell Sans and Noto Sans Kawi as an
explicit fixture cascade. Startup calls `line-enable-cascade` to load the additional
atlases and invalidate paragraph preparation. It probes script/level spans until covered,
then shapes selected font runs with their actual line context. Width-only reflow
retains font choices. The cache identity includes source and the immutable cascade;
changing fonts must invalidate preparation. The default strut accommodates the loaded fonts; rich lines combine their
selected run metrics with the supplied insertion strut. Final quads carry flat font IDs, and `contextual-draw` renders all
atlases in one draw in visual order, without uploading geometry per font run.
Rust supplies the existing font/Unicode primitives; no native fallback selector
participates in this consumer.

Both evaluators cover real Cyrillic fallback, three-font contextual layout,
combining sequences, unresolved glyphs, malformed spans and capacity/work failure.
The removed Studio integration previously exercised Latin-only missing-glyph handoff, mixed
Latin/Cyrillic/Kawi/Arabic painting, width reflow without new font probes,
unresolved-character editing and exact undo/source preservation. Its captured
frame is component rendering evidence, not native application acceptance.
Cold startup remains timing-sensitive: the consolidated run recorded one
20-second first-frame timeout; the unchanged failed test and complete multiline
suite passed on rerun without extending the deadline. Startup robustness remains
an open gap.
`cascade-probes`, `ctx-font-mask`, `ctx-missing-glyphs` and
`line-unresolved-graphemes` make selection work and remaining coverage gaps visible.
System font discovery, color emoji, general atlas page management and Loom's atlas integration
remain open. The font primitives now expose collection faces, variable-font
settings and language/features. The contextual widget accepts explicit rich
spans with these immutable tables; selecting fallback faces within rich spans
is still open.

## Viewport interaction and multiline editing

[`library/widget.easl`](library/widget.easl) connects the editor/view and line
index APIs without owning the consumer's layout arrays or document persistence.
Keep a `TextWidgetState` alongside each view and call `text-widget-reset` once.
`TextWidgetLayout` is a borrowed snapshot containing the visible source identity,
cell/line counts, viewport size and content extent. Its readers must expose the
validated cells and index from that same layout. Publish replacement geometry
before processing the next navigation event after text, font or width changes.

- `text-widget-pointer` translates viewport points by the scroll offset and uses
  indexed hit testing. Pointer press establishes focus; captured dragging may
  leave the view. It shares selection transactions with `text-view-input`.
- `text-widget-input` adds Page Up/Down and shift extension, retaining the preferred
  x coordinate used by vertical arrows. A page keeps one line of overlap. Other
  commands, exact text commits, history and composition use the existing modules.
  Physical Enter does not synthesize another insertion before its text commit.
- `text-widget-decorations` emits selection rectangles or per-line composition
  underlines and returns the caret and its visibility. All geometry is in content
  coordinates. Subtract the offset for drawing and the native IME candidate area.
- `text-widget-reveal` clamps scrolling to reveal that caret. Scroll/focus changes
  reuse the layout and shaping. Blur cancels composition and removes caret focus.

The source guard returns status 5 for stale geometry, and composition guards
preserve the existing status 3. Other validation/capacity/unhandled statuses are
inherited from the underlying modules. Consumers still route input only to its
focus owner, check every status and own the atomic publication of geometry.
Read-only `text-editor-visible` and `text-editor-copy` accept borrowed editor
state; they do not require transferring the editor's history value.

[`examples/multiline_text.easl`](examples/multiline_text.easl) is a complete
frame loop over these APIs. It shares font, shaping, flow and painting with
[`examples/flow_scene.easl`](examples/flow_scene.easl), also used by the retained
flow proofs. It processes ordered events individually and rebuilds after each
visible-source change before the next navigation. Latin/LTR documents use the
existing flow; other scripts, bidi levels or missing display glyphs use the
contextual consumer and its font cascade. Tabs and paragraph controls alone do
not trigger fallback.
Width changes reuse safe shaping;
page/selection movement reuses both shaping and layout. Its `main` works as a
standalone window entry. Native window acceptance remains open. The shared controller has both-evaluator CPU coverage.

The real offscreen test exercises wrapped multiline text, page selection, resize,
combining/multiline preedit, blur, a single Enter insertion and undo. It preserves
the `.easl` source. `multiline-text`, `multiline-visible`, `multiline-state`,
`multiline-scroll` and `multiline-frame` are explicit view exports; `flow-shapes`
and `flow-layouts` count work. They do not grant Loom document authority.

The offscreen tab regression pastes literal tabs, aligns three caret columns,
resizes without shaping, inserts one tab through an ordered key/payload event,
and undoes back to the original bytes. `flow-shapes` counts document shaping;
the space run used to configure tab stops is separate font-setup work.

The contextual offscreen test pastes mixed English/Arabic prose, resizes without
new shaping, previews and commits combining Arabic input, then checks explicit
unsupported-layout failure and undo recovery. `multiline-contextual` exposes
the selected path; `ctx-shapes`, `ctx-preparations`, `ctx-cache-hits` and
`ctx-measurements` expose work on that path.

Discretionary hyphens return status 4; contextual tabs are also unsupported.
Emergency wrapping keeps unbroken words editable, including when an indivisible
span exceeds the measure. Latin/LTR documents covered by the primary font retain
their tab support; documents needing the contextual cascade still reject tabs.
Failed layout preserves text/history and retains the last completed image with
IME disabled. Undo can recover even when it restores the last drawn text identity.
`flow-status` exposes the failure; user-facing error presentation remains open.
General rich fallback, broader multilingual coverage, wheel
transport, platform clipboard, native OS IME/accessibility and Loom integration
remain required. This is an integrated component, not full text-widget parity.

## Rich contextual document layout

`contextual-layout-styled(count, span-at, font-for-style, strut)` connects the
existing rich line consumer to the multiline editor, viewport and GPU painter.
Spans use the visible document's original UTF-8 offsets, including separators;
callers derive them again after text edits or preedit changes. Shared
`text-project-styles` fills gaps and projects styles into each paragraph without
changing bytes. Empty paragraphs retain insertion styles. Projection validates
the complete ordered input and capacity/work budget before emitting anything;
the consumer additionally rejects style edges inside extended graphemes/CRLF.

Each selected line combines its run metrics with its strut, shares a baseline,
and advances the next line by its actual height. Glyph ink, caret/selection
cells, line indexes, viewport extent and IME placement use those same bands.
Colors and underline/strikeout rectangles survive wrapping; glyph and decoration
draws submit only visible lines and share scroll coordinates. Published GPU
buffers remain separate from staging so late layout failure retains the prior
complete glyph, color, decoration and caret arrays.

The single-paragraph shaping cache compares source identity, actual style values
and selected font IDs. Width-only reflow reuses it; style changes invalidate it
even with unchanged bytes. Font/feature/language tables remain immutable between
preparations; resource changes must invalidate `line-prepared` and `ctx-prepared`.
Multi-paragraph cache retention, general font matching, rich fallback and a
formatting/source model remain caller/integration work. The consumer still has
three atlas slots and a 4,095-grapheme document bound. This is reusable EASL
layout consumed by the standalone widget; Loom retains native line/caret layout.

Both CPU evaluators check mixed sizes/colors, source cells, CRLF/empty paragraphs,
cache reuse/invalidation and late failure recovery. The removed Studio integration also exercised resize, IME preedit/commit and
undo; those historical results do not qualify the current standalone host.
Native application acceptance and interactive performance qualification remain open.

## Font atlas and native input primitives

[`examples/font_atlas.easl`](examples/font_atlas.easl) is an ordinary EASL program.
It uses the compiler/runtime built-ins below, imports the shared library, and
renders with EASL vertex/fragment shaders. No Loom or custom text host is needed.

```lisp
(= font (open-font "fonts/example.ttf" 48.))
(= bytes (utf8-bytes "Office, café, affinity."))
(= run (shape-font-run font bytes (text-script-latin) false))
(= ids (zeroed-array (array-length run)))
(for [i (array-length run)] (= (ids i) (.id (run i))))
(= font (rasterize-font font ids))
(= ink (font-atlas-glyphs font))
```

Declare `font` as a `(Texture2D f32)`, `ink` as `[FontAtlasGlyph]`, `bytes` as
`[u32]`, `ids` as `[u32]`, and `run` as `[FontShapedGlyph]`. Relative font paths resolve against the
entry file's directory. Resolution is pixels per em. Swash rasterizes outlines;
HarfRust shapes one explicit script/direction run. EASL chooses glyph positions,
reverses RTL clusters while preserving mark order, and generates the geometry.
`font-metrics` returns ascent, descent, leading and resolution. Atlas records
contain pixel rectangles, downward-positive baseline offsets and nominal advances.
Nominal advances can differ from shaped advances because shaping applies glyph
positioning and rounds variation deltas to whole font units; placement uses the
shaped advances.

`open-font` opens an immutable outline face and its shaping state, returning a
transparent 1×1 texture without rasterizing glyphs. `rasterize-font font ids`
builds a new page containing just the requested glyph IDs at the source page's
resolution. A third argument sets a new resolution: `(rasterize-font font ids 96.)`.
The page shares font bytes, collection face and normalized variation instance;
metrics and shaping scale to its own resolution. Shape/scale placement for that
resolution before using its ink. `load-font` remains available for consumers
that deliberately request a complete font atlas.

Each page is immutable: rebuilding or assigning a texture never changes an older
page's bitmap or UV coordinates. `font-rasterized-glyphs` returns its sorted unique
resident IDs, including glyphs with empty ink such as spaces. Duplicate requests
are accepted. `font-atlas-glyphs` remains indexed by every font glyph ID, but ink
rectangles for unrequested IDs are zero; consult residency before painting.
An empty request returns a transparent 1×1 page with usable shaping facts.
Requests allow at most 1,048,576 IDs; invalid IDs/resolutions and oversized pages
fail before replacement. Earlier pages remain usable. Both CPU evaluators support
texture-global assignment, including self-assignment; earlier queued draws retain
their resources when a later draw changes the bound page.

The importable `font_scene.easl` consumer now chooses its page after shaping.
Native pixel comparisons, both evaluator recovery checks and real offscreen GPU
draws exercise the path. This supplies the page primitive; a general EASL page
cache/eviction policy, rich-widget font-pool integration and native acceptance
remain open. Texture values and queued dispatch snapshots now share immutable CPU
pixel storage. The native renderer uploads a live read-only font page once per
GPU context and reuses it across bindings and subsequent draws. This also applies
to eager `load-font` atlases. Dimensions participate in resource identity, and a
writable replacement always detaches, including at equal dimensions. Earlier
draws keep their own inputs. The cache holds weak ownership: releasing all EASL
values and queued snapshots lets the next frame or page allocation retire the
page and its old binding references. Hot reload removes obsolete texture slots.

`GpuCore::texture_transfer_stats` reports upload count, actual GPU-format bytes,
read-only reuse and currently cached pages. An eight-frame fixture reduced the
same heading's transfers from eight uploads (46,923,776 bytes) to one
(5,865,472 bytes); CPU assignment/dispatch snapshots share its 2,932,736-byte
bitmap in both evaluators. GPU pixels, equal-size replacement, writable-target
detachment, explicit host copy-on-write and page retirement are checked. These
are component transfer results, not an interactive latency or whole-Loom
performance qualification. GPU-written texture snapshots still use the existing
RGBA8 readback representation; floating-point/HDR copy fidelity remains open.

### EASL page selection and deferred ink

[`library/pages.easl`](library/pages.easl) implements `text-page-plan`. Given
reusable `TextPageScratch` storage, required glyph IDs and the current page's sorted resident IDs, it retains a
covering page or emits the unique current request for a replacement. Glyph zero
is an ordinary resident. Duplicate requests do not consume extra capacity.
Validation and bounded work precede every output write. Status 0 is success,
1 invalid input and 2 exhausted work/capacity. The current bounds are 65,535
requests, 4,096 unique glyphs, a 65,536-glyph font and one million work units.
Readers must be stable and separate from output and scratch. Initialize scratch
to zero and reserve its mutation for the module. It clears only previously used
hash slots, counting those visits toward the work limit. This is one immutable page per
explicit font slot, not a general font matcher, multi-page packer or LRU pool.

`TextPlacedGlyph`, `text-position-scaled-run` and `text-flow-position` separate
logical placement/caret geometry from bitmap packing. They retain shaped offsets,
source ranges, baseline and ink scale. `text-placed-glyph-quad` resolves ink and
UVs after a consumer chooses the final page. Existing immediate-quad APIs wrap
the same positioning pass, preserving cluster order and advance geometry.

The single-line, plain-flow and rich contextual consumers now use this policy.
They collect the final composition's glyphs before rasterizing any replacement,
stage all changed font pages, then publish pages and project every quad against
those pages. Width changes that need no new glyphs preserve page residency and
UVs. Later paragraph failures preserve the published view. The explicit fallback
and variable-font specimens open native faces without eagerly rasterizing every
glyph. Independent cached layouts must retain their matching immutable page;
page replacement does not rewrite another layout's previously stored UVs.

CPU-only texture globals can now hold these pending/retained values without
occupying a GPU binding. Assign to an explicitly bound texture before sampling
or rendering. Shader entry points reject direct or transitive access to an
unbound texture; an unbound render target reports a recoverable runtime error.
These are host values, not externally serialized fields or general arrays of
texture resources. Loom's production experiment still uses its retained native
line/caret and Vello paths; this work advances the reusable EASL widget.

Shaped records contain `id`, `start`, `end`, `x`, `y`, `advance` and `flags`
(seven 32-bit words), with exact UTF-8 cluster ranges. Flag value `1` means breaking before
that cluster requires reshaping both sides; flag value `2` means text changes across the
boundary can affect shaping. Absence of bit 2 on one run alone does not establish
safe concatenation. These are shaping facts, not line-break policy. Load metadata
once, shape when text/context changes, and place from those buffers: there is no
per-glyph host call.
The current CPU VM requires global resource/array bindings and literal paths
and `utf8-bytes` strings; runtime text arrives in the UTF-8 array. Font atlases
are read-only render resources. Sampler bindings use linear clamp filtering.
The fourth `dispatch-render-shaders` argument can select replace=0u, add=1u,
or straight-alpha compositing=2u; existing boolean replace/add calls retain
their meaning. Use 2u for glyph coverage over existing content.

For runtime editor text, use the borrowed-source span primitive:

```lisp
(= run (shape-font-span font source start end (text-script-arabic) true))
```

`source` is a `TextBuffer`; `start`/`end` are half-open UTF-8 offsets on extended
**grapheme** boundaries. The host reads that span directly from immutable storage,
without expanding the source into an EASL `[u32]` and then copying it back. Adjacent
source supplies the shaper's context (up to five scalars per side). Cluster ranges
in the result are **relative to the span**; placement adds `start` exactly once.
If a line boundary should stop contextual joining, pass the bounded line source,
then shape its font/script spans. Do not treat a font change as a text boundary.
Cache identity includes source/context, range, font face and variation instance,
resolution, script, direction, feature settings and language. The whole-source `shape-font-run` call uses the same primitive.

Shaping rejects unknown/implicit script tags, split graphemes, invalid ranges,
stale text handles and sources over 1 MiB. It caps intermediate glyph buffers at
1,048,576 glyphs and the instrumented shaping operation counter at 16,777,216,
subject to smaller internal limits. Exhaustion is an explicit error and array
assignment publishes nothing; existing output remains available. This is not a
wall-clock deadline or a bound on every parsing instruction. Native validation
also checks glyph IDs, finite horizontal geometry and logical whole-grapheme
cluster ranges. HarfRust font-table caches live with the font resource.

The extended `open-font` and `load-font` overloads select a collection face and variable-font
instance; the extended `shape-font-span` overload applies per-span OpenType
features and language:

```lisp
(var axes: [FontVariation]) (var features: [FontFeature])
; wght = 0x77676874; liga = 0x6c696761 (four ASCII bytes, big endian).
(= axes (zeroed-array 1u))
(= (axes 0u) (FontVariation 2003265652u 700.))
(= font (load-font "fonts/example.ttf" 48. 0u axes))
(= features (zeroed-array 1u))
(= (features 0u) (FontFeature 1818847073u 0u))
(let [language (make-text "en")]
  (= run (shape-font-span font source start end (text-script-latin) false features language))
  (text-release language))
```

The face index is zero-based. Variation coordinates are user-space values, such
as weight 700 or italic 1, normalized once with the font's `fvar`/`avar` data.
Swash metrics and rasterization consume the same normalized coordinates as
HarfRust shaping. OpenType clamps coordinates to each supported axis; unsupported
axes and features are ignored by the font engine. This does not synthesize a
missing bold/italic face. Collection face selection also remains explicit; system
font discovery and matching are not implemented by this primitive.

`font-decoration-metrics(font) -> vec4f` returns underline offset/thickness and
strikeout offset/thickness in atlas pixels, with offsets positive above the
baseline. The native primitive reads separate `post`/`OS/2` values and their
`MVAR` adjustments at the same normalized instance. Missing values are zero;
EASL chooses fallback appearance. Controlled font-table tests distinguish the
two thicknesses, and both evaluators exercise the four-value transport.

Each settings array admits at most 64 unique printable four-byte tags. Variation
values must be finite and within ±1,000,000; fonts with more than 64 axes are
rejected. Features cover the entire selected span. EASL owns the font/style run
partition and must use the same features/language for fallback probes and final
shaping. Language is an immutable `TextBuffer`, empty for unspecified, or up to
64 ASCII bytes of hyphen-separated subtags (1–8 bytes, alphabetic first subtag,
alphanumeric remaining subtags). This is syntax validation, not language-registry
validation. It never reads the process locale. The short overloads use face zero,
default variations/features and unspecified language.

Settings arrays are named globals in the current VM, like the other bulk font
arrays. Failed loads and shaping calls leave their destination resource/array
unchanged, including invalid or released language handles. Native and both-CPU
checks cover independent HarfBuzz feature/language/variation cases, collection
faces, exact byte ranges, malformed settings and retained resources. The cached
[`style_specimen.easl`](examples/style_specimen.easl) uses real regular and bold
italic instances of one variable font with EASL placement and ink; it is an
offscreen component fixture, not Loom's painter or UI baseline.

The real-font tests compare selected Kawi/Arabic glyph IDs, mark order, positions
and flags against an independent HarfBuzz 12.3.2 oracle. Both evaluators exercise
span context, seven-word transport, failure retention and Kawi atlas/caret cells.
The bounded shaper tests include insertion beyond a glyph cap with preallocated
storage, work exhaustion, long runs and large mark clusters. These finite cases
are not a complete shaping or multilingual layout certification.

[`examples/style_specimen.easl`](examples/style_specimen.easl) uses the shared
rich line consumer to prepare two runs once, retain their quads/decorations in
independent dynamic globals, and choose each draw's ink and baseline in EASL.
The first run applies size, tracking, word spacing and underline; the second
uses an actual bold italic instance with strikeout. `style-close` releases its
owned text resources and allows repeated preparation. Ordinary whole-array
assignments such as `(= bound-quads cached-quads)` copy values in both CPU
evaluators; subsequent mutations do not alias the cache. The VM reuses allocated
word storage where possible and preserves lazy zero storage. This does not add
general dynamic-array expressions or local dynamic-array arguments. Differently
sized GPU bindings can still require buffer/bind-group rebuilding. The specimen
is a styled-run rendering check, not a rich-document editor or performance claim.

The reference tree evaluator borrows side-effect-free field/element projections
and copies only the selected value. Computed indices retain ordinary evaluation;
whole-array assignments and returned records remain independent values. It also
avoids recalculating effect sets outside function calls, where GPU synchronization
uses them. The bytecode VM now writes nested array mutations from children back
to their parent records, including compound assignments and mutable arguments.
Earlier argument write-back order is preserved. Tests compare lazy zeros,
nested copies, swizzles and side-effecting indices in both evaluators. The full
5,000-cluster synthetic layout/cell check now runs in both, although the reference
evaluator remains substantially slower than the production bytecode VM.
The compiler's closure-extraction pass now limits capture-effect analysis to
closures. Profiling an offscreen startup timeout located repeated transitive
effect walks on nonclosure expressions; those walks did not contribute to capture
analysis. The remaining compiler and interactive costs still need measurement.
Further startup profiling of the contextual editor found full transitive effect
collection in a shader-input-only pass. That pass now gathers builtin inputs
with one visit per reachable implementation per query. Argument lowering also
avoids an effect walk when no earlier argument reads need preservation. Generated
shader globals/entry inputs are checked against full effect analysis, including
shared helpers and explicit argument fields; wrong-stage rejection and CPU
mutation order remain covered. These reduce redundant analysis, without a
persistent cache that could become stale after compiler rewrites.

The atlas supports explicit faces of TTF/OTF outline fonts and collections,
1–256 pixels per em, a 64 MiB file limit and a 4096×4096 atlas limit. Unsupported fonts and
oversized pages fail explicitly; there is no silent fallback. Color fonts,
general atlas page management, system font discovery and intra-ligature caret data remain required.
Rich consumers must integrate the instance/language/feature primitives above. Explicit fallback policy
is supplied by the EASL library above. Both shaping builtins take one
known OpenType script and direction, not an arbitrary mixed-script paragraph.
The contextual consumer above owns that paragraph/run composition in EASL.

The window runtime supplies ordered `text-input-events` and `text-input-bytes`
snapshots; neither query consumes events. It clears them after the frame.
`TextInputEvent` has kind, key, modifiers, text-start/end and preedit cursor-start/end.
Kinds are text=0, key-down=1, key-up=2, preedit=3, IME-enabled=4, IME-disabled=5,
blur=6. Modifiers are shift=1, control=2, alt=4, command/super=8, repeat=16.
Cursor offsets are relative UTF-8 bytes; `u32::MAX` means no visible preedit cursor.
[`examples/input_policy.easl`](examples/input_policy.easl) imports and exercises
the command policy. Navigation, clipboard and history remain unhandled by that
basic translator. The retained native editor still supplies those operations.
The standalone winit loop supplies these facts. An embedding host implements
`IOManager` input snapshots and candidate-area methods. Blur cancels composition;
queries retain one immutable batch throughout a callback. Dynamic input arrays
must be stored in named globals before querying their lengths; direct dynamic
array expressions remain a VM limitation.

Native and captured rendering share ordered GPU execution: each draw observes
its own uploads and intermediate readbacks preserve preceding work. The runtime
retains completed images independently of a callback's unfinished drawing.
`tests/draw_order.rs` and `tests/texture_staging.rs` exercise native GPU order and
snapshot isolation without Studio. The bounded import loader accepts an
in-memory root plus its filename; native asset paths use that root's directory.
`tests/imports.rs` covers graph limits, dependency identity and bounded exports.
The removed Studio source editor, preview worker and pane host are not part of
this stack. Their earlier input/preview checks are historical evidence only.
The native mouse API still exposes per-frame snapshots, not an ordered pointer
stream; repeated complete clicks within one frame remain a gap.

`(text-input-area true (vec4f x y width height))` enables IME and supplies the
candidate-window anchor in physical pixels; pass `false` on loss of text focus.
Frames are bounded to 1024 events and 1 MiB of UTF-8. Invalid or overflowing frames
produce an error, not a silently truncated commit. Real OS IME and accessibility
acceptance, a fully EASL editable widget, and full Loom integration remain open.

```sh
export PATH="$(dirname "$(rustup which --toolchain 1.92.0 cargo)"):$PATH"
cargo test --locked --profile native-view -p easl-text -p easl-native-text
cargo test --locked --profile native-view -p easl-text --features gpu --test imports --test draw_order --test texture_staging
cargo run --locked --profile native-view -p easl-text --example specimen -- target/easl-library-specimen.png
cargo run --locked --profile native-view -p easl-text --features gpu --example atlas_specimen -- target/easl-atlas-specimen.png
cargo run --locked --profile native-view -p easl-text --features gpu --example atlas_specimen -- target/easl-editor-specimen.png crates/services/easl/easl-text/examples/editor_specimen.easl editor-proof
cargo run --locked --profile native-view -p easl-text --features gpu --example atlas_specimen -- target/easl-styled.png crates/services/easl/easl-text/examples/style_specimen.easl styled-proof
cargo run --locked --profile native-view -p easl-text --example editor_bench
cargo run --locked --profile native-view -p easl-text --example flow_bench
cargo run --locked --profile native-view -p easl-text --features gpu --example atlas_specimen -- target/easl-view-selection.png crates/services/easl/easl-text/examples/view_specimen.easl view-selection-proof
cargo run --locked --profile native-view -p easl-text --features gpu --example atlas_specimen -- target/easl-view-preedit.png crates/services/easl/easl-text/examples/view_specimen.easl view-preedit-proof
cargo run --locked --profile native-view -p easl-text --features gpu --example atlas_specimen -- target/easl-flow.png crates/services/easl/easl-text/examples/flow_specimen.easl flow-render-proof
cargo run --locked --profile native-view -p easl-text --features gpu --example atlas_specimen -- target/easl-flow-scroll.png crates/services/easl/easl-text/examples/flow_specimen.easl flow-scroll-proof
```

The paragraph specimen renders actual glyphs with EASL-selected lines and reports cold VM
compilation, native preparation, and width-only native-plus-EASL reflow timings.
It includes the native Rust optimizer as a comparison. The initial local results
and their limits are recorded in [MEASUREMENTS.md](MEASUREMENTS.md).
The editor specimen additionally checks an exact edit/undo/redo round trip before
rendering, and `editor_bench` measures the EASL editor core separately from shaping
and painting. Neither specimen exercises a window or OS input. A rendered
specimen and these timings do not establish application acceptance.
