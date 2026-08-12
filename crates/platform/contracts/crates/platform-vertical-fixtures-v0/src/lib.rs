#![forbid(unsafe_code)]

//! Pure data and validation for Wave 1 vertical baselines.
//!
//! This crate cannot execute a replay, open an artifact, contact a provider,
//! access a credential store, or mint live authority. Product-owned tests
//! supply bytes and observations explicitly. Passing validation establishes
//! only that those supplied values match a frozen, non-authorizing fixture.

mod model;
mod validation;

pub use model::{
    ALL_VERTICAL_IDS, ArtifactAvailabilityV0, DurableStateFactV0, EquivalenceProjectionV0,
    EventFactV0, EvidenceDispositionV0, FactValueV0, FixtureArtifactV0, FixtureCaseV0,
    FixtureClassV0, GitSourceV0, LifecycleFactV0, NegativeEvidenceV0, NetworkBoundaryV0,
    ObservationEnvelopeV0, OwnershipFactsV0, PrerequisiteKindV0, PrerequisiteV0, ReplayProgramV0,
    ReplayRecipeV0, StateDispositionV0, StateIdentityV0, VERTICAL_FIXTURE_LOCK_SCHEMA_V0,
    VERTICAL_FIXTURE_MANIFEST_SCHEMA_V0, VERTICAL_OBSERVATION_SCHEMA_V0,
    VerticalFixtureLockEntryV0, VerticalFixtureLockV0, VerticalFixtureManifestV0, VerticalIdV0,
    W1_CONTRACT_REVISION,
};
pub use validation::{
    ValidationError, VerifiedPrerequisiteV0, compare_candidate, sha256_identity, validate_baseline,
    validate_lock, validate_manifest, validate_observation, verify_prerequisite_chunks,
};
