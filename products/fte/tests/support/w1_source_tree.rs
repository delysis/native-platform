use platform_vertical_fixtures_v0::sha256_identity;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

pub const BYTES: &[u8] =
    include_bytes!("../fixtures/w1/v0/fte-production-tree-7975000.json");

const IMPORTED_BASELINE_COMMIT: &str = "c46631a929ded6f4c58aff5ba340c2d44da4fca5";

#[derive(Deserialize)]
struct Descriptor {
    schema: String,
    repository_id: String,
    commit: String,
    prefixes: Vec<Prefix>,
    git_trees: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Prefix {
    path: String,
    boundary: String,
    sha256: String,
    byte_len: u64,
}

fn git_output(repository: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repository)
        .output()
        .expect("execute git source-identity check");
    assert!(
        output.status.success(),
        "git source-identity check failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn production_prefix(bytes: &[u8]) -> &[u8] {
    let boundary = b"\n#[cfg(test)]";
    let position = bytes
        .windows(boundary.len())
        .position(|window| window == boundary)
        .expect("production/test source boundary");
    &bytes[..position + 1]
}

fn imported_layout(repository: &Path) -> bool {
    !String::from_utf8_lossy(&git_output(repository, &["rev-parse", "--show-prefix"]))
        .trim()
        .is_empty()
}

fn revision_path(imported: bool, revision: &str, path: &str) -> String {
    if imported {
        format!("{revision}:products/fte/{path}")
    } else {
        format!("{revision}:{path}")
    }
}

pub fn verify() {
    let descriptor: Descriptor = serde_json::from_slice(BYTES).expect("source-tree descriptor");
    assert_eq!(descriptor.schema, "delysis.production_source_roots.v0");
    assert_eq!(descriptor.repository_id, "delysis/free-token-energy");
    assert_eq!(
        descriptor.commit,
        "797500060047ccd10f9810fb4d5c8f374e00eb08"
    );

    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    let imported = imported_layout(repository);
    let baseline_commit = if imported {
        IMPORTED_BASELINE_COMMIT
    } else {
        descriptor.commit.as_str()
    };
    let ancestry = Command::new("git")
        .args(["merge-base", "--is-ancestor", baseline_commit, "HEAD"])
        .current_dir(repository)
        .status()
        .expect("execute source ancestry check");
    assert!(ancestry.success(), "fixture revision must descend from baseline");

    for prefix in descriptor.prefixes {
        assert_eq!(prefix.boundary, "first_cfg_test");
        let current = std::fs::read(repository.join(&prefix.path)).expect("read current source");
        let baseline_spec = revision_path(imported, baseline_commit, &prefix.path);
        let baseline = git_output(repository, &["show", &baseline_spec]);
        for bytes in [production_prefix(&current), production_prefix(&baseline)] {
            let identity = sha256_identity("production.prefix", bytes);
            assert_eq!(identity.digest.hex, prefix.sha256, "source drift: {}", prefix.path);
            assert_eq!(identity.length, prefix.byte_len, "source length drift: {}", prefix.path);
        }
    }

    for (path, expected_oid) in descriptor.git_trees {
        let dirty = git_output(repository, &["status", "--porcelain", "--", &path]);
        assert!(dirty.is_empty(), "working tree drift: {path}");
        for revision in [baseline_commit, "HEAD"] {
            let spec = revision_path(imported, revision, &path);
            let actual = String::from_utf8(git_output(repository, &["rev-parse", &spec]))
                .expect("git object ID is UTF-8");
            assert_eq!(actual.trim(), expected_oid, "production tree drift: {path}");
        }
    }
}
