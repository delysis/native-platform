//! Small OS adapter for application-owned native text. No text, history or model owner.
#![deny(unsafe_op_in_unsafe_fn)]
#[cfg(target_os = "macos")]
mod macos;
mod mailbox;
#[cfg(any(test, target_os = "macos"))]
mod tree;
#[cfg(target_os = "macos")]
pub use macos::{Bridge, Notifications};
pub use mailbox::{Callbacks, MAX_ACTIONS, MAX_VALUE_BYTES, Mailbox, PendingAction};

#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("Native accessibility must be attached on the main thread")]
    WrongThread,
    #[error("Native accessibility needs a live AppKit view")]
    WindowHandle,
    #[error("Native accessibility is already attached to this view")]
    AlreadyAttached,
    #[error("Accessibility action queue exceeded its bound")]
    QueueFull,
    #[error("Accessibility action value exceeded its bound")]
    ValueTooLarge,
    #[error("Accessibility event-loop wake failed")]
    WakeFailed,
    #[error("Accessibility requires a complete unique window-root tree")]
    Tree,
}
