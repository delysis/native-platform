#![forbid(unsafe_code)]
//! Shared exact document projections, validated history and reference syntax.
//!
//! Parsing a name never reads a file, selects a model, or grants execution.

pub mod context;
mod history;
mod projection;
pub mod references;

pub use history::{BranchIndex, HistoryError, HistoryNode};
pub use projection::*;
