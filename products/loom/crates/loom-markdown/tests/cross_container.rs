use loom_markdown::{Dialect, EditorDocument, Markdown, Selection};

fn assert_same_view(doc: &EditorDocument, source: &str) {
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
fn replacement_across_containers_matches_original_editor_structure() {
    // Semantic results observed with the installed original ProseMirror parser,
    // TextSelection and insertText. Native punctuation can retain source style.
    for (source, start, end, expected) in [
        ("> one\n>\n> two\n\nTail", 1, 5, "> oXwo\n\nTail"),
        ("> one\n\nBody", 2, 5, "> onXody"),
        ("Body\n\n> quote\n>\n> tail", 2, 7, "BoXote\n\n> tail"),
        ("- one\n- two\n- three", 1, 5, "* oXwo\n* three"),
        (
            "- parent\n  - child\n  - next\n- sibling",
            3,
            10,
            "* parXld\n  * next\n* sibling",
        ),
        (
            "- parent\n  - child\n  - next\n- sibling",
            10,
            21,
            "* parent\n  * chiXling",
        ),
        (
            "Body\n\n- one\n\n  continuation\n- two",
            2,
            7,
            "BoXe\n\n* continuation\n\n* two",
        ),
        (
            "- one\n  - child\n- two\n  - tail\n- last",
            1,
            12,
            "* oXo\n  * tail\n* last",
        ),
        (
            "- parent\n  - child\n- sibling\n  - tail\n\n  continuation\n- last",
            10,
            17,
            "* parent\n\n  * chiXing\n\n\n  - tail\n\n  continuation\n\n* last",
        ),
        ("> left\n\n7. right\n8. last", 2, 8, "> leXht\n\n7. last"),
        (
            "1. left\n2. middle\n\n7) right\n8) last",
            2,
            16,
            "1. leXt\n2. last",
        ),
        (
            "> left\n\n1. right\n   * tail\n2. last",
            2,
            8,
            "> leXht\n\n1. * tail\n2. last",
        ),
        ("1. left\n\n> right\n>\n> tail", 2, 8, "1. leXht\n\n> tail"),
    ] {
        for backwards in [false, true] {
            let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
            let selection = if backwards {
                Selection {
                    anchor: end,
                    focus: start,
                }
            } else {
                Selection {
                    anchor: start,
                    focus: end,
                }
            };
            doc.replace_visual(selection, "X")
                .unwrap_or_else(|e| panic!("{source:?}, {start}..{end}: {e}"));
            assert_same_view(&doc, expected);
            assert_eq!(doc.visual_selection().unwrap(), Selection::caret(start + 1));
            doc.undo().unwrap();
            assert_eq!(doc.source(), source);
            assert_eq!(doc.visual_selection().unwrap(), selection);
            doc.redo().unwrap();
            assert_same_view(&doc, expected);
        }
    }
}

#[test]
fn cross_container_replacement_retains_marks_and_unrelated_source() {
    for ending in ["\n", "\r\n", "\r"] {
        let source = "untouched &amp; prefix\n\n> **café**\n>\n> *word*\n\nuntouched &amp; suffix"
            .replace('\n', ending);
        let mut doc = EditorDocument::new(&source, Dialect::Loom).unwrap();
        let start = doc.projection().text().find('é').unwrap();
        let end = doc.projection().text().find("word").unwrap() + 2;
        doc.replace_visual(
            Selection {
                anchor: start,
                focus: end,
            },
            "!",
        )
        .unwrap();
        assert_eq!(
            doc.projection().text(),
            "untouched & prefix\ncaf!rd\nuntouched & suffix"
        );
        assert!(doc.projection().spans().iter().any(|s| s.style.bold));
        assert!(doc.projection().spans().iter().any(|s| s.style.italic));
        assert!(
            doc.source()
                .starts_with(&format!("untouched &amp; prefix{ending}{ending}"))
        );
        assert!(
            doc.source()
                .ends_with(&format!("{ending}{ending}untouched &amp; suffix"))
        );
        doc.undo().unwrap();
        assert_eq!(doc.source(), source);
    }
}

#[test]
fn hidden_reference_definitions_survive_rewriting_their_container() {
    for source in [
        "one\n\n[unused]: https://example.com/unused \"Title\"\n\ntwo\n\nTail",
        "one\n\n[unused]: https://example.com/unused \"Title\"\n\ntwo\n\n[kept][unused]",
        "> one\n>\n> [unused]: https://example.com/unused \"Title\"\n>\n> two\n\nTail",
        "> one\n>\n> [unused]: https://example.com/unused\n>   \"Title\"\n>\n> two\n\n[kept][unused]",
        "> one\n\n[unused]: https://example.com/unused\n\n> two\n\nTail",
    ] {
        let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
        let mut expected = doc.projection().text().to_owned();
        expected.replace_range(1..5, "X");
        doc.replace_visual(
            Selection {
                anchor: 1,
                focus: 5,
            },
            "X",
        )
        .unwrap_or_else(|e| panic!("{source:?}: {e}"));
        assert_eq!(doc.projection().text(), expected);
        assert!(doc.source().contains("[unused]:"));
        assert!(doc.source().contains("https://example.com/unused"));
        if source.ends_with("[kept][unused]") {
            assert!(doc.source().ends_with("[kept][unused]"));
            assert_eq!(
                doc.projection()
                    .spans()
                    .last()
                    .unwrap()
                    .style
                    .link_title
                    .as_deref(),
                Some("Title")
            );
        }
        doc.undo().unwrap();
        assert_eq!(doc.source(), source);
    }
}

#[test]
fn unrepresented_duplicate_definitions_cannot_be_silently_erased() {
    let source = "> one\n>\n> [r]: first\n> [r]: second\n>\n> two\n\nTail";
    let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
    assert_eq!(
        doc.replace_visual(
            Selection {
                anchor: 1,
                focus: 5
            },
            "X"
        ),
        Err(loom_markdown::Error::UnsupportedEdit)
    );
    assert_eq!(doc.source(), source);
    assert!(!doc.can_undo());
}

#[test]
fn deleting_boundaries_and_pasting_multiple_lines_keep_retained_list_content() {
    for (source, selected, replacement, expected) in [
        (
            "- one\n- two\n- three",
            Selection {
                anchor: 4,
                focus: 3,
            },
            "",
            "- onetwo\n- three",
        ),
        (
            "> one\n\nBody",
            Selection {
                anchor: 4,
                focus: 3,
            },
            "",
            "> oneBody",
        ),
        (
            "Body\n\n> quote\n>\n> tail",
            Selection {
                anchor: 5,
                focus: 4,
            },
            "",
            "Bodyquote\n\n> tail",
        ),
        (
            "- one\n  - selected\n- two\n  - tail\n- last",
            Selection {
                anchor: 1,
                focus: 16,
            },
            "A\r\nB\r\nC",
            "- oA\n- B\n- C\n  - tail\n- last",
        ),
        (
            "> one\n>\n> two\n>\n> tail",
            Selection {
                anchor: 1,
                focus: 5,
            },
            "A\nB",
            "> oA\n>\n> Bwo\n>\n> tail",
        ),
        (
            "> one\n>\n> ```\n> selected\n> ```\n>\n> two\n\nTail",
            Selection {
                anchor: 1,
                focus: 15,
            },
            "X",
            "> oXo\n\nTail",
        ),
    ] {
        let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
        doc.replace_visual(selected, replacement)
            .unwrap_or_else(|e| panic!("{source:?}: {e}"));
        assert_same_view(&doc, expected);
        doc.undo().unwrap();
        assert_eq!(doc.source(), source);
        assert_eq!(doc.visual_selection().unwrap(), selected);
    }
}

#[test]
fn refused_cross_container_admission_preserves_history_and_selection() {
    use loom_markdown::{Bias, Error};
    let source = "> one\n\nBody";
    let selection = Selection {
        anchor: 5,
        focus: 2,
    };
    let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
    doc.select_visual(selection, Bias::After).unwrap();
    doc.set_admission_check(|_, projection| {
        if projection.text().contains('X') {
            Err(Error::Limit)
        } else {
            Ok(())
        }
    })
    .unwrap();
    assert_eq!(doc.replace_visual(selection, "X"), Err(Error::Limit));
    assert_eq!(doc.source(), source);
    assert_eq!(doc.visual_selection().unwrap(), selection);
    assert_eq!(doc.revision(), 0);
    assert!(!doc.can_undo());
}

#[test]
fn code_and_prose_boundaries_keep_visible_bytes_in_the_starting_context() {
    use loom_markdown::BlockStyle;
    for (source, start, end, expected, code) in [
        (
            "```rust\ncode\n```\n\n**body**\n\nTail",
            2,
            7,
            "coXdy\nTail",
            true,
        ),
        (
            "Body\n\n```rust\ncode\nnext\n```\n\nTail",
            2,
            7,
            "BoXde\nnext\nTail",
            false,
        ),
        (
            "> Body\r\n>\r\n> ```rust\r\n> code\r\n> next\r\n> ```\r\n\r\nTail",
            2,
            7,
            "BoXde\r\nnext\nTail",
            false,
        ),
        ("    code\n\nBody\n\nTail", 2, 7, "coXdy\nTail", true),
        (
            "```rust\r\ncode\r\nnext\r\n```\r\n\r\nBody\r\n\r\nTail",
            8,
            13,
            "code\r\nneXdy\nTail",
            true,
        ),
    ] {
        let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
        let selection = Selection {
            anchor: start,
            focus: end,
        };
        doc.replace_visual(selection, "X")
            .unwrap_or_else(|e| panic!("{source:?}: {e}"));
        assert_eq!(doc.projection().text(), expected);
        assert_eq!(
            matches!(doc.projection().blocks()[0].style, BlockStyle::Code(_)),
            code
        );
        assert!(doc.source().ends_with("Tail"));
        doc.undo().unwrap();
        assert_eq!(doc.source(), source);
        doc.redo().unwrap();
        assert_eq!(doc.projection().text(), expected);
    }
}
