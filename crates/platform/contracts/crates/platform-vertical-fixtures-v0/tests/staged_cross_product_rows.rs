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

#[test]
fn staged_mom_cache_slice_preserves_its_exact_claim_boundary() {
    let manifest: VerticalFixtureManifestV0 =
        serde_json::from_slice(CACHE_MANIFEST).expect("staged cache manifest JSON");
    let projection: EquivalenceProjectionV0 =
        serde_json::from_slice(MOM_CACHE_PROJECTION).expect("staged Mom cache projection JSON");
    assert_eq!(
        manifest.vertical_id,
        VerticalIdV0::CorruptedDisposableCaches
    );
    assert_eq!(manifest.cases.len(), 1);
    assert_eq!(manifest.cases[0].case_id, "mom.disposable-cache-corruption");
    assert_eq!(
        manifest.omitted_claims,
        [
            "FTE provider-owned remote caches",
            "required Information resumable-staging corruption case is not yet frozen; row incomplete",
            "Loom disposable caches because none are product-owned",
            "Native persistent cache storage because native-kit owns only cache values",
            "Speech Hugging Face model cache because it is externally owned",
        ]
    );

    let case = &manifest.cases[0];
    let observation = ObservationEnvelopeV0 {
        schema: VERTICAL_OBSERVATION_SCHEMA_V0.to_owned(),
        vertical_id: manifest.vertical_id,
        case_id: case.case_id.clone(),
        implementation_revision: case.source.commit.clone(),
        observed_prerequisites: Vec::new(),
        evidence: EvidenceClaimV0 {
            schema: EVIDENCE_SCHEMA_V0.to_owned(),
            tier: EvidenceTier::Reproducible,
            threat_model: "accepted Mom feature-gated cache replay; no non-Mom cache claim"
                .to_owned(),
            exact_source: case.source.production_tree.digest.clone(),
            exact_runtime_or_artifact: case.expected_projection.digest.clone(),
            execution_kind: ExecutionKind::Fixture,
            omitted_claims: manifest.omitted_claims.clone(),
            negative_evidence: Vec::new(),
        },
        projection,
    };

    validate_row_baselines(
        &manifest,
        &[CaseBaselineV0 {
            expected_projection_bytes: MOM_CACHE_PROJECTION,
            verified_prerequisites: &[],
            observation: &observation,
        }],
    )
    .expect("staged Mom-scoped corrupted-cache slice");
}

#[test]
fn staged_mom_cache_replay_is_one_fully_qualified_exact_product_test() {
    let manifest: VerticalFixtureManifestV0 =
        serde_json::from_slice(CACHE_MANIFEST).expect("staged cache manifest JSON");
    let replay = &manifest.cases[0].replay;
    assert_eq!(replay.len(), 1);
    assert_eq!(
        replay[0].argv,
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
}
