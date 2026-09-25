#![forbid(unsafe_code)]
//! Shared exact document projections and reference syntax.
//!
//! Parsing a name never reads a file, selects a model, or grants execution.

mod projection;
pub mod references;

pub use projection::*;
