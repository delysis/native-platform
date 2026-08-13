mod support;

use platform_contracts_v0::TerminalClass;
use platform_vertical_fixtures_v0::{
    GitSourceV0, StateDispositionV0, ValidationError, VerticalIdV0, compare_candidate,
    sha256_identity, validate_baseline, validate_observation, verify_prerequisite_chunks,
};

fn authenticated_candidate(
    vertical_id: VerticalIdV0,
) -> (
    platform_vertical_fixtures_v0::ObservationEnvelopeV0,
    GitSourceV0,
    Vec<u8>,
) {
    let bytes = b"candidate production tree".to_vec();
    let source = GitSourceV0 {
        repository_id: "delysis/product".to_owned(),
        commit: support::CANDIDATE_REVISION.to_owned(),
        production_tree: sha256_identity("candidate.production.tree", &bytes),
    };
    let mut observation = support::observation(vertical_id);
    observation
        .implementation_revision
        .clone_from(&source.commit);
    observation
        .evidence
        .exact_source
        .clone_from(&source.production_tree.digest);
    (observation, source, bytes)
}

#[test]
fn baseline_binds_source_and_exact_projection_bytes() {
    let vertical_id = VerticalIdV0::MomChatCancelRetry;
    let (manifest, expected) = support::manifest_and_projection(vertical_id);
    let observation = support::observation(vertical_id);
    validate_baseline(&manifest, "primary", &expected, &[], &observation).expect("exact baseline");

    let mut wrong_source = observation.clone();
    wrong_source.implementation_revision = support::CANDIDATE_REVISION.to_owned();
    assert_eq!(
        validate_baseline(&manifest, "primary", &expected, &[], &wrong_source),
        Err(ValidationError::Inconsistent {
            field: "implementation_revision"
        })
    );

    let mut tampered = expected;
    tampered.push(b'\n');
    assert!(matches!(
        validate_baseline(&manifest, "primary", &tampered, &[], &observation),
        Err(ValidationError::LengthMismatch {
            field: "expected_projection"
        })
    ));
}

#[test]
fn durable_state_dispositions_must_match_their_before_after_shapes() {
    let mut observation = support::observation(VerticalIdV0::FteLegacyDatabase);
    observation.projection.durable_state[0].disposition = StateDispositionV0::Created;
    assert_eq!(
        validate_observation(&observation),
        Err(ValidationError::Inconsistent {
            field: "durable_state.disposition"
        })
    );

    observation.projection.durable_state[0].before = None;
    validate_observation(&observation).expect("created state has no predecessor");

    observation.projection.ownership.expected_workers = 0;
    observation.projection.ownership.joined_workers = 0;
    validate_observation(&observation)
        .expect("synchronous state migration need not invent a worker");
}

#[test]
fn candidate_revision_may_change_but_observable_behavior_may_not() {
    let vertical_id = VerticalIdV0::InformationInstallQuery;
    let (manifest, expected) = support::manifest_and_projection(vertical_id);
    let (mut candidate, source, source_bytes) = authenticated_candidate(vertical_id);
    compare_candidate(
        &manifest,
        "primary",
        &expected,
        &source,
        &source_bytes,
        &[],
        &candidate,
    )
    .expect("equivalent authenticated candidate");

    candidate.projection.ordered_events[0].kind = "failed".to_owned();
    assert_eq!(
        compare_candidate(
            &manifest,
            "primary",
            &expected,
            &source,
            &source_bytes,
            &[],
            &candidate,
        ),
        Err(ValidationError::ProjectionMismatch)
    );
}

#[test]
fn candidate_source_claim_is_authenticated_and_revision_bound() {
    let vertical_id = VerticalIdV0::InformationInstallQuery;
    let (manifest, expected) = support::manifest_and_projection(vertical_id);
    let (candidate, source, source_bytes) = authenticated_candidate(vertical_id);

    let mut wrong_claim = candidate.clone();
    wrong_claim.evidence.exact_source = support::digest('e');
    assert_eq!(
        compare_candidate(
            &manifest,
            "primary",
            &expected,
            &source,
            &source_bytes,
            &[],
            &wrong_claim,
        ),
        Err(ValidationError::Inconsistent {
            field: "evidence.exact_source"
        })
    );

    let mut tampered_bytes = source_bytes.clone();
    tampered_bytes.push(0);
    assert!(matches!(
        compare_candidate(
            &manifest,
            "primary",
            &expected,
            &source,
            &tampered_bytes,
            &[],
            &candidate,
        ),
        Err(ValidationError::LengthMismatch {
            field: "candidate_source.production_tree"
        })
    ));

    let mut wrong_revision = candidate;
    wrong_revision.implementation_revision = support::BASELINE_REVISION.to_owned();
    assert_eq!(
        compare_candidate(
            &manifest,
            "primary",
            &expected,
            &source,
            &source_bytes,
            &[],
            &wrong_revision,
        ),
        Err(ValidationError::Inconsistent {
            field: "implementation_revision"
        })
    );

    let mut wrong_repository = source.clone();
    wrong_repository.repository_id = "other/product".to_owned();
    let mut repository_candidate = support::observation(vertical_id);
    repository_candidate
        .implementation_revision
        .clone_from(&source.commit);
    repository_candidate
        .evidence
        .exact_source
        .clone_from(&source.production_tree.digest);
    assert_eq!(
        compare_candidate(
            &manifest,
            "primary",
            &expected,
            &wrong_repository,
            &source_bytes,
            &[],
            &repository_candidate,
        ),
        Err(ValidationError::Inconsistent {
            field: "candidate_source.repository_id"
        })
    );
}

#[test]
fn candidate_consumes_stream_verified_exact_prerequisite() {
    let vertical_id = VerticalIdV0::CurrentExactQwen;
    let (manifest, expected) = support::manifest_and_projection(vertical_id);
    let (candidate, source, source_bytes) = authenticated_candidate(vertical_id);
    let stored = support::prerequisite_bytes(vertical_id);
    let verified = stored
        .iter()
        .map(|(prerequisite_id, bytes)| {
            let prerequisite = manifest.cases[0]
                .prerequisites
                .iter()
                .find(|prerequisite| prerequisite.prerequisite_id == *prerequisite_id)
                .expect("declared prerequisite");
            verify_prerequisite_chunks(prerequisite_id, &prerequisite.identity, bytes.chunks(2))
                .expect("streamed prerequisite")
        })
        .collect::<Vec<_>>();
    compare_candidate(
        &manifest,
        "primary",
        &expected,
        &source,
        &source_bytes,
        &verified,
        &candidate,
    )
    .expect("candidate exact streamed prerequisite");

    let mut substituted_bytes = stored[0].1.clone();
    substituted_bytes[0] ^= 1;
    assert!(matches!(
        verify_prerequisite_chunks(
            stored[0].0.as_str(),
            &manifest.cases[0].prerequisites[0].identity,
            substituted_bytes.chunks(2),
        ),
        Err(ValidationError::DigestMismatch {
            field: "prerequisite_chunks"
        })
    ));
}

#[test]
fn event_state_lifecycle_and_fail_closed_drift_are_detected() {
    let vertical_id = VerticalIdV0::SpeechPeerCancellation;
    let (manifest, expected) = support::manifest_and_projection(vertical_id);
    let (base, source, source_bytes) = authenticated_candidate(vertical_id);

    let mut state = base.clone();
    state.projection.durable_state[0].after = Some(support::artifact("state.after", 'f'));
    assert!(
        compare_candidate(
            &manifest,
            "primary",
            &expected,
            &source,
            &source_bytes,
            &[],
            &state,
        )
        .is_err()
    );

    let mut terminal = base.clone();
    terminal.projection.lifecycle[0].terminal = TerminalClass::Cancelled;
    assert!(
        compare_candidate(
            &manifest,
            "primary",
            &expected,
            &source,
            &source_bytes,
            &[],
            &terminal,
        )
        .is_err()
    );

    let mut fail_closed = base;
    fail_closed.projection.fail_closed_facts.clear();
    assert_eq!(
        compare_candidate(
            &manifest,
            "primary",
            &expected,
            &source,
            &source_bytes,
            &[],
            &fail_closed,
        ),
        Err(ValidationError::Empty {
            field: "projection.fail_closed_facts"
        })
    );
}

#[test]
fn projection_rejects_unbound_or_duplicate_lifecycle_and_empty_fail_closed_facts() {
    let mut unrelated = support::observation(VerticalIdV0::SpeechPeerCancellation);
    unrelated.projection.lifecycle[0].operation_id = "operation.unrelated".to_owned();
    assert_eq!(
        validate_observation(&unrelated),
        Err(ValidationError::Inconsistent {
            field: "projection.event_lifecycle_identity"
        })
    );

    let mut mismatched_correlation = support::observation(VerticalIdV0::SpeechPeerCancellation);
    mismatched_correlation.projection.ordered_events[0].correlation_id =
        Some("journey.one".to_owned());
    assert_eq!(
        validate_observation(&mismatched_correlation),
        Err(ValidationError::Inconsistent {
            field: "projection.event_lifecycle_identity"
        })
    );

    let mut duplicate = support::observation(VerticalIdV0::SpeechPeerCancellation);
    duplicate
        .projection
        .lifecycle
        .push(duplicate.projection.lifecycle[0].clone());
    assert_eq!(
        validate_observation(&duplicate),
        Err(ValidationError::Duplicate {
            field: "projection.lifecycle.identity"
        })
    );

    let mut fail_closed = support::observation(VerticalIdV0::SpeechPeerCancellation);
    fail_closed.projection.fail_closed_facts.clear();
    assert_eq!(
        validate_observation(&fail_closed),
        Err(ValidationError::Empty {
            field: "projection.fail_closed_facts"
        })
    );
}

#[test]
fn mom_cancel_retry_requires_distinct_attempt_identities() {
    let mut observation = support::observation(VerticalIdV0::MomChatCancelRetry);
    observation.projection.ordered_events.pop();
    observation.projection.lifecycle.pop();
    assert_eq!(
        validate_observation(&observation),
        Err(ValidationError::Invalid {
            field: "mom_chat_cancel_retry.lifecycle"
        })
    );

    let mut reversed = support::observation(VerticalIdV0::MomChatCancelRetry);
    reversed.projection.ordered_events.swap(0, 1);
    reversed.projection.ordered_events[0].sequence = 0;
    reversed.projection.ordered_events[1].sequence = 1;
    assert_eq!(
        validate_observation(&reversed),
        Err(ValidationError::Invalid {
            field: "mom_chat_cancel_retry.lifecycle"
        })
    );

    let mut shared_operation = support::observation(VerticalIdV0::MomChatCancelRetry);
    let event_operation = shared_operation.projection.ordered_events[0]
        .operation_id
        .clone();
    let lifecycle_operation = shared_operation.projection.lifecycle[0]
        .operation_id
        .clone();
    shared_operation.projection.ordered_events[1]
        .operation_id
        .clone_from(&event_operation);
    shared_operation.projection.lifecycle[1]
        .operation_id
        .clone_from(&lifecycle_operation);
    assert_eq!(
        validate_observation(&shared_operation),
        Err(ValidationError::Invalid {
            field: "mom_chat_cancel_retry.lifecycle"
        })
    );

    let mut unrelated_retry = support::observation(VerticalIdV0::MomChatCancelRetry);
    unrelated_retry.projection.ordered_events[1].correlation_id =
        Some("different.journey".to_owned());
    unrelated_retry.projection.lifecycle[1].correlation_id = Some("different.journey".to_owned());
    assert_eq!(
        validate_observation(&unrelated_retry),
        Err(ValidationError::Invalid {
            field: "mom_chat_cancel_retry.lifecycle"
        })
    );
}
