use accesskit::{Action, ActionData, ActionRequest, NodeId, Role, TreeId};
use easl_tauri_probe::{
    TwoFields,
    accessibility::{Accessibility, Outcome},
};
use easl_text::EditAction;

fn request(action: Action, target: u64, data: Option<ActionData>) -> ActionRequest {
    ActionRequest {
        action,
        target_tree: TreeId::ROOT,
        target_node: NodeId(target),
        data,
    }
}

#[test]
fn accessible_value_edits_the_real_buffer_once_and_uses_ordinary_undo() {
    let mut view = TwoFields::new().unwrap();
    view.resize([800., 600.]).unwrap();
    view.set_window_focus(true);
    view.edit(EditAction::Replace("original café")).unwrap();
    let mut accessibility = Accessibility::default();
    let snapshot = accessibility.snapshot(&mut view, 1.).unwrap();
    let action = request(
        Action::SetValue,
        2,
        Some(ActionData::Value("changed 🧑🏽‍💻".into())),
    );
    assert_eq!(
        accessibility
            .apply(&mut view, snapshot.revision, &action)
            .unwrap(),
        Outcome::Changed
    );
    assert_eq!(view.text(0).unwrap(), "changed 🧑🏽‍💻");
    assert_eq!(view.text(1).unwrap(), "");
    assert_eq!(
        accessibility
            .apply(&mut view, snapshot.revision, &action)
            .unwrap(),
        Outcome::Ignored
    );
    view.undo(false).unwrap();
    assert_eq!(view.text(0).unwrap(), "original café");
    view.undo(true).unwrap();
    assert_eq!(view.text(0).unwrap(), "changed 🧑🏽‍💻");
}

#[test]
fn native_tree_has_two_real_editors_and_an_unfocused_root() {
    let mut view = TwoFields::new().unwrap();
    view.resize([800., 600.]).unwrap();
    let mut accessibility = Accessibility::default();
    let snapshot = accessibility.snapshot(&mut view, 2.).unwrap();
    assert_eq!(snapshot.update.focus, NodeId(1));
    for (id, label) in [(2, "First text field"), (3, "Second text field")] {
        let node = &snapshot
            .update
            .nodes
            .iter()
            .find(|(key, _)| *key == NodeId(id))
            .unwrap()
            .1;
        assert_eq!(node.role(), Role::MultilineTextInput);
        assert_eq!(node.label(), Some(label));
        assert!(node.supports_action(Action::Focus));
        assert!(node.bounds().is_some());
    }
}

fn ready() -> (TwoFields, Accessibility) {
    let mut view = TwoFields::new().unwrap();
    view.resize([800., 600.]).unwrap();
    view.set_window_focus(true);
    view.edit(EditAction::Replace("café 👨‍👩‍👧‍👦\nsecond line"))
        .unwrap();
    (view, Accessibility::default())
}

#[test]
fn a_request_queued_before_typing_cannot_overwrite_the_newer_buffer() {
    let (mut view, mut accessibility) = ready();
    let old = accessibility.snapshot(&mut view, 1.).unwrap();
    view.edit(EditAction::Replace(" newer")).unwrap();
    let before = view.text(0).unwrap();
    let value = request(Action::SetValue, 2, Some(ActionData::Value("stale".into())));
    assert_eq!(
        accessibility
            .apply(&mut view, old.revision, &value)
            .unwrap(),
        Outcome::Ignored
    );
    assert_eq!(view.text(0).unwrap(), before);
}

#[test]
fn undo_never_revalidates_an_old_request_even_when_bytes_match_again() {
    let (mut view, mut accessibility) = ready();
    let old = accessibility.snapshot(&mut view, 1.).unwrap();
    view.edit(EditAction::Replace(" temporary")).unwrap();
    view.undo(false).unwrap();
    assert!(view.revision() > old.revision);
    let value = request(
        Action::SetValue,
        2,
        Some(ActionData::Value("old reply".into())),
    );
    assert_eq!(
        accessibility
            .apply(&mut view, old.revision, &value)
            .unwrap(),
        Outcome::Ignored
    );
}

#[test]
fn identical_numeric_ids_in_another_window_do_not_transfer_a_projection() {
    let (mut first, mut accessibility) = ready();
    let snapshot = accessibility.snapshot(&mut first, 1.).unwrap();
    let (mut second, _) = ready();
    let value = request(
        Action::SetValue,
        2,
        Some(ActionData::Value("foreign".into())),
    );
    assert_eq!(
        accessibility
            .apply(&mut second, snapshot.revision, &value)
            .unwrap(),
        Outcome::Ignored
    );
    assert!(accessibility.snapshot(&mut second, 1.).is_err());
    assert_eq!(first.text(0).unwrap(), second.text(0).unwrap());
}

#[test]
fn field_focus_is_requested_before_mutation_and_os_focus_is_not_invented() {
    let (mut view, mut accessibility) = ready();
    view.set_window_focus(false);
    let snapshot = accessibility.snapshot(&mut view, 1.).unwrap();
    let focus = request(Action::Focus, 3, None);
    assert_eq!(
        accessibility
            .apply(&mut view, snapshot.revision, &focus)
            .unwrap(),
        Outcome::FocusWindow
    );
    assert_eq!(view.active(), 1);
    assert!(!view.focused());
    let snapshot = accessibility.snapshot(&mut view, 1.).unwrap();
    let value = request(
        Action::SetValue,
        3,
        Some(ActionData::Value("second".into())),
    );
    assert_eq!(
        accessibility
            .apply(&mut view, snapshot.revision, &value)
            .unwrap(),
        Outcome::Ignored
    );
    view.set_window_focus(true); // The host's actual OS focus event.
    let snapshot = accessibility.snapshot(&mut view, 1.).unwrap();
    assert_eq!(
        accessibility
            .apply(&mut view, snapshot.revision, &value)
            .unwrap(),
        Outcome::Changed
    );
    assert_eq!(view.text(1).unwrap(), "second");
    assert_ne!(view.text(0).unwrap(), "second");
}

#[test]
fn a_noop_focus_does_not_revoke_the_current_value_action() {
    let (mut view, mut accessibility) = ready();
    let snapshot = accessibility.snapshot(&mut view, 1.).unwrap();
    assert_eq!(
        accessibility
            .apply(
                &mut view,
                snapshot.revision,
                &request(Action::Focus, 2, None)
            )
            .unwrap(),
        Outcome::Ignored
    );
    assert_eq!(view.revision(), snapshot.revision);
    let value = request(Action::SetValue, 2, Some(ActionData::Value("value".into())));
    assert_eq!(
        accessibility
            .apply(&mut view, snapshot.revision, &value)
            .unwrap(),
        Outcome::Changed
    );
}

#[test]
fn unknown_nodes_wrong_actions_and_unfocused_fields_leave_source_unchanged() {
    let (mut view, mut accessibility) = ready();
    let snapshot = accessibility.snapshot(&mut view, 1.).unwrap();
    let before = [view.text(0).unwrap(), view.text(1).unwrap()];
    for action in [
        request(
            Action::SetValue,
            1000,
            Some(ActionData::Value("bad".into())),
        ),
        request(Action::SetValue, 3, Some(ActionData::Value("bad".into()))),
        request(Action::Focus, 2, Some(ActionData::Value("bad".into()))),
        request(Action::Click, 2, None),
    ] {
        assert_eq!(
            accessibility
                .apply(&mut view, snapshot.revision, &action)
                .unwrap(),
            Outcome::Ignored
        );
    }
    assert_eq!([view.text(0).unwrap(), view.text(1).unwrap()], before);
}

#[test]
fn oversized_value_preserves_source_selection_and_undo() {
    let (mut view, mut accessibility) = ready();
    let snapshot = accessibility.snapshot(&mut view, 1.).unwrap();
    let before = (view.text(0).unwrap(), view.selection(0).unwrap());
    let value = request(
        Action::SetValue,
        2,
        Some(ActionData::Value(
            "x".repeat(easl_native_text::MAX_TEXT_BYTES + 1).into(),
        )),
    );
    assert!(
        accessibility
            .apply(&mut view, snapshot.revision, &value)
            .is_err()
    );
    assert_eq!((view.text(0).unwrap(), view.selection(0).unwrap()), before);
    view.undo(false).unwrap();
    assert_eq!(view.text(0).unwrap(), "");
}

#[test]
fn unicode_selection_uses_published_character_positions_not_utf8_offsets() {
    let (mut view, mut accessibility) = ready();
    let snapshot = accessibility.snapshot(&mut view, 1.).unwrap();
    let (id, run) = snapshot
        .update
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::TextRun && !node.character_lengths().is_empty())
        .unwrap();
    let selection = accesskit::TextSelection {
        anchor: accesskit::TextPosition {
            node: *id,
            character_index: 0,
        },
        focus: accesskit::TextPosition {
            node: *id,
            character_index: run.character_lengths().len(),
        },
    };
    let value = request(
        Action::SetTextSelection,
        2,
        Some(ActionData::SetTextSelection(selection)),
    );
    assert_eq!(
        accessibility
            .apply(&mut view, snapshot.revision, &value)
            .unwrap(),
        Outcome::Changed
    );
    let (anchor, focus) = view.selection(0).unwrap();
    let text = view.text(0).unwrap();
    assert!(text.is_char_boundary(anchor) && text.is_char_boundary(focus));
    assert_ne!(anchor, focus);
    assert_eq!(text, "café 👨‍👩‍👧‍👦\nsecond line");
}

#[test]
fn selection_rejects_foreign_runs_and_out_of_range_character_indices() {
    let (mut view, mut accessibility) = ready();
    let snapshot = accessibility.snapshot(&mut view, 1.).unwrap();
    let (id, run) = snapshot
        .update
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::TextRun && !node.character_lengths().is_empty())
        .unwrap();
    let before = view.selection(0).unwrap();
    for (node, character_index) in [
        (*id, run.character_lengths().len() + 1),
        (NodeId(u64::MAX), 0),
    ] {
        let point = accesskit::TextPosition {
            node,
            character_index,
        };
        let selection = accesskit::TextSelection {
            anchor: point,
            focus: point,
        };
        let value = request(
            Action::SetTextSelection,
            2,
            Some(ActionData::SetTextSelection(selection)),
        );
        assert_eq!(
            accessibility
                .apply(&mut view, snapshot.revision, &value)
                .unwrap(),
            Outcome::Ignored
        );
    }
    assert_eq!(view.selection(0).unwrap(), before);
}

#[test]
fn invalid_scale_is_rejected_without_poisoning_the_next_valid_projection() {
    let (mut view, mut accessibility) = ready();
    for scale in [f64::NAN, f64::INFINITY, 0., 9.] {
        assert!(accessibility.snapshot(&mut view, scale).is_err());
    }
    let snapshot = accessibility.snapshot(&mut view, 2.).unwrap();
    assert_eq!(snapshot.update.focus, NodeId(2));
    assert_eq!(view.text(0).unwrap(), "café 👨‍👩‍👧‍👦\nsecond line");
}

#[test]
fn setting_the_existing_value_is_not_an_extra_undo_transaction() {
    let (mut view, mut accessibility) = ready();
    let snapshot = accessibility.snapshot(&mut view, 1.).unwrap();
    let before = view.text(0).unwrap();
    let action = request(Action::SetValue, 2, Some(ActionData::Value(before.into())));
    assert_eq!(
        accessibility
            .apply(&mut view, snapshot.revision, &action)
            .unwrap(),
        Outcome::Ignored
    );
    assert_eq!(view.revision(), snapshot.revision);
    view.undo(false).unwrap();
    assert_eq!(view.text(0).unwrap(), "");
}

#[test]
fn complete_snapshot_contains_every_child_and_reuses_ids_without_duplicate_nodes() {
    let (mut view, mut accessibility) = ready();
    let mut previous = None;
    for _ in 0..2 {
        let snapshot = accessibility.snapshot(&mut view, 1.).unwrap();
        let ids: std::collections::BTreeSet<_> =
            snapshot.update.nodes.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids.len(), snapshot.update.nodes.len());
        if let Some(before) = previous.replace(ids.clone()) {
            assert_eq!(ids, before);
        }
        for (_, node) in &snapshot.update.nodes {
            for child in node.children() {
                assert!(ids.contains(child));
            }
        }
    }
}

#[test]
fn consecutive_absolute_selections_share_source_authority_but_publish_new_presentations() {
    let (mut view, mut accessibility) = ready();
    let snapshot = accessibility.snapshot(&mut view, 1.).unwrap();
    let (id, run) = snapshot
        .update
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::TextRun && node.character_lengths().len() > 1)
        .unwrap();
    let mut last = None;
    for character_index in [0, run.character_lengths().len()] {
        let point = accesskit::TextPosition {
            node: *id,
            character_index,
        };
        let selection = accesskit::TextSelection {
            anchor: point,
            focus: point,
        };
        let action = request(
            Action::SetTextSelection,
            2,
            Some(ActionData::SetTextSelection(selection)),
        );
        assert_eq!(
            accessibility
                .apply(&mut view, snapshot.revision, &action)
                .unwrap(),
            Outcome::Changed
        );
        assert_eq!(view.revision(), snapshot.revision);
        assert!(view.presentation_revision() > snapshot.generation);
        let observed = view.selection(0).unwrap();
        if let Some(before) = last {
            assert_ne!(before, observed);
        }
        last = Some(observed);
    }
    assert_eq!(view.text(0).unwrap(), "café 👨‍👩‍👧‍👦\nsecond line");
    view.undo(false).unwrap();
    assert_eq!(view.text(0).unwrap(), ""); // Selection created no history entry.
}

#[test]
fn geometry_and_field_switches_revoke_old_text_run_authority() {
    let (mut view, mut accessibility) = ready();
    let old = accessibility.snapshot(&mut view, 1.).unwrap();
    let action = request(Action::SetValue, 2, Some(ActionData::Value("stale".into())));
    view.resize([640., 480.]).unwrap();
    assert_eq!(
        accessibility
            .apply(&mut view, old.revision, &action)
            .unwrap(),
        Outcome::Ignored
    );
    let old = accessibility.snapshot(&mut view, 1.).unwrap();
    view.cycle_focus();
    view.cycle_focus();
    assert_eq!(
        accessibility
            .apply(&mut view, old.revision, &action)
            .unwrap(),
        Outcome::Ignored
    );
}

#[test]
fn multiline_values_include_empty_fields_and_preserve_exact_source_bytes() {
    let (mut view, mut accessibility) = ready();
    let snapshot = accessibility.snapshot(&mut view, 1.).unwrap();
    for (id, expected) in [(2, "café 👨‍👩‍👧‍👦\nsecond line"), (3, "")] {
        let node = &snapshot
            .update
            .nodes
            .iter()
            .find(|(key, _)| *key == NodeId(id))
            .unwrap()
            .1;
        assert_eq!(node.value(), Some(expected));
    }
}
