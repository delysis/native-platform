#![forbid(unsafe_code)]

//! Exact-revision W2 compatibility harness.
//!
//! Direct dependencies make Cargo compile the lightweight current public crates
//! in one resolver graph. The full native FFI and Mom runtime remain an explicit
//! opt-in probe because their compilation boundary is materially different.

/// Marker used by integration tests to prove this package linked successfully.
pub const GRAPH_KIND: &str = "exact-revision-current-compatibility";
