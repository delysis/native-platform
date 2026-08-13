use platform_vertical_fixtures_v0::{
    ALL_VERTICAL_IDS, FixtureClassV0, VerticalFixtureLockV0, W1_CONTRACT_REVISION, sha256_identity,
    validate_lock,
};
use std::collections::BTreeSet;

const PROTOCOL_COMMIT: &str = "fc24ffff08c52690390b4460f44617d5d9732563";
const LOCK: &[u8] = include_bytes!("../../../verticals/v0/W1-VERTICALS.lock.json");
const MANIFESTS: [&[u8]; 18] = [
    include_bytes!("../../../verticals/v0/mom-chat-cancel-retry.manifest.json"),
    include_bytes!("../../../verticals/v0/mom-attachment.manifest.json"),
    include_bytes!("../../../verticals/v0/fte-hosted-fixture-loopback.manifest.json"),
    include_bytes!("../../../verticals/v0/speech-peer-cancellation.manifest.json"),
    include_bytes!("../../../verticals/v0/information-install-query.manifest.json"),
    include_bytes!("../../../verticals/v0/loom-suggestion-promotion.manifest.json"),
    include_bytes!(
        "../../../verticals/v0/loom-research-diagnostic-admitted-distinction.manifest.json"
    ),
    include_bytes!("../../../verticals/v0/quit-relaunch-fake-owners.manifest.json"),
    include_bytes!("../../../verticals/v0/current-exact-qwen.manifest.json"),
    include_bytes!("../../../verticals/v0/current-exact-gemma.manifest.json"),
    include_bytes!("../../../verticals/v0/current-parakeet-model-audio.manifest.json"),
    include_bytes!("../../../verticals/v0/apple-installed-voice.manifest.json"),
    include_bytes!("../../../verticals/v0/mom-prior-release-store.manifest.json"),
    include_bytes!("../../../verticals/v0/loom-prior-project-store.manifest.json"),
    include_bytes!("../../../verticals/v0/fte-legacy-database.manifest.json"),
    include_bytes!("../../../verticals/v0/information-resource-store.manifest.json"),
    include_bytes!("../../../verticals/v0/corrupted-disposable-caches.manifest.json"),
    include_bytes!("../../../verticals/v0/partial-publication-states.manifest.json"),
];

#[test]
fn sealed_candidate_authenticates_the_complete_catalogue() {
    let lock: VerticalFixtureLockV0 = serde_json::from_slice(LOCK).expect("lock JSON");
    assert_eq!(lock.protocol_commit, PROTOCOL_COMMIT);
    assert_eq!(lock.contract_revision, W1_CONTRACT_REVISION);
    assert_eq!(
        lock.entries
            .iter()
            .map(|entry| entry.vertical_id)
            .collect::<Vec<_>>(),
        ALL_VERTICAL_IDS
    );
    assert_eq!(
        lock.entries
            .iter()
            .map(|entry| entry.class)
            .collect::<Vec<_>>(),
        ALL_VERTICAL_IDS
            .iter()
            .map(|vertical_id| vertical_id.class())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        lock.entries
            .iter()
            .filter(|entry| entry.class == FixtureClassV0::ModelFree)
            .count(),
        8
    );
    assert_eq!(
        lock.entries
            .iter()
            .filter(|entry| entry.class == FixtureClassV0::Real)
            .count(),
        4
    );
    assert_eq!(
        lock.entries
            .iter()
            .filter(|entry| entry.class == FixtureClassV0::State)
            .count(),
        6
    );

    let manifest_digests = lock
        .entries
        .iter()
        .map(|entry| entry.manifest.digest.hex.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(manifest_digests.len(), 18);
    for ((entry, bytes), vertical_id) in lock.entries.iter().zip(MANIFESTS).zip(ALL_VERTICAL_IDS) {
        let computed = sha256_identity(entry.manifest.id.clone(), bytes);
        assert_eq!(entry.vertical_id, vertical_id);
        assert_eq!(entry.manifest, computed);
    }

    validate_lock(&lock, MANIFESTS).expect("complete authenticated W1 seal candidate");
}
