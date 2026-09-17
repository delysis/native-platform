use easl_native_text::{EditCommand, Movement, TextEditor, TextStyle, TextSystem};

#[test]
fn page_navigation_uses_view_height_and_preserves_horizontal_intent_and_selection() {
    let text = "abcdefghijk\nabcdefghijk\nabcdefghijk\nx\nabcdefghijk\nabcdefghijk\nabcdefghijk\nabcdefghijk\nabcdefghijk\nabcdefghijk";
    let style = TextStyle::default();
    let mut system = TextSystem::new();
    let mut editor = TextEditor::new(text, style.clone()).unwrap();
    editor
        .set_projection(&mut system, text, &[], (8, 8))
        .unwrap();
    editor
        .ensure_layout(&mut system, &style, 500., 200.)
        .unwrap();
    let line_height = editor.inner().cursor_geometry(1.).unwrap().height() as f32;
    editor
        .ensure_layout(&mut system, &style, 500., line_height * 4.)
        .unwrap();
    editor
        .command(&mut system, EditCommand::Move(Movement::PageDown, false))
        .unwrap();
    assert_eq!(editor.selection_bytes(), (37, 37)); // Short line clamps to its end.
    editor
        .command(&mut system, EditCommand::Move(Movement::PageDown, false))
        .unwrap();
    assert_eq!(editor.selection_bytes(), (70, 70)); // Original column on line six.
    editor
        .command(&mut system, EditCommand::Move(Movement::PageUp, false))
        .unwrap();
    assert_eq!(editor.selection_bytes(), (37, 37));
    editor
        .command(&mut system, EditCommand::Move(Movement::PageUp, false))
        .unwrap();
    assert_eq!(editor.selection_bytes(), (8, 8));
    editor
        .command(&mut system, EditCommand::Move(Movement::PageDown, true))
        .unwrap();
    assert_eq!(editor.selection_bytes(), (8, 37));
    editor
        .command(&mut system, EditCommand::Move(Movement::PageUp, true))
        .unwrap();
    assert_eq!(editor.selection_bytes(), (8, 8));
    editor
        .ensure_layout(&mut system, &style, 500., line_height * 7.)
        .unwrap();
    editor
        .command(&mut system, EditCommand::Move(Movement::PageDown, false))
        .unwrap();
    assert_eq!(editor.selection_bytes(), (70, 70));
    assert_eq!(editor.text(), text);
}

#[test]
fn paging_reveals_the_caret_and_clamps_at_document_edges() {
    let text = "אבגדה cafe\u{301} 日本語\n".repeat(40);
    let style = TextStyle::default();
    let mut system = TextSystem::new();
    let mut editor = TextEditor::new(&text, style.clone()).unwrap();
    editor
        .ensure_layout(&mut system, &style, 180., 160.)
        .unwrap();
    editor
        .command(&mut system, EditCommand::Move(Movement::PageDown, false))
        .unwrap();
    let first = editor.selection_bytes().1;
    assert!(first > 0 && first < text.len() / 2);
    assert!(text.is_char_boundary(first));
    editor
        .ensure_layout(&mut system, &style, 180., 160.)
        .unwrap();
    let caret = editor.inner().cursor_geometry(1.).unwrap();
    assert!(caret.y0 >= f64::from(editor.scroll));
    assert!(caret.y1 <= f64::from(editor.scroll + 160.));
    for _ in 0..50 {
        editor
            .command(&mut system, EditCommand::Move(Movement::PageDown, false))
            .unwrap();
    }
    assert_eq!(editor.selection_bytes(), (text.len(), text.len()));
    for _ in 0..50 {
        editor
            .command(&mut system, EditCommand::Move(Movement::PageUp, false))
            .unwrap();
    }
    assert_eq!(editor.selection_bytes(), (0, 0));
    assert_eq!(editor.text(), text);
    let mut empty = TextEditor::new("", style.clone()).unwrap();
    empty.ensure_layout(&mut system, &style, 0., 0.).unwrap();
    for movement in [Movement::PageUp, Movement::PageDown] {
        empty
            .command(&mut system, EditCommand::Move(movement, true))
            .unwrap();
        assert_eq!(empty.selection_bytes(), (0, 0));
    }
}
