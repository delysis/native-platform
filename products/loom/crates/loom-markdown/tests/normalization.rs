use loom_markdown::{Bias, Dialect, EditorDocument, Error, Projection, Selection};

#[test]
fn explicit_storage_normalization_preserves_source_and_decoded_visual_selections_on_undo() {
    let source = "> **café**\r\n>\r\n> ```rust\r\n> A\r\n> B\r\n> ```\r\n\r\n&fjlig; tail";
    let canonical = source.replace("\r\n", "\n");
    for source_mode in [false, true] {
        for backwards in [false, true] {
            let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
            let (start, end) = if source_mode {
                (source.find('A').unwrap(), source.find("tail").unwrap())
            } else {
                let text = doc.projection().text();
                (text.find('A').unwrap(), text.find("fj").unwrap() + 1)
            };
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
            if source_mode {
                doc.select(selection).unwrap();
            } else {
                doc.select_visual(selection, Bias::After).unwrap();
            }
            assert!(doc.normalize_line_endings().unwrap());
            assert_eq!(doc.source(), canonical);
            let mapped = if source_mode {
                (
                    canonical.find('A').unwrap(),
                    canonical.find("tail").unwrap(),
                )
            } else {
                (
                    doc.projection().text().find('A').unwrap(),
                    doc.projection().text().find("fj").unwrap() + 1,
                )
            };
            let expected = if backwards {
                Selection {
                    anchor: mapped.1,
                    focus: mapped.0,
                }
            } else {
                Selection {
                    anchor: mapped.0,
                    focus: mapped.1,
                }
            };
            if source_mode {
                assert_eq!(doc.source_selection().unwrap(), expected);
            } else {
                assert_eq!(doc.visual_selection().unwrap(), expected);
                assert_eq!(doc.source_selection(), Err(Error::AmbiguousBoundary));
            }
            doc.undo().unwrap();
            assert_eq!(doc.source(), source);
            assert_eq!(
                if source_mode {
                    doc.source_selection()
                } else {
                    doc.visual_selection()
                }
                .unwrap(),
                selection
            );
            doc.redo().unwrap();
            assert_eq!(doc.source(), canonical);
        }
    }
}

#[test]
fn normalization_is_optional_and_rejected_admission_is_atomic() {
    fn original_only(source: &str, _: &Projection) -> Result<(), Error> {
        if source.contains('\r') {
            Ok(())
        } else {
            Err(Error::Limit)
        }
    }
    let mut doc = EditorDocument::new("already\ncanonical", Dialect::Loom).unwrap();
    assert!(!doc.normalize_line_endings().unwrap());
    assert!(!doc.can_undo());
    let source = "one\rtwo\r\nthree";
    let mut doc = EditorDocument::new(source, Dialect::Loom).unwrap();
    doc.set_admission_check(original_only).unwrap();
    let selection = Selection::caret(2);
    doc.select_visual(selection, Bias::After).unwrap();
    assert_eq!(doc.normalize_line_endings(), Err(Error::Limit));
    assert_eq!(doc.source(), source);
    assert_eq!(doc.visual_selection().unwrap(), selection);
    assert!(!doc.can_undo());
}
