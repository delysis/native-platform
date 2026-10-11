use loom_markdown::{Bias, BlockStyle, Dialect, EditorDocument, Selection};

#[test]
fn empty_paragraphs_have_distinct_reversible_caret_positions() {
    for newline in ["\n", "\r\n", "\r"] {
        let pair = newline.repeat(2);
        for (source, text, offsets) in [
            (String::new(), "", vec![0]),
            (pair.clone(), "\n", vec![0, pair.len()]),
            (pair.repeat(2), "\n\n", vec![0, pair.len(), pair.len() * 2]),
            (format!("{pair}two"), "\ntwo", vec![0]),
            (format!("one{pair}"), "one\n", vec![3 + pair.len()]),
            (
                format!("one{pair}{pair}two"),
                "one\n\ntwo",
                vec![3 + pair.len()],
            ),
        ] {
            let doc = EditorDocument::new(&source, Dialect::Loom).unwrap();
            assert_eq!(doc.source(), source);
            assert_eq!(doc.projection().text(), text, "{source:?}");
            let empties = doc
                .projection()
                .blocks()
                .iter()
                .filter(|b| b.display.is_empty())
                .collect::<Vec<_>>();
            assert_eq!(
                empties
                    .iter()
                    .map(|b| b.empty_caret_source)
                    .collect::<Vec<_>>(),
                offsets,
                "{source:?}"
            );
            for block in empties {
                assert_eq!(
                    doc.projection()
                        .source_at(block.display.start, Bias::Before)
                        .unwrap(),
                    block.empty_caret_source
                );
                assert_eq!(
                    doc.projection()
                        .display_at(block.empty_caret_source, Bias::After)
                        .unwrap(),
                    block.display.start
                );
            }
        }
    }
}

#[test]
fn enter_splits_styled_prose_and_undo_restores_exact_source() {
    for source in [
        "**one two**",
        "# one two",
        "one two\r\n\r\nuntouched &amp; bytes",
    ] {
        let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
        let before = doc.projection().text().to_owned();
        doc.replace_visual(Selection::caret(4), "\n")
            .unwrap_or_else(|e| panic!("{source:?}: {e}"));
        let mut expected = before;
        expected.insert(4, '\n');
        assert_eq!(doc.projection().text(), expected, "{source:?}");
        assert_eq!(doc.projection().blocks()[1].style, BlockStyle::Body);
        assert_eq!(doc.visual_selection().unwrap(), Selection::caret(5));
        if source.ends_with("bytes") {
            assert!(doc.source().ends_with("\r\n\r\nuntouched &amp; bytes"));
        }
        doc.undo().unwrap();
        assert_eq!(doc.source(), source);
        doc.redo().unwrap();
        assert_eq!(doc.projection().text(), expected);
    }
}

#[test]
fn enter_at_empty_document_and_edges_leaves_a_typable_paragraph() {
    for (source, at, expected) in [("", 0, "\n"), ("one", 3, "one\n"), ("one", 0, "\none")] {
        let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
        doc.replace_visual(Selection::caret(at), "\n").unwrap();
        assert_eq!(doc.projection().text(), expected);
        doc.replace_visual(Selection::caret(at + 1), "more")
            .unwrap();
        let mut expected = expected.to_owned();
        expected.insert_str(at + 1, "more");
        assert_eq!(doc.projection().text(), expected);
        doc.undo().unwrap();
        doc.undo().unwrap();
        assert_eq!(doc.source(), source);
    }
}

#[test]
fn deleting_a_paragraph_boundary_joins_marks_without_rewriting_neighbors() {
    for newline in ["\n", "\r\n", "\r"] {
        let source =
            format!("**one**{newline}{newline}*two*{newline}{newline}untouched &amp; bytes");
        let mut doc = EditorDocument::new(&source, Dialect::Loom).unwrap();
        doc.replace_visual(
            Selection {
                anchor: 4,
                focus: 3,
            },
            "",
        )
        .unwrap();
        assert_eq!(doc.projection().text(), "onetwo\nuntouched & bytes");
        assert!(
            doc.source()
                .ends_with(&format!("{newline}{newline}untouched &amp; bytes"))
        );
        assert!(doc.projection().spans()[0].style.bold);
        assert!(doc.projection().spans()[1].style.italic);
        doc.undo().unwrap();
        assert_eq!(doc.source(), source);
    }
}

#[test]
fn multiline_paste_and_select_all_clear_have_one_atomic_undo() {
    let source = "first\n\nsecond\n\nthird";
    let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
    doc.replace_visual(
        Selection {
            anchor: 2,
            focus: 15,
        },
        "new\r\n\r\nparagraph",
    )
    .unwrap();
    assert_eq!(doc.projection().text(), "finew\n\nparagraphird");
    doc.undo().unwrap();
    assert_eq!(doc.source(), source);
    for original in [
        source,
        "* one\n* two",
        "![alt](x.png)",
        "[text][ref]\n\n[ref]: https://example.com",
    ] {
        let mut doc = EditorDocument::new(original, Dialect::Loom).unwrap();
        let end = doc.projection().text().len();
        doc.replace_visual(
            Selection {
                anchor: 0,
                focus: end,
            },
            "",
        )
        .unwrap();
        assert_eq!(doc.source(), "");
        doc.undo().unwrap();
        assert_eq!(doc.source(), original);
    }
}

#[test]
fn quoted_empty_paragraphs_keep_carets_after_the_source_prefix() {
    for (source, expected) in [
        ("> ", ""),
        ("> one\n>\n> ", "one\n"),
        ("> one\n>\n>\n>\n> two", "one\n\ntwo"),
        ("> \r\n>\r\n> ", "\n"),
    ] {
        let doc = EditorDocument::new(source, Dialect::Loom).unwrap();
        assert_eq!(doc.projection().text(), expected, "{source:?}");
        for block in doc.projection().blocks() {
            assert_eq!(block.quote_depth, 1);
            if block.display.is_empty() {
                let offset = doc
                    .projection()
                    .source_at(block.display.start, Bias::Before)
                    .unwrap();
                assert_eq!(
                    source[..offset].rsplit(['\r', '\n']).next().unwrap().trim(),
                    ">"
                );
            }
        }
    }
}
