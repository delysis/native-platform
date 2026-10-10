//! One native keyboard interpretation shared by manuscript and transient inputs.
use easl_native_text::{EditCommand, Movement};
#[derive(Clone, Copy, Debug)]
pub struct EditKey<'a> {
    pub key: u32,
    pub command: bool,
    pub shift: bool,
    pub alt: bool,
    pub text: &'a str,
}
impl EditKey<'_> {
    #[allow(
        clippy::too_many_lines,
        reason = "One inspectable platform shortcut table"
    )]
    pub fn resolve(self, selected: Option<&str>) -> Result<Option<EditCommand>, String> {
        let Self {
            key,
            command,
            shift,
            alt,
            ..
        } = self;
        let edit = if command {
            match key {
                16 => Some(EditCommand::SelectAll),
                17 | 19 => {
                    if let Some(text) = selected {
                        arboard::Clipboard::new()
                            .map_err(|e| e.to_string())?
                            .set_text(text)
                            .map_err(|e| e.to_string())?;
                        if key == 19 {
                            Some(EditCommand::DeleteSelection)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
                18 => {
                    let text = arboard::Clipboard::new()
                        .map_err(|e| e.to_string())?
                        .get_text()
                        .map_err(|e| e.to_string())?;
                    Some(EditCommand::Paste(text))
                }
                20 => Some(if shift {
                    EditCommand::Redo
                } else {
                    EditCommand::Undo
                }),
                5..=8 => Some(EditCommand::Move(
                    match key {
                        5 => Movement::LineStart,
                        6 => Movement::LineEnd,
                        7 => Movement::TextStart,
                        _ => Movement::TextEnd,
                    },
                    shift,
                )),
                _ => None,
            }
        } else {
            let motion = match key {
                5 => Some(if alt {
                    Movement::WordLeft
                } else {
                    Movement::Left
                }),
                6 => Some(if alt {
                    Movement::WordRight
                } else {
                    Movement::Right
                }),
                7 => Some(Movement::Up),
                8 => Some(Movement::Down),
                9 => Some(Movement::LineStart),
                10 => Some(Movement::LineEnd),
                11 => Some(Movement::PageUp),
                12 => Some(Movement::PageDown),
                _ => None,
            };
            if let Some(motion) = motion {
                Some(EditCommand::Move(motion, shift))
            } else {
                match key {
                    2 => Some(EditCommand::Insert("\n".into())),
                    13 => Some(EditCommand::Insert("\t".into())),
                    3 => Some(if alt {
                        EditCommand::BackspaceWord
                    } else {
                        EditCommand::Backspace
                    }),
                    4 => Some(if alt {
                        EditCommand::DeleteWord
                    } else {
                        EditCommand::Delete
                    }),
                    14 => Some(EditCommand::CancelCompose),
                    _ => {
                        if !self.text.is_empty()
                            && !self
                                .text
                                .chars()
                                .any(|c| c.is_control() && c != '\n' && c != '\t')
                        {
                            Some(EditCommand::Insert(self.text.to_owned()))
                        } else {
                            None
                        }
                    }
                }
            }
        };
        Ok(edit)
    }
}
