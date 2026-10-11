use loom_markdown::{
    Bias, Dialect, EditorDocument, Error, FormattingCommand as Command, InlineFormat,
    ParagraphFormat, Selection, StructureFormat,
};

#[test]
fn caret_marks_and_typing_match_the_current_web_editor_at_inline_boundaries() {
    // Measured with the checked-in app's ProseMirror parser, TextSelection,
    // $from.marks(), insertText and serializer. Visual offsets exclude block
    // tokens; new paragraphs never inherit the preceding paragraph's marks.
    for (source, byte, bold, expected) in [
        ("a**b**c", 0, false, "Xa**b**c"),
        ("a**b**c", 1, false, "aX**b**c"),
        ("a**b**c", 2, true, "a**bX**c"),
        ("a**b**c", 3, false, "a**b**cX"),
        ("**ab** c", 0, true, "**Xab** c"),
        ("**ab** c", 2, true, "**abX** c"),
        ("**ab** c", 3, false, "**ab** Xc"),
        (
            "[ab](https://example.com) c",
            0,
            false,
            "X[ab](https://example.com) c",
        ),
        (
            "[ab](https://example.com) c",
            1,
            false,
            "[aXb](https://example.com) c",
        ),
        (
            "[ab](https://example.com) c",
            2,
            false,
            "[ab](https://example.com)X c",
        ),
        ("**one**\n\ntwo", 4, false, "**one**\n\nXtwo"),
    ] {
        let mut document = EditorDocument::new(source, Dialect::Loom).unwrap();
        let selection = Selection::caret(byte);
        let capture = document.capture_formatting_selection(selection).unwrap();
        assert_eq!(
            document.formatting_state(&capture).unwrap().bold,
            bold,
            "{source:?} at {byte}"
        );
        document.type_visual(selection, "X").unwrap();
        assert_eq!(document.source(), expected, "{source:?} at {byte}");
        document.undo().unwrap();
        assert_eq!(document.source(), source);
    }
}

#[test]
fn replacing_a_selected_span_inherits_that_span_not_the_preceding_text() {
    for selection in [
        Selection {
            anchor: 1,
            focus: 2,
        },
        Selection {
            anchor: 2,
            focus: 1,
        },
    ] {
        let mut document = EditorDocument::new("a**b**c", Dialect::Loom).unwrap();
        document.type_visual(selection, "X").unwrap();
        assert_eq!(document.source(), "a**X**c");
        document.undo().unwrap();
        assert_eq!(document.source(), "a**b**c");
        assert_eq!(document.visual_selection().unwrap(), selection);
    }
}

#[test]
fn captured_formatting_preserves_backward_selection_and_has_one_undo_per_command() {
    let source = "café tail\r\n\r\nuntouched  \r\n";
    let mut document = EditorDocument::new(source, Dialect::Loom).unwrap();
    let selection = Selection {
        anchor: 5,
        focus: 0,
    };
    document.select_visual(selection, Bias::After).unwrap();
    let mut capture = document.capture_formatting_selection(selection).unwrap();
    document
        .select_visual(Selection::caret(10), Bias::After)
        .unwrap();
    document
        .apply_formatting(&mut capture, &Command::Inline(InlineFormat::Bold))
        .unwrap();
    assert_eq!(document.source(), "**café** tail\r\n\r\nuntouched  \r\n");
    assert_eq!(document.visual_selection().unwrap(), selection);
    assert!(document.formatting_state(&capture).unwrap().bold);
    document
        .apply_formatting(&mut capture, &Command::Inline(InlineFormat::Italic))
        .unwrap();
    let state = document.formatting_state(&capture).unwrap();
    assert!(state.bold && state.italic);
    document.undo().unwrap();
    assert_eq!(document.source(), "**café** tail\r\n\r\nuntouched  \r\n");
    assert_eq!(
        document.apply_formatting(
            &mut capture,
            &Command::Paragraph(ParagraphFormat::Heading(1))
        ),
        Err(Error::StaleTransaction)
    );
    document.undo().unwrap();
    assert_eq!(document.source(), source);
    assert_eq!(document.visual_selection().unwrap(), selection);
}

#[test]
fn foreign_stale_invalid_and_literal_selections_never_acquire_formatting_authority() {
    let mut first = EditorDocument::new("café", Dialect::Loom).unwrap();
    let mut other = EditorDocument::new("café", Dialect::Loom).unwrap();
    let mut capture = first
        .capture_formatting_selection(Selection {
            anchor: 0,
            focus: 5,
        })
        .unwrap();
    let command = Command::Inline(InlineFormat::Bold);
    assert_eq!(
        other.apply_formatting(&mut capture, &command),
        Err(Error::StaleTransaction)
    );
    assert_eq!(other.source(), "café");
    assert!(!other.can_undo());
    assert!(matches!(
        first.capture_formatting_selection(Selection::caret(4)),
        Err(Error::InvalidRange)
    ));
    first
        .select_visual(Selection::caret(0), Bias::After)
        .unwrap();
    first.type_visual(Selection::caret(0), "X").unwrap();
    assert_eq!(
        first.apply_formatting(&mut capture, &command),
        Err(Error::StaleTransaction)
    );
    first.undo().unwrap();
    // Returning to identical bytes does not revive an old document revision.
    assert_eq!(
        first.apply_formatting(&mut capture, &command),
        Err(Error::StaleTransaction)
    );
    let literal = EditorDocument::new("*literal*", Dialect::PlainText).unwrap();
    assert!(matches!(
        literal.capture_formatting_selection(Selection::caret(0)),
        Err(Error::UnsupportedEdit)
    ));
}

#[test]
fn palette_state_matches_mixed_marks_nested_structures_and_empty_typing_marks() {
    let source = "# **Title**\n\n> 1. *outer*\n>    - inner\n";
    let mut document = EditorDocument::new(source, Dialect::Loom).unwrap();
    let all = document
        .capture_formatting_selection(Selection {
            anchor: document.projection().text().len(),
            focus: 0,
        })
        .unwrap();
    let state = document.formatting_state(&all).unwrap();
    assert_eq!(state.paragraph, ParagraphFormat::Heading(1));
    assert!(state.bold && state.italic && state.quote && state.bullet_list && state.ordered_list);
    let inner = document.projection().text().find("inner").unwrap();
    let selection = Selection::caret(inner + 1);
    document.select_visual(selection, Bias::After).unwrap();
    let mut capture = document.capture_formatting_selection(selection).unwrap();
    let state = document.formatting_state(&capture).unwrap();
    assert_eq!(state.paragraph, ParagraphFormat::Body);
    assert!(!state.bold && !state.italic);
    assert!(state.quote && state.bullet_list && state.ordered_list);
    document
        .apply_formatting(&mut capture, &Command::Inline(InlineFormat::Bold))
        .unwrap();
    assert!(document.formatting_state(&capture).unwrap().bold);
    assert_eq!(document.source(), source);
    assert!(!document.can_undo());
    let mut empty = EditorDocument::new("# ", Dialect::Loom).unwrap();
    let mut capture = empty
        .capture_formatting_selection(Selection::caret(0))
        .unwrap();
    assert_eq!(
        empty.formatting_state(&capture).unwrap().paragraph,
        ParagraphFormat::Heading(1)
    );
    empty
        .apply_formatting(&mut capture, &Command::Structure(StructureFormat::Quote))
        .unwrap();
    assert!(empty.formatting_state(&capture).unwrap().quote);
}

#[test]
fn rejected_command_retains_live_selection_and_typing_marks() {
    let mut document = EditorDocument::new("words", Dialect::Loom).unwrap();
    let mut capture = document
        .capture_formatting_selection(Selection::caret(0))
        .unwrap();
    document
        .select_visual(Selection::caret(5), Bias::After)
        .unwrap();
    document
        .format_inline(Selection::caret(5), &InlineFormat::Italic)
        .unwrap();
    assert_eq!(
        document.apply_formatting(
            &mut capture,
            &Command::Paragraph(ParagraphFormat::Heading(7))
        ),
        Err(Error::UnsupportedEdit)
    );
    assert_eq!(document.visual_selection().unwrap(), Selection::caret(5));
    let live = document
        .capture_formatting_selection(Selection::caret(5))
        .unwrap();
    assert!(document.formatting_state(&live).unwrap().italic);
    assert_eq!(document.source(), "words");
    assert!(!document.can_undo());
}
