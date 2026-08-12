#![allow(dead_code)]

use platform_contracts_v0::evidence::EVIDENCE_SCHEMA_V0;
use platform_contracts_v0::{
    ArtifactIdentityV0, ContentDigest, EvidenceClaimV0, EvidenceTier, ExecutionKind, TerminalClass,
};
use platform_vertical_fixtures_v0::{
    ALL_VERTICAL_IDS, ArtifactAvailabilityV0, DurableStateFactV0, EquivalenceProjectionV0,
    EventFactV0, FactValueV0, FixtureArtifactV0, FixtureCaseV0, GitSourceV0, LifecycleFactV0,
    NetworkBoundaryV0, ObservationEnvelopeV0, OwnershipFactsV0, PrerequisiteKindV0, PrerequisiteV0,
    ReplayProgramV0, ReplayRecipeV0, StateDispositionV0, StateIdentityV0,
    VERTICAL_FIXTURE_LOCK_SCHEMA_V0, VERTICAL_FIXTURE_MANIFEST_SCHEMA_V0,
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

    let mut ordered_events = vec![EventFactV0 {
        sequence: 0,
        operation_id: "operation.primary".to_owned(),
        attempt_id: Some("attempt.1".to_owned()),
        correlation_id: None,
        kind: "completed".to_owned(),
        payload: Some(artifact("event.payload", '1')),
    }];
    let mut lifecycle = vec![LifecycleFactV0 {
        operation_id: "operation.primary".to_owned(),
        attempt_id: Some("attempt.1".to_owned()),
        correlation_id: None,
        terminal: TerminalClass::Completed,
        released: true,
    }];
    if vertical_id == VerticalIdV0::MomChatCancelRetry {
        "operation.cancelled".clone_into(&mut ordered_events[0].operation_id);
        ordered_events[0].correlation_id = Some("chat.retry.journey".to_owned());
        "cancelled".clone_into(&mut ordered_events[0].kind);
        "operation.cancelled".clone_into(&mut lifecycle[0].operation_id);
        lifecycle[0].correlation_id = Some("chat.retry.journey".to_owned());
        lifecycle[0].terminal = TerminalClass::Cancelled;
        ordered_events.push(EventFactV0 {
            sequence: 1,
            operation_id: "operation.retry".to_owned(),
            attempt_id: Some("attempt.2".to_owned()),
            correlation_id: Some("chat.retry.journey".to_owned()),
            kind: "completed".to_owned(),
            payload: Some(artifact("event.payload", '1')),
        });
        lifecycle.push(LifecycleFactV0 {
            operation_id: "operation.retry".to_owned(),
            attempt_id: Some("attempt.2".to_owned()),
            correlation_id: Some("chat.retry.journey".to_owned()),
            terminal: TerminalClass::Completed,
            released: true,
        });
    }

    EquivalenceProjectionV0 {
        ordered_events,
        durable_state: vec![DurableStateFactV0 {
            state_id: "state.primary".to_owned(),
            schema_id: "state.schema.v0".to_owned(),
            before: Some(artifact("state.before", '2')),
            after: Some(artifact("state.after", '3')),
            disposition: StateDispositionV0::Updated,
        }],
        lifecycle,
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
    let prerequisites = prerequisites(vertical_id);
    let state_identities =
        if vertical_id.class() == platform_vertical_fixtures_v0::FixtureClassV0::State {
            vec![StateIdentityV0 {
                state_id: "state.primary".to_owned(),
                schema_id: "state.schema.v0".to_owned(),
                baseline: FixtureArtifactV0 {
                    identity: artifact("state.before", '2'),
                    availability: ArtifactAvailabilityV0::CheckedIn,
                    relative_path: Some("tests/fixtures/state.before".to_owned()),
                },
            }]
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
                state_identities,
                prerequisites,
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

pub fn prerequisite_bytes(vertical_id: VerticalIdV0) -> Vec<(String, Vec<u8>)> {
    match vertical_id {
        VerticalIdV0::CurrentExactQwen => vec![(
            "model.qwen.gguf".to_owned(),
            b"exact qwen gguf fixture bytes".to_vec(),
        )],
        VerticalIdV0::CurrentExactGemma => vec![(
            "model.gemma.gguf".to_owned(),
            b"exact gemma gguf fixture bytes".to_vec(),
        )],
        VerticalIdV0::CurrentParakeetModelAudio => vec![
            (
                "model.parakeet".to_owned(),
                b"exact parakeet model fixture bytes".to_vec(),
            ),
            (
                "audio.input".to_owned(),
                b"exact parakeet audio fixture bytes".to_vec(),
            ),
        ],
        VerticalIdV0::AppleInstalledVoice => vec![(
            "voice.apple.installed".to_owned(),
            b"exact installed Apple voice inventory".to_vec(),
        )],
        _ => Vec::new(),
    }
}

fn prerequisites(vertical_id: VerticalIdV0) -> Vec<PrerequisiteV0> {
    prerequisite_bytes(vertical_id)
        .into_iter()
        .map(|(prerequisite_id, bytes)| {
            let kind = if vertical_id == VerticalIdV0::AppleInstalledVoice {
                PrerequisiteKindV0::PlatformInventory
            } else {
                PrerequisiteKindV0::ExactExternalArtifact
            };
            PrerequisiteV0 {
                identity: sha256_identity(format!("{prerequisite_id}.bytes"), &bytes),
                prerequisite_id,
                kind,
            }
        })
        .collect()
}

pub fn observation(vertical_id: VerticalIdV0) -> ObservationEnvelopeV0 {
    ObservationEnvelopeV0 {
        schema: VERTICAL_OBSERVATION_SCHEMA_V0.to_owned(),
        vertical_id,
        case_id: "primary".to_owned(),
        implementation_revision: BASELINE_REVISION.to_owned(),
        observed_prerequisites: prerequisites(vertical_id),
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

pub fn complete_lock() -> (VerticalFixtureLockV0, Vec<Vec<u8>>) {
    let manifests = ALL_VERTICAL_IDS
        .iter()
        .copied()
        .map(|vertical_id| {
            let (manifest, _) = manifest_and_projection(vertical_id);
            serde_json::to_vec(&manifest).expect("manifest JSON")
        })
        .collect::<Vec<_>>();
    let entries = ALL_VERTICAL_IDS
        .iter()
        .copied()
        .zip(&manifests)
        .map(|(vertical_id, bytes)| VerticalFixtureLockEntryV0 {
            vertical_id,
            class: vertical_id.class(),
            manifest: sha256_identity(format!("manifest.{vertical_id:?}"), bytes),
        })
        .collect();
    (
        VerticalFixtureLockV0 {
            schema: VERTICAL_FIXTURE_LOCK_SCHEMA_V0.to_owned(),
            protocol_commit: "f".repeat(40),
            contract_revision: CONTRACT_REVISION.to_owned(),
            entries,
        },
        manifests,
    )
}
