use platform_vertical_fixtures_v0::{ALL_VERTICAL_IDS, VerticalFixtureLockV0, validate_lock};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

const LOCK_SHA256: &str = "6b5c6cc5da5d4bc25b670d9e73087d6d313d05df30cb1a1b611861e302fd5599";
const PROTOCOL_COMMIT: &str = "fc24ffff08c52690390b4460f44617d5d9732563";
const CONTRACT_REVISION: &str = "cbab33555ab9355a6ac453d659c55ec9e0666821";

#[test]
fn imported_w1_source_authenticates_all_verticals() {
    let lock_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/platform/contracts/verticals/v0/W1-VERTICALS.lock.json");
    assert!(is_exact_lock(&lock_path), "imported W1 lock digest drifted");
    let lock_bytes = fs::read(&lock_path).expect("read exact W1 lock");
    let lock: VerticalFixtureLockV0 =
        serde_json::from_slice(&lock_bytes).expect("parse exact W1 lock");
    assert_eq!(lock.protocol_commit, PROTOCOL_COMMIT);
    assert_eq!(lock.contract_revision, CONTRACT_REVISION);
    assert_eq!(
        lock.entries
            .iter()
            .map(|entry| entry.vertical_id)
            .collect::<Vec<_>>(),
        ALL_VERTICAL_IDS
    );

    let source_root = lock_path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("lock beneath source root");
    let manifests = lock
        .entries
        .iter()
        .map(|entry| fs::read(source_root.join(&entry.manifest.id)).expect("read locked manifest"))
        .collect::<Vec<_>>();
    validate_lock(&lock, manifests.iter().map(Vec::as_slice))
        .expect("authenticate exact accepted W1 vertical bundle");
}

fn is_exact_lock(path: &std::path::Path) -> bool {
    fs::read(path)
        .ok()
        .is_some_and(|bytes| format!("{:x}", Sha256::digest(bytes)) == LOCK_SHA256)
}
