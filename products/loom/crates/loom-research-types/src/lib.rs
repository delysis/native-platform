#![forbid(unsafe_code)]

//! Fail-closed, storage-independent contracts for Loom research artifacts.
//!
//! The records in this crate make structural authorship mistakes difficult:
//! text ranges are non-empty UTF-8 ranges, assemblies reconstruct from exact
//! model-call evidence, and declared pipeline eligibility is derived from a
//! complete operation graph. They deliberately carry claims, never live
//! admission. `loom-inference` alone consumes the native backend's opaque
//! generation seal and mints a `VerifiedInferenceEnvelope`; the store may
//! persist and adopt that envelope, but must never recreate its authority from
//! a serialized enum, hash, receipt label, or record replay.

mod assembly;
mod bounded;
mod call;
mod graph;
mod ids;
mod range;

pub use assembly::*;
pub use bounded::*;
pub use call::*;
pub use graph::*;
pub use ids::*;
pub use range::*;

/// Maximum declared bytes in one raw model completion.
pub const MAX_RAW_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;
/// Maximum token observations associated with one completion.
pub const MAX_GENERATED_TOKENS: u32 = 1_048_576;
/// Maximum writer occurrences in one flat candidate assembly.
pub const MAX_ASSEMBLY_PARTS: usize = 256;
/// Maximum UTF-8 bytes in one assembled candidate.
pub const MAX_ASSEMBLY_BYTES: usize = 64 * 1024 * 1024;
/// Maximum nodes in a candidate operation graph.
pub const MAX_OPERATION_NODES: usize = 4_096;
/// Maximum direct inputs to one operation node.
pub const MAX_OPERATION_INPUTS: usize = 512;
/// Maximum total edges in one candidate operation graph.
pub const MAX_OPERATION_EDGES: usize = 8_192;
/// Maximum exact source manuscript bytes accepted by projection helpers.
pub const MAX_SOURCE_BYTES: usize = 128 * 1024 * 1024;
/// Maximum aggregate raw-output evidence replayed for one assembly.
pub const MAX_ASSEMBLY_EVIDENCE_BYTES: usize = 256 * 1024 * 1024;
/// Maximum aggregate generated token IDs replayed for one assembly.
pub const MAX_ASSEMBLY_EVIDENCE_TOKENS: usize = 4_194_304;

#[cfg(test)]
mod tests;
