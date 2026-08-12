#![allow(dead_code)]

use platform_contracts_v0::evidence::EVIDENCE_SCHEMA_V0;
use platform_contracts_v0::{
    ArtifactIdentityV0, ContentDigest, EvidenceClaimV0, EvidenceTier, ExecutionKind, TerminalClass,
};
use platform_vertical_fixtures_v0::{
    ALL_VERTICAL_IDS, ArtifactAvailabilityV0, DurableStateFactV0, EquivalenceProjectionV0,
    EventFactV0, FactValueV0, FixtureArtifactV0, FixtureCaseV0, GitSourceV0, LifecycleFactV0,
    NetworkBoundaryV0, ObservationEnvelopeV0, OwnershipFactsV0, ReplayProgramV0, ReplayRecipeV0,
    StateDispositionV0, VERTICAL_FIXTURE_LOCK_SCHEMA_V0, VERTICAL_FIXTURE_MANIFEST_SCHEMA_V0,
    VERTICAL_OBSERVATION_SCHEMA_V0, VerticalFixtureLockEntryV0, VerticalFixtureLockV0,
    VerticalFixtureManifestV0, VerticalIdV0, sha256_identity,
};
use std::collections::BTreeMap;

pub const CONTRACT_REVISION: &str = "cbab33555ab9355a6ac453d659c55ec9e0666821";
pub const BASELINE_REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const CANDIDATE_REVISION: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

pub fn digest(byte: char) -> ContentDigest {
    ContentDigest::sha256(byte.to_string().repeat(64)).expect("test digest")
}

pub fn artifact(id: &str, byte: char) -> ArtifactIdentityV0 {
    ArtifactIdentityV0 {
        id: id.to_owned(),
        digest: digest(byte),
        length: 1,
    }
}

pub fn projection(vertical_id: VerticalIdV0) -> EquivalenceProjectionV0 {
    let mut output_facts =
        BTreeMap::from([("output_present".to_owned(), FactValueV0::Boolean(true))]);
    if vertical_id == VerticalIdV0::LoomSuggestionPromotion {
        output_facts.extend([
            ("visible_ghost_count".to_owned(), FactValueV0::Integer(1)),
            (
                "ghost_anchor".to_owned(),
                FactValueV0::Text("caret_local".to_owned()),
            ),
            (
                "tab_exact_boundary".to_owned(),
                FactValueV0::Text("promote_visible_ghost".to_owned()),
            ),
            (
                "tab_without_exact_boundary".to_owned(),
                FactValueV0::Text("ordinary_tab_or_indent".to_owned()),
            ),
            (
                "additional_candidates".to_owned(),
                FactValueV0::Text("hidden_until_explicit_review".to_owned()),
            ),
            (
                "persistent_candidate_count".to_owned(),
                FactValueV0::Boolean(false),
            ),
            (
                "skip_to_manuscript_control".to_owned(),
                FactValueV0::Boolean(false),
            ),
            (
                "primary_use_this_control".to_owned(),
                FactValueV0::Boolean(false),
            ),
            (
                "dismissed_manuscript_unchanged".to_owned(),
                FactValueV0::Boolean(true),
            ),
            (
                "stale_manuscript_unchanged".to_owned(),
                FactValueV0::Boolean(true),
            ),
        ]);
    }

    EquivalenceProjectionV0 {
        ordered_events: vec![EventFactV0 {
            sequence: 0,
            operation_id: "operation.primary".to_owned(),
            attempt_id: Some("attempt.1".to_owned()),
            kind: "completed".to_owned(),
            payload: Some(artifact("event.payload", '1')),
        }],
        durable_state: vec![DurableStateFactV0 {
            state_id: "state.primary".to_owned(),
            before: Some(artifact("state.before", '2')),
            after: Some(artifact("state.after", '3')),
            disposition: StateDispositionV0::Updated,
        }],
        lifecycle: vec![LifecycleFactV0 {
            operation_id: "operation.primary".to_owned(),
            attempt_id: Some("attempt.1".to_owned()),
            terminal: TerminalClass::Completed,
            released: true,
        }],
        ownership: OwnershipFactsV0 {
            active_operations: 0,
            retained_tasks: 0,
            expected_workers: 1,
            joined_workers: 1,
        },
        output_facts,
        fail_closed_facts: vec!["stale input did not mutate durable state".to_owned()],
    }
}

pub fn manifest_and_projection(vertical_id: VerticalIdV0) -> (VerticalFixtureManifestV0, Vec<u8>) {
    let projection = projection(vertical_id);
    let projection_bytes = serde_json::to_vec(&projection).expect("projection JSON");
    let expected_projection = sha256_identity("expected.projection", &projection_bytes);
    let network = if vertical_id == VerticalIdV0::FteHostedFixtureLoopback {
        NetworkBoundaryV0::LoopbackOnly
    } else {
        NetworkBoundaryV0::Denied
    };
    let omitted_claims = if vertical_id == VerticalIdV0::FteHostedFixtureLoopback {
        vec!["live hosted-provider behavior".to_owned()]
    } else {
        Vec::new()
    };
    (
        VerticalFixtureManifestV0 {
            schema: VERTICAL_FIXTURE_MANIFEST_SCHEMA_V0.to_owned(),
            vertical_id,
            class: vertical_id.class(),
            contract_revision: CONTRACT_REVISION.to_owned(),
            cases: vec![FixtureCaseV0 {
                case_id: "primary".to_owned(),
                source: GitSourceV0 {
                    repository_id: "delysis/product".to_owned(),
                    commit: BASELINE_REVISION.to_owned(),
                    production_tree: artifact("production.tree", 'b'),
                },
                inputs: vec![FixtureArtifactV0 {
                    identity: artifact("request.input", 'c'),
                    availability: ArtifactAvailabilityV0::CheckedIn,
                    relative_path: Some("tests/fixtures/request.json".to_owned()),
                }],
                state_identities: Vec::new(),
                prerequisites: Vec::new(),
                replay: vec![ReplayRecipeV0 {
                    program: ReplayProgramV0::Cargo,
                    argv: vec![
                        "test".to_owned(),
                        "--test".to_owned(),
                        "w1_verticals".to_owned(),
                    ],
                    required_environment: Vec::new(),
                    network,
                }],
                expected_projection,
            }],
            omitted_claims,
            negative_evidence: Vec::new(),
        },
        projection_bytes,
    )
}

pub fn observation(vertical_id: VerticalIdV0) -> ObservationEnvelopeV0 {
    ObservationEnvelopeV0 {
        schema: VERTICAL_OBSERVATION_SCHEMA_V0.to_owned(),
        vertical_id,
        case_id: "primary".to_owned(),
        implementation_revision: BASELINE_REVISION.to_owned(),
        evidence: EvidenceClaimV0 {
            schema: EVIDENCE_SCHEMA_V0.to_owned(),
            tier: EvidenceTier::Reproducible,
            threat_model: "exact fixture projection only".to_owned(),
            exact_source: digest('b'),
            exact_runtime_or_artifact: digest('d'),
            execution_kind: match vertical_id.class() {
                platform_vertical_fixtures_v0::FixtureClassV0::Real => ExecutionKind::LocalRuntime,
                platform_vertical_fixtures_v0::FixtureClassV0::ModelFree
                | platform_vertical_fixtures_v0::FixtureClassV0::State => ExecutionKind::Fixture,
            },
            omitted_claims: Vec::new(),
            negative_evidence: Vec::new(),
        },
        projection: projection(vertical_id),
    }
}

pub fn complete_lock() -> VerticalFixtureLockV0 {
    VerticalFixtureLockV0 {
        schema: VERTICAL_FIXTURE_LOCK_SCHEMA_V0.to_owned(),
        protocol_commit: "f".repeat(40),
        contract_revision: CONTRACT_REVISION.to_owned(),
        entries: ALL_VERTICAL_IDS
            .iter()
            .copied()
            .map(|vertical_id| VerticalFixtureLockEntryV0 {
                vertical_id,
                class: vertical_id.class(),
                manifest: artifact(&format!("manifest.{vertical_id:?}"), '9'),
            })
            .collect(),
    }
}
