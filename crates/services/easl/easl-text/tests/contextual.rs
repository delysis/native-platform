use easl::{
    CompilerTarget,
    compiler::{builtins::built_in_macros, program::Program},
    interpreter::{CpuRuntime, IOEvent, StringIO, run_program_entry_with_io_and_runtime_from_path},
    parse::{EaslMultiDocument, parse_easl_without_comments},
};
use std::path::Path;

fn check(body: &str, extra: &str) {
    let source = format!(
        r#"
      (var c-source: TextBuffer) (var c-assertion: u32)
      (defn expect [ok: bool] (+= c-assertion 1u) (when (not ok) (print c-assertion)))
      {extra}
      @cpu (defn main [] (= c-source (make-text "")) {body} (text-release c-source) (print "done"))
    "#
    );
    let parsed = parse_easl_without_comments(&source);
    assert!(
        parsed.parsing_failures.is_empty(),
        "{:?}",
        parsed.parsing_failures
    );
    let mut docs =
        EaslMultiDocument::from_singular_document(parsed, "contextual-test.easl".into(), source);
    for module in [
        "atlas",
        "paragraph",
        "flow",
        "script",
        "bidi",
        "runs",
        "measure",
    ] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("library/{module}.easl"));
        let source = std::fs::read_to_string(path).unwrap();
        let parsed = parse_easl_without_comments(&source);
        assert!(
            parsed.parsing_failures.is_empty(),
            "{module}: {:?}",
            parsed.parsing_failures
        );
        docs.add_document(parsed, format!("{module}.easl"), source);
    }
    let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(errors.is_empty(), "{errors:?}");
    for runtime in [CpuRuntime::TreeWalking, CpuRuntime::BytecodeVm] {
        let io = run_program_entry_with_io_and_runtime_from_path(
            program.clone(),
            Some("main"),
            StringIO::new(),
            Path::new("contextual-test.easl"),
            runtime,
        )
        .unwrap()
        .0;
        assert_eq!(io.events, [IOEvent::Print("done".into())], "{runtime:?}");
    }
}

#[test]
fn actual_arabic_line_can_fit_when_its_suffix_does_not() {
    let font_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../easl-native-text/tests/fonts/amiri.ttf");
    let font = easl::font::FontAtlas::from_bytes(std::fs::read(&font_path).unwrap(), 32.)
        .unwrap()
        .0;
    let width = |text: &str| {
        font.shape_span(text, 0..text.len(), u32::from_be_bytes(*b"arab"), true)
            .unwrap()
            .iter()
            .map(|g| g.advance)
            .sum::<f32>()
    };
    // Independent HarfBuzz 12.3.2, Amiri 1000 upem: lam + meem = 441 font
    // units; isolated meem = 452. At 32ppem the longer string alone fits 14.3px.
    assert!((width("لم") - 441. * 0.032).abs() < 0.00001);
    assert!((width("م") - 452. * 0.032).abs() < 0.00001);
    check(
        &format!(
            r#"
      (text-release c-source) (= c-source (make-text "لم"))
      (= c-font (load-font "{}" 32.)) (= c-breaks (zeroed-array 3u))
      (let [pruned (text-compose 3u c-point c-measure 14.3 (text-default-policy) c-emit)]
        (expect (== pruned.status 2u)) (expect (== c-writes 0u)))
      (let [result (text-compose-contextual 3u c-point c-measure 14.3 (text-default-policy) c-emit)]
        (expect (== result.status 0u)) (expect (== result.count 1u))
        (expect (== (c-breaks 0u) 2u)) (expect (== c-writes 1u)))
      (= c-writes 0u)
      (expect (== (.status (text-compose-contextual 3u c-point c-measure 14.3
        (TextPolicy 0.05 0.2 0. 1u) c-emit)) 3u))
      (expect (== c-writes 0u))
    "#,
            font_path.display()
        ),
        r#"
      @{group 0 binding 0} (var c-font: (Texture2D f32))
      (var c-glyphs: [FontShapedGlyph]) (var c-breaks: [u32]) (var c-writes: u32)
      (defn c-point [i: u32]: TextBreak
        (TextBreak i (vec2f 0.) (vec2f 0.) 0. (if (== i 2u) 1u 0u) 4294967295u))
      (defn c-emit [i: u32 item: u32] (+= c-writes 1u) (= (c-breaks i) item))
      (defn c-measure [start: u32 end: u32 budget: u32]: TextLineMeasure
        (when (== budget 0u) (return (TextLineMeasure 3u 0. 0u)))
        (let [part (text-slice c-source (* start 2u) (* end 2u)) @var advance 0.]
          (= c-glyphs (shape-font-span c-font part 0u (text-length part) (text-script-arabic) true))
          (text-release part)
          (for [i (array-length c-glyphs)] (+= advance (.advance (c-glyphs i))))
          (TextLineMeasure 0u advance 1u)))
    "#,
    );
}

#[test]
fn contextual_composition_matches_exhaustive_partitions_and_prefers_fitting_text() {
    // The oracle enumerates every partition, independently of the EASL DP.
    // Deliberately nonmonotone measurements model contextual width changes.
    let mut body = String::from("(= c-breaks (zeroed-array 7u))\n");
    let mut fitting = false;
    let mut unavoidable_overflow = false;
    for seed in 0..32_u32 {
        let measure = |start: u32, end: u32| {
            let ordinary = 1 + (seed * 19 + start * 7 + end * 11 + start * end * 5) % 15;
            // Half the cases have one character that cannot fit in any span.
            f64::from(if seed % 2 == 1 && start <= 3 && end > 3 {
                10 + ordinary % 5
            } else {
                ordinary
            })
        };
        for weight in [0., 1., 100.] {
            let best = (0..32)
                .filter_map(|mask| {
                    let mut start = 0;
                    let mut overflow = 0_u32;
                    let mut cost = 0.;
                    for end in (1..6).filter(|i| mask & (1 << (i - 1)) != 0).chain([6]) {
                        let occupied = measure(start, end);
                        if occupied > 9. {
                            if end != start + 1 {
                                return None;
                            }
                            overflow += 1;
                        }
                        let slack = (9. - occupied).max(0.) / 9.;
                        cost += slack * slack * if end == 6 { weight } else { 1. };
                        start = end;
                    }
                    Some((overflow, cost))
                })
                .min_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.total_cmp(&b.1)))
                .unwrap();
            fitting |= best.0 == 0;
            unavoidable_overflow |= best.0 > 0;
            body += &format!(
                r#"
              (= c-seed {seed}u)
              (let [chosen (text-compose-contextual 7u c-point c-measure 9.
                      (TextPolicy 0. 0. {weight:.1} 1000000u) c-emit)
                    @var start 0u @var overflow 0u @var cost 0.]
                (expect (== chosen.status 0u))
                (for [i chosen.count]
                  (let [end (c-breaks i) occupied (.advance (c-measure start end 0u))
                        slack (/ (max 0. (- 9. occupied)) 9.)]
                    (when (> occupied 9.) (expect (== end (+ start 1u))) (+= overflow 1u))
                    (+= cost (* (* slack slack) (if (== end 6u) {weight:.1} 1.)))
                    (= start end)))
                (expect (== start 6u)) (expect (== overflow {overflow}u))
                (expect (< (abs (- cost {cost:.9})) 0.0001)))
            "#,
                overflow = best.0,
                cost = best.1,
            );
        }
    }
    assert!(fitting && unavoidable_overflow);
    check(
        &body,
        r#"
      (var c-seed: u32) (var c-breaks: [u32])
      (defn c-point [i: u32]: TextBreak
        (TextBreak i (vec2f 0.) (vec2f 0.) 0. (if (== i 6u) 1u 0u)
          (if (== i 0u) 4294967295u (- i 1u))))
      (defn c-measure [start: u32 end: u32 budget: u32]: TextLineMeasure
        (let [ordinary (+ 1u (% (+ (+ (* c-seed 19u) (* start 7u))
          (+ (* end 11u) (* (* start end) 5u))) 15u))]
          (TextLineMeasure 0u (f32 (if (and (== (% c-seed 2u) 1u) (and (<= start 3u) (> end 3u)))
            (+ 10u (% ordinary 5u)) ordinary)) 0u)))
      (defn c-emit [i: u32 item: u32] (= (c-breaks i) item))
    "#,
    );
}

#[test]
fn shaping_safe_prefixes_match_independently_reshaped_substrings() {
    let font =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../easl-native-text/tests/fonts/amiri.ttf");
    let mut body = format!("(= c-font (load-font \"{}\" 32.))\n", font.display());
    for (source, arabic) in [
        ("Office café é", false),
        ("ببب لم العربية", true),
        ("", false),
    ] {
        body += &format!(
            r#"
          (text-release c-source) (= c-source (make-text "{source}")) (= c-arabic {arabic})
          (= c-facts (text-graphemes c-source))
          (= c-slots (zeroed-array (+ 1u (array-length c-facts))))
          (= c-prefixes (zeroed-array (+ 1u (array-length c-facts))))
          (c-shape 0u (text-length c-source))
          (let [result (text-measure-run (array-length c-facts) c-fact-at (array-length c-glyphs)
                         c-glyph-at 0u (array-length c-slots) c-slot-at c-slot-write 1000000u)]
            (expect (== result.status 0u)))
          (expect (== (.status (text-measure-prefixes (array-length c-facts) c-slot-at
                         (array-length c-prefixes) c-prefix-write)) 0u))
          (let [facts (array-length c-facts)]
            (for [start facts]
              (for [end (+ start 1u) (<= end facts) (+= end 1u)]
                (let [a (c-prefixes start) b (c-prefixes end)]
                  (when (and a.safe b.safe)
                    (c-shape (.start (c-facts start))
                      (if (== end facts) (text-length c-source) (.start (c-facts end))))
                    (let [@var actual 0.]
                      (for [i (array-length c-glyphs)] (+= actual (.advance (c-glyphs i))))
                      (expect (< (abs (- actual (+ (- b.advance.x a.advance.x)
                                                   (- b.advance.y a.advance.y)))) 0.001))))))))
        "#
        );
    }
    check(
        &body,
        r#"
      @{group 0 binding 0} (var c-font: (Texture2D f32))
      (var c-arabic: bool) (var c-facts: [TextGrapheme]) (var c-glyphs: [FontShapedGlyph])
      (var c-slots: [TextMeasureSlot]) (var c-prefixes: [TextShapedPrefix])
      (defn c-fact-at [i: u32]: TextGrapheme (c-facts i))
      (defn c-glyph-at [i: u32]: FontShapedGlyph (c-glyphs i))
      (defn c-slot-at [i: u32]: TextMeasureSlot (c-slots i))
      (defn c-slot-write [i: u32 item: TextMeasureSlot] (= (c-slots i) item))
      (defn c-prefix-write [i: u32 item: TextShapedPrefix] (= (c-prefixes i) item))
      (defn c-shape [start: u32 end: u32]
        (let [part (text-slice c-source start end)]
          (= c-glyphs (shape-font-span c-font part 0u (text-length part)
            (if c-arabic (text-script-arabic) (text-script-latin)) c-arabic))
          (text-release part)))
    "#,
    );
}

#[test]
fn measurements_preflight_malformed_overlap_capacity_work_and_incomplete_coverage() {
    check(
        r#"
      (text-release c-source) (= c-source (make-text "abc"))
      (= c-facts (text-graphemes c-source)) (= c-slots (zeroed-array 4u))
      (= c-glyphs (zeroed-array 3u)) (= c-prefixes (zeroed-array 4u))
      (for [i 3u] (= (c-glyphs i) (FontShapedGlyph 1u i (+ i 1u) 0. 0. 10. 0u)))
      (expect (== (.status (text-measure-run 3u c-fact-at 3u c-glyph-at 0u 3u c-slot-at c-slot-write 1000000u)) 2u))
      (expect (== c-writes 0u))
      (= (.end (c-glyphs 2u)) 2u)
      (expect (== (.status (text-measure-run 3u c-fact-at 3u c-glyph-at 0u 4u c-slot-at c-slot-write 1000000u)) 1u))
      (expect (== c-writes 0u))
      (= (.end (c-glyphs 2u)) 3u)
      (expect (== (.status (text-measure-run 3u c-fact-at 3u c-glyph-at 0u 4u c-slot-at c-slot-write 215u)) 2u))
      (expect (== c-writes 0u))
      (let [result (text-measure-run 3u c-fact-at 3u c-glyph-at 0u 4u c-slot-at c-slot-write 216u)]
        (expect (== result.status 0u)) (expect (== result.work 216u)))
      (= c-writes 0u)
      (expect (== (.status (text-measure-run 3u c-fact-at 3u c-glyph-at 0u 4u c-slot-at c-slot-write 216u)) 1u))
      (expect (== c-writes 0u))
      (= (.covered (c-slots 2u)) false)
      (expect (== (.status (text-measure-prefixes 3u c-slot-at 4u c-prefix-write)) 1u))
      (expect (== c-published 0u))
      (= (.covered (c-slots 2u)) true)
      (= (.amount (c-slots 3u)) (vec2f 100000000000000. 0.))
      (= (.amount (c-slots 2u)) (vec2f 100000000000000. 0.))
      (expect (== (.status (text-measure-prefixes 3u c-slot-at 4u c-prefix-write)) 1u))
      (expect (== c-published 0u))
      (= (.amount (c-slots 3u)) (vec2f 10. 0.)) (= (.amount (c-slots 2u)) (vec2f 10. 0.))
      (expect (== (.status (text-measure-prefixes 3u c-slot-at 4u c-prefix-write)) 0u))
      (expect (== c-published 4u)) (expect (== (.x (.advance (c-prefixes 3u))) 30.))
      ; A glyph can move backward while its whole cluster still advances.
      (= c-slots (zeroed-array 4u)) (= c-writes 0u) (= c-published 0u)
      (= (c-glyphs 0u) (FontShapedGlyph 1u 0u 1u 0. 0. 1. 0u))
      (= (c-glyphs 1u) (FontShapedGlyph 2u 0u 1u 0. 0. -2. 0u))
      (= (c-glyphs 2u) (FontShapedGlyph 3u 1u 3u 0. 0. 12. 0u))
      (expect (== (.status (text-measure-run 3u c-fact-at 3u c-glyph-at 0u 4u c-slot-at c-slot-write 1000000u)) 1u))
      (expect (== c-writes 0u))
      (= (.advance (c-glyphs 0u)) 10.)
      (expect (== (.status (text-measure-run 3u c-fact-at 3u c-glyph-at 0u 4u c-slot-at c-slot-write 1000000u)) 0u))
      (expect (== (.status (text-measure-prefixes 3u c-slot-at 4u c-prefix-write)) 0u))
      (expect (== (.x (.advance (c-prefixes 3u))) 20.))
      (expect (not (.safe (c-prefixes 2u))))
    "#,
        r#"
      (var c-facts: [TextGrapheme]) (var c-glyphs: [FontShapedGlyph])
      (var c-slots: [TextMeasureSlot]) (var c-prefixes: [TextShapedPrefix])
      (var c-writes: u32) (var c-published: u32)
      (defn c-fact-at [i: u32]: TextGrapheme (c-facts i))
      (defn c-glyph-at [i: u32]: FontShapedGlyph (c-glyphs i))
      (defn c-slot-at [i: u32]: TextMeasureSlot (c-slots i))
      (defn c-slot-write [i: u32 item: TextMeasureSlot] (+= c-writes 1u) (= (c-slots i) item))
      (defn c-prefix-write [i: u32 item: TextShapedPrefix] (+= c-published 1u) (= (c-prefixes i) item))
    "#,
    );
}

fn check_scene(body: &str) {
    check_rich_scene(body, "");
}

fn check_rich_scene(body: &str, extra: &str) {
    // EASL strings do not decode escapes: supply the exact fixture control bytes.
    let body = body.replace("\\r\\n", "\r\n").replace("\\n", "\n");
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/contextual-proof.easl");
    let source = format!(
        r#"
      (import "contextual_layout.easl")
      {extra}
      (var proof-assertion: u32)
      (defn proof-expect [ok: bool] (+= proof-assertion 1u) (when (not ok) (print proof-assertion)))
      @cpu (defn main [] {body} (text-editor-close demo-editor) (print "done"))
    "#
    );
    let docs = easl::parse::load_easl_imports_with_lookup_function(
        parse_easl_without_comments(&source),
        Some(&path),
        source,
        easl::parse::ImportLimits::default(),
        |path| std::fs::read_to_string(path),
    )
    .unwrap_or_else(|docs| {
        panic!(
            "Parse errors: {:?}",
            docs.sources
                .iter()
                .filter(|(d, _, _)| !d.parsing_failures.is_empty())
                .map(|(d, p, _)| (p, &d.parsing_failures))
                .collect::<Vec<_>>()
        )
    })
    .unwrap_or_else(|(_, errors)| panic!("Import errors: {errors:?}"));
    let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(errors.is_empty(), "{errors:?}");
    for runtime in [CpuRuntime::TreeWalking, CpuRuntime::BytecodeVm] {
        let io = run_program_entry_with_io_and_runtime_from_path(
            program.clone(),
            Some("main"),
            StringIO::new(),
            &path,
            runtime,
        )
        .unwrap()
        .0;
        assert_eq!(io.events, [IOEvent::Print("done".into())], "{runtime:?}");
    }
}

#[test]
fn real_contextual_multiline_document_reuses_safe_runs_and_keeps_source_cells() {
    check_scene(
        r#"
      (= specimen-font (load-font "../../easl-native-text/tests/fonts/amiri.ttf" 32.))
      (= specimen-metrics (font-metrics specimen-font)) (= specimen-ink (font-atlas-glyphs specimen-font))
      (let [seed (make-text "Office العربية keeps its measure. الكتابة تبقى واضحة. Café élan.")]
        (text-editor-open demo-editor seed) (text-release seed))
      (= demo-viewport (vec2f 900. 650.)) (= flow-width 180.)
      (proof-expect (== (contextual-layout) 0u))
      (proof-expect (> flow-line-count 1u))
      (let [source (text-editor-visible demo-editor) shapes ctx-shapes preparations ctx-preparations
            first-lines flow-line-count @var cursor 0u]
        (while (< cursor (text-length source))
          (let [@var copies 0u after (text-boundary source cursor 1u)]
            (for [i demo-cell-count]
              (let [cell (demo-cells i)]
                (when (== cell.start cursor) (+= copies 1u) (proof-expect (== cell.end after)))))
            (proof-expect (== copies 1u)) (= cursor after)))
        (let [edge (.end (flow-lines 0u))
              before (text-caret-at demo-cell-count demo-cell-at edge 0u)
              after (text-caret-at demo-cell-count demo-cell-at edge 1u)]
          (proof-expect (== before.status 0u)) (proof-expect (== after.status 0u))
          (proof-expect (> after.line before.line)))
        (= flow-width 330.) (proof-expect (== (contextual-layout) 0u))
        (proof-expect (< flow-line-count first-lines))
        (proof-expect (== ctx-preparations preparations))
        (proof-expect (== ctx-shapes shapes))
        (proof-expect (> ctx-cache-hits 0u))
        (proof-expect (== demo-source.handle source.handle)))
      (text-editor-close demo-editor)
      (let [seed (make-text "لملم")]
        (text-editor-open demo-editor seed) (text-release seed))
      (= flow-width 14.3) (proof-expect (== (contextual-layout) 0u))
      (proof-expect (== flow-line-count 2u))
      (proof-expect (== (.end (flow-lines 0u)) 4u))
      (proof-expect (== (.end (flow-lines 1u)) 8u))
      (proof-expect (<= (.z (.bounds (flow-lines 0u))) 14.31))
      (proof-expect (<= (.z (.bounds (flow-lines 1u))) 14.31))
      (text-editor-close demo-editor)
      (let [seed (make-text "First العربية.\r\n\r\nثانٍ paragraph.\n")]
        (text-editor-open demo-editor seed) (text-release seed))
      (= flow-width 330.) (proof-expect (== (contextual-layout) 0u))
      (proof-expect (== flow-line-count 4u))
      (proof-expect (== (.end (flow-lines 3u)) (text-length demo-editor.current.text)))
      (proof-expect (== (.start (flow-lines 3u)) (.end (flow-lines 3u))))
    "#,
    );
}

#[test]
fn contextual_fallback_uses_three_atlases_and_keeps_choices_across_reflow() {
    check_scene(
        r#"
      (= specimen-font (load-font "../../easl-native-text/tests/fonts/amiri.ttf" 32.))
      (= specimen-metrics (font-metrics specimen-font)) (= specimen-ink (font-atlas-glyphs specimen-font))
      (let [seed (make-text "Office Привет é 𑼒𑽂𑼒𑼶 العربية")]
        (text-editor-open demo-editor seed) (text-release seed))
      (= demo-viewport (vec2f 900. 650.)) (= flow-width 220.)
      (proof-expect (== (contextual-layout) 0u))
      (proof-expect (> ctx-missing-glyphs 0u))
      (line-enable-cascade)
      (proof-expect (== (contextual-layout) 0u))
      (proof-expect (== ctx-missing-glyphs 0u)) (proof-expect (== ctx-font-mask 7u))
      (proof-expect (== line-unresolved-graphemes 0u))
      (let [source (text-editor-visible demo-editor) probes cascade-probes preparations ctx-preparations
            @var cursor 0u]
        (while (< cursor (text-length source))
          (let [@var copies 0u end (text-boundary source cursor 1u)]
            (for [i demo-cell-count]
              (let [cell (demo-cells i)]
                (when (== cell.start cursor) (+= copies 1u) (proof-expect (== cell.end end)))))
            (proof-expect (== copies 1u)) (= cursor end)))
        (= flow-width 500.) (proof-expect (== (contextual-layout) 0u))
        (proof-expect (== cascade-probes probes)) (proof-expect (== ctx-preparations preparations))
        (proof-expect (== ctx-missing-glyphs 0u)) (proof-expect (== ctx-font-mask 7u))
        (proof-expect (== demo-source.handle source.handle)))
      (text-editor-close demo-editor)
      (let [seed (make-text "🙂")]
        (text-editor-open demo-editor seed) (text-release seed))
      (proof-expect (== (contextual-layout) 0u))
      (proof-expect (> ctx-missing-glyphs 0u)) (proof-expect (== line-unresolved-graphemes 1u))
      (proof-expect (== demo-cell-count 1u)) (proof-expect (== (.end (demo-cells 0u)) 4u))
    "#,
    );
}

#[test]
fn rich_multiline_styles_share_line_boxes_source_cells_and_cached_reflow() {
    check_rich_scene(
        r#"
      (= specimen-font (load-font "../../easl-native-text/tests/fonts/amiri.ttf" 32.))
      (= specimen-metrics (font-metrics specimen-font)) (= specimen-ink (font-atlas-glyphs specimen-font))
      (let [seed (make-text "Small LARGE tail words.\r\n\r\nآخر words.\n")]
        (text-editor-open demo-editor seed) (text-release seed))
      (= proof-large 80.) (= demo-viewport (vec2f 1000. 650.)) (= flow-width 900.)
      (proof-expect (== (proof-layout) 0u))
      (proof-expect (== flow-line-count 4u))
      (proof-expect (> ctx-decoration-count 0u))
      (proof-expect (>= (.w (.bounds (flow-lines 0u))) 99.999))
      (proof-expect (< (abs (- (.w (.bounds (flow-lines 1u))) 30.)) 0.001))
      (let [@var y 24. @var reds 0u @var blues 0u]
        (for [i flow-line-count]
          (let [line (flow-lines i)]
            (proof-expect (< (abs (- line.bounds.y y)) 0.001))
            (for [j demo-cell-count]
              (let [cell (demo-cells j)]
                (when (== cell.line i)
                  (proof-expect (== cell.bounds.y line.bounds.y))
                  (proof-expect (== cell.bounds.w line.bounds.w)))))
            (+= y line.bounds.w)))
        (proof-expect (< (abs (- (.y (.extent (flow-widget-layout))) (+ y 24.))) 0.001))
        (for [i specimen-count]
          (let [q (specimen-quads i) c (ctx-paint-colors i)]
            (if (and (>= q.start 6u) (< q.start 11u))
              (let [] (proof-expect (all (== c (vec4u 180u 20u 20u 255u)))) (+= reds 1u))
              (let [] (proof-expect (all (== c (vec4u 20u 40u 140u 255u)))) (+= blues 1u)))))
        (proof-expect (> reds 0u)) (proof-expect (> blues 0u)))
      (let [source (text-editor-visible demo-editor) @var cursor 0u]
        (while (< cursor (text-length source))
          (let [after (text-boundary source cursor 1u) @var copies 0u]
            (for [i demo-cell-count]
              (let [cell (demo-cells i)]
                (when (== cell.start cursor) (proof-expect (== cell.end after)) (+= copies 1u))))
            (proof-expect (== copies 1u)) (= cursor after)))
        (proof-expect (== demo-editor.undo-count 0u))
        (proof-expect (== source.handle demo-source.handle)))
      ; Source styles cannot split the CRLF grapheme, even before projection.
      (let [count specimen-count]
        (proof-expect (== (contextual-layout-styled 1u
          (fn [i] (TextStyledSpan 24u 26u (proof-style true))) (fn [s] 0u) (proof-style false)) 4u))
        (proof-expect (== specimen-count count)))
      ; A late invalid face must retain all published glyph/color/decoration and
      ; caret arrays, even though the preceding paragraph was already staged.
      (let [first (specimen-quads 0u) color (ctx-paint-colors 0u) builds cascade-page-builds
            decoration (ctx-paint-decorations 0u) count specimen-count cells demo-cell-count]
        (= proof-bad true) (proof-expect (== (proof-layout) 1u))
        (proof-expect (== specimen-count count)) (proof-expect (== demo-cell-count cells))
        (proof-expect (== cascade-page-builds builds))
        (proof-expect (all (== (.bounds (specimen-quads 0u)) first.bounds)))
        (proof-expect (all (== (ctx-paint-colors 0u) color)))
        (proof-expect (all (== (.bounds (ctx-paint-decorations 0u)) decoration.bounds)))
        (= proof-bad false) (proof-expect (== (proof-layout) 0u)))
      (text-editor-close demo-editor)
      (let [seed (make-text "Small LARGE tail words keep their measure.")]
        (text-editor-open demo-editor seed) (text-release seed))
      (= flow-width 210.) (proof-expect (== (proof-layout) 0u))
      (let [shapes ctx-shapes preparations ctx-preparations original-lines flow-line-count
            source demo-source.handle]
        (= flow-width 600.) (proof-expect (== (proof-layout) 0u))
        (proof-expect (< flow-line-count original-lines))
        (proof-expect (== ctx-shapes shapes)) (proof-expect (== ctx-preparations preparations))
        ; Same bytes, changed style: stale shaping must not survive.
        (= proof-large 36.) (proof-expect (== (proof-layout) 0u))
        (proof-expect (> ctx-preparations preparations)) (proof-expect (> ctx-shapes shapes))
        (proof-expect (== demo-source.handle source)) (proof-expect (== demo-editor.undo-count 0u)))
      (line-close)
    "#,
        r#"
      (var proof-bad: bool) (var proof-large: f32)
      (defn proof-style [large: bool]: TextRunStyle
        (TextRunStyle 0u (if large proof-large 20.) (if large 100. 30.) 400. 0. 0.
          0u 0u 4294967295u (if large (vec4u 180u 20u 20u 255u) (vec4u 20u 40u 140u 255u))
          (if large 22u 18u)))
      (defn proof-span [i: u32]: TextStyledSpan
        (let [@var s (proof-style (== i 1u))]
          (when (and proof-bad (== i 3u)) (= s.family 999u))
          (TextStyledSpan (match i 0u 0u 1u 6u 2u 11u _ 27u)
            (match i 0u 6u 1u 11u 2u 27u _ (text-length (text-editor-visible demo-editor))) s)))
      (defn proof-layout []: u32
        (contextual-layout-styled 4u proof-span (fn [s] (if (== s.family 999u) 3u 0u)) (proof-style false)))
    "#,
    );
}

#[test]
fn selected_font_pages_follow_single_line_edits_and_keep_visible_ink() {
    check_scene(
        r#"
      (demo-prepare)
      (let [replacement (make-text "ZZZ")]
        (= demo-editor.current.anchor 0u) (= demo-editor.current.focus (text-length demo-editor.current.text))
        (proof-expect (== (text-editor-commit demo-editor replacement) 0u))
        (text-release replacement))
      (demo-layout)
      (proof-expect (== demo-layout-status 0u))
      (proof-expect (== specimen-count 3u))
      (for [i specimen-count]
        (proof-expect (all (> (.zw (.bounds (specimen-quads i))) (vec2f 0.)))))
      (= cascade-resident (font-rasterized-glyphs specimen-font))
      (proof-expect (== (array-length cascade-resident) 1u))
      (line-enable-cascade)
      (let [resident (cascade-resident 0u) first (specimen-quads 0u)]
        ; Primary page zero is staged successfully, then the second font's
        ; invalid glyph rejects the batch. Neither bound page nor UVs change.
        (proof-expect (== (cascade-prepare-pages 2u (fn [i] i)
          (fn [i] (if (== i 0u) 0u 65536u))) 1u))
        (cascade-publish-pages)
        (= cascade-resident (font-rasterized-glyphs specimen-font))
        (proof-expect (== (array-length cascade-resident) 1u))
        (proof-expect (== (cascade-resident 0u) resident))
        (demo-layout) (proof-expect (== demo-layout-status 0u))
        (proof-expect (all (== (.uv (specimen-quads 0u)) first.uv))))
      (let [builds cascade-page-builds first (specimen-quads 0u)]
        (demo-layout) (proof-expect (== cascade-page-builds builds))
        (proof-expect (all (== (.uv (specimen-quads 0u)) first.uv))))
      (proof-expect (text-editor-history demo-editor false)) (demo-layout)
      (proof-expect (== demo-layout-status 0u))
      (proof-expect (> (.z (.bounds (specimen-quads 0u))) 0.))
      (let [expected (make-text "Office, café, affinity.")]
        (proof-expect (text-equal demo-editor.current.text expected)) (text-release expected))
      (line-close)
    "#,
    );
}

#[test]
fn whole_document_pages_cover_final_glyphs_and_match_full_atlas_ink_geometry() {
    check_rich_scene(
        r#"
      (= specimen-font (open-font "../../easl-native-text/tests/fonts/amiri.ttf" 32.))
      (= specimen-metrics (font-metrics specimen-font)) (= specimen-ink (font-atlas-glyphs specimen-font))
      (line-enable-cascade)
      (= proof-amiri (load-font "../../easl-native-text/tests/fonts/amiri.ttf" 32.))
      (= proof-latin (load-font "../../easl-native-text/tests/fonts/shantell-sans-regular.ttf" 32.))
      (= proof-kawi (load-font "../tests/fonts/noto-sans-kawi.ttf" 32.))
      (= proof-amiri-ink (font-atlas-glyphs proof-amiri))
      (= proof-latin-ink (font-atlas-glyphs proof-latin))
      (= proof-kawi-ink (font-atlas-glyphs proof-kawi))
      (let [seed (make-text "Office Привет é\r\n𑼒𑽂𑼒𑼶 العربية\nZZZ café آخر")]
        (text-editor-open demo-editor seed) (text-release seed))
      (= demo-viewport (vec2f 1600. 900.)) (= flow-width 1200.)
      (proof-expect (== (contextual-layout) 0u))
      (proof-expect (== ctx-font-mask 7u)) (proof-expect (== ctx-missing-glyphs 0u))
      (proof-check-ink)
      (let [builds cascade-page-builds]
        (proof-expect (== builds 3u))
        (= flow-width 1400.) (proof-expect (== (contextual-layout) 0u))
        (proof-expect (== cascade-page-builds builds)) (proof-check-ink))
      (= flow-width 70.) (proof-expect (== (contextual-layout) 0u)) (proof-check-ink)
      (line-close)
    "#,
        r#"
      (var proof-amiri: (Texture2D f32)) (var proof-latin: (Texture2D f32))
      (var proof-kawi: (Texture2D f32))
      (var proof-amiri-ink: [FontAtlasGlyph]) (var proof-latin-ink: [FontAtlasGlyph])
      (var proof-kawi-ink: [FontAtlasGlyph]) (var proof-resident: [u32])
      (defn proof-check-ink []
        (for [font 3u]
          (match font
            0u (= proof-resident (font-rasterized-glyphs specimen-font))
            1u (= proof-resident (font-rasterized-glyphs cascade-second))
            _ (= proof-resident (font-rasterized-glyphs cascade-third)))
          (proof-expect (< (array-length proof-resident) 128u))
          (for [i specimen-count]
            (when (== (ctx-paint-fonts i) font)
              (let [item (ctx-positioned i) id item.glyph.id @var found false
                    full (match font 0u (proof-amiri-ink id) 1u (proof-latin-ink id) _ (proof-kawi-ink id))
                    expected (text-placed-glyph-quad item full (vec2f 1.))
                    q (specimen-quads i)]
                (for [j (array-length proof-resident)] (when (== (proof-resident j) id) (= found true)))
                (proof-expect found)
                (proof-expect (all (< (abs (- q.bounds expected.bounds)) (vec4f 0.0001))))
                (proof-expect (== q.start item.glyph.start)) (proof-expect (== q.end item.glyph.end))
                (proof-expect (all (>= q.uv (vec4f 0.)))) (proof-expect (all (<= q.uv (vec4f 1.)))))))))
    "#,
    );
}
