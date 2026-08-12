use platform_vertical_fixtures_v0::{
    ALL_VERTICAL_IDS, FixtureClassV0, VerticalFixtureManifestV0, validate_manifest,
};
use std::collections::BTreeSet;
use std::path::Path;

const MANIFESTS: [(&str, &[u8]); 18] = [
    (
        "mom-chat-cancel-retry",
        include_bytes!("../../../verticals/v0/mom-chat-cancel-retry.manifest.json"),
    ),
    (
        "mom-attachment",
        include_bytes!("../../../verticals/v0/mom-attachment.manifest.json"),
    ),
    (
        "fte-hosted-fixture-loopback",
        include_bytes!("../../../verticals/v0/fte-hosted-fixture-loopback.manifest.json"),
    ),
    (
        "speech-peer-cancellation",
        include_bytes!("../../../verticals/v0/speech-peer-cancellation.manifest.json"),
    ),
    (
        "information-install-query",
        include_bytes!("../../../verticals/v0/information-install-query.manifest.json"),
    ),
    (
        "loom-suggestion-promotion",
        include_bytes!("../../../verticals/v0/loom-suggestion-promotion.manifest.json"),
    ),
    (
        "loom-research-diagnostic-admitted-distinction",
        include_bytes!(
            "../../../verticals/v0/loom-research-diagnostic-admitted-distinction.manifest.json"
        ),
    ),
    (
        "quit-relaunch-fake-owners",
        include_bytes!("../../../verticals/v0/quit-relaunch-fake-owners.manifest.json"),
    ),
    (
        "current-exact-qwen",
        include_bytes!("../../../verticals/v0/current-exact-qwen.manifest.json"),
    ),
    (
        "current-exact-gemma",
        include_bytes!("../../../verticals/v0/current-exact-gemma.manifest.json"),
    ),
    (
        "current-parakeet-model-audio",
        include_bytes!("../../../verticals/v0/current-parakeet-model-audio.manifest.json"),
    ),
    (
        "apple-installed-voice",
        include_bytes!("../../../verticals/v0/apple-installed-voice.manifest.json"),
    ),
    (
        "mom-prior-release-store",
        include_bytes!("../../../verticals/v0/mom-prior-release-store.manifest.json"),
    ),
    (
        "loom-prior-project-store",
        include_bytes!("../../../verticals/v0/loom-prior-project-store.manifest.json"),
    ),
    (
        "fte-legacy-database",
        include_bytes!("../../../verticals/v0/fte-legacy-database.manifest.json"),
    ),
    (
        "information-resource-store",
        include_bytes!("../../../verticals/v0/information-resource-store.manifest.json"),
    ),
    (
        "corrupted-disposable-caches",
        include_bytes!("../../../verticals/v0/corrupted-disposable-caches.manifest.json"),
    ),
    (
        "partial-publication-states",
        include_bytes!("../../../verticals/v0/partial-publication-states.manifest.json"),
    ),
];

#[test]
fn staged_catalogue_is_complete_ordered_and_unsealed() {
    let lock_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../verticals/v0/W1-VERTICALS.lock.json");
    assert!(!lock_path.exists(), "catalogue must remain unsealed");

    let manifests = MANIFESTS
        .iter()
        .map(|(name, bytes)| {
            let manifest: VerticalFixtureManifestV0 = serde_json::from_slice(bytes)
                .unwrap_or_else(|error| panic!("{name} manifest must parse: {error}"));
            validate_manifest(&manifest)
                .unwrap_or_else(|error| panic!("{name} manifest must validate: {error}"));
            manifest
        })
        .collect::<Vec<_>>();

    let ids = manifests
        .iter()
        .map(|manifest| manifest.vertical_id)
        .collect::<Vec<_>>();
    assert_eq!(ids, ALL_VERTICAL_IDS);
    assert_eq!(ids.iter().copied().collect::<BTreeSet<_>>().len(), 18);
    assert_eq!(
        manifests
            .iter()
            .filter(|manifest| manifest.class == FixtureClassV0::ModelFree)
            .count(),
        8
    );
    assert_eq!(
        manifests
            .iter()
            .filter(|manifest| manifest.class == FixtureClassV0::Real)
            .count(),
        4
    );
    assert_eq!(
        manifests
            .iter()
            .filter(|manifest| manifest.class == FixtureClassV0::State)
            .count(),
        6
    );
    assert!(
        manifests
            .iter()
            .all(|manifest| manifest.class == manifest.vertical_id.class())
    );
}
