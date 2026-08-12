use crate::model::{
    ALL_VERTICAL_IDS, ArtifactAvailabilityV0, EquivalenceProjectionV0, FactValueV0,
    FixtureArtifactV0, FixtureCaseV0, FixtureClassV0, GitSourceV0, NegativeEvidenceV0,
    ObservationEnvelopeV0, ReplayProgramV0, ReplayRecipeV0, StateIdentityV0,
    VERTICAL_FIXTURE_LOCK_SCHEMA_V0, VERTICAL_FIXTURE_MANIFEST_SCHEMA_V0,
    VERTICAL_OBSERVATION_SCHEMA_V0, VerticalFixtureLockV0, VerticalFixtureManifestV0, VerticalIdV0,
    W1_CONTRACT_REVISION,
};
use platform_contracts_v0::{ArtifactIdentityV0, ContentDigest, EvidenceTier, ExecutionKind};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const MAX_ID_BYTES: usize = 256;
const MAX_TEXT_BYTES: usize = 4096;
const LIVE_HOSTED_PROVIDER_OMISSION: &str = "live hosted-provider behavior";

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
    #[error("observed projection does not equal the frozen projection")]
    ProjectionMismatch,
    #[error("manifest does not contain case {0}")]
    MissingCase(String),
    #[error("vertical lock is missing rows: {0:?}")]
    MissingVerticals(Vec<VerticalIdV0>),
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
        validate_case(case)?;
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
    Ok(())
}

/// Validates that a lock covers all eighteen section-16 rows exactly once.
///
/// # Errors
///
/// Returns [`ValidationError`] for missing, duplicate, malformed, or
/// incorrectly classified lock entries.
pub fn validate_lock(lock: &VerticalFixtureLockV0) -> Result<(), ValidationError> {
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
    for entry in &lock.entries {
        if entry.class != entry.vertical_id.class() {
            return Err(ValidationError::Inconsistent {
                field: "entries.class",
            });
        }
        validate_artifact("entries.manifest", &entry.manifest)?;
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
    Ok(())
}

/// Binds an observation to its exact baseline source and expected projection.
///
/// # Errors
///
/// Returns [`ValidationError`] when any envelope is invalid, the named case is
/// absent, source identity differs, expected bytes fail authentication, or the
/// observable projection differs.
pub fn validate_baseline(
    manifest: &VerticalFixtureManifestV0,
    case_id: &str,
    expected_projection_bytes: &[u8],
    observation: &ObservationEnvelopeV0,
) -> Result<(), ValidationError> {
    validate_manifest(manifest)?;
    validate_observation(observation)?;
    let case = find_case(manifest, case_id)?;
    validate_case_binding(manifest, case, observation)?;
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
/// The candidate revision may differ from the baseline. Its complete
/// observable projection may not.
///
/// # Errors
///
/// Returns [`ValidationError`] when any envelope is invalid, the named case is
/// absent, expected bytes fail authentication, or the projection differs.
pub fn compare_candidate(
    manifest: &VerticalFixtureManifestV0,
    case_id: &str,
    expected_projection_bytes: &[u8],
    candidate: &ObservationEnvelopeV0,
) -> Result<(), ValidationError> {
    validate_manifest(manifest)?;
    validate_observation(candidate)?;
    let case = find_case(manifest, case_id)?;
    validate_case_binding(manifest, case, candidate)?;
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

fn validate_case(case: &FixtureCaseV0) -> Result<(), ValidationError> {
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
        |prerequisite| {
            validate_safe_id("prerequisite_id", &prerequisite.prerequisite_id)?;
            validate_artifact("prerequisite.identity", &prerequisite.identity)?;
            reject_authority_name("prerequisite_id", &prerequisite.prerequisite_id)
        },
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
    validate_ownership(&projection.ownership)?;
    validate_output_facts(&projection.output_facts)?;
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
    for fact in lifecycle {
        validate_safe_id("lifecycle.operation_id", &fact.operation_id)?;
        if let Some(attempt_id) = &fact.attempt_id {
            validate_safe_id("lifecycle.attempt_id", attempt_id)?;
        }
        if !fact.released {
            return Err(ValidationError::Invalid {
                field: "lifecycle.released",
            });
        }
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
