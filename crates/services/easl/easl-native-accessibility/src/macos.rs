//! Only the platform attachment crosses the raw-handle boundary. No editor lives here.
use crate::{Error, Mailbox, tree};
use accesskit::{NodeId, TreeUpdate};
use accesskit_macos::{QueuedEvents, SubclassingAdapter};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{cell::RefCell, collections::BTreeSet, marker::PhantomData, rc::Rc};

thread_local! {
    static ATTACHED: RefCell<BTreeSet<usize>> = const { RefCell::new(BTreeSet::new()) };
}

struct Registration(usize);
impl Drop for Registration {
    fn drop(&mut self) {
        ATTACHED.with(|attached| {
            attached.borrow_mut().remove(&self.0);
        });
    }
}

/// Synchronous Cocoa notifications. Raise only after releasing editor/host borrows.
#[must_use = "accessibility notifications must be raised outside the editor borrow"]
pub struct Notifications {
    events: Vec<QueuedEvents>,
    // Normal snapshots are obsolete after a later publication or suspension.
    // Retirement notifications must be delivered before native destruction.
    scope: Option<(crate::Callbacks, u64)>,
}
impl std::fmt::Debug for Notifications {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Notifications")
            .field("batches", &self.events.len())
            .finish_non_exhaustive()
    }
}
impl Notifications {
    pub fn raise(self) {
        for events in self.events {
            if self
                .scope
                .as_ref()
                .is_some_and(|(scope, generation)| !scope.is_current(*generation))
            {
                break;
            }
            events.raise();
        }
    }
}

/// Main-thread-only, retained-`NSView` attachment. It does not own the Tauri window.
/// Attach once before first show/focus. Suspend before native destruction is possible.
/// No `Send` or `Sync` implementation is provided, including for queued callbacks.
pub struct Bridge {
    // Drop the subclass before clearing its registration or callback mailbox.
    adapter: SubclassingAdapter,
    _registration: Registration,
    mailbox: Mailbox,
    root: Option<NodeId>,
    _main_thread: PhantomData<Rc<()>>,
}
impl std::fmt::Debug for Bridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bridge")
            .field("mailbox", &self.mailbox)
            .finish_non_exhaustive()
    }
}
impl Bridge {
    pub fn attach(
        window: &impl HasWindowHandle,
        wake: impl Fn() -> bool + 'static,
    ) -> Result<Self, Error> {
        // SAFETY: pthread_main_np has no pointer arguments or initialization requirements.
        if unsafe { libc::pthread_main_np() } == 0 {
            return Err(Error::WrongThread);
        }
        let handle = window.window_handle().map_err(|_| Error::WindowHandle)?;
        let RawWindowHandle::AppKit(raw) = handle.as_raw() else {
            return Err(Error::WindowHandle);
        };
        let identity = raw.ns_view.as_ptr() as usize;
        let inserted = ATTACHED.with(|attached| attached.borrow_mut().insert(identity));
        if !inserted {
            return Err(Error::AlreadyAttached);
        }
        let registration = Registration(identity);
        let mailbox = Mailbox::new(wake);
        // SAFETY: the borrowed WindowHandle supplies a valid live NSView during this call.
        // Main-thread execution is checked above. AccessKit 0.26.3 retains that NSView
        // synchronously, restores its original class on drop, and invokes these handlers
        // only on the main thread. Registration prevents duplicate attachments by us.
        // Bridge is !Send/!Sync and never exposes the raw pointer to application code.
        let adapter = unsafe {
            SubclassingAdapter::new(
                raw.ns_view.as_ptr(),
                mailbox.callbacks(),
                mailbox.callbacks(),
            )
        };
        Ok(Self {
            adapter,
            _registration: registration,
            mailbox,
            root: None,
            _main_thread: PhantomData,
        })
    }
    pub fn mailbox(&self) -> &Mailbox {
        &self.mailbox
    }
    /// A full tree is required because activation returns None until the next batch.
    /// Returns notifications without raising them under the caller's editor borrow.
    pub fn publish(
        &mut self,
        generation: u64,
        input_revision: u64,
        tree: TreeUpdate,
        focused: bool,
    ) -> Result<Notifications, Error> {
        let root = tree::window_root(&tree)?;
        let mut events = Vec::with_capacity(2);
        if let Some(update) = self.adapter.update_if_active(|| tree) {
            events.push(update);
        }
        if let Some(update) = self.adapter.update_view_focus_state(focused) {
            events.push(update);
        }
        self.root = Some(root);
        self.mailbox.published(generation, input_revision);
        Ok(Notifications {
            events,
            scope: Some((self.mailbox.callbacks(), generation)),
        })
    }
    /// Retire text runs and geometry before a close/exit intention reaches the OS.
    /// Keep the subclass attached across vetoes: its constructor is first-show-only.
    /// Raise these retirement events outside all borrows, before returning control
    /// to the native close path. Resume must publish a fresh full real tree.
    pub fn suspend(&mut self) -> Notifications {
        self.mailbox.set_accepting(false);
        let mut events = Vec::with_capacity(2);
        if let Some(root) = self.root
            && let Some(update) = self.adapter.update_if_active(|| tree::cleared_tree(root))
        {
            events.push(update);
        }
        if let Some(update) = self.adapter.update_view_focus_state(false) {
            events.push(update);
        }
        Notifications {
            events,
            scope: None,
        }
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.mailbox.set_accepting(false);
    }
}
