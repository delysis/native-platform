use crate::model::{
    ALL_VERTICAL_IDS, ArtifactAvailabilityV0, EquivalenceProjectionV0, FactValueV0,
    FixtureArtifactV0, FixtureCaseV0, FixtureClassV0, GitSourceV0, NegativeEvidenceV0,
    ObservationEnvelopeV0, PrerequisiteKindV0, PrerequisiteV0, ReplayProgramV0, ReplayRecipeV0,
    StateIdentityV0, VERTICAL_FIXTURE_LOCK_SCHEMA_V0, VERTICAL_FIXTURE_MANIFEST_SCHEMA_V0,
    VERTICAL_OBSERVATION_SCHEMA_V0, VerticalFixtureLockV0, VerticalFixtureManifestV0, VerticalIdV0,
    W1_CONTRACT_REVISION,
};
use platform_contracts_v0::{
    ArtifactIdentityV0, ContentDigest, EvidenceTier, ExecutionKind, TerminalClass,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const MAX_ID_BYTES: usize = 256;
const MAX_TEXT_BYTES: usize = 4096;
const LIVE_HOSTED_PROVIDER_OMISSION: &str = "live hosted-provider behavior";
const QWEN_MODEL_PREREQUISITE: &str = "model.qwen.gguf";
const GEMMA_MODEL_PREREQUISITE: &str = "model.gemma.gguf";
const PARAKEET_MODEL_PREREQUISITE: &str = "model.parakeet";
const PARAKEET_AUDIO_PREREQUISITE: &str = "audio.input";
const APPLE_VOICE_PREREQUISITE: &str = "voice.apple.installed";

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ValidationError {
    #[error("{field} must not be empty")]
    Empty { field: &'static str },
    #[error("{field} contains an invalid value")]
    Invalid { field: &'static str },
    #[error("{field} contains a duplicate value")]
    Duplicate { field: &'static str },
    #[error("{field} disagrees with another field")]
    Inconsistent { field: &'static str },
    #[error("{field} digest does not match supplied bytes")]
    DigestMismatch { field: &'static str },
    #[error("{field} length does not match supplied bytes")]
    LengthMismatch { field: &'static str },
    #[error("expected projection is not valid JSON: {0}")]
    InvalidProjectionJson(String),
    #[error("locked manifest is not valid JSON: {0}")]
    InvalidManifestJson(String),
    #[error("observed projection does not equal the frozen projection")]
    ProjectionMismatch,
    #[error("manifest does not contain case {0}")]
    MissingCase(String),
    #[error("vertical lock is missing rows: {0:?}")]
    MissingVerticals(Vec<VerticalIdV0>),
}

/// Proof that caller-supplied chunks matched one exact prerequisite identity.
///
/// Only [`verify_prerequisite_chunks`] can construct this token. It retains the
/// small declared identity, never the artifact bytes. Callers may therefore
/// authenticate multi-gigabyte artifacts without making them contiguous or
/// resident in memory at once.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedPrerequisiteV0 {
    prerequisite_id: String,
    identity: ArtifactIdentityV0,
}

impl VerifiedPrerequisiteV0 {
    #[must_use]
    pub fn prerequisite_id(&self) -> &str {
        &self.prerequisite_id
    }

    #[must_use]
    pub const fn identity(&self) -> &ArtifactIdentityV0 {
        &self.identity
    }
}

/// Validates one row manifest without reading or executing anything it names.
///
/// # Errors
///
/// Returns [`ValidationError`] when an identity, case, replay recipe, class,
/// or evidence record violates the v0 protocol.
pub fn validate_manifest(manifest: &VerticalFixtureManifestV0) -> Result<(), ValidationError> {
    if manifest.schema != VERTICAL_FIXTURE_MANIFEST_SCHEMA_V0 {
        return Err(ValidationError::Invalid { field: "schema" });
    }
    if manifest.class != manifest.vertical_id.class() {
        return Err(ValidationError::Inconsistent { field: "class" });
    }
    validate_git_commit("contract_revision", &manifest.contract_revision)?;
    if manifest.contract_revision != W1_CONTRACT_REVISION {
        return Err(ValidationError::Invalid {
            field: "contract_revision",
        });
    }
    if manifest.cases.is_empty() {
        return Err(ValidationError::Empty { field: "cases" });
    }

    let mut cases = BTreeSet::new();
    for case in &manifest.cases {
        validate_case(manifest.vertical_id, case)?;
        if !cases.insert(case.case_id.as_str()) {
            return Err(ValidationError::Duplicate {
                field: "cases.case_id",
            });
        }
    }
    validate_unique_text("omitted_claims", &manifest.omitted_claims)?;

    let mut negative_ids = BTreeSet::new();
    for evidence in &manifest.negative_evidence {
        validate_negative_evidence(evidence)?;
        if !negative_ids.insert(evidence.evidence_id.as_str()) {
            return Err(ValidationError::Duplicate {
                field: "negative_evidence.evidence_id",
            });
        }
    }

    if manifest.vertical_id == VerticalIdV0::FteHostedFixtureLoopback
        && !manifest
            .omitted_claims
            .iter()
            .any(|claim| claim == LIVE_HOSTED_PROVIDER_OMISSION)
    {
        return Err(ValidationError::Invalid {
            field: "omitted_claims.live_hosted_provider",
        });
    }
    if manifest.vertical_id == VerticalIdV0::FteHostedFixtureLoopback
        && manifest
            .cases
            .iter()
            .any(|case| !case.prerequisites.is_empty())
    {
        return Err(ValidationError::Invalid {
            field: "cases.prerequisites.hosted_fixture",
        });
    }
    Ok(())
}

/// Validates a product-supplied observation as non-authorizing evidence.
///
/// # Errors
///
/// Returns [`ValidationError`] when the observation is malformed, contains
/// negative evidence, claims a disallowed execution kind, or violates the
/// canonical projection rules.
pub fn validate_observation(observation: &ObservationEnvelopeV0) -> Result<(), ValidationError> {
    if observation.schema != VERTICAL_OBSERVATION_SCHEMA_V0 {
        return Err(ValidationError::Invalid { field: "schema" });
    }
    validate_safe_id("case_id", &observation.case_id)?;
    validate_git_commit(
        "implementation_revision",
        &observation.implementation_revision,
    )?;
    validate_unique_by_id(
        "observed_prerequisites.prerequisite_id",
        &observation.observed_prerequisites,
        |prerequisite| prerequisite.prerequisite_id.as_str(),
        validate_prerequisite,
    )?;
    observation
        .evidence
        .validate()
        .map_err(|_| ValidationError::Invalid { field: "evidence" })?;
    if !observation.evidence.negative_evidence.is_empty() {
        return Err(ValidationError::Invalid {
            field: "evidence.negative_evidence",
        });
    }
    if !matches!(
        observation.evidence.tier,
        EvidenceTier::Operational | EvidenceTier::Reproducible
    ) {
        return Err(ValidationError::Invalid {
            field: "evidence.tier",
        });
    }
    match (
        observation.vertical_id.class(),
        observation.evidence.execution_kind,
    ) {
        (FixtureClassV0::ModelFree, ExecutionKind::Fixture)
        | (FixtureClassV0::Real, ExecutionKind::LocalRuntime)
        | (FixtureClassV0::State, ExecutionKind::Fixture | ExecutionKind::LocalRuntime) => {}
        _ => {
            return Err(ValidationError::Inconsistent {
                field: "evidence.execution_kind",
            });
        }
    }
    validate_projection(&observation.projection)?;
    if observation.vertical_id == VerticalIdV0::LoomSuggestionPromotion {
        validate_loom_projection(&observation.projection)?;
    }
    if observation.vertical_id == VerticalIdV0::MomChatCancelRetry {
        validate_mom_chat_retry_projection(&observation.projection)?;
    }
    Ok(())
}

/// Authenticates and validates that a lock covers all eighteen section-16 rows exactly once.
///
/// # Errors
///
/// Returns [`ValidationError`] for missing, duplicate, malformed, unauthenticated,
/// or incorrectly classified lock entries and manifests.
pub fn validate_lock<'a>(
    lock: &VerticalFixtureLockV0,
    manifest_bytes: impl IntoIterator<Item = &'a [u8]>,
) -> Result<(), ValidationError> {
    if lock.schema != VERTICAL_FIXTURE_LOCK_SCHEMA_V0 {
        return Err(ValidationError::Invalid { field: "schema" });
    }
    validate_git_commit("protocol_commit", &lock.protocol_commit)?;
    validate_git_commit("contract_revision", &lock.contract_revision)?;
    if lock.contract_revision != W1_CONTRACT_REVISION {
        return Err(ValidationError::Invalid {
            field: "contract_revision",
        });
    }

    let mut present = BTreeSet::new();
    let mut manifest_digests = BTreeSet::new();
    for entry in &lock.entries {
        if entry.class != entry.vertical_id.class() {
            return Err(ValidationError::Inconsistent {
                field: "entries.class",
            });
        }
        validate_artifact("entries.manifest", &entry.manifest)?;
        if !manifest_digests.insert(entry.manifest.digest.hex.as_str()) {
            return Err(ValidationError::Duplicate {
                field: "entries.manifest.digest",
            });
        }
        if !present.insert(entry.vertical_id) {
            return Err(ValidationError::Duplicate {
                field: "entries.vertical_id",
            });
        }
    }

    let missing = ALL_VERTICAL_IDS
        .iter()
        .copied()
        .filter(|vertical_id| !present.contains(vertical_id))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(ValidationError::MissingVerticals(missing));
    }
    if lock.entries.len() != ALL_VERTICAL_IDS.len() {
        return Err(ValidationError::Invalid { field: "entries" });
    }

    let mut authenticated = BTreeSet::new();
    for bytes in manifest_bytes {
        let manifest: VerticalFixtureManifestV0 = serde_json::from_slice(bytes)
            .map_err(|error| ValidationError::InvalidManifestJson(error.to_string()))?;
        validate_manifest(&manifest)?;
        if !authenticated.insert(manifest.vertical_id) {
            return Err(ValidationError::Duplicate {
                field: "manifests.vertical_id",
            });
        }
        let entry = lock
            .entries
            .iter()
            .find(|entry| entry.vertical_id == manifest.vertical_id)
            .ok_or(ValidationError::Invalid {
                field: "manifests.vertical_id",
            })?;
        if entry.class != manifest.class || lock.contract_revision != manifest.contract_revision {
            return Err(ValidationError::Inconsistent {
                field: "entries.manifest",
            });
        }
        verify_artifact_bytes("entries.manifest", &entry.manifest, bytes)?;
    }

    let missing_manifests = ALL_VERTICAL_IDS
        .iter()
        .copied()
        .filter(|vertical_id| !authenticated.contains(vertical_id))
        .collect::<Vec<_>>();
    if !missing_manifests.is_empty() {
        return Err(ValidationError::MissingVerticals(missing_manifests));
    }
    Ok(())
}

/// Binds an observation to its exact baseline source and expected projection.
///
/// # Errors
///
/// Returns [`ValidationError`] when any envelope is invalid, the named case is
/// absent, source or prerequisite identity differs, the expected projection
/// fails authentication, an exact-external prerequisite lacks a matching
/// stream-verified token, or the observable projection differs.
pub fn validate_baseline(
    manifest: &VerticalFixtureManifestV0,
    case_id: &str,
    expected_projection_bytes: &[u8],
    verified_prerequisites: &[VerifiedPrerequisiteV0],
    observation: &ObservationEnvelopeV0,
) -> Result<(), ValidationError> {
    validate_manifest(manifest)?;
    validate_observation(observation)?;
    let case = find_case(manifest, case_id)?;
    validate_case_binding(manifest, case, observation)?;
    authenticate_exact_prerequisites(case, verified_prerequisites)?;
    if observation.implementation_revision != case.source.commit {
        return Err(ValidationError::Inconsistent {
            field: "implementation_revision",
        });
    }
    if observation.evidence.exact_source != case.source.production_tree.digest {
        return Err(ValidationError::Inconsistent {
            field: "evidence.exact_source",
        });
    }
    compare_projection(case, expected_projection_bytes, observation)
}

/// Compares a later implementation observation with the frozen projection.
///
/// The candidate revision may differ from the baseline. The supplied candidate
/// source identifies that revision and its production-tree bytes; both are
/// authenticated and bound to the observation. Its complete observable
/// projection may not differ.
///
/// # Errors
///
/// Returns [`ValidationError`] when any envelope is invalid, the named case is
/// absent, expected or candidate-source bytes fail authentication, an
/// exact-external prerequisite lacks a matching stream-verified token, source
/// or prerequisite identity differs, or the projection differs.
pub fn compare_candidate(
    manifest: &VerticalFixtureManifestV0,
    case_id: &str,
    expected_projection_bytes: &[u8],
    candidate_source: &GitSourceV0,
    candidate_production_tree_bytes: &[u8],
    verified_prerequisites: &[VerifiedPrerequisiteV0],
    candidate: &ObservationEnvelopeV0,
) -> Result<(), ValidationError> {
    validate_manifest(manifest)?;
    validate_observation(candidate)?;
    let case = find_case(manifest, case_id)?;
    validate_case_binding(manifest, case, candidate)?;
    authenticate_exact_prerequisites(case, verified_prerequisites)?;
    validate_source(candidate_source)?;
    verify_artifact_bytes(
        "candidate_source.production_tree",
        &candidate_source.production_tree,
        candidate_production_tree_bytes,
    )?;
    if candidate.implementation_revision != candidate_source.commit {
        return Err(ValidationError::Inconsistent {
            field: "implementation_revision",
        });
    }
    if candidate_source.repository_id != case.source.repository_id {
        return Err(ValidationError::Inconsistent {
            field: "candidate_source.repository_id",
        });
    }
    if candidate.evidence.exact_source != candidate_source.production_tree.digest {
        return Err(ValidationError::Inconsistent {
            field: "evidence.exact_source",
        });
    }
    compare_projection(case, expected_projection_bytes, candidate)
}

#[must_use]
/// Computes a SHA-256 and byte-length identity for caller-supplied bytes.
///
/// # Panics
///
/// Panics only if a Rust slice length cannot fit in `u64`, which no supported
/// Rust target address space permits, or if SHA-256 formatting violates its
/// fixed lowercase 64-hex-digit contract.
pub fn sha256_identity(id: impl Into<String>, bytes: &[u8]) -> ArtifactIdentityV0 {
    ArtifactIdentityV0 {
        id: id.into(),
        digest: ContentDigest::sha256(format!("{:x}", Sha256::digest(bytes)))
            .expect("SHA-256 implementation always emits 64 lowercase hexadecimal digits"),
        length: u64::try_from(bytes.len()).expect("artifact length must fit in u64"),
    }
}

/// Authenticates one exact prerequisite from independently supplied chunks.
///
/// Chunks are hashed in iterator order and discarded immediately. Empty chunks
/// are permitted and have no effect. This function performs no filesystem or
/// other I/O; a product adapter remains responsible for supplying the stream.
///
/// # Errors
///
/// Returns [`ValidationError`] when the prerequisite identifier or declared
/// identity is malformed, the total byte length overflows `u64`, or the
/// streamed length or SHA-256 differs from the declaration.
pub fn verify_prerequisite_chunks<I, B>(
    prerequisite_id: impl Into<String>,
    identity: &ArtifactIdentityV0,
    chunks: I,
) -> Result<VerifiedPrerequisiteV0, ValidationError>
where
    I: IntoIterator<Item = B>,
    B: AsRef<[u8]>,
{
    let prerequisite_id = prerequisite_id.into();
    validate_safe_id("prerequisite_id", &prerequisite_id)?;
    validate_artifact("prerequisite.identity", identity)?;

    let mut digest = Sha256::new();
    let mut length = 0_u64;
    for chunk in chunks {
        let chunk = chunk.as_ref();
        let chunk_length = u64::try_from(chunk.len()).map_err(|_| ValidationError::Invalid {
            field: "prerequisite_chunks",
        })?;
        length = length
            .checked_add(chunk_length)
            .ok_or(ValidationError::Invalid {
                field: "prerequisite_chunks",
            })?;
        digest.update(chunk);
    }

    if identity.length != length {
        return Err(ValidationError::LengthMismatch {
            field: "prerequisite_chunks",
        });
    }
    if identity.digest.hex != format!("{:x}", digest.finalize()) {
        return Err(ValidationError::DigestMismatch {
            field: "prerequisite_chunks",
        });
    }

    Ok(VerifiedPrerequisiteV0 {
        prerequisite_id,
        identity: identity.clone(),
    })
}

fn find_case<'a>(
    manifest: &'a VerticalFixtureManifestV0,
    case_id: &str,
) -> Result<&'a FixtureCaseV0, ValidationError> {
    manifest
        .cases
        .iter()
        .find(|case| case.case_id == case_id)
        .ok_or_else(|| ValidationError::MissingCase(case_id.to_owned()))
}

fn validate_case_binding(
    manifest: &VerticalFixtureManifestV0,
    case: &FixtureCaseV0,
    observation: &ObservationEnvelopeV0,
) -> Result<(), ValidationError> {
    if observation.vertical_id != manifest.vertical_id {
        return Err(ValidationError::Inconsistent {
            field: "vertical_id",
        });
    }
    if observation.case_id != case.case_id {
        return Err(ValidationError::Inconsistent { field: "case_id" });
    }
    bind_observed_prerequisites(case, observation)?;
    if manifest.class == FixtureClassV0::State {
        let declared = case
            .state_identities
            .iter()
            .map(|state| {
                (
                    state.state_id.as_str(),
                    (state.schema_id.as_str(), &state.baseline.identity),
                )
            })
            .collect::<BTreeMap<_, _>>();
        if declared.len() != observation.projection.durable_state.len() {
            return Err(ValidationError::Inconsistent {
                field: "projection.durable_state",
            });
        }
        for state in &observation.projection.durable_state {
            let (schema_id, baseline) =
                declared
                    .get(state.state_id.as_str())
                    .ok_or(ValidationError::Inconsistent {
                        field: "projection.durable_state.state_id",
                    })?;
            if state.schema_id != *schema_id {
                return Err(ValidationError::Inconsistent {
                    field: "projection.durable_state.schema_id",
                });
            }
            if state.before.as_ref() != Some(*baseline) {
                return Err(ValidationError::Inconsistent {
                    field: "projection.durable_state.before",
                });
            }
        }
    }
    Ok(())
}

fn compare_projection(
    case: &FixtureCaseV0,
    expected_projection_bytes: &[u8],
    observation: &ObservationEnvelopeV0,
) -> Result<(), ValidationError> {
    verify_artifact_bytes(
        "expected_projection",
        &case.expected_projection,
        expected_projection_bytes,
    )?;
    let expected: EquivalenceProjectionV0 = serde_json::from_slice(expected_projection_bytes)
        .map_err(|error| ValidationError::InvalidProjectionJson(error.to_string()))?;
    validate_projection(&expected)?;
    if expected != observation.projection {
        return Err(ValidationError::ProjectionMismatch);
    }
    Ok(())
}

fn validate_case(vertical_id: VerticalIdV0, case: &FixtureCaseV0) -> Result<(), ValidationError> {
    validate_safe_id("case_id", &case.case_id)?;
    validate_source(&case.source)?;
    if case.inputs.is_empty() {
        return Err(ValidationError::Empty { field: "inputs" });
    }
    validate_unique_by_id(
        "inputs.identity.id",
        &case.inputs,
        |input| input.identity.id.as_str(),
        validate_fixture_artifact,
    )?;
    validate_row_prerequisites(vertical_id, case)?;
    validate_unique_by_id(
        "state_identities.state_id",
        &case.state_identities,
        |state| state.state_id.as_str(),
        validate_state_identity,
    )?;
    validate_unique_by_id(
        "prerequisites.prerequisite_id",
        &case.prerequisites,
        |prerequisite| prerequisite.prerequisite_id.as_str(),
        validate_prerequisite,
    )?;
    if case.replay.is_empty() {
        return Err(ValidationError::Empty { field: "replay" });
    }
    for replay in &case.replay {
        validate_replay(replay)?;
    }
    validate_artifact("expected_projection", &case.expected_projection)
}

fn validate_source(source: &GitSourceV0) -> Result<(), ValidationError> {
    validate_repository_id(&source.repository_id)?;
    validate_git_commit("source.commit", &source.commit)?;
    validate_artifact("source.production_tree", &source.production_tree)
}

fn validate_fixture_artifact(artifact: &FixtureArtifactV0) -> Result<(), ValidationError> {
    validate_artifact("artifact.identity", &artifact.identity)?;
    match (artifact.availability, artifact.relative_path.as_deref()) {
        (ArtifactAvailabilityV0::CheckedIn, Some(path)) => validate_relative_path(path),
        (ArtifactAvailabilityV0::CheckedIn, None) => Err(ValidationError::Empty {
            field: "artifact.relative_path",
        }),
        (_, None) => Ok(()),
        (_, Some(_)) => Err(ValidationError::Inconsistent {
            field: "artifact.relative_path",
        }),
    }
}

fn validate_state_identity(state: &StateIdentityV0) -> Result<(), ValidationError> {
    validate_safe_id("state_id", &state.state_id)?;
    validate_safe_id("schema_id", &state.schema_id)?;
    validate_fixture_artifact(&state.baseline)
}

fn validate_replay(replay: &ReplayRecipeV0) -> Result<(), ValidationError> {
    if let ReplayProgramV0::RepositoryScript { relative_path } = &replay.program {
        validate_relative_path(relative_path)?;
    }
    if replay.argv.is_empty() {
        return Err(ValidationError::Empty {
            field: "replay.argv",
        });
    }
    for argument in &replay.argv {
        validate_argument(argument)?;
    }
    let mut environment = BTreeSet::new();
    for name in &replay.required_environment {
        validate_environment_name(name)?;
        reject_authority_name("replay.required_environment", name)?;
        if !environment.insert(name) {
            return Err(ValidationError::Duplicate {
                field: "replay.required_environment",
            });
        }
    }
    Ok(())
}

fn validate_negative_evidence(evidence: &NegativeEvidenceV0) -> Result<(), ValidationError> {
    validate_safe_id("negative_evidence.evidence_id", &evidence.evidence_id)?;
    validate_artifact("negative_evidence.artifact", &evidence.artifact)?;
    validate_text("negative_evidence.reason", &evidence.reason)
}

fn validate_projection(projection: &EquivalenceProjectionV0) -> Result<(), ValidationError> {
    validate_events(&projection.ordered_events)?;
    validate_durable_state(&projection.durable_state)?;
    validate_lifecycle(&projection.lifecycle)?;
    validate_event_lifecycle_binding(projection)?;
    validate_ownership(&projection.ownership)?;
    validate_output_facts(&projection.output_facts)?;
    if projection.fail_closed_facts.is_empty() {
        return Err(ValidationError::Empty {
            field: "projection.fail_closed_facts",
        });
    }
    validate_unique_text(
        "projection.fail_closed_facts",
        &projection.fail_closed_facts,
    )
}

fn validate_events(events: &[crate::EventFactV0]) -> Result<(), ValidationError> {
    if events.is_empty() {
        return Err(ValidationError::Empty {
            field: "projection.ordered_events",
        });
    }
    for (expected, event) in events.iter().enumerate() {
        if event.sequence != u64::try_from(expected).expect("event index must fit in u64") {
            return Err(ValidationError::Invalid {
                field: "projection.ordered_events.sequence",
            });
        }
        validate_safe_id("event.operation_id", &event.operation_id)?;
        if let Some(attempt_id) = &event.attempt_id {
            validate_safe_id("event.attempt_id", attempt_id)?;
        }
        if let Some(correlation_id) = &event.correlation_id {
            validate_safe_id("event.correlation_id", correlation_id)?;
        }
        validate_safe_id("event.kind", &event.kind)?;
        if let Some(payload) = &event.payload {
            validate_artifact("event.payload", payload)?;
        }
    }
    Ok(())
}

fn validate_durable_state(
    durable_state: &[crate::DurableStateFactV0],
) -> Result<(), ValidationError> {
    if durable_state.is_empty() {
        return Err(ValidationError::Empty {
            field: "projection.durable_state",
        });
    }
    let mut state_ids = BTreeSet::new();
    for state in durable_state {
        validate_safe_id("durable_state.state_id", &state.state_id)?;
        validate_safe_id("durable_state.schema_id", &state.schema_id)?;
        if !state_ids.insert(state.state_id.as_str()) {
            return Err(ValidationError::Duplicate {
                field: "projection.durable_state.state_id",
            });
        }
        if let Some(before) = &state.before {
            validate_artifact("durable_state.before", before)?;
        }
        if let Some(after) = &state.after {
            validate_artifact("durable_state.after", after)?;
        }
        let valid_transition = match state.disposition {
            crate::StateDispositionV0::Unchanged => {
                state.before.is_some() && state.before == state.after
            }
            crate::StateDispositionV0::Created => state.before.is_none() && state.after.is_some(),
            crate::StateDispositionV0::Updated => {
                state.before.is_some() && state.after.is_some() && state.before != state.after
            }
            crate::StateDispositionV0::Removed => state.before.is_some() && state.after.is_none(),
            crate::StateDispositionV0::Quarantined | crate::StateDispositionV0::Recovered => {
                state.before.is_some() && state.after.is_some()
            }
        };
        if !valid_transition {
            return Err(ValidationError::Inconsistent {
                field: "durable_state.disposition",
            });
        }
    }
    Ok(())
}

fn validate_lifecycle(lifecycle: &[crate::LifecycleFactV0]) -> Result<(), ValidationError> {
    if lifecycle.is_empty() {
        return Err(ValidationError::Empty {
            field: "projection.lifecycle",
        });
    }
    let mut identities = BTreeSet::new();
    for fact in lifecycle {
        validate_safe_id("lifecycle.operation_id", &fact.operation_id)?;
        if let Some(attempt_id) = &fact.attempt_id {
            validate_safe_id("lifecycle.attempt_id", attempt_id)?;
        }
        if let Some(correlation_id) = &fact.correlation_id {
            validate_safe_id("lifecycle.correlation_id", correlation_id)?;
        }
        if !fact.released {
            return Err(ValidationError::Invalid {
                field: "lifecycle.released",
            });
        }
        if !identities.insert((fact.operation_id.as_str(), fact.attempt_id.as_deref())) {
            return Err(ValidationError::Duplicate {
                field: "projection.lifecycle.identity",
            });
        }
    }
    Ok(())
}

fn validate_event_lifecycle_binding(
    projection: &EquivalenceProjectionV0,
) -> Result<(), ValidationError> {
    let event_identities = projection
        .ordered_events
        .iter()
        .map(|event| {
            (
                event.operation_id.as_str(),
                event.attempt_id.as_deref(),
                event.correlation_id.as_deref(),
            )
        })
        .collect::<BTreeSet<_>>();
    let lifecycle_identities = projection
        .lifecycle
        .iter()
        .map(|fact| {
            (
                fact.operation_id.as_str(),
                fact.attempt_id.as_deref(),
                fact.correlation_id.as_deref(),
            )
        })
        .collect::<BTreeSet<_>>();
    if event_identities != lifecycle_identities {
        return Err(ValidationError::Inconsistent {
            field: "projection.event_lifecycle_identity",
        });
    }
    Ok(())
}

fn validate_mom_chat_retry_projection(
    projection: &EquivalenceProjectionV0,
) -> Result<(), ValidationError> {
    let proves_cancel_retry = projection.lifecycle.iter().any(|cancelled| {
        cancelled.terminal == TerminalClass::Cancelled
            && cancelled.attempt_id.is_some()
            && cancelled.correlation_id.is_some()
            && projection.lifecycle.iter().any(|completed| {
                completed.terminal == TerminalClass::Completed
                    && completed.operation_id != cancelled.operation_id
                    && completed.attempt_id.is_some()
                    && completed.attempt_id != cancelled.attempt_id
                    && completed.correlation_id == cancelled.correlation_id
                    && matches!(
                        (
                            event_position(projection, cancelled),
                            event_position(projection, completed)
                        ),
                        (Some(cancelled_position), Some(completed_position))
                            if cancelled_position < completed_position
                    )
            })
    });
    if !proves_cancel_retry {
        return Err(ValidationError::Invalid {
            field: "mom_chat_cancel_retry.lifecycle",
        });
    }
    Ok(())
}

fn event_position(
    projection: &EquivalenceProjectionV0,
    lifecycle: &crate::LifecycleFactV0,
) -> Option<usize> {
    projection.ordered_events.iter().position(|event| {
        event.operation_id == lifecycle.operation_id
            && event.attempt_id == lifecycle.attempt_id
            && event.correlation_id == lifecycle.correlation_id
    })
}

fn bind_observed_prerequisites(
    case: &FixtureCaseV0,
    observation: &ObservationEnvelopeV0,
) -> Result<(), ValidationError> {
    if case.prerequisites.len() != observation.observed_prerequisites.len() {
        return Err(ValidationError::Inconsistent {
            field: "observed_prerequisites",
        });
    }
    let observed = observation
        .observed_prerequisites
        .iter()
        .map(|prerequisite| (prerequisite.prerequisite_id.as_str(), prerequisite))
        .collect::<BTreeMap<_, _>>();
    if case
        .prerequisites
        .iter()
        .any(|expected| observed.get(expected.prerequisite_id.as_str()).copied() != Some(expected))
    {
        return Err(ValidationError::Inconsistent {
            field: "observed_prerequisites",
        });
    }
    Ok(())
}

fn authenticate_exact_prerequisites(
    case: &FixtureCaseV0,
    verified_prerequisites: &[VerifiedPrerequisiteV0],
) -> Result<(), ValidationError> {
    let mut supplied = BTreeMap::new();
    for artifact in verified_prerequisites {
        validate_safe_id(
            "verified_prerequisites.prerequisite_id",
            artifact.prerequisite_id(),
        )?;
        if supplied
            .insert(artifact.prerequisite_id(), artifact.identity())
            .is_some()
        {
            return Err(ValidationError::Duplicate {
                field: "verified_prerequisites.prerequisite_id",
            });
        }
    }
    let exact = case
        .prerequisites
        .iter()
        .filter(|prerequisite| prerequisite.kind == PrerequisiteKindV0::ExactExternalArtifact)
        .collect::<Vec<_>>();
    if exact.len() != supplied.len() {
        return Err(ValidationError::Inconsistent {
            field: "verified_prerequisites",
        });
    }
    for prerequisite in exact {
        let identity = supplied.get(prerequisite.prerequisite_id.as_str()).ok_or(
            ValidationError::Inconsistent {
                field: "verified_prerequisites",
            },
        )?;
        if *identity != &prerequisite.identity {
            return Err(ValidationError::Inconsistent {
                field: "verified_prerequisites",
            });
        }
    }
    Ok(())
}

fn validate_prerequisite(prerequisite: &PrerequisiteV0) -> Result<(), ValidationError> {
    validate_safe_id("prerequisite_id", &prerequisite.prerequisite_id)?;
    validate_artifact("prerequisite.identity", &prerequisite.identity)?;
    reject_authority_name("prerequisite_id", &prerequisite.prerequisite_id)
}

fn validate_row_prerequisites(
    vertical_id: VerticalIdV0,
    case: &FixtureCaseV0,
) -> Result<(), ValidationError> {
    match vertical_id {
        VerticalIdV0::CurrentExactQwen => require_prerequisite(
            case,
            QWEN_MODEL_PREREQUISITE,
            PrerequisiteKindV0::ExactExternalArtifact,
        ),
        VerticalIdV0::CurrentExactGemma => require_prerequisite(
            case,
            GEMMA_MODEL_PREREQUISITE,
            PrerequisiteKindV0::ExactExternalArtifact,
        ),
        VerticalIdV0::CurrentParakeetModelAudio => {
            require_prerequisite(
                case,
                PARAKEET_MODEL_PREREQUISITE,
                PrerequisiteKindV0::ExactExternalArtifact,
            )?;
            require_prerequisite(
                case,
                PARAKEET_AUDIO_PREREQUISITE,
                PrerequisiteKindV0::ExactExternalArtifact,
            )
        }
        VerticalIdV0::AppleInstalledVoice => require_prerequisite(
            case,
            APPLE_VOICE_PREREQUISITE,
            PrerequisiteKindV0::PlatformInventory,
        ),
        vertical_id if vertical_id.class() == FixtureClassV0::State => {
            if case.state_identities.is_empty() {
                Err(ValidationError::Empty {
                    field: "state_identities",
                })
            } else {
                Ok(())
            }
        }
        _ => Ok(()),
    }
}

fn require_prerequisite(
    case: &FixtureCaseV0,
    prerequisite_id: &'static str,
    kind: PrerequisiteKindV0,
) -> Result<(), ValidationError> {
    let prerequisite = case
        .prerequisites
        .iter()
        .find(|prerequisite| prerequisite.prerequisite_id == prerequisite_id)
        .ok_or(ValidationError::Invalid {
            field: "prerequisites.required",
        })?;
    if prerequisite.kind != kind || prerequisite.identity.length == 0 {
        return Err(ValidationError::Inconsistent {
            field: "prerequisites.required",
        });
    }
    Ok(())
}

fn validate_ownership(ownership: &crate::OwnershipFactsV0) -> Result<(), ValidationError> {
    if ownership.active_operations != 0 {
        return Err(ValidationError::Invalid {
            field: "ownership.active_operations",
        });
    }
    if ownership.retained_tasks != 0 {
        return Err(ValidationError::Invalid {
            field: "ownership.retained_tasks",
        });
    }
    if ownership.joined_workers != ownership.expected_workers {
        return Err(ValidationError::Inconsistent {
            field: "ownership.joined_workers",
        });
    }
    Ok(())
}

fn validate_output_facts(
    output_facts: &std::collections::BTreeMap<String, FactValueV0>,
) -> Result<(), ValidationError> {
    if output_facts.is_empty() {
        return Err(ValidationError::Empty {
            field: "projection.output_facts",
        });
    }
    for (name, value) in output_facts {
        validate_safe_id("output_facts.name", name)?;
        if let FactValueV0::Text(text) = value {
            validate_text("output_facts.text", text)?;
        }
        if let FactValueV0::Digest(digest) = value {
            digest.validate().map_err(|_| ValidationError::Invalid {
                field: "output_facts.digest",
            })?;
        }
    }
    Ok(())
}

fn validate_loom_projection(projection: &EquivalenceProjectionV0) -> Result<(), ValidationError> {
    let required = [
        ("visible_ghost_count", FactValueV0::Integer(1)),
        ("ghost_anchor", FactValueV0::Text("caret_local".to_owned())),
        (
            "tab_exact_boundary",
            FactValueV0::Text("promote_visible_ghost".to_owned()),
        ),
        (
            "tab_without_exact_boundary",
            FactValueV0::Text("ordinary_tab_or_indent".to_owned()),
        ),
        (
            "additional_candidates",
            FactValueV0::Text("hidden_until_explicit_review".to_owned()),
        ),
        ("persistent_candidate_count", FactValueV0::Boolean(false)),
        ("skip_to_manuscript_control", FactValueV0::Boolean(false)),
        ("primary_use_this_control", FactValueV0::Boolean(false)),
        ("dismissed_manuscript_unchanged", FactValueV0::Boolean(true)),
        ("stale_manuscript_unchanged", FactValueV0::Boolean(true)),
    ];
    for (name, value) in required {
        if projection.output_facts.get(name) != Some(&value) {
            return Err(ValidationError::Invalid {
                field: "loom.output_facts",
            });
        }
    }
    Ok(())
}

fn validate_artifact(
    field: &'static str,
    artifact: &ArtifactIdentityV0,
) -> Result<(), ValidationError> {
    validate_safe_id("artifact.id", &artifact.id)?;
    artifact
        .validate()
        .map_err(|_| ValidationError::Invalid { field })
}

fn verify_artifact_bytes(
    field: &'static str,
    identity: &ArtifactIdentityV0,
    bytes: &[u8],
) -> Result<(), ValidationError> {
    validate_artifact(field, identity)?;
    if identity.length != u64::try_from(bytes.len()).expect("artifact length must fit in u64") {
        return Err(ValidationError::LengthMismatch { field });
    }
    let actual = format!("{:x}", Sha256::digest(bytes));
    if identity.digest.hex != actual {
        return Err(ValidationError::DigestMismatch { field });
    }
    Ok(())
}

fn validate_repository_id(value: &str) -> Result<(), ValidationError> {
    validate_safe_id("source.repository_id", value)?;
    let mut parts = value.split('/');
    let owner = parts.next();
    let repository = parts.next();
    if owner.is_none_or(str::is_empty)
        || repository.is_none_or(str::is_empty)
        || parts.next().is_some()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
    {
        return Err(ValidationError::Invalid {
            field: "source.repository_id",
        });
    }
    Ok(())
}

fn validate_git_commit(field: &'static str, value: &str) -> Result<(), ValidationError> {
    if value.len() != 40
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(ValidationError::Invalid { field });
    }
    Ok(())
}

fn validate_relative_path(value: &str) -> Result<(), ValidationError> {
    if value.is_empty()
        || value.len() > 1024
        || value.starts_with('/')
        || value.ends_with('/')
        || value.contains('\\')
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
        || value
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err(ValidationError::Invalid {
            field: "relative_path",
        });
    }
    Ok(())
}

fn validate_argument(value: &str) -> Result<(), ValidationError> {
    if value.is_empty()
        || value.len() > MAX_TEXT_BYTES
        || value.contains('\0')
        || value.contains('\n')
        || value.contains('\r')
    {
        return Err(ValidationError::Invalid {
            field: "replay.argv",
        });
    }
    Ok(())
}

fn validate_environment_name(value: &str) -> Result<(), ValidationError> {
    let mut bytes = value.bytes();
    let first = bytes.next().ok_or(ValidationError::Empty {
        field: "replay.required_environment",
    })?;
    if value.len() > 128
        || !(first.is_ascii_uppercase() || first == b'_')
        || !bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(ValidationError::Invalid {
            field: "replay.required_environment",
        });
    }
    Ok(())
}

fn reject_authority_name(field: &'static str, value: &str) -> Result<(), ValidationError> {
    let upper = value.to_ascii_uppercase();
    let authority_segment = upper.split('_').any(|segment| {
        matches!(
            segment,
            "KEY" | "TOKEN" | "SECRET" | "PASSWORD" | "CREDENTIAL"
        )
    });
    if authority_segment
        || ["APIKEY", "ACCESSTOKEN"]
            .iter()
            .any(|forbidden| upper.contains(forbidden))
    {
        return Err(ValidationError::Invalid { field });
    }
    Ok(())
}

fn validate_safe_id(field: &'static str, value: &str) -> Result<(), ValidationError> {
    if value.is_empty() {
        return Err(ValidationError::Empty { field });
    }
    if value.len() > MAX_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'"' | b'\\'))
    {
        return Err(ValidationError::Invalid { field });
    }
    Ok(())
}

fn validate_text(field: &'static str, value: &str) -> Result<(), ValidationError> {
    if value.is_empty() {
        return Err(ValidationError::Empty { field });
    }
    if value.len() > MAX_TEXT_BYTES || value.contains('\0') {
        return Err(ValidationError::Invalid { field });
    }
    Ok(())
}

fn validate_unique_text(field: &'static str, values: &[String]) -> Result<(), ValidationError> {
    let mut seen = BTreeSet::new();
    for value in values {
        validate_text(field, value)?;
        if !seen.insert(value) {
            return Err(ValidationError::Duplicate { field });
        }
    }
    Ok(())
}

fn validate_unique_by_id<T>(
    field: &'static str,
    values: &[T],
    id: impl Fn(&T) -> &str,
    validate: impl Fn(&T) -> Result<(), ValidationError>,
) -> Result<(), ValidationError> {
    let mut seen = BTreeSet::new();
    for value in values {
        validate(value)?;
        if !seen.insert(id(value)) {
            return Err(ValidationError::Duplicate { field });
        }
    }
    Ok(())
}
