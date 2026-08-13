#![forbid(unsafe_code)]

//! Fail-closed admission for live, in-process base-writer inference.
//!
//! This crate is the only Loom layer allowed to consume llama-native-kit's
//! opaque owner-worker generation seal. Serializable call records remain
//! claims. A [`VerifiedInferenceOutcome`] minted by a backend verifier is live
//! authority and cannot be cloned, defaulted, serialized, deserialized, or
//! reconstructed from stored receipts.

pub mod admission;
mod canonical;
pub mod contracts;
pub mod controller;
#[cfg(feature = "native-evidence")]
mod persisted;
mod profile;

pub use admission::*;
pub use contracts::*;
pub use controller::*;
#[cfg(feature = "native-evidence")]
pub use persisted::*;
pub use profile::{
    BaseWriterBinding, BindingCompileError, CriticAdapterIdentity, CriticBinding,
    CriticBindingCompileError,
};

/// In-process llama-native-kit admission.
///
/// Native request, ticket, sampler, and receipt types are intentionally
/// available only under this named module and explicit feature. The crate-root
/// contracts remain backend-neutral.
#[cfg(feature = "native-llama")]
#[path = "bridge.rs"]
pub mod native_llama;

/// Owner-worker-sealed native controlled generation and exact-token
/// embeddings, with optional final joined-worker lineage binding.
#[cfg(feature = "native-llama")]
#[path = "controlled.rs"]
pub mod native_controlled;

/// In-process, structured local-critic inference. This path is role-separated
/// from base-writer calls and yields evaluation input only.
#[cfg(feature = "native-llama")]
pub mod local_critic;
