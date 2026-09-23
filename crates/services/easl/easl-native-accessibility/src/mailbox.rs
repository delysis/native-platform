#![forbid(unsafe_code)]
use crate::Error;
use accesskit::{Action, ActionData, ActionHandler, ActionRequest, ActivationHandler, TreeUpdate};
use std::{
    cell::Cell,
    rc::Rc,
    sync::mpsc::{self, Receiver, SyncSender},
};

pub const MAX_ACTIONS: usize = 32;
pub const MAX_VALUE_BYTES: usize = 1024 * 1024;

pub struct PendingAction {
    pub revision: u64,
    pub request: ActionRequest,
}
impl std::fmt::Debug for PendingAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingAction")
            .field("revision", &self.revision)
            .field("action", &self.request.action)
            .finish_non_exhaustive()
    }
}
struct Shared {
    active: Cell<bool>,
    accepting: Cell<bool>,
    generation: Cell<Option<u64>>,
    input_revision: Cell<Option<u64>>,
    wake_pending: Cell<bool>,
    failure: Cell<Option<Error>>,
    wake: Box<dyn Fn() -> bool>,
}
impl Shared {
    fn wake(&self) {
        if !self.wake_pending.replace(true) && !(self.wake)() {
            self.failure.set(Some(Error::WakeFailed));
        }
    }
}
#[derive(Clone)]
pub struct Callbacks {
    send: SyncSender<PendingAction>,
    shared: Rc<Shared>,
}
impl std::fmt::Debug for Callbacks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Callbacks").finish_non_exhaustive()
    }
}
impl ActivationHandler for Callbacks {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        self.shared.active.set(true);
        if self.shared.accepting.get() {
            self.shared.wake();
        }
        // The main event loop publishes the full real tree. Never fabricate a
        // text tree or borrow editor state recursively from a Cocoa callback.
        None
    }
}
impl ActionHandler for Callbacks {
    fn do_action(&mut self, request: ActionRequest) {
        if !self.shared.accepting.get() || self.shared.failure.get().is_some() {
            return;
        }
        let Some(revision) = self.shared.input_revision.get() else {
            return;
        };
        match (&request.action, &request.data) {
            (Action::Focus, None)
            | (Action::SetTextSelection, Some(ActionData::SetTextSelection(_))) => {}
            (Action::SetValue, Some(ActionData::Value(value))) => {
                if value.len() > MAX_VALUE_BYTES {
                    self.shared.failure.set(Some(Error::ValueTooLarge));
                    self.shared.wake();
                    return;
                }
            }
            _ => return,
        }
        if self
            .send
            .try_send(PendingAction { revision, request })
            .is_err()
        {
            self.shared.failure.set(Some(Error::QueueFull));
        }
        self.shared.wake();
    }
}

pub struct Mailbox {
    callbacks: Callbacks,
    receive: Receiver<PendingAction>,
}
impl std::fmt::Debug for Mailbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mailbox")
            .field("active", &self.active())
            .field("failure", &self.callbacks.shared.failure.get())
            .finish_non_exhaustive()
    }
}
impl Mailbox {
    pub fn new(wake: impl Fn() -> bool + 'static) -> Self {
        let (send, receive) = mpsc::sync_channel(MAX_ACTIONS);
        Self {
            callbacks: Callbacks {
                send,
                shared: Rc::new(Shared {
                    active: Cell::new(false),
                    accepting: Cell::new(true),
                    input_revision: Cell::new(None),
                    generation: Cell::new(None),
                    wake_pending: Cell::new(false),
                    failure: Cell::new(None),
                    wake: Box::new(wake),
                }),
            },
            receive,
        }
    }
    pub fn callbacks(&self) -> Callbacks {
        self.callbacks.clone()
    }
    pub fn active(&self) -> bool {
        self.callbacks.shared.active.get()
    }
    pub fn needs_update(&self, generation: u64) -> bool {
        self.active()
            && self.callbacks.shared.accepting.get()
            && self.callbacks.shared.generation.get() != Some(generation)
    }
    pub fn published(&self, generation: u64, input_revision: u64) {
        let valid = generation != u64::MAX && input_revision != u64::MAX;
        self.callbacks
            .shared
            .generation
            .set(valid.then_some(generation));
        self.callbacks
            .shared
            .input_revision
            .set(valid.then_some(input_revision));
    }
    pub fn begin_batch(&self) -> Result<(), Error> {
        self.callbacks.shared.wake_pending.set(false);
        self.callbacks.shared.failure.get().map_or(Ok(()), Err)
    }
    pub fn pop(&self) -> Result<Option<PendingAction>, Error> {
        if let Some(error) = self.callbacks.shared.failure.get() {
            return Err(error);
        }
        Ok(self.receive.try_recv().ok())
    }
    pub fn set_accepting(&self, accepting: bool) {
        if self.callbacks.shared.accepting.replace(accepting) == accepting {
            return;
        }
        self.callbacks.shared.generation.set(None);
        self.callbacks.shared.input_revision.set(None);
        while self.receive.try_recv().is_ok() {}
    }
}

#[cfg(test)]
mod tests;

impl Callbacks {
    pub fn is_current(&self, generation: u64) -> bool {
        self.shared.accepting.get()
            && self.shared.failure.get().is_none()
            && self.shared.generation.get() == Some(generation)
    }
}

impl Drop for Mailbox {
    fn drop(&mut self) {
        self.set_accepting(false);
    }
}
