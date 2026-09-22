//! Tao keys are not Winit `NamedKey` values. Never fall back from a command to text.
use easl_native_text::Movement;
use easl_text::EditAction;
use tauri_runtime_wry::tao::keyboard::{Key, ModifiersState};

pub(super) enum Command<'a> {
    Edit(EditAction<'a>),
    Move(Movement, bool),
    Undo(bool),
    FocusNext,
    Copy(bool),
    Paste,
    Close,
}

pub(super) fn resolve<'a>(
    key: &Key<'_>,
    text: Option<&'a str>,
    modifiers: ModifiersState,
    repeat: bool,
    macos: bool,
) -> Option<Command<'a>> {
    let shift = modifiers.shift_key();
    let command = if macos {
        modifiers.super_key()
    } else {
        modifiers.control_key() && !modifiers.alt_key()
    };
    let word = if macos { modifiers.alt_key() } else { command };
    if command && let Key::Character(letter) = key {
        if repeat {
            return None;
        }
        return match letter.to_ascii_lowercase().as_str() {
            "a" => Some(Command::Edit(EditAction::SelectAll)),
            "c" => Some(Command::Copy(false)),
            "x" => Some(Command::Copy(true)),
            "v" => Some(Command::Paste),
            "z" => Some(Command::Undo(shift)),
            "q" => Some(Command::Close),
            _ => None,
        };
    }
    let movement = match key {
        Key::ArrowLeft => Some(if macos && command {
            Movement::LineStart
        } else if word {
            Movement::WordLeft
        } else {
            Movement::Left
        }),
        Key::ArrowRight => Some(if macos && command {
            Movement::LineEnd
        } else if word {
            Movement::WordRight
        } else {
            Movement::Right
        }),
        Key::ArrowUp => Some(if command {
            Movement::TextStart
        } else {
            Movement::Up
        }),
        Key::ArrowDown => Some(if command {
            Movement::TextEnd
        } else {
            Movement::Down
        }),
        Key::Home => Some(if command {
            Movement::TextStart
        } else {
            Movement::LineStart
        }),
        Key::End => Some(if command {
            Movement::TextEnd
        } else {
            Movement::LineEnd
        }),
        Key::PageUp => Some(Movement::PageUp),
        Key::PageDown => Some(Movement::PageDown),
        _ => None,
    };
    if let Some(movement) = movement {
        return Some(Command::Move(movement, shift));
    }
    match key {
        Key::Backspace if !command || !macos => Some(Command::Edit(if word {
            EditAction::BackspaceWord
        } else {
            EditAction::Backspace
        })),
        Key::Delete if !command || !macos => Some(Command::Edit(if word {
            EditAction::DeleteWord
        } else {
            EditAction::Delete
        })),
        Key::Tab if !command && !modifiers.alt_key() && !repeat => Some(Command::FocusNext),
        Key::Enter if !command => Some(Command::Edit(EditAction::Replace("\n"))),
        _ if command || modifiers.super_key() || (macos && modifiers.control_key()) => None,
        Key::Character(_) | Key::Space => text
            .filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
            .map(|text| Command::Edit(EditAction::Replace(text))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn committed_text_uses_event_text_not_the_shortcut_label() {
        let result = resolve(
            &Key::Character("e"),
            Some("é"),
            ModifiersState::empty(),
            false,
            true,
        );
        assert!(matches!(
            result,
            Some(Command::Edit(EditAction::Replace("é")))
        ));
    }
    #[test]
    fn altgr_is_not_a_control_shortcut() {
        let result = resolve(
            &Key::Character("q"),
            Some("@"),
            ModifiersState::CONTROL | ModifiersState::ALT,
            false,
            false,
        );
        assert!(matches!(
            result,
            Some(Command::Edit(EditAction::Replace("@")))
        ));
    }
    #[test]
    fn command_and_repeat_cannot_become_text_or_repeated_focus_changes() {
        for letter in ["v", "q", "unknown"] {
            assert!(
                resolve(
                    &Key::Character(letter),
                    Some(letter),
                    ModifiersState::SUPER,
                    true,
                    true
                )
                .is_none()
            );
        }
        assert!(resolve(&Key::Tab, Some("\t"), ModifiersState::empty(), true, true).is_none());
        assert!(
            resolve(
                &Key::Character("p"),
                Some("p"),
                ModifiersState::SUPER,
                false,
                true
            )
            .is_none()
        );
    }
    #[test]
    fn mac_control_chords_are_not_unmodified_text() {
        assert!(
            resolve(
                &Key::Character("a"),
                Some("a"),
                ModifiersState::CONTROL,
                false,
                true
            )
            .is_none()
        );
    }
    #[test]
    fn ordinary_and_reverse_tab_use_the_same_two_field_focus_owner() {
        for modifiers in [ModifiersState::empty(), ModifiersState::SHIFT] {
            assert!(matches!(
                resolve(&Key::Tab, Some("\t"), modifiers, false, true),
                Some(Command::FocusNext)
            ));
        }
    }
}
