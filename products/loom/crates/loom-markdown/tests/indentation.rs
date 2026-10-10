use loom_markdown::{Dialect, EditorDocument, Selection};

#[test]
fn source_outdent_preserves_crlf_unicode_selection_direction_and_atomic_undo() {
    let source = "\tone α\r\n    two β\n\tthree\r";
    let end = source.find("\tthree").unwrap();
    for selection in [
        Selection {
            anchor: 0,
            focus: end,
        },
        Selection {
            anchor: end,
            focus: 0,
        },
    ] {
        let mut document = EditorDocument::new(source, Dialect::Loom).unwrap();
        document.select(selection).unwrap();
        let tracked = document
            .track_selection(Selection::caret(source.len()))
            .unwrap();
        assert!(document.outdent_source(selection).unwrap());
        assert_eq!(document.source(), "one α\r\ntwo β\n\tthree\r");
        assert_eq!(
            document.source_selection().unwrap(),
            Selection {
                anchor: selection.anchor.saturating_sub(5),
                focus: selection.focus.saturating_sub(5),
            }
        );
        assert_eq!(
            document.tracked_selection(tracked).unwrap(),
            Selection::caret(source.len() - 5)
        );
        assert!(document.undo().unwrap());
        assert_eq!(document.source(), source);
        assert_eq!(document.source_selection().unwrap(), selection);
        assert!(!document.can_undo());
    }
}

#[test]
fn unindented_and_invalid_source_selections_leave_history_and_source_unchanged() {
    let mut document = EditorDocument::new("α plain\r\nnext", Dialect::PlainText).unwrap();
    assert!(!document.outdent_source(Selection::caret(2)).unwrap());
    assert!(document.outdent_source(Selection::caret(1)).is_err());
    assert_eq!(document.source(), "α plain\r\nnext");
    assert_eq!(document.revision(), 0);
    assert!(!document.can_undo());
}

#[test]
fn outdent_at_a_caret_removes_only_one_indentation_step() {
    let mut document = EditorDocument::new("\t\tα\n  β\n", Dialect::PlainText).unwrap();
    assert!(
        document
            .outdent_source(Selection::caret(3 + "α".len()))
            .unwrap()
    );
    assert_eq!(document.source(), "\t\tα\nβ\n");
    assert!(document.outdent_source(Selection::caret(2)).unwrap());
    assert_eq!(document.source(), "\tα\nβ\n");
}
