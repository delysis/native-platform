use anyhow::{Context, Result, ensure};
use platform_contracts_v0::{
    EvidenceClaimV0, EvidenceTier, ExecutionKind, evidence::EVIDENCE_SCHEMA_V0,
};
use platform_vertical_fixtures_v0::{
    ArtifactAvailabilityV0, EquivalenceProjectionV0, ObservationEnvelopeV0,
    VERTICAL_OBSERVATION_SCHEMA_V0, VerticalFixtureManifestV0, VerticalIdV0, sha256_identity,
    validate_baseline,
};

const INITIAL_BASELINE_COMMIT: &str = "097da612140c6479f9d40e7816f0500271464ca9";
const INITIAL_BASELINE_TREE_BYTES: &[u8] = b"eeb82598e65f1dea397d20a5159856167f9a48a6";
const QUIT_RELAUNCH_BASELINE_COMMIT: &str = "b5a276c6152e9bf1d6d1f2b5cf9c199871c45778";
const QUIT_RELAUNCH_BASELINE_TREE_BYTES: &[u8] = b"71db4c79a072cfce2f72f34494531264933d9892";

/// Authenticates one checked-in Mom fixture bundle and compares product facts
/// against its frozen W1 projection.
///
/// This feature-gated helper reads no path, invokes no backend, and grants no
/// authority. Callers must obtain `actual` from the product replay named by the
/// manifest.
pub fn validate_w1_fixture_projection(
    vertical_id: VerticalIdV0,
    actual: EquivalenceProjectionV0,
) -> Result<()> {
    let (manifest_bytes, projection_bytes) = bundle(vertical_id)?;
    let manifest: VerticalFixtureManifestV0 =
        serde_json::from_slice(manifest_bytes).context("parse checked-in W1 fixture manifest")?;
    ensure!(
        manifest.vertical_id == vertical_id,
        "W1 manifest row mismatch"
    );
    ensure!(
        manifest.cases.len() == 1,
        "Mom W1 bundle must contain one case"
    );
    let case = manifest
        .cases
        .first()
        .context("W1 manifest must contain its product case")?;
    let production_tree_bytes = match case.source.commit.as_str() {
        INITIAL_BASELINE_COMMIT => INITIAL_BASELINE_TREE_BYTES,
        QUIT_RELAUNCH_BASELINE_COMMIT => QUIT_RELAUNCH_BASELINE_TREE_BYTES,
        commit => anyhow::bail!("unrecognized Mom W1 production baseline commit: {commit}"),
    };
    ensure!(
        sha256_identity(
            case.source.production_tree.id.clone(),
            production_tree_bytes
        ) == case.source.production_tree,
        "baseline production-tree identity is not authenticated"
    );
    authenticate_inputs(case)?;

    let observation = ObservationEnvelopeV0 {
        schema: VERTICAL_OBSERVATION_SCHEMA_V0.to_owned(),
        vertical_id,
        case_id: case.case_id.clone(),
        implementation_revision: case.source.commit.clone(),
        observed_prerequisites: Vec::new(),
        evidence: EvidenceClaimV0 {
            schema: EVIDENCE_SCHEMA_V0.to_owned(),
            tier: EvidenceTier::Reproducible,
            threat_model: "feature-gated local Mom fixture replay; no network, real model, personal credential, or acceptance authority".to_owned(),
            exact_source: case.source.production_tree.digest.clone(),
            exact_runtime_or_artifact: case.expected_projection.digest.clone(),
            execution_kind: ExecutionKind::Fixture,
            omitted_claims: manifest.omitted_claims.clone(),
            negative_evidence: Vec::new(),
        },
        projection: actual,
    };
    validate_baseline(
        &manifest,
        &case.case_id,
        projection_bytes,
        &[],
        &observation,
    )
    .context("validate authenticated Mom W1 fixture projection")
}

fn authenticate_inputs(case: &platform_vertical_fixtures_v0::FixtureCaseV0) -> Result<()> {
    for input in &case.inputs {
        ensure!(
            input.availability == ArtifactAvailabilityV0::CheckedIn,
            "Mom W1 input must be checked in"
        );
        let relative_path = input
            .relative_path
            .as_deref()
            .context("checked-in W1 input path")?;
        let bytes = checked_in_input(relative_path)?;
        ensure!(
            sha256_identity(input.identity.id.clone(), bytes) == input.identity,
            "checked-in W1 input identity mismatch for {relative_path}"
        );
    }
    Ok(())
}

fn checked_in_input(relative_path: &str) -> Result<&'static [u8]> {
    match relative_path {
        "crates/mom-llama-runtime/fixtures/w1/chat-cancel-retry-v1.json" => {
            Ok(include_bytes!("../fixtures/w1/chat-cancel-retry-v1.json"))
        }
        "crates/mom-llama-runtime/fixtures/w1/ordinary-notes.md" => {
            Ok(include_bytes!("../fixtures/w1/ordinary-notes.md"))
        }
        "crates/mom-llama-runtime/fixtures/w1/ordinary-notes-projection-v1.json" => Ok(
            include_bytes!("../fixtures/w1/ordinary-notes-projection-v1.json"),
        ),
        "crates/mom-llama-runtime/fixtures/w1/prior-store-v1.json" => {
            Ok(include_bytes!("../fixtures/w1/prior-store-v1.json"))
        }
        "crates/mom-llama-runtime/fixtures/w1/cache-corruption-v1.json" => {
            Ok(include_bytes!("../fixtures/w1/cache-corruption-v1.json"))
        }
        "crates/mom-llama-runtime/fixtures/w1/cache-native-prefix-state-v1.json" => Ok(
            include_bytes!("../fixtures/w1/cache-native-prefix-state-v1.json"),
        ),
        "crates/mom-llama-runtime/fixtures/w1/cache-session-state-v1.json" => {
            Ok(include_bytes!("../fixtures/w1/cache-session-state-v1.json"))
        }
        "crates/mom-llama-runtime/fixtures/w1/cache-native-prefix-after-state-v1.json" => Ok(
            include_bytes!("../fixtures/w1/cache-native-prefix-after-state-v1.json"),
        ),
        "crates/mom-llama-runtime/fixtures/w1/cache-session-after-state-v1.json" => Ok(
            include_bytes!("../fixtures/w1/cache-session-after-state-v1.json"),
        ),
        "crates/mom-llama-runtime/fixtures/w1/quit-relaunch-v1.json" => {
            Ok(include_bytes!("../fixtures/w1/quit-relaunch-v1.json"))
        }
        _ => anyhow::bail!("unrecognized checked-in Mom W1 input: {relative_path}"),
    }
}

fn bundle(vertical_id: VerticalIdV0) -> Result<(&'static [u8], &'static [u8])> {
    match vertical_id {
        VerticalIdV0::MomChatCancelRetry => Ok((
            include_bytes!("../fixtures/w1/chat-cancel-retry-manifest-v0.json"),
            include_bytes!("../fixtures/w1/chat-cancel-retry-projection-v0.json"),
        )),
        VerticalIdV0::MomAttachment => Ok((
            include_bytes!("../fixtures/w1/attachment-manifest-v0.json"),
            include_bytes!("../fixtures/w1/attachment-projection-v0.json"),
        )),
        VerticalIdV0::MomPriorReleaseStore => Ok((
            include_bytes!("../fixtures/w1/prior-store-manifest-v0.json"),
            include_bytes!("../fixtures/w1/prior-store-projection-v0.json"),
        )),
        VerticalIdV0::CorruptedDisposableCaches => Ok((
            include_bytes!("../fixtures/w1/cache-corruption-manifest-v0.json"),
            include_bytes!("../fixtures/w1/cache-corruption-projection-v0.json"),
        )),
        VerticalIdV0::QuitRelaunchFakeOwners => Ok((
            include_bytes!("../fixtures/w1/quit-relaunch-manifest-v0.json"),
            include_bytes!("../fixtures/w1/quit-relaunch-projection-v0.json"),
        )),
        _ => anyhow::bail!("Mom does not own a product case for {vertical_id:?}"),
    }
}
