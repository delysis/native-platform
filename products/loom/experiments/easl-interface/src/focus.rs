//! Keyboard focus is independent of the last editing target. A focused control
//! never inherits authority to type into that editor when its scene is rebuilt.
use crate::{
    interface::{Draw, Rect, Scene},
    pane_divider::DividerId,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target {
    Editor(u32),
    Control(u32),
    Divider(DividerId),
    /// No editor authority; Tab/pointer input can choose a new visible target.
    Window,
}

#[derive(Debug)]
pub struct Focus {
    pub chrome: Option<Target>,
    pub visible: bool,
    trace: Option<std::fs::File>,
    trace_remaining: usize,
}
impl Default for Focus {
    fn default() -> Self {
        let trace = std::env::var_os("LOOM_EASL_FOCUS_TRACE").and_then(|path| {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options.open(path).ok()
        });
        Self {
            chrome: None,
            visible: false,
            trace,
            trace_remaining: 4096,
        }
    }
}
impl Focus {
    /// Opt-in, bounded event metadata for native focus investigations. Never
    /// records text, clipboard contents, file names or composition payloads.
    pub fn record(&mut self, event: impl std::fmt::Display) {
        use std::io::Write;
        if self.trace_remaining == 0 {
            return;
        }
        if let Some(file) = &mut self.trace {
            self.trace_remaining -= 1;
            let _ = writeln!(file, "{event} focus={:?}", self.chrome);
        }
    }
    pub fn divider(&self) -> Option<DividerId> {
        match self.chrome {
            Some(Target::Divider(id)) => Some(id),
            _ => None,
        }
    }
    pub fn control(&self) -> Option<u32> {
        match self.chrome {
            Some(Target::Control(id)) => Some(id),
            _ => None,
        }
    }
    pub fn clear(&mut self) {
        self.chrome = None;
        self.visible = false;
    }
    /// A select popup borrows focus from its selector. Removing the chosen
    /// option must not implicitly give the last manuscript permission to edit.
    pub fn menu_changed(&mut self, before: Option<usize>, after: Option<usize>) {
        if before != after
            && let Some(position) = after.or(before)
            && let Ok(position @ 0..=2) = u32::try_from(position)
        {
            self.chrome = Some(Target::Control(200 + position));
        }
    }
    pub fn close_format_menu(&mut self) {
        if self.control().is_some_and(|id| (300..=311).contains(&id)) {
            self.clear();
        }
    }
    pub fn reconcile(&mut self, scene: &Scene) -> bool {
        let Some(target) = self.chrome else {
            return false;
        };
        let present = match target {
            Target::Control(key) => scene.controls.iter().any(|control| control.key == key),
            Target::Divider(id) => scene.dividers.iter().any(|divider| divider.id == id),
            Target::Editor(id) => scene.draws.iter().any(|draw| {
                matches!(draw, Draw::Editor(_, style, _) if crate::interface::id(style[2]).ok() == Some(id))
            }),
            Target::Window => true,
        };
        if present {
            // Becoming disabled removes a control from traversal, not from its
            // focus lease. Ordinary typing must still not reach the manuscript.
            return false;
        }
        self.record("reconcile missing target");
        self.chrome = Some(
            targets(scene)
                .into_iter()
                .find(|target| matches!(target, Target::Control(_)))
                .unwrap_or(Target::Window),
        );
        self.visible = true;
        true
    }
}

pub fn contains(rect: Rect, point: [f32; 2]) -> bool {
    let Rect([x, y, w, h]) = rect;
    point[0] >= x && point[0] < x + w && point[1] >= y && point[1] < y + h
}

/// Current Loom's document order: titlebar, outline, main, right, bottom. Stable
/// view/control IDs keep focus attached when labels and sibling controls change.
pub fn targets(scene: &Scene) -> Vec<Target> {
    if scene
        .controls
        .iter()
        .any(|control| (400..404).contains(&control.key))
    {
        return scene
            .controls
            .iter()
            .filter(|control| control.enabled && (400..404).contains(&control.key))
            .map(|control| Target::Control(control.key))
            .collect();
    }
    let right = scene
        .dividers
        .iter()
        .find(|d| d.id == DividerId::Right)
        .map(|d| d.rect.0[0])
        .or_else(|| {
            scene.draws.iter().find_map(|draw| match draw {
                Draw::Slot(rect, _, 201) => Some(rect.0[0]),
                _ => None,
            })
        });
    let bottom = scene
        .dividers
        .iter()
        .find(|d| d.id == DividerId::Bottom)
        .map(|d| d.rect.0[1])
        .or_else(|| {
            scene.draws.iter().find_map(|draw| match draw {
                Draw::Slot(rect, _, 202) => Some(rect.0[1]),
                _ => None,
            })
        });
    let mut entries = Vec::new();
    for control in scene.controls.iter().filter(|control| control.enabled) {
        let lane = match control.key {
            100..=115 => 1,
            200 | 220 => 3,
            201 | 221 => 5,
            202 | 222 => 7,
            240..=247 => 8,
            300..=311 => 9,
            _ => 0,
        };
        let position = if lane == 0 {
            control.rect.0[0]
        } else {
            control.rect.0[1]
        };
        entries.push((lane, position, Target::Control(control.key)));
    }
    for divider in scene.dividers.iter().filter(|divider| divider.enabled) {
        let lane = match divider.id {
            DividerId::Outline => 2,
            DividerId::Right => 4,
            DividerId::Bottom => 6,
        };
        entries.push((lane, 0., Target::Divider(divider.id)));
    }
    for draw in &scene.draws {
        if let Draw::Editor(rect, style, _) = draw {
            let lane = if bottom.is_some_and(|y| rect.0[1] >= y) {
                7
            } else if right.is_some_and(|x| rect.0[0] >= x) {
                5
            } else {
                3
            };
            if let Ok(id) = crate::interface::id(style[2]) {
                entries.push((lane, rect.0[1], Target::Editor(id)));
            }
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.total_cmp(&b.1)));
    entries.into_iter().map(|(_, _, target)| target).collect()
}

pub fn adjacent(scene: &Scene, current: Target, backwards: bool) -> Option<Target> {
    let targets = targets(scene);
    if targets.is_empty() {
        return None;
    }
    let index = match targets.iter().position(|target| *target == current) {
        Some(index) if backwards => (index + targets.len() - 1) % targets.len(),
        Some(index) => (index + 1) % targets.len(),
        None if backwards => targets.len() - 1,
        None => 0,
    };
    Some(targets[index])
}

impl crate::App {
    pub(crate) fn focus_from_pointer(&mut self) {
        self.focus.record("pointer focus");
        if !self.docs.can_switch_views() {
            return;
        }
        if let Some(control) = self
            .scene
            .controls
            .iter()
            .rev()
            .find(|control| control.enabled && contains(control.rect, self.pointer))
        {
            self.focus_target(Target::Control(control.key), false);
        } else if self.docs.workspace.menu.is_none() && !self.add_menu.is_open() {
            self.clear_chrome_focus();
        }
    }
    pub(crate) fn clear_chrome_focus(&mut self) {
        self.focus.record("clear chrome");
        self.focus.clear();
        self.divider.release();
    }
    pub(crate) fn focus_target(&mut self, target: Target, visible: bool) {
        if target != Target::Control(crate::formatting::DESTINATION)
            && self.composing_field == Some(crate::formatting::DESTINATION)
            && let Some(palette) = &mut self.format_palette
            && let Err(error) = palette.destination.command(
                &mut self.renderer.text,
                easl_native_text::EditCommand::CancelCompose,
            )
        {
            self.error(error);
            return;
        }
        self.divider.release();
        self.dragging = false;
        if let Target::Editor(id) = target {
            self.focus.clear();
            self.state[2] = crate::interface::small_number(id);
        } else {
            self.focus.chrome = Some(target);
            self.focus.visible = visible;
        }
        self.focus.record(format_args!("target {target:?}"));
    }
    pub(crate) fn traverse_focus(&mut self, backwards: bool) {
        let current = self.focus.chrome.unwrap_or_else(|| {
            Target::Editor(crate::interface::id(self.state[2]).unwrap_or_default())
        });
        if let Some(next) = adjacent(&self.scene, current, backwards) {
            self.focus_target(next, true);
        }
    }
    pub(crate) fn activate_control(
        &mut self,
        key: u32,
        loop_: &winit::event_loop::ActiveEventLoop,
    ) {
        let Some(control) = self
            .scene
            .controls
            .iter()
            .find(|c| c.key == key && c.enabled)
        else {
            return;
        };
        let Rect([x, y, w, h]) = control.rect;
        // Activation uses the same scene hit and reducer as a pointer click.
        let pointer = self.pointer;
        self.pointer = [x + w * 0.5, y + h * 0.5];
        self.update(1, loop_);
        self.pointer = pointer;
        self.dragging = false;
        self.update(0, loop_);
    }
    pub(crate) fn chrome_key(&mut self, loop_: &winit::event_loop::ActiveEventLoop) -> bool {
        if self.docs.workspace.menu.is_some() {
            if self.key == 13 && !self.command() && !self.modifiers.alt_key() {
                if let Err(error) = self.pane_action(21, 13) {
                    self.error(error);
                } else {
                    // Closing the popup re-enables the underlying scene. Only
                    // then can Tab find the selector's actual next sibling.
                    self.update(0, loop_);
                    self.traverse_focus(self.modifiers.shift_key());
                }
                self.update(0, loop_);
                return true;
            }
            return false;
        }
        let Some(target) = self.focus.chrome else {
            return false;
        };
        self.focus.visible = true;
        // These global commands remain available without granting editor input.
        if self.command()
            && (matches!(self.key, 21..=24)
                || (matches!(self.key, 28 | 29) && self.modifiers.shift_key()))
        {
            return false;
        }
        if target == Target::Control(crate::formatting::DESTINATION) && !matches!(self.key, 13 | 14)
        {
            if let Err(error) = self.edit_input() {
                self.error(error);
            }
            self.update(0, loop_);
            return true;
        }
        match self.key {
            13 if !self.command() && !self.modifiers.alt_key() => {
                self.traverse_focus(self.modifiers.shift_key());
            }
            14 => {
                self.close_formatting();
                self.clear_chrome_focus();
            }
            _ => match target {
                Target::Divider(_) if !self.command() => self.resize_key(self.key),
                Target::Control(key) if !self.command() && (self.key == 2 || self.text == " ") => {
                    self.activate_control(key, loop_);
                }
                _ => {}
            },
        }
        self.update(0, loop_);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface::{INPUT_COUNT, Interface, SOURCE};

    fn scene() -> (Interface, [f32; INPUT_COUNT]) {
        let mut input = [0.; INPUT_COUNT];
        input[0..2].copy_from_slice(&[1200., 800.]);
        input[8] = 1.;
        input[10] = 1.;
        input[12] = 2.;
        input[27..31].fill(1.);
        input[34] = 2.;
        input[38] = 1.;
        (Interface::compile(SOURCE).unwrap(), input)
    }

    #[test]
    fn reference_order_reaches_every_visible_enabled_control_without_trapping_the_editor() {
        let (mut ui, input) = scene();
        let scene = ui.step(input).unwrap();
        let order = targets(&scene);
        assert_eq!(
            &order[..5],
            &[
                Target::Control(1),
                Target::Control(2),
                Target::Control(211),
                Target::Control(212),
                Target::Control(210),
            ]
        );
        assert!(order.contains(&Target::Control(100)));
        assert!(order.contains(&Target::Control(101)));
        assert!(!order.contains(&Target::Control(3)));
        let before = adjacent(&scene, Target::Editor(1), true).unwrap();
        assert_eq!(before, Target::Divider(DividerId::Outline));
        assert_eq!(adjacent(&scene, before, false), Some(Target::Editor(1)));
        let mut walked = Vec::new();
        let mut cursor = order[0];
        for _ in 0..order.len() {
            walked.push(cursor);
            cursor = adjacent(&scene, cursor, false).unwrap();
        }
        assert_eq!(walked, order);
        assert_eq!(cursor, order[0]);
    }

    #[test]
    fn format_destination_and_enabled_link_commands_follow_the_palette_tab_order() {
        let (mut ui, mut input) = scene();
        input[22] = 1.;
        let scene = ui.step(input).unwrap();
        assert_eq!(
            adjacent(&scene, Target::Control(308), false),
            Some(Target::Control(309))
        );
        assert!(!targets(&scene).contains(&Target::Control(310)));
        assert!(!targets(&scene).contains(&Target::Control(311)));
        input[46] = 1.;
        input[47] = 1.;
        let scene = ui.step(input).unwrap();
        for (from, to) in [(308, 309), (309, 310), (310, 311)] {
            assert_eq!(
                adjacent(&scene, Target::Control(from), false),
                Some(Target::Control(to))
            );
            assert_eq!(
                adjacent(&scene, Target::Control(to), true),
                Some(Target::Control(from))
            );
        }
        let mut focus = Focus {
            chrome: Some(Target::Control(309)),
            ..Default::default()
        };
        focus.close_format_menu();
        assert!(focus.chrome.is_none());
    }

    #[test]
    fn focused_control_survives_label_and_sibling_changes_and_blocks_easl_text_edits() {
        let (mut ui, mut input) = scene();
        let mut focus = Focus {
            chrome: Some(Target::Control(1)),
            visible: true,
            ..Default::default()
        };
        let before = ui.step(input).unwrap();
        input[8] = 0.;
        let after = ui.step(input).unwrap();
        assert_ne!(before.controls.len(), after.controls.len());
        assert!(!focus.reconcile(&after));
        assert_ne!(
            before.controls.iter().find(|c| c.key == 1).unwrap().label,
            after.controls.iter().find(|c| c.key == 1).unwrap().label
        );
        input[44] = 1.;
        input[45] = 1.;
        input[2] = 2.;
        for command in [0., 1.] {
            input[6] = command;
            for key in [1., 2., 3., 13., 16., 17., 18., 19., 20., 26., 27.] {
                input[5] = key;
                let scene = ui.step(input).unwrap();
                assert!(scene.actions.is_empty(), "key {key}, command {command}");
                assert!(
                    scene
                        .draws
                        .iter()
                        .all(|draw| !matches!(draw, Draw::Editor(_, style, _) if style[3] != 0.))
                );
            }
        }
        focus.chrome = Some(Target::Control(100));
        assert!(focus.reconcile(&after));
        assert_eq!(focus.control(), Some(1));
    }

    #[test]
    fn malformed_duplicate_control_identity_rejects_reload_before_activation() {
        let (mut ui, input) = scene();
        assert!(ui.step(input).is_ok());
        let duplicate = SOURCE.replace(
            "(control new-button can-add 2.)",
            "(control new-button can-add 1.)",
        );
        assert_ne!(
            duplicate, SOURCE,
            "negative-control anchor must actually change"
        );
        let error = Interface::compile(&duplicate)
            .unwrap()
            .step(input)
            .unwrap_err();
        assert!(error.contains("duplicate control identity"), "{error}");
    }

    #[test]
    fn clicking_a_control_while_dismissing_format_does_not_redirect_typing_to_the_editor() {
        let (mut ui, mut input) = scene();
        input[22] = 1.;
        let open = ui.step(input).unwrap();
        let toggle = open
            .controls
            .iter()
            .find(|control| control.key == 211)
            .unwrap();
        input[2] = 1.;
        input[3] = toggle.rect.0[0] + 12.;
        input[4] = toggle.rect.0[1] + 12.;
        input[44] = 211.;
        let clicked = ui.step(input).unwrap();
        assert_eq!(clicked.actions, [(14, 0), (17, 1)]);
        let mut focus = Focus {
            chrome: Some(Target::Control(211)),
            ..Default::default()
        };
        focus.close_format_menu();
        assert_eq!(focus.control(), Some(211));
        input[22] = 0.;
        input[29] = 0.;
        input[44] = focus.control().map_or(0., crate::interface::small_number);
        input[2] = 2.;
        input[5] = 1.;
        let closed = ui.step(input).unwrap();
        assert!(!focus.reconcile(&closed));
        assert!(closed.actions.is_empty());
        focus.chrome = Some(Target::Control(300));
        focus.close_format_menu();
        assert!(focus.chrome.is_none());
    }
}
