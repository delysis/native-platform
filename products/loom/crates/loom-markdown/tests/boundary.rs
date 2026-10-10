use loom_markdown::{DeleteDirection, Dialect, EditorDocument, Markdown, Selection};

fn assert_view(doc: &EditorDocument, source: &str) {
    let expected = Markdown::parse(source, Dialect::Loom)
        .unwrap()
        .project()
        .unwrap();
    assert_eq!(doc.projection().text(), expected.text(), "{}", doc.source());
    let shape = |projection: &loom_markdown::Projection| {
        projection
            .blocks()
            .iter()
            .map(|b| {
                (
                    b.style.clone(),
                    b.quote_depth,
                    b.list_depth,
                    b.marker.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        shape(doc.projection()),
        shape(&expected),
        "{}",
        doc.source()
    );
}

#[test]
fn backspace_lifts_the_nearest_container_at_its_first_text_boundary() {
    // Reference results: original ProseMirror baseKeymap.Backspace, using its
    // installed Markdown schema. Each action is one undoable structural edit.
    for (source, block, expected) in [
        ("- one\n- two", 0, "one\n\n* two"),
        ("> one\n>\n> two", 0, "one\n\n> two"),
        ("> - one\n> - two", 0, "> one\n>\n> * two"),
        (
            "- > one\n  >\n  > two\n- last",
            0,
            "* one\n\n  > two\n\n* last",
        ),
        (
            "- parent\n  - child\n  - next\n- last",
            1,
            "* parent\n\n  child\n  * next\n* last",
        ),
        (
            "- one\n\n  continuation\n- two",
            0,
            "one\n\n* continuation\n\n* two",
        ),
        ("- one\n  - child\n- two", 0, "one\n\n* * child\n* two"),
    ] {
        let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
        let selection = Selection::caret(doc.projection().blocks()[block].display.start);
        assert!(
            doc.delete_boundary(selection, DeleteDirection::Backward)
                .unwrap()
        );
        assert_view(&doc, expected);
        assert_eq!(doc.visual_selection().unwrap(), selection);
        doc.undo().unwrap();
        assert_eq!(doc.source(), source);
        assert_eq!(doc.visual_selection().unwrap(), selection);
        doc.redo().unwrap();
        assert_view(&doc, expected);
    }
}

#[test]
fn keyboard_boundaries_preserve_paragraphs_before_the_next_text_join() {
    for (source, before, expected) in [
        ("> one\n\nBody", 0, "> one\n>\n> Body"),
        ("- one\n- two", 0, "* one\n\n  two"),
        ("Body\n\n> quote\n>\n> tail", 0, "Body\n\nquote\n\n> tail"),
        (
            "1. one\n\n> quoted\n>\n> tail",
            0,
            "1. one\n2. > quoted\n   >\n   > tail",
        ),
        (
            "Body\n\n> - nested\n> - tail",
            0,
            "Body\n\n> nested\n>\n> * tail",
        ),
    ] {
        for direction in [DeleteDirection::Backward, DeleteDirection::Forward] {
            let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
            let caret = match direction {
                DeleteDirection::Backward => doc.projection().blocks()[before + 1].display.start,
                DeleteDirection::Forward => doc.projection().blocks()[before].display.end,
            };
            let selection = Selection::caret(caret);
            assert!(
                doc.delete_boundary(selection, direction)
                    .unwrap_or_else(|e| panic!("{source:?}: {e}"))
            );
            assert_view(&doc, expected);
            assert_eq!(doc.visual_selection().unwrap(), selection);
            doc.undo().unwrap();
            assert_eq!(doc.source(), source);
            assert_eq!(doc.visual_selection().unwrap(), selection);
            doc.redo().unwrap();
            assert_view(&doc, expected);
        }
    }
}

#[test]
fn text_boundaries_and_selections_decline_without_mutation() {
    for (source, block) in [
        ("one\n\ntwo", 1),
        ("> one\n>\n> two", 1),
        ("- parent\n\n  continuation\n- last", 1),
        ("# heading", 0),
        ("```rust\ncode\n```", 0),
    ] {
        let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
        let selection = Selection::caret(doc.projection().blocks()[block].display.start);
        assert!(
            !doc.delete_boundary(selection, DeleteDirection::Backward)
                .unwrap()
        );
        assert_eq!(doc.source(), source);
        assert!(!doc.can_undo());
    }
    let mut doc = EditorDocument::new("- one\n- two", Dialect::Loom).unwrap();
    for selection in [
        Selection::caret(2),
        Selection {
            anchor: 3,
            focus: 4,
        },
    ] {
        assert!(
            !doc.delete_boundary(selection, DeleteDirection::Backward)
                .unwrap()
        );
        assert!(!doc.can_undo());
    }
    let mut plain = EditorDocument::new("- one\n- two", Dialect::PlainText).unwrap();
    assert!(
        !plain
            .delete_boundary(Selection::caret(0), DeleteDirection::Backward)
            .unwrap()
    );
}
