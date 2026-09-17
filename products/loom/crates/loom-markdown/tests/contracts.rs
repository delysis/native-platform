use loom_markdown::{
    Bias, BlockStyle, Dialect, EditorDocument, Error, Markdown, NodeKind, Selection,
};

#[test]
fn source_round_trips_without_normalizing_unedited_syntax() {
    for source in [
        "A quiet paragraph.",
        "# Heading\n\nA paragraph.",
        "A quiet paragraph.\t",
        "\tA quiet paragraph.",
        "It ",
        "It  ",
        "- A quiet item ",
        "```text\nA\tcode line\n```",
        "~~literal GFM~~",
        "| a | b |\n|---|---|\n| c | d |",
        "line\r\nsecond\rthird\n",
        "<script>alert('inert')</script>",
        "[a][reference]\n\n[reference]: https://example.com 'Title'\n",
        "\\*literal\\* &amp; &#x1f600;",
        "    code\n",
        "",
        "\n\n",
        "**bold**\n\nuntouched  \n",
        "> quote\n> continuation\n",
    ] {
        for dialect in [Dialect::Loom, Dialect::CommonMark] {
            let doc = Markdown::parse(source, dialect).unwrap();
            let projection = doc.project().unwrap();
            assert_eq!(doc.source(), source);
            for span in projection.spans() {
                assert!(source.is_char_boundary(span.source.start));
                assert!(source.is_char_boundary(span.source.end));
                assert!(projection.text().is_char_boundary(span.display.start));
                assert!(projection.text().is_char_boundary(span.display.end));
            }
        }
    }
}

#[test]
fn native_projection_resolves_styles_entities_and_original_byte_boundaries() {
    let doc = Markdown::parse("# Title\n\nHello **bold** &amp; 😀.", Dialect::Loom).unwrap();
    let view = doc.project().unwrap();
    assert_eq!(view.text(), "Title\nHello bold & 😀.");
    assert_eq!(view.blocks()[0].style, BlockStyle::Heading(1));
    let bold = view.spans().iter().find(|s| s.style.bold).unwrap();
    assert_eq!(&view.text()[bold.display.clone()], "bold");
    assert_eq!(&doc.source()[bold.source.clone()], "bold");
    assert_eq!(
        view.source_at(bold.display.end, Bias::Before).unwrap(),
        bold.source.end
    );
    assert_eq!(
        view.source_at(bold.display.end, Bias::After).unwrap(),
        bold.source.end + 2
    );
    let emoji = view.text().find('😀').unwrap();
    assert_eq!(
        view.source_at(emoji, Bias::After).unwrap(),
        doc.source().find('😀').unwrap()
    );
    assert_eq!(
        view.source_at(emoji + 1, Bias::After),
        Err(Error::InvalidRange)
    );
}

#[test]
fn leading_tabs_remain_manuscript_indentation_in_the_loom_dialect() {
    let source = "\tA\tquiet café.\t";
    let doc = Markdown::parse(source, Dialect::Loom).unwrap();
    let view = doc.project().unwrap();
    assert_eq!(view.text(), source);
    for offset in source.char_indices().map(|(i, _)| i).chain([source.len()]) {
        assert_eq!(view.source_at(offset, Bias::After).unwrap(), offset);
    }
    assert!(
        Markdown::parse(source, Dialect::CommonMark)
            .unwrap()
            .nodes()
            .iter()
            .any(|n| matches!(n.kind, NodeKind::CodeBlock { .. }))
    );
}

#[test]
fn lists_quotes_code_links_and_images_have_native_semantics() {
    let source = "> Quoted *words*.\n\n1. First\n2. Second\n   * Nested\n\n```rust\nlet a = 1;\n```\n\n[a](https://example.com) ![alt](assets/a.png)";
    let doc = Markdown::parse(source, Dialect::Loom).unwrap();
    let view = doc.project().unwrap();
    assert_eq!(view.blocks()[0].quote_depth, 1);
    assert!(
        view.blocks()
            .iter()
            .any(|b| b.marker.as_deref() == Some("2."))
    );
    assert!(
        view.blocks()
            .iter()
            .any(|b| b.list_depth == 2 && b.marker.as_deref() == Some("•"))
    );
    assert!(
        view.blocks()
            .iter()
            .any(|b| b.style == BlockStyle::Code("rust".into()))
    );
    assert!(
        view.spans()
            .iter()
            .any(|s| s.style.link.as_deref() == Some("https://example.com"))
    );
    assert!(view.text().contains('\u{fffc}'));
}

#[test]
fn transactions_are_atomic_and_bound_to_an_exact_editor_revision() {
    let source = "café **bold**\r\n\r\nuntouched  ";
    let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
    let mut stale = doc.transaction();
    stale.replace(0..0, "stale").unwrap();
    let mut edit = doc.transaction();
    edit.replace(0..5, "tea").unwrap();
    assert!(doc.apply(edit).unwrap());
    assert_eq!(doc.source(), "tea **bold**\r\n\r\nuntouched  ");
    assert_eq!(doc.apply(stale.clone()), Err(Error::StaleTransaction));
    let mut other = EditorDocument::new(source, Dialect::Loom).unwrap();
    assert_eq!(other.apply(stale), Err(Error::StaleTransaction));
    doc.undo().unwrap();
    assert_eq!(doc.source(), source);
    doc.redo().unwrap();
    assert_eq!(doc.source(), "tea **bold**\r\n\r\nuntouched  ");
}

#[test]
fn rejected_edits_leave_selection_source_and_history_unchanged() {
    let mut doc = EditorDocument::new("café e\u{301} 👩🏽‍🚀\r\n", Dialect::Loom).unwrap();
    doc.select(Selection::caret(doc.source().len())).unwrap();
    for range in [4..5, 6..7, 9..13, 0..999] {
        let before = doc.source().to_owned();
        let selection = doc.source_selection();
        let mut t = doc.transaction();
        t.replace(range, "x").unwrap();
        assert_eq!(doc.apply(t), Err(Error::InvalidRange));
        assert_eq!(doc.source(), before);
        assert_eq!(doc.source_selection(), selection);
        assert!(!doc.can_undo());
    }
    let mut t = doc.transaction();
    t.replace(0..3, "x").unwrap();
    t.replace(1..5, "y").unwrap();
    assert_eq!(doc.apply(t), Err(Error::OverlappingEdits));
}

#[test]
fn undo_reverses_grapheme_merges_and_adjacent_deletions_exactly() {
    let mut doc = EditorDocument::new("e", Dialect::Loom).unwrap();
    doc.select(Selection::caret(1)).unwrap();
    doc.replace_selection("\u{301}").unwrap();
    assert_eq!(doc.source(), "e\u{301}");
    doc.undo().unwrap();
    assert_eq!(doc.source(), "e");
    doc.redo().unwrap();
    assert_eq!(doc.source(), "e\u{301}");
    let mut doc = EditorDocument::new("abc", Dialect::Loom).unwrap();
    let mut t = doc.transaction();
    t.replace(0..1, "").unwrap();
    t.replace(1..2, "").unwrap();
    doc.apply(t).unwrap();
    assert_eq!(doc.source(), "c");
    doc.undo().unwrap();
    assert_eq!(doc.source(), "abc");
    doc.redo().unwrap();
    assert_eq!(doc.source(), "c");
}
