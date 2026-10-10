use loom_markdown::{Dialect, EditorDocument, InlineFormat, Selection};

#[test]
fn unlink_includes_selected_whitespace_and_undo_restores_exact_source() {
    let source = "[ a ](https://example.com)";
    let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
    let selection = Selection {
        anchor: 0,
        focus: 3,
    };
    doc.format_inline(selection, &InlineFormat::Unlink).unwrap();
    assert!(
        doc.projection()
            .spans()
            .iter()
            .all(|s| s.style.link.is_none())
    );
    assert_eq!(doc.projection().text(), " a ");
    doc.undo().unwrap();
    assert_eq!(doc.source(), source);
}

#[test]
fn formatting_link_state_matches_first_selected_link_and_noninclusive_caret_edges() {
    let doc = EditorDocument::new(
        "[a](https://one.example) [b](https://two.example)",
        Dialect::Loom,
    )
    .unwrap();
    for (selection, href, empty) in [
        (
            Selection {
                anchor: 3,
                focus: 0,
            },
            Some("https://one.example"),
            false,
        ),
        (Selection::caret(0), None, true),
        (Selection::caret(1), None, true),
        (
            Selection {
                anchor: 2,
                focus: 3,
            },
            Some("https://two.example"),
            false,
        ),
    ] {
        let capture = doc.capture_formatting_selection(selection).unwrap();
        let state = doc.formatting_state(&capture).unwrap();
        assert_eq!(state.link.as_deref(), href);
        assert_eq!(state.selection_empty, empty);
    }
}
