use super::*;
use accesskit::{NodeId, TreeId};

fn action() -> ActionRequest {
    ActionRequest {
        action: Action::Focus,
        target_tree: TreeId::ROOT,
        target_node: NodeId(2),
        data: None,
    }
}
#[test]
fn activation_is_deferred_and_wakes_once_without_touching_an_editor() {
    let wakes = Rc::new(Cell::new(0));
    let count = wakes.clone();
    let mailbox = Mailbox::new(move || {
        count.set(count.get() + 1);
        true
    });
    let mut callbacks = mailbox.callbacks();
    assert!(callbacks.request_initial_tree().is_none());
    assert!(callbacks.request_initial_tree().is_none());
    assert_eq!(wakes.get(), 1);
    assert!(mailbox.needs_update(1));
    mailbox.published(1, 1);
    assert!(!mailbox.needs_update(1));
    assert!(mailbox.needs_update(2));
}
#[test]
fn requests_capture_published_revision_not_the_future_editor_revision() {
    let mailbox = Mailbox::new(|| true);
    mailbox.published(9, 9);
    mailbox.callbacks().do_action(action());
    mailbox.published(10, 10);
    assert_eq!(mailbox.pop().unwrap().unwrap().revision, 9);
    assert!(mailbox.pop().unwrap().is_none());
}
#[test]
fn suspension_discards_queued_requests_and_requires_republication() {
    let mailbox = Mailbox::new(|| true);
    mailbox.published(1, 1);
    mailbox.callbacks().do_action(action());
    mailbox.set_accepting(false);
    mailbox.callbacks().do_action(action());
    assert!(mailbox.pop().unwrap().is_none());
    mailbox.set_accepting(true);
    mailbox.callbacks().do_action(action());
    assert!(mailbox.pop().unwrap().is_none());
    mailbox.published(2, 2);
    mailbox.callbacks().do_action(action());
    assert_eq!(mailbox.pop().unwrap().unwrap().revision, 2);
}
#[test]
fn a_full_queue_is_a_failure_not_silent_truncation() {
    let mailbox = Mailbox::new(|| true);
    mailbox.published(1, 1);
    for _ in 0..MAX_ACTIONS {
        mailbox.callbacks().do_action(action());
    }
    mailbox.callbacks().do_action(action());
    assert_eq!(mailbox.begin_batch(), Err(Error::QueueFull));
    assert!(matches!(mailbox.pop(), Err(Error::QueueFull)));
}
#[test]
fn failed_wake_and_oversize_value_never_report_success() {
    let mailbox = Mailbox::new(|| false);
    assert!(mailbox.callbacks().request_initial_tree().is_none());
    assert_eq!(mailbox.begin_batch(), Err(Error::WakeFailed));
    let mailbox = Mailbox::new(|| true);
    mailbox.published(1, 1);
    let mut request = action();
    request.action = Action::SetValue;
    request.data = Some(ActionData::Value("x".repeat(MAX_VALUE_BYTES + 1).into()));
    mailbox.callbacks().do_action(request);
    assert_eq!(mailbox.begin_batch(), Err(Error::ValueTooLarge));
}
#[test]
fn callbacks_without_a_published_tree_cannot_enqueue_edits() {
    let mailbox = Mailbox::new(|| true);
    mailbox.callbacks().do_action(action());
    assert!(mailbox.pop().unwrap().is_none());
    mailbox.published(u64::MAX, u64::MAX);
    mailbox.callbacks().do_action(action());
    assert!(mailbox.pop().unwrap().is_none());
}

#[test]
fn callbacks_after_mailbox_destruction_cannot_wake_or_modify_a_future_owner() {
    let wakes = Rc::new(Cell::new(0));
    let count = wakes.clone();
    let mailbox = Mailbox::new(move || {
        count.set(count.get() + 1);
        true
    });
    let mut callbacks = mailbox.callbacks();
    mailbox.published(1, 1);
    drop(mailbox);
    callbacks.do_action(action());
    assert!(callbacks.request_initial_tree().is_none());
    assert_eq!(wakes.get(), 0);
    assert!(!callbacks.is_current(1));
}

#[test]
fn repeated_accepting_state_does_not_discard_an_already_queued_action() {
    let mailbox = Mailbox::new(|| true);
    mailbox.published(1, 1);
    mailbox.callbacks().do_action(action());
    mailbox.set_accepting(true);
    assert_eq!(mailbox.pop().unwrap().unwrap().revision, 1);
}

#[test]
fn each_event_batch_rearms_one_wake_without_a_polling_loop() {
    let wakes = Rc::new(Cell::new(0));
    let count = wakes.clone();
    let mailbox = Mailbox::new(move || {
        count.set(count.get() + 1);
        true
    });
    mailbox.published(1, 1);
    for _ in 0..3 {
        mailbox.callbacks().do_action(action());
    }
    assert_eq!(wakes.get(), 1);
    mailbox.begin_batch().unwrap();
    while mailbox.pop().unwrap().is_some() {}
    mailbox.callbacks().do_action(action());
    assert_eq!(wakes.get(), 2);
}

#[test]
fn debugging_a_pending_value_never_prints_its_text() {
    let mailbox = Mailbox::new(|| true);
    mailbox.published(1, 1);
    let mut value = action();
    value.action = Action::SetValue;
    value.data = Some(ActionData::Value("private manuscript sentinel".into()));
    mailbox.callbacks().do_action(value);
    let pending = mailbox.pop().unwrap().unwrap();
    assert!(!format!("{pending:?} {mailbox:?}").contains("private manuscript sentinel"));
}

#[test]
fn activation_while_suspended_is_remembered_for_the_next_live_batch() {
    let wakes = Rc::new(Cell::new(0));
    let count = wakes.clone();
    let mailbox = Mailbox::new(move || {
        count.set(count.get() + 1);
        true
    });
    mailbox.set_accepting(false);
    assert!(mailbox.callbacks().request_initial_tree().is_none());
    assert_eq!(wakes.get(), 0);
    mailbox.set_accepting(true);
    assert!(mailbox.needs_update(1));
}

#[test]
fn presentation_refreshes_do_not_change_the_source_epoch_captured_by_actions() {
    let mailbox = Mailbox::new(|| true);
    assert!(mailbox.callbacks().request_initial_tree().is_none());
    mailbox.published(10, 3);
    mailbox.callbacks().do_action(action());
    mailbox.published(11, 3);
    assert!(mailbox.callbacks().is_current(11));
    assert!(!mailbox.callbacks().is_current(10));
    assert!(!mailbox.needs_update(11));
    assert_eq!(mailbox.pop().unwrap().unwrap().revision, 3);
}
