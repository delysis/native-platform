//! A bounded display-list protocol. The application layout and reducer live in EASL.
use std::{path::PathBuf, sync::Arc};

use easl::{
    CompilerTarget,
    compiler::{builtins::built_in_macros, program::Program},
    external::ExternalVars,
    interpreter::{
        BufferUpload, EvalError, FrameDriver, IOManager, UserspaceEvalError, VmCpuRuntime,
        WindowEvent,
    },
    parse::{EaslMultiDocument, parse_easl_without_comments},
};
use serde::Serialize;

pub const SOURCE: &str = include_str!("../ui/loom.easl");
pub const SOURCE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/ui/loom.easl");
pub const MAX_SOURCE: usize = 64 * 1024;
pub const INPUT_COUNT: usize = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Rect(pub [f32; 4]);

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum Draw {
    Rect(Rect, [f32; 4]),
    RoundedRect(Rect, [f32; 4], f32),
    Icon(Rect, [f32; 4], [f32; 4]),
    Drag(Rect),
    WebView(Rect),
    Text(Rect, [f32; 4], String),
    Slot(Rect, [f32; 4], u32),
    StyledText(Rect, [f32; 4], u32, bool, String),
    Input(InputView),
    Editor(Rect, [f32; 4], [f32; 4]),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InputView {
    pub rect: Rect,
    pub content: Rect,
    pub key: u32,
    pub typography: u32,
    pub focused: bool,
    pub color: [f32; 4],
    pub placeholder: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Scene {
    pub dividers: Vec<crate::pane_divider::Divider>,
    pub controls: Vec<Control>,
    pub draws: Vec<Draw>,
    pub typography: Vec<(u32, easl_native_text::TextStyle)>,
    pub paragraphs: Vec<(u32, ParagraphStyle)>,
    pub selection_color: Option<[f32; 4]>,
    pub state: [f32; 4],
    pub actions: Vec<(u32, u32)>,
}

/// EASL owns paragraph appearance; the native editor resolves it into line boxes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct ParagraphStyle {
    pub inset_left: f32,
    pub inset_right: f32,
    pub first_line_indent: f32,
    pub space_before: f32,
    pub space_after: f32,
}
impl ParagraphStyle {
    pub fn at(self, start: usize) -> easl_native_text::ParagraphLayout {
        easl_native_text::ParagraphLayout {
            start,
            inset_left: self.inset_left,
            inset_right: self.inset_right,
            first_line_indent: self.first_line_indent,
            space_before: self.space_before,
            space_after: self.space_after,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Control {
    pub key: u32,
    pub rect: Rect,
    pub label: String,
    pub enabled: bool,
    pub toggled: Option<bool>,
    pub input: bool,
    pub label_slot: Option<u32>,
}

struct SceneIo {
    lines: Vec<String>,
    budget: u32,
    overflow: bool,
}

fn runtime_error(message: &str) -> EvalError {
    UserspaceEvalError::RuntimeError(message.into()).into()
}

impl IOManager for SceneIo {
    fn check_execution(&mut self) -> Result<(), EvalError> {
        self.budget = self
            .budget
            .checked_sub(1)
            .ok_or_else(|| runtime_error("EASL interface instruction budget exhausted"))?;
        if self.overflow {
            return Err(runtime_error("EASL display list exceeded its bound"));
        }
        Ok(())
    }
    fn println(&mut self, text: &str) {
        if self.lines.len() >= 1024 || text.len() > 1024 {
            self.overflow = true;
        } else {
            self.lines.push(text.into());
        }
    }
    fn record_draw(
        &mut self,
        _: u16,
        _: u16,
        _: &str,
        _: &str,
        _: u32,
        _: Vec<((u8, u8), BufferUpload)>,
        _: easl::interpreter::RenderBlend,
        _: Option<(u8, u8)>,
    ) -> Result<(), EvalError> {
        Err(runtime_error("Use interface drawing primitives"))
    }
    fn record_compute(
        &mut self,
        _: u16,
        _: &str,
        _: (u32, u32, u32),
        _: Vec<((u8, u8), BufferUpload)>,
    ) -> Result<(), EvalError> {
        Err(runtime_error("Compute is outside the interface protocol"))
    }
    fn take_frame_draw_calls(&mut self) -> Vec<WindowEvent> {
        Vec::new()
    }
    fn record_close_window(&mut self) {}
    fn sync_gpu_to_cpu(&mut self, _: u8, _: u8, _: u64) -> Option<Vec<u8>> {
        None
    }
    fn run_spawn_window_driver<D: FrameDriver<IO = Self>>(_: &mut D) -> Result<bool, EvalError> {
        Err(runtime_error("The native host owns the window"))
    }
}

pub struct Interface {
    runtime: VmCpuRuntime<SceneIo>,
    external: Arc<ExternalVars>,
    faulted: bool,
}

impl std::fmt::Debug for Interface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Interface").finish_non_exhaustive()
    }
}

impl Interface {
    pub fn compile(source: &str) -> Result<Self, String> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Self::compile_inner(source)))
            .map_err(|_| {
                "EASL compiler rejected this program; previous interface retained".to_owned()
            })?
    }
    fn compile_inner(source: &str) -> Result<Self, String> {
        if source.len() > MAX_SOURCE {
            return Err("Interface source exceeds 64 KiB".into());
        }
        let document = parse_easl_without_comments(source);
        if !document.parsing_failures.is_empty() {
            return Err(format!("EASL parse error: {:?}", document.parsing_failures));
        }
        let documents =
            EaslMultiDocument::from_singular_document(document, "loom.easl".into(), source.into());
        let (mut program, errors) = Program::from_easl_documents(&documents, built_in_macros());
        if !errors.is_empty() {
            return Err(format!("EASL compile error: {errors:?}"));
        }
        let errors = program.validate_raw_program(CompilerTarget::WGSL);
        if !errors.is_empty() {
            return Err(format!("EASL type error: {errors:?}"));
        }
        let external = ExternalVars::new(&program);
        let io = SceneIo {
            lines: Vec::new(),
            budget: 4096,
            overflow: false,
        };
        let runtime = VmCpuRuntime::new_with_external(
            program,
            io,
            None::<PathBuf>,
            None,
            Some(external.clone()),
        )
        .map_err(|e| e.to_string())?;
        Ok(Self {
            runtime,
            external,
            faulted: false,
        })
    }

    pub fn step(&mut self, input: [f32; crate::interface::INPUT_COUNT]) -> Result<Scene, String> {
        if self.faulted {
            return Err("EASL runtime faulted; reload a valid interface".into());
        }
        if let Ok(result) =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.step_inner(input)))
        {
            result
        } else {
            self.faulted = true;
            Err("EASL runtime fault; writing and last scene retained".into())
        }
    }
    fn step_inner(&mut self, input: [f32; crate::interface::INPUT_COUNT]) -> Result<Scene, String> {
        if input.iter().any(|v| !v.is_finite()) {
            return Err("Nonfinite interface input".into());
        }
        let words = input.map(f32::to_bits);
        self.external
            .write_external_var_raw("input", &words)
            .map_err(|e| e.to_string())?;
        self.runtime.env.io.lines.clear();
        self.runtime.env.io.budget = 4096;
        self.runtime.env.io.overflow = false;
        self.runtime.run("main").map_err(|e| e.to_string())?;
        if self.runtime.env.io.overflow {
            return Err("Interface output overflow".into());
        }
        decode(&self.runtime.env.io.lines)
    }
}

fn vec4(text: &str) -> Result<[f32; 4], String> {
    let text = text
        .strip_prefix("(vec4f ")
        .and_then(|s| s.strip_suffix(')'))
        .ok_or_else(|| format!("Expected vec4f, got {text}"))?;
    let values = text
        .split_whitespace()
        .map(str::parse::<f32>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let values: [f32; 4] = values.try_into().map_err(|_| "Expected four components")?;
    if values.iter().any(|v| !v.is_finite() || v.abs() > 100_000.) {
        return Err("Invalid interface geometry".into());
    }
    Ok(values)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn id(value: f32) -> Result<u32, String> {
    if !(0. ..=4096.).contains(&value) || value.fract() != 0. {
        Err("Invalid interface identifier".into())
    } else {
        Ok(value as u32)
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "One bounded protocol decoder keeps command validation together"
)]
fn decode(lines: &[String]) -> Result<Scene, String> {
    let mut scene = Scene::default();
    let mut lines = lines.iter();
    let mut has_state = false;
    while let Some(kind) = lines.next() {
        let a = vec4(lines.next().ok_or("Incomplete interface command")?)?;
        match kind.as_str() {
            "selection" => {
                validate_color(a)?;
                if scene.selection_color.replace(a).is_some() {
                    return Err("Duplicate selection color".into());
                }
            }
            "paragraph" => {
                let b = vec4(lines.next().ok_or("Missing paragraph spacing")?)?;
                if scene.paragraphs.len() >= 16
                    || [a[1], a[2], b[0], b[1]]
                        .iter()
                        .any(|n| !(0. ..=512.).contains(n))
                    || !(-512. ..=512.).contains(&a[3])
                    || a[1] + a[3] < 0.
                {
                    return Err("Invalid paragraph style".into());
                }
                let identity = id(a[0])?;
                if scene.paragraphs.iter().any(|(key, _)| *key == identity) {
                    return Err("Duplicate paragraph style".into());
                }
                scene.paragraphs.push((
                    identity,
                    ParagraphStyle {
                        inset_left: a[1],
                        inset_right: a[2],
                        first_line_indent: a[3],
                        space_before: b[0],
                        space_after: b[1],
                    },
                ));
            }
            "divider" => {
                use crate::pane_divider::{Divider, DividerId};
                let state = vec4(lines.next().ok_or("Missing divider state")?)?;
                let flags = vec4(lines.next().ok_or("Missing divider flags")?)?;
                let identity = DividerId::from_id(id(state[0])?)?;
                if scene.dividers.iter().any(|d| d.id == identity)
                    || a[2] <= 0.
                    || a[3] <= 0.
                    || state[2] <= 0.
                    || state[3] < state[2]
                    || !(state[2]..=state[3]).contains(&state[1])
                    || ![0., 1.].contains(&flags[0])
                {
                    return Err("Invalid pane divider geometry or range".into());
                }
                scene.dividers.push(Divider {
                    id: identity,
                    rect: Rect(a),
                    size: state[1],
                    min: state[2],
                    max: state[3],
                    enabled: flags[0] > 0.,
                });
            }
            "webview" => {
                if scene.draws.iter().any(|d| matches!(d, Draw::WebView(_))) {
                    return Err("Only one companion webview is supported".into());
                }
                validate_rect(Rect(a))?;
                scene.draws.push(Draw::WebView(Rect(a)));
            }
            "control" | "control-slot" | "toggle-slot" => {
                if scene.controls.len() >= 64 {
                    return Err("Too many controls".into());
                }
                let state = vec4(lines.next().ok_or("Missing control state")?)?;
                let slot_toggle = kind == "toggle-slot";
                let label_slot = if kind != "control" {
                    Some(id(state[1])?)
                } else {
                    None
                };
                if label_slot
                    .is_some_and(|slot| !((100..116).contains(&slot) || (200..248).contains(&slot)))
                    || (slot_toggle && !label_slot.is_some_and(|slot| (210..=212).contains(&slot)))
                {
                    return Err("Unknown control label slot".into());
                }
                if ![0., 1.].contains(&state[0])
                    || ![0., 1.].contains(&state[3])
                    || (!slot_toggle
                        && state[3] > 0.
                        && (label_slot.is_some() || ![0., 1.].contains(&state[1])))
                {
                    return Err("Invalid control toggle state".into());
                }
                scene.controls.push(Control {
                    key: id(state[2])?,
                    rect: Rect(a),
                    label: if label_slot.is_none() {
                        lines.next().ok_or("Missing control label")?.clone()
                    } else {
                        String::new()
                    },
                    enabled: state[0] > 0.,
                    toggled: if slot_toggle {
                        Some(state[3] > 0.)
                    } else {
                        (state[3] > 0.).then_some(state[1] > 0.)
                    },
                    input: false,
                    label_slot,
                });
            }
            "styled-text" => {
                let color = vec4(lines.next().ok_or("Missing styled label color")?)?;
                let style = vec4(lines.next().ok_or("Missing label role")?)?;
                if ![0., 1.].contains(&style[1]) {
                    return Err("Invalid label alignment".into());
                }
                scene.draws.push(Draw::StyledText(
                    Rect(a),
                    color,
                    id(style[0])?,
                    style[1] > 0.,
                    lines.next().ok_or("Missing styled label")?.clone(),
                ));
            }
            "input" => {
                let content = Rect(vec4(lines.next().ok_or("Missing input content bounds")?)?);
                let style = vec4(lines.next().ok_or("Missing input identity")?)?;
                let color = vec4(lines.next().ok_or("Missing input color")?)?;
                let label = lines.next().ok_or("Missing input label")?.clone();
                let placeholder = lines.next().ok_or("Missing input placeholder")?.clone();
                if scene.controls.len() >= 64 || ![0., 1.].contains(&style[2]) {
                    return Err("Invalid input semantics".into());
                }
                let key = id(style[0])?;
                scene.controls.push(Control {
                    key,
                    rect: Rect(a),
                    label,
                    enabled: true,
                    toggled: None,
                    input: true,
                    label_slot: None,
                });
                scene.draws.push(Draw::Input(InputView {
                    rect: Rect(a),
                    content,
                    key,
                    typography: id(style[1])?,
                    focused: style[2] > 0.,
                    color,
                    placeholder,
                }));
            }
            "drag" => scene.draws.push(Draw::Drag(Rect(a))),
            "round" => {
                let color = vec4(lines.next().ok_or("Missing rounded rectangle color")?)?;
                let radius = vec4(lines.next().ok_or("Missing corner radius")?)?[0];
                if !(0. ..=512.).contains(&radius) {
                    return Err("Invalid corner radius".into());
                }
                scene.draws.push(Draw::RoundedRect(Rect(a), color, radius));
            }
            "icon" => {
                let color = vec4(lines.next().ok_or("Missing icon color")?)?;
                let style = vec4(lines.next().ok_or("Missing icon style")?)?;
                if id(style[0])? as usize >= crate::icon::KEYS.len()
                    || !(0. ..=32.).contains(&style[1])
                    || ![0., 1.].contains(&style[2])
                {
                    return Err("Invalid icon style".into());
                }
                scene.draws.push(Draw::Icon(Rect(a), color, style));
            }
            "typography" => {
                let b = vec4(lines.next().ok_or("Missing typography spacing")?)?;
                let family = lines.next().ok_or("Missing font family")?.clone();
                let features = lines.next().ok_or("Missing font features")?.clone();
                let variations = lines.next().ok_or("Missing font variations")?.clone();
                let style = easl_native_text::TextStyle {
                    family,
                    size: a[1],
                    line_height: a[2],
                    weight: a[3],
                    letter_spacing: b[0],
                    word_spacing: b[1],
                    italic: b[2] > 0.,
                    underline: b[3] > 0.,
                    features,
                    variations,
                    ..Default::default()
                };
                style.validate().map_err(|e| e.to_string())?;
                if scene.typography.len() >= 32 {
                    return Err("Too many typography styles".into());
                }
                scene.typography.push((id(a[0])?, style));
            }
            "state" => {
                if has_state {
                    return Err("Duplicate interface state".into());
                }
                scene.state = a;
                has_state = true;
            }
            "action" => {
                if a[2] != 0. || a[3] != 0. {
                    return Err("Nonzero reserved action channels".into());
                }
                let action = crate::actions::Action::from_wire(id(a[0])?, id(a[1])?)?;
                scene.actions.push(action.wire());
            }
            "rect" | "text" | "slot" | "editor" => {
                let b = vec4(lines.next().ok_or("Missing style")?)?;
                let draw = match kind.as_str() {
                    "rect" => Draw::Rect(Rect(a), b),
                    "text" => Draw::Text(Rect(a), b, lines.next().ok_or("Missing label")?.clone()),
                    "slot" => Draw::Slot(
                        Rect(a),
                        b,
                        id(vec4(lines.next().ok_or("Missing slot")?)?[0])?,
                    ),
                    _ => Draw::Editor(
                        Rect(a),
                        b,
                        vec4(lines.next().ok_or("Missing editor style")?)?,
                    ),
                };
                scene.draws.push(draw);
            }
            _ => return Err(format!("Unknown interface command: {kind}")),
        }
        if scene.draws.len() > 256 || scene.actions.len() > 4 {
            return Err("Interface command limit exceeded".into());
        }
    }
    if !has_state
        || scene.state[..2].iter().any(|v| ![0., 1.].contains(v))
        || !(0..=9).contains(&id(scene.state[2])?)
        || !(14. ..=32.).contains(&scene.state[3])
    {
        return Err("Invalid interface state".into());
    }
    let mut control_keys = std::collections::BTreeSet::new();
    for control in &scene.controls {
        if !(1..=1023).contains(&control.key) || !control_keys.insert(control.key) {
            return Err("Invalid or duplicate control identity".into());
        }
        validate_rect(control.rect)?;
    }
    let mut fields = Vec::new();
    for draw in &scene.draws {
        match draw {
            Draw::WebView(rect) | Draw::Drag(rect) => validate_rect(*rect)?,
            Draw::Rect(rect, color)
            | Draw::RoundedRect(rect, color, _)
            | Draw::Icon(rect, color, _) => {
                validate_rect(*rect)?;
                validate_color(*color)?;
            }
            Draw::Text(rect, color, _) | Draw::Slot(rect, color, _) => {
                validate_rect(*rect)?;
                if !(1. ..=512.).contains(&rect.0[3])
                    || color[..3].iter().any(|v| !(0. ..=1.).contains(v))
                {
                    return Err("Invalid label typography".into());
                }
            }
            Draw::StyledText(rect, color, role, _, _) => {
                validate_rect(*rect)?;
                validate_color(*color)?;
                if !scene.typography.iter().any(|(id, _)| id == role) {
                    return Err("Unknown label typography".into());
                }
            }
            Draw::Input(view) => {
                validate_rect(view.rect)?;
                validate_rect(view.content)?;
                validate_color(view.color)?;
                if !scene
                    .typography
                    .iter()
                    .any(|(id, _)| *id == view.typography)
                    || view.content.0[0] < view.rect.0[0]
                    || view.content.0[1] < view.rect.0[1]
                    || view.content.0[0] + view.content.0[2] > view.rect.0[0] + view.rect.0[2]
                    || view.content.0[1] + view.content.0[3] > view.rect.0[1] + view.rect.0[3]
                {
                    return Err("Invalid input typography or bounds".into());
                }
            }
            Draw::Editor(rect, style, color) => {
                validate_rect(*rect)?;
                validate_color(*color)?;
                if !(1. ..=512.).contains(&style[0])
                    || !(1. ..=2048.).contains(&style[1])
                    || !(1..=9).contains(&id(style[2])?)
                    || fields.contains(&style[2])
                {
                    return Err("Invalid editor typography or identity".into());
                }
                fields.push(style[2]);
            }
        }
    }
    if scene.state[2] != 0. && !fields.contains(&scene.state[2]) {
        return Err("Focused editor must be visible".into());
    }
    Ok(scene)
}
fn validate_rect(rect: Rect) -> Result<(), String> {
    if rect.0[2] < 0. || rect.0[3] < 0. {
        Err("Negative display-list extent".into())
    } else {
        Ok(())
    }
}
fn validate_color(color: [f32; 4]) -> Result<(), String> {
    if color.iter().any(|v| !(0. ..=1.).contains(v)) {
        Err("Invalid display-list color".into())
    } else {
        Ok(())
    }
}

/// IDs in the EASL ABI are bounded integers carried in f32 slots.
pub fn small_number(value: u32) -> f32 {
    f32::from(u16::try_from(value).expect("EASL protocol integer exceeds its bound"))
}
/// Native display coordinates tolerate f32 precision at the validated size limit.
#[allow(clippy::cast_possible_truncation)]
pub fn logical_pixels(value: f64) -> f32 {
    value as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input() -> [f32; crate::interface::INPUT_COUNT] {
        let mut i = [0.; crate::interface::INPUT_COUNT];
        i[0] = 1200.;
        i[1] = 800.;
        i[8] = 0.;
        i[9] = 0.;
        i[10] = 1.;
        i[11] = 19.;
        i[12] = 3.;
        i
    }
    #[test]
    fn actual_easl_layout_reacts_to_pointer_and_resize() {
        let mut ui = Interface::compile(SOURCE).unwrap();
        let mut i = input();
        i[8] = 1.;
        let wide = ui.step(i).unwrap();
        assert!(
            wide.draws
                .iter()
                .any(|d| matches!(d, Draw::Editor(_, _, _)))
        );
        i[2] = 1.;
        let outline = wide
            .controls
            .iter()
            .find(|c| c.label == "Close manuscript outline")
            .unwrap();
        i[3] = outline.rect.0[0] + 12.;
        i[4] = outline.rect.0[1] + 12.;
        let clicked = ui.step(i).unwrap();
        assert_eq!(id(clicked.state[0]).unwrap(), 0);
        i[8..12].copy_from_slice(&clicked.state);
        i[2] = 0.;
        i[0] = 640.;
        let narrow = ui.step(i).unwrap();
        assert!(narrow.draws.len() < wide.draws.len());
    }
    #[test]
    fn pointer_motion_only_invalidates_changed_hover_geometry() {
        let mut ui = Interface::compile(SOURCE).unwrap();
        let mut i = input();
        i[3] = 400.;
        i[4] = 300.;
        let baseline = ui.step(i).unwrap();
        for x in 401_u16..501 {
            i[3] = f32::from(x);
            assert_eq!(ui.step(i).unwrap(), baseline);
        }
        let control = baseline.controls.iter().find(|c| c.label == "Add").unwrap();
        i[3] = control.rect.0[0] + 10.;
        i[4] = control.rect.0[1] + 10.;
        let hovered = ui.step(i).unwrap();
        assert_ne!(hovered, baseline);
        i[3] += 1.;
        assert_eq!(ui.step(i).unwrap(), hovered);
    }
    #[test]
    fn easl_routes_save_and_text_input_without_mutating_documents() {
        let mut ui = Interface::compile(SOURCE).unwrap();
        let mut i = input();
        i[2] = 2.;
        i[5] = 21.;
        i[6] = 1.;
        assert_eq!(ui.step(i).unwrap().actions, vec![(4, 0)]);
        i[5] = 1.;
        i[6] = 0.;
        assert_eq!(ui.step(i).unwrap().actions, vec![(1, 1)]);
    }
    #[test]
    fn current_source_and_format_shortcuts_work_from_editor_and_chrome_focus() {
        let mut ui = Interface::compile(SOURCE).unwrap();
        let mut i = input();
        i[2] = 2.;
        i[6] = 1.;
        i[7] = 1.;
        for chrome in [0., 1., 201.] {
            i[44] = chrome;
            i[5] = 28.;
            assert_eq!(ui.step(i).unwrap().actions, [(11, 0)]);
            i[5] = 29.;
            for (open, action) in [(0., 1), (1., 0)] {
                i[22] = open;
                assert_eq!(ui.step(i).unwrap().actions, [(14, action)]);
            }
            i[5] = 25.;
            // Cmd-Shift-B no longer toggles the outline or formats text.
            assert!(ui.step(i).unwrap().actions.is_empty());
        }
    }
    #[test]
    fn link_controls_have_separate_input_focus_enabled_states_and_reference_typography() {
        let mut ui = Interface::compile(SOURCE).unwrap();
        let mut i = input();
        i[22] = 1.;
        let scene = ui.step(i).unwrap();
        let control = scene.controls.iter().find(|c| c.key == 309).unwrap();
        assert!(control.input && control.enabled);
        assert_eq!(control.label, "Link destination");
        for key in [310, 311] {
            assert!(
                !scene
                    .controls
                    .iter()
                    .find(|c| c.key == key)
                    .unwrap()
                    .enabled
            );
        }
        let view = scene
            .draws
            .iter()
            .find_map(|d| {
                if let Draw::Input(v) = d {
                    Some(v)
                } else {
                    None
                }
            })
            .unwrap();
        i[2] = 1.;
        i[3] = view.rect.0[0] + 4.;
        i[4] = view.rect.0[1] + 4.;
        i[44] = 309.;
        assert_eq!(ui.step(i).unwrap().actions, [(24, 0)]);
        // Dragging outside the palette remains owned by the URL input.
        i[2] = 5.;
        i[3] = 100.;
        i[4] = 300.;
        assert_eq!(ui.step(i).unwrap().actions, [(24, 1)]);
        i[2] = 2.;
        i[5] = 1.;
        assert!(ui.step(i).unwrap().actions.is_empty());
        i[2] = 0.;
        i[46] = 1.;
        i[47] = 1.;
        let scene = ui.step(i).unwrap();
        for (key, action) in [(310, 9), (311, 10)] {
            let c = scene.controls.iter().find(|c| c.key == key).unwrap();
            i[2] = 1.;
            i[3] = c.rect.0[0] + 3.;
            i[4] = c.rect.0[1] + 3.;
            assert_eq!(ui.step(i).unwrap().actions, [(12, action)]);
        }
        for (text, bold, italic) in [("B", true, false), ("I", false, true), ("“”", false, false)]
        {
            let (role, centered) = scene
                .draws
                .iter()
                .find_map(|d| match d {
                    Draw::StyledText(_, _, role, centered, label) if label == text => {
                        Some((*role, *centered))
                    }
                    _ => None,
                })
                .unwrap();
            let style = &scene
                .typography
                .iter()
                .find(|(id, _)| *id == role)
                .unwrap()
                .1;
            assert!(centered);
            assert!((style.size - 13.).abs() < f32::EPSILON);
            assert!((style.weight - if bold { 700. } else { 400. }).abs() < f32::EPSILON);
            assert_eq!(style.italic, italic);
        }
    }
    #[test]
    fn formatting_palette_uses_the_reference_accessible_names() {
        let mut ui = Interface::compile(SOURCE).unwrap();
        let mut i = input();
        i[22] = 1.;
        let scene = ui.step(i).unwrap();
        let names: Vec<_> = scene
            .controls
            .iter()
            .filter(|control| (300..=308).contains(&control.key))
            .map(|control| control.label.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "Body",
                "Title",
                "Heading",
                "Subheading",
                "Bold",
                "Italic",
                "Block quote",
                "Bulleted list",
                "Numbered list"
            ]
        );
    }
    #[test]
    fn formatting_palette_paints_only_the_captured_active_controls() {
        let mut ui = Interface::compile(SOURCE).unwrap();
        let mut i = input();
        i[22] = 1.;
        i[24] = 2. + 16. + 32. + 128. + 256.;
        for dark in [false, true] {
            i[19] = f32::from(dark);
            let scene = ui.step(i).unwrap();
            for control in scene
                .controls
                .iter()
                .filter(|c| (300..=308).contains(&c.key))
            {
                let active = matches!(control.key, 301 | 304 | 305 | 307 | 308);
                assert_eq!(control.toggled, Some(active));
                assert_eq!(scene.draws.iter().any(|draw| matches!(draw, Draw::RoundedRect(rect, _, _) if *rect == control.rect)), active);
            }
            assert!(
                scene
                    .controls
                    .iter()
                    .filter(|c| c.key < 300 && !(210..=212).contains(&c.key))
                    .all(|c| c.toggled.is_none())
            );
        }
        let control = |state: &str| {
            [
                "control",
                "(vec4f 0 0 20 20)",
                state,
                "Body",
                "state",
                "(vec4f 0 0 0 19)",
            ]
            .map(str::to_owned)
        };
        // The positive control proves that these fixtures reach toggle-state
        // validation, not an unrelated vec4 parser or missing-editor failure.
        assert!(decode(&control("(vec4f 1 0 300 1)")).is_ok());
        for state in [
            "(vec4f 1 2 300 1)",
            "(vec4f 1 0 300 2)",
            "(vec4f 2 0 300 1)",
        ] {
            assert_eq!(
                decode(&control(state)).unwrap_err(),
                "Invalid control toggle state"
            );
        }
    }
    #[test]
    fn appearance_changes_palette_without_changing_document_geometry() {
        let mut ui = Interface::compile(SOURCE).unwrap();
        let mut i = input();
        let light = ui.step(i).unwrap();
        i[19] = 1.;
        i[20] = 2.;
        let dark = ui.step(i).unwrap();
        let editors = |scene: &Scene| {
            scene
                .draws
                .iter()
                .filter_map(|draw| {
                    if let Draw::Editor(rect, style, _) = draw {
                        Some((*rect, *style))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(editors(&light), editors(&dark));
        assert_eq!(light.paragraphs, dark.paragraphs);
        assert_ne!(light.selection_color, dark.selection_color);
        assert_ne!(light.draws[0], dark.draws[0]);
    }
    #[test]
    fn companion_bounds_follow_easl_context_and_resize() {
        let mut ui = Interface::compile(SOURCE).unwrap();
        let mut i = input();
        i[15] = 1.;
        i[25] = 1.;
        let scene = ui.step(i).unwrap();
        let web = scene
            .draws
            .iter()
            .find_map(|d| match d {
                Draw::WebView(r) => Some(r.0),
                _ => None,
            })
            .unwrap();
        let notes = scene
            .draws
            .iter()
            .find_map(|d| match d {
                Draw::Editor(r, s, _) if id(s[2]).ok() == Some(2) => Some(r.0),
                _ => None,
            })
            .unwrap();
        assert!(web[1] >= notes[1] + notes[3]);
        assert!(web[0] + web[2] <= i[0] && web[1] + web[3] <= i[1]);
        i[0] = 720.;
        let narrow = ui.step(i).unwrap();
        assert!(
            narrow
                .draws
                .iter()
                .filter_map(|d| if let Draw::WebView(r) = d {
                    Some(r.0)
                } else {
                    None
                })
                .all(|r| r[0] >= 0. && r[0] + r[2] <= i[0])
        );
        i[15] = 0.;
        i[25] = 0.;
        assert!(
            !ui.step(i)
                .unwrap()
                .draws
                .iter()
                .any(|d| matches!(d, Draw::WebView(_)))
        );
    }
    #[test]
    fn reload_rejects_invalid_extents_and_invisible_focus() {
        let source = SOURCE.replace(
            "(vec4f column-x body-top column-width",
            "(vec4f column-x body-top -1.",
        );
        assert!(Interface::compile(&source).unwrap().step(input()).is_err());
        let mut i = input();
        i[9] = 0.;
        i[10] = 2.;
        let mut ui = Interface::compile(SOURCE).unwrap();
        assert_eq!(id(ui.step(i).unwrap().state[2]).unwrap(), 1);
        i[8] = 1.;
        let invalid_divider =
            SOURCE.replace("1. sidebar 150. outline-limit", "1. sidebar 500. 100.");
        assert!(
            Interface::compile(&invalid_divider)
                .unwrap()
                .step(i)
                .is_err()
        );
    }
    #[test]
    fn rejects_bad_reload_and_bounded_infinite_execution() {
        assert!(Interface::compile("(").is_err());
        let source = format!(
            "@external (var input: [{INPUT_COUNT}: f32] (zeroed-array)) @cpu (defn main [] (print (input 0u)) (while true ()))"
        );
        let mut ui = Interface::compile(&source).unwrap();
        let error = ui.step(input()).unwrap_err();
        assert!(error.contains("instruction budget exhausted"), "{error}");
    }
}
