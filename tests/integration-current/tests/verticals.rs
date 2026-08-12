use platform_vertical_fixtures_v0_current::{
    ALL_VERTICAL_IDS, VerticalFixtureLockV0, validate_lock,
};
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const LOCK_SHA256: &str = "6b5c6cc5da5d4bc25b670d9e73087d6d313d05df30cb1a1b611861e302fd5599";
const PROTOCOL_COMMIT: &str = "fc24ffff08c52690390b4460f44617d5d9732563";
const CONTRACT_REVISION: &str = "cbab33555ab9355a6ac453d659c55ec9e0666821";

#[test]
fn exact_w1_git_checkout_authenticates_all_verticals() {
    let lock_path = find_exact_lock().expect("exact W1 lock in Cargo Git checkout");
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

fn find_exact_lock() -> Option<PathBuf> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--locked", "--offline"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let package = metadata["packages"].as_array()?.iter().find(|package| {
        package["name"].as_str() == Some("platform-vertical-fixtures-v0")
            && package["source"].as_str().is_some_and(|source| {
                source.ends_with(
                    "?rev=3ed1f3235edb6d481c324f05fe83b2379e3431e6#3ed1f3235edb6d481c324f05fe83b2379e3431e6",
                )
            })
    })?;
    let manifest = PathBuf::from(package["manifest_path"].as_str()?);
    let source_root = manifest.parent()?.parent()?.parent()?;
    let lock_path = source_root.join("verticals/v0/W1-VERTICALS.lock.json");
    let bytes = fs::read(&lock_path).ok()?;
    (format!("{:x}", Sha256::digest(bytes)) == LOCK_SHA256).then_some(lock_path)
}
