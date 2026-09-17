#![forbid(unsafe_code)]

//! Local candidate discovery. File names and header magic do not establish
//! model authenticity, capabilities, runtime readiness, or permission to load.
mod discovery;
mod projector;

pub use discovery::*;
pub use projector::*;
