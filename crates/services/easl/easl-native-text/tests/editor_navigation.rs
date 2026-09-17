use easl_native_text::{EditCommand, Movement, TextEditor, TextStyle, TextSystem};

#[test]
fn navigation_preserves_redo_and_graphemes_after_editing_composition_and_restore() {
    let mut system = TextSystem::new();
    let mut editor = TextEditor::new("e", TextStyle::default()).unwrap();
    editor
        .command(&mut system, EditCommand::Move(Movement::TextEnd, false))
        .unwrap();
    editor
        .command(&mut system, EditCommand::Insert("\u{301}".into()))
        .unwrap();
    assert_eq!(editor.text(), "e\u{301}");
    assert_eq!(editor.deletion_range(&mut system, true, false), 0..3);
    editor.command(&mut system, EditCommand::Undo).unwrap();
    editor.command(&mut system, EditCommand::SelectAll).unwrap();
    editor
        .command(&mut system, EditCommand::Move(Movement::TextEnd, false))
        .unwrap();
    editor.command(&mut system, EditCommand::Redo).unwrap();
    assert_eq!(editor.text(), "e\u{301}");
    assert_eq!(editor.selection_bytes(), (3, 3));
    editor
        .command(&mut system, EditCommand::Preedit("🇯🇵".into(), None))
        .unwrap();
    assert_eq!(editor.inner().raw_text(), "e\u{301}🇯🇵");
    editor
        .command(&mut system, EditCommand::Move(Movement::Left, false))
        .unwrap();
    assert_eq!(editor.text(), "e\u{301}");
    assert_eq!(editor.inner().raw_text(), "e\u{301}");
    assert_eq!(editor.selection_bytes(), (0, 0));
    assert_eq!(editor.deletion_range(&mut system, false, false), 0..3);
    editor.command(&mut system, EditCommand::Delete).unwrap();
    assert_eq!(editor.text(), "");
    editor.command(&mut system, EditCommand::Undo).unwrap();
    assert_eq!(editor.text(), "e\u{301}");
    // Navigation and the cancelled composition add no history entry.
    editor.command(&mut system, EditCommand::Undo).unwrap();
    assert_eq!(editor.text(), "e");
}

#[test]
fn external_projections_rebuild_unicode_boundaries_and_reject_invalid_pointer_input_atomically() {
    let mut system = TextSystem::new();
    let mut editor = TextEditor::new("old", TextStyle::default()).unwrap();
    let text = "e\u{301}🇯🇵\r\nx";
    editor
        .set_projection(&mut system, text, &[], (1, 1))
        .unwrap();
    assert_eq!(editor.selection_bytes(), (0, 0));
    for (start, end) in [(0, 3), (3, 11), (11, 13), (13, 14)] {
        editor
            .set_projection(&mut system, text, &[], (start, start))
            .unwrap();
        assert_eq!(editor.deletion_range(&mut system, false, false), start..end);
        editor
            .set_projection(&mut system, text, &[], (end, end))
            .unwrap();
        assert_eq!(editor.deletion_range(&mut system, true, false), start..end);
    }
    editor
        .command(&mut system, EditCommand::Preedit("👩🏽‍🚀".into(), None))
        .unwrap();
    let composing = editor.inner().raw_text().to_owned();
    let selection = editor.selection_bytes();
    assert!(
        editor
            .command(&mut system, EditCommand::Click(f32::NAN, 1., 1, false))
            .is_err()
    );
    assert!(
        editor
            .command(&mut system, EditCommand::Drag(0., f32::INFINITY))
            .is_err()
    );
    assert_eq!(editor.inner().raw_text(), composing);
    assert_eq!(editor.selection_bytes(), selection);
    assert_eq!(editor.text(), text);
}
