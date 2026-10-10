//! Native accessibility semantics are supplied alongside the EASL scene.
use crate::{App, interface::Draw};
use accesskit::{Action, ActionData, Node, NodeId, Role, Tree, TreeId, TreeUpdate};
use winit::event_loop::ActiveEventLoop;

enum TextAction<'a> {
    Focus,
    Selection(&'a accesskit::TextSelection),
    Value(&'a str),
}

fn text_action(action: Action, data: Option<&ActionData>) -> Option<TextAction<'_>> {
    match (action, data) {
        (Action::Focus, None) => Some(TextAction::Focus),
        (Action::SetTextSelection, Some(ActionData::SetTextSelection(selection))) => {
            Some(TextAction::Selection(selection))
        }
        (Action::SetValue, Some(ActionData::Value(value))) => Some(TextAction::Value(value)),
        _ => None,
    }
}

fn editor_node(field_id: u32, readonly: bool, rect: [f32; 4], scale: f64) -> Node {
    let mut node = Node::new(Role::MultilineTextInput);
    node.set_label(if field_id == 1 {
        "Manuscript"
    } else {
        "Pane editor"
    });
    node.add_action(Action::Focus);
    if readonly {
        node.set_read_only();
    } else {
        node.add_action(Action::SetTextSelection);
        node.add_action(Action::SetValue);
    }
    node.set_bounds(bounds(rect, 1.));
    node.set_transform(accesskit::Affine::scale(scale));
    node
}

struct EditorNodes<'a> {
    docs: &'a mut crate::document::Documents,
    system: &'a mut easl_native_text::TextSystem,
    next_id: &'a mut u64,
}

impl EditorNodes<'_> {
    fn append(
        &mut self,
        scene: &crate::interface::Scene,
        root: &mut Node,
        update: &mut TreeUpdate,
        readonly: bool,
        scale: f64,
    ) {
        for draw in &scene.draws {
            if let Draw::Editor(rect, style, _) = draw {
                let field_id = crate::interface::id(style[2]).unwrap_or_default();
                let Ok(field) = self.docs.field(field_id, self.system) else {
                    continue;
                };
                let id = NodeId(9 + u64::from(field_id));
                let mut node = editor_node(field_id, readonly, rect.0, scale);
                field.editor.accessibility(
                    self.system,
                    update,
                    &mut node,
                    self.next_id,
                    [
                        f64::from(rect.0[0]),
                        f64::from(rect.0[1] - field.editor.scroll),
                    ],
                );
                root.push_child(id);
                update.nodes.push((id, node));
            }
        }
    }
}

impl App {
    pub(crate) fn update_accessibility(&mut self) {
        let Some(native) = &mut self.native else {
            return;
        };
        let scale = native.window.scale_factor();
        let scene = &self.scene;
        let add_open = self.add_menu.is_open();
        let readonly = self.docs.transitioning() || self.docs.workspace.menu.is_some() || add_open;
        let docs = &mut self.docs;
        let system = &mut self.renderer.text;
        let next_id = &mut self.next_access_id;
        let palette = &mut self.format_palette;
        native.access.update_if_active(|| {
            let mut root = Node::new(Role::Window);
            root.set_label("Loom in EASL · Native interface experiment");
            let mut update = TreeUpdate {
                tree_id: TreeId::ROOT,
                nodes: vec![],
                tree: Some(Tree::new(NodeId(1))),
                focus: if self.state[2] == 0.
                    || self.focus.chrome == Some(crate::focus::Target::Window)
                {
                    NodeId(1)
                } else {
                    NodeId(9 + u64::from(crate::interface::id(self.state[2]).unwrap_or_default()))
                },
            };
            add_dividers(
                &mut root,
                &mut update,
                &scene.dividers,
                self.focus.divider(),
                scale,
            );
            let mut menu = Node::new(Role::ListBox);
            menu.set_label("Pane");
            let mut add_menu = Node::new(Role::Menu);
            add_menu.set_label("Add");
            for control in &scene.controls {
                let id = NodeId(1000 + u64::from(control.key));
                let mut node = control_node(control, &docs.workspace, scale);
                decorate_chrome(&mut node, control.key, self.state[0] > 0., add_open);
                if control.input
                    && let Some(palette) = palette.as_mut()
                    && let Some(Draw::Input(view)) = scene
                        .draws
                        .iter()
                        .find(|draw| matches!(draw, Draw::Input(view) if view.key == control.key))
                {
                    palette.destination.accessibility(
                        system,
                        &mut update,
                        &mut node,
                        next_id,
                        view,
                        scale,
                    );
                }
                if self.focus.control() == Some(control.key) {
                    update.focus = id;
                }
                let option = control.label_slot.filter(|slot| (240..248).contains(slot));
                if (400..404).contains(&control.key) {
                    add_menu.push_child(id);
                } else if let Some(slot) = option {
                    let highlighted = (slot - 240) as usize == docs.workspace.menu_cursor();
                    node.set_selected(highlighted);
                    if highlighted {
                        update.focus = id;
                    }
                    menu.push_child(id);
                } else {
                    root.push_child(id);
                }
                update.nodes.push((id, node));
            }
            if docs.workspace.menu.is_some() {
                root.push_child(NodeId(40));
                update.nodes.push((NodeId(40), menu));
            }
            if add_open {
                root.push_child(NodeId(41));
                update.nodes.push((NodeId(41), add_menu));
            }
            EditorNodes {
                docs,
                system,
                next_id,
            }
            .append(scene, &mut root, &mut update, readonly, scale);
            update.nodes.push((NodeId(1), root));
            update
        });
    }
    pub(crate) fn accessibility_event(
        &mut self,
        loop_: &ActiveEventLoop,
        event: accesskit_winit::Event,
    ) {
        if self
            .native
            .as_ref()
            .is_none_or(|n| n.window.id() != event.window_id)
        {
            return;
        }
        match event.window_event {
            accesskit_winit::WindowEvent::InitialTreeRequested => self.update_accessibility(),
            accesskit_winit::WindowEvent::ActionRequested(req) => {
                self.focus.record(format_args!(
                    "accessibility {:?} node={}",
                    req.action, req.target_node.0
                ));
                if (51..=53).contains(&req.target_node.0) {
                    self.accessible_resize(&req, loop_);
                } else if (1001..=2023).contains(&req.target_node.0) && self.docs.can_switch_views()
                {
                    let key = u32::try_from(req.target_node.0 - 1000).unwrap_or_default();
                    if self
                        .scene
                        .controls
                        .iter()
                        .any(|control| control.key == key && control.enabled)
                    {
                        if key == crate::formatting::DESTINATION {
                            let Some(action) = text_action(req.action, req.data.as_ref()) else {
                                return;
                            };
                            if self.format_palette.is_none() {
                                return;
                            }
                            self.focus_target(crate::focus::Target::Control(key), true);
                            if let Some(palette) = &mut self.format_palette {
                                let result = match action {
                                    TextAction::Focus => Ok(()),
                                    TextAction::Selection(selection) => palette
                                        .destination
                                        .field
                                        .select_accessible(&mut self.renderer.text, selection),
                                    TextAction::Value(value) => palette
                                        .destination
                                        .set_value(&mut self.renderer.text, value),
                                };
                                if let Err(error) = result {
                                    self.error(error);
                                }
                            }
                            self.update(0, loop_);
                        } else if req.action == Action::Focus {
                            self.focus_target(crate::focus::Target::Control(key), true);
                            self.update(0, loop_);
                        } else if req.action == Action::Click {
                            self.focus_target(crate::focus::Target::Control(key), true);
                            self.activate_control(key, loop_);
                        }
                    }
                } else if (10..=18).contains(&req.target_node.0) && !self.docs.transitioning() {
                    let Some(action) = text_action(req.action, req.data.as_ref()) else {
                        return;
                    };
                    let id = u32::try_from(req.target_node.0 - 9).unwrap_or_default();
                    if self.add_menu.is_open() || self.docs.workspace.menu.is_some() || !self.scene.draws.iter().any(|draw| {
                        matches!(draw, Draw::Editor(_, style, _) if crate::interface::id(style[2]).ok() == Some(id))
                    }) {
                        return;
                    }
                    self.state[2] = crate::interface::small_number(id);
                    self.clear_chrome_focus();
                    if let Ok(field) = self.docs.field(id, &mut self.renderer.text) {
                        let result = match action {
                            TextAction::Selection(selection) => {
                                field.select_accessible(&mut self.renderer.text, selection)
                            }
                            TextAction::Value(value) => {
                                field.set_accessible_value(&mut self.renderer.text, value)
                            }
                            TextAction::Focus => Ok(()),
                        };
                        if let Err(e) = result {
                            self.error(e);
                        }
                    }
                    self.update(0, loop_);
                }
            }
            accesskit_winit::WindowEvent::AccessibilityDeactivated => {}
        }
    }
}
fn add_dividers(
    root: &mut Node,
    update: &mut TreeUpdate,
    dividers: &[crate::pane_divider::Divider],
    focus: Option<crate::pane_divider::DividerId>,
    scale: f64,
) {
    for divider in dividers {
        let id = NodeId(50 + u64::from(divider.id as u16));
        let mut node = Node::new(Role::Splitter);
        node.set_label(divider.id.label());
        node.set_orientation(if divider.id == crate::pane_divider::DividerId::Bottom {
            accesskit::Orientation::Horizontal
        } else {
            accesskit::Orientation::Vertical
        });
        node.set_bounds(bounds(divider.rect.0, scale));
        node.set_numeric_value(f64::from(divider.size));
        node.set_min_numeric_value(f64::from(divider.min));
        node.set_max_numeric_value(f64::from(divider.max));
        node.set_numeric_value_step(10.);
        node.set_numeric_value_jump(40.);
        if divider.enabled {
            for action in [
                Action::Focus,
                Action::Click,
                Action::SetValue,
                Action::Increment,
                Action::Decrement,
            ] {
                node.add_action(action);
            }
        } else {
            node.set_disabled();
        }
        if focus == Some(divider.id) {
            update.focus = id;
        }
        root.push_child(id);
        update.nodes.push((id, node));
    }
}
fn bounds(rect: [f32; 4], scale: f64) -> accesskit::Rect {
    accesskit::Rect {
        x0: f64::from(rect[0]) * scale,
        y0: f64::from(rect[1]) * scale,
        x1: f64::from(rect[0] + rect[2]) * scale,
        y1: f64::from(rect[1] + rect[3]) * scale,
    }
}

fn control_node(
    control: &crate::interface::Control,
    workspace: &crate::workspace::Workspace,
    scale: f64,
) -> Node {
    let selector = control.label_slot.filter(|slot| (200..203).contains(slot));
    let option = control.label_slot.filter(|slot| (240..248).contains(slot));
    let mut node = Node::new(if selector.is_some() {
        Role::ComboBox
    } else if option.is_some() {
        Role::ListBoxOption
    } else if (400..404).contains(&control.key) {
        Role::MenuItem
    } else {
        Role::Button
    });
    if let Some(slot) = selector {
        node.set_label("Pane");
        node.set_value(control.label.as_str());
        node.set_expanded(workspace.menu == Some((slot - 200) as usize));
    } else {
        node.set_label(control.label.as_str());
    }
    if let Some(toggled) = control.toggled {
        node.set_toggled(accesskit::Toggled::from(toggled));
    }
    if control.input {
        node.set_role(Role::TextInput);
    }
    if control.enabled {
        if control.input {
            node.add_action(Action::SetValue);
            node.add_action(Action::SetTextSelection);
        } else {
            node.add_action(Action::Click);
        }
        node.add_action(Action::Focus);
    } else {
        node.set_disabled();
    }
    if !control.enabled
        && let Some(reason) = crate::chrome_state::unavailable_reason(control.key)
    {
        node.set_description(reason);
    }
    node.set_bounds(bounds(control.rect.0, scale));
    node
}

fn decorate_chrome(node: &mut Node, key: u32, outline_open: bool, add_open: bool) {
    match key {
        1 => node.set_expanded(outline_open),
        2 => node.set_expanded(add_open),
        _ => {}
    }
}

#[cfg(test)]
#[path = "accessibility_tests.rs"]
mod current_chrome_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_destination_is_a_text_input_and_disabled_commands_have_no_click_action() {
        let mut ui = crate::interface::Interface::compile(crate::interface::SOURCE).unwrap();
        let mut input = [0.; crate::interface::INPUT_COUNT];
        input[..2].copy_from_slice(&[1280., 820.]);
        input[8..12].copy_from_slice(&[0., 0., 1., 19.]);
        input[22] = 1.;
        let scene = ui.step(input).unwrap();
        let workspace = crate::workspace::Workspace::new(loom_types::ProjectId::new());
        let control = scene
            .controls
            .iter()
            .find(|c| c.key == crate::formatting::DESTINATION)
            .unwrap();
        let node = control_node(control, &workspace, 2.);
        assert_eq!(node.role(), Role::TextInput);
        assert_eq!(node.label(), Some("Link destination"));
        assert!(
            node.supports_action(Action::SetValue)
                && node.supports_action(Action::SetTextSelection)
        );
        assert!(!node.supports_action(Action::Click));
        for key in [310, 311] {
            let node = control_node(
                scene.controls.iter().find(|c| c.key == key).unwrap(),
                &workspace,
                2.,
            );
            assert!(!node.supports_action(Action::Click));
            assert!(node.is_disabled());
        }
    }
    #[test]
    fn formatting_controls_expose_names_and_toggle_states_without_changing_activation() {
        let mut ui = crate::interface::Interface::compile(crate::interface::SOURCE).unwrap();
        let mut input = [0.; crate::interface::INPUT_COUNT];
        input[..2].copy_from_slice(&[1280., 820.]);
        input[8..12].copy_from_slice(&[0., 0., 1., 19.]);
        input[22] = 1.;
        input[24] = 1. + 16.;
        let scene = ui.step(input).unwrap();
        let workspace = crate::workspace::Workspace::new(loom_types::ProjectId::new());
        for (key, label, active) in [
            (300, "Body", true),
            (304, "Bold", true),
            (305, "Italic", false),
        ] {
            let control = scene.controls.iter().find(|c| c.key == key).unwrap();
            let node = control_node(control, &workspace, 2.);
            assert_eq!(node.label(), Some(label));
            assert_eq!(node.toggled(), Some(accesskit::Toggled::from(active)));
            assert!(node.supports_action(Action::Click));
            assert!(node.supports_action(Action::Focus));
        }
    }
    #[test]
    fn unrelated_or_malformed_accessibility_actions_cannot_focus_or_edit_a_manuscript() {
        let value = ActionData::Value("replacement".into());
        for action in [Action::Click, Action::ScrollIntoView, Action::Blur] {
            assert!(text_action(action, None).is_none());
            assert!(text_action(action, Some(&value)).is_none());
        }
        assert!(text_action(Action::SetValue, None).is_none());
        assert!(text_action(Action::SetTextSelection, Some(&value)).is_none());
        assert!(text_action(Action::Focus, Some(&value)).is_none());
        assert!(matches!(
            text_action(Action::Focus, None),
            Some(TextAction::Focus)
        ));
        assert!(matches!(
            text_action(Action::SetValue, Some(&value)),
            Some(TextAction::Value("replacement"))
        ));
    }
}
