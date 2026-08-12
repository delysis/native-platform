use platform_contracts_v0::evidence::EVIDENCE_SCHEMA_V0;
use platform_contracts_v0::{EvidenceClaimV0, EvidenceTier, ExecutionKind};
use platform_vertical_fixtures_v0::{
    CaseBaselineV0, EquivalenceProjectionV0, ObservationEnvelopeV0, VERTICAL_OBSERVATION_SCHEMA_V0,
    VerticalFixtureManifestV0, VerticalIdV0, validate_row_baselines,
};

const CACHE_MANIFEST: &[u8] =
    include_bytes!("../../../verticals/v0/corrupted-disposable-caches.manifest.json");
const MOM_CACHE_PROJECTION: &[u8] =
    include_bytes!("../../../verticals/v0/corrupted-disposable-caches.mom.projection.json");
const INFORMATION_CACHE_PROJECTION: &[u8] =
    include_bytes!("../../../verticals/v0/corrupted-disposable-caches.information.projection.json");

#[test]
fn staged_cache_bundle_preserves_each_exact_claim_boundary() {
    let manifest: VerticalFixtureManifestV0 =
        serde_json::from_slice(CACHE_MANIFEST).expect("staged cache manifest JSON");
    let projection: EquivalenceProjectionV0 =
        serde_json::from_slice(MOM_CACHE_PROJECTION).expect("staged Mom cache projection JSON");
    let information_projection: EquivalenceProjectionV0 =
        serde_json::from_slice(INFORMATION_CACHE_PROJECTION)
            .expect("staged Information cache projection JSON");
    assert_eq!(
        manifest.vertical_id,
        VerticalIdV0::CorruptedDisposableCaches
    );
    assert_eq!(manifest.cases.len(), 2);
    assert_eq!(manifest.cases[0].case_id, "mom.disposable-cache-corruption");
    assert_eq!(
        manifest.cases[1].case_id,
        "information.corrupted-disposable-acquisition-cache.v0"
    );
    assert_eq!(
        manifest.cases[1].source.repository_id,
        "delysis/information-native-kit"
    );
    assert_eq!(
        manifest.omitted_claims,
        [
            "FTE provider-owned remote caches",
            "Information identity-bound HTTP resume sidecar and partial-artifact corruption are not yet frozen as a vertical projection; row incomplete",
            "Loom disposable caches because none are product-owned",
            "Native persistent cache storage because native-kit owns only cache values",
            "No authoritative staging manifest, artifact target, or ready receipt is malformed or discarded.",
            "No live network acquisition is exercised.",
            "Speech Hugging Face model cache because it is externally owned",
        ]
    );

    let mom_case = &manifest.cases[0];
    let mom_observation = ObservationEnvelopeV0 {
        schema: VERTICAL_OBSERVATION_SCHEMA_V0.to_owned(),
        vertical_id: manifest.vertical_id,
        case_id: mom_case.case_id.clone(),
        implementation_revision: mom_case.source.commit.clone(),
        observed_prerequisites: Vec::new(),
        evidence: EvidenceClaimV0 {
            schema: EVIDENCE_SCHEMA_V0.to_owned(),
            tier: EvidenceTier::Reproducible,
            threat_model: "accepted Mom feature-gated cache replay; no non-Mom cache claim"
                .to_owned(),
            exact_source: mom_case.source.production_tree.digest.clone(),
            exact_runtime_or_artifact: mom_case.expected_projection.digest.clone(),
            execution_kind: ExecutionKind::Fixture,
            omitted_claims: manifest.omitted_claims.clone(),
            negative_evidence: Vec::new(),
        },
        projection,
    };
    let information_case = &manifest.cases[1];
    let information_observation = ObservationEnvelopeV0 {
        schema: VERTICAL_OBSERVATION_SCHEMA_V0.to_owned(),
        vertical_id: manifest.vertical_id,
        case_id: information_case.case_id.clone(),
        implementation_revision: information_case.source.commit.clone(),
        observed_prerequisites: Vec::new(),
        evidence: EvidenceClaimV0 {
            schema: EVIDENCE_SCHEMA_V0.to_owned(),
            tier: EvidenceTier::Reproducible,
            threat_model: "accepted Information acquisition-journal temporary cleanup; no resumable HTTP sidecar claim".to_owned(),
            exact_source: information_case.source.production_tree.digest.clone(),
            exact_runtime_or_artifact: information_case.expected_projection.digest.clone(),
            execution_kind: ExecutionKind::Fixture,
            omitted_claims: manifest.omitted_claims.clone(),
            negative_evidence: Vec::new(),
        },
        projection: information_projection,
    };

    validate_row_baselines(
        &manifest,
        &[
            CaseBaselineV0 {
                expected_projection_bytes: MOM_CACHE_PROJECTION,
                verified_prerequisites: &[],
                observation: &mom_observation,
            },
            CaseBaselineV0 {
                expected_projection_bytes: INFORMATION_CACHE_PROJECTION,
                verified_prerequisites: &[],
                observation: &information_observation,
            },
        ],
    )
    .expect("staged Mom and Information corrupted-cache bundle");
}

#[test]
fn staged_cache_replays_are_fully_qualified_exact_product_tests() {
    let manifest: VerticalFixtureManifestV0 =
        serde_json::from_slice(CACHE_MANIFEST).expect("staged cache manifest JSON");
    let mom_replay = &manifest.cases[0].replay;
    assert_eq!(mom_replay.len(), 1);
    assert_eq!(
        mom_replay[0].argv,
        [
            "test",
            "-p",
            "mom-llama-runtime",
            "--features",
            "unstable-w1-vertical-fixtures",
            "kv_cache::tests::w1_authenticated_persistent_cache_corruption_invalidates_and_falls_back",
            "--",
            "--exact",
        ]
    );
    let information_replay = &manifest.cases[1].replay;
    assert_eq!(information_replay.len(), 1);
    assert_eq!(
        information_replay[0].argv,
        [
            "test",
            "--locked",
            "-p",
            "information-native-host",
            "--features",
            "unstable-w1-vertical-tests",
            "--test",
            "w1_vertical",
            "corrupted_disposable_acquisition_cache_is_cold_reset",
            "--",
            "--exact",
        ]
    );
}
