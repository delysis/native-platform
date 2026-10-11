use loom_markdown::{Dialect, EditorDocument, Error, Selection};

#[test]
fn pane_selections_follow_unicode_edits_and_shared_undo() {
    let mut doc = EditorDocument::new("café 🌍 tail", Dialect::PlainText).unwrap();
    let end = doc.source().len();
    let forward = doc
        .track_selection(Selection {
            anchor: 0,
            focus: end,
        })
        .unwrap();
    let backward = doc
        .track_selection(Selection {
            anchor: end,
            focus: 6,
        })
        .unwrap();
    let mut edit = doc.transaction();
    edit.replace(0..0, "→ ").unwrap();
    doc.apply(edit).unwrap();
    assert_eq!(
        doc.tracked_selection(forward).unwrap(),
        Selection {
            anchor: 4,
            focus: end + 4
        }
    );
    assert_eq!(
        doc.tracked_selection(backward).unwrap(),
        Selection {
            anchor: end + 4,
            focus: 10
        }
    );
    doc.undo().unwrap();
    assert_eq!(
        doc.tracked_selection(forward).unwrap(),
        Selection {
            anchor: 0,
            focus: end
        }
    );
    doc.redo().unwrap();
    assert_eq!(doc.tracked_selection(backward).unwrap().focus, 10);
}

#[test]
fn rejected_changes_do_not_move_other_views_or_consume_their_handles() {
    let mut doc = EditorDocument::new("café", Dialect::PlainText).unwrap();
    let handle = doc.track_selection(Selection::caret(5)).unwrap();
    doc.set_admission_check(|text, _| {
        if text.starts_with('X') {
            Err(Error::Limit)
        } else {
            Ok(())
        }
    })
    .unwrap();
    let mut edit = doc.transaction();
    edit.replace(0..0, "X").unwrap();
    assert_eq!(doc.apply(edit), Err(Error::Limit));
    assert_eq!(doc.tracked_selection(handle).unwrap(), Selection::caret(5));
    assert_eq!(doc.revision(), 0);
    assert!(!doc.can_undo());
    let mut other = EditorDocument::new("café", Dialect::PlainText).unwrap();
    assert_eq!(
        other.tracked_selection(handle),
        Err(Error::UnknownSelection)
    );
    assert_eq!(
        other.release_selection(handle),
        Err(Error::UnknownSelection)
    );
    doc.release_selection(handle).unwrap();
    let fresh = doc.track_selection(Selection::caret(0)).unwrap();
    assert_ne!(handle, fresh);
    assert_eq!(doc.tracked_selection(handle), Err(Error::UnknownSelection));
}

#[test]
fn an_edit_that_joins_graphemes_keeps_tracked_carets_on_whole_graphemes() {
    let mut doc = EditorDocument::new("ab", Dialect::PlainText).unwrap();
    let handle = doc.track_selection(Selection::caret(1)).unwrap();
    let mut edit = doc.transaction();
    edit.replace(1..1, "\u{301}").unwrap();
    doc.apply(edit).unwrap();
    assert_eq!(doc.tracked_selection(handle).unwrap(), Selection::caret(3));
    doc.undo().unwrap();
    assert_eq!(doc.tracked_selection(handle).unwrap(), Selection::caret(1));
}
