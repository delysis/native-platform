//! A projection and action gate over the existing text owners, never a second editor.
use crate::{Error, TwoFields};
use accesskit::{Action, ActionData, ActionRequest, Node, NodeId, Role, Tree, TreeId, TreeUpdate};
use std::collections::BTreeMap;

pub const ROOT: NodeId = NodeId(1);
pub const FIELDS: [NodeId; 2] = [NodeId(2), NodeId(3)];

#[derive(Debug)]
pub struct Snapshot {
    pub generation: u64,
    pub revision: u64,
    pub update: TreeUpdate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ignored,
    Changed,
    /// Request OS focus outside the host's mutable borrow. This is not proof of focus.
    FocusWindow,
}

#[derive(Debug)]
pub struct Accessibility {
    owner: Option<u64>,
    revision: Option<u64>,
    next_id: u64,
    text_runs: [BTreeMap<NodeId, usize>; 2],
}
impl Default for Accessibility {
    fn default() -> Self {
        Self {
            owner: None,
            revision: None,
            next_id: 1024,
            text_runs: Default::default(),
        }
    }
}
impl Accessibility {
    /// All IDs are window-local. A projection cannot be transferred to another view.
    pub fn snapshot(&mut self, view: &mut TwoFields, scale: f64) -> Result<Snapshot, Error> {
        if self.owner.is_some_and(|owner| owner != view.identity)
            || view.revision == u64::MAX
            || view.presentation == u64::MAX
            || self.next_id > u64::MAX - 4 * (easl_native_text::MAX_TEXT_BYTES as u64 + 1)
        {
            return Err(Error::Accessibility);
        }
        if !scale.is_finite() || !(0.25..=8.).contains(&scale) {
            return Err(Error::Geometry);
        }
        let boxes = view.boxes.ok_or(Error::Geometry)?;
        let mut root = Node::new(Role::Window);
        root.set_label("EASL native text");
        root.set_children(FIELDS);
        let mut update = TreeUpdate {
            tree_id: TreeId::ROOT,
            tree: Some(Tree::new(ROOT)),
            nodes: Vec::new(),
            focus: if view.window_focused {
                FIELDS[view.active]
            } else {
                ROOT
            },
        };
        let mut text_runs: [BTreeMap<NodeId, usize>; 2] = Default::default();
        for (index, (field, rect)) in view.fields.iter_mut().zip(boxes).enumerate() {
            let content = rect.content();
            field.ensure_layout(&mut view.system, &view.style, content.0[2], content.0[3])?;
            let mut node = Node::new(Role::MultilineTextInput);
            node.set_label(if index == 0 {
                "First text field"
            } else {
                "Second text field"
            });
            // AccessKit does not derive scalar values for multiline parents.
            // This immutable snapshot comes from the same canonical text owner.
            node.set_value(field.text());
            node.add_action(Action::Focus);
            node.add_action(Action::SetTextSelection);
            node.add_action(Action::SetValue);
            node.set_clips_children();
            let [x, y, width, height] = rect.0.map(f64::from);
            node.set_bounds(accesskit::Rect {
                x0: x,
                y0: y,
                x1: x + width,
                y1: y + height,
            });
            node.set_transform(accesskit::Affine::scale(scale));
            let start = update.nodes.len();
            field.accessibility(
                &mut view.system,
                &mut update,
                &mut node,
                &mut self.next_id,
                [
                    f64::from(content.0[0]),
                    f64::from(content.0[1] - field.scroll),
                ],
            );
            for (id, child) in &update.nodes[start..] {
                if child.role() == Role::TextRun {
                    text_runs[index].insert(*id, child.character_lengths().len());
                }
            }
            update.nodes.push((FIELDS[index], node));
        }
        update.nodes.push((ROOT, root));
        self.owner = Some(view.identity);
        self.revision = Some(view.revision);
        self.text_runs = text_runs;
        Ok(Snapshot {
            generation: view.presentation,
            revision: view.revision,
            update,
        })
    }

    /// A request must belong to the published view revision and its exact text runs.
    /// Field focus is never inferred from an OS focus *request*.
    pub fn apply(
        &self,
        view: &mut TwoFields,
        revision: u64,
        request: &ActionRequest,
    ) -> Result<Outcome, Error> {
        if request.target_tree != TreeId::ROOT
            || self.owner != Some(view.identity)
            || self.revision != Some(revision)
            || revision != view.revision
            || revision == u64::MAX
        {
            return Ok(Outcome::Ignored);
        }
        let Some(index) = FIELDS.iter().position(|id| *id == request.target_node) else {
            return Ok(Outcome::Ignored);
        };
        if matches!((&request.action, &request.data), (Action::Focus, None)) {
            if view.window_focused && view.active == index {
                return Ok(Outcome::Ignored);
            }
            view.fields[view.active].edit_context(&mut view.system, false)?;
            view.invalidate();
            view.active = index;
            view.capture = None;
            return Ok(Outcome::FocusWindow);
        }
        if !view.window_focused || view.active != index {
            return Ok(Outcome::Ignored);
        }
        match (&request.action, &request.data) {
            (Action::SetValue, Some(ActionData::Value(value))) => {
                let length = view.fields[index]
                    .edit_context(&mut view.system, false)?
                    .text_len;
                // replace_range validates the complete candidate before modifying
                // source, selection or history. One AX request is one undo step.
                easl_native_text::validate_text(value)?;
                if view.fields[index].equals(value) {
                    return Ok(Outcome::Ignored);
                }
                view.invalidate();
                view.fields[index].replace_range(
                    &mut view.system,
                    0..length,
                    value,
                    (value.len(), value.len()),
                )?;
                Ok(Outcome::Changed)
            }
            (Action::SetTextSelection, Some(ActionData::SetTextSelection(selection))) => {
                let belongs = |position: &accesskit::TextPosition| {
                    self.text_runs[index]
                        .get(&position.node)
                        .is_some_and(|length| position.character_index <= *length)
                };
                if !belongs(&selection.anchor) || !belongs(&selection.focus) {
                    return Ok(Outcome::Ignored);
                }
                view.fields[index].edit_context(&mut view.system, false)?;
                view.touch();
                view.fields[index].select_accessible(&mut view.system, selection);
                Ok(Outcome::Changed)
            }
            _ => Ok(Outcome::Ignored),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use easl_native_text::EditCommand;
    use easl_text::EditAction;

    #[test]
    fn accessibility_cannot_replace_an_active_composition() {
        let mut view = TwoFields::new().unwrap();
        view.resize([800., 600.]).unwrap();
        view.set_window_focus(true);
        view.edit(EditAction::Replace("retained source")).unwrap();
        let mut accessibility = Accessibility::default();
        let snapshot = accessibility.snapshot(&mut view, 1.).unwrap();
        // Exercise the actual underlying text owner, not a fabricated IME flag.
        // This is a component test; the OS preedit binding is still absent.
        view.fields[0]
            .command(&mut view.system, EditCommand::Preedit("候補".into(), None))
            .unwrap();
        let action = ActionRequest {
            action: Action::SetValue,
            target_tree: TreeId::ROOT,
            target_node: FIELDS[0],
            data: Some(ActionData::Value("must not replace composition".into())),
        };
        assert!(matches!(
            accessibility.apply(&mut view, snapshot.revision, &action),
            Err(Error::Text(easl_native_text::Error::CompositionActive))
        ));
        assert_eq!(view.text(0).unwrap(), "retained source");
        view.fields[0]
            .command(&mut view.system, EditCommand::CancelCompose)
            .unwrap();
        view.undo(false).unwrap();
        assert_eq!(view.text(0).unwrap(), "");
    }
}
