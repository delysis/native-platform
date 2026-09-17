# Native text for EASL hosts

Safe Rust text preparation, layout, editing and transparent RGBA rendering. This
crate has **no EASL runtime, browser, window, project storage or Loom dependency**.
EASL hosts supply text/style values and paint the resulting native layouts.

This is the current native backend. The intended EASL ecosystem deliverable also
includes composition and editing policies written in EASL, usable without Loom;
see [Text layout and editing as an EASL library](../TEXT-LIBRARY.md). That library
is not implemented by exposing this Rust crate alone.

Parley 0.11.1 (local corrections), Fontique, HarfRust, Skrifa and ICU supply font
fallback, shaping, bidi and Unicode segmentation. Vello CPU 0.2.0 paints the actual
shaped glyphs. Layout, selection, hit testing and painting use the same geometry.
Use the workspace `native-view` or release profile for interactive rendering;
unoptimized pixel and shaping loops are unsuitable for performance evaluation.

## Implemented contracts

- Prepared text keeps font references and shaped glyphs across width changes.
- Preserved/collapsed whitespace with original UTF-8 source mapping; grapheme-safe
  emergency breaks, NBSP, eight-space tabs, conditional soft hyphens and keep-all.
- Mixed styles, inline box geometry, font fallback, bidi, ligatures, kerning,
  OpenType feature/variation settings, spacing, justification and decorations.
- Streaming line boxes for exclusions and columns; source ranges, materialization,
  bounds and column-aware hit testing. Failed flows retain the previous layout.
- Heading balancing and bounded paragraph-wide minimum-raggedness optimization.
  Optimization rejects tabs, inline boxes and unsupported/over-budget input;
  callers can explicitly fall back to greedy reflow. This is not full TeX.
- Native plain-text editing, grapheme-aware movement/deletion, selection, undo/redo,
  composition and AccessKit text semantics. Composing text cannot silently replace
  the committed save value. Rendering clips to the viewport.
  Page Up/Down use the current viewport height with one line of overlap; Shift
  extends the selection while repeated paging retains the desired column.
  Selection-only commands reuse a bounded grapheme index and avoid text copies,
  full-buffer validation and history snapshots. Text changes, external projections,
  IME updates and history restoration rebuild that index.
- Rich editor projections install character styles and paragraph boxes together.
  Insets, first-line indentation, spacing and empty-paragraph font metrics feed
  the same geometry used for glyphs, carets, selection, hit testing and IME.
  External document models can call `validate_text` before publishing an edit.
- Editor width, alignment and paragraph-box changes retain shaped runs and font
  references. Reflow refreshes line geometry and selection, resetting tabs,
  discretionary hyphens and prior justification. Text, styles, scale,
  quantization and IME updates still require shaping. Pending shaping invalidation
  cannot be downgraded by a later geometry change. `shaping_generation` exposes
  whether that work ran, separately from the generation used for repainting.

Input bounds: 1 MiB text; prepared layout allows 4,096 styled spans, 512 inline
boxes and 131,072 lines. The editor accepts up to 32,766 style spans (leaving room
for range endpoints and IME within Parley's u16 style identifiers) and 131,072
paragraph descriptors. Its total shaping work still scales with the full buffer;
paragraph caching and viewport virtualization are outstanding. Discretionary
soft-hyphen shaping context work is capped at 8 MiB. Paragraph optimization allows
4,096 candidates and 1 million edges. Undo retains at most 128 edits/4 MiB. Raster
surfaces allow 16 million physical pixels and at most 4,096 draw commands per frame.

## Verification and remaining parity work

The MIT retained fixture from [Pretext](https://github.com/chenglou/pretext) is
pinned at `cc8619ad5d856190925951545965911458abe29d`. The full native replay covers
34,159 prepared inputs and 177,065 width layouts, checking source coverage, UTF-8
and grapheme boundaries, finite geometry and preparation reuse. The default test
samples all retained groups: 1,731 preparations and 13,604 layouts.

This is **not browser geometry equivalence or complete Pretext feature parity**.
The fixture's seven unpaired UTF-16 units become U+FFFD at the Rust UTF-8 boundary;
provenance and licenses are in `tests/pretext` and the migration record. The replay
does not reproduce every browser option, rich-item identity/noBreak behavior, or
browser font measurement. Native bidi and glyph rendering exceed Pretext's scope,
but do not prove equivalent line-break choices across engines.

IME exclusion rectangles remain in content coordinates for unwrapped lines,
including text beyond the viewport width. Hosts apply their scroll offset and
clip afterward. The regression checks the caret before and during preedit; it
is component geometry evidence, not an OS input-method qualification.

Still needed before broad renderer acceptance: browser differential measurements,
rich-item parity, cross-platform font/raster evidence, long-document incremental
editing, real OS IME coverage and complete mixed native/webview accessibility.
Dictionary hyphenation, full Knuth–Plass glue/fitness, optical margins and TeX-level
microtypography are not implemented. Complex-script discretionary hyphen shaping
needs more comparison evidence. The current surface is CPU rendered; it can be
uploaded/composited by a host but has no direct GPU backend here.

```sh
rustup run 1.92.0 cargo test -p easl-native-text
EASL_FULL_CORPUS=1 rustup run 1.92.0 cargo test -p easl-native-text --test pretext_corpus -- --nocapture
rustup run 1.92.0 cargo run --profile native-view -p easl-native-text --example specimen -- target/easl-type-specimen.png
rustup run 1.92.0 cargo run --profile native-view -p easl-native-text --example editor_latency
```

`editor_latency` measures synthetic offscreen initial layout, 1,000 movements and
100 alternating-width reflows on unchanged text separately. It reports whether
reflow retained shaping. It excludes window events, repainting and Markdown
transactions; it must not be reported as end-to-end application latency.
