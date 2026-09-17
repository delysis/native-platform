use easl::{
    CompilerTarget,
    compiler::{builtins::built_in_macros, program::Program},
    interpreter::{
        CpuRuntime, IOEvent, StringIO, VmCpuRuntime,
        run_program_entry_with_io_and_runtime_from_path,
    },
    parse::{EaslMultiDocument, parse_easl_without_comments},
};

#[test]
fn pointer_affinity_retains_the_clicked_cluster_at_a_shared_edge() {
    check(
        r#"
      (= (g-cells 0u) (TextCell (vec4f 0. 0. 10. 20.) 0u 1u 0u false))
      (= (g-cells 1u) (TextCell (vec4f 10. 0. 10. 20.) 1u 2u 0u false))
      (let [left (text-hit-test 2u cell-at-test (vec2f 9. 5.))
            right (text-hit-test 2u cell-at-test (vec2f 11. 5.))]
        (expect (== left.byte 1u)) (expect (== left.affinity 0u))
        (expect (== right.byte 1u)) (expect (== right.affinity 1u)))
      (= (g-cells 0u) (TextCell (vec4f 0. 0. 10. 20.) 2u 4u 0u true))
      (= (g-cells 1u) (TextCell (vec4f 10. 0. 10. 20.) 0u 2u 0u true))
      (let [left (text-hit-test 2u cell-at-test (vec2f 9. 5.))
            right (text-hit-test 2u cell-at-test (vec2f 11. 5.))]
        (expect (== left.byte 2u)) (expect (== left.affinity 1u))
        (expect (== right.byte 2u)) (expect (== right.affinity 0u)))
    "#,
    );
}

#[test]
fn line_navigation_policy_preserves_columns_and_resolves_page_gaps_in_both_evaluators() {
    check(
        r#"
      (let [@var c (TextNavigationContext 2u 5u 9. 40. 20. 60. 100. 16. 1u)
            down (text-navigation-plan c 1u)
            page (text-navigation-plan c 3u)
            gap (text-navigation-resolve c 3u page 2u)
            found (text-navigation-resolve c 3u page 4u)]
        (expect (== down.status 0u)) (expect (== down.line 3u))
        (expect (== down.x 16.)) (expect (== down.preserve-x 1u))
        (expect (== page.kind 1u)) (expect (== page.y 90.))
        (expect (== gap.line 3u)) (expect (== found.line 4u))
        (expect (== (.status (text-navigation-resolve c 3u page 5u)) 1u))
        (= c.line 0u) (= c.top 0.)
        (let [top (text-navigation-plan c 0u)]
          (expect (== top.line 0u)) (expect (== top.edge 1u))
          (expect (== top.preserve-x 0u)))
        (= c.line 4u) (= c.top 80.)
        (let [bottom (text-navigation-plan c 3u)]
          (expect (== bottom.line 4u)) (expect (== bottom.edge 2u))
          (expect (== bottom.preserve-x 0u)))
        (let [line-end (text-navigation-plan c 5u)
              hard (text-navigation-edge line-end (TextNavigationLine 12u 24u 22u))
              soft (text-navigation-edge line-end (TextNavigationLine 12u 24u 24u))
              end (text-navigation-edge (text-navigation-plan c 7u) (TextNavigationLine 12u 24u 22u))]
          (expect (== hard.byte 22u)) (expect (== hard.affinity 1u))
          (expect (== soft.byte 24u)) (expect (== soft.affinity 0u))
          (expect (== end.byte 24u)) (expect (== end.affinity 0u))
          (expect (== (.status (text-navigation-edge line-end (TextNavigationLine 12u 24u 10u))) 1u)))
        (expect (== (.status (text-navigation-plan c 8u)) 1u))
        (= c.has-preferred 2u)
        (expect (== (.status (text-navigation-plan c 0u)) 1u))
        (= c.has-preferred 0u) (= c.lines 0u)
        (expect (== (.status (text-navigation-plan c 0u)) 1u)))
    "#,
    );
}

fn program(body: &str) -> Program {
    let source = format!(
        r#"
      (var g-text: TextBuffer)
      (var g-clusters: [16: TextClusterBox])
      (var g-cells: [128: TextCell])
      (var g-rects: [128: vec4f])
      (var g-count: u32)
      (var g-assertion: u32)
      (var g-editor: TextEditorState)
      (var g-view: TextViewState)
      (var g-geometry: TextBuffer)
      (var g-widget: TextWidgetState)
      (var g-layout: TextWidgetLayout)
      (var g-line-index: [16: TextLineCells])
      (defn index-at-test [i: u32]: TextLineCells (g-line-index i))
      (defn emit-index-test [i: u32 item: TextLineCells] (= (g-line-index i) item))
      (defn widget-key-test [key: u32 modifiers: u32]: u32
        (text-widget-input g-editor g-widget g-layout index-at-test cell-at-test
          (TextInputEvent 1u key modifiers 0u 0u 0u 0u) (TextBuffer 0u) 8u 4u))
      (defn cluster-at-test [i: u32]: TextClusterBox (g-clusters i))
      (defn cell-at-test [i: u32]: TextCell (g-cells i))
      (defn emit-cell-test [i: u32 item: TextCell] (= (g-cells i) item))
      (defn emit-rect-test [i: u32 item: vec4f] (= (g-rects i) item))
      (defn view-key-test [key: u32 modifiers: u32]: u32
        (text-view-input g-editor g-view g-geometry g-count cell-at-test
          (TextInputEvent 1u key modifiers 0u 0u 0u 0u) (TextBuffer 0u) 8u 4u))
      (defn expect [value: bool] (+= g-assertion 1u) (when (not value) (print g-assertion)))
      (defn prepare-test [n: u32]
        (let [result (text-build-cells g-text n cluster-at-test 128u emit-cell-test)]
          (expect (== result.status 0u)) (= g-count result.count)))
      @cpu (defn main [] {body} (text-editor-close g-editor) (text-release g-text) (print "done"))
    "#
    );
    let parsed = parse_easl_without_comments(&source);
    assert!(
        parsed.parsing_failures.is_empty(),
        "{:?}",
        parsed.parsing_failures
    );
    let mut docs =
        EaslMultiDocument::from_singular_document(parsed, "geometry-test.easl".into(), source);
    for (name, source) in [
        ("atlas.easl", include_str!("../library/atlas.easl")),
        ("geometry.easl", include_str!("../library/geometry.easl")),
        ("view.easl", include_str!("../library/view.easl")),
        ("viewport.easl", include_str!("../library/viewport.easl")),
        ("widget.easl", include_str!("../library/widget.easl")),
        ("editor.easl", include_str!("../library/editor.easl")),
        ("input.easl", include_str!("../library/input.easl")),
        ("editing.easl", include_str!("../library/editing.easl")),
        (
            "navigation.easl",
            include_str!("../library/navigation.easl"),
        ),
    ] {
        let parsed = parse_easl_without_comments(source);
        assert!(
            parsed.parsing_failures.is_empty(),
            "{name}: {:?}",
            parsed.parsing_failures
        );
        docs.add_document(parsed, name.into(), source.into());
    }
    let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(errors.is_empty(), "{errors:?}");
    program
}

fn check(body: &str) {
    let program = program(body);
    let expected = vec![IOEvent::Print("done".into())];
    let io = run_program_entry_with_io_and_runtime_from_path(
        program.clone(),
        Some("main"),
        StringIO::new(),
        std::path::Path::new("geometry-test.easl"),
        CpuRuntime::TreeWalking,
    )
    .unwrap()
    .0;
    assert_eq!(io.events, expected);
    let mut vm = VmCpuRuntime::new_cpu_with_external(
        program,
        StringIO::new(),
        None::<std::path::PathBuf>,
        None,
    )
    .unwrap();
    vm.run("main").unwrap();
    assert_eq!(vm.env.io.events, expected);
    assert_eq!(vm.env.text_values.live_values(), 0);
}

#[test]
fn ligature_cells_hit_testing_and_selection_preserve_extended_graphemes() {
    check(
        r#"
      (= g-text (make-text "office é🌍"))
      (= (g-clusters 0u) (TextClusterBox (vec4f 0. 0. 10. 20.) 0u 1u 0u false))
      (= (g-clusters 1u) (TextClusterBox (vec4f 10. 0. 30. 20.) 1u 4u 0u false))
      (= (g-clusters 2u) (TextClusterBox (vec4f 40. 0. 10. 20.) 4u 5u 0u false))
      (= (g-clusters 3u) (TextClusterBox (vec4f 50. 0. 10. 20.) 5u 6u 0u false))
      (= (g-clusters 4u) (TextClusterBox (vec4f 60. 0. 8. 20.) 6u 7u 0u false))
      (= (g-clusters 5u) (TextClusterBox (vec4f 68. 0. 12. 20.) 7u 10u 0u false))
      (= (g-clusters 6u) (TextClusterBox (vec4f 80. 0. 20. 20.) 10u 14u 0u false))
      (prepare-test 7u)
      (expect (== g-count 9u))
      (expect (== (.byte (text-hit-test g-count cell-at-test (vec2f 27. 10.))) 3u))
      (expect (== (.byte (text-hit-test g-count cell-at-test (vec2f 70. 10.))) 7u))
      (expect (== (.byte (text-hit-test g-count cell-at-test (vec2f 95. 10.))) 14u))
      (expect (== (.byte (text-hit-test g-count cell-at-test (vec2f -80. -10.))) 0u))
      (expect (== (.byte (text-hit-test g-count cell-at-test (vec2f 1000. 10.))) 14u))
      (expect (== (.x (.rect (text-caret-at g-count cell-at-test 2u 1u))) 20.))
      (expect (== (.status (text-caret-at g-count cell-at-test 8u 1u)) 2u))
      (expect (== (text-selection-rects g-count cell-at-test 3u 1u emit-rect-test) 2u))
      (expect (== (.x (g-rects 0u)) 10.)) (expect (== (.x (g-rects 1u)) 20.))
      (expect (== (text-selection-rects g-count cell-at-test 1u 1u emit-rect-test) 0u))
    "#,
    );
}

#[test]
fn bidi_boundary_affinity_and_physical_movement_use_visual_cells() {
    check(
        r#"
      (= g-text (make-text "abאבcd"))
      (= (g-clusters 0u) (TextClusterBox (vec4f 0. 0. 20. 20.) 0u 2u 0u false))
      (= (g-clusters 1u) (TextClusterBox (vec4f 20. 0. 20. 20.) 2u 6u 0u true))
      (= (g-clusters 2u) (TextClusterBox (vec4f 40. 0. 20. 20.) 6u 8u 0u false))
      (prepare-test 3u)
      (let [before (text-caret-at g-count cell-at-test 2u 0u)
            after (text-caret-at g-count cell-at-test 2u 1u)]
        (expect (== before.rect.x 20.)) (expect (== after.rect.x 40.))
        (expect (== (.byte (text-move-horizontal g-count cell-at-test after false)) 4u)))
      (expect (== (.byte (text-hit-test g-count cell-at-test (vec2f 28. 10.))) 4u))
      (expect (== (.byte (text-hit-test g-count cell-at-test (vec2f 37. 10.))) 2u))
      (expect (== (text-selection-rects g-count cell-at-test 0u 4u emit-rect-test) 3u))
      (expect (== (.x (g-rects 2u)) 30.))
    "#,
    );
}

#[test]
fn vertical_movement_retains_preferred_x_across_short_and_empty_lines() {
    check(
        r#"
      (= g-text (make-text "abcdefghi"))
      (= (g-clusters 0u) (TextClusterBox (vec4f 0. 0. 30. 20.) 0u 3u 0u false))
      (= (g-clusters 1u) (TextClusterBox (vec4f 0. 30. 10. 20.) 3u 4u 1u false))
      (= (g-clusters 2u) (TextClusterBox (vec4f 0. 60. 0. 20.) 4u 4u 2u false))
      (= (g-clusters 3u) (TextClusterBox (vec4f 0. 90. 50. 20.) 4u 9u 3u false))
      (prepare-test 4u)
      (let [first (text-caret-at g-count cell-at-test 2u 1u)
            second (text-move-vertical g-count cell-at-test first 20. true)
            empty (text-move-vertical g-count cell-at-test second 20. true)
            last (text-move-vertical g-count cell-at-test empty 20. true)]
        (expect (== second.byte 4u)) (expect (== second.line 1u))
        (expect (== empty.byte 4u)) (expect (== empty.line 2u))
        (expect (== last.byte 6u)) (expect (== last.rect.x 20.))
        (expect (== (.line (text-move-vertical g-count cell-at-test last 20. true)) 3u)))
      (expect (== (.line (text-caret-at g-count cell-at-test 3u 0u)) 0u))
      (expect (== (.line (text-caret-at g-count cell-at-test 3u 1u)) 1u))
      (expect (== (.line (text-hit-test g-count cell-at-test (vec2f 20. 67.))) 2u))
    "#,
    );
}

#[test]
fn malformed_geometry_and_capacity_errors_leave_the_previous_cells_untouched() {
    check(
        r#"
      (= g-text (make-text "é🌍"))
      (= (g-clusters 0u) (TextClusterBox (vec4f 0. 0. 20. 20.) 0u 7u 0u false))
      (prepare-test 1u)
      (let [result (text-build-cells g-text 1u cluster-at-test 1u emit-cell-test)]
        (expect (== result.status 2u)) (expect (== result.count 0u)))
      (= (.start (g-clusters 0u)) 1u)
      (expect (== (.status (text-build-cells g-text 1u cluster-at-test 128u emit-cell-test)) 1u))
      (= (.start (g-clusters 0u)) 0u) (= (.w (.bounds (g-clusters 0u))) -1.)
      (expect (== (.status (text-build-cells g-text 1u cluster-at-test 128u emit-cell-test)) 1u))
      (expect (== (.start (g-cells 0u)) 0u)) (expect (== (.end (g-cells 0u)) 3u))
      (expect (== (.end (g-cells 1u)) 7u))
      (expect (== (.status (text-hit-test 0u cell-at-test (vec2f 0.))) 2u))
    "#,
    );
}

#[test]
fn pointer_selection_and_input_reject_stale_layout_without_losing_preedit() {
    check(
        r#"
      (= g-text (make-text "éabc🌍"))
      (= (g-clusters 0u) (TextClusterBox (vec4f 0. 0. 50. 20.) 0u 10u 0u false))
      (prepare-test 1u) (text-editor-open g-editor g-text) (text-view-reset g-view)
      (expect (== (text-view-pointer g-editor g-view g-editor.current.text g-count cell-at-test
        (vec2f 2. 5.) true true true false) 0u))
      (expect (== (text-view-pointer g-editor g-view g-editor.current.text g-count cell-at-test
        (vec2f 38. 5.) false false true false) 0u))
      (expect (== g-editor.current.anchor 0u)) (expect (== g-editor.current.focus 6u))
      (expect (== (text-view-pointer g-editor g-view g-editor.current.text g-count cell-at-test
        (vec2f 38. 5.) false false false false) 4u))
      (expect (not g-view.dragging))
      (expect (== (text-view-pointer g-editor g-view g-editor.current.text g-count cell-at-test
        (vec2f 48. 5.) true true false true) 0u))
      (expect (== g-editor.current.focus 10u)) (expect (== g-editor.current.anchor 0u))
      ; Use the retained editor identity for the captured layout, not the seed's
      ; independently owned handle, even though their immutable bytes are equal.
      (let [captured g-editor.current.text insert (make-text "Z")]
        (expect (== (text-view-input g-editor g-view captured g-count cell-at-test
          (TextInputEvent 1u 3u 0u 0u 0u 0u 0u) (TextBuffer 0u) 8u 4u) 0u))
        (expect (== g-editor.current.focus 0u))
        (expect (== (text-view-input g-editor g-view captured g-count cell-at-test
          (TextInputEvent 0u 0u 0u 0u 1u 0u 0u) insert 8u 4u) 0u))
        (expect (== (text-view-pointer g-editor g-view captured g-count cell-at-test
          (vec2f 48. 5.) true true true false) 5u))
        (expect (== g-editor.current.focus 1u))
        (let [preview (make-text "仮")]
          (expect (== (text-editor-preedit g-editor preview 0u 3u) 0u))
          (expect (== (text-view-pointer g-editor g-view captured g-count cell-at-test
            (vec2f 48. 5.) true true true false) 3u))
          (expect g-editor.composing) (text-release preview))
        (expect (== g-editor.undo-count 1u)) (text-release insert))
    "#,
    );
}

#[test]
fn view_navigation_preserves_preferred_column_and_routes_platform_line_commands() {
    check(
        r#"
      (= g-text (make-text "abcdefgh"))
      (= (g-clusters 0u) (TextClusterBox (vec4f 0. 0. 30. 20.) 0u 3u 0u false))
      (= (g-clusters 1u) (TextClusterBox (vec4f 0. 30. 10. 20.) 3u 4u 1u false))
      (= (g-clusters 2u) (TextClusterBox (vec4f 0. 60. 40. 20.) 4u 8u 2u false))
      (prepare-test 3u) (text-editor-open g-editor g-text) (text-view-reset g-view)
      (= g-geometry g-editor.current.text)
      (expect (== (text-editor-edit g-editor (TextEditCommand 3u 2u 0u 0u) (TextBuffer 0u)) 0u))
      (expect (== (view-key-test 6u 0u) 0u)) (expect (== g-editor.current.focus 4u))
      (expect (== g-view.preferred-x 20.))
      (expect (== (text-view-input g-editor g-view g-geometry g-count cell-at-test
        (TextInputEvent 2u 6u 0u 0u 0u 0u 0u) (TextBuffer 0u) 8u 4u) 4u))
      (expect g-view.vertical)
      (expect (== (view-key-test 6u 1u) 0u)) (expect (== g-editor.current.focus 6u))
      (expect (== g-editor.current.anchor 4u))
      (expect (== (view-key-test 3u 8u) 0u)) (expect (== g-editor.current.focus 4u))
      (expect (not g-view.vertical))
      (expect (== (view-key-test 8u 0u) 0u)) (expect (== g-editor.current.focus 8u))
      (expect (== g-editor.undo-count 0u))
    "#,
    );
}

#[test]
fn zero_advance_graphemes_remain_reachable_by_keyboard() {
    let source = r#"
      (= g-text (make-text "a<zwsp>b"))
      (= (g-clusters 0u) (TextClusterBox (vec4f 0. 0. 10. 20.) 0u 1u 0u false))
      (= (g-clusters 1u) (TextClusterBox (vec4f 10. 0. 0. 20.) 1u 4u 0u false))
      (= (g-clusters 2u) (TextClusterBox (vec4f 10. 0. 10. 20.) 4u 5u 0u false))
      (prepare-test 3u)
      (let [before (text-caret-at g-count cell-at-test 1u 1u)
            after (text-move-horizontal g-count cell-at-test before true)
            back (text-move-horizontal g-count cell-at-test after false)]
        (expect (== after.byte 4u)) (expect (== after.rect.x 10.))
        (expect (== back.byte 1u)) (expect (== back.rect.x 10.)))
    "#;
    // Rust escapes the invisible character; EASL receives the actual UTF-8.
    check(&source.replace("<zwsp>", "\u{200b}"));
}

#[test]
fn viewport_controller_translates_pointer_hits_pages_and_reveals_without_editing_source() {
    check(
        r#"
      (= g-text (make-text "abcdefghij"))
      (for [i 10u]
        (= (g-clusters i) (TextClusterBox (vec4f 5. (* (f32 i) 20.) 10. 20.) i (+ i 1u) i false)))
      (prepare-test 10u) (text-editor-open g-editor g-text)
      (= g-geometry (text-editor-visible g-editor))
      (expect (== (.status (text-index-lines g-count cell-at-test 16u emit-index-test)) 0u))
      (= g-layout (TextWidgetLayout g-geometry g-count 10u (vec2f 100. 60.) (vec2f 100. 200.)))
      (text-widget-reset g-widget) (= g-widget.offset (vec2f 0. 40.))
      (expect (== (text-widget-pointer g-editor g-widget g-layout index-at-test cell-at-test
        (vec2f 5. 10.) true true true false) 0u))
      (expect g-widget.focused) (expect (== g-editor.current.focus 2u))
      (expect (== (widget-key-test 10u 1u) 0u))
      (expect (== g-editor.current.anchor 2u)) (expect (== g-editor.current.focus 4u))
      (expect (== (text-widget-reveal g-widget g-layout
        (text-caret-at g-count cell-at-test 4u g-widget.view.affinity) (vec2f 4.)) 0u))
      (expect (> g-widget.offset.y 40.))
      (expect (== (widget-key-test 9u 0u) 0u)) (expect (== g-editor.current.focus 2u))
      (expect (== g-widget.view.preferred-x 5.))
      (expect (text-equal g-editor.current.text g-text)) (expect (== g-editor.undo-count 0u))
      (let [payload (make-text "Z")]
        (expect (== (text-widget-input g-editor g-widget g-layout index-at-test cell-at-test
          (TextInputEvent 0u 0u 0u 0u 1u 0u 0u) payload 8u 4u) 0u))
        (text-release payload))
      (expect (== (widget-key-test 10u 0u) 5u))
      (expect (== (.status (text-widget-decorations g-editor g-widget g-layout cell-at-test emit-rect-test)) 5u))
    "#,
    );
}

#[test]
fn widget_pages_progress_through_gaps_and_finish_at_document_edges() {
    check(
        r#"
      (= g-text (make-text "abc"))
      (for [i 3u]
        (= (g-clusters i) (TextClusterBox (vec4f 0. (* (f32 i) 100.) 10. 20.) i (+ i 1u) i false)))
      (prepare-test 3u) (text-editor-open g-editor g-text)
      (= g-geometry (text-editor-visible g-editor))
      (text-index-lines g-count cell-at-test 16u emit-index-test)
      (= g-layout (TextWidgetLayout g-geometry g-count 3u (vec2f 100. 20.) (vec2f 100. 220.)))
      (text-widget-reset g-widget)
      (expect (== (widget-key-test 10u 1u) 0u))
      (expect (== g-editor.current.focus 1u))
      (expect (== (widget-key-test 10u 1u) 0u))
      (expect (== g-editor.current.focus 2u))
      (expect (== (widget-key-test 10u 1u) 0u))
      (expect (== g-editor.current.focus 3u))
      (expect (== g-editor.current.anchor 0u))
      (expect (not g-widget.view.vertical))
      (expect (text-equal g-editor.current.text g-text))
      (expect (== g-editor.undo-count 0u))
    "#,
    );
}

#[test]
fn multiline_composition_underlines_follow_each_line_and_blur_cancels_without_history() {
    check(
        r#"
      (= g-text (make-text "abcd")) (text-editor-open g-editor g-text)
      (let [payload (make-text "xy")]
        (expect (== (text-editor-preedit g-editor payload 0u 2u) 0u)) (text-release payload))
      (= g-geometry (text-editor-visible g-editor))
      ; The two composing characters cross a visual wrap.
      (for [i 6u]
        (= (g-cells i) (TextCell (vec4f 5. (* (f32 i) 20.) 10. 20.) i (+ i 1u) i false)))
      (= g-count 6u)
      (text-index-lines g-count cell-at-test 16u emit-index-test)
      (= g-layout (TextWidgetLayout g-geometry g-count 6u (vec2f 100. 60.) (vec2f 100. 120.)))
      (text-widget-reset g-widget) (= g-widget.focused true)
      (let [decoration (text-widget-decorations g-editor g-widget g-layout cell-at-test emit-rect-test)]
        (expect (== decoration.status 0u)) (expect (== decoration.count 2u))
        (expect decoration.show-caret) (expect (== decoration.caret.byte 2u))
        (expect (== (.y (g-rects 0u)) 18.5)) (expect (== (.y (g-rects 1u)) 38.5))
        (expect (== (.w (g-rects 0u)) 1.5)))
      (expect (text-equal g-editor.current.text g-text))
      (expect (== (text-widget-input g-editor g-widget g-layout index-at-test cell-at-test
        (TextInputEvent 6u 0u 0u 0u 0u 0u 0u) (TextBuffer 0u) 8u 4u) 0u))
      (expect (not g-widget.focused)) (expect (not g-editor.composing))
      (expect (== g-editor.undo-count 0u))
    "#,
    );
}
