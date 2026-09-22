use easl_tauri_probe::TwoFields;
use easl_text::EditAction;

#[test]
fn focus_and_undo_cannot_cross_the_two_real_buffers() {
    let mut view = TwoFields::new().unwrap();
    view.resize([640., 480.]).unwrap();
    view.set_window_focus(true);
    view.edit(EditAction::Replace("first café")).unwrap();
    view.cycle_focus();
    view.edit(EditAction::Replace("second 文")).unwrap();
    view.undo(false).unwrap();
    assert_eq!(view.text(0).unwrap(), "first café");
    assert_eq!(view.text(1).unwrap(), "");
    view.cycle_focus();
    view.undo(false).unwrap();
    assert_eq!(view.text(0).unwrap(), "");
}

fn ready() -> TwoFields {
    let mut view = TwoFields::new().unwrap();
    view.resize([640., 480.]).unwrap();
    view.set_window_focus(true);
    view
}

#[test]
fn cold_or_blurred_fields_cannot_accept_commands() {
    let mut view = TwoFields::new().unwrap();
    assert!(view.edit(EditAction::Replace("not admitted")).is_err());
    view.resize([640., 480.]).unwrap();
    assert!(view.edit(EditAction::Replace("still blurred")).is_err());
    view.set_window_focus(true);
    view.edit(EditAction::Replace("kept")).unwrap();
    view.set_window_focus(false);
    view.cycle_focus();
    assert_eq!(view.active(), 0);
    assert!(view.undo(false).is_err());
    assert!(view.edit(EditAction::Replace("no")).is_err());
    assert_eq!(view.text(0).unwrap(), "kept");
}

#[test]
fn combining_sequences_and_emoji_delete_as_graphemes() {
    let mut view = ready();
    let first = "cafe\u{301}";
    view.edit(EditAction::Replace(first)).unwrap();
    view.edit(EditAction::Replace("👩🏽‍🚀")).unwrap();
    view.edit(EditAction::Backspace).unwrap();
    assert_eq!(view.text(0).unwrap(), first);
    view.edit(EditAction::Backspace).unwrap();
    assert_eq!(view.text(0).unwrap(), "caf");
    view.undo(false).unwrap();
    assert_eq!(view.text(0).unwrap(), first);
}

#[test]
fn exact_mixed_newlines_and_trailing_spaces_survive_independent_history() {
    let mut view = ready();
    let source = "α\r\nβ\nγ\r  \t";
    view.edit(EditAction::Replace(source)).unwrap();
    view.edit(EditAction::SelectAll).unwrap();
    assert_eq!(view.selected_text(), Some(source));
    view.edit(EditAction::Replace("replacement")).unwrap();
    view.undo(false).unwrap();
    assert_eq!(view.text(0).unwrap(), source);
    view.undo(true).unwrap();
    assert_eq!(view.text(0).unwrap(), "replacement");
    view.cycle_focus();
    assert_eq!(view.text(1).unwrap(), "");
    view.undo(false).unwrap();
    assert_eq!(view.text(0).unwrap(), "replacement");
}

#[test]
fn captured_drag_stays_with_original_buffer_until_release_or_blur() {
    let mut view = ready();
    view.edit(EditAction::Replace("first field\nsecond line"))
        .unwrap();
    view.cycle_focus();
    view.edit(EditAction::Replace("other buffer")).unwrap();
    let boxes = view.boxes().unwrap();
    let start = [boxes[0].0[0] + 10., boxes[0].0[1] + 10.];
    let end = [boxes[1].0[0] + 90., boxes[1].0[1] + 10.];
    let second_selection = view.selection(1).unwrap();
    assert!(view.pointer_down(start, false).unwrap());
    assert!(view.pointer_move(end).unwrap());
    assert_eq!(view.active(), 0);
    assert_eq!(view.selection(1).unwrap(), second_selection);
    assert!(view.capturing());
    view.set_window_focus(false);
    assert!(!view.capturing());
    assert!(!view.pointer_move(start).unwrap());
    assert_eq!(view.text(0).unwrap(), "first field\nsecond line");
}

#[test]
fn failed_admission_preserves_text_selection_and_undo() {
    let mut view = ready();
    view.edit(EditAction::Replace("kept")).unwrap();
    let before = view.selection(0).unwrap();
    let oversized = "x".repeat(easl_native_text::MAX_TEXT_BYTES + 1);
    assert!(view.edit(EditAction::Replace(&oversized)).is_err());
    assert_eq!(view.text(0).unwrap(), "kept");
    assert_eq!(view.selection(0).unwrap(), before);
    view.undo(false).unwrap();
    assert_eq!(view.text(0).unwrap(), "");
}

#[test]
fn bad_geometry_does_not_replace_current_layout_or_poison_valid_resize() {
    let mut view = ready();
    let before = view.boxes().unwrap();
    for size in [
        [f32::NAN, 480.],
        [640., f32::INFINITY],
        [32., 64.],
        [20_000., 480.],
    ] {
        assert!(view.resize(size).is_err());
        assert_eq!(view.boxes().unwrap(), before);
    }
    view.resize([320., 240.]).unwrap();
    let [first, second] = view.boxes().unwrap();
    assert!(first.0[1] + first.0[3] <= second.0[1]);
    assert!(second.0[1] + second.0[3] <= 240.);
}

#[test]
fn actual_editor_content_changes_the_offscreen_raster() {
    let mut view = ready();
    view.set_window_focus(false); // Only glyphs, not moving caret pixels, may change the image.
    let mut surface = easl_native_text::RasterSurface::new(640, 480, 1.).unwrap();
    surface.begin(640, 480, 1.).unwrap();
    surface
        .rect([0., 0., 640., 480.], [245, 245, 245, 255])
        .unwrap();
    view.paint(&mut surface).unwrap();
    let before = surface.finish().to_vec();
    view.set_window_focus(true);
    view.edit(EditAction::Replace("native glyphs")).unwrap();
    view.set_window_focus(false);
    surface.begin(640, 480, 1.).unwrap();
    surface
        .rect([0., 0., 640., 480.], [245, 245, 245, 255])
        .unwrap();
    view.paint(&mut surface).unwrap();
    let after = surface.finish();
    assert_eq!(after.len(), before.len());
    assert_ne!(after, before.as_slice());
    assert_eq!(view.text(0).unwrap(), "native glyphs");
    assert_eq!(view.text(1).unwrap(), "");
}

#[test]
fn rejected_pointer_geometry_does_not_transfer_focus_or_change_selection() {
    let mut view = ready();
    view.edit(EditAction::Replace("left intact")).unwrap();
    let before = view.selection(0).unwrap();
    assert!(view.pointer_down([f32::NAN, 300.], false).is_err());
    assert_eq!(view.active(), 0);
    assert_eq!(view.selection(0).unwrap(), before);
    assert_eq!(view.text(0).unwrap(), "left intact");
    assert_eq!(view.text(1).unwrap(), "");
}
