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
    let (lock, manifests) = support::complete_lock();
    validate_lock(&lock, manifests.iter().map(Vec::as_slice)).expect("complete authenticated lock");

    let mut missing = lock.clone();
    missing.entries.pop();
    assert!(matches!(
        validate_lock(&missing, manifests.iter().map(Vec::as_slice)),
        Err(ValidationError::MissingVerticals(_))
    ));

    let mut duplicate = lock;
    duplicate.entries[1].vertical_id = duplicate.entries[0].vertical_id;
    assert_eq!(
        validate_lock(&duplicate, manifests.iter().map(Vec::as_slice)),
        Err(ValidationError::Duplicate {
            field: "entries.vertical_id"
        })
    );
}

#[test]
fn lock_cannot_silently_move_the_accepted_contract_revision() {
    let (mut lock, manifests) = support::complete_lock();
    lock.contract_revision = "a".repeat(40);
    assert_eq!(
        validate_lock(&lock, manifests.iter().map(Vec::as_slice)),
        Err(ValidationError::Invalid {
            field: "contract_revision"
        })
    );
}

#[test]
fn lock_authenticates_every_distinct_manifest_and_its_row() {
    let (lock, manifests) = support::complete_lock();

    let mut tampered = manifests.clone();
    tampered[0].push(b'\n');
    assert!(matches!(
        validate_lock(&lock, tampered.iter().map(Vec::as_slice)),
        Err(ValidationError::LengthMismatch {
            field: "entries.manifest"
        })
    ));

    let mut duplicate_digest = lock.clone();
    let first_digest = duplicate_digest.entries[0].manifest.digest.clone();
    duplicate_digest.entries[1]
        .manifest
        .digest
        .clone_from(&first_digest);
    assert_eq!(
        validate_lock(&duplicate_digest, manifests.iter().map(Vec::as_slice)),
        Err(ValidationError::Duplicate {
            field: "entries.manifest.digest"
        })
    );

    let mut swapped_rows = lock.clone();
    let first_manifest = swapped_rows.entries[0].manifest.clone();
    let second_manifest = swapped_rows.entries[1].manifest.clone();
    swapped_rows.entries[0]
        .manifest
        .clone_from(&second_manifest);
    swapped_rows.entries[1].manifest = first_manifest;
    assert!(matches!(
        validate_lock(&swapped_rows, manifests.iter().map(Vec::as_slice)),
        Err(ValidationError::DigestMismatch {
            field: "entries.manifest"
        } | ValidationError::LengthMismatch {
            field: "entries.manifest"
        })
    ));

    let missing = manifests.iter().skip(1).map(Vec::as_slice);
    assert!(matches!(
        validate_lock(&lock, missing),
        Err(ValidationError::MissingVerticals(_))
    ));
}
