//! Persistent, authenticated collaboration for Loom workspaces.
#![forbid(unsafe_code)]

pub mod compute;
mod crypto;
mod document;
mod store;
mod transport;

pub use crypto::{Identity, Signed};
pub use document::{Create, DocumentView, Edit, EditResult, MetadataEdit, TextKind};
pub use store::{Cabal, ChangeEnvelope, ChangePayload, Invitation, Member, Membership, Roster};
pub use transport::{Network, NetworkMode, PeerStatus};

pub const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;
pub const MAX_CHANGE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_DOCUMENTS: usize = 64;
pub const MAX_MEMBERS: usize = 32;
pub const MAX_CHANGES: usize = 20000;
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Cabal storage failed: {0}")]
    Storage(#[from] rusqlite::Error),
    #[error("Cabal I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Invalid cabal data: {0}")]
    Format(#[from] serde_json::Error),
    #[error("Cabal document failed: {0}")]
    Document(#[from] automerge::AutomergeError),
    #[error("{0}")]
    Invalid(&'static str),
    #[error("Cabal connection failed: {0}")]
    Network(String),
}

pub type Result<T> = std::result::Result<T, Error>;
