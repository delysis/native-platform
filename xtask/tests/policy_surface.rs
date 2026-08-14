#![forbid(unsafe_code)]

const POLICY_SOURCE: &str = include_str!("../src/main.rs");

#[test]
fn ordinary_policy_has_only_live_repository_inputs() {
    let policy_body = POLICY_SOURCE
        .split_once("fn check_policy")
        .expect("policy function exists")
        .1
        .split_once("fn check_workspace")
        .expect("workspace function follows policy")
        .0;
    assert!(policy_body.contains("check_workspace(root)?"));
    assert!(!policy_body.contains("lean::run"));

    for archival_input in [
        "migration/ledger.json",
        "migration/seal-manifest.json",
        "loom-ce041-reconciliation.json",
        "SHA256SUMS.json",
    ] {
        assert!(
            !POLICY_SOURCE.contains(archival_input),
            "ordinary policy must not read archival input {archival_input}"
        );
    }
}
