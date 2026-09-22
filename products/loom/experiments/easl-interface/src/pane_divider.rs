//! Native interaction for the view's bounded pane splitters. EASL supplies the
//! visible geometry and limits; this state owns no documents or configuration.
use crate::interface::Rect;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[repr(u16)]
pub enum DividerId {
    Outline = 1,
    Right = 2,
    Bottom = 3,
}
impl DividerId {
    pub fn from_id(id: u32) -> Result<Self, String> {
        match id {
            1 => Ok(Self::Outline),
            2 => Ok(Self::Right),
            3 => Ok(Self::Bottom),
            _ => Err("Unknown pane divider".into()),
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Outline => "Resize documents",
            Self::Right => "Resize right pane",
            Self::Bottom => "Resize bottom pane",
        }
    }
    fn coordinate(self, pointer: [f32; 2]) -> f32 {
        pointer[usize::from(self == Self::Bottom)]
    }
    fn direction(self) -> f32 {
        if self == Self::Outline { 1. } else { -1. }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Divider {
    pub id: DividerId,
    pub rect: Rect,
    pub size: f32,
    pub min: f32,
    pub max: f32,
    pub enabled: bool,
}
impl Divider {
    pub fn contains(self, point: [f32; 2]) -> bool {
        let [x, y, w, h] = self.rect.0;
        point[0] >= x && point[0] < x + w && point[1] >= y && point[1] < y + h
    }
    pub fn value(self, value: f64) -> Option<f32> {
        (self.enabled && value.is_finite()).then(|| {
            crate::interface::logical_pixels(
                value
                    .clamp(f64::from(self.min), f64::from(self.max))
                    .round(),
            )
        })
    }
    /// The reference uses 10 px steps, Shift for 40 px, Home/End for bounds.
    pub fn key(self, key: u32, shift: bool) -> Option<f32> {
        let delta = match (self.id == DividerId::Bottom, key) {
            (false, 5) | (true, 7) => -1.,
            (false, 6) | (true, 8) => 1.,
            _ => 0.,
        };
        let value = match key {
            9 => self.min,
            10 => self.max,
            _ if delta != 0. => {
                self.size + delta * self.id.direction() * if shift { 40. } else { 10. }
            }
            _ => return None,
        };
        self.value(f64::from(value))
    }
}

#[derive(Clone, Copy, Debug)]
struct Drag {
    id: DividerId,
    coordinate: f32,
    size: f32,
}
#[derive(Debug, Default)]
pub struct Interaction {
    drag: Option<Drag>,
}
impl Interaction {
    pub fn begin(&mut self, divider: Divider, pointer: [f32; 2]) {
        if divider.enabled {
            self.drag = Some(Drag {
                id: divider.id,
                coordinate: divider.id.coordinate(pointer),
                size: divider.size,
            });
        }
    }
    pub fn release(&mut self) {
        self.drag = None;
    }
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }
    pub fn motion(&self, dividers: &[Divider], pointer: [f32; 2]) -> Option<(DividerId, f32)> {
        let drag = self.drag?;
        let divider = dividers.iter().find(|d| d.id == drag.id)?;
        let value =
            drag.size + drag.id.direction() * (drag.id.coordinate(pointer) - drag.coordinate);
        divider.value(f64::from(value)).map(|size| (drag.id, size))
    }
}

impl crate::App {
    pub(crate) fn accessible_resize(
        &mut self,
        request: &accesskit::ActionRequest,
        loop_: &winit::event_loop::ActiveEventLoop,
    ) {
        use accesskit::{Action, ActionData};
        if !self.docs.can_switch_views() {
            return;
        }
        let Some(divider) = self
            .scene
            .dividers
            .iter()
            .find(|d| request.target_node.0 == 50 + u64::from(d.id as u16) && d.enabled)
            .copied()
        else {
            return;
        };
        let value = match request.action {
            Action::Focus | Action::Click => None,
            Action::Increment => Some(f64::from(divider.size) + 10.),
            Action::Decrement => Some(f64::from(divider.size) - 10.),
            Action::SetValue => match &request.data {
                Some(ActionData::NumericValue(value)) => Some(*value),
                Some(ActionData::Value(value)) => {
                    let Ok(value) = value.parse() else {
                        return;
                    };
                    Some(value)
                }
                _ => return,
            },
            _ => return,
        };
        if let Some(value) = value {
            let Some(size) = divider.value(value) else {
                return;
            };
            self.docs.workspace.resize(divider.id, size);
        }
        self.dragging = false;
        self.divider.release();
        self.focus_target(crate::focus::Target::Divider(divider.id), true);
        self.update(0, loop_);
    }
    pub(crate) fn begin_resize(&mut self, id: u32) -> Result<(), String> {
        if !self.docs.can_switch_views() {
            return Ok(());
        }
        let id = DividerId::from_id(id)?;
        let divider = self
            .scene
            .dividers
            .iter()
            .find(|d| d.id == id)
            .copied()
            .ok_or("Pane divider is not visible")?;
        self.dragging = false;
        self.focus_target(crate::focus::Target::Divider(id), false);
        self.divider.begin(divider, self.pointer);
        Ok(())
    }
    pub(crate) fn resize_key(&mut self, key: u32) {
        if matches!(key, 13 | 14) {
            self.clear_chrome_focus();
        } else if let Some(divider) = self
            .scene
            .dividers
            .iter()
            .find(|d| Some(d.id) == self.focus.divider())
            && let Some(size) = divider.key(key, self.modifiers.shift_key())
        {
            self.docs.workspace.resize(divider.id, size);
        }
    }
    pub(crate) fn move_divider(&mut self) {
        if let Some((id, size)) = self.divider.motion(&self.scene.dividers, self.pointer) {
            self.docs.workspace.resize(id, size);
        }
    }
    pub(crate) fn update_pointer_cursor(&mut self) {
        use winit::window::CursorIcon;
        if let Some(native) = &mut self.native {
            let input_focused = self.focus.control() == Some(crate::formatting::DESTINATION)
                && self.format_palette.is_some();
            let ime_allowed = self.docs.workspace.menu.is_none()
                && !self.add_menu.is_open()
                && (input_focused || (self.focus.chrome.is_none() && self.state[2] != 0.))
                && (self.composing_field != Some(crate::formatting::DESTINATION) || input_focused);
            if native.ime_allowed != ime_allowed {
                native.window.set_ime_allowed(ime_allowed);
                native.ime_allowed = ime_allowed;
            }
            let divider = if self.divider.dragging() {
                self.focus.divider()
            } else {
                self.scene
                    .dividers
                    .iter()
                    .find(|d| d.enabled && d.contains(self.pointer))
                    .map(|d| d.id)
            };
            let cursor = match divider {
                Some(DividerId::Bottom) => CursorIcon::RowResize,
                Some(_) => CursorIcon::ColResize,
                None => CursorIcon::Default,
            };
            native.window.set_cursor(cursor);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface::{Draw, INPUT_COUNT, Interface, SOURCE};

    fn input() -> [f32; INPUT_COUNT] {
        let mut input = [0.; INPUT_COUNT];
        input[0..2].copy_from_slice(&[1200., 800.]);
        input[8] = 1.;
        input[10] = 1.;
        input[27..31].fill(1.);
        input[34] = 2.;
        input[38] = 1.;
        input
    }
    fn pixels_equal(actual: f32, expected: f32) {
        // The EASL protocol carries f32; the web reference uses JS doubles.
        assert!((actual - expected).abs() < 0.001, "{actual} != {expected}");
    }

    #[test]
    fn actual_view_splitters_match_reference_limits_and_own_their_pointer_hits() {
        let mut ui = Interface::compile(SOURCE).unwrap();
        let mut input = input();
        let scene = ui.step(input).unwrap();
        assert_eq!(scene.dividers.len(), 3);
        for (id, min, max, size) in [
            (DividerId::Outline, 150., 420., 220.),
            (DividerId::Right, 180., 780., 320.),
            (DividerId::Bottom, 100., 462., 240.),
        ] {
            let divider = *scene.dividers.iter().find(|d| d.id == id).unwrap();
            pixels_equal(divider.min, min);
            pixels_equal(divider.max, max);
            pixels_equal(divider.size, size);
            input[2] = 1.;
            input[3] = divider.rect.0[0] + 3.;
            input[4] = divider.rect.0[1] + 3.;
            let click = ui.step(input).unwrap();
            assert_eq!(click.actions, [(22, u32::from(id as u16))]);
            assert!(
                click
                    .controls
                    .iter()
                    .filter(|c| c.label == "Add" || c.label_slot == Some(201))
                    .all(|c| c.enabled)
            );
            input[2] = 5.;
            assert_eq!(ui.step(input).unwrap().actions, [(10, 1)]);
            input[2] = 2.;
            input[43] = f32::from(id as u16);
            for key in [1_u16, 2, 5, 6, 7, 8, 9, 10, 13, 14, 19] {
                input[5] = f32::from(key);
                assert_eq!(ui.step(input).unwrap().actions, [(23, u32::from(key))]);
            }
            input[43] = 0.;
        }
        input[2] = 0.;
        input[36] = 900.;
        input[37] = 600.;
        input[42] = 800.;
        input[0..2].copy_from_slice(&[640., 480.]);
        let small = ui.step(input).unwrap();
        pixels_equal(small.dividers[0].size, 224.);
        pixels_equal(small.dividers[1].size, 216.);
        pixels_equal(small.dividers[2].size, 270.);
        assert!(small.draws.iter().all(|d| match d {
            Draw::Editor(r, _, _) =>
                r.0[0] >= 0. && r.0[1] >= 30. && r.0[0] + r.0[2] <= 640. && r.0[1] + r.0[3] <= 480.,
            _ => true,
        }));
        input[8] = 0.;
        input[29] = 0.;
        input[30] = 0.;
        assert!(ui.step(input).unwrap().dividers.is_empty());
    }

    #[test]
    fn drag_uses_visible_size_current_limits_and_stops_after_capture_ends() {
        let mut ui = Interface::compile(SOURCE).unwrap();
        let mut input = input();
        input[36] = 1000.;
        let scene = ui.step(input).unwrap();
        let divider = *scene
            .dividers
            .iter()
            .find(|d| d.id == DividerId::Right)
            .unwrap();
        let mut interaction = Interaction::default();
        interaction.begin(divider, [300., 90.]);
        let (_, size) = interaction.motion(&scene.dividers, [310., 400.]).unwrap();
        pixels_equal(size, 770.); // The visible 780 px, not the preferred 1000 px.
        let (_, size) = interaction.motion(&scene.dividers, [4000., 400.]).unwrap();
        pixels_equal(size, 180.);
        input[0] = 700.;
        let narrower = ui.step(input).unwrap();
        let (_, size) = interaction
            .motion(&narrower.dividers, [-300., 400.])
            .unwrap();
        pixels_equal(size, 280.);
        interaction.release();
        assert!(interaction.motion(&scene.dividers, [0., 0.]).is_none());
        assert!(!interaction.dragging());
        interaction.begin(
            Divider {
                enabled: false,
                ..divider
            },
            [0., 0.],
        );
        assert!(!interaction.dragging());
        assert!(divider.value(f64::NAN).is_none());
        assert!(divider.value(f64::INFINITY).is_none());
    }

    #[test]
    fn arrow_keys_home_and_end_follow_each_reference_edge() {
        let mut ui = Interface::compile(SOURCE).unwrap();
        let scene = ui.step(input()).unwrap();
        for divider in scene.dividers {
            let (forward, backward) = if divider.id == DividerId::Bottom {
                (8, 7)
            } else {
                (6, 5)
            };
            let direction = if divider.id == DividerId::Outline {
                1.
            } else {
                -1.
            };
            pixels_equal(
                divider.key(forward, false).unwrap(),
                divider.size + direction * 10.,
            );
            pixels_equal(
                divider.key(backward, true).unwrap(),
                divider.size - direction * 40.,
            );
            pixels_equal(divider.key(9, false).unwrap(), divider.min);
            pixels_equal(divider.key(10, false).unwrap(), divider.max);
            assert!(divider.key(1, false).is_none());
        }
    }
}
