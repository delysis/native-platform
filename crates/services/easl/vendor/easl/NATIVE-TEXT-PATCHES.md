# Local text-library primitives and embedding corrections

Studio, Cast, Hollow and the Studio host adapter have been removed. References
to earlier Studio checks below describe historical defect discovery, not current
dependencies or acceptance. Native regressions live in `easl-text/tests`.

The paragraph library in `../../easl-text` exposed these runtime defects. The
original import provenance remains in the repository migration record.

- `VmCpuRuntime::new_cpu_with_external` has a stable signature with or without
  Cargo's unified `window` feature, allowing a CPU library and a windowed app to
  consume EASL in one dependency graph.
- Successful CPU entry completion and explicit close publish dirty external
  variables, matching the existing completed-window-frame boundary. Runtime
  errors do not publish an incomplete entry's final state.
- VM local value bindings copy pre-existing storage rather than aliasing an
  initializer's variable or field. Fresh expression temporaries remain reusable.
  Without this correction, decrementing a local copy of a loop counter mutated
  the outer counter and could prevent the loop from terminating.

Executable regressions are in `easl-text/src/runtime.rs`; library import and
paragraph tests exercise the combined path. These corrections do not implement
general dynamic-array function arguments or module namespaces.

The font/input extension adds native primitives used by ordinary EASL programs:

- `load-font` returns a read-only `Texture2D` rasterized by Swash at the requested
  resolution. Bulk atlas metrics and single-script shaping preserve UTF-8 cluster
  ranges. EASL owns glyph placement, quad generation, composition and edit policy.
  The bounded outline-font implementation and its remaining limits are documented
  in [`easl-text`](../../easl-text/README.md).
- `open-font` opens the same immutable font instance without eager rasterization.
  `rasterize-font` accepts bounded glyph-ID requests and an optional resolution,
  returning independent pages that share native face/shaping data.
  `font-rasterized-glyphs` exposes sorted residency, including empty ink.
  The EASL consumer owns glyph selection, layout and eventual cache/eviction
  policy. Invalid IDs, resolutions and page overflow leave existing textures
  usable. Private face metadata validates IDs independently of public metric
  tables. Texture-global assignment works in both evaluators, snapshots GPU
  writes when needed, and retains read-only font metadata. CPU pixels and queued
  snapshots use shared immutable storage. A native GPU cache reuses live
  read-only resources by pixel allocation and dimensions; weak ownership permits
  retirement once EASL values and queued draws release them. Writable replacements
  detach even at equal dimensions, and reload prunes removed texture slots.
  Transfer counters and `easl-text/tests/font_residency.rs` verify CPU sharing,
  real GPU reuse/pixels, copy-on-write, retirement and independent render targets.
  GPU-write snapshots retain the existing RGBA8 readback representation.
- CPU-only texture globals use host storage without a shader binding. Shader
  output omits them and their helpers; direct/transitive GPU entry-point access
  is rejected alongside unbound runtime arrays. Assigning a texture to a bound
  global makes it available to shaders. Unbound render targets report runtime
  errors. This supports EASL-owned pending page publication without dummy slots.
- Ordered keyboard, committed-text and preedit snapshots retain repeats,
  modifiers and UTF-8 cursor offsets. The winit host applies the EASL-specified
  IME anchor; other hosts must forward input through `IOManager`.
- Samplers receive actual GPU sampler bindings, and render dispatch supports
  straight-alpha compositing in addition to the existing replace/add modes.
- Vertex/fragment interfaces accept matching location types and reject mismatches.
  CPU-only runtime-array helpers are excluded from WGSL; GPU entry points using
  those arrays receive a compilation error.
- Tree-walker CPU entries publish successful external state just as the VM does.
  CPU-only builds retain the supplied external-variable handle in both runtimes.

`font-decoration-metrics(Texture2D) -> vec4f` exposes separate underline
offset/thickness and strikeout offset/thickness in atlas pixels, with upward
positive offsets. The font loader uses the already locked `read-fonts` version
to read `post`/`OS/2` and `MVAR` values at the same normalized instance used for
shaping/rasterization. This avoids Swash's shared stroke-size field conflating
the two decorations. Both evaluators transport these facts; EASL owns logical
scaling, fallback thickness, placement and color. `easl-text/tests/rich.rs`
checks independent pinned table values and distinct controlled thicknesses.

`easl-text/tests/font_atlas.rs` and `tests/input.rs` exercise both interpreters,
including rejected resource replacement and platform modifier conflicts. The
headless `atlas_specimen` example exercises native GPU sampling and alpha blending
with EASL vertex/fragment shaders. These are component checks; real OS
IME/accessibility acceptance remains incomplete. Studio now forwards ordered
input snapshots and focus-scoped candidate rectangles through its native and
hosted preview paths. The Studio worker retains ordered batches under load,
coalesces only idle snapshots, and stops with a diagnostic on bounded-queue
overflow. `InputFrame::validate`, `event_count` and `text_byte_len` let a host
validate/budget these batches without copying their UTF-8 storage. Offscreen
Studio tests cover delivery, no replay, focus/source isolation and errors.
Direct dynamic-array-producing expressions passed to `array-length` still
panic in the VM; consumers currently use named global snapshots.

The editor core adds bounded, immutable `TextBuffer` values. Native primitives
provide UTF-8 operations, Unicode facts and explicit retain/release; EASL owns
selection, history stacks and composition. Three regressions exposed by actual
resource lifetimes are corrected:

- Resource operations carry an explicit `HostResource` effect, so the optimizer
  cannot erase allocation, release or validation merely because a result is unused.
- VM struct constructors use their already-evaluated argument slots. Previously
  they compiled the arguments again, duplicating mutations and allocations.
- Unit-like argument removal updates every specialized call signature but prunes
  a shared implementation's parameters/body once. Calling the same higher-order
  atlas helper from font preparation and editor rendering previously panicked.

The editor tests close multiple independent widgets and verify that no native
text values remain, including after history eviction, undo/redo and preedit.
`struct_constructor_evaluates_once.easl` and `repeated_higher_order_caller.easl`
retain the language regressions. No general dynamic-array argument support is claimed.

Native grapheme queries use `GraphemeCursor` over the complete immutable chunk
instead of walking every cluster from byte zero. Every byte offset is checked
against known cluster sequences, including CRLF, combining marks, emoji ZWJ,
regional indicators, Hangul and prepend characters. Word/line queries still
scan text, and unusual Unicode lookbehind can require more than adjacent bytes.

The geometry/view extension exposed another VM reference defect: a field access
discarded its mutable-reference context when compiling the containing array
element. Assignment changed a temporary without emitting the element write-back.
Field access now propagates that context. `array_struct_field_writeback.easl`
compares direct assignment, compound assignment, nested vector fields and helper
references for fixed and dynamic arrays in both CPU interpreters. This correction
does not claim general nested-array or mutable-swizzle-reference conformance.

Nested higher-order callers also exposed an order-dependent specialization bug.
Several call sites could retain different implementation copies for one generated
function name. Unit-like argument removal updated the registered body while a
stale caller retained the old argument list, producing an intermittent runtime
arity error. After specialization converges, named composite-function references
now point to their registered implementation before body-mutating passes run.
`nested_shared_specialization.easl` retains the shared nested-call shape; repeated
compilation/evaluation of the actual vertical-navigation fixture exercises the
previously intermittent failure.

The multiline extension adds `text-graphemes(TextBuffer) -> [TextGrapheme]` as a
bulk Unicode-facts operation in both CPU evaluators. Each record has exact UTF-8
start/end, first scalar and prohibited/allowed/hard break-after. ICU4X 2.2 auto
line segmentation supplies opportunities; extended graphemes use the existing
Unicode segmentation dependency. EOF is not a hard separator, and CRLF is one
grapheme. The logical-line boundary primitive now recognizes all Unicode hard
separators (including VT, FF and NEL), consistently with bulk facts. Native code
does not choose lines, trim spaces, place glyphs, move carets or scroll.

The flow consumer exposed two tree-walking defects. Numeric vector comparisons
were accepted by the compiler but evaluated as scalars; they now compare each
component like the VM. Also, `let` and `for` bindings were not removed on early
return, break/continue or evaluation failure. They now unwind lexical bindings,
including partially evaluated initializers, before propagating the result.
`vector_ordering.easl` and `lexical_unwind.easl` retain the regressions. The actual
font/reflow proof exercises the previously leaked composer temporary shadowing
its caller's selection offset.

The shared fsexp parser now reports unmatched top-level closing delimiters
without popping an empty parser stack. It preserves adjacent terminals and
following forms, including exact UTF-8 byte positions. A minimal failing parse
reproduced the panic before the correction; fsexp's parser suite covers recovery.


Studio's default anonymous vertex/fragment closures exposed an implicit-entry
marking defect. Dispatch validation updated the signature but skipped its own
locked registry entry while propagating the stage to implementations. Later I/O
attribute validation could therefore reject a shader as a non-entry function.
The compiler now reconciles marked signatures with their implementations after
releasing traversal read guards. Studio retains a repeated default-program
compilation regression; its explicit host smoke diagnostic exercises the real
GPU preview, source replacement and input session together.


The shared import loader now accepts a parsed in-memory root plus an intended
filename. File compilation and Studio use the same bounded graph traversal;
canonical dependency identities avoid repeated reads and preserve per-document
AST paths. Invalid/missing files, parse failures and graph limits retain source
context. The root buffer is authoritative and is never reread or written.
Default graphs admit 128 documents / 8 MiB source; Studio caps each dependency
read at 1 MiB. The callback API still uses local canonical paths, not a virtual
module registry. Native assets retain root-relative semantics.

The native `read_external_var_raw_bounded` API checks the immutable snapshot
size before copying and rejects private bindings. The retained regression is
`easl-text/tests/imports.rs`. Studio-specific source editing, compilation queues,
preview handoff and tests were removed with the application and host adapter.


## Ordered draws and cached runs

`GpuCore::execute_frame_events` replaces the separate screen/compute replay
paths. Native windows, CaptureIO and Studio execute the same event timeline.
Uploads submit earlier draws before changing their inputs; compute batches
track all used bindings when detecting later upload conflicts. A target clears
only on its first draw in a callback and loads prior pixels after readbacks or
returns from another target. Texture replacement checks both dimensions, even
when the old and new images have equal area. Headless/occluded screen shaders
execute against native textures, retaining their storage side effects.

`begin_frame` starts that target lifetime; `finish_frame` publishes the complete
offscreen image. Studio reuses two textures, acknowledging each UI blit before
admitting the next callback. This preserves the visible front image even when
a readback submits intermediate draws from a slow callback. Native surface
presentation uses the same ordered executor but has not been exercised in an OS
window for this change.

CPU VM `DynCopy` implements whole dynamic-global-array assignment with independent
value semantics, lazy zero storage and reusable word allocations. The existing
lowering owns GPU synchronization and destination dirty marks. Both evaluators
are checked for source/copy isolation, self-copy and empty/zeroed arrays. GPU
checks cover copying updated storage into a cached run. Direct dynamic-array
expressions, dynamic local arguments and audio support are unchanged.

`easl-text/tests/draw_order.rs` retains native GPU ordering coverage. Earlier host checks exercised actual offscreen pixels, mixed
compute/render/readback order, equal-area texture resizing, slow-frame retention
and EASL cached text runs. It also runs the retained offscreen, ping-pong and
bidirectional-render corpus through both CPU evaluators. These are component
checks; they do not establish native OS or complete Loom acceptance.

## Contextual editor startup analysis

The multilingual editor exposed repeated full effect collection during shader
input extraction. `builtin_attribute_lookups` now follows only builtin input
effects, visiting each reachable composite implementation once per query. The
result is local to that query, so compiler rewrites cannot leave a stale cache.
Generated input globals and entry arguments are checked against the existing
full effect analysis, including shared helpers, closures and explicit struct
fields. Invalid transitive input use still rejects the wrong shader stage.

Argument lowering no longer computes an argument's transitive effects when no
preceding argument names have been read. The original preservation logic still
applies when preceding reads exist. Both CPU evaluators check the resulting
read/mutation order. Regressions live in `easl-text/tests/compiler.rs` and
`tests/vm_conformance.rs`; Studio's offscreen multilingual editor exercises the
combined startup and rendering path. This is not general latency qualification.

The font-cascade consumer exposed the same repeated traversal in full effect
queries during shader compilation. Completed callee effects are now memoized by
their live implementation identity within one query. The cache is discarded
before any compiler rewrite. A shared-helper regression checks transitive reads
and mutations, then rewrites a leaf and checks that a fresh query observes the
new effects. This changes neither recursion handling nor the effect rules.

The cascade's flat integer font IDs also exposed two interpolation defects:
short or Unicode annotation values could panic during byte slicing, and WGSL
emission nested a complete interpolation attribute inside another. Parsing now
uses checked string prefixes; emission writes one attribute. Struct field parsing
also forwards errors to its caller instead of discarding them in a shadowed log.
Compiler regressions cover valid interpolation modes and malformed values. The offscreen consumer
exercises the flat integer varying through the actual GPU pipeline.


## Font instances and span style settings

`load-font(path, ppem, face-index, [FontVariation])` exposes explicit collection
faces and variable-font coordinates. HarfRust normalizes the instance once;
Swash metrics and rasterization use those same coordinates. `shape-font-span`
adds `[FontFeature]` and a language `TextBuffer` to its extended overload. The
short calls retain their default behavior. The tree evaluator and bytecode host
share validation and publish results only after success. Settings are bounded,
duplicate or malformed tags are rejected, and language handles are checked.
Font matching, style/run selection, placement and editing remain EASL/host policy.

`easl-text/tests/font_atlas.rs` checks independent HarfBuzz cases, actual variable
atlas pixels/advances, collection faces and rejected-setting resource retention
in both evaluators. Its EASL style specimen consumes these operations and caches
two actual font instances. This is not full rich-document or native OS acceptance.
