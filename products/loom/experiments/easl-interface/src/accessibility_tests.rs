use super::*;
use crate::{
    chrome_state::ADD_LABELS,
    interface::{INPUT_COUNT, Interface, SOURCE},
};

fn scene(add_open: bool) -> crate::interface::Scene {
    let mut input = [0.; INPUT_COUNT];
    input[..2].copy_from_slice(&[1280., 820.]);
    input[8..12].copy_from_slice(&[0., 0., 1., 19.]);
    input[12] = 1.;
    input[38] = 1.;
    input[48] = f32::from(add_open);
    Interface::compile(SOURCE).unwrap().step(input).unwrap()
}

#[test]
fn add_rows_are_named_menu_items_and_unconnected_services_have_no_actions() {
    let scene = scene(true);
    let workspace = crate::workspace::Workspace::new(loom_types::ProjectId::new());
    for (offset, label) in ADD_LABELS.iter().enumerate() {
        let key = 400 + u32::try_from(offset).unwrap();
        let control = scene.controls.iter().find(|c| c.key == key).unwrap();
        let node = control_node(control, &workspace, 2.);
        assert_eq!(node.role(), Role::MenuItem);
        assert_eq!(node.label(), Some(*label));
        assert_eq!(node.supports_action(Action::Click), offset == 0);
        assert_eq!(node.supports_action(Action::Focus), offset == 0);
        if offset != 0 {
            assert!(node.is_disabled());
            assert!(node.description().is_some());
        }
    }
}

#[test]
fn outline_and_add_expose_expanded_state_not_a_fake_pressed_toggle() {
    let scene = scene(true);
    let workspace = crate::workspace::Workspace::new(loom_types::ProjectId::new());
    for key in [1, 2] {
        let control = scene.controls.iter().find(|c| c.key == key).unwrap();
        let mut node = control_node(control, &workspace, 1.);
        decorate_chrome(&mut node, key, false, true);
        assert_eq!(node.is_expanded(), Some(key == 2));
        assert!(node.toggled().is_none());
    }
}

#[test]
fn main_pane_exposes_visibility_and_recording_has_a_specific_unavailable_reason() {
    let scene = scene(false);
    let workspace = crate::workspace::Workspace::new(loom_types::ProjectId::new());
    let main = control_node(
        scene.controls.iter().find(|c| c.key == 210).unwrap(),
        &workspace,
        1.,
    );
    assert_eq!(main.toggled(), Some(accesskit::Toggled::from(true)));
    let record = control_node(
        scene.controls.iter().find(|c| c.key == 3).unwrap(),
        &workspace,
        1.,
    );
    assert_eq!(
        record.description(),
        Some("Recording service is not connected.")
    );
    assert!(!record.supports_action(Action::Click));
}
