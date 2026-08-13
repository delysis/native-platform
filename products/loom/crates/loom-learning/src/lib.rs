#![forbid(unsafe_code)]

//! Deterministic learned evaluators over frozen embedding evidence.
//!
//! This crate owns no model runtime and cannot mint writer, admission,
//! promotion, or benchmark authority. Training inputs carry exact embedding
//! and grouping fingerprints. Model artifacts are exploratory evidence until
//! their separately recorded label and confirmation requirements are met.
//! The crate scores complete frozen segments only; it deliberately exposes no
//! token-level guidance, sampler, model-runtime, or manuscript mutation API.

mod artifact;
mod dataset;
mod optimizer;
mod rankgen;
mod reward;

pub use artifact::*;
pub use dataset::*;
pub(crate) use optimizer::{Adam, OptimizerError};
pub use rankgen::*;
pub use reward::*;
