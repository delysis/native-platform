use super::FrameState;

#[test]
fn a_pointer_burst_requests_one_redraw_until_consumed() {
    let mut frame = FrameState::default();
    assert!(frame.queue_pointer());
    for _ in 0..1_000 {
        assert!(!frame.queue_pointer());
    }
    assert!(frame.pointer_pending);
    // This is the consumption performed at the native redraw boundary.
    frame.pointer_pending = false;
    assert!(frame.queue_pointer());
}

#[test]
fn pointer_queueing_does_not_discard_an_existing_dirty_frame() {
    let mut frame = FrameState {
        dirty: true,
        ..Default::default()
    };
    assert!(frame.queue_pointer());
    assert!(frame.dirty);
    assert!(!frame.queue_pointer());
    assert!(frame.dirty);
}
