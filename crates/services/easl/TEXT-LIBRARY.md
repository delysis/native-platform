# Text layout and editing as an EASL library

The intended deliverable includes a reusable library written in EASL. An EASL
program should be able to compose and edit text, choose typography and extend
layout behavior without writing a product-specific Rust adapter. Loom is a
consumer of that library. Keeping application services in native-kit Rust crates
does not require putting all view algorithms in Rust.

Studio and its host adapter are excluded. Native EASL text/rendering work uses
the compiler/runtime directly. Earlier Studio offscreen results are historical;
current runtime tests and standalone EASL specimens do not depend on that editor.

## Current implementation

`easl-native-text` supplies preparation, native layout, editing and drawing over
Parley and Vello CPU. [`easl-text`](easl-text/README.md) now implements paragraph
line choice and its typography policies in an importable EASL module. A standalone
EASL consumer and a native glyph specimen exercise that module. The native adapter
exchanges shaped measurements and validates chosen boundaries; it does not choose
the library's line endings.

Loom's `loom.easl` supplies geometry, style records and controls through its
experimental host protocol. Its native editor now consumes the shared EASL
selection/replacement, pointer-hit, line/page navigation and rich-style policy while
`loom-markdown` owns source transactions and history. The reusable atlas module places shaped glyphs and implements text
quads in EASL, backed by native `load-font`/Swash primitives. The window runtime
now exposes ordered UTF-8, repeat, modifier and IME facts to EASL input policy.
The reusable `editor.easl` core now owns independent editor state, selection,
bounded undo/redo and composition over immutable native UTF-8 values. Input replay
and a headless editor/atlas specimen exercise that path without a product text
adapter. Grapheme cells now share the atlas placement pass, and EASL implements
caret/hit geometry, selection rectangles, pointer dragging and physical/vertical
keyboard movement. A standalone single-line consumer renders selected text and
composition with these modules. The flow module now connects Unicode break facts,
paragraph line choice, atlas placement and cells for bounded Latin/LTR multiline
text. Its consumer retains source and selection during width-only reflow without
reshaping. An EASL line index supplies visible-range lookup, indexed pointer hits
and scroll-reveal geometry. A bounded contextual consumer now adds mixed-script
flow with exact line-edge shaping, described below. Rich text, native widget
acceptance and the remaining Loom integration are still required.

Loom's pointer bridge streams actual native shaped caret stops on one indexed
line through the reusable EASL geometry policy, in bounded batches. It retains
wrap/bidi affinity, whole-grapheme edges, rich styles and paragraph insets without
copying source into the VM. Shared-edge ties retain the clicked cluster before
affinity, and empty-line font struts do not create phantom carets. Native granular
selection preserves whole CRLF ranges and word/line drag anchors. CPU integration
checks cover Markdown source/undo and independent pane selections; this is not
live native pointer acceptance. The shared EASL navigation module now chooses
vertical/page and logical line/document destinations from live native line facts.
Preferred columns stay with each view, selections retain their anchors, and CRLF
edges remain complete. The standalone EASL widget shares page overlap, forced
progress through paragraph gaps and document-edge policy. Rasterization, horizontal
and word navigation, and granular word/line selection rules remain native.

The shared EASL `styles.easl` module now resolves the real Loom editor's rich
styles. The Markdown adapter supplies base roles and inline selectors; EASL
applies ordered field masks and coalesces equal adjacent spans, preserving
language, variation/features, spacing, colors and decorations. Tables, ranges,
work and output capacity validate before publication. The safe Rust `Styling`
transport checks exact UTF-8 ranges and interns strings; it owns no document
transactions. Each pane caches resolved spans by source revision/mode and
appearance, so caret movement and resize reuse the result. Integration checks
cover current roles, source mode, independent panes, undo, preedit and recovery
from invalid styles. Parley still supplies Loom's shaping/line/caret geometry;
Vello supplies rasterization. The general EASL atlas path needs rich-widget
integration and native acceptance.

The reusable `rich.easl` module now applies size and spacing to shaped glyphs
before measurement/caching, combines mixed-size runs around a common baseline,
and emits underline/strikeout rectangles from native font-table metrics. The
existing line consumer accepts resolved style spans, flat feature/language
tables and explicit font instances. Tracking disables optional ligatures unless
the author overrides them. Scaled ink, measurement and editable cells share
logical coordinates; unchanged native glyphs skip the transform. CPU checks
exercise mixed styles, exact source boundaries, empty insertion styles, work
exhaustion and resource cleanup. The styled offscreen consumer uses these same
functions. The contextual document consumer now projects original-source style
spans into paragraphs, retains colors/decorations through wrapping and uses
variable line bands for glyphs, caret/selection cells, scrolling and composition.
Its single-paragraph cache compares actual styles and selected font IDs; late
failure retains the previous complete GPU/caret arrays. The standalone widget
consumes this path, while Loom still retains native line/caret layout. Rich
fallback, multi-paragraph cache retention, negative final cluster advances,
fragmented-grapheme style edges and general font matching remain explicit gaps.

The actual Loom renderer now also uses `GlyphPainting` and the shared EASL
`painting.easl` policy for final glyph positioning and inline decoration
geometry, including labels and list markers. Rust transports bounded batches of
already shaped glyphs and font facts; EASL resolves decoration overrides and
accumulates glyph positions. A bounded run cache avoids VM work on unchanged
repaints and host-origin/display-scale changes. The native surface stages all
visible outputs before drawing, retains font resources, and applies clipping
and display scale once. CPU pixel comparisons cover mixed scripts/sizes,
clipping and three display scales; both EASL evaluators exercise failure
atomicity and explicit decoration values. Current Loom render/pane checks
exercise the actual path. This removes final glyph/decorative placement policy
from Loom's Rust paint loop while retaining native shaping, line/caret layout
and Vello rasterization. Full atlas-widget integration remains open.

Font opening and bitmap residency are now separate primitives. `open-font` retains
immutable native font/shaping facts without rasterizing a complete font;
`rasterize-font` builds a page for glyph IDs and resolution selected by EASL.
Pages share the exact collection/variation instance and retain independent UVs
and resident-ID lists. Invalid or oversized requests preserve earlier pages.
The existing EASL atlas specimen now shapes first and requests only its glyphs.
Both evaluators and the real offscreen GPU path exercise page selection and
resource retention across draws. A 256-ppem Amiri heading that exceeded the full
atlas limit fits a seven-glyph page. This is a bounded component improvement;
multi-page caching, actual Loom widget integration, native acceptance and overall
editor performance remain unfinished. Texture values and queued snapshots now
share immutable bitmap storage. The native renderer retains one GPU image for a
live read-only page across binding changes, while EASL owns page selection and
lifetime. Equal-size writable replacements detach before drawing; weak cache
ownership permits discarded pages and their binding references to retire.
Both evaluators and real GPU checks cover reuse, independent pixels and cleanup.
Eight frames of a fixed heading now require one texture upload instead of eight;
this transfer probe does not qualify overall application latency.

The reusable EASL page planner now retains covering pages and deduplicates
replacement requests with bounded validation/work before publication. Placement
can retain page-independent glyph origins and source ranges, then resolve UVs
against the chosen immutable page. Single-line, plain multiline and contextual
rich consumers use this path; final glyphs from all paragraphs are collected
before any page is published. Their explicit fallback and variable font faces
open without full rasterization. CPU-only texture globals hold pending pages
without shader slots; shader access requires an explicit binding. General font
pool matching, multi-page packing/eviction and actual Loom atlas adoption remain
open. This also repairs newly typed glyphs missing from the single-line editor's
initial specimen page.

The compiler supports bounded library imports with source-qualified diagnostics.
Native and captured GPU execution share ordered uploads/draws/readbacks. CPU
arrays have independent value semantics; both evaluators cover the full
5,000-cluster synthetic flow and cell checks. Native GPU ordering and bounded
import/export regressions are retained in `easl-text/tests`.

The EASL widget controller now connects indexed pointer hits, page movement,
scroll reveal and per-line composition decoration to the shared editor. The
multiline consumer runs these policies, wrapping and atlas painting as a standalone EASL program. Earlier offscreen checks covered selection, resize without reshaping,
composition, exact Enter commits and undo recovery after unsupported layout.
Read-only editor queries now accept borrowed state. EASL emergency wrapping now
keeps long unbroken input editable, including at a measure narrower than a glyph.
It preserves complete shaped clusters, original bytes and fitting paragraphs'
composition, and derives scroll extent from actual placed width. Both CPU
interpreters exercise capacity/cluster boundaries; earlier offscreen checks exercised
long-word paste, narrow resize without shaping and undo. The consumer remains
bounded Latin/LTR. Literal tabs now use configurable line-relative stops in EASL,
with shared compensated widths for paragraph choice, placement and caret cells.
The table visits are bounded by the composer's work budget, ordinary text keeps
constant-time candidate measurement, and tabs retain their source bytes and
trailing width without control-glyph ink. Both evaluators check fractional stops,
capacity/work failures and exhaustive small paragraph partitions. Earlier offscreen checks covered tab paste, aligned caret columns, key insertion, resize without shaping and
undo. General rich text, wheel input and complete native acceptance remain open.

Native paragraph analysis now exposes exact-byte scalar facts, resolved bidi
levels and raw script tags. The EASL `bidi.easl` module owns per-line whitespace
resets, visual ordering and coalescing into logical-range level runs. Both
interpreters compare 540 selected lines against an independent native L1/L2 path;
real Amiri shaping feeds the existing EASL placement/caret geometry for an
English/Arabic boundary. Capacity and work failures precede output. The tables
currently cover Unicode 16 bidi and ICU4X 2.2's Unicode 17 script properties.
The EASL `script.easl` module now resolves Script_Extensions, weak characters and
paired-bracket context in logical paragraph scopes, preserving complete graphemes.
It bounds validation, nesting, work and output, with no partial publication on
failure. Both evaluators cover script policy and malformed/exhausted input; native
checks visit every Unicode scalar. Real Latin/Cyrillic Shantell Sans shaping
feeds EASL atlas/caret geometry across three script runs in one LTR direction.
These fixtures also repaired vector bitwise emission in the VM and UTF-8
delimiter scanning in the compiler lexer. Host font/style discovery and broader contextual multiline coverage remain required;
these components alone do not establish a general text renderer.

The font primitive now uses HarfRust 0.12.0 for shaping and retains Swash atlas
rasterization. Its bounded API reports glyph/work exhaustion without publishing
partial runs, including when a reused buffer already has storage. The EASL
`shape-font-span` builtin borrows immutable source, preserves adjoining shaping
context and returns span-relative whole-grapheme clusters and boundary safety
flags. EASL still chooses scripts, line boundaries, visual placement and editing
policy. The multiline consumer uses this bulk API without first expanding source
bytes into an EASL array. Both evaluators exercise contextual Arabic spans, exact
ranges, invalid/released inputs and retention of prior runs after failure; real
Kawi and Arabic glyph cases have a finite independent HarfBuzz oracle. This does not itself choose fonts or replace Loom's existing native painter.
The extended font overloads now accept explicit collection faces, variations,
per-span features and language. One normalized variable-font instance feeds both
shaping and atlas rasterization. EASL fallback coalescing preserves caller-supplied
per-grapheme style IDs. A cached EASL specimen uses actual regular and bold italic
instances. Loom now resolves rich style values in EASL as described above; its
font selection and painter still use the retained native stack. Contextual
final-line composition now uses these primitives below.

Offscreen startup also exposed repeated transitive effect analysis in the
compiler's closure-extraction pass, before the first draw. That pass now computes
capture effects only for closures while still walking other expressions. This
removes redundant call-graph traversal without changing closure capture policy.
The contextual editor exposed two further startup costs. Shader-input extraction
now queries only builtin input attributes and visits each reachable implementation
once per query. Argument lowering skips transitive effect analysis when there
are no preceding reads to preserve. Shader-input tests compare the generated
globals and entry arguments against full effect analysis; both CPU evaluators
check mutation order. Compiler startup and interactive latency still need broader
qualification under representative load.

The EASL `runs.easl` module now intersects paragraph script and caller-selected
font/style partitions with a selected line's bidi runs. Output is visually
ordered while source ranges stay logical; explicit context ranges preserve
Arabic joining across style edges and stop it at line/level boundaries. Dry-run
validation bounds work and output before publication. A level edge inside a
single extended grapheme reports an explicit gap instead of altering Unicode
levels or publishing incorrect caret cells. Both evaluators exercise real
Latin/Cyrillic/Arabic fonts, contextual style edges and 4,096 style partitions.
The existing single-line EASL editor now consumes this planner, stages its
replacement geometry and retains prior pixels plus source/history on layout
failure. Earlier offscreen commit/preedit/undo tests verified mixed-direction editing and
recovery, including disabling stale candidate geometry and clearing failed state
when undo returns to cached text. It uses one explicit Amiri font and a 4,096
scalar/run/glyph/cell bound. The multiline consumer below enables EASL fallback.
Fragmented-grapheme geometry and Loom's painter replacement remain open.

The EASL `measure.easl` module now validates shaped runs and builds compensated
logical prefix widths with per-grapheme shaping safety. The contextual composer
examines nonmonotone candidate widths and minimizes indivisible overflow before
typography cost. A real Amiri case proves why both are necessary: two joined
Arabic letters fit where an isolated suffix does not. Exhaustive small partitions
independently check line choices. The multiline EASL editor routes mixed-script
and bidi documents through EASL run planning, safe-cluster caching, exact line-edge
shaping and the existing atlas/cell/widget modules. CPU tests exercise CRLF,
empty paragraphs, source cells, wrap affinity and resize reuse; earlier offscreen input
exercised mixed-script paste, resize, Arabic preedit/commit and undo recovery.
Geometry publishes only after the whole document validates. The consumer is
bounded to 4,095 document graphemes. The reusable EASL `fallback.easl` module now
chooses whole-grapheme fonts from contextual shaping results, preferring fewer
missing glyphs and retaining earlier candidates on ties. Validation and bounded
work precede writes; unresolved graphemes remain source-preserving editable text.
The contextual consumer uses explicit Amiri/Shantell Sans/Noto Sans Kawi atlases,
retains font choices across reflow and paints all atlases in one draw using flat
per-quad font IDs, preserving visual order. Both evaluators cover real fallback,
exact cells and missing-glyph reporting. Earlier offscreen integration exercised the
three-font painter, Latin-only fallback, reflow without new font probes,
unresolved characters and exact undo/source preservation.
Contextual tabs, multi-paragraph cache retention, system font discovery, color
emoji, rich styles, fragmented graphemes and Loom's painter replacement remain
open. No foreground GUI acceptance is implied.

The vendored EASL implementation has functions, structs, arrays, a CPU VM and
external data exchange. Those are a starting point for text algorithms; they are
not evidence of a finished text API, library loading or adequate performance.
The earlier application's slowness does not establish that an EASL implementation
of layout or editing policy is inherently too slow.

## Implementation direction

- Native primitives provide font access, Unicode and shaping information, glyph
  metrics and drawing, text-buffer operations, and OS IME/accessibility adapters.
  Their interfaces must expose sufficient data for EASL algorithms to make real
  choices, including exact source ranges and cluster/caret geometry.
- EASL library code owns reusable composition and editing policies: rich-text
  styles, paragraph and region composition, line-choice policy, editor commands,
  selection interactions and reusable editable widgets. Start with a bounded
  vertical slice and expand against the existing native behavior and fixtures.
- Use typed text/font/layout resources and bulk operations. Measure transfers,
  copies and VM execution separately. Avoid per-glyph host calls and repaint-time
  reshaping of unchanged text. Native acceleration is an implementation choice
  supported by measurements, not a requirement that all algorithms live in Rust.
- Preserve exact UTF-8 and distinguish source offsets from display clusters.
  Validated native buffer storage is compatible with EASL owning the commands
  and policies operating on it. The experiment's current statement that manuscript
  strings never enter its VM describes that implementation; it is not a permanent
  prohibition on a general EASL text API.
- Keep project storage, immutable provenance, configuration, model authority and
  generation lifecycle in the existing shared Rust application crates. A text
  widget emits edits and actions; it does not acquire Loom's persistence authority.
- As EASL implementations replace host policies, move production consumers to
  the shared library. Retain native implementations as deliberate primitives or
  comparison oracles where useful, rather than maintaining two independent
  production policies by accident.

## Acceptance

1. A standalone EASL specimen imports the library and displays rich editable
   text without depending on Loom or a custom Rust text adapter.
2. The specimen changes composition and editor behavior through EASL library
   code; equivalent logic is not hidden in host branches for that specimen.
3. Loom consumes the same library while retaining its current interface contract
   and shared Rust application/configuration authority.
4. Executed tests cover source fidelity, mixed-script layout, selection, edits,
   undo and composition. Real native interaction and performance measurements
   remain separate acceptance gates from unit tests and screenshots.
5. Benchmarks compare cold preparation, width-only reflow, selection movement
   and edits on unchanged and long documents, including the language/host
   boundary. Optimizations must preserve the same text and layout contracts.

This document records the full target. Paragraph composition, atlas placement
and editor state/history/composition are implemented components; they do not
establish the complete library or Loom acceptance described above.
