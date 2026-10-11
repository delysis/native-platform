use loom_markdown::{Dialect, EditorDocument, Selection};

#[test]
fn literal_typing_remains_visible_across_markdown_delimiters_and_unicode() {
    for text in [
        "# Title",
        "##",
        "> quote",
        "- item",
        "1. item",
        "**bold**",
        "_em_",
        "[link](url)",
        "`code`",
        "<tag>",
        "A & B",
        "\\escaped",
        " two spaces  ",
        "\tindent",
        "café العربية 日本語 👩🏽‍🚀",
        "a\u{301}b",
    ] {
        let mut doc = EditorDocument::new("", Dialect::Loom).unwrap();
        let mut expected = String::new();
        for c in text.chars() {
            let caret = doc.projection().text().len();
            let typed = c.to_string();
            doc.replace_visual(Selection::caret(caret), &typed)
                .unwrap_or_else(|e| panic!("{text:?} after {expected:?}, inserting {c:?}: {e}"));
            expected.push(c);
            assert_eq!(doc.projection().text(), expected);
        }
        while doc.can_undo() {
            doc.undo().unwrap();
        }
        assert_eq!(doc.source(), "");
    }
}

#[test]
fn empty_deletions_do_not_erase_hidden_source() {
    for source in ["[ref]: https://example.com", "#", "\n", ""] {
        let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
        doc.delete_visual(Selection::caret(0), 0..0).unwrap();
        assert_eq!(doc.source(), source);
        assert!(!doc.can_undo());
    }
}

#[test]
fn typing_after_hidden_definitions_preserves_them_and_never_interprets_delimiters() {
    for source in [
        "[ref]: https://example.com",
        "[ref]: <path>\r\n",
        "[ref]: path\n\n",
    ] {
        for text in ["*literal*", "one\ntwo", "#", "\n"] {
            let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
            assert!(doc.projection().text().is_empty());
            assert_eq!(doc.source_selection().unwrap().focus, 0);
            doc.replace_visual(Selection::caret(0), text)
                .unwrap_or_else(|e| panic!("{source:?} + {text:?}: {e}"));
            assert_eq!(doc.projection().text(), text);
            assert!(doc.source().starts_with(source));
            doc.undo().unwrap();
            assert_eq!(doc.source(), source);
        }
        let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
        doc.format_paragraph(
            Selection::caret(0),
            loom_markdown::ParagraphFormat::Heading(1),
        )
        .unwrap();
        assert!(doc.source().starts_with(source));
        assert_eq!(
            doc.projection().blocks()[0].style,
            loom_markdown::BlockStyle::Heading(1)
        );
    }
}
