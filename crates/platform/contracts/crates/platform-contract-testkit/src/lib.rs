//! Executable model tests for the Wave 1 platform contracts.
//!
//! Product adapters implement the traits in this crate. The traits are test
//! surfaces, not production runtime abstractions.

#![forbid(unsafe_code)]

pub mod barrier;
pub mod envelope_suite;
pub mod fault;
pub mod lifecycle_suite;
pub mod model;
pub mod publication_suite;

pub use platform_contracts_v0 as contracts;

pub use envelope_suite::{validate_capability_snapshot_v0, validate_service_error_v0};
pub use model::{
    AdapterError, AttemptIdentity, ClosedFacts, LifecyclePhase, OperationModelAdapter,
    OperationPhase, OperationSnapshot, ReferenceAdapter, ReferenceLease, ReferenceTicket,
    Reservation, TerminalClass, TerminalRecord, TestConfig, WaitObservation,
};
pub use publication_suite::{
    PublicationErrorContext, PublicationModelAdapter, PublicationRequest, PublicationStep,
    ReferencePublicationAdapter, ReferencePublicationError, ReferenceStage, StagingFacts,
    assert_publication_model, run_publication_model,
};
