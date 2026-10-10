//! Transient single-line view state. It never enters Loom's document store.
use crate::{interface::Scene, text_field::TextField};
use easl_native_text::{EditCommand, TextStyle, TextSystem};

#[derive(Debug)]
pub struct InputField {
    pub field: TextField,
    viewport: easl_text::Viewport,
    pub scroll_x: f32,
}
impl InputField {
    pub fn new(value: &str) -> Result<Self, String> {
        Ok(Self {
            field: TextField::single_line::<{ easl_native_text::MAX_TEXT_BYTES }>(value)?,
            viewport: easl_text::Viewport::new().map_err(|e| e.to_string())?,
            scroll_x: 0.,
        })
    }
    pub fn value(&self) -> &str {
        self.field.source()
    }
    pub fn composing(&self) -> bool {
        self.field.is_composing()
    }
    pub fn command(&mut self, system: &mut TextSystem, command: EditCommand) -> Result<(), String> {
        let command = match command {
            EditCommand::Insert(text) => EditCommand::Insert(text.replace(['\r', '\n'], "")),
            EditCommand::Paste(text) => EditCommand::Paste(text.replace(['\r', '\n'], "")),
            EditCommand::Commit(text) => EditCommand::Commit(text.replace(['\r', '\n'], "")),
            EditCommand::Preedit(ref text, _)
                if text.len() > easl_native_text::MAX_TEXT_BYTES
                    || text.contains(['\r', '\n', '\u{2028}', '\u{2029}']) =>
            {
                return Err("Single-line input composition exceeds its bounds".into());
            }
            other => other,
        };
        self.field.command(system, command)
    }
    pub fn set_value(&mut self, system: &mut TextSystem, value: &str) -> Result<(), String> {
        self.field
            .set_accessible_value(system, &value.replace(['\r', '\n'], ""))
    }
    pub fn accessibility(
        &mut self,
        system: &mut TextSystem,
        update: &mut accesskit::TreeUpdate,
        node: &mut accesskit::Node,
        next_id: &mut u64,
        view: &crate::interface::InputView,
        scale: f64,
    ) {
        let [x, y, width, height] = view.rect.0.map(f64::from);
        node.set_bounds(accesskit::Rect::new(x, y, x + width, y + height));
        node.set_transform(accesskit::Affine::scale(scale));
        node.set_placeholder(view.placeholder.as_str());
        self.field.editor.accessibility(
            system,
            update,
            node,
            next_id,
            [
                f64::from(view.content.0[0] - self.scroll_x),
                f64::from(view.content.0[1] - self.field.editor.scroll),
            ],
        );
    }
    pub fn layout(
        &mut self,
        system: &mut TextSystem,
        style: &TextStyle,
        width: f32,
        height: f32,
    ) -> Result<(), String> {
        let mut style = style.clone();
        style.wrap = false;
        self.field
            .ensure_layout(system, &style, &Scene::default(), width, height)
            .map_err(|e| e.to_string())?;
        let editor = &self.field.editor;
        let layout = editor
            .inner()
            .try_layout()
            .ok_or("Input layout unavailable")?;
        let caret = editor.inner().raw_selection().focus().geometry(layout, 1.3);
        let rect = [
            caret.x0,
            caret.y0,
            (caret.x1 - caret.x0).max(1.),
            (caret.y1 - caret.y0).max(1.),
        ]
        .map(crate::interface::logical_pixels);
        let extent = [
            layout.full_width().max(rect[0] + rect[2]),
            layout.height().max(height),
        ];
        self.scroll_x = self
            .viewport
            .reveal(
                [self.scroll_x, 0.],
                [width.max(1.), height.max(1.)],
                extent,
                rect,
                [0., 0.],
            )
            .map_err(|e| e.to_string())?[0];
        Ok(())
    }
}

impl crate::App {
    pub(crate) fn input_view(&self) -> Option<&crate::interface::InputView> {
        self.scene.draws.iter().find_map(|draw| match draw {
            crate::interface::Draw::Input(view) if view.key == crate::formatting::DESTINATION => {
                Some(view)
            }
            _ => None,
        })
    }
    pub(crate) fn edit_input(&mut self) -> Result<(), String> {
        // The reference input is outside a form: Return neither submits a link
        // nor replaces a selected destination with a stripped newline.
        if self.key == 2 {
            return Ok(());
        }
        let key = crate::keyboard::EditKey {
            key: self.key,
            command: self.command(),
            shift: self.modifiers.shift_key(),
            alt: self.modifiers.alt_key(),
            text: &self.text,
        };
        let Some(palette) = &mut self.format_palette else {
            return Ok(());
        };
        if let Some(command) = key.resolve(palette.destination.field.editor.selected_text())? {
            palette
                .destination
                .command(&mut self.renderer.text, command)?;
        }
        Ok(())
    }
    pub(crate) fn pointer_input(&mut self, drag: u32) -> Result<(), String> {
        let Some(view) = self.input_view().cloned() else {
            return Ok(());
        };
        let Some(palette) = &mut self.format_palette else {
            return Ok(());
        };
        let x = self.pointer[0] - view.content.0[0] + palette.destination.scroll_x;
        let y = self.pointer[1] - view.content.0[1] + palette.destination.field.editor.scroll;
        palette.destination.command(
            &mut self.renderer.text,
            if drag > 0 {
                EditCommand::Drag(x, y)
            } else {
                EditCommand::Click(x, y, self.clicks, self.modifiers.shift_key())
            },
        )?;
        self.dragging = true;
        Ok(())
    }
    pub(crate) fn input_ime(
        &mut self,
        event: winit::event::Ime,
        loop_: &winit::event_loop::ActiveEventLoop,
    ) {
        let focused = self.focus.control() == Some(crate::formatting::DESTINATION);
        let Some((composing, edit)) = input_ime_command(event, focused) else {
            return;
        };
        self.composing_field = composing.then_some(crate::formatting::DESTINATION);
        // Closing the palette leaves an IME owner tombstone until the terminal
        // event: late URL commits cannot become manuscript input.
        if let Some(palette) = &mut self.format_palette
            && let Err(error) = palette.destination.command(&mut self.renderer.text, edit)
        {
            self.error(error);
        }
        self.update(0, loop_);
    }
}

/// Preserve the old IME owner after focus leaves a transient field. A native
/// composition on a manuscript also keeps its owner when UI focus changes.
pub fn owns_ime(owner: Option<u32>, focused: Option<u32>) -> bool {
    owner == Some(crate::formatting::DESTINATION)
        || (owner.is_none() && focused == Some(crate::formatting::DESTINATION))
}
fn input_ime_command(event: winit::event::Ime, focused: bool) -> Option<(bool, EditCommand)> {
    use winit::event::Ime;
    match event {
        Ime::Enabled => None,
        Ime::Preedit(text, range) if !text.is_empty() => Some((
            true,
            if focused {
                EditCommand::Preedit(text, range)
            } else {
                EditCommand::CancelCompose
            },
        )),
        Ime::Disabled => Some((false, EditCommand::CancelCompose)),
        // Empty preedit can precede Commit. After blur it cannot release the
        // old owner and let that following commit reach a manuscript.
        Ime::Preedit(_, _) => Some((!focused, EditCommand::CancelCompose)),
        Ime::Commit(text) => Some((
            false,
            if focused {
                EditCommand::Commit(text)
            } else {
                EditCommand::CancelCompose
            },
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use easl_native_text::Movement;

    #[test]
    fn link_input_is_literal_single_line_scrolls_without_wrapping_and_has_its_own_undo() {
        let mut system = TextSystem::new();
        let mut input = InputField::new("https://example.com/**literal**/").unwrap();
        let original = input.value().to_owned();
        input
            .command(&mut system, EditCommand::Move(Movement::TextEnd, false))
            .unwrap();
        input
            .command(&mut system, EditCommand::Paste("a\r\nb".into()))
            .unwrap();
        assert_eq!(input.value(), format!("{original}ab"));
        let style = TextStyle {
            size: 11.,
            line_height: 15.,
            ..Default::default()
        };
        input.layout(&mut system, &style, 80., 18.).unwrap();
        assert_eq!(input.field.editor.inner().try_layout().unwrap().len(), 1);
        assert!(input.scroll_x > 0.);
        let end = input.field.editor.inner().cursor_geometry(1.).unwrap();
        assert!(end.x1 - f64::from(input.scroll_x) <= 80.1);
        input
            .command(&mut system, EditCommand::Move(Movement::TextStart, false))
            .unwrap();
        input.layout(&mut system, &style, 80., 18.).unwrap();
        assert!(input.scroll_x.abs() < f32::EPSILON);
        input.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(input.value(), original);
        assert!(input.set_value(&mut system, "bad\u{2028}line").is_err());
        assert_eq!(input.value(), original);
    }
    #[test]
    fn link_input_preedit_is_temporary_and_late_ime_commits_cannot_target_a_manuscript() {
        let mut system = TextSystem::new();
        let mut input = InputField::new("https://example.com/").unwrap();
        let original = input.value().to_owned();
        input
            .command(&mut system, EditCommand::Move(Movement::TextEnd, false))
            .unwrap();
        input
            .command(&mut system, EditCommand::Preedit("仮".into(), Some((0, 3))))
            .unwrap();
        assert_eq!(input.value(), original);
        assert!(input.composing());
        assert!(owns_ime(Some(crate::formatting::DESTINATION), None));
        assert!(!owns_ime(Some(1), Some(crate::formatting::DESTINATION)));
        let (keep_owner, clear) =
            input_ime_command(winit::event::Ime::Preedit(String::new(), None), false).unwrap();
        assert!(keep_owner);
        assert!(matches!(clear, EditCommand::CancelCompose));
        let (still_composing, late) =
            input_ime_command(winit::event::Ime::Commit("文".into()), false).unwrap();
        assert!(!still_composing);
        assert!(matches!(late, EditCommand::CancelCompose));
        input.command(&mut system, late).unwrap();
        assert_eq!(input.value(), original);
        assert!(!input.composing());
        input
            .command(&mut system, EditCommand::Preedit("仮".into(), None))
            .unwrap();
        let (_, commit) = input_ime_command(winit::event::Ime::Commit("文".into()), true).unwrap();
        input.command(&mut system, commit).unwrap();
        assert_eq!(input.value(), format!("{original}文"));
        input.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(input.value(), original);
    }
}
