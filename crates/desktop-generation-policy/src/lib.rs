#![forbid(unsafe_code)]

//! Product policy shared by chat and writing. No I/O, model loading, prompt
//! formatting, or resident-worker authority belongs here.

mod context;
mod sampling;

pub use context::*;
pub use sampling::*;
