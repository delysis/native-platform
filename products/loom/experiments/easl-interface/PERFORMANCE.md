# Native text latency investigation — 2026-09-14

The reported text slowness reproduced in the unoptimized debug bundle. EASL was
not rendering glyphs: it spent about 0.7 ms emitting a view. Unoptimized Vello CPU
rasterization dominated the frame, with additional unoptimized shaping and pixel
conversion costs. The host also ran the VM twice and repainted the entire surface
for every mouse move, including unchanged hover.

Offscreen local macOS arm64 measurements: 1280 × 820 logical window, 2× scale
(2560 × 1640 physical pixels), 80 repetitions of the profiler's mixed-script prose.
Times below are representative warm frames from the same synthetic fixture.

| Stage | Unoptimized test profile | Optimized `native-view` profile |
| --- | ---: | ---: |
| EASL VM + protocol decode | 0.67 ms | 0.17 ms |
| Native encoding/layout | 20.3 ms | 0.66 ms |
| Native CPU rasterization | 768 ms | 13.4 ms |
| RGBA to window buffer conversion | 52.4 ms | 0.9 ms |
| Complete offscreen frame | 841 ms | 15.1 ms |
| Single-character insertion into fixture | 106.5 ms | 5.1 ms |

The optimized frame is approximately 56× faster. At 1× scale the populated warm
frame is about 4.4 ms. Initial glyph-cache population remains slower: the populated
2× first frame in this sequence took about 58 ms. These are a short diagnostic
sample, not a statistically stable benchmark or end-to-end input latency claim.

A later run with the interactive review window open measured 32–36 ms populated
Retina warm frames and 14.4 ms insertion. The variability is recorded rather than
treating the first run's 15 ms as a guaranteed budget. Both runs remove the
hundreds-of-milliseconds unoptimized raster bottleneck.

Repair: use the optimized native-view profile (with assertions and line tables),
coalesce pointer events, compare evaluated scenes before repainting hover, and
only execute a second view pass after a host action changes inputs. Prepared label
and glyph caches remain active. A regression checks that moving inside an unchanged
hover region preserves the same scene while entering a control changes it.

Performance bounds still matter. The editor reshapes a whole document on edits;
1 MiB being accepted does not promise low latency at 1 MiB. Paragraph-level
invalidation and incremental text storage are outstanding. The compositor still
paints the full CPU surface when something actually changes. GPU composition or
damage caching can improve that further without changing the EASL interface or
moving project/application logic into the renderer.

## Editor resize reflow — 2026-09-16

The actual Loom text-field regression reproduced a second avoidable cost: changing
pane width rebuilt font matching and shaping for unchanged text. The editor now
distinguishes shaping invalidation from line geometry invalidation. Width,
alignment and paragraph-box changes reuse shaped runs; edits, styles, IME, scale
and quantization changes still rebuild them. EASL policy and application/config
ownership are unchanged. This does not replace native line breaking with EASL.

Local macOS arm64 `native-view` component probe, using `editor_latency`: 100
alternations between 240 and 720 logical pixels, after initial layout and 1,000
caret movements. The source repeats a short Latin/combining-mark paragraph.
The before measurement used `f71702c0` plus generation instrumentation and the
same probe; the after measurement includes the editor invalidation change.

| UTF-8 bytes | Before median / p95 | After median / p95 |
| ---: | ---: | ---: |
| 3,500 | 1.186 / 1.265 ms | 0.040 / 0.041 ms |
| 35,000 | 12.194 / 12.468 ms | 0.399 / 0.421 ms |
| 175,000 | 62.672 / 63.627 ms | 1.993 / 2.122 ms |

Each after sequence retained its shaping generation; each before sequence changed
it. This is one local component sample, excluding rasterization, window events,
Markdown transactions and EASL evaluation. It does not establish end-to-end
latency or incremental editing performance. Reflow still visits the whole layout.

Regression comparisons exercise repeated widths and alignment against freshly
shaped layouts, including rich styles, paragraph insets/gaps, zero-width boxes,
tabs, soft hyphens, bidi, combining marks, emoji and empty final paragraphs. They
compare line ranges/metrics, glyphs, both caret affinities and pointer hits. A
composition test retains source/selection/preedit through resize, then commits
and undoes exactly. The Loom adapter test verifies that resize and caret movement
retain both its EASL style cache and its native shaped runs.
