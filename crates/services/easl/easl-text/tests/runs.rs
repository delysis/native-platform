use easl::{
    CompilerTarget,
    compiler::{builtins::built_in_macros, program::Program},
    interpreter::{CpuRuntime, IOEvent, StringIO, run_program_entry_with_io_and_runtime_from_path},
    parse::{EaslMultiDocument, parse_easl_without_comments},
};
use std::path::Path;

fn check(body: &str) {
    check_extra(body, "", &["bidi", "script", "runs"]);
}

fn check_extra(body: &str, extra: &str, modules: &[&str]) {
    let source = format!(
        r#"
      (var source: TextBuffer)
      (var b-facts: [TextDirectionalFact])
      (var b-slots: [TextBidiSlot])
      (var b-runs: [TextBidiRun])
      (var r-scripts: [TextScriptRun]) (var r-fonts: [TextFontRun])
      (var r-output: [TextShapeRun]) (var r-writes: u32)
      (defn r-same [a: TextShapeRun b: TextShapeRun]: bool
        (and (all (== (vec4u a.start a.end a.script-code a.font) (vec4u b.start b.end b.script-code b.font)))
             (all (== (vec4u a.style a.level a.context-start a.context-end) (vec4u b.style b.level b.context-start b.context-end)))))
      (defn r-script-at [i: u32]: TextScriptRun (r-scripts i))
      (defn r-font-at [i: u32]: TextFontRun (r-fonts i))
      (defn r-emit [i: u32 item: TextShapeRun] (+= r-writes 1u) (= (r-output i) item))
      (defn r-plan [levels: u32 scripts: u32 fonts: u32 capacity: u32 budget: u32]: TextRunResult
        (text-plan-runs source levels b-run-at scripts r-script-at fonts r-font-at capacity r-emit budget))
      (defn b-run-at [i: u32]: TextBidiRun (b-runs i))
      (var assertion: u32)
      (defn b-fact-at [i: u32]: TextDirectionalFact (b-facts i))
      (defn b-slot-at [i: u32]: TextBidiSlot (b-slots i))
      (defn b-emit-slot [i: u32 slot: TextBidiSlot] (= (b-slots i) slot))
      (defn b-emit-run [i: u32 run: TextBidiRun] (= (b-runs i) run))
      (defn expect [ok: bool] (+= assertion 1u) (when (not ok) (print assertion)))
      (defn prepare [direction: u32]
        (= b-facts (text-directional-facts source direction))
        (= b-slots (zeroed-array (array-length b-facts)))
        (= b-runs (zeroed-array (array-length b-facts))))
      {extra}
      @cpu (defn main [] {body} (text-release source) (print "done"))
    "#
    );
    let parsed = parse_easl_without_comments(&source);
    assert!(
        parsed.parsing_failures.is_empty(),
        "{:?}",
        parsed.parsing_failures
    );
    let mut documents =
        EaslMultiDocument::from_singular_document(parsed, "runs-test.easl".into(), source);
    for module in modules {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("library/{module}.easl"));
        let library = std::fs::read_to_string(path).unwrap();
        let parsed = parse_easl_without_comments(&library);
        assert!(
            parsed.parsing_failures.is_empty(),
            "{:?}",
            parsed.parsing_failures
        );
        documents.add_document(parsed, format!("{module}.easl"), library);
    }
    let (mut program, errors) = Program::from_easl_documents(&documents, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(errors.is_empty(), "{errors:?}");
    for runtime in [CpuRuntime::TreeWalking, CpuRuntime::BytecodeVm] {
        let io = run_program_entry_with_io_and_runtime_from_path(
            program.clone(),
            Some("main"),
            StringIO::new(),
            Path::new("runs-test.easl"),
            runtime,
        )
        .unwrap()
        .0;
        assert_eq!(io.events, [IOEvent::Print("done".into())], "{runtime:?}");
    }
}

#[test]
fn visual_intersections_preserve_logical_ranges_styles_and_level_context() {
    check(
        r#"
      (= source (make-text "abcdefgh")) (prepare 0u)
      (= r-scripts (zeroed-array 3u)) (= r-fonts (zeroed-array 3u))
      (= r-output (zeroed-array 8u))
      (= (r-scripts 0u) (TextScriptRun 0u 3u 25u))
      (= (r-scripts 1u) (TextScriptRun 3u 5u 8u))
      (= (r-scripts 2u) (TextScriptRun 5u 8u 2u))
      (= (r-fonts 0u) (TextFontRun 0u 4u 0u 10u))
      (= (r-fonts 1u) (TextFontRun 4u 7u 1u 20u))
      (= (r-fonts 2u) (TextFontRun 7u 8u 2u 30u))
      (= (b-runs 0u) (TextBidiRun 0u 2u 0u))
      (= (b-runs 1u) (TextBidiRun 6u 8u 2u))
      (= (b-runs 2u) (TextBidiRun 2u 6u 1u))
      (let [result (r-plan 3u 3u 3u 8u 1000000u)]
        (expect (== result.status 0u)) (expect (== result.count 7u))
        (expect (== r-writes 7u))
        (expect (r-same (r-output 0u) (TextShapeRun 0u 2u 25u 0u 10u 0u 0u 2u)))
        (expect (r-same (r-output 1u) (TextShapeRun 6u 7u 2u 1u 20u 2u 6u 8u)))
        (expect (r-same (r-output 2u) (TextShapeRun 7u 8u 2u 2u 30u 2u 6u 8u)))
        (expect (r-same (r-output 3u) (TextShapeRun 5u 6u 2u 1u 20u 1u 2u 6u)))
        (expect (r-same (r-output 4u) (TextShapeRun 4u 5u 8u 1u 20u 1u 2u 6u)))
        (expect (r-same (r-output 5u) (TextShapeRun 3u 4u 8u 0u 10u 1u 2u 6u)))
        (expect (r-same (r-output 6u) (TextShapeRun 2u 3u 25u 0u 10u 1u 2u 6u)))
        (= r-writes 0u)
        (expect (== (.status (r-plan 3u 3u 3u 6u 1000000u)) 2u))
        (expect (== r-writes 0u))
        (expect (== (.status (r-plan 3u 3u 3u 8u (- result.work 1u))) 2u))
        (expect (== r-writes 0u))
        (expect (== (.status (r-plan 3u 3u 3u 8u result.work)) 0u))
        (expect (== r-writes 7u)))
      ; Selected soft line only; paragraph script/style context is unchanged.
      (= (b-runs 0u) (TextBidiRun 4u 6u 1u))
      (expect (== (.count (r-plan 1u 3u 3u 8u 1000000u)) 2u))
      (expect (r-same (r-output 0u) (TextShapeRun 5u 6u 2u 1u 20u 1u 4u 6u)))
      (expect (r-same (r-output 1u) (TextShapeRun 4u 5u 8u 1u 20u 1u 4u 6u)))
    "#,
    );
}

#[test]
fn malformed_partitions_and_split_graphemes_never_publish_partial_runs() {
    check(
        r#"
      (= source (make-text "aéb")) (prepare 0u)
      (= r-scripts (zeroed-array 2u)) (= r-fonts (zeroed-array 2u))
      (= r-output (zeroed-array 8u))
      (= (r-scripts 0u) (TextScriptRun 0u 5u 25u))
      (= (r-fonts 0u) (TextFontRun 0u 4u 0u 0u))
      (= (r-fonts 1u) (TextFontRun 4u 5u 1u 1u))
      (= (b-runs 0u) (TextBidiRun 0u 5u 0u))
      (expect (== (.status (r-plan 1u 1u 2u 8u 1000000u)) 0u))
      (= r-writes 0u)
      ; Late malformed font run, then a font/style edge inside e + acute.
      (= (.start (r-fonts 1u)) 5u)
      (expect (== (.status (r-plan 1u 1u 2u 8u 1000000u)) 1u))
      (= (.start (r-fonts 1u)) 2u) (= (.end (r-fonts 0u)) 2u)
      (expect (== (.status (r-plan 1u 1u 2u 8u 1000000u)) 1u))
      (= (.start (r-fonts 1u)) 4u) (= (.end (r-fonts 0u)) 4u)
      (= (.end (r-scripts 0u)) 4u)
      (expect (== (.status (r-plan 1u 1u 2u 8u 1000000u)) 1u))
      (= (.end (r-scripts 0u)) 5u) (= (.script-code (r-scripts 0u)) 0u)
      (expect (== (.status (r-plan 1u 1u 2u 8u 1000000u)) 1u))
      (= (.script-code (r-scripts 0u)) 25u) (= (.level (b-runs 0u)) 127u)
      (expect (== (.status (r-plan 1u 1u 2u 8u 1000000u)) 1u))
      (= (.level (b-runs 0u)) 0u) (= (.end (b-runs 0u)) 2u)
      (expect (== (.status (r-plan 1u 1u 2u 8u 1000000u)) 3u))
      (= (.end (b-runs 0u)) 6u)
      (expect (== (.status (r-plan 1u 1u 2u 8u 1000000u)) 1u))
      (expect (== (.status (r-plan 1u 65536u 2u 8u 1000000u)) 2u))
      (expect (== (.status (r-plan 1u 1u 65536u 8u 1000000u)) 2u))
      (expect (== (.status (r-plan 65536u 1u 2u 8u 1000000u)) 2u))
      (expect (== (.status (r-plan 1u 1u 2u 8u 1000001u)) 2u))
      (expect (== (.status (r-plan 1u 1u 2u 8u 0u)) 2u))
      (expect (== r-writes 0u))
      (text-release source) (= source (make-text ""))
      (expect (== (.status (r-plan 0u 0u 0u 0u 0u)) 0u))
      (expect (== r-writes 0u))
    "#,
    );
}

#[test]
fn real_bidi_changes_inside_a_grapheme_report_the_fragmentation_gap_explicitly() {
    // U+0600 is Prepend (UAX #29) but AN (UAX #9); with an LTR base its
    // level differs from the following Latin letter, in the SAME grapheme.
    // Preserve both facts instead of silently rewriting levels or source.
    let text = "\u{600}a";
    let facts = easl::text::directional_words(text, 1).unwrap();
    assert_ne!(facts[3], facts[11]);
    assert_eq!(easl::text::grapheme_words(text).len(), 4);
    check(&format!(
        r#"
      (= source (make-text "{text}")) (prepare 1u)
      (= r-scripts (zeroed-array 1u)) (= r-fonts (zeroed-array 1u))
      (= r-output (zeroed-array 4u))
      (= (r-scripts 0u) (TextScriptRun 0u 3u 25u))
      (= (r-fonts 0u) (TextFontRun 0u 3u 0u 0u))
      (expect (== (.status (text-bidi-order 2u b-fact-at 0u 2u 2u b-slot-at b-emit-slot 1000000u)) 0u))
      (expect (== (.count (text-bidi-runs 2u b-slot-at b-fact-at 2u b-emit-run)) 2u))
      (expect (== (.status (r-plan 2u 1u 1u 4u 1000000u)) 3u))
      (expect (== r-writes 0u))
    "#
    ));
}

#[test]
fn script_font_and_bidi_runs_feed_real_shaping_cells_and_contextual_style_edges() {
    let amiri =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../easl-native-text/tests/fonts/amiri.ttf");
    let shantell = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../easl-native-text/tests/fonts/shantell-sans-regular.ttf");
    let body = format!(
        r#"
      (= r-amiri (load-font "{}" 32.)) (= r-shantell (load-font "{}" 32.))
      (= r-amiri-ink (font-atlas-glyphs r-amiri)) (= r-shantell-ink (font-atlas-glyphs r-shantell))
      (= source (make-text "Office (Привет) العربية 123")) (prepare 0u)
      (= r-script-facts (text-script-facts source))
      (= r-scripts (zeroed-array 64u)) (= r-fonts (zeroed-array 64u)) (= r-output (zeroed-array 64u))
      (= r-quads (zeroed-array 128u)) (= r-clusters (zeroed-array 128u)) (= r-cells (zeroed-array 128u))
      (let [scripts (text-itemize-scripts (array-length r-script-facts) r-script-fact-at 0u
                      (array-length r-script-facts) 25u 64u r-emit-script 1000000u)
            ordered (text-bidi-order (array-length b-facts) b-fact-at 0u
                      (array-length b-facts) (array-length b-slots) b-slot-at b-emit-slot 1000000u)]
        (expect (== scripts.status 0u)) (expect (== ordered.status 0u))
        ; Explicit fixture font selection: Shantell covers Cyrillic, Amiri Arabic/Latin.
        (for [i scripts.count]
          (let [item (r-scripts i)]
            (= (r-fonts i) (TextFontRun item.start item.end (if (== item.script-code 8u) 1u 0u) 0u))))
        (let [levels (text-bidi-runs ordered.count b-slot-at b-fact-at 64u b-emit-run)]
          (expect (== levels.status 0u))
          (let [plan (r-plan levels.count scripts.count scripts.count 64u 1000000u) @var x 40.]
            (expect (== plan.status 0u)) (expect (> plan.count levels.count))
            (for [i plan.count]
              (let [run (r-output i) rtl (!= (& run.level 1u) 0u)]
                (r-shape run)
                (for [j (array-length r-glyphs)] (expect (!= (.id (r-glyphs j)) 0u)))
                (let [placed (text-place-run-geometry (array-length r-glyphs) r-glyph-at r-ink-at
                      (if (== run.font 0u) (vec2f (texture-dimensions r-amiri)) (vec2f (texture-dimensions r-shantell)))
                      (vec2f x 80.) rtl r-emit-quad 0u (vec2f 30. 70.) run.start r-emit-cluster)]
                  (+= x placed.advance) (+= r-cluster-base placed.clusters)
                  (+= r-quad-base (array-length r-glyphs)))))
            (let [cells (text-build-cells source r-cluster-base r-cluster-at 128u r-emit-cell)]
              (expect (== cells.status 0u)) (expect (== cells.count 27u))
              ; Every original grapheme has exactly one cell, in visual run order.
              (let [@var cursor 0u]
                (while (< cursor (text-length source))
                  (let [@var copies 0u after (text-boundary source cursor 1u)]
                    (for [i cells.count]
                      (let [cell (r-cells i)]
                        (when (== cell.start cursor) (+= copies 1u) (expect (== cell.end after)))))
                    (expect (== copies 1u)) (= cursor after))))
              ; Arabic starts at byte 22, with distinct upstream/downstream carets.
              (let [up (text-caret-at cells.count r-cell-at 22u 0u)
                    down (text-caret-at cells.count r-cell-at 22u 1u)]
                (expect (== up.status 0u)) (expect (== down.status 0u))
                (expect (> down.rect.x up.rect.x)))
              (expect (== (.byte (text-hit-test cells.count r-cell-at (vec2f 40. 60.))) 0u))))))
      (text-release source) (= source (make-text "ببب")) (prepare 0u)
      (= (r-scripts 0u) (TextScriptRun 0u 6u 2u))
      (= (r-fonts 0u) (TextFontRun 0u 2u 0u 10u))
      (= (r-fonts 1u) (TextFontRun 2u 4u 0u 20u))
      (= (r-fonts 2u) (TextFontRun 4u 6u 0u 30u))
      (expect (== (.status (text-bidi-order 3u b-fact-at 0u 3u 3u b-slot-at b-emit-slot 1000000u)) 0u))
      (expect (== (.count (text-bidi-runs 3u b-slot-at b-fact-at 3u b-emit-run)) 1u))
      (expect (== (.count (r-plan 1u 1u 3u 64u 1000000u)) 3u))
      (expect (r-same (r-output 1u) (TextShapeRun 2u 4u 2u 0u 20u 1u 0u 6u)))
      (r-shape (r-output 1u))
      (expect (== (array-length r-glyphs) 1u)) (expect (== (.id (r-glyphs 0u)) 1588u))
      ; At a genuine line edge, the same source character must become isolated.
      (expect (== (.status (text-bidi-order 3u b-fact-at 1u 2u 3u b-slot-at b-emit-slot 1000000u)) 0u))
      (expect (== (.count (text-bidi-runs 1u b-slot-at b-fact-at 3u b-emit-run)) 1u))
      (expect (== (.count (r-plan 1u 1u 3u 64u 1000000u)) 1u))
      (r-shape (r-output 0u))
      (expect (== (array-length r-glyphs) 1u)) (expect (== (.id (r-glyphs 0u)) 56u))
    "#,
        amiri.display(),
        shantell.display()
    );
    check_extra(
        &body,
        r#"
      @{group 0 binding 0} (var r-amiri: (Texture2D f32))
      @{group 0 binding 1} (var r-shantell: (Texture2D f32))
      (var r-amiri-ink: [FontAtlasGlyph]) (var r-shantell-ink: [FontAtlasGlyph])
      (var r-glyphs: [FontShapedGlyph]) (var r-active-font: u32)
      (var r-script-facts: [TextScriptFact])
      (var r-quads: [TextGlyphQuad]) (var r-clusters: [TextClusterBox]) (var r-cells: [TextCell])
      (var r-quad-base: u32) (var r-cluster-base: u32)
      (defn r-script-fact-at [i: u32]: TextScriptFact (r-script-facts i))
      (defn r-emit-script [i: u32 item: TextScriptRun] (= (r-scripts i) item))
      (defn r-glyph-at [i: u32]: FontShapedGlyph (r-glyphs i))
      (defn r-ink-at [i: u32]: FontAtlasGlyph
        (if (== r-active-font 0u) (r-amiri-ink i) (r-shantell-ink i)))
      (defn r-cluster-at [i: u32]: TextClusterBox (r-clusters i))
      (defn r-cell-at [i: u32]: TextCell (r-cells i))
      (defn r-emit-quad [i: u32 item: TextGlyphQuad] (= (r-quads (+ r-quad-base i)) item))
      (defn r-emit-cluster [i: u32 item: TextClusterBox] (= (r-clusters (+ r-cluster-base i)) item))
      (defn r-emit-cell [i: u32 item: TextCell] (= (r-cells i) item))
      (defn r-shape [run: TextShapeRun]
        (= r-active-font run.font)
        (let [context (text-slice source run.context-start run.context-end)
              start (- run.start run.context-start) end (- run.end run.context-start)
              script (text-script-tag run.script-code) rtl (!= (& run.level 1u) 0u)]
          (if (== run.font 0u)
            (= r-glyphs (shape-font-span r-amiri context start end script rtl))
            (= r-glyphs (shape-font-span r-shantell context start end script rtl)))
          (text-release context)))
    "#,
        &["bidi", "script", "runs", "atlas", "geometry"],
    );
}

#[test]
fn thousands_of_style_edges_obey_capacity_and_work_before_publication() {
    let source = "x".repeat(4096);
    check_extra(
        &format!(
            r#"
      (= source (make-text "{source}")) (prepare 0u)
      (= r-scripts (zeroed-array 1u)) (= r-output (zeroed-array 4096u))
      (= (r-scripts 0u) (TextScriptRun 0u 4096u 25u))
      (= (b-runs 0u) (TextBidiRun 0u 4096u 1u))
      (let [expected-work 12356u]
        (expect (== (.status (text-plan-runs source 1u b-run-at 1u r-script-at
          4096u r-every-font 4095u r-emit 1000000u)) 2u))
        (expect (== r-writes 0u))
        (expect (== (.status (text-plan-runs source 1u b-run-at 1u r-script-at
          4096u r-every-font 4096u r-emit (- expected-work 1u))) 2u))
        (expect (== r-writes 0u))
        (let [result (text-plan-runs source 1u b-run-at 1u r-script-at
          4096u r-every-font 4096u r-emit expected-work)]
          (expect (== result.status 0u)) (expect (== result.count 4096u))
          (expect (== result.work expected-work)) (expect (== r-writes 4096u))))
      (for [i 4096u]
        (let [start (- 4095u i)]
          (expect (r-same (r-output i)
            (TextShapeRun start (+ start 1u) 25u (% start 3u) start 1u 0u 4096u)))))
    "#
        ),
        r#"
      (defn r-every-font [i: u32]: TextFontRun (TextFontRun i (+ i 1u) (% i 3u) i))
    "#,
        &["bidi", "script", "runs"],
    );
}
