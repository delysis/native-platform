use platform_contracts_v0::evidence::EVIDENCE_SCHEMA_V0;
use platform_contracts_v0::{EvidenceClaimV0, EvidenceTier, ExecutionKind};
use platform_vertical_fixtures_v0::{
    CaseBaselineV0, EquivalenceProjectionV0, ObservationEnvelopeV0, VERTICAL_OBSERVATION_SCHEMA_V0,
    VerticalFixtureManifestV0, VerticalIdV0, validate_row_baselines,
};

const MANIFEST: &[u8] =
    include_bytes!("../../../verticals/v0/quit-relaunch-fake-owners.manifest.json");
const MOM_PROJECTION: &[u8] =
    include_bytes!("../../../verticals/v0/quit-relaunch-fake-owners.mom.projection.json");
const SPEECH_PROJECTION: &[u8] =
    include_bytes!("../../../verticals/v0/quit-relaunch-fake-owners.speech.projection.json");
const LOOM_PROJECTION: &[u8] =
    include_bytes!("../../../verticals/v0/quit-relaunch-fake-owners.loom.projection.json");
const NATIVE_PROJECTION: &[u8] =
    include_bytes!("../../../verticals/v0/quit-relaunch-fake-owners.native.projection.json");
const FTE_PROJECTION: &[u8] =
    include_bytes!("../../../verticals/v0/quit-relaunch-fake-owners.fte.projection.json");

fn observation(
    manifest: &VerticalFixtureManifestV0,
    case_index: usize,
    threat_model: &str,
    projection: EquivalenceProjectionV0,
) -> ObservationEnvelopeV0 {
    let case = &manifest.cases[case_index];
    ObservationEnvelopeV0 {
        schema: VERTICAL_OBSERVATION_SCHEMA_V0.to_owned(),
        vertical_id: manifest.vertical_id,
        case_id: case.case_id.clone(),
        implementation_revision: case.source.commit.clone(),
        observed_prerequisites: Vec::new(),
        evidence: EvidenceClaimV0 {
            schema: EVIDENCE_SCHEMA_V0.to_owned(),
            tier: EvidenceTier::Reproducible,
            threat_model: threat_model.to_owned(),
            exact_source: case.source.production_tree.digest.clone(),
            exact_runtime_or_artifact: case.expected_projection.digest.clone(),
            execution_kind: ExecutionKind::Fixture,
            omitted_claims: manifest.omitted_claims.clone(),
            negative_evidence: Vec::new(),
        },
        projection,
    }
}

#[test]
fn staged_quit_relaunch_bundle_preserves_all_five_product_baselines() {
    let manifest: VerticalFixtureManifestV0 =
        serde_json::from_slice(MANIFEST).expect("staged quit/relaunch manifest JSON");
    assert_eq!(manifest.vertical_id, VerticalIdV0::QuitRelaunchFakeOwners);
    assert_eq!(manifest.cases.len(), 5);
    assert_eq!(
        manifest
            .cases
            .iter()
            .map(|case| (case.case_id.as_str(), case.source.repository_id.as_str()))
            .collect::<Vec<_>>(),
        [
            ("mom.full-app-runtime-quit-relaunch", "delysis/mom-llama",),
            (
                "speech.quit_relaunch_fake_owners.v1",
                "delysis/speech-native-kit",
            ),
            (
                "loom.full-application-close-and-fresh-runtime",
                "delysis/loom-native",
            ),
            (
                "native-quit-relaunch-fake-owners",
                "delysis/llama-native-kit",
            ),
            ("gateway_owner.quit_relaunch", "delysis/free-token-energy"),
        ]
    );
    assert_eq!(
        manifest.omitted_claims,
        [
            "Apple platform synthesis",
            "Parakeet model transcription",
            "downstream product durable-store format",
            "downstream product user-interface quit behavior",
            "live Tauri window event dispatch",
            "live hosted-provider behavior",
            "loaded native-model worker shutdown",
            "native GUI process relaunch",
            "operating-system process enumeration outside product-owned join receipts",
            "operating-system process relaunch",
            "personal Keychain credential authorization",
            "real GGUF loading or inference",
            "real model inference",
            "real model inference during the lifecycle probe",
            "real model loading or inference",
        ]
    );

    let projections = [
        MOM_PROJECTION,
        SPEECH_PROJECTION,
        LOOM_PROJECTION,
        NATIVE_PROJECTION,
        FTE_PROJECTION,
    ];
    let threat_models = [
        "accepted Mom full-AppRuntime quit, exact owner join, and same-store reopen",
        "accepted Speech host/backend quit and relaunch with exact retained worker identities",
        "accepted Loom full-application generation-owner close and same-project reopen",
        "accepted Native deterministic model-owner quit and same-receipt-store relaunch",
        "accepted FTE production Gateway owner quit and same-database fresh-runtime relaunch",
    ];
    let observations = projections
        .iter()
        .zip(threat_models)
        .enumerate()
        .map(|(index, (bytes, threat_model))| {
            let projection = serde_json::from_slice(bytes).expect("product projection JSON");
            observation(&manifest, index, threat_model, projection)
        })
        .collect::<Vec<_>>();
    let baselines = projections
        .iter()
        .zip(&observations)
        .map(|(expected_projection_bytes, observation)| CaseBaselineV0 {
            expected_projection_bytes,
            verified_prerequisites: &[],
            observation,
        })
        .collect::<Vec<_>>();

    validate_row_baselines(&manifest, &baselines).expect("complete five-product quit/relaunch row");
}
