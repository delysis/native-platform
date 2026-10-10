#![forbid(unsafe_code)]
//! Lossless native Markdown document model for Loom views.
//!
//! Original UTF-8 bytes are authoritative. Parsing and projecting never serialize
//! or normalize them. Changes are explicit, revision-bound atomic transactions.
mod editing;
mod model;
mod projection;
mod transaction;

pub use editing::*;
pub use model::*;
pub use projection::*;
pub use transaction::*;

pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;
pub const MAX_NODES: usize = 131_072;
pub const MAX_DEPTH: usize = 128;

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("Markdown exceeds its bounded source, structure or history capacity")]
    Limit,
    #[error("Selection or edit does not follow valid UTF-8/grapheme boundaries")]
    InvalidRange,
    #[error("This visible boundary does not identify a unique source byte boundary")]
    AmbiguousBoundary,
    #[error("The document changed after this transaction was prepared")]
    StaleTransaction,
    #[error("The view selection is no longer registered with this document")]
    UnknownSelection,
    #[error("Transaction edits overlap or have an ambiguous order")]
    OverlappingEdits,
    #[error("The Markdown parser produced inconsistent source geometry")]
    InvalidParse,
    #[error("This operation requires a supported visual text block")]
    UnsupportedEdit,
    #[error("The proposed Markdown edit would change its intended visible text or marks")]
    SerializationMismatch,
}
