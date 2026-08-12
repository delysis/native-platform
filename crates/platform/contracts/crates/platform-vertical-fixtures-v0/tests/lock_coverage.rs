mod support;

use platform_vertical_fixtures_v0::{
    ALL_VERTICAL_IDS, FixtureClassV0, ValidationError, validate_lock,
};
use std::collections::BTreeSet;

#[test]
fn closed_catalog_has_exact_section_16_classification() {
    assert_eq!(ALL_VERTICAL_IDS.len(), 18);
    assert_eq!(
        ALL_VERTICAL_IDS
            .iter()
            .filter(|id| id.class() == FixtureClassV0::ModelFree)
            .count(),
        8
    );
    assert_eq!(
        ALL_VERTICAL_IDS
            .iter()
            .filter(|id| id.class() == FixtureClassV0::Real)
            .count(),
        4
    );
    assert_eq!(
        ALL_VERTICAL_IDS
            .iter()
            .filter(|id| id.class() == FixtureClassV0::State)
            .count(),
        6
    );
    assert_eq!(
        ALL_VERTICAL_IDS
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .len(),
        18
    );
}

#[test]
fn lock_requires_every_row_exactly_once() {
    let lock = support::complete_lock();
    validate_lock(&lock).expect("complete lock");

    let mut missing = lock.clone();
    missing.entries.pop();
    assert!(matches!(
        validate_lock(&missing),
        Err(ValidationError::MissingVerticals(_))
    ));

    let mut duplicate = lock;
    duplicate.entries[1].vertical_id = duplicate.entries[0].vertical_id;
    assert_eq!(
        validate_lock(&duplicate),
        Err(ValidationError::Duplicate {
            field: "entries.vertical_id"
        })
    );
}

#[test]
fn lock_cannot_silently_move_the_accepted_contract_revision() {
    let mut lock = support::complete_lock();
    lock.contract_revision = "a".repeat(40);
    assert_eq!(
        validate_lock(&lock),
        Err(ValidationError::Invalid {
            field: "contract_revision"
        })
    );
}
