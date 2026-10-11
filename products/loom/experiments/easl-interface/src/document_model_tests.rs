use super::*;
use loom_markdown::Selection;

#[test]
fn admission_preserves_exact_source_for_both_dialects() {
    let fixtures = [
        "",
        "café e\u{301} עברית العربية ไทย 🧑🏽‍💻",
        "first\r\nsecond\nthird\r\n\t trailing  ",
        "**bold** &amp; [link][target]\r\n\r\n[target]: https://example.com\r\n",
        "```rust\r\nlet x = 1;\t\r\n```\r\n",
    ];
    for source in fixtures {
        for verse in [false, true] {
            let document = create(source, verse).unwrap();
            assert_eq!(document.source().as_bytes(), source.as_bytes());
            assert_eq!(document.revision(), 0);
            assert!(!document.can_undo() && !document.can_redo());
            assert_eq!(
                document.markdown().dialect(),
                if verse {
                    Dialect::PlainText
                } else {
                    Dialect::Loom
                }
            );
        }
    }
}

#[test]
fn source_capacity_is_rejected_before_constructing_an_editor() {
    let source = "x".repeat(easl_native_text::MAX_TEXT_BYTES + 1);
    for verse in [false, true] {
        assert!(create(&source, verse).is_err());
    }
}

#[test]
fn discretionary_shaping_work_is_bounded_even_for_small_sources() {
    let source = "a\u{ad}".repeat(3_000);
    assert!(source.len() < easl_native_text::MAX_TEXT_BYTES);
    assert!(easl_native_text::validate_text(&source).is_err());
    for verse in [false, true] {
        assert!(create(&source, verse).is_err());
    }
}

#[test]
fn projected_entity_expansion_obeys_the_native_shaping_budget() {
    // Entity spelling is harmless to source validation. Decoding it produces
    // enough consecutive discretionary breaks to exceed native shaping work.
    let source = "&shy;".repeat(3_000);
    assert!(easl_native_text::validate_text(&source).is_ok());
    assert!(create(&source, false).is_err());
    // Literal verse does not decode entities and therefore remains admitted.
    assert_eq!(create(&source, true).unwrap().source(), source);
}

#[test]
fn style_span_capacity_is_enforced_on_the_real_markdown_projection() {
    let source = "**x** y ".repeat(easl_native_text::MAX_EDITOR_SPANS / 2 + 1);
    let raw = EditorDocument::new(&source, Dialect::Loom).unwrap();
    assert!(raw.projection().spans().len() >= easl_native_text::MAX_EDITOR_SPANS);
    assert_eq!(
        admit_projection(&source, raw.projection()),
        Err(loom_markdown::Error::Limit)
    );
    assert!(create(&source, false).is_err());
}

#[test]
fn rejected_edit_retains_source_selection_revision_and_redo() {
    for verse in [false, true] {
        let mut document = create("stable", verse).unwrap();
        document.select(Selection::caret(6)).unwrap();
        document.replace_selection("!").unwrap();
        document.undo().unwrap();
        let revision = document.revision();
        let selection = document.source_selection().unwrap();
        assert!(document.can_redo());
        assert!(
            document
                .replace_selection(&"a\u{ad}".repeat(3_000))
                .is_err()
        );
        assert_eq!(document.source(), "stable");
        assert_eq!(document.revision(), revision);
        assert_eq!(document.source_selection().unwrap(), selection);
        assert!(!document.can_undo());
        assert!(document.can_redo());
        document.redo().unwrap();
        assert_eq!(document.source(), "stable!");
        document.undo().unwrap();
        assert_eq!(document.source(), "stable");
    }
}
