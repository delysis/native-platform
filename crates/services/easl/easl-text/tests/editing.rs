use easl_native_text::{EditCommand, TextEditor, TextStyle, TextSystem};
use easl_text::{EditAction, Editing};

#[test]
fn native_widget_edits_follow_easl_plans_and_preserve_exact_undo() {
    let mut system = TextSystem::new();
    let mut behavior = Editing::new().unwrap();
    let source = "A👩🏽‍🚀e\u{301}\r\n";
    let mut editor = TextEditor::new(source, TextStyle::default()).unwrap();
    behavior
        .apply(
            &mut editor,
            &mut system,
            EditAction::MoveTo {
                byte: 16,
                extend: false,
            },
        )
        .unwrap();
    behavior
        .apply(&mut editor, &mut system, EditAction::Backspace)
        .unwrap();
    assert_eq!(editor.text(), "Ae\u{301}\r\n");
    assert_eq!(editor.selection_bytes(), (1, 1));
    editor.command(&mut system, EditCommand::Undo).unwrap();
    assert_eq!(editor.text(), source);
    assert_eq!(editor.selection_bytes(), (16, 16));
    editor.command(&mut system, EditCommand::Redo).unwrap();
    assert_eq!(editor.text(), "Ae\u{301}\r\n");
    behavior
        .apply(&mut editor, &mut system, EditAction::Delete)
        .unwrap();
    assert_eq!(editor.text(), "A\r\n");
    behavior
        .apply(&mut editor, &mut system, EditAction::Delete)
        .unwrap();
    assert_eq!(editor.text(), "A");
    editor.command(&mut system, EditCommand::Undo).unwrap();
    assert_eq!(editor.text(), "A\r\n");
    behavior
        .apply(&mut editor, &mut system, EditAction::SelectAll)
        .unwrap();
    behavior
        .apply(&mut editor, &mut system, EditAction::Replace("Καλημέρα\n"))
        .unwrap();
    assert_eq!(editor.text(), "Καλημέρα\n");
    editor.command(&mut system, EditCommand::Undo).unwrap();
    assert_eq!(editor.text(), "A\r\n");
    assert_eq!(editor.selection_bytes(), (0, 3));
    assert_eq!(behavior.decisions, 6);
}

#[test]
fn reversed_selection_and_extension_do_not_create_history() {
    let mut behavior = Editing::new().unwrap();
    let mut system = TextSystem::new();
    let mut editor = TextEditor::new("abc def", TextStyle::default()).unwrap();
    for action in [
        EditAction::MoveTo {
            byte: 7,
            extend: false,
        },
        EditAction::MoveTo {
            byte: 0,
            extend: true,
        },
    ] {
        behavior.apply(&mut editor, &mut system, action).unwrap();
    }
    assert_eq!(editor.selection_bytes(), (7, 0));
    let plan = behavior
        .plan_for(&mut editor, &mut system, EditAction::DeleteWord)
        .unwrap();
    assert_eq!(plan.replacement(), Some(0..7));
    assert_eq!(plan.selection(), (0, 0));
    behavior
        .apply(&mut editor, &mut system, EditAction::CollapseEnd)
        .unwrap();
    assert_eq!(editor.selection_bytes(), (7, 7));
    behavior
        .apply(&mut editor, &mut system, EditAction::Replace("!"))
        .unwrap();
    behavior
        .apply(&mut editor, &mut system, EditAction::SelectAll)
        .unwrap();
    behavior
        .apply(&mut editor, &mut system, EditAction::CollapseStart)
        .unwrap();
    editor.command(&mut system, EditCommand::Undo).unwrap();
    assert_eq!(editor.text(), "abc def");
    assert_eq!(editor.selection_bytes(), (7, 7));
}

#[test]
fn word_deletion_keeps_native_logical_unicode_boundaries() {
    let mut behavior = Editing::new().unwrap();
    let mut system = TextSystem::new();
    for source in [
        "café words",
        "שלום עולם",
        "العربية test",
        "A\r\nB",
        "👩🏽‍🚀 e\u{301} α",
    ] {
        let mut editor = TextEditor::new(source, TextStyle::default()).unwrap();
        behavior
            .apply(
                &mut editor,
                &mut system,
                EditAction::MoveTo {
                    byte: source.len(),
                    extend: false,
                },
            )
            .unwrap();
        let expected = editor.deletion_range(&mut system, true, true);
        if source == "A\r\nB" {
            assert_eq!(expected, 3..4);
        }
        let plan = behavior
            .plan_for(&mut editor, &mut system, EditAction::BackspaceWord)
            .unwrap();
        assert_eq!(plan.replacement(), Some(expected.clone()), "{source:?}");
        behavior
            .apply(&mut editor, &mut system, EditAction::BackspaceWord)
            .unwrap_or_else(|error| panic!("{source:?} {expected:?}: {error}"));
        let mut expected_source = source.to_owned();
        expected_source.replace_range(expected, "");
        assert_eq!(editor.text(), expected_source);
        editor.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(editor.text(), source);
        editor
            .command(&mut system, EditCommand::BackspaceWord)
            .unwrap();
        assert_eq!(editor.text(), expected_source);
        editor.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(editor.text(), source);
    }
}

#[test]
fn invalid_offsets_oversized_edits_and_preedit_do_not_mutate_the_widget() {
    let mut behavior = Editing::new().unwrap();
    let mut system = TextSystem::new();
    let mut editor = TextEditor::new("e\u{301}🌍", TextStyle::default()).unwrap();
    assert!(
        behavior
            .apply(
                &mut editor,
                &mut system,
                EditAction::MoveTo {
                    byte: 1,
                    extend: false
                }
            )
            .is_err()
    );
    assert_eq!(editor.selection_bytes(), (0, 0));
    let oversized = "x".repeat(easl_native_text::MAX_TEXT_BYTES + 1);
    assert!(
        behavior
            .apply(&mut editor, &mut system, EditAction::Replace(&oversized))
            .is_err()
    );
    assert_eq!(editor.text(), "e\u{301}🌍");
    editor
        .command(&mut system, EditCommand::Preedit("仮名".into(), None))
        .unwrap();
    let visible = editor.inner().raw_text().to_owned();
    assert!(
        behavior
            .apply(&mut editor, &mut system, EditAction::Backspace)
            .is_err()
    );
    assert_eq!(editor.inner().raw_text(), visible);
    assert_eq!(editor.text(), "e\u{301}🌍");
    editor
        .command(&mut system, EditCommand::CancelCompose)
        .unwrap();
    behavior
        .apply(&mut editor, &mut system, EditAction::SelectAll)
        .unwrap();
    behavior
        .apply(&mut editor, &mut system, EditAction::Replace("ok"))
        .unwrap();
    assert_eq!(editor.text(), "ok");
}
