mod support;

use platform_vertical_fixtures_v0::{
    CaseBaselineV0, ValidationError, VerticalIdV0, validate_row_baselines,
};

#[test]
fn all_manifest_cases_must_be_observed_exactly_once() {
    let vertical_id = VerticalIdV0::CorruptedDisposableCaches;
    let (mut manifest, expected) = support::manifest_and_projection(vertical_id);
    let first = support::observation(vertical_id);
    let mut second_case = manifest.cases[0].clone();
    second_case.case_id = "secondary".to_owned();
    manifest.cases.push(second_case);
    let mut second = first.clone();
    second.case_id = "secondary".to_owned();

    validate_row_baselines(
        &manifest,
        &[
            CaseBaselineV0 {
                expected_projection_bytes: &expected,
                verified_prerequisites: &[],
                observation: &first,
            },
            CaseBaselineV0 {
                expected_projection_bytes: &expected,
                verified_prerequisites: &[],
                observation: &second,
            },
        ],
    )
    .expect("complete cross-product row");

    assert_eq!(
        validate_row_baselines(
            &manifest,
            &[CaseBaselineV0 {
                expected_projection_bytes: &expected,
                verified_prerequisites: &[],
                observation: &first,
            }],
        ),
        Err(ValidationError::Inconsistent {
            field: "row_baselines.cases"
        })
    );

    assert_eq!(
        validate_row_baselines(
            &manifest,
            &[
                CaseBaselineV0 {
                    expected_projection_bytes: &expected,
                    verified_prerequisites: &[],
                    observation: &first,
                },
                CaseBaselineV0 {
                    expected_projection_bytes: &expected,
                    verified_prerequisites: &[],
                    observation: &first,
                },
            ],
        ),
        Err(ValidationError::Duplicate {
            field: "row_baselines.case_id"
        })
    );
}

#[test]
fn row_aggregation_does_not_merge_or_weaken_case_projections() {
    let vertical_id = VerticalIdV0::CorruptedDisposableCaches;
    let (manifest, expected) = support::manifest_and_projection(vertical_id);
    let mut observation = support::observation(vertical_id);
    observation.projection.output_facts.insert(
        "invented_cross_product_claim".to_owned(),
        platform_vertical_fixtures_v0::FactValueV0::Boolean(true),
    );

    assert_eq!(
        validate_row_baselines(
            &manifest,
            &[CaseBaselineV0 {
                expected_projection_bytes: &expected,
                verified_prerequisites: &[],
                observation: &observation,
            }],
        ),
        Err(ValidationError::ProjectionMismatch)
    );
}

#[test]
fn every_observation_must_repeat_the_exact_manifest_omissions() {
    let vertical_id = VerticalIdV0::FteHostedFixtureLoopback;
    let (manifest, expected) = support::manifest_and_projection(vertical_id);
    let observation = support::observation(vertical_id);

    validate_row_baselines(
        &manifest,
        &[CaseBaselineV0 {
            expected_projection_bytes: &expected,
            verified_prerequisites: &[],
            observation: &observation,
        }],
    )
    .expect("exact omissions pass");

    let mut cleared = observation.clone();
    cleared.evidence.omitted_claims.clear();
    assert_eq!(
        validate_row_baselines(
            &manifest,
            &[CaseBaselineV0 {
                expected_projection_bytes: &expected,
                verified_prerequisites: &[],
                observation: &cleared,
            }],
        ),
        Err(ValidationError::Inconsistent {
            field: "evidence.omitted_claims"
        })
    );

    let mut changed = observation;
    changed.evidence.omitted_claims = vec!["some other omission".to_owned()];
    assert_eq!(
        validate_row_baselines(
            &manifest,
            &[CaseBaselineV0 {
                expected_projection_bytes: &expected,
                verified_prerequisites: &[],
                observation: &changed,
            }],
        ),
        Err(ValidationError::Inconsistent {
            field: "evidence.omitted_claims"
        })
    );
}
