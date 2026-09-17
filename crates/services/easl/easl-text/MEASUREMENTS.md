# Component measurements

## Paragraph and font checkpoint (`000a919`)

Local Apple M4 Max, arm64, Rust 1.92.0, optimized `native-view` profile.
These historical measurements were refreshed for the font/input extension with the toolchain's
bin directory first in `PATH`, ensuring Cargo also resolves Rust 1.92 subcommands.
This is a local development-machine sample, not an isolated performance lab run.
`examples/specimen.rs` alternates widths of 328 and 440 pixels. It interleaves
the EASL composer and the native Rust comparison optimizer over the same prepared
text, discards each first sample, and records 100 samples per implementation.
Font preparation does not repeat during these reflows.

| UTF-8 bytes | EASL + native p50 | EASL + native p95 | Native comparison p50 | Native comparison p95 |
| --- | --- | --- | --- | --- |
| 356 | 0.248 ms | 0.726 ms | 0.032 ms | 0.077 ms |
| 2,848 | 1.698 ms | 2.329 ms | 0.224 ms | 0.320 ms |
| 11,392 | 5.724 ms | 6.799 ms | 0.849 ms | 0.934 ms |

Cold library compilation was 61.613 ms. Native preparation for these samples
was 0.238, 1.223 and 4.599 ms respectively. The EASL path includes native
opportunity extraction, bulk exchange, VM line selection, validation and final
native line layout. It is slower than the Rust comparison in this measurement;
the absolute cost supports further work without establishing full editor speed.

This is one local microbenchmark. It excludes input handling, editing, painting,
window compositing, accessibility and OS IME. It does not establish incremental
long-document performance, browser parity or an application latency budget.

At the paragraph checkpoint (`624ea92`), 91 tests passed across `easl-text`, `easl-native-text` and
`loom-easl-interface`; two pre-existing native review tests remained ignored.
Strict Clippy passed for the three packages. The library tests include an ordinary
EASL import, exhaustive small paragraph partitions, policy-dependent line choices,
mixed-script source coverage, large-offset measurement precision, rejected-layout
preservation, and CPU external-value publication. Ten retained EASL conformance
programs also produced matching tree-walker/VM output for references, closures,
loops and enum/struct values.

The offscreen specimen was rendered and visually inspected. It shows two actual
compositions with different last-line policies; it is not a Loom window or an
editable EASL widget. Current native application acceptance remains outstanding.

## Font atlas and editing/input extension

`examples/font_atlas.easl` now loads the retained Amiri outline font through
`load-font` at 48 pixels per em, requests bulk glyph metrics and Latin shaping,
and imports EASL glyph placement and quad generation. EASL vertex and fragment
shaders draw to a native Metal texture with linear sampling and alpha compositing.
The resulting PNG was visually inspected and contains legible text with ligatures
and accents. It was generated headlessly; no application window was exercised.

One local run took 63.337 ms to compile EASL and 387.747 ms for full-font atlas
preparation, text shaping/placement, GPU initialization, rendering and PNG export.
These are cold component timings, not frame or editing latency. The first atlas
loads every glyph; paging and demand-driven rasterization remain work.

The extension also exercises EASL selection/deletion policy against native buffer
transactions, exact undo, reversed selections, emoji/combining clusters, CRLF
word boundaries and rejected edits during preedit. Loom's experiment consumes the
same policy while retaining its Markdown document history. Input replay covers
ordered Unicode commits, repeat/modifier facts, overlapping shortcut/word modifiers,
AltGr, preedit offsets, frame limits, and focus loss through both the tree-walker
and CPU VM. Font loading and input
bindings are tested with and without the optional window feature. These tests do
not establish real OS IME, accessibility or complete native interface acceptance.

The extension checkpoint passes 110 tests across `easl-text`, `easl-native-text`,
`easl-native-host`, and `loom-easl-interface`, with two existing native review
tests ignored. Eight font/input tests also pass without the window feature.
Strict Clippy passes for the four selected packages; existing Cast and Studio
dependency warnings remain. Repository policy, documentation, ignored-test
registry, and CI metadata selection checks pass.

## EASL editor core (`c36ca89`)

`examples/editor_bench.rs` compiles the imported EASL editor once, seeds native
immutable UTF-8 values, then measures an EASL insertion and a logical left/right
movement pair. It measures 50 samples of each operation after discarding one
warmup. Input is ASCII with the caret at the end. History and its payload bounds
remain active; each case checks final source length and releases all text values.
Hardware, toolchain and profile match the preceding section. The final after run
enables the optional GPU feature, but this benchmark executes only CPU VM entries.

The first implementation scanned graphemes from byte zero on every boundary
query. Replacing that scan with `GraphemeCursor` over the immutable chunk changed
the measurements below. Native replacement still copies the edited UTF-8 value.

| Initial bytes | Insert before p50 | Insert after p50 | Insert after p95 | Left/right before p50 | Left/right after p50/p95 |
| --- | --- | --- | --- | --- | --- |
| 1,024 | 0.079 ms | 0.002 ms | 0.002 ms | 0.195 ms | 0.002 / 0.003 ms |
| 16,384 | 1.172 ms | 0.003 ms | 0.003 ms | 2.927 ms | 0.002 / 0.003 ms |
| 131,072 | 9.406 ms | 0.015 ms | 0.019 ms | 23.354 ms | 0.002 / 0.003 ms |
| 1,047,552 | 67.446 ms | 0.042 ms | 0.098 ms | 184.154 ms | 0.002 / 0.003 ms |

Cold editor-library compilation in the final after run was 203.258 ms. Retained payload
after the insertion sequence was 54,575, 853,295, 6,817,071 and 9,428,392 bytes
respectively, including current/input values and bounded history. The editor
does not retain every 1 MiB snapshot indefinitely.

These results cover the new general EASL editor core, not the existing Loom
widget. They exclude OS delivery, shaping, painting, compositing and application
services. ASCII end-of-buffer movement does not establish a worst-case Unicode
bound: regional-indicator/combining context can need more scanning, and word/line
navigation still scans text. No full application latency or Pretext parity claim
follows from this microbenchmark. Earlier paragraph/font timings above precede
the editor's additional compiler fixes and have not been remeasured here.

`examples/editor_specimen.easl` joins the components: EASL selects/replaces text,
undoes and redoes it, then exports the exact committed bytes to native Swash
shaping and draws through EASL glyph placement, quads and shaders. The runner
checks the exact source round trip and that closing the editor leaves no retained
text values. The headless Metal output was visually inspected; it reads
“Editing and undo, in EASL. Café, office, affinity.” It remains an offscreen
component check, not an interactive widget or a Loom acceptance result.

The final specimen compiled in 263.921 ms and took 275.961 ms for font preparation,
shaping, EASL editing/placement, GPU initialization, drawing and PNG export. This
is a cold integration timing, not a frame-latency measurement.

The consolidated gate passes 121 tests across `easl-text`, `easl-native-text`,
`easl-native-host` and `loom-easl-interface`, with the two existing native review
tests ignored. Twelve EASL conformance fixtures compare both interpreters within
that count. Strict Clippy passes for all targets of those four packages; existing
Cast/Studio dependency warnings remain. Repository policy, current documentation
and ignored-test registry checks also pass. No foreground application was driven.

## EASL geometry and view interaction

The next extension shares glyph placement with grapheme cell geometry. It exercises
ligature subdivision, combining marks, real Latin/Arabic shaping, caret affinity
at bidi/wrap boundaries, disjoint selection rectangles, preferred-column vertical
movement, empty lines, zero-advance graphemes, pointer dragging and stale-layout
rejection. Invalid geometry and output-capacity failures emit no replacement cells.

The single-line view specimen selects `ffi` in `Office` through EASL pointer hit
testing and paints the selection and caret. A second entry displays an underlined
`é` preedit replacing that selection, while asserting that committed text remains
exactly `Office, café, affinity.` Both headless Metal images were visually inspected,
and each runner verifies expected source/selection and zero retained text values
after editor close. The optional standalone window entry is compiled by these
checks but has not been exercised interactively.

Cold compile times were 1,219.444 and 1,263.872 ms for the selection and preedit
entries. Font preparation, edits, layout, GPU initialization, drawing and PNG export
took 345.684 and 306.832 ms respectively. These cold specimen timings do not measure
keystroke latency. Cell queries currently scan their supplied layout; indexing,
viewport narrowing, incremental reflow and integrated interaction benchmarks remain.
Earlier benchmark tables above describe their named checkpoints and were not
remeasured after the additional compiler fixes.

The combined gate passes 129 tests across the same four packages, with the two
existing native review tests ignored. Fourteen retained language fixtures compare
both interpreters within that count. An intermittent nested-specialization arity
failure was reproduced and fixed; the previously failing navigation case then
passed 40 independent compiler/evaluator executions. Strict Clippy, repository
policy, documentation and ignored-test registry checks pass; pre-existing
Cast/Studio dependency warnings remain.

This is component evidence. Font GDEF caret positions, paragraph bidi resolution,
full multiline/rich widget integration, Studio input forwarding, Loom's painter
replacement, real OS IME/accessibility and current-interface acceptance remain open.

## EASL multiline flow and viewport extension

`flow.easl` connects bulk Unicode facts, Swash glyph records, paragraph-wide
line choice, atlas quads and caret cells. `viewport.easl` builds a validated line
index for binary-search visible ranges, indexed pointer hits and caret reveal.
`flow_specimen.easl` exercises the path in both CPU evaluators with a retained
Amiri font. It keeps source and cross-line pointer selection through two width
changes, then reveals the final caret in a smaller viewport without reshaping.
The final offscreen proofs are `easl-flow-selection-final.png` (900×650) and
`easl-flow-scroll-final.png` (900×180), under `target/easl-integration`.
Both were visually inspected. The scroll render draws only visible glyph ranges.
No native window, OS wheel/IME event, accessibility action or Loom user document
was exercised by these proofs.

The `flow_bench` example measures 100 width-only reflows after five warmups,
alternating 410 and 820 pixels. Its source has 230 UTF-8 bytes, 228 Unicode
scalars and four hard newlines. The measured operation includes EASL composition,
quad/cell placement, buffer allocation and line indexing; it excludes native
input delivery, painting, GPU upload and application services. Source measurement
and shaping are cached: 105 width changes retain exactly one shaping pass, and
closing the consumer leaves zero native text values.

| Measurement | This local run |
| --- | ---: |
| Cold EASL compilation | 2625.245 ms |
| Font/source preparation plus initial flow | 45.303 ms |
| Width-only reflow p50 | 0.620 ms |
| Width-only reflow p95 | 0.630 ms |
| Width-only reflow p99 | 0.648 ms |

These are small-specimen component measurements, not long-document or native
keystroke latency claims. `easl-flow-indexed-bench.log` preserves the run.
The combined four-package gate passes 139 tests, with the same two pre-existing
ignored native/large-file tests. Strict Clippy, repository policy and documentation
checks pass. A redundant String conversion in the test helper was removed after
the combined run; Clippy then compiled the final test source. The 16 retained
language conformance fixtures include vector comparisons and lexical unwinding.

The real flow exposed two tree-walking runtime defects: numeric vector ordering
had scalar-only evaluation, and early control flow leaked lexical bindings into
callers. Both have retained regressions. Integrating line indexing also exposed
fractional-height rounding overlaps; placement now computes bands from shared
edges and rejects unrepresentable heights before publication.

This bounded flow is Latin/LTR. Mixed-script itemization, paragraph bidi,
contextual reshaping at selected line edges, tabs, discretionary hyphens,
emergency wrapping, font caret data, rich styles and incremental long-document
layout remain incomplete. Studio input forwarding, replacing Loom's painter and
actual native interaction remain separate required work. A failed flow returns
status; it does not silently normalize source or claim a fitting layout.
