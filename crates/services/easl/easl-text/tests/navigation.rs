use easl_native_text::{EditCommand, Movement, ParagraphLayout, TextEditor, TextStyle, TextSystem};
use easl_text::{HitTesting, LineMovement, Navigation};

fn editor(source: &str, system: &mut TextSystem, width: f32, height: f32) -> TextEditor {
    let style = TextStyle {
        family: "monospace".into(),
        ..TextStyle::default()
    };
    let mut editor = TextEditor::new(source, style.clone()).unwrap();
    editor.ensure_layout(system, &style, width, height).unwrap();
    editor
}

#[test]
fn vertical_selection_preserves_the_column_through_short_lines_and_resets_on_pointer_input() {
    let source = "abcdefghij\nx\nabcdefghij\n";
    let mut system = TextSystem::new();
    let mut navigation = Navigation::new().unwrap();
    let mut hits = HitTesting::new().unwrap();
    let mut editor = editor(source, &mut system, 400., 100.);
    editor.select_range(&mut system, (7, 7)).unwrap();
    for (movement, expected) in [
        (LineMovement::Down, 12),
        (LineMovement::Down, 20),
        (LineMovement::Up, 12),
        (LineMovement::Up, 7),
    ] {
        navigation
            .apply(&mut editor, &mut system, &mut hits, movement, true)
            .unwrap();
        assert_eq!(editor.selection_bytes(), (7, expected));
    }
    navigation
        .apply(
            &mut editor,
            &mut system,
            &mut hits,
            LineMovement::Down,
            false,
        )
        .unwrap();
    assert_eq!(editor.selection_bytes(), (12, 12));
    let hit = hits
        .hit_editor(&mut editor, &mut system, [0., 40.])
        .unwrap();
    editor
        .select_pointer_target(&mut system, hit.byte, hit.affinity, 1, false)
        .unwrap();
    navigation
        .apply(
            &mut editor,
            &mut system,
            &mut hits,
            LineMovement::Down,
            false,
        )
        .unwrap();
    assert_eq!(editor.selection_bytes(), (13, 13));
    editor.command(&mut system, EditCommand::Undo).unwrap();
    assert_eq!(editor.text(), source);
}

#[test]
fn logical_line_and_document_edges_preserve_whole_separators_and_wrap_affinity() {
    let source = "alpha beta gamma\r\nשלום עולם\u{2028}last\n";
    let mut system = TextSystem::new();
    let mut navigation = Navigation::new().unwrap();
    let mut hits = HitTesting::new().unwrap();
    let mut editor = editor(source, &mut system, 110., 100.);
    let count = editor.navigation_context(&mut system).unwrap().lines;
    for index in 0..count {
        let line = editor.line_geometry(&mut system, index).unwrap();
        let hit = hits.hit_line(&mut editor, &mut system, index, 0.).unwrap();
        editor
            .select_pointer_target(&mut system, hit.byte, hit.affinity, 1, false)
            .unwrap();
        navigation
            .apply(
                &mut editor,
                &mut system,
                &mut hits,
                LineMovement::LineEnd,
                true,
            )
            .unwrap();
        assert_eq!(editor.selection_bytes(), (hit.byte, line.separator_start));
        assert_eq!(editor.navigation_context(&mut system).unwrap().line, index);
        navigation
            .apply(
                &mut editor,
                &mut system,
                &mut hits,
                LineMovement::LineStart,
                false,
            )
            .unwrap();
        assert_eq!(editor.selection_bytes(), (line.start, line.start));
    }
    navigation
        .apply(
            &mut editor,
            &mut system,
            &mut hits,
            LineMovement::TextStart,
            false,
        )
        .unwrap();
    assert_eq!(editor.selection_bytes(), (0, 0));
    navigation
        .apply(
            &mut editor,
            &mut system,
            &mut hits,
            LineMovement::TextEnd,
            true,
        )
        .unwrap();
    assert_eq!(editor.selection_bytes(), (0, source.len()));
    assert_eq!(editor.text(), source);
}

#[test]
fn page_navigation_crosses_paragraph_gaps_and_clamps_at_document_edges() {
    let source = "abcdefghij\nx\nabcdefghij\nlast";
    let mut system = TextSystem::new();
    let mut navigation = Navigation::new().unwrap();
    let mut hits = HitTesting::new().unwrap();
    let mut editor = editor(source, &mut system, 400., 20.);
    editor
        .set_paragraphs(&[ParagraphLayout {
            start: 0,
            space_after: 100.,
            ..ParagraphLayout::default()
        }])
        .unwrap();
    editor.select_range(&mut system, (7, 7)).unwrap();
    navigation
        .apply(
            &mut editor,
            &mut system,
            &mut hits,
            LineMovement::PageDown,
            true,
        )
        .unwrap();
    assert_eq!(editor.selection_bytes(), (7, 12));
    navigation
        .apply(
            &mut editor,
            &mut system,
            &mut hits,
            LineMovement::PageDown,
            true,
        )
        .unwrap();
    assert_eq!(editor.selection_bytes(), (7, 20));
    for _ in 0..5 {
        navigation
            .apply(
                &mut editor,
                &mut system,
                &mut hits,
                LineMovement::PageDown,
                true,
            )
            .unwrap();
    }
    assert_eq!(editor.selection_bytes(), (7, source.len()));
    for _ in 0..8 {
        navigation
            .apply(
                &mut editor,
                &mut system,
                &mut hits,
                LineMovement::PageUp,
                true,
            )
            .unwrap();
    }
    assert_eq!(editor.selection_bytes(), (7, 0));
    assert_eq!(editor.text(), source);
}

#[test]
fn ordinary_line_navigation_matches_the_existing_native_commands() {
    let source = "alpha beta gamma\nx\nlonger text again\nlast";
    let mut system = TextSystem::new();
    let mut navigation = Navigation::new().unwrap();
    let mut hits = HitTesting::new().unwrap();
    for width in [90., 500.] {
        let mut expected = editor(source, &mut system, width, 75.);
        let mut actual = editor(source, &mut system, width, 75.);
        for initial in [0, 3, 10, 18, source.len()] {
            expected
                .select_range(&mut system, (initial, initial))
                .unwrap();
            actual
                .select_range(&mut system, (initial, initial))
                .unwrap();
            for (movement, extend) in [
                (Movement::Down, false),
                (Movement::Down, true),
                (Movement::Up, true),
                (Movement::PageDown, false),
                (Movement::PageUp, true),
                (Movement::LineEnd, true),
                (Movement::LineStart, false),
                (Movement::TextEnd, true),
                (Movement::TextStart, false),
            ] {
                expected
                    .command(&mut system, EditCommand::Move(movement, extend))
                    .unwrap();
                navigation
                    .apply(
                        &mut actual,
                        &mut system,
                        &mut hits,
                        LineMovement::from_movement(movement).unwrap(),
                        extend,
                    )
                    .unwrap();
                assert_eq!(
                    actual.selection_bytes(),
                    expected.selection_bytes(),
                    "width {width} initial {initial} {movement:?} {extend}"
                );
            }
        }
    }
}

#[test]
fn preedit_rejection_does_not_change_selection_or_source() {
    let mut system = TextSystem::new();
    let mut navigation = Navigation::new().unwrap();
    let mut hits = HitTesting::new().unwrap();
    let mut editor = editor("first\nsecond", &mut system, 300., 100.);
    editor
        .command(&mut system, EditCommand::Preedit("仮".into(), Some((0, 3))))
        .unwrap();
    let selection = editor.selection_bytes();
    let source = editor.text();
    assert!(
        navigation
            .apply(
                &mut editor,
                &mut system,
                &mut hits,
                LineMovement::Down,
                false
            )
            .is_err()
    );
    assert_eq!(editor.selection_bytes(), selection);
    assert_eq!(editor.text(), source);
    editor
        .command(&mut system, EditCommand::CancelCompose)
        .unwrap();
    navigation
        .apply(
            &mut editor,
            &mut system,
            &mut hits,
            LineMovement::TextEnd,
            false,
        )
        .unwrap();
    assert_eq!(editor.selection_bytes(), (12, 12));
}
