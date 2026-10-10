//! Window-local presentation, not document, import or inference authority.
//! This module is dependency-free so its reducer tests need no application build.

pub const ADD_CONTROL: u32 = 2;
pub const FIRST_ADD_ITEM: u32 = 400;
#[cfg(test)]
pub const ADD_LABELS: [&str; 4] = [
    "New document",
    "Add files…",
    "Open library…",
    "Connect sources…",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddChoice {
    NewDocument,
}

/// Disabled items remain named, but there is deliberately no callable fake
/// import/library/connection command. Add those variants with real service work.
pub fn add_choice(index: u32) -> Option<AddChoice> {
    (index == 0).then_some(AddChoice::NewDocument)
}

pub fn unavailable_reason(key: u32) -> Option<&'static str> {
    match key {
        2 => Some("Finish the current edit or file operation."),
        3 => Some("Recording service is not connected."),
        4 => Some("Suggestion service is not connected."),
        401 => Some("File import service is not connected."),
        402 => Some("Library service is not connected."),
        403 => Some("Source connections are not connected."),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AddMenu {
    open: bool,
}

impl AddMenu {
    pub fn is_open(self) -> bool {
        self.open
    }

    pub fn set_open(&mut self, open: bool, available: bool) {
        self.open = open && available;
    }

    pub fn close(&mut self) -> bool {
        std::mem::take(&mut self.open)
    }

    pub fn choose(&mut self, index: u32, available: bool) -> Option<AddChoice> {
        if !self.open || !available {
            return None;
        }
        let choice = add_choice(index)?;
        self.close();
        Some(choice)
    }
}

/// Menus and buttons must not create repeated documents or toggle repeatedly
/// from a held activation key. Text, deletion, undo and navigation still repeat.
pub fn one_shot(key: u32, command: bool, shift: bool, button_activation: bool) -> bool {
    key == 15
        || (command && (matches!(key, 21..=24) || (shift && matches!(key, 28 | 29))))
        || (!command && button_activation)
}

/// Latch the *physical* activation until release. Checking only current focus
/// is insufficient: the first Enter can close a menu, after which repeated
/// Enter events would otherwise insert newlines into the newly focused editor.
#[derive(Debug)]
pub struct RepeatGuard<K> {
    held: Vec<K>,
}

impl<K> Default for RepeatGuard<K> {
    fn default() -> Self {
        Self { held: Vec::new() }
    }
}

impl<K: PartialEq> RepeatGuard<K> {
    pub fn suppress(&mut self, key: K, repeat: bool, one_shot: bool) -> bool {
        if repeat {
            return one_shot || self.held.contains(&key);
        }
        // A fresh press also recovers from a release lost during window blur.
        self.held.retain(|held| held != &key);
        if one_shot {
            if self.held.len() >= 32 {
                // Do not execute an activation that cannot be latched safely.
                return true;
            }
            self.held.push(key);
        }
        false
    }

    pub fn release(&mut self, key: &K) {
        self.held.retain(|held| held != key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_requires_current_native_availability() {
        let mut menu = AddMenu::default();
        menu.set_open(true, false);
        assert!(!menu.is_open());
        menu.set_open(true, true);
        assert!(menu.is_open());
        menu.set_open(true, false);
        assert!(!menu.is_open());
    }

    #[test]
    fn a_successful_choice_is_one_shot() {
        let mut menu = AddMenu::default();
        menu.set_open(true, true);
        assert_eq!(menu.choose(0, true), Some(AddChoice::NewDocument));
        assert!(!menu.is_open());
        assert_eq!(menu.choose(0, true), None);
    }

    #[test]
    fn missing_services_and_out_of_range_ids_never_turn_into_new_document() {
        let mut menu = AddMenu::default();
        menu.set_open(true, true);
        for index in [1, 2, 3, 4, 4096, u32::MAX] {
            assert_eq!(menu.choose(index, true), None);
            assert!(menu.is_open());
        }
        assert_eq!(menu.choose(0, false), None);
        assert!(menu.is_open());
    }

    #[test]
    fn dismissal_is_idempotent_and_retains_no_selection_or_payload() {
        let mut menu = AddMenu::default();
        assert!(!menu.close());
        menu.set_open(true, true);
        assert!(menu.close());
        assert!(!menu.close());
    }

    #[test]
    fn disabled_controls_have_specific_reasons_without_renaming_the_reference_labels() {
        assert_eq!(ADD_LABELS.len(), 4);
        assert_eq!(ADD_LABELS[0], "New document");
        assert_eq!(ADD_LABELS[3], "Connect sources…");
        for key in [3, 4, 401, 402, 403] {
            assert!(unavailable_reason(key).is_some());
        }
        assert!(unavailable_reason(FIRST_ADD_ITEM).is_none());
    }

    #[test]
    fn held_activation_is_not_repeated_but_normal_typing_is() {
        assert!(one_shot(2, false, false, true));
        assert!(one_shot(1, false, false, true));
        assert!(!one_shot(2, false, false, false));
        assert!(!one_shot(1, false, false, false));
        for key in [3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 20] {
            assert!(!one_shot(key, false, false, false));
        }
    }

    #[test]
    fn global_one_shot_commands_do_not_suppress_undo_or_arrow_repeat() {
        for key in 21..=24 {
            assert!(one_shot(key, true, false, false));
        }
        assert!(one_shot(28, true, true, false));
        assert!(one_shot(29, true, true, false));
        assert!(!one_shot(20, true, false, false));
        assert!(!one_shot(5, true, false, false));
        assert!(one_shot(15, false, false, false));
    }

    #[test]
    fn activation_latch_survives_a_focus_change_and_recovers_after_release() {
        let mut guard = RepeatGuard::default();
        assert!(!guard.suppress(42, false, true));
        assert!(guard.suppress(42, true, false));
        assert!(!guard.suppress(7, true, false));
        guard.release(&42);
        assert!(!guard.suppress(42, false, false));
        assert!(!guard.suppress(42, true, false));
    }

    #[test]
    fn a_new_physical_press_recovers_from_a_lost_release() {
        let mut guard = RepeatGuard::default();
        assert!(!guard.suppress(1, false, true));
        assert!(!guard.suppress(1, false, false));
        assert!(!guard.suppress(1, true, false));
    }

    #[test]
    fn repeat_capture_has_a_bound_and_fails_before_uncaptured_activation() {
        let mut guard = RepeatGuard::default();
        for key in 0..32 {
            assert!(!guard.suppress(key, false, true));
        }
        assert!(guard.suppress(32, false, true));
        assert!(guard.suppress(0, true, false));
        guard.release(&0);
        assert!(!guard.suppress(32, false, true));
    }
}
