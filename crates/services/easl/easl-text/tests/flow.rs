use easl::{
    CompilerTarget,
    compiler::{builtins::built_in_macros, program::Program},
    interpreter::{
        CpuRuntime, IOEvent, StringIO, VmCpuRuntime,
        run_program_entry_with_io_and_runtime_from_path,
    },
    parse::{EaslMultiDocument, parse_easl_without_comments},
};
use std::path::Path;

fn check(body: &str) {
    check_runtime(body);
}

fn check_runtime(body: &str) -> (std::time::Duration, std::time::Duration) {
    let source = format!(
        r#"
      (var f-source: TextBuffer)
      (var f-facts: [TextGrapheme])
      (var f-glyphs: [FontShapedGlyph])
      (var f-spans: [TextFlowSpan])
      (var f-points: [TextFlowPoint])
      (var f-tabs: [TextFlowTab])
      (var f-tab-policy: TextTabs)
      (var f-breaks: [u32])
      (var f-lines: [TextFlowLine])
      (var f-index: [TextLineCells])
      (var f-line-reads: u32)
      (var f-quads: [TextGlyphQuad])
      (var f-clusters: [TextClusterBox])
      (var f-cells: [TextCell])
      (var f-assertion: u32)
      (var f-span-count: u32)
      (var f-cell-count: u32)
      (var f-line-count: u32)
      (var f-emergency: bool)
      (defn fact-at-flow [i: u32]: TextGrapheme (f-facts i))
      (defn glyph-at-flow [i: u32]: FontShapedGlyph (f-glyphs i))
      (defn span-at-flow [i: u32]: TextFlowSpan (f-spans i))
      (defn point-at-flow [i: u32]: TextBreak (.mark (f-points i)))
      (defn tab-at-flow [i: u32]: TextFlowTab (f-tabs i))
      (defn emit-tab-flow [i: u32 item: TextFlowTab] (= (f-tabs i) item))
      (defn measure-line-flow [start: u32 end: u32 budget: u32]: TextLineMeasure
        (text-flow-measure (f-points start) (f-points end) f-tab-policy
          (array-length f-tabs) tab-at-flow budget))
      (defn break-at-flow [i: u32]: u32 (f-breaks i))
      (defn index-at-flow [i: u32]: TextLineCells (+= f-line-reads 1u) (f-index i))
      (defn emit-index-flow [i: u32 item: TextLineCells] (= (f-index i) item))
      (defn cell-at-flow [i: u32]: TextCell (f-cells i))
      (defn cluster-at-flow [i: u32]: TextClusterBox (f-clusters i))
      (defn ink-at-flow [i: u32]: FontAtlasGlyph (FontAtlasGlyph (vec4u 0u 0u 8u 12u) (vec2f 0. -12.) 10.))
      (defn emit-span-flow [i: u32 item: TextFlowSpan] (= (f-spans i) item))
      (defn emit-point-flow [i: u32 item: TextFlowPoint] (= (f-points i) item))
      (defn emit-break-flow [i: u32 item: u32] (= (f-breaks i) item))
      (defn emit-line-flow [i: u32 item: TextFlowLine] (= (f-lines i) item))
      (defn emit-quad-flow [i: u32 item: TextGlyphQuad] (= (f-quads i) item))
      (defn emit-cluster-flow [i: u32 item: TextClusterBox] (= (f-clusters i) item))
      (defn emit-cell-flow [i: u32 item: TextCell] (= (f-cells i) item))
      (defn expect [value: bool] (+= f-assertion 1u) (when (not value) (print f-assertion)))
      (defn prepare-flow []
        (= f-facts (text-graphemes f-source))
        (= f-tab-policy (TextTabs 40. 0.))
        (let [n (array-length f-facts)]
          (= f-tabs (zeroed-array n)) (= f-glyphs (zeroed-array n)) (= f-spans (zeroed-array n))
          (= f-points (zeroed-array (+ n 1u))) (= f-breaks (zeroed-array (+ n 1u)))
          (= f-lines (zeroed-array (+ n 1u))) (= f-index (zeroed-array (+ n 1u))) (= f-quads (zeroed-array n))
          (= f-clusters (zeroed-array (+ n 1u))) (= f-cells (zeroed-array (+ n 1u)))
          (for [i n]
            (let [fact (f-facts i)]
              (= (f-glyphs i) (FontShapedGlyph 0u fact.start fact.end 0. 0. 10. 0u))))))
      (defn spans-flow []: TextFlowResult
        (text-flow-spans f-source (array-length f-facts) fact-at-flow
          (array-length f-glyphs) glyph-at-flow (array-length f-spans) emit-span-flow))
      (defn layout-flow [width: f32]: u32
        (let [spans (spans-flow)]
          (when (!= spans.status 0u) (return spans.status))
          (= f-span-count spans.count)
          (let [points (text-flow-points spans.count span-at-flow width f-emergency f-tab-policy (array-length f-points) emit-point-flow (array-length f-tabs) emit-tab-flow)]
            (when (!= points.status 0u) (return points.status))
            (let [result (if (== spans.count 0u) (TextComposition 0u 0u 0u)
                  (text-compose points.count point-at-flow measure-line-flow width (text-default-policy) emit-break-flow))]
            (when (!= result.status 0u) (return (+ 10u result.status)))
            (let [placed (text-flow-place spans.count span-at-flow result.count break-at-flow f-emergency f-tab-policy
                    glyph-at-flow ink-at-flow (vec2f 128.) (vec2f 5. 7.) 15. 20.
                    (array-length f-lines) emit-line-flow emit-quad-flow emit-cluster-flow)
                  cells (text-build-cells f-source (+ spans.count (if (== placed.count result.count) 0u 1u))
                    cluster-at-flow (array-length f-cells) emit-cell-flow)]
              (= f-line-count placed.count) (= f-cell-count cells.count)
              (expect (== placed.status 0u)) (expect (== cells.status 0u))))))
        0u)
      @cpu (defn main [] {body} (text-release f-source) (print "done"))
    "#
    );
    let parsed = parse_easl_without_comments(&source);
    assert!(
        parsed.parsing_failures.is_empty(),
        "{:?}",
        parsed.parsing_failures
    );
    let mut documents =
        EaslMultiDocument::from_singular_document(parsed, "flow-test.easl".into(), source);
    for name in ["atlas", "paragraph", "flow", "geometry", "viewport"] {
        let source = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("library/{name}.easl")),
        )
        .unwrap();
        let parsed = parse_easl_without_comments(&source);
        assert!(
            parsed.parsing_failures.is_empty(),
            "{name}: {:?}",
            parsed.parsing_failures
        );
        documents.add_document(parsed, format!("{name}.easl"), source);
    }
    let (mut program, errors) = Program::from_easl_documents(&documents, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(errors.is_empty(), "{errors:?}");
    let expected = vec![IOEvent::Print("done".into())];
    let tree_start = std::time::Instant::now();
    let io = run_program_entry_with_io_and_runtime_from_path(
        program.clone(),
        Some("main"),
        StringIO::new(),
        Path::new("flow-test.easl"),
        CpuRuntime::TreeWalking,
    )
    .unwrap()
    .0;
    assert_eq!(io.events, expected, "tree walking");
    let tree_elapsed = tree_start.elapsed();
    let mut vm = VmCpuRuntime::new_cpu_with_external(
        program,
        StringIO::new(),
        None::<std::path::PathBuf>,
        None,
    )
    .unwrap();
    let start = std::time::Instant::now();
    vm.run("main").unwrap();
    let elapsed = start.elapsed();
    assert_eq!(vm.env.io.events, expected, "bytecode");
    assert_eq!(vm.env.text_values.live_values(), 0);
    (tree_elapsed, elapsed)
}

#[test]
fn wrapping_preserves_spaces_source_ranges_and_dual_carets_at_line_edges() {
    check(
        r#"
      (= f-source (make-text "one two three")) (prepare-flow)
      (expect (== (layout-flow 50.) 0u)) (expect (== f-line-count 3u))
      (expect (== (.end (f-lines 0u)) 4u)) (expect (== (.end (f-lines 1u)) 8u))
      (expect (== (.z (.bounds (f-lines 0u))) 30.))
      (expect (== (.z (.bounds (f-cells 3u))) 0.))
      (let [upstream (text-caret-at f-cell-count cell-at-flow 4u 0u)
            downstream (text-caret-at f-cell-count cell-at-flow 4u 1u)]
        (expect (== upstream.line 0u)) (expect (== upstream.rect.x 35.))
        (expect (== downstream.line 1u)) (expect (== downstream.rect.x 5.)))
      (expect (== (.byte (text-hit-test f-cell-count cell-at-flow (vec2f 22. 52.))) 10u))
      (let [original (make-text "one two three")]
        (expect (text-equal original f-source)) (text-release original))
    "#,
    );
}

#[test]
fn crlf_blank_lines_and_trailing_separator_keep_exact_grapheme_offsets() {
    check(
        &r#"
      (= f-source (make-text "a<CRLF><CRLF>b<LS>")) (prepare-flow)
      (expect (== (array-length f-facts) 5u))
      (expect (== (.start (f-facts 1u)) 1u)) (expect (== (.end (f-facts 1u)) 3u))
      (expect (== (layout-flow 80.) 0u)) (expect (== f-line-count 4u))
      (expect (== f-cell-count 6u))
      (expect (== (.start (f-lines 1u)) 3u)) (expect (== (.end (f-lines 1u)) 5u))
      (expect (== (.z (.bounds (f-lines 1u))) 0.))
      (expect (== (.start (f-lines 3u)) 9u)) (expect (== (.end (f-lines 3u)) 9u))
      (expect (== (.byte (text-hit-test f-cell-count cell-at-flow (vec2f 40. 71.))) 9u))
    "#
        .replace("<CRLF>", "\r\n")
        .replace("<LS>", "\u{2028}"),
    );
}

#[test]
fn empty_text_has_one_editable_line() {
    check(
        r#"
      (= f-source (make-text "")) (prepare-flow)
      (expect (== (layout-flow 20.) 0u)) (expect (== f-line-count 1u))
      (expect (== f-cell-count 1u))
      (expect (== (.byte (text-hit-test f-cell-count cell-at-flow (vec2f 100.))) 0u))
      (expect (== (.status (text-caret-at f-cell-count cell-at-flow 0u 1u)) 0u))
    "#,
    );
}

#[test]
fn unicode_opportunities_do_not_split_no_break_spaces_or_extended_graphemes() {
    check(
        r#"
      (= f-source (make-text "é🌍 A B 中文")) (prepare-flow)
      (expect (== (.end (f-facts 0u)) 3u)) (expect (== (.end (f-facts 1u)) 7u))
      (expect (== (.break-after (f-facts 3u)) 0u))
      (expect (== (.break-after (f-facts 4u)) 0u))
      (expect (== (.break-after (f-facts 7u)) 1u))
      (expect (== (layout-flow 30.) 0u)) (expect (== f-line-count 3u))
      (expect (== (.end (f-lines 0u)) 8u)) (expect (== (.end (f-lines 1u)) 13u))
    "#,
    );
}

#[test]
fn invalid_spans_and_capacity_failure_do_not_publish_partial_geometry() {
    check(
        r#"
      (= f-source (make-text "abc")) (prepare-flow)
      (= (f-spans 0u) (TextFlowSpan 99u 99u 0u 0u 0. 0u false false))
      (= (.end (f-glyphs 2u)) 4u)
      (expect (== (.status (spans-flow)) 1u)) (expect (== (.start (f-spans 0u)) 99u))
      (= (.end (f-glyphs 2u)) 3u)
      (expect (== (.status (text-flow-spans f-source 3u fact-at-flow 3u glyph-at-flow 2u emit-span-flow)) 2u))
      (expect (== (.start (f-spans 0u)) 99u))
      (expect (== (layout-flow 20.) 12u))
      (expect (== (.start (f-lines 0u)) 0u))
    "#,
    );
}

#[test]
fn real_font_multiline_reflow_keeps_source_and_pointer_selection_in_both_cpus() {
    use easl::{
        external::ExternalVars,
        interpreter::run_program_entry_with_io_runtime_and_external_from_path,
        parse::load_and_parse_easl_multidocument,
    };
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/flow_specimen.easl");
    let documents = load_and_parse_easl_multidocument(&path)
        .unwrap()
        .unwrap()
        .unwrap();
    let (mut program, errors) = Program::from_easl_documents(&documents, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(errors.is_empty(), "{errors:?}");
    for runtime in [CpuRuntime::TreeWalking, CpuRuntime::BytecodeVm] {
        let external = ExternalVars::new(&program);
        run_program_entry_with_io_runtime_and_external_from_path(
            program.clone(),
            Some("flow-scroll-prepare"),
            StringIO::new(),
            &path,
            runtime,
            Some(external.clone()),
        )
        .unwrap();
        assert_eq!(external.read_external_var_raw("flow-status").unwrap(), [0]);
        assert_eq!(
            external.read_external_var_raw("demo-proof-ok").unwrap(),
            [1]
        );
        assert!(external.read_external_var_raw("flow-line-count").unwrap()[0] >= 6);
        assert!(external.read_external_var_raw("demo-cell-count").unwrap()[0] > 200);
    }
}

#[test]
fn indexed_viewport_queries_and_scroll_reveal_use_content_geometry() {
    check(
        r#"
      (= f-source (make-text "a b c d e f g h i j")) (prepare-flow)
      (expect (== (layout-flow 10.) 0u)) (expect (== f-line-count 10u))
      (let [index (text-index-lines f-cell-count cell-at-flow (array-length f-index) emit-index-flow)]
        (expect (== index.status 0u)) (expect (== index.count 10u)))
      (= f-line-reads 0u)
      (let [visible (text-visible-lines 10u index-at-flow 48. 38.)]
        (expect (== visible.x 2u)) (expect (== visible.y 4u))
        (expect (<= f-line-reads 8u)))
      (= f-line-reads 0u)
      (let [hit (text-indexed-hit 10u index-at-flow cell-at-flow (vec2f 12. 189.))]
        (expect (== hit.line 9u)) (expect (== hit.byte 19u)) (expect (<= f-line-reads 6u)))
      (expect (== (.line (text-indexed-hit 10u index-at-flow cell-at-flow (vec2f 5. -30.))) 0u))
      (let [reveal (text-scroll-reveal (vec2f 0.) (vec2f 100.) (vec2f 100. 207.)
                      (vec4f 5. 187. 1. 20.) (vec2f 5.))]
        (expect (== reveal.status 0u)) (expect (== reveal.offset.y 107.))
        (expect (== reveal.offset.x 0.)))
      (expect (== (.status (text-scroll-reveal (vec2f 8.) (vec2f -1.)
        (vec2f 100.) (vec4f 0.) (vec2f 0.))) 1u))
      (= (.first (f-index 0u)) 99u) (= (.line (f-cells (- f-cell-count 1u))) 0u)
      (expect (== (.status (text-index-lines f-cell-count cell-at-flow (array-length f-index) emit-index-flow)) 1u))
      (expect (== (.first (f-index 0u)) 99u))
    "#,
    );
}

#[test]
fn flow_keeps_ligatures_whole_and_preflights_illegal_break_lists() {
    check(
        r#"
      (= f-source (make-text "office word")) (prepare-flow)
      (= f-glyphs (zeroed-array 9u))
      (= (f-glyphs 0u) (FontShapedGlyph 0u 0u 1u 0. 0. 10. 0u))
      (= (f-glyphs 1u) (FontShapedGlyph 0u 1u 4u 0. 0. 30. 0u))
      (for [i 2u (< i 9u) (+= i 1u)]
        (= (f-glyphs i) (FontShapedGlyph 0u (+ i 2u) (+ i 3u) 0. 0. 10. 0u)))
      (expect (== (layout-flow 60.) 0u)) (expect (== f-line-count 2u))
      (expect (== f-span-count 9u)) (expect (== f-cell-count 11u))
      (expect (== (.end (f-lines 0u)) 7u))
      (expect (== (.x (.rect (text-caret-at f-cell-count cell-at-flow 2u 1u))) 25.))
      (= (.start (f-lines 0u)) 99u) (= (f-breaks 0u) 1u)
      (expect (== (.status (text-flow-place f-span-count span-at-flow 2u break-at-flow false f-tab-policy
        glyph-at-flow ink-at-flow (vec2f 128.) (vec2f 5. 7.) 15. 20.
        (array-length f-lines) emit-line-flow emit-quad-flow emit-cluster-flow)) 1u))
      (expect (== (.start (f-lines 0u)) 99u))
      (expect (== (.status (text-flow-points 4294967295u span-at-flow 20. false f-tab-policy 20u emit-point-flow (array-length f-tabs) emit-tab-flow)) 2u))
    "#,
    );
}

#[test]
fn discretionary_hyphens_are_explicitly_unsupported() {
    check(
        &r#"
          (= f-source (make-text "a<SHY>b")) (prepare-flow)
          (= (f-spans 0u) (TextFlowSpan 99u 99u 0u 0u 0. 0u false false))
          (expect (== (.status (spans-flow)) 4u))
          (expect (== (.start (f-spans 0u)) 99u))
        "#
        .replace("<SHY>", "\u{ad}"),
    );
}

#[test]
fn emergency_wrapping_preserves_source_clusters_and_fitting_paragraphs() {
    check(
        r#"
      (= f-source (make-text "one two three")) (prepare-flow)
      (expect (== (layout-flow 50.) 0u))
      (let [first (.end (f-lines 0u)) second (.end (f-lines 1u))]
        (= f-emergency true)
        (expect (== (layout-flow 50.) 0u))
        (expect (== f-line-count 3u))
        (expect (== (.end (f-lines 0u)) first))
        (expect (== (.end (f-lines 1u)) second)))
      (text-release f-source) (= f-source (make-text "abcde")) (prepare-flow)
      (expect (== (layout-flow 20.) 0u)) (expect (== f-line-count 3u))
      (expect (== (.end (f-lines 0u)) 2u)) (expect (== (.end (f-lines 1u)) 4u))
      (expect (== (.end (f-lines 2u)) 5u))
      (let [expected (make-text "abcde")]
        (expect (text-equal expected f-source)) (text-release expected))
      (text-release f-source) (= f-source (make-text "é🌍A B")) (prepare-flow)
      (expect (== (layout-flow 1.) 0u)) (expect (== f-line-count 5u))
      (expect (== (.end (f-lines 0u)) 3u)) (expect (== (.end (f-lines 1u)) 7u))
      (expect (== (.z (.bounds (f-lines 0u))) 10.))
      (expect (== f-cell-count 5u))
    "#,
    );
}

#[test]
fn emergency_wrapping_keeps_oversized_ligatures_and_trailing_separators_together() {
    check(
        &r#"
      (= f-emergency true)
      (= f-source (make-text "ffi <CRLF>ab")) (prepare-flow)
      (= f-glyphs (zeroed-array 5u))
      (= (f-glyphs 0u) (FontShapedGlyph 0u 0u 3u 0. 0. 30. 0u))
      (= (f-glyphs 1u) (FontShapedGlyph 0u 3u 4u 0. 0. 10. 0u))
      (= (f-glyphs 2u) (FontShapedGlyph 0u 4u 6u 0. 0. 0. 0u))
      (= (f-glyphs 3u) (FontShapedGlyph 0u 6u 7u 0. 0. 10. 0u))
      (= (f-glyphs 4u) (FontShapedGlyph 0u 7u 8u 0. 0. 10. 0u))
      (expect (== (layout-flow 10.) 0u)) (expect (== f-line-count 3u))
      (expect (== (.start (f-lines 0u)) 0u)) (expect (== (.end (f-lines 0u)) 6u))
      (expect (== (.z (.bounds (f-lines 0u))) 30.))
      (expect (== (.start (f-lines 1u)) 6u)) (expect (== (.end (f-lines 2u)) 8u))
      (expect (== (.x (.rect (text-caret-at f-cell-count cell-at-flow 1u 1u))) 15.))
      ; Even emergency placement may not cross a real hard separator.
      (= (.start (f-lines 0u)) 99u) (= (f-breaks 0u) f-span-count)
      (expect (== (.status (text-flow-place f-span-count span-at-flow 1u break-at-flow true f-tab-policy
        glyph-at-flow ink-at-flow (vec2f 128.) (vec2f 5. 7.) 15. 20.
        (array-length f-lines) emit-line-flow emit-quad-flow emit-cluster-flow)) 1u))
      (expect (== (.start (f-lines 0u)) 99u))
    "#
        .replace("<CRLF>", "\r\n"),
    );
}

#[test]
fn emergency_points_preflight_capacity_and_invalid_input_without_partial_output() {
    check(
        r#"
      (= f-source (make-text "abcdef")) (prepare-flow)
      (expect (== (.status (spans-flow)) 0u))
      (= (.mark (f-points 0u)) (TextBreak 99u (vec2f 0.) (vec2f 0.) 0. 0u 4294967295u))
      (expect (== (.status (text-flow-points 6u span-at-flow 10. true f-tab-policy 3u emit-point-flow (array-length f-tabs) emit-tab-flow)) 2u))
      (expect (== (.cluster (.mark (f-points 0u))) 99u))
      (= (.advance (f-spans 5u)) -1.)
      (expect (== (.status (text-flow-points 6u span-at-flow 10. true f-tab-policy 7u emit-point-flow (array-length f-tabs) emit-tab-flow)) 1u))
      (expect (== (.cluster (.mark (f-points 0u))) 99u))
      (= (.advance (f-spans 5u)) 10.)
      (expect (== (.status (text-flow-points 6u span-at-flow 0. true f-tab-policy 7u emit-point-flow (array-length f-tabs) emit-tab-flow)) 1u))
      (expect (== (.cluster (.mark (f-points 0u))) 99u))
      (let [points (text-flow-points 6u span-at-flow 10. true f-tab-policy 7u emit-point-flow (array-length f-tabs) emit-tab-flow)]
        (expect (== points.status 0u)) (expect (== points.count 7u))
        (= (f-breaks 0u) 99u)
        (let [policy (TextPolicy 0.05 0.2 0. 0u)]
          (expect (== (.status (text-compose points.count point-at-flow measure-line-flow 10. policy emit-break-flow)) 3u)))
        (expect (== (f-breaks 0u) 99u))
        (= (.overflow-start (.mark (f-points 1u))) (.cluster (.mark (f-points 1u))))
        (expect (== (.status (text-compose points.count point-at-flow measure-line-flow 10. (text-default-policy) emit-break-flow)) 1u))
        (expect (== (f-breaks 0u) 99u)))
    "#,
    );
}

#[test]
fn emergency_wrap_work_depends_on_spans_and_capacity_on_actual_lines() {
    check(
        r#"
      (= f-source (make-text ""))
      (= f-points (zeroed-array 501u)) (= f-breaks (zeroed-array 500u))
      ; The reader supplies 5,000 already validated, equally wide shaped clusters.
      ; Only 500 emergency lines are needed, despite 5,000 possible boundaries.
      (let [span-at (fn [i] (TextFlowSpan i (+ i 1u) i (+ i 1u) 10. 0u false false))
            points (text-flow-points 5000u span-at 100. true f-tab-policy 501u emit-point-flow (array-length f-tabs) emit-tab-flow)]
        (expect (== points.status 0u)) (expect (== points.count 501u))
        (let [result (text-compose points.count point-at-flow measure-line-flow 100. (text-default-policy) emit-break-flow)]
          (expect (== result.status 0u)) (expect (== result.count 500u))
          (for [i result.count] (expect (== (f-breaks i) (* (+ i 1u) 10u)))))
        (= (.cluster (.mark (f-points 0u))) 99u)
        (expect (== (.status (text-flow-points 5000u span-at 1. true f-tab-policy 501u emit-point-flow (array-length f-tabs) emit-tab-flow)) 2u))
        (expect (== (.cluster (.mark (f-points 0u))) 99u)))
    "#,
    );
}

#[test]
fn large_emergency_flow_keeps_exact_source_and_geometry_in_both_evaluators() {
    // This previously spent minutes copying entire arrays in the tree evaluator.
    // Both runtimes must exercise the same complete geometry and source checks.
    let source = "x".repeat(5000);
    let (tree_elapsed, vm_elapsed) = check_runtime(&format!(
        r#"
          (= f-emergency true)
          (= f-source (make-text "{source}")) (prepare-flow)
          (expect (== (layout-flow 100.) 0u)) (expect (== f-line-count 500u))
          (for [i f-line-count]
            (expect (== (.start (f-lines i)) (* i 10u)))
            (expect (== (.end (f-lines i)) (* (+ i 1u) 10u))))
          (expect (== f-cell-count 5000u))
          (expect (== (.end (f-lines 499u)) (text-length f-source)))
        "#
    ));
    eprintln!(
        "5,000 synthetic shaped clusters: tree initialize/layout/checks={tree_elapsed:?}; bytecode layout/checks={vm_elapsed:?}"
    );
}

#[test]
fn tabs_share_line_relative_stops_with_cells_hits_and_selection() {
    check(
        &r#"
      (= f-source (make-text "a<TAB>b<NL>i<TAB>b<NL><TAB>Office")) (prepare-flow)
      (expect (== (layout-flow 200.) 0u)) (expect (== f-line-count 3u))
      (expect (== (.x (.rect (text-caret-at f-cell-count cell-at-flow 2u 1u))) 45.))
      (expect (== (.x (.rect (text-caret-at f-cell-count cell-at-flow 6u 1u))) 45.))
      (expect (== (.x (.rect (text-caret-at f-cell-count cell-at-flow 9u 1u))) 45.))
      (expect (== (.z (.bounds (f-cells 1u))) 30.))
      (expect (== (.z (.bounds (f-cells 8u))) 40.))
      (expect (== (.byte (text-hit-test f-cell-count cell-at-flow (vec2f 18. 12.))) 1u))
      (expect (== (.byte (text-hit-test f-cell-count cell-at-flow (vec2f 40. 12.))) 2u))
      (expect (== (.quad-end (f-lines 0u)) 2u))
      (= f-emergency true)
      (expect (== (layout-flow 1.) 0u))
      (expect (== (.end (f-lines (- f-line-count 1u))) 15u))
      (expect (== f-cell-count 15u))
      (expect (== (layout-flow 200.) 0u)) (expect (== f-line-count 3u))
    "#
        .replace("<TAB>", "\t")
        .replace("<NL>", "\n"),
    );
}

#[test]
fn leading_repeated_and_trailing_tabs_preserve_width_without_control_glyph_ink() {
    check(
        &r#"
      (= f-source (make-text "<TAB><TAB> <NL><TAB>")) (prepare-flow)
      (expect (== (layout-flow 120.) 0u)) (expect (== f-line-count 2u))
      (expect (== (.z (.bounds (f-lines 0u))) 80.))
      (expect (== (.z (.bounds (f-lines 1u))) 40.))
      (expect (== (.quad-end (f-lines 1u)) 0u))
      (expect (== (.z (.bounds (f-cells 0u))) 40.))
      (expect (== (.z (.bounds (f-cells 1u))) 40.))
      (expect (== (.z (.bounds (f-cells 2u))) 0.))
      ; Font shaping may omit tabs completely. Consume no invented glyph.
      (= f-glyphs (zeroed-array 2u))
      (= (f-glyphs 0u) (FontShapedGlyph 0u 2u 3u 0. 0. 10. 0u))
      (= (f-glyphs 1u) (FontShapedGlyph 0u 3u 4u 0. 0. 0. 0u))
      (expect (== (layout-flow 120.) 0u))
      (expect (== (.z (.bounds (f-lines 0u))) 80.))
      (= f-tab-policy (TextTabs 0. 0.))
      (expect (== (layout-flow 1.) 0u))
      (expect (== (.z (.bounds (f-lines 0u))) 0.))
      (expect (== (.z (.bounds (f-lines 1u))) 0.))
    "#
        .replace("<TAB>", "\t")
        .replace("<NL>", "\n"),
    );
}

#[test]
fn fractional_stops_minimum_advance_and_invalid_settings_are_explicit() {
    check(
        &r#"
      (= f-source (make-text "ab<TAB>c")) (prepare-flow)
      (= (.advance (f-glyphs 0u)) 14.75)
      (= (.advance (f-glyphs 1u)) 14.75)
      (= f-tab-policy (TextTabs 30.25 1.))
      (expect (== (text-tab-after 29.25 f-tab-policy) 30.25))
      (expect (== (text-tab-after 29.5 f-tab-policy) 60.5))
      (expect (== (text-tab-after 30.25 f-tab-policy) 60.5))
      (expect (== (layout-flow 100.) 0u))
      (expect (== (.z (.bounds (f-lines 0u))) 70.5))
      (expect (== (.x (.rect (text-caret-at f-cell-count cell-at-flow 3u 1u))) 65.5))
      (expect (== (text-tab-after 100000000. (TextTabs 0.00000001 0.)) -1.))
      (= (.cluster (.mark (f-points 0u))) 99u)
      (let [settings [(TextTabs -1. 0.) (TextTabs 0. 1.) (TextTabs 4. 5.) (TextTabs 10000001. 0.)]]
       (for [i 4u]
        (expect (== (.status (text-flow-points 4u span-at-flow 100. false (settings i)
          (array-length f-points) emit-point-flow (array-length f-tabs) emit-tab-flow)) 1u))
        (expect (== (.cluster (.mark (f-points 0u))) 99u))))
    "#
        .replace("<TAB>", "\t"),
    );
}

#[test]
fn tab_tables_and_measurement_work_preflight_without_publishing_partial_output() {
    check(
        &r#"
      (= f-source (make-text "a<TAB>b<TAB>c")) (prepare-flow)
      (expect (== (.status (spans-flow)) 0u))
      (= (.cluster (.mark (f-points 0u))) 99u)
      (= (.advance (f-tabs 0u)) (vec2f 99. 0.))
      (expect (== (.status (text-flow-points 5u span-at-flow 100. false f-tab-policy
        (array-length f-points) emit-point-flow 1u emit-tab-flow)) 2u))
      (expect (== (.cluster (.mark (f-points 0u))) 99u))
      (expect (== (.x (.advance (f-tabs 0u))) 99.))
      (let [points (text-flow-points 5u span-at-flow 100. false f-tab-policy
        (array-length f-points) emit-point-flow (array-length f-tabs) emit-tab-flow)]
        (expect (== points.status 0u))
        (= (f-breaks 0u) 99u)
        (let [result (text-compose points.count point-at-flow measure-line-flow
                100. (TextPolicy 0.05 0.2 0. 1u) emit-break-flow)]
          (expect (== result.status 3u)) (expect (== (f-breaks 0u) 99u)))
        (let [measured (measure-line-flow 0u (- points.count 1u) 2u)]
          (expect (== measured.status 0u)) (expect (== measured.advance 90.))
          (expect (== measured.work 2u)))
        (expect (== (.status (measure-line-flow 0u (- points.count 1u) 1u)) 3u))
        (= (.advance (f-tabs 1u)) (vec2f -1. 0.))
        (expect (== (.status (measure-line-flow 0u (- points.count 1u) 2u)) 1u)))
      ; A shaped cluster cannot swallow the tab or cross it from either side.
      (= f-glyphs (zeroed-array 1u))
      (= (f-glyphs 0u) (FontShapedGlyph 0u 0u 5u 0. 0. 50. 0u))
      (= (.start (f-spans 0u)) 99u)
      (expect (== (.status (spans-flow)) 1u))
      (expect (== (.start (f-spans 0u)) 99u))
    "#
        .replace("<TAB>", "\t"),
    );
}

#[test]
fn tab_composition_matches_exhaustive_partition_costs_and_placed_widths() {
    // Independent exhaustive partitions: character widths are integral fixture
    // units. Every separator is a retained tab; the final word has no tab.
    // Score actual placement, rather than duplicating the prefix-table DP.
    let mut body = String::from("(= f-source (make-text \"\"))\n");
    for lengths in [[1_usize, 3, 2, 1], [4, 1, 3, 2], [2, 2, 2, 3]] {
        for width in [40_u32, 80, 100, 140] {
            for minimum in [0_u32, 5] {
                let source = lengths.map(|n| "x".repeat(n)).join("\t");
                let mut best = f64::INFINITY;
                for mask in 0..8_u32 {
                    let mut x = 0_u32;
                    let mut cost = 0.;
                    let mut fits = true;
                    for (i, length) in lengths.iter().enumerate() {
                        x += *length as u32 * 10;
                        if i != 3 {
                            let next = (x / 40 + 1) * 40;
                            x = if next - x < minimum { next + 40 } else { next };
                        }
                        if i == 3 || mask & (1 << i) != 0 {
                            fits &= x <= width;
                            if i != 3 {
                                cost += (1. - f64::from(x) / f64::from(width)).powi(2);
                            }
                            x = 0;
                        }
                    }
                    if fits {
                        best = best.min(cost);
                    }
                }
                body += &format!(
                    "(text-release f-source) (= f-source (make-text \"{source}\")) (prepare-flow)\n\
                     (= f-tab-policy (TextTabs 40. {minimum}.))\n"
                );
                if best.is_finite() {
                    body += &format!(
                        "(expect (== (layout-flow {width}.) 0u))\n\
                         (let [@var cost 0.]\n\
                           (for [i f-line-count]\n\
                             (let [occupied (.z (.bounds (f-lines i))) slack (/ (- {width}. occupied) {width}.)]\n\
                               (expect (<= occupied {width}.))\n\
                               (when (< (+ i 1u) f-line-count) (+= cost (* slack slack)))))\n\
                           (expect (< (abs (- cost {best:.9})) 0.0001)))\n"
                    );
                } else {
                    body += &format!("(expect (== (layout-flow {width}.) 12u))\n");
                }
            }
        }
    }
    check(&body);
}

#[test]
fn tab_thresholds_use_compensated_advances_in_composition_and_placement() {
    check(
        &r#"
      (= f-source (make-text "xxxxxxxxxx<TAB>x")) (prepare-flow)
      (for [i (array-length f-glyphs)] (= (.advance (f-glyphs i)) 0.1))
      (= f-tab-policy (TextTabs 1.000000119 0.))
      (expect (== (layout-flow 1.2) 0u)) (expect (== f-line-count 1u))
      (expect (< (abs (- (.z (.bounds (f-lines 0u))) 1.100000143)) 0.000001))
      (expect (< (abs (- (.x (.rect (text-caret-at f-cell-count cell-at-flow 11u 1u))) 6.000000119)) 0.000001))
    "#
        .replace("<TAB>", "\t"),
    );
}

#[test]
fn tabs_keep_adjacent_ligatures_combining_graphemes_and_crlf_ranges_whole() {
    check(
        &r#"
      (= f-source (make-text "ffi<TAB>é́<CRLF>")) (prepare-flow)
      (= f-glyphs (zeroed-array 4u))
      (= (f-glyphs 0u) (FontShapedGlyph 0u 0u 3u 0. 0. 30. 0u))
      (= (f-glyphs 1u) (FontShapedGlyph 0u 3u 4u 0. 0. 10. 0u))
      (= (f-glyphs 2u) (FontShapedGlyph 0u 4u 8u 0. 0. 10. 0u))
      (= (f-glyphs 3u) (FontShapedGlyph 0u 8u 10u 0. 0. 0. 0u))
      (expect (== (layout-flow 60.) 0u)) (expect (== f-line-count 2u))
      (expect (== f-cell-count 7u)) (expect (== f-span-count 4u))
      (expect (== (.quad-end (f-lines 0u)) 2u))
      (expect (== (.z (.bounds (f-lines 0u))) 50.))
      (expect (== (.x (.rect (text-caret-at f-cell-count cell-at-flow 1u 1u))) 15.))
      (expect (== (.x (.rect (text-caret-at f-cell-count cell-at-flow 4u 1u))) 45.))
      (expect (== (.start (f-lines 1u)) 10u))
      (= f-emergency true)
      (expect (== (layout-flow 1.) 0u))
      (expect (== (.end (f-lines 0u)) 3u))
      (expect (== (.end (f-lines 1u)) 4u))
      (expect (== (.end (f-lines 2u)) 10u))
      (expect (== (.start (f-lines 3u)) 10u))
    "#
        .replace("<TAB>", "\t")
        .replace("<CRLF>", "\r\n"),
    );
}

#[test]
fn unrepresentable_tab_stops_reject_before_any_geometry_is_emitted() {
    check(
        &r#"
      (= f-source (make-text "a<TAB>")) (prepare-flow)
      (= (.advance (f-glyphs 0u)) 10000000.)
      (expect (== (.status (spans-flow)) 0u))
      (= f-tab-policy (TextTabs 0.00000001 0.))
      (= (.cluster (.mark (f-points 0u))) 99u)
      (expect (== (.status (text-flow-points 2u span-at-flow 10000000. false f-tab-policy
        (array-length f-points) emit-point-flow (array-length f-tabs) emit-tab-flow)) 1u))
      (expect (== (.cluster (.mark (f-points 0u))) 99u))
      (= (f-breaks 0u) 2u)
      (= (.start (f-lines 0u)) 99u) (= (.start (f-quads 0u)) 99u)
      (expect (== (.status (text-flow-place 2u span-at-flow 1u break-at-flow false f-tab-policy
        glyph-at-flow ink-at-flow (vec2f 128.) (vec2f 5. 7.) 15. 20.
        (array-length f-lines) emit-line-flow emit-quad-flow emit-cluster-flow)) 1u))
      (expect (== (.start (f-lines 0u)) 99u)) (expect (== (.start (f-quads 0u)) 99u))
    "#
        .replace("<TAB>", "\t"),
    );
}
