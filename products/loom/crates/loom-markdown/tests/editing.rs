use loom_markdown::{
    BlockStyle, Dialect, EditorDocument, InlineFormat, ParagraphFormat, Selection,
};
fn editor(text: &str) -> EditorDocument {
    EditorDocument::new(text, Dialect::Loom).unwrap()
}
fn all(doc: &EditorDocument) -> Selection {
    Selection {
        anchor: 0,
        focus: doc.projection().text().len(),
    }
}

#[test]
fn headings_and_inline_toggles_preserve_unselected_source_bytes() {
    for (format, expected) in [
        (InlineFormat::Bold, "**Words**"),
        (InlineFormat::Italic, "*Words*"),
    ] {
        let mut doc = editor("Words\r\n\r\n  untouched &amp; bytes\t ");
        let selected = Selection {
            anchor: 0,
            focus: 5,
        };
        doc.format_inline(selected, &format).unwrap();
        assert_eq!(
            doc.source(),
            format!("{expected}\r\n\r\n  untouched &amp; bytes\t ")
        );
        doc.format_inline(selected, &format).unwrap();
        assert_eq!(doc.source(), "Words\r\n\r\n  untouched &amp; bytes\t ");
    }
    let mut doc = editor("Words");
    doc.format_paragraph(Selection::caret(3), ParagraphFormat::Heading(2))
        .unwrap();
    assert_eq!(doc.source(), "## Words");
    assert_eq!(doc.projection().blocks()[0].style, BlockStyle::Heading(2));
    doc.format_paragraph(all(&doc), ParagraphFormat::Body)
        .unwrap();
    assert_eq!(doc.source(), "Words");
}

#[test]
fn terminal_whitespace_stays_outside_marks_and_links() {
    for format in [
        InlineFormat::Bold,
        InlineFormat::Italic,
        InlineFormat::Link("https://example.com".into()),
    ] {
        let mut doc = editor(" Words ");
        doc.format_inline(all(&doc), &format).unwrap();
        assert_eq!(doc.projection().text(), "Words ");
        assert!(doc.source().ends_with(' '));
        assert!(!doc.projection().spans().last().unwrap().style.bold);
    }
}

#[test]
fn replacing_across_an_inline_boundary_does_not_leave_dangling_delimiters() {
    let mut doc = editor("**bold** more\n\nuntouched");
    doc.replace_visual(
        Selection {
            anchor: 1,
            focus: 7,
        },
        "X",
    )
    .unwrap();
    assert_eq!(doc.projection().text(), "bXre\nuntouched");
    assert!(doc.source().ends_with("\n\nuntouched"));
    doc.undo().unwrap();
    assert_eq!(doc.source(), "**bold** more\n\nuntouched");
}

#[test]
fn visual_edits_decode_entities_without_rewriting_another_paragraph() {
    let mut doc = editor("A &amp; B\n\nC &amp; D");
    doc.replace_visual(
        Selection {
            anchor: 2,
            focus: 3,
        },
        "and",
    )
    .unwrap();
    assert_eq!(doc.projection().text(), "A and B\nC & D");
    assert!(doc.source().ends_with("\n\nC &amp; D"));
    doc.undo().unwrap();
    assert_eq!(doc.source(), "A &amp; B\n\nC &amp; D");
}

#[test]
fn links_images_and_nested_styles_survive_an_edit_in_the_same_paragraph() {
    let mut doc = editor(
        "[Link](https://example.com \"Title\") ![alt](assets/image.png) **strong *emphasis*** tail",
    );
    let end = doc.projection().text().len();
    doc.replace_visual(
        Selection {
            anchor: end - 4,
            focus: end,
        },
        "ending",
    )
    .unwrap();
    assert!(doc.source().contains("Title"));
    assert!(doc.source().contains("![alt](assets/image.png)"));
    assert!(
        doc.projection()
            .spans()
            .iter()
            .any(|s| s.style.bold && s.style.italic)
    );
    assert!(doc.projection().text().ends_with("ending"));
}

#[test]
fn formatting_keeps_list_and_quote_containers() {
    for source in ["* one\n* two", "> one\n> two", "1. one\n   1. two"] {
        let mut doc = editor(source);
        let display = doc.projection().text().to_owned();
        doc.format_inline(all(&doc), &InlineFormat::Bold).unwrap();
        assert_eq!(doc.projection().text(), display);
        doc.undo().unwrap();
        assert_eq!(doc.source(), source);
    }
}

#[test]
fn arming_a_mark_at_the_caret_formats_subsequent_text() {
    let mut doc = editor("Words");
    doc.format_inline(Selection::caret(5), &InlineFormat::Bold)
        .unwrap();
    doc.replace_visual(Selection::caret(5), " ").unwrap();
    doc.replace_visual(Selection::caret(6), "more").unwrap();
    assert_eq!(doc.source(), "Words **more**");
}

#[test]
fn visual_caret_inside_an_entity_is_editable_but_not_a_source_authority() {
    use loom_markdown::{Bias, Error};
    let mut doc = editor("&fjlig;");
    doc.select_visual(Selection::caret(1), Bias::After).unwrap();
    assert_eq!(doc.source_selection(), Err(Error::AmbiguousBoundary));
    doc.replace_visual(Selection::caret(1), "X").unwrap();
    assert_eq!(doc.source(), "fXj");
    doc.undo().unwrap();
    assert_eq!(doc.source(), "&fjlig;");
    assert_eq!(doc.visual_selection().unwrap(), Selection::caret(1));
    assert_eq!(doc.source_selection(), Err(Error::AmbiguousBoundary));
    doc.redo().unwrap();
    assert_eq!(doc.visual_selection().unwrap(), Selection::caret(2));
}

#[test]
fn undo_restores_selection_direction_and_pre_deletion_caret() {
    let mut doc = editor("one\n\ntwo");
    doc.delete_visual(Selection::caret(4), 3..4).unwrap();
    doc.undo().unwrap();
    assert_eq!(doc.visual_selection().unwrap(), Selection::caret(4));
    let reversed = Selection {
        anchor: 7,
        focus: 1,
    };
    doc.replace_visual(reversed, "X").unwrap();
    doc.undo().unwrap();
    assert_eq!(doc.visual_selection().unwrap(), reversed);
}
