use loom_markdown::{Bias, BlockStyle, Dialect, EditorDocument, Markdown, Selection};

#[test]
fn visual_code_edits_preserve_literal_bytes_and_container_structure() {
    for source in [
        "```rust\nA\tβ\nB\n```\n\nuntouched &amp; bytes",
        "> ```rust\r\n> A\tβ\r\n> B\r\n> ```\r\n\r\nuntouched &amp; bytes",
        "> * ```rust\r\n>   A\tβ\r\n>   B\r\n>   ```\r\n\r\nuntouched &amp; bytes",
        "  ~~~~rust\n  A\tβ\n  B\n  ~~~~\n\nuntouched &amp; bytes",
        "    A\tβ\n    B\n\nuntouched &amp; bytes",
        ">     A\tβ\r\n>     B\r\n\r\nuntouched &amp; bytes",
        "*     A\tβ\r\n      B\r\n\r\nuntouched &amp; bytes",
        "> ```rust\n> A\tβ\n> B\n\nuntouched &amp; bytes",
    ] {
        let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
        let before = doc.projection().text().to_owned();
        let offset = before.find('β').unwrap();
        let selected = Selection {
            anchor: offset + 'β'.len_utf8(),
            focus: offset,
        };
        let inserted = "cafe\u{301}\r\n```\r<raw>&#9;\n\t";
        doc.replace_visual(selected, inserted)
            .unwrap_or_else(|e| panic!("{source:?}: {e}"));
        let mut expected = before;
        expected.replace_range(selected.range(), inserted);
        assert_eq!(doc.projection().text(), expected);
        assert!(doc.source().ends_with("untouched &amp; bytes"));
        assert_eq!(
            doc.visual_selection().unwrap(),
            Selection::caret(offset + inserted.len())
        );
        doc.undo().unwrap();
        assert_eq!(doc.source(), source);
        assert_eq!(doc.visual_selection().unwrap(), selected);
        doc.redo().unwrap();
        assert_eq!(doc.projection().text(), expected);
    }
}

#[test]
fn structural_code_edits_preserve_mixed_endings_and_authored_blank_lines() {
    use loom_markdown::StructureFormat;
    for source in [
        "```rust\r\nA\rB\nC\r\n\r\n```",
        "~~~language`with`backticks &amp; text\nA\n\n~~~",
        "```language&#96;with&#96;backticks\nA\n```",
        "```\n```",
    ] {
        for format in [
            StructureFormat::Quote,
            StructureFormat::BulletList,
            StructureFormat::OrderedList,
        ] {
            let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
            let before = doc.projection().text().to_owned();
            let style = doc.projection().blocks()[0].style.clone();
            doc.format_structure(
                Selection {
                    anchor: 0,
                    focus: before.len(),
                },
                format,
            )
            .unwrap_or_else(|e| panic!("{source:?}: {e}"));
            assert_eq!(doc.projection().text(), before);
            assert_eq!(doc.projection().blocks()[0].style, style);
            doc.undo().unwrap();
            assert_eq!(doc.source(), source);
        }
    }
}

#[test]
fn code_endings_do_not_merge_into_structural_fence_endings() {
    for source in ["```\n```", "```\r```", "> ```\r\n> ```"] {
        for content in ["\r", "\n", "\r\n", "\nA\r", "\rA\n", "\n\r"] {
            let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
            doc.replace_visual(Selection::caret(0), content)
                .unwrap_or_else(|e| panic!("{source:?}, {content:?}: {e}"));
            assert_eq!(doc.projection().text(), content);
            doc.undo().unwrap();
            assert_eq!(doc.source(), source);
        }
    }
}

#[test]
fn code_expansion_limits_refuse_atomically_before_publishing_source_or_history() {
    use loom_markdown::Error;
    let source = "> ```\n> code\n> ```";
    let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
    let selected = Selection {
        anchor: 4,
        focus: 0,
    };
    for content in ["`".repeat(400_000), "\n".repeat(400_000)] {
        assert_eq!(doc.replace_visual(selected, &content), Err(Error::Limit));
        assert_eq!(doc.source(), source);
        assert!(!doc.can_undo());
        assert_eq!(doc.revision(), 0);
    }
    doc.set_admission_check(|_, p| {
        if p.text().contains('!') {
            Err(Error::Limit)
        } else {
            Ok(())
        }
    })
    .unwrap();
    assert_eq!(doc.replace_visual(selected, "!"), Err(Error::Limit));
    assert_eq!(doc.source(), source);
    assert!(!doc.can_undo());
}

#[test]
fn code_fence_line_break_is_syntax_and_empty_code_has_a_content_caret() {
    for (source, expected) in [
        ("```\nA\n```", "A"),
        ("```\r\nA\r\n\r\n```", "A\r\n"),
        ("```\n```", ""),
        ("> ```\r\n> ```", ""),
        ("```\nA", "A"),
    ] {
        let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
        assert_eq!(doc.projection().text(), expected, "{source:?}");
        let end = doc.projection().text().len();
        doc.type_visual(Selection::caret(end), "**literal**\t")
            .unwrap();
        assert_eq!(doc.projection().text(), format!("{expected}**literal**\t"));
        assert!(matches!(
            doc.projection().blocks()[0].style,
            BlockStyle::Code(_)
        ));
        doc.replace_visual(
            Selection {
                anchor: 0,
                focus: doc.projection().text().len(),
            },
            "",
        )
        .unwrap();
        assert_eq!(doc.projection().text(), "");
        let body = doc.projection().source_at(0, Bias::After).unwrap();
        assert!(doc.source()[body..].starts_with('`') || doc.source()[body..].starts_with('>'));
        assert!(body > doc.source().find('\n').unwrap());
        doc.undo().unwrap();
        doc.undo().unwrap();
        assert_eq!(doc.source(), source);
    }
}

#[test]
fn code_projection_preserves_content_and_endings_without_container_prefixes() {
    for (source, expected) in [
        ("```rust\nA\tβ\nB\n```", "A\tβ\nB"),
        ("> ```rust\n> A\tβ\n> B\n> ```", "A\tβ\nB"),
        ("> * ```rust\n>   A\tβ\n>   B\n>   ```", "A\tβ\nB"),
        ("```rust\r\nA\tβ\r\nB\r\n```", "A\tβ\r\nB"),
        ("> ```rust\r\n> A\tβ\r\n> B\r\n> ```", "A\tβ\r\nB"),
        ("> * ```rust\r\n>   A\tβ\r\n>   B\r\n>   ```", "A\tβ\r\nB"),
        ("```rust\r\n\r\nA\rB\nC\r\n```", "\r\nA\rB\nC"),
        ("```rust\r\nA\tβ", "A\tβ"),
    ] {
        let markdown = Markdown::parse(source, Dialect::Loom).unwrap();
        let projection = markdown.project().unwrap();
        let code = projection
            .blocks()
            .iter()
            .find(|block| matches!(block.style, BlockStyle::Code(_)))
            .unwrap();
        assert_eq!(
            &projection.text()[code.display.clone()],
            expected,
            "{source:?}: {:?}",
            markdown.nodes()
        );
        assert_eq!(markdown.source(), source);
        if let Some(offset) = expected.find('B') {
            let display = code.display.start + offset;
            let source_byte = source.find('B').unwrap();
            assert_eq!(
                projection.source_at(display, Bias::After).unwrap(),
                source_byte
            );
            assert_eq!(
                projection.source_at(display + 1, Bias::Before).unwrap(),
                source_byte + 1
            );
        }
    }
}
