#![forbid(unsafe_code)]

//! Sealed, headless confirmatory execution contracts.
//!
//! This crate compiles a complete immutable benchmark schedule. It deliberately
//! owns no campaign search, inference, storage, UI, or subprocess behavior.
//! Serializable receipts and results are diagnostic records, never live model,
//! evaluator, promotion, or human-review authority.
//!
//! The former caller-authored claim surface is intentionally gone:
//!
//! ```compile_fail
//! use loom_benchmark::{
//!     BenchmarkRunReceiptClaim, CandidateVerificationClaims, FrozenHarnessProfile,
//! };
//! ```

mod assignment;
mod finalist;
mod human;
mod journal;
mod profile;
mod seal;

pub use assignment::*;
pub use finalist::*;
pub use human::*;
pub use journal::*;
pub use profile::*;
pub use seal::*;

#[cfg(test)]
mod tests;
