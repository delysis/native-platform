//! View-owned lifetime for the shared Markdown model's formatting selection.
use crate::text_field::{FormattingCapture, TextField};
use easl_native_text::TextSystem;
use loom_markdown::{FormattingState, ParagraphFormat};

pub const DESTINATION: u32 = 309;

#[derive(Debug)]
pub struct Palette {
    pub owner: u32,
    pub destination: crate::input_field::InputField,
    capture: FormattingCapture,
    state: FormattingState,
}
impl Palette {
    pub fn open(owner: u32, field: &mut TextField) -> Result<Self, String> {
        let capture = field.capture_formatting()?;
        let state = field.formatting_state(&capture)?;
        let destination = crate::input_field::InputField::new(state.link.as_deref().unwrap_or(""))?;
        Ok(Self {
            owner,
            destination,
            capture,
            state,
        })
    }
    pub fn refresh(&mut self, field: &mut TextField, editor_focused: bool) -> Result<(), String> {
        // Validate before recapturing: focus/selection movement cannot revive a
        // token invalidated by a source edit or document replacement.
        if field.refresh_formatting(&mut self.capture, editor_focused)? {
            self.state = field.formatting_state(&self.capture)?;
        }
        Ok(())
    }
    pub fn apply(
        &mut self,
        field: &mut TextField,
        system: &mut TextSystem,
        action: u32,
    ) -> Result<(), String> {
        if self.destination.composing() {
            return Err("Finish link destination composition before formatting".into());
        }
        match action {
            9 => {
                if !self.can_link() {
                    return Err("Select text and enter a link destination".into());
                }
                let href = self.destination.value().trim().to_owned();
                field.apply_formatting(
                    system,
                    &mut self.capture,
                    &loom_markdown::FormattingCommand::Inline(loom_markdown::InlineFormat::Link(
                        href.clone(),
                    )),
                )?;
                self.destination.set_value(system, &href)?;
            }
            10 => {
                if !self.can_unlink() {
                    return Err("Select linked text to remove its link".into());
                }
                field.apply_formatting(
                    system,
                    &mut self.capture,
                    &loom_markdown::FormattingCommand::Inline(loom_markdown::InlineFormat::Unlink),
                )?;
            }
            _ => field.format_captured(system, &mut self.capture, action)?,
        }
        self.state = field.formatting_state(&self.capture)?;
        Ok(())
    }
    pub fn renew_after_input(&mut self, field: &mut TextField) -> Result<(), String> {
        self.capture = field.capture_formatting()?;
        self.state = field.formatting_state(&self.capture)?;
        Ok(())
    }
    pub fn can_link(&self) -> bool {
        !self.state.selection_empty
            && !self.destination.value().trim().is_empty()
            && !self.destination.composing()
    }
    pub fn can_unlink(&self) -> bool {
        !self.state.selection_empty && self.state.link.is_some() && !self.destination.composing()
    }
    /// EASL button indices are view protocol, not document semantics.
    pub fn active_mask(&self) -> u16 {
        let paragraph = match self.state.paragraph {
            ParagraphFormat::Heading(level @ 1..=3) => level,
            _ => 0,
        };
        (1 << paragraph)
            | (u16::from(self.state.bold) << 4)
            | (u16::from(self.state.italic) << 5)
            | (u16::from(self.state.quote) << 6)
            | (u16::from(self.state.bullet_list) << 7)
            | (u16::from(self.state.ordered_list) << 8)
    }
}

impl crate::App {
    pub(crate) fn close_formatting(&mut self) {
        self.format_palette = None;
        self.focus.close_format_menu();
    }
    pub(crate) fn reconcile_formatting(&mut self) -> bool {
        let Some(palette) = &mut self.format_palette else {
            return false;
        };
        let valid_owner = crate::interface::id(self.state[2]).ok() == Some(palette.owner)
            && !self.docs.transitioning();
        let valid_selection = valid_owner
            && self
                .docs
                .field(palette.owner, &mut self.renderer.text)
                .and_then(|field| palette.refresh(field, self.focus.chrome.is_none()))
                .is_ok();
        if valid_selection {
            false
        } else {
            self.close_formatting();
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use easl_native_text::{EditCommand, Movement};

    #[test]
    fn link_destination_edits_stay_separate_and_link_unlink_preserve_captured_undo() {
        let mut system = TextSystem::new();
        let source = "before *café* after";
        let mut field = TextField::new(source, true).unwrap();
        field.command(&mut system, EditCommand::SelectAll).unwrap();
        let mut palette = Palette::open(1, &mut field).unwrap();
        assert!(!palette.can_link() && !palette.can_unlink());
        palette
            .destination
            .set_value(&mut system, "  https://example.com/café  ")
            .unwrap();
        assert_eq!(field.text(), source);
        assert!(palette.can_link());
        palette.apply(&mut field, &mut system, 9).unwrap();
        assert_eq!(palette.destination.value(), "https://example.com/café");
        assert!(palette.can_unlink());
        let linked = field.text();
        assert!(linked.contains("https://example.com/café"));
        // A form-field undo cannot undo the document's link transaction.
        palette
            .destination
            .command(&mut system, EditCommand::Undo)
            .unwrap();
        assert_eq!(field.text(), linked);
        palette.apply(&mut field, &mut system, 10).unwrap();
        assert!(!palette.can_unlink());
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(field.text(), linked);
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(field.text(), source);
    }
    #[test]
    fn link_destination_retains_its_draft_during_owner_navigation_and_rejects_bad_or_foreign_targets()
     {
        let mut system = TextSystem::new();
        let mut field = TextField::new("one two", true).unwrap();
        field.command(&mut system, EditCommand::SelectAll).unwrap();
        let mut palette = Palette::open(1, &mut field).unwrap();
        palette
            .destination
            .set_value(&mut system, "https://bad url")
            .unwrap();
        assert!(palette.apply(&mut field, &mut system, 9).is_err());
        assert_eq!(field.text(), "one two");
        palette
            .destination
            .set_value(&mut system, "https://example.com")
            .unwrap();
        let mut replacement = TextField::new("one two", true).unwrap();
        assert!(palette.apply(&mut replacement, &mut system, 9).is_err());
        assert_eq!(replacement.text(), "one two");
        field
            .command(&mut system, EditCommand::Move(Movement::TextEnd, false))
            .unwrap();
        palette.renew_after_input(&mut field).unwrap();
        assert!(!palette.can_link());
        assert_eq!(palette.destination.value(), "https://example.com");
        field.command(&mut system, EditCommand::SelectAll).unwrap();
        palette.refresh(&mut field, true).unwrap();
        palette
            .destination
            .command(&mut system, EditCommand::Preedit("仮".into(), None))
            .unwrap();
        assert!(!palette.can_link());
        assert!(palette.apply(&mut field, &mut system, 9).is_err());
        assert_eq!(field.text(), "one two");
    }
    #[test]
    fn palette_retains_its_document_view_and_selection_through_rendering_and_commands() {
        let mut system = TextSystem::new();
        let mut field = TextField::new("café tail", true).unwrap();
        field.command(&mut system, EditCommand::SelectAll).unwrap();
        let mut palette = Palette::open(1, &mut field).unwrap();
        field.activate_view(2, &mut system).unwrap();
        assert!(palette.apply(&mut field, &mut system, 4).is_err());
        assert_eq!(field.text(), "café tail");
        field.activate_view(1, &mut system).unwrap();
        // Renderer visits do not transfer the palette's selection to a pane.
        palette.refresh(&mut field, false).unwrap();
        palette.apply(&mut field, &mut system, 4).unwrap();
        assert_eq!(field.text(), "**café tail**");
        assert_eq!(palette.active_mask(), 1 | 16);
        palette.apply(&mut field, &mut system, 5).unwrap();
        assert_eq!(palette.active_mask(), 1 | 16 | 32);
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert!(palette.refresh(&mut field, true).is_err());
        assert!(palette.apply(&mut field, &mut system, 1).is_err());
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(field.text(), "café tail");
    }

    #[test]
    fn palette_does_not_follow_replacement_documents_source_mode_or_preedit() {
        let mut system = TextSystem::new();
        let mut field = TextField::new("words", true).unwrap();
        let mut palette = Palette::open(1, &mut field).unwrap();
        let mut replacement = TextField::new("words", true).unwrap();
        assert!(palette.apply(&mut replacement, &mut system, 1).is_err());
        assert_eq!(replacement.text(), "words");
        field.toggle_source(&mut system).unwrap();
        assert!(palette.refresh(&mut field, false).is_err());
        assert!(Palette::open(1, &mut field).is_err());
        field.toggle_source(&mut system).unwrap();
        field
            .command(&mut system, EditCommand::Preedit("仮".into(), None))
            .unwrap();
        let raw = field.editor.inner().raw_text().to_owned();
        assert!(field.format(&mut system, 4).is_err());
        assert!(Palette::open(1, &mut field).is_err());
        assert!(field.is_composing());
        assert_eq!(field.editor.inner().raw_text(), raw);
        assert_eq!(field.text(), "words");
    }

    #[test]
    fn palette_capture_preserves_typing_marks_when_the_renderer_visits_another_pane() {
        let mut system = TextSystem::new();
        let mut field = TextField::new("words", true).unwrap();
        field
            .command(&mut system, EditCommand::Move(Movement::TextEnd, false))
            .unwrap();
        field.format(&mut system, 4).unwrap();
        field.activate_view(2, &mut system).unwrap();
        field.activate_view(1, &mut system).unwrap();
        let palette = Palette::open(1, &mut field).unwrap();
        assert_ne!(palette.active_mask() & 16, 0);
        field
            .command(&mut system, EditCommand::Insert("Y".into()))
            .unwrap();
        assert_eq!(field.text(), "words**Y**");
    }
    #[test]
    fn focused_editor_navigation_refreshes_the_capture_without_source_or_history_changes() {
        let mut system = TextSystem::new();
        let mut field = TextField::new("plain **bold**", true).unwrap();
        let mut palette = Palette::open(1, &mut field).unwrap();
        field
            .command(&mut system, EditCommand::Move(Movement::TextEnd, false))
            .unwrap();
        palette.refresh(&mut field, true).unwrap();
        assert_ne!(palette.active_mask() & 16, 0);
        palette.apply(&mut field, &mut system, 4).unwrap();
        assert_eq!(palette.active_mask() & 16, 0);
        assert_eq!(field.text(), "plain **bold**");
        field
            .command(&mut system, EditCommand::Insert("!".into()))
            .unwrap();
        assert!(palette.refresh(&mut field, true).is_err());
        assert_eq!(field.text(), "plain **bold**!");
        field.command(&mut system, EditCommand::Undo).unwrap();
        assert_eq!(field.text(), "plain **bold**");
    }
}
