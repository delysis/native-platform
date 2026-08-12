mod support;

use platform_contracts_v0::TerminalClass;
use platform_vertical_fixtures_v0::{
    StateDispositionV0, ValidationError, VerticalIdV0, compare_candidate, validate_baseline,
    validate_observation,
};

#[test]
fn baseline_binds_source_and_exact_projection_bytes() {
    let vertical_id = VerticalIdV0::MomChatCancelRetry;
    let (manifest, expected) = support::manifest_and_projection(vertical_id);
    let observation = support::observation(vertical_id);
    validate_baseline(&manifest, "primary", &expected, &observation).expect("exact baseline");

    let mut wrong_source = observation.clone();
    wrong_source.implementation_revision = support::CANDIDATE_REVISION.to_owned();
    assert_eq!(
        validate_baseline(&manifest, "primary", &expected, &wrong_source),
        Err(ValidationError::Inconsistent {
            field: "implementation_revision"
        })
    );

    let mut tampered = expected;
    tampered.push(b'\n');
    assert!(matches!(
        validate_baseline(&manifest, "primary", &tampered, &observation),
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
    let mut candidate = support::observation(vertical_id);
    candidate.implementation_revision = support::CANDIDATE_REVISION.to_owned();
    candidate.evidence.exact_source = support::digest('e');
    compare_candidate(&manifest, "primary", &expected, &candidate).expect("equivalent candidate");

    candidate.projection.ordered_events[0].kind = "failed".to_owned();
    assert_eq!(
        compare_candidate(&manifest, "primary", &expected, &candidate),
        Err(ValidationError::ProjectionMismatch)
    );
}

#[test]
fn event_state_lifecycle_and_fail_closed_drift_are_detected() {
    let vertical_id = VerticalIdV0::SpeechPeerCancellation;
    let (manifest, expected) = support::manifest_and_projection(vertical_id);
    let base = support::observation(vertical_id);

    let mut state = base.clone();
    state.projection.durable_state[0].after = Some(support::artifact("state.after", 'f'));
    assert!(compare_candidate(&manifest, "primary", &expected, &state).is_err());

    let mut terminal = base.clone();
    terminal.projection.lifecycle[0].terminal = TerminalClass::Cancelled;
    assert!(compare_candidate(&manifest, "primary", &expected, &terminal).is_err());

    let mut fail_closed = base;
    fail_closed.projection.fail_closed_facts.clear();
    assert_eq!(
        compare_candidate(&manifest, "primary", &expected, &fail_closed),
        Err(ValidationError::ProjectionMismatch)
    );
}
