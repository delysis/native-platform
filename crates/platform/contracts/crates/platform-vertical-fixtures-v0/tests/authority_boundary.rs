mod support;

use platform_contracts_v0::ExecutionKind;
use platform_vertical_fixtures_v0::{
    NegativeEvidenceV0, NetworkBoundaryV0, PrerequisiteKindV0, PrerequisiteV0, ValidationError,
    VerticalFixtureManifestV0, VerticalIdV0, validate_baseline, validate_manifest,
    validate_observation,
};

#[test]
fn unknown_authority_credential_and_schedule_fields_fail_deserialization() {
    let (manifest, _) = support::manifest_and_projection(VerticalIdV0::MomAttachment);
    for field in ["authority", "api_key", "schedule", "foreground_command"] {
        let mut json = serde_json::to_value(&manifest).expect("manifest JSON");
        json.as_object_mut()
            .expect("manifest object")
            .insert(field.to_owned(), serde_json::json!("forbidden"));
        assert!(
            serde_json::from_value::<VerticalFixtureManifestV0>(json).is_err(),
            "unknown {field} field must fail closed"
        );
    }

    let mut nested = serde_json::to_value(&manifest).expect("manifest JSON");
    nested["cases"][0]["replay"][0]["schedule"] = serde_json::json!("weekly");
    assert!(serde_json::from_value::<VerticalFixtureManifestV0>(nested).is_err());
}

#[test]
fn replay_records_argv_and_non_secret_environment_names_only() {
    let (mut manifest, _) = support::manifest_and_projection(VerticalIdV0::CurrentExactQwen);
    manifest.cases[0].replay[0].required_environment = vec!["MOM_LLAMA_MODEL_PATH".to_owned()];
    validate_manifest(&manifest).expect("external model path name is non-secret");

    for forbidden in [
        "CEREBRAS_API_KEY",
        "CEREBRAS_KEY",
        "PASSWORD",
        "HOSTED_ACCESS_TOKEN",
    ] {
        manifest.cases[0].replay[0].required_environment = vec![forbidden.to_owned()];
        assert_eq!(
            validate_manifest(&manifest),
            Err(ValidationError::Invalid {
                field: "replay.required_environment"
            })
        );
    }
}

#[test]
fn hosted_protocol_fixture_requires_no_credential_or_remote_network() {
    let (mut manifest, _) =
        support::manifest_and_projection(VerticalIdV0::FteHostedFixtureLoopback);
    assert!(manifest.cases[0].prerequisites.is_empty());
    assert_eq!(
        manifest.cases[0].replay[0].network,
        NetworkBoundaryV0::LoopbackOnly
    );
    validate_manifest(&manifest).expect("provider-independent loopback fixture");

    manifest.omitted_claims.clear();
    assert_eq!(
        validate_manifest(&manifest),
        Err(ValidationError::Invalid {
            field: "omitted_claims.live_hosted_provider"
        })
    );

    let (mut manifest, _) =
        support::manifest_and_projection(VerticalIdV0::FteHostedFixtureLoopback);
    manifest.cases[0].prerequisites.push(PrerequisiteV0 {
        prerequisite_id: "provider.cerebras".to_owned(),
        kind: PrerequisiteKindV0::LocalRuntime,
        identity: support::artifact("credential.requirement", '7'),
    });
    assert_eq!(
        validate_manifest(&manifest),
        Err(ValidationError::Invalid {
            field: "cases.prerequisites.hosted_fixture"
        })
    );
}

#[test]
fn hosted_network_observations_cannot_be_vertical_evidence() {
    let mut observation = support::observation(VerticalIdV0::CurrentExactGemma);
    observation.evidence.execution_kind = ExecutionKind::HostedNetwork;
    assert_eq!(
        validate_observation(&observation),
        Err(ValidationError::Inconsistent {
            field: "evidence.execution_kind"
        })
    );
}

#[test]
fn negative_evidence_is_preserved_but_cannot_pass() {
    let vertical_id = VerticalIdV0::MomChatCancelRetry;
    let (mut manifest, expected) = support::manifest_and_projection(vertical_id);
    manifest.negative_evidence.push(NegativeEvidenceV0 {
        evidence_id: "cancelled.attempt.laundered".to_owned(),
        disposition: platform_vertical_fixtures_v0::EvidenceDispositionV0::Rejected,
        artifact: support::artifact("negative.transcript", '8'),
        reason: "retry reused the cancelled attempt identity".to_owned(),
    });
    validate_manifest(&manifest).expect("negative record remains reviewable");

    let mut observation = support::observation(vertical_id);
    observation
        .evidence
        .negative_evidence
        .push("cancelled attempt was laundered".to_owned());
    assert_eq!(
        validate_baseline(&manifest, "primary", &expected, &observation),
        Err(ValidationError::Invalid {
            field: "evidence.negative_evidence"
        })
    );
}
