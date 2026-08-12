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
    find_exact_lock_from_metadata().or_else(find_exact_lock_from_cargo_home)
}

fn find_exact_lock_from_metadata() -> Option<PathBuf> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--locked", "--offline"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    // `source` URL normalization is platform-dependent (`.git` may be
    // inserted). The root-lock test separately proves the exact Git revision;
    // this lookup proves the checkout bytes by the accepted lock digest.
    let package = metadata["packages"]
        .as_array()?
        .iter()
        .find(|package| package["name"].as_str() == Some("platform-vertical-fixtures-v0"))?;
    let manifest = PathBuf::from(package["manifest_path"].as_str()?);
    manifest.ancestors().find_map(|ancestor| {
        let lock_path = ancestor.join("verticals/v0/W1-VERTICALS.lock.json");
        is_exact_lock(&lock_path).then_some(lock_path)
    })
}

fn find_exact_lock_from_cargo_home() -> Option<PathBuf> {
    let cargo_home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
        .or_else(|| env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".cargo")))?;
    find_matching_lock(&cargo_home.join("git/checkouts"))
}

fn find_matching_lock(directory: &Path) -> Option<PathBuf> {
    for entry in fs::read_dir(directory).ok()?.flatten() {
        let path = entry.path();
        if entry.file_type().ok()?.is_dir() {
            if let Some(lock) = find_matching_lock(&path) {
                return Some(lock);
            }
        } else if entry.file_name() == "W1-VERTICALS.lock.json" && is_exact_lock(&path) {
            return Some(path);
        }
    }
    None
}

fn is_exact_lock(path: &Path) -> bool {
    fs::read(path)
        .ok()
        .is_some_and(|bytes| format!("{:x}", Sha256::digest(bytes)) == LOCK_SHA256)
}
