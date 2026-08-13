#![forbid(unsafe_code)]

//! Evidence-bound fiction evaluation primitives.
//!
//! This crate validates exact quotations against candidate bytes, compiles the
//! immutable fiction rubric packs, constructs blinded comparison cells, and
//! computes deterministic pessimistic aggregates and case-clustered intervals.
//! It runs no judge and mints no inference, admission, promotion, or benchmark
//! authority.

mod aggregate;
mod blind;
mod evidence;
mod gates;
#[cfg(feature = "local-critic")]
mod local_critic;
mod ncurve;
mod pack;

pub use aggregate::*;
pub use blind::*;
pub use evidence::*;
pub use gates::*;
#[cfg(feature = "local-critic")]
pub use local_critic::*;
pub use ncurve::*;
pub use pack::*;
