use loom_markdown::{BlockStyle, Dialect, EditorDocument, Error, InlineFormat, Selection};

fn type_text(doc: &mut EditorDocument, text: &str) {
    for c in text.chars() {
        let selection = doc.visual_selection().unwrap();
        doc.type_visual(selection, &c.to_string())
            .unwrap_or_else(|e| panic!("typing {c:?} in {:?}: {e}", doc.source()));
    }
}

#[test]
fn native_block_rules_match_the_original_heading_quote_and_list_patterns() {
    for (input, source, style, quote, list, marker) in [
        ("# Heading", "# Heading", BlockStyle::Heading(1), 0, 0, None),
        (
            "## Subhead",
            "## Subhead",
            BlockStyle::Heading(2),
            0,
            0,
            None,
        ),
        ("### Third", "### Third", BlockStyle::Heading(3), 0, 0, None),
        ("> Quote", "> Quote", BlockStyle::Body, 1, 0, None),
        ("* Item", "* Item", BlockStyle::Body, 0, 1, Some("•")),
        ("- Item", "* Item", BlockStyle::Body, 0, 1, Some("•")),
        ("+ Item", "* Item", BlockStyle::Body, 0, 1, Some("•")),
        ("1. Item", "1. Item", BlockStyle::Body, 0, 1, Some("1.")),
        ("37. Item", "37. Item", BlockStyle::Body, 0, 1, Some("37.")),
        ("0. Item", "0. Item", BlockStyle::Body, 0, 1, Some("0.")),
    ] {
        let mut doc = EditorDocument::new("", Dialect::Loom).unwrap();
        type_text(&mut doc, input);
        assert_eq!(doc.source(), source);
        let block = &doc.projection().blocks()[0];
        assert_eq!(
            (
                block.style.clone(),
                block.quote_depth,
                block.list_depth,
                block.marker.as_deref()
            ),
            (style, quote, list, marker)
        );
        while doc.can_undo() {
            doc.undo().unwrap();
        }
        assert_eq!(doc.source(), "");
    }
}

#[test]
fn native_inline_rules_preserve_unicode_and_stop_new_marks_after_the_pattern() {
    for (input, text, bold, italic, link) in [
        ("This is **café**.", "This is café.", true, false, None),
        ("This is __café__.", "This is café.", true, false, None),
        ("This is *café*.", "This is café.", false, true, None),
        ("This is _café_.", "This is café.", false, true, None),
        (
            "[café](https://example.com).",
            "café.",
            false,
            false,
            Some("https://example.com"),
        ),
    ] {
        let mut doc = EditorDocument::new("", Dialect::Loom).unwrap();
        type_text(&mut doc, input);
        assert_eq!(doc.projection().text(), text, "{input}");
        let span = doc
            .projection()
            .spans()
            .iter()
            .find(|span| doc.projection().text()[span.display.clone()].contains("café"))
            .unwrap();
        assert_eq!(
            (
                span.style.bold,
                span.style.italic,
                span.style.link.as_deref()
            ),
            (bold, italic, link)
        );
        let last = doc.projection().spans().last().unwrap();
        assert!(!last.style.bold && !last.style.italic && last.style.link.is_none());
    }
}

#[test]
fn paste_escaped_patterns_and_code_remain_literal() {
    for text in ["# Heading", "> Quote", "* Item", "**bold**", "[link](url)"] {
        let mut doc = EditorDocument::new("", Dialect::Loom).unwrap();
        doc.replace_visual(Selection::caret(0), text).unwrap();
        assert_eq!(doc.projection().text(), text);
    }
    for text in [
        r"\*literal\*",
        r"\**literal**",
        r"\[link](url)",
        "![image](url)",
        "#### Literal",
        "1000000000. Literal",
    ] {
        let mut doc = EditorDocument::new("", Dialect::Loom).unwrap();
        type_text(&mut doc, text);
        assert_eq!(doc.projection().text(), text);
    }
    let mut code = EditorDocument::new("`**word*`", Dialect::Loom).unwrap();
    code.select_visual(Selection::caret(7), loom_markdown::Bias::After)
        .unwrap();
    code.type_visual(Selection::caret(7), "*").unwrap();
    assert_eq!(code.projection().text(), "**word**");
    assert!(
        code.projection()
            .spans()
            .iter()
            .all(|span| span.style.code && !span.style.bold)
    );
}

#[test]
fn rule_and_trigger_are_one_undo_and_failed_admission_is_atomic() {
    let mut doc = EditorDocument::new("", Dialect::Loom).unwrap();
    type_text(&mut doc, "**word*");
    let before = doc.source().to_owned();
    let selection = doc.visual_selection().unwrap();
    let revision = doc.revision();
    doc.type_visual(selection, "*").unwrap();
    assert_eq!(doc.projection().text(), "word");
    assert_eq!(doc.revision(), revision + 1);
    doc.undo().unwrap();
    assert_eq!(doc.source(), before);
    assert_eq!(doc.visual_selection().unwrap(), selection);
    doc.redo().unwrap();
    assert_eq!(doc.projection().text(), "word");
    doc.undo().unwrap();
    doc.set_admission_check(|_, projection| {
        if projection.spans().iter().any(|s| s.style.bold) {
            Err(Error::Limit)
        } else {
            Ok(())
        }
    })
    .unwrap();
    assert_eq!(doc.type_visual(selection, "*"), Err(Error::Limit));
    assert_eq!(doc.source(), before);
    assert_eq!(doc.visual_selection().unwrap(), selection);
    assert!(doc.can_redo());
}

#[test]
fn committed_multi_character_input_and_inherited_marks_remain_atomic() {
    for input in ["New **café**", "**café**"] {
        let mut doc = EditorDocument::new("", Dialect::Loom).unwrap();
        doc.format_inline(Selection::caret(0), &InlineFormat::Italic)
            .unwrap();
        doc.type_visual(Selection::caret(0), input).unwrap();
        assert_eq!(doc.projection().text(), input.replace("**", ""));
        assert!(doc.projection().spans().iter().all(|s| s.style.italic));
        doc.type_visual(doc.visual_selection().unwrap(), "!")
            .unwrap();
        assert!(!doc.projection().spans().last().unwrap().style.bold);
        assert!(doc.projection().spans().last().unwrap().style.italic);
        doc.undo().unwrap();
        doc.undo().unwrap();
        assert_eq!(doc.source(), "");
    }
}

#[test]
fn ordered_input_joins_only_the_correct_preceding_sequence() {
    for (input, marker, roots) in [("3. Three", "3.", 1), ("7. Seven", "7.", 2)] {
        let mut doc = EditorDocument::new("1. One\n2. Two\n\n", Dialect::Loom).unwrap();
        doc.select_visual(
            Selection::caret(doc.projection().text().len()),
            loom_markdown::Bias::After,
        )
        .unwrap();
        let (prefix, rest) = input.split_once(' ').unwrap();
        type_text(&mut doc, &format!("{prefix} "));
        assert_eq!(
            doc.projection().blocks().last().unwrap().marker.as_deref(),
            Some(marker),
            "at marker: {}",
            doc.source()
        );
        type_text(&mut doc, rest);
        assert_eq!(
            doc.projection().blocks().last().unwrap().marker.as_deref(),
            Some(marker),
            "{}",
            doc.source()
        );
        assert_eq!(
            doc.markdown().nodes()[0].children.len(),
            roots,
            "{}",
            doc.source()
        );
    }
}

#[test]
fn rules_inside_existing_containers_and_after_hidden_definitions_keep_context() {
    for (source, input, depth, heading) in [
        ("> Parent\n>\n> ", "> Child", 2, false),
        ("> Parent\n>\n> ", "# Child", 1, true),
        ("[ref]: https://example.com\r\n", "# Child", 0, true),
    ] {
        let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
        doc.select_visual(
            Selection::caret(doc.projection().text().len()),
            loom_markdown::Bias::After,
        )
        .unwrap();
        type_text(&mut doc, input);
        let last = doc.projection().blocks().last().unwrap();
        assert_eq!(last.quote_depth, depth);
        assert_eq!(matches!(last.style, BlockStyle::Heading(1)), heading);
        if source.starts_with("[ref]") {
            assert!(doc.source().starts_with(source));
        }
        while doc.can_undo() {
            doc.undo().unwrap();
        }
        assert_eq!(doc.source(), source);
    }
}

#[test]
fn selected_text_direction_and_long_context_are_preserved_on_undo() {
    let mut doc = EditorDocument::new("", Dialect::Loom).unwrap();
    doc.replace_visual(Selection::caret(0), "**old*tail")
        .unwrap();
    let source = doc.source().to_owned();
    let selection = Selection {
        anchor: 10,
        focus: 6,
    };
    doc.type_visual(selection, "*").unwrap();
    assert_eq!(doc.projection().text(), "old");
    doc.undo().unwrap();
    assert_eq!(doc.source(), source);
    assert_eq!(doc.visual_selection().unwrap(), selection);

    let prefix = "é".repeat(9000);
    let mut doc = EditorDocument::new(&prefix, Dialect::Loom).unwrap();
    doc.select_visual(Selection::caret(prefix.len()), loom_markdown::Bias::After)
        .unwrap();
    type_text(&mut doc, " **cafe\u{301}**!");
    assert_eq!(doc.projection().text(), format!("{prefix} cafe\u{301}!"));
    assert!(doc.source().starts_with(&prefix));
}

#[test]
fn link_input_retains_literal_destination_characters() {
    for destination in [
        "folder/(part",
        "quote\"mark",
        "brackets<>",
        r"path\file",
        "&#x41;",
        "&copy;",
        "<&#x41;>",
        r"path\*file",
        "https://example.com/?a=1&b=two",
        "café",
    ] {
        let mut doc = EditorDocument::new("", Dialect::Loom).unwrap();
        type_text(&mut doc, &format!("[a]({destination})"));
        assert_eq!(
            doc.projection().text(),
            "a",
            "{destination:?}: {}",
            doc.source()
        );
        assert_eq!(
            doc.projection().spans()[0].style.link.as_deref(),
            Some(destination)
        );
    }
}

#[test]
fn link_attributes_survive_repeated_editing_and_exact_undo() {
    let source = "[café](a&#38;copy;&#92;*b \"a &#38;copy; &#34;title&#34;\") tail";
    let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
    let link = doc.projection().spans()[0].style.clone();
    assert_eq!(link.link.as_deref(), Some(r"a&copy;\*b"));
    assert_eq!(link.link_title.as_deref(), Some("a &copy; \"title\""));
    doc.format_inline(
        Selection {
            anchor: 0,
            focus: 5,
        },
        &InlineFormat::Bold,
    )
    .unwrap();
    assert_eq!(doc.projection().spans()[0].style.link, link.link);
    assert_eq!(
        doc.projection().spans()[0].style.link_title,
        link.link_title
    );
    doc.replace_visual(Selection::caret(2), "!").unwrap();
    assert_eq!(doc.projection().text(), "ca!fé tail");
    assert_eq!(doc.projection().spans()[0].style.link, link.link);
    assert_eq!(
        doc.projection().spans()[0].style.link_title,
        link.link_title
    );
    doc.undo().unwrap();
    doc.undo().unwrap();
    assert_eq!(doc.source(), source);
}
