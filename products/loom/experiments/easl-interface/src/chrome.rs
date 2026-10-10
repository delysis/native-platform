//! Native lifecycle for the Add menu. The EASL scene owns its appearance; the
//! existing document worker remains the only route to document creation.
use crate::{
    App,
    chrome_state::{ADD_CONTROL, AddChoice, FIRST_ADD_ITEM},
    focus::Target,
};
use winit::event_loop::ActiveEventLoop;

impl App {
    pub(crate) fn add_available(&self) -> bool {
        self.docs.can_switch_views() && !self.docs.busy() && self.composing_field.is_none()
    }

    pub(crate) fn dismiss_add(&mut self) {
        if self.add_menu.close() {
            self.focus.chrome = Some(Target::Control(ADD_CONTROL));
            self.dragging = false;
            self.divider.release();
        }
    }

    pub(crate) fn add_action(&mut self, action: u32, argument: u32) -> Result<(), String> {
        match action {
            25 if argument <= 1 => {
                if argument == 0 {
                    self.dismiss_add();
                } else if self.add_available() {
                    self.close_formatting();
                    self.docs.workspace.menu = None;
                    self.add_menu.set_open(true, true);
                    self.focus_target(Target::Control(FIRST_ADD_ITEM), self.focus.visible);
                }
            }
            26 => {
                let available = self.add_available();
                match self.add_menu.choose(argument, available) {
                    Some(AddChoice::NewDocument) => {
                        // A cancelled Save/Discard dialog returns to Add, not
                        // to a vanished menu item or an implicitly writable editor.
                        self.focus.chrome = Some(Target::Control(ADD_CONTROL));
                        self.transition(crate::document::Destination::NewDocument)?;
                    }
                    None => return Err("This Add command is not available".into()),
                }
            }
            _ => return Err("Invalid Add menu action".into()),
        }
        Ok(())
    }

    /// Consume all ordinary text while the popup owns focus. Only New document
    /// is currently callable; arrow/Home/End navigation cannot select a disabled
    /// import row. Tab closes first, then traverses the newly uncovered scene.
    pub(crate) fn add_key(&mut self, loop_: &ActiveEventLoop) -> bool {
        if !self.add_menu.is_open() {
            return false;
        }
        self.focus.visible = true;
        if self.key == 14 {
            self.dismiss_add();
        } else if self.key == 13 && !self.command() && !self.modifiers.alt_key() {
            self.dismiss_add();
            self.update(0, loop_);
            self.traverse_focus(self.modifiers.shift_key());
        } else if !self.command() && !self.modifiers.alt_key() {
            if self.key == 2 || self.text == " " {
                if let Err(error) = self.perform(crate::actions::Action::AddNew, loop_) {
                    self.error(error);
                }
            } else if matches!(self.key, 5..=12) {
                self.focus_target(Target::Control(FIRST_ADD_ITEM), true);
            }
        }
        self.update(0, loop_);
        true
    }
}
