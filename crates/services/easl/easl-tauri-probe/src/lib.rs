#![forbid(unsafe_code)]
//! Isolated two-field integration proof. No Loom documents, grants or services.
//! Native accessibility shares these buffers; Tao composition is still unbound.
pub mod accessibility;
mod layout;
#[cfg(feature = "native-probe")]
pub mod native;
mod paint;
pub mod routing;
#[cfg(any(test, feature = "native-probe"))]
mod text_events;

use easl_native_text::{EditCommand, Movement, TextEditor, TextStyle, TextSystem};
use easl_text::{EditAction, Editing};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid surface geometry")]
    Geometry,
    #[error("The embedded layout failed; recreate the surface")]
    Layout,
    #[error("No focused writing surface")]
    NotFocused,
    #[error("Unknown text field")]
    Field,
    #[error("Accessibility ownership or revision is invalid")]
    Accessibility,
    #[error(transparent)]
    Text(#[from] easl_native_text::Error),
    #[error(transparent)]
    Policy(#[from] easl_text::Error),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect(pub [f32; 4]);
impl Rect {
    fn validate_inside(self, size: [f32; 2]) -> Result<(), Error> {
        let [x, y, width, height] = self.0;
        if self.0.iter().any(|v| !v.is_finite())
            || x < 0.
            || y < 0.
            || width < 24.
            || height < 24.
            || x + width > size[0]
            || y + height > size[1]
        {
            return Err(Error::Geometry);
        }
        Ok(())
    }
    pub fn contains(self, point: [f32; 2]) -> bool {
        let [x, y, width, height] = self.0;
        point[0] >= x && point[0] < x + width && point[1] >= y && point[1] < y + height
    }
    pub(crate) fn content(self) -> Self {
        let [x, y, width, height] = self.0;
        Self([x + 8., y + 8., width - 16., height - 16.])
    }
}

/// Two plain text owners, one font/layout context and one EASL edit-policy VM.
/// Policy plans are applied synchronously to the exact field that supplied them.
pub struct TwoFields {
    fields: [TextEditor; 2],
    system: TextSystem,
    policy: Editing,
    layout: layout::Layout,
    style: TextStyle,
    boxes: Option<[Rect; 2]>,
    active: usize,
    window_focused: bool,
    capture: Option<usize>,
    identity: u64,
    revision: u64,
    presentation: u64,
}
impl std::fmt::Debug for TwoFields {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TwoFields")
            .field("active", &self.active)
            .field("window_focused", &self.window_focused)
            .field("captured", &self.capture.is_some())
            .finish_non_exhaustive()
    }
}
impl TwoFields {
    pub fn new() -> Result<Self, Error> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_VIEW: AtomicU64 = AtomicU64::new(1);
        let identity = NEXT_VIEW
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| Error::Accessibility)?;
        let style = TextStyle {
            size: 18.,
            line_height: 26.,
            color: [32, 32, 32, 255],
            ..Default::default()
        };
        Ok(Self {
            fields: [
                TextEditor::new("", style.clone())?,
                TextEditor::new("", style.clone())?,
            ],
            system: TextSystem::new(),
            policy: Editing::new()?,
            layout: layout::Layout::new()?,
            style,
            boxes: None,
            active: 0,
            window_focused: false,
            capture: None,
            identity,
            revision: 0,
            presentation: 0,
        })
    }
    /// Text, geometry and focus-owner changes revoke queued text-run authority.
    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// Caret/scroll updates repaint and republish without changing source authority.
    pub fn presentation_revision(&self) -> u64 {
        self.presentation
    }
    fn touch(&mut self) {
        self.presentation = self.presentation.saturating_add(1);
    }
    fn invalidate(&mut self) {
        // MAX is permanently unpublished, so exhaustion cannot recycle authority.
        self.revision = self.revision.saturating_add(1);
        self.touch();
    }
    pub fn text(&self, field: usize) -> Result<String, Error> {
        self.fields
            .get(field)
            .map(TextEditor::text)
            .ok_or(Error::Field)
    }
    pub fn selection(&self, field: usize) -> Result<(usize, usize), Error> {
        self.fields
            .get(field)
            .map(TextEditor::selection_bytes)
            .ok_or(Error::Field)
    }
    pub fn active(&self) -> usize {
        self.active
    }
    pub fn focused(&self) -> bool {
        self.window_focused
    }
    pub fn boxes(&self) -> Option<[Rect; 2]> {
        self.boxes
    }
    pub fn capturing(&self) -> bool {
        self.capture.is_some()
    }
    pub fn selected_text(&self) -> Option<&str> {
        if self.window_focused {
            self.fields[self.active].selected_text()
        } else {
            None
        }
    }
    pub fn resize(&mut self, size: [f32; 2]) -> Result<(), Error> {
        let boxes = self.layout.boxes(size)?;
        self.invalidate();
        for (field, rect) in self.fields.iter_mut().zip(boxes) {
            let content = rect.content();
            field.ensure_layout(&mut self.system, &self.style, content.0[2], content.0[3])?;
        }
        self.boxes = Some(boxes);
        Ok(())
    }
    pub fn set_window_focus(&mut self, focused: bool) {
        if self.window_focused != focused {
            self.invalidate();
        }
        self.window_focused = focused;
        if !focused {
            self.capture = None;
        }
    }
    pub fn cycle_focus(&mut self) {
        if self.window_focused {
            self.invalidate();
            self.active = 1 - self.active;
            self.capture = None;
        }
    }
    pub fn edit(&mut self, action: EditAction<'_>) -> Result<(), Error> {
        self.require_focus()?;
        let field = &mut self.fields[self.active];
        let text = match &action {
            EditAction::Replace(text) => *text,
            _ => "",
        };
        let plan = self.policy.plan_for(field, &mut self.system, action)?;
        if let Some(range) = plan.replacement() {
            field.replace_range(&mut self.system, range, text, plan.selection())?;
            self.invalidate();
        } else {
            field.select_range(&mut self.system, plan.selection())?;
            self.touch();
        }
        Ok(())
    }
    pub fn move_caret(&mut self, movement: Movement, extend: bool) -> Result<(), Error> {
        self.require_focus()?;
        self.fields[self.active].command(&mut self.system, EditCommand::Move(movement, extend))?;
        self.touch();
        Ok(())
    }
    pub fn undo(&mut self, redo: bool) -> Result<(), Error> {
        self.require_focus()?;
        self.invalidate();
        self.fields[self.active].command(
            &mut self.system,
            if redo {
                EditCommand::Redo
            } else {
                EditCommand::Undo
            },
        )?;
        Ok(())
    }
    pub fn pointer_down(&mut self, point: [f32; 2], extend: bool) -> Result<bool, Error> {
        self.require_focus()?;
        check_point(point)?;
        let Some(index) = self
            .boxes
            .and_then(|boxes| boxes.iter().position(|rect| rect.contains(point)))
        else {
            self.capture = None;
            return Ok(false);
        };
        let content = self.boxes.ok_or(Error::Geometry)?[index].content();
        let field = &mut self.fields[index];
        field.command(
            &mut self.system,
            EditCommand::Click(
                point[0] - content.0[0],
                point[1] - content.0[1] + field.scroll,
                1,
                extend,
            ),
        )?;
        if self.active == index {
            self.touch();
        } else {
            self.invalidate();
        }
        self.active = index;
        self.capture = Some(index);
        Ok(true)
    }
    pub fn pointer_move(&mut self, point: [f32; 2]) -> Result<bool, Error> {
        check_point(point)?;
        let Some(index) = self.capture else {
            return Ok(false);
        };
        self.require_focus()?;
        let content = self.boxes.ok_or(Error::Geometry)?[index].content();
        let field = &mut self.fields[index];
        field.command(
            &mut self.system,
            EditCommand::Drag(
                point[0] - content.0[0],
                point[1] - content.0[1] + field.scroll,
            ),
        )?;
        self.touch();
        Ok(true)
    }
    pub fn pointer_up(&mut self) {
        self.capture = None;
    }
    pub fn scroll(&mut self, point: [f32; 2], delta: f32) -> Result<bool, Error> {
        check_point(point)?;
        if !delta.is_finite() {
            return Err(Error::Geometry);
        }
        let Some(index) = self
            .boxes
            .and_then(|boxes| boxes.iter().position(|rect| rect.contains(point)))
        else {
            return Ok(false);
        };
        self.touch();
        let field = &mut self.fields[index];
        field.scroll = (field.scroll + delta).max(0.);
        field.reveal_caret = false;
        Ok(true)
    }
    fn require_focus(&self) -> Result<(), Error> {
        if self.window_focused && self.boxes.is_some() {
            Ok(())
        } else {
            Err(Error::NotFocused)
        }
    }
}
fn check_point(point: [f32; 2]) -> Result<(), Error> {
    if point.iter().all(|v| v.is_finite() && v.abs() <= 1e7) {
        Ok(())
    } else {
        Err(Error::Geometry)
    }
}
