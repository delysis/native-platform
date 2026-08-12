#![forbid(unsafe_code)]

//! Version-zero serialized platform contracts.
//!
//! This crate contains data envelopes and their validation only. It does not
//! own runtime work, mint live authority, access external resources, or define
//! a universal service runtime trait.

pub mod capability;
pub mod error;
pub mod evidence;
pub mod ids;
pub mod lifecycle;
pub mod privacy;
pub mod publication;
pub mod shutdown;

pub use capability::{
    CapabilityEntryV0, CapabilitySnapshotV0, CapabilitySourceReportV0, Readiness, TriState,
};
pub use error::{ErrorClass, RetryAdvice, ServiceErrorV0};
pub use evidence::{EvidenceClaimV0, EvidenceTier, ExecutionKind};
pub use ids::{
    AttemptId, ContentDigest, DigestAlgorithm, IdentifierError, OperationId, ProviderId, ServiceId,
};
pub use lifecycle::{OperationPhase, SupervisorPhase, TerminalClass, TerminalV0};
pub use privacy::{
    DataHandlingV0, DataTierV0, LoggingPolicyV0, NetworkPolicyV0, PayloadRedactionV0,
    PrivacyDecisionV0, PrivacyDenialV0, PrivacyPolicyV0, RedactionStateV0, RoutePrivacyContextV0,
    RouteTargetV0,
};
pub use publication::{
    ArtifactIdentityV0, DestinationIdentityV0, PublicationOutcomeV0, PublicationReceiptV0,
};
pub use shutdown::{
    ClosedSummaryV0, ShutdownFailureV0, ShutdownResourceKind, ShutdownResourceState,
    ShutdownResourceV0,
};

/// Validation error shared by the version-zero data envelopes.
#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
#[non_exhaustive]
pub enum ContractError {
    #[error("{field} must not be empty")]
    Empty { field: &'static str },
    #[error("{field} exceeds its maximum length of {max} bytes")]
    TooLong { field: &'static str, max: usize },
    #[error("{field} contains an invalid value")]
    Invalid { field: &'static str },
    #[error("{field} contains a duplicate value")]
    Duplicate { field: &'static str },
    #[error("{field} disagrees with another field")]
    Inconsistent { field: &'static str },
}

pub(crate) fn validate_nonempty_bounded(
    field: &'static str,
    value: &str,
    max: usize,
) -> Result<(), ContractError> {
    if value.is_empty() {
        return Err(ContractError::Empty { field });
    }
    if value.len() > max {
        return Err(ContractError::TooLong { field, max });
    }
    Ok(())
}
