use loom_markdown::{Dialect, EditorDocument, ListIndent, Selection, StructureFormat};

fn all(doc: &EditorDocument) -> Selection {
    Selection {
        anchor: 0,
        focus: doc.projection().text().len(),
    }
}

#[test]
fn quote_and_list_toggles_preserve_text_selection_and_exact_undo() {
    for (format, expected) in [
        (StructureFormat::Quote, "> Words"),
        (StructureFormat::BulletList, "* Words"),
        (StructureFormat::OrderedList, "1. Words"),
    ] {
        let mut doc = EditorDocument::new("Words", Dialect::Loom).unwrap();
        let selection = all(&doc);
        doc.format_structure(selection, format).unwrap();
        assert_eq!(doc.source(), expected);
        assert_eq!(doc.visual_selection().unwrap(), selection);
        doc.format_structure(selection, format).unwrap();
        assert_eq!(doc.source(), "Words");
        doc.undo().unwrap();
        assert_eq!(doc.source(), expected);
        doc.undo().unwrap();
        assert_eq!(doc.source(), "Words");
    }
}

#[test]
fn mixed_and_disjoint_containers_unwrap_in_one_transaction() {
    for (source, format, expected) in [
        (
            "Plain\n\n> Quoted",
            StructureFormat::Quote,
            "Plain\n\nQuoted",
        ),
        (
            "Plain\n\n* Listed",
            StructureFormat::BulletList,
            "Plain\n\nListed",
        ),
        (
            "Plain\n\n1. Listed",
            StructureFormat::OrderedList,
            "Plain\n\nListed",
        ),
        (
            "> One\n\nPlain\n\n> Two",
            StructureFormat::Quote,
            "One\n\nPlain\n\nTwo",
        ),
        (
            "* One\n\nPlain\n\n* Two",
            StructureFormat::BulletList,
            "One\n\nPlain\n\nTwo",
        ),
        (
            "1. One\n\nPlain\n\n1. Two",
            StructureFormat::OrderedList,
            "One\n\nPlain\n\nTwo",
        ),
    ] {
        let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
        doc.format_structure(all(&doc), format)
            .unwrap_or_else(|e| panic!("{source:?}: {e}"));
        assert_eq!(doc.source(), expected);
        doc.undo().unwrap();
        assert_eq!(doc.source(), source);
    }
}

#[test]
fn rules_code_images_and_inline_marks_survive_structural_toggles() {
    for source in [
        "---",
        "---\n\n**Words**",
        "Words\n\n---",
        "```rust\nlet x = 1;\n```",
        "![alt](image.png) [Words](https://example.com \"title\")",
        "<div>inert HTML</div>",
        "* One <em>literal HTML</em>",
    ] {
        let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
        let before = doc.projection().text().to_owned();
        doc.format_structure(all(&doc), StructureFormat::Quote)
            .unwrap_or_else(|e| panic!("{source:?}: {e}"));
        assert_eq!(doc.projection().text(), before);
        doc.format_structure(all(&doc), StructureFormat::Quote)
            .unwrap();
        assert_eq!(doc.projection().text(), before);
        doc.undo().unwrap();
        doc.undo().unwrap();
        assert_eq!(doc.source(), source);
    }
}

#[test]
fn a_list_marker_appears_once_when_its_item_starts_with_a_quote() {
    let doc = EditorDocument::new("- > One\n  >\n  > Two\n- Last", Dialect::Loom).unwrap();
    assert_eq!(doc.projection().text(), "One\nTwo\nLast");
    assert_eq!(
        doc.projection()
            .blocks()
            .iter()
            .map(|b| b.marker.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("•"), None, Some("•")]
    );
}

#[test]
fn partial_container_lifts_keep_unselected_roots_and_other_items() {
    for (source, format, expected) in [
        (
            "* One\n* Two\n* Three",
            StructureFormat::BulletList,
            "* One\n\nTwo\n\n* Three",
        ),
        (
            "> One\n>\n> Two\n>\n> Three",
            StructureFormat::Quote,
            "> One\n\nTwo\n\n> Three",
        ),
    ] {
        let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
        doc.format_structure(Selection::caret(5), format)
            .unwrap_or_else(|e| panic!("{source:?}: {e}"));
        assert_eq!(doc.source(), expected);
    }
    let source = "  untouched &amp; bytes\r\n\r\nWords\r\n\r\nEnd &amp; bytes";
    let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
    let at = doc.projection().text().find("Words").unwrap();
    doc.format_structure(Selection::caret(at), StructureFormat::Quote)
        .unwrap();
    assert!(doc.source().starts_with("  untouched &amp; bytes\r\n\r\n"));
    assert!(doc.source().ends_with("\r\n\r\nEnd &amp; bytes"));
}

#[test]
fn enter_splits_list_items_and_an_empty_final_item_exits_the_list() {
    for (source, split) in [
        ("* One\n* Two", "* One\n* Two\n* "),
        ("1. One\n2. Two", "1. One\n2. Two\n3. "),
    ] {
        let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
        doc.replace_visual(Selection::caret(7), "\n")
            .unwrap_or_else(|e| panic!("{source:?}: {e}"));
        assert_eq!(doc.source(), split);
        assert_eq!(doc.visual_selection().unwrap(), Selection::caret(8));
        doc.replace_visual(Selection::caret(8), "Three").unwrap();
        assert_eq!(doc.source(), format!("{split}Three"));
        doc.undo().unwrap();
        doc.replace_visual(Selection::caret(8), "\n").unwrap();
        assert_eq!(doc.projection().text(), "One\nTwo\n");
        assert_eq!(doc.projection().blocks().last().unwrap().list_depth, 0);
        doc.replace_visual(Selection::caret(8), "Outside").unwrap();
        assert_eq!(doc.projection().blocks().last().unwrap().list_depth, 0);
        while doc.can_undo() {
            doc.undo().unwrap();
        }
        assert_eq!(doc.source(), source);
    }
}

#[test]
fn enter_splits_quoted_paragraphs_and_exits_an_empty_quote() {
    let mut doc = EditorDocument::new("> One", Dialect::Loom).unwrap();
    doc.replace_visual(Selection::caret(3), "\n").unwrap();
    assert_eq!(doc.projection().text(), "One\n");
    assert!(doc.projection().blocks().iter().all(|b| b.quote_depth == 1));
    doc.replace_visual(Selection::caret(4), "Two").unwrap();
    assert_eq!(doc.projection().text(), "One\nTwo");
    doc.undo().unwrap();
    doc.replace_visual(Selection::caret(4), "\n").unwrap();
    assert_eq!(doc.projection().text(), "One\n");
    assert_eq!(doc.projection().blocks().last().unwrap().quote_depth, 0);
}

#[test]
fn nested_list_enter_retains_its_container_and_marks() {
    let mut doc = EditorDocument::new("* One\n  * **Two**", Dialect::Loom).unwrap();
    doc.replace_visual(Selection::caret(7), "\n").unwrap();
    assert_eq!(doc.projection().text(), "One\nTwo\n");
    assert_eq!(doc.projection().blocks().last().unwrap().list_depth, 2);
    doc.replace_visual(Selection::caret(8), "Three").unwrap();
    assert_eq!(doc.projection().blocks().last().unwrap().list_depth, 2);
    doc.undo().unwrap();
    doc.replace_visual(Selection::caret(8), "\n").unwrap();
    assert_eq!(doc.projection().blocks().last().unwrap().list_depth, 1);
}

#[test]
fn tab_sinks_items_and_shift_tab_lifts_them_with_one_atomic_undo() {
    for (source, nested) in [
        ("1. One\n2. Two", "1. One\n   1. Two"),
        ("* One\n* Two", "* One\n  * Two"),
    ] {
        let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
        assert!(
            doc.indent_list(Selection::caret(7), ListIndent::Indent)
                .unwrap()
        );
        assert_eq!(doc.source(), nested);
        assert_eq!(doc.visual_selection().unwrap(), Selection::caret(7));
        assert!(
            doc.indent_list(Selection::caret(7), ListIndent::Outdent)
                .unwrap()
        );
        assert_eq!(doc.source(), source);
        doc.undo().unwrap();
        assert_eq!(doc.source(), nested);
        doc.undo().unwrap();
        assert_eq!(doc.source(), source);
    }
}

#[test]
fn list_indentation_preserves_nested_content_and_multiple_selected_items() {
    let source = "* One\n* Two\n  * nested\n* Three\n* Four";
    let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
    let selection = Selection {
        anchor: 4,
        focus: 20,
    };
    let before = doc.projection().text().to_owned();
    doc.indent_list(selection, ListIndent::Indent).unwrap();
    assert_eq!(doc.projection().text(), before);
    assert_eq!(
        doc.projection()
            .blocks()
            .iter()
            .map(|b| b.list_depth)
            .collect::<Vec<_>>(),
        [1, 2, 3, 2, 1]
    );
    doc.indent_list(selection, ListIndent::Outdent).unwrap();
    assert_eq!(doc.source(), source);
}

#[test]
fn tab_without_a_preceding_list_item_declines_and_top_level_outdent_unwraps() {
    let mut doc = EditorDocument::new("1. One\n2. Two\n3. Three", Dialect::Loom).unwrap();
    assert!(
        !doc.indent_list(Selection::caret(0), ListIndent::Indent)
            .unwrap()
    );
    assert!(!doc.can_undo());
    doc.indent_list(Selection::caret(5), ListIndent::Outdent)
        .unwrap();
    assert_eq!(doc.source(), "1. One\n\nTwo\n\n3. Three");
    let mut doc = EditorDocument::new("Plain", Dialect::Loom).unwrap();
    assert!(
        !doc.indent_list(Selection::caret(3), ListIndent::Indent)
            .unwrap()
    );
    assert!(!doc.can_undo());
}

#[test]
fn toggling_a_nested_container_changes_the_selected_level_only() {
    for (source, format, expected_quotes, expected_lists) in [
        (
            "> One\n>\n> > Two\n> >\n> > Three\n>\n> Four",
            StructureFormat::Quote,
            vec![1, 1, 2, 1],
            vec![0, 0, 0, 0],
        ),
        (
            "* One\n  * Two\n  * Three\n* Four",
            StructureFormat::BulletList,
            vec![0, 0, 0, 0],
            vec![1, 1, 2, 1],
        ),
    ] {
        for newline in ["\n", "\r\n"] {
            let source = source.replace('\n', newline);
            let mut doc = EditorDocument::new(&source, Dialect::Loom).unwrap();
            let before = doc.projection().text().to_owned();
            let selection = Selection::caret(before.find("Two").unwrap() + 1);
            doc.format_structure(selection, format).unwrap();
            assert_eq!(doc.projection().text(), before);
            assert_eq!(
                doc.projection()
                    .blocks()
                    .iter()
                    .map(|b| b.quote_depth)
                    .collect::<Vec<_>>(),
                expected_quotes,
                "{source:?}"
            );
            assert_eq!(
                doc.projection()
                    .blocks()
                    .iter()
                    .map(|b| b.list_depth)
                    .collect::<Vec<_>>(),
                expected_lists,
                "{source:?}"
            );
            assert_eq!(doc.visual_selection().unwrap(), selection);
            doc.undo().unwrap();
            assert_eq!(doc.source(), source);
            doc.redo().unwrap();
            assert_eq!(doc.projection().text(), before);
        }
    }
}

#[test]
fn mixed_nested_selection_lifts_the_wrapper_shared_by_the_whole_selection() {
    let source = "> * > Two\n> * Three\n>\n> Four";
    let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
    let text = doc.projection().text().to_owned();
    let selection = Selection {
        anchor: 0,
        focus: text.find("Three").unwrap() + 5,
    };
    doc.format_structure(selection, StructureFormat::Quote)
        .unwrap();
    assert_eq!(doc.projection().text(), text);
    assert_eq!(
        doc.projection()
            .blocks()
            .iter()
            .map(|b| b.quote_depth)
            .collect::<Vec<_>>(),
        [1, 0, 1]
    );
    doc.undo().unwrap();
    assert_eq!(doc.source(), source);
}

#[test]
fn quote_selection_crossing_container_edges_keeps_unselected_descendants_nested() {
    for source in [
        "> One\n>\n> > Two\n> >\n> > Three\n>\n> Four",
        "> One\n>\n> 7. Two\n> 8. Three\n>\n> Four",
    ] {
        let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
        let text = doc.projection().text().to_owned();
        let before = doc.projection().blocks().to_vec();
        let selection = Selection {
            anchor: 0,
            focus: text.find("Two").unwrap() + 3,
        };
        doc.format_structure(selection, StructureFormat::Quote)
            .unwrap();
        assert_eq!(doc.projection().text(), text);
        for (index, (old, new)) in before.iter().zip(doc.projection().blocks()).enumerate() {
            assert_eq!(
                new.quote_depth,
                old.quote_depth - usize::from(index < 2),
                "block {index} in {source:?}"
            );
            assert_eq!(new.list_depth, old.list_depth);
            assert_eq!(new.marker, old.marker);
        }
        doc.undo().unwrap();
        assert_eq!(doc.source(), source);
    }
}

#[test]
fn quote_lift_never_invents_a_new_list_item_for_an_unselected_continuation() {
    let source = "> One\n>\n> 7. Two\n>\n>    Three";
    let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
    let text = doc.projection().text().to_owned();
    let selection = Selection {
        anchor: 0,
        focus: text.find("Two").unwrap() + 3,
    };
    assert_eq!(
        doc.format_structure(selection, StructureFormat::Quote),
        Err(loom_markdown::Error::UnsupportedEdit)
    );
    assert_eq!(doc.source(), source);
    assert_eq!(doc.projection().text(), text);
    assert_eq!(doc.revision(), 0);
    assert!(!doc.can_undo());
}
