//! Close requests are intentions, not proof that Tauri accepted destruction.
use easl_tauri_probe::routing::SurfaceSlot;
use std::{cell::Cell, rc::Rc};

struct NativeResource(Rc<Cell<u32>>);
impl Drop for NativeResource {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}
struct View {
    source: String,
    selection: (usize, usize),
    native: Option<NativeResource>,
}
impl View {
    fn suspend(&mut self) {
        self.native.take();
    }
}
fn fixture(drops: &Rc<Cell<u32>>) -> SurfaceSlot<u32, View> {
    SurfaceSlot::new(
        7,
        View {
            source: "author café".into(),
            selection: (7, 12),
            native: Some(NativeResource(drops.clone())),
        },
    )
}

#[test]
fn vetoed_close_releases_native_resources_but_retains_exact_editor_state() {
    let drops = Rc::new(Cell::new(0));
    let mut slot = fixture(&drops);
    assert!(slot.suspend(&7, View::suspend));
    assert_eq!(drops.get(), 1);
    assert!(slot.get_mut(&7).is_none());
    assert!(slot.redraw(&7).is_none());
    assert!(!slot.invalidate(&7));
    assert_eq!(slot.retained(&7).unwrap().source, "author café");
    assert_eq!(slot.retained(&7).unwrap().selection, (7, 12));
    assert!(slot.resume(&7));
    assert_eq!(slot.get_mut(&7).unwrap().source, "author café");
    assert!(slot.take_redraw(&7).is_some());
    assert!(slot.take_redraw(&7).is_none());
}

#[test]
fn repeated_and_foreign_close_intentions_do_not_release_other_owners() {
    let drops = Rc::new(Cell::new(0));
    let mut slot = fixture(&drops);
    assert!(!slot.suspend(&8, View::suspend));
    assert!(!slot.resume(&8));
    assert!(slot.retained(&8).is_none());
    assert_eq!(drops.get(), 0);
    assert!(slot.suspend(&7, View::suspend));
    assert!(!slot.suspend(&7, |_| panic!("duplicate suspension")));
    assert_eq!(drops.get(), 1);
    assert!(slot.close(&7));
    assert!(!slot.resume(&7));
    assert!(slot.retained(&7).is_none());
    assert!(slot.take_redraw(&7).is_none());
    assert_eq!(drops.get(), 1);
}

#[test]
fn accepted_destruction_is_terminal_even_after_a_pending_repaint() {
    let drops = Rc::new(Cell::new(0));
    let mut slot = fixture(&drops);
    assert!(slot.invalidate(&7));
    assert!(slot.close(&7));
    assert_eq!(drops.get(), 1);
    assert!(!slot.suspend(&7, |_| panic!("dead editor revived")));
    assert!(!slot.resume(&7));
    assert!(!slot.invalidate(&7));
    assert!(slot.redraw(&7).is_none());
    assert!(slot.take_redraw(&7).is_none());
    drop(slot);
    assert_eq!(drops.get(), 1);
}

#[test]
fn idle_batches_do_not_paint_and_foreign_exposure_cannot_consume_pending_work() {
    let drops = Rc::new(Cell::new(0));
    let mut slot = fixture(&drops);
    for _ in 0..1000 {
        assert!(slot.take_redraw(&7).is_none());
    }
    assert!(slot.invalidate(&7));
    assert!(slot.take_redraw(&8).is_none());
    assert!(slot.take_redraw(&7).is_some());
    assert!(slot.redraw(&7).is_some()); // A genuine OS expose still paints.
}

#[test]
fn close_veto_keeps_real_easl_text_selections_and_separate_undo_histories() {
    use easl_tauri_probe::TwoFields;
    use easl_text::EditAction;
    let mut fields = TwoFields::new().unwrap();
    fields.resize([640., 480.]).unwrap();
    fields.set_window_focus(true);
    fields.edit(EditAction::Replace("first café")).unwrap();
    fields.cycle_focus();
    fields.edit(EditAction::Replace("second 日本語")).unwrap();
    let selections = [fields.selection(0).unwrap(), fields.selection(1).unwrap()];
    let mut slot = SurfaceSlot::new(7, fields);
    assert!(slot.suspend(&7, |_| {}));
    assert!(slot.resume(&7));
    let fields = slot.get_mut(&7).unwrap();
    assert_eq!(fields.active(), 1);
    assert_eq!(fields.selection(0).unwrap(), selections[0]);
    assert_eq!(fields.selection(1).unwrap(), selections[1]);
    fields.undo(false).unwrap();
    assert_eq!(fields.text(1).unwrap(), "");
    assert_eq!(fields.text(0).unwrap(), "first café");
    fields.undo(true).unwrap();
    assert_eq!(fields.text(1).unwrap(), "second 日本語");
    assert_eq!(fields.selection(1).unwrap(), selections[1]);
}

#[test]
fn many_vetoes_retain_one_logical_owner_and_do_not_accumulate_repaint_requests() {
    let drops = Rc::new(Cell::new(0));
    let mut slot = fixture(&drops);
    for _ in 0..100 {
        assert!(slot.suspend(&7, View::suspend));
        assert!(slot.resume(&7));
        assert!(!slot.resume(&7));
        assert!(!slot.invalidate(&7));
        assert!(slot.take_redraw(&7).is_some());
        assert!(slot.take_redraw(&7).is_none());
    }
    assert_eq!(drops.get(), 1);
    assert_eq!(slot.retained(&7).unwrap().source, "author café");
}
