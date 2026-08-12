#![forbid(unsafe_code)]
#![cfg(feature = "unstable-w1-vertical-tests")]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use platform_vertical_fixtures_v0::{
    EquivalenceProjectionV0, ObservationEnvelopeV0, VerticalFixtureManifestV0, sha256_identity,
    validate_baseline,
};
use serde::Deserialize;
use serde_json::json;

const BASELINE_COMMIT: &str = "a733508adcb1ef1e689e90ed0c8e410160cb602a";
const PRODUCTION_TREE: &[u8] =
    include_bytes!("../../../fixtures/w1/source/loom-production-tree-a733508.json");

#[derive(Deserialize)]
struct ProductionTreeDescriptor {
    commit: String,
    source_roots: BTreeMap<String, String>,
}

struct VerticalCase {
    manifest: &'static [u8],
    projection: &'static [u8],
    runtime_artifact: &'static [u8],
}

fn frozen_content_blob_bytes() -> &'static [u8] {
    include_bytes!(
        "../../../fixtures/w1/state/loom-prior-v10/.loom/blobs/sha256/83/4c141212ac3cf23062a3864b45cdf630ae8fc8029092807602fffce1b70739"
    )
}

fn fixture_bytes(relative_path: &str) -> &'static [u8] {
    match relative_path {
        "fixtures/w1/loom-suggestion-promotion-v1.json" => {
            include_bytes!("../../../fixtures/w1/loom-suggestion-promotion-v1.json")
        }
        "fixtures/w1/loom-research-authority-v1.json" => {
            include_bytes!("../../../fixtures/w1/loom-research-authority-v1.json")
        }
        "fixtures/w1/loom-prior-store-v10-v1.json" => {
            include_bytes!("../../../fixtures/w1/loom-prior-store-v10-v1.json")
        }
        "fixtures/w1/state/loom-prior-v10/.loom/project.json" => {
            include_bytes!("../../../fixtures/w1/state/loom-prior-v10/.loom/project.json")
        }
        "fixtures/w1/state/loom-prior-v10/manuscript/001.md" => {
            include_bytes!("../../../fixtures/w1/state/loom-prior-v10/manuscript/001.md")
        }
        "fixtures/w1/state/loom-prior-v10/.loom/blobs/sha256/83/4c141212ac3cf23062a3864b45cdf630ae8fc8029092807602fffce1b70739" => {
            frozen_content_blob_bytes()
        }
        "fixtures/w1/state/loom-prior-v10-migrated-summary-v1.json" => {
            include_bytes!("../../../fixtures/w1/state/loom-prior-v10-migrated-summary-v1.json")
        }
        "fixtures/w1/state/loom-prior-v10/.loom/loom.sqlite3" => {
            include_bytes!("../../../fixtures/w1/state/loom-prior-v10/.loom/loom.sqlite3")
        }
        _ => panic!("unmapped W1 fixture artifact: {relative_path}"),
    }
}

fn evidence_bytes(relative_path: &str) -> &'static [u8] {
    match relative_path {
        "gemma-current-input-v0.json" => {
            include_bytes!("../../../fixtures/w1/gemma-current-input-v0.json")
        }
        "gemma-current-manifest-v0.json" => {
            include_bytes!("../../../fixtures/w1/gemma-current-manifest-v0.json")
        }
        "gemma-current-projection-v0.json" => {
            include_bytes!("../../../fixtures/w1/gemma-current-projection-v0.json")
        }
        "gemma-current-source-tree-v0.txt" => {
            include_bytes!("../../../fixtures/w1/gemma-current-source-tree-v0.txt")
        }
        "loom-prior-store-v10-v1.json" => fixture_bytes("fixtures/w1/loom-prior-store-v10-v1.json"),
        "loom-quit-relaunch-v1.json" => {
            include_bytes!("../../../fixtures/w1/loom-quit-relaunch-v1.json")
        }
        "loom-research-authority-v1.json" => {
            fixture_bytes("fixtures/w1/loom-research-authority-v1.json")
        }
        "loom-suggestion-promotion-v1.json" => {
            fixture_bytes("fixtures/w1/loom-suggestion-promotion-v1.json")
        }
        "manifests/loom-prior-project-store-v0.json" => {
            include_bytes!("../../../fixtures/w1/manifests/loom-prior-project-store-v0.json")
        }
        "manifests/loom-quit-relaunch-v0.json" => {
            include_bytes!("../../../fixtures/w1/manifests/loom-quit-relaunch-v0.json")
        }
        "manifests/loom-research-diagnostic-admitted-v0.json" => include_bytes!(
            "../../../fixtures/w1/manifests/loom-research-diagnostic-admitted-v0.json"
        ),
        "manifests/loom-suggestion-promotion-v0.json" => {
            include_bytes!("../../../fixtures/w1/manifests/loom-suggestion-promotion-v0.json")
        }
        "projections/loom-prior-project-store-v10-v1.json" => {
            include_bytes!("../../../fixtures/w1/projections/loom-prior-project-store-v10-v1.json")
        }
        "projections/loom-quit-relaunch-v1.json" => {
            include_bytes!("../../../fixtures/w1/projections/loom-quit-relaunch-v1.json")
        }
        "projections/loom-research-authority-v1.json" => {
            include_bytes!("../../../fixtures/w1/projections/loom-research-authority-v1.json")
        }
        "projections/loom-suggestion-promotion-v1.json" => {
            include_bytes!("../../../fixtures/w1/projections/loom-suggestion-promotion-v1.json")
        }
        "source/loom-production-tree-a733508.json" => PRODUCTION_TREE,
        "source/loom-row8-production-tree-a210de5.json" => {
            include_bytes!("../../../fixtures/w1/source/loom-row8-production-tree-a210de5.json")
        }
        "state/loom-prior-v10-migrated-summary-v1.json" => {
            fixture_bytes("fixtures/w1/state/loom-prior-v10-migrated-summary-v1.json")
        }
        "state/loom-prior-v10/.loom/project.json" => {
            fixture_bytes("fixtures/w1/state/loom-prior-v10/.loom/project.json")
        }
        "state/loom-prior-v10/.loom/loom.sqlite3" => {
            fixture_bytes("fixtures/w1/state/loom-prior-v10/.loom/loom.sqlite3")
        }
        "state/loom-prior-v10/manuscript/001.md" => {
            fixture_bytes("fixtures/w1/state/loom-prior-v10/manuscript/001.md")
        }
        "state/loom-prior-v10/.loom/blobs/sha256/83/4c141212ac3cf23062a3864b45cdf630ae8fc8029092807602fffce1b70739" => {
            fixture_bytes(
                "fixtures/w1/state/loom-prior-v10/.loom/blobs/sha256/83/4c141212ac3cf23062a3864b45cdf630ae8fc8029092807602fffce1b70739",
            )
        }
        _ => panic!("unmapped W1 evidence artifact: {relative_path}"),
    }
}

fn cases() -> [VerticalCase; 3] {
    [
        VerticalCase {
            manifest: include_bytes!(
                "../../../fixtures/w1/manifests/loom-suggestion-promotion-v0.json"
            ),
            projection: include_bytes!(
                "../../../fixtures/w1/projections/loom-suggestion-promotion-v1.json"
            ),
            runtime_artifact: include_bytes!(
                "../../../fixtures/w1/loom-suggestion-promotion-v1.json"
            ),
        },
        VerticalCase {
            manifest: include_bytes!(
                "../../../fixtures/w1/manifests/loom-research-diagnostic-admitted-v0.json"
            ),
            projection: include_bytes!(
                "../../../fixtures/w1/projections/loom-research-authority-v1.json"
            ),
            runtime_artifact: include_bytes!(
                "../../../fixtures/w1/loom-research-authority-v1.json"
            ),
        },
        VerticalCase {
            manifest: include_bytes!(
                "../../../fixtures/w1/manifests/loom-prior-project-store-v0.json"
            ),
            projection: include_bytes!(
                "../../../fixtures/w1/projections/loom-prior-project-store-v10-v1.json"
            ),
            runtime_artifact: include_bytes!(
                "../../../fixtures/w1/state/loom-prior-v10/.loom/loom.sqlite3"
            ),
        },
    ]
}

#[test]
fn w1_sha256_ledger_authenticates_every_checked_in_byte_artifact() {
    let ledger = include_str!("../../../fixtures/w1/MANIFEST.sha256");
    let mut paths = BTreeSet::new();
    for line in ledger.lines() {
        let (expected_digest, relative_path) =
            line.split_once("  ").expect("ledger uses sha256sum format");
        assert!(
            paths.insert(relative_path.to_owned()),
            "duplicate ledger path: {relative_path}"
        );
        let identity = sha256_identity("ledger.artifact", evidence_bytes(relative_path));
        assert_eq!(
            identity.digest.hex, expected_digest,
            "artifact drifted: {relative_path}"
        );
    }
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    let tracked = Command::new("git")
        .args(["ls-files", "fixtures/w1"])
        .current_dir(repository)
        .output()
        .expect("enumerate tracked W1 fixture artifacts");
    assert!(
        tracked.status.success(),
        "enumerate tracked W1 fixture artifacts"
    );
    let tracked_paths = String::from_utf8(tracked.stdout)
        .expect("tracked fixture paths are UTF-8")
        .lines()
        .filter_map(|path| path.strip_prefix("fixtures/w1/"))
        .filter(|path| *path != "MANIFEST.sha256")
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    assert_eq!(paths, tracked_paths, "fixture ledger must be complete");
}

#[test]
fn w1_canonical_manifests_authenticate_inputs_and_validate_product_observations() {
    for vertical in cases() {
        let manifest: VerticalFixtureManifestV0 =
            serde_json::from_slice(vertical.manifest).expect("parse canonical W1 manifest");
        assert_eq!(manifest.cases.len(), 1);
        let case = &manifest.cases[0];
        assert_eq!(case.source.commit, BASELINE_COMMIT);
        assert_eq!(
            sha256_identity(case.source.production_tree.id.clone(), PRODUCTION_TREE),
            case.source.production_tree
        );
        for input in &case.inputs {
            let relative_path = input
                .relative_path
                .as_deref()
                .expect("checked-in input has a relative path");
            assert_eq!(
                sha256_identity(input.identity.id.clone(), fixture_bytes(relative_path)),
                input.identity,
                "fixture identity drifted: {relative_path}"
            );
        }
        for state in &case.state_identities {
            let relative_path = state
                .baseline
                .relative_path
                .as_deref()
                .expect("checked-in state has a relative path");
            assert_eq!(
                sha256_identity(
                    state.baseline.identity.id.clone(),
                    fixture_bytes(relative_path)
                ),
                state.baseline.identity,
                "state identity drifted: {relative_path}"
            );
        }
        assert_eq!(
            sha256_identity(case.expected_projection.id.clone(), vertical.projection),
            case.expected_projection
        );

        // The row-specific unit/integration replays assert the production facts.
        // This adapter serializes precisely those reviewed facts into the closed
        // central envelope; it does not execute a second fixture-owned model.
        let projection: EquivalenceProjectionV0 = serde_json::from_slice(vertical.projection)
            .expect("parse canonical product projection");
        let runtime = sha256_identity("loom.vertical.runtime_artifact", vertical.runtime_artifact);
        let observation: ObservationEnvelopeV0 = serde_json::from_value(json!({
            "schema": "delysis.vertical_observation.v0",
            "vertical_id": manifest.vertical_id,
            "case_id": case.case_id,
            "implementation_revision": BASELINE_COMMIT,
            "observed_prerequisites": [],
            "evidence": {
                "schema": "delysis.evidence_claim.v0",
                "tier": "reproducible",
                "threat_model": "model-free or frozen-state product replay only",
                "exact_source": case.source.production_tree.digest,
                "exact_runtime_or_artifact": runtime.digest,
                "execution_kind": "fixture",
                "omitted_claims": manifest.omitted_claims,
                "negative_evidence": []
            },
            "projection": projection
        }))
        .expect("construct canonical product observation");
        validate_baseline(
            &manifest,
            &case.case_id,
            vertical.projection,
            &[],
            &observation,
        )
        .expect("central protocol accepts exact Loom baseline");
    }
}

#[test]
fn w1_fixture_descendant_preserves_every_bound_production_source_root() {
    let descriptor: ProductionTreeDescriptor =
        serde_json::from_slice(PRODUCTION_TREE).expect("parse production-tree descriptor");
    assert_eq!(descriptor.commit, BASELINE_COMMIT);
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    let ancestry = Command::new("git")
        .args(["merge-base", "--is-ancestor", BASELINE_COMMIT, "HEAD"])
        .current_dir(repository)
        .status()
        .expect("execute git ancestry proof");
    assert!(
        ancestry.success(),
        "fixture commit must descend from baseline"
    );

    for (source_root, expected_oid) in descriptor.source_roots {
        let output = Command::new("git")
            .args(["rev-parse", &format!("HEAD:{source_root}")])
            .current_dir(repository)
            .output()
            .expect("read current source-root identity");
        assert!(
            output.status.success(),
            "missing source root: {source_root}"
        );
        let actual_oid = String::from_utf8(output.stdout)
            .expect("git object id is UTF-8")
            .trim()
            .to_owned();
        if source_root == "crates/tauri-plugin-loom/src" {
            assert_tauri_plugin_delta_is_test_only(repository, &actual_oid, &expected_oid);
        } else {
            assert_eq!(
                actual_oid, expected_oid,
                "production source changed: {source_root}"
            );
        }
    }
}

fn assert_tauri_plugin_delta_is_test_only(repository: &Path, actual_oid: &str, expected_oid: &str) {
    if actual_oid == expected_oid {
        return;
    }
    let baseline = git_blob(
        repository,
        &format!("{BASELINE_COMMIT}:crates/tauri-plugin-loom/src/lib.rs"),
    );
    let current = git_blob(repository, "HEAD:crates/tauri-plugin-loom/src/lib.rs");
    let test_module = concat!(
        "#[cfg(all(test, feature = \"unstable-w1-vertical-tests\"))]\n",
        "mod w1_vertical_fixture;\n",
    );
    assert_eq!(
        current.replacen(test_module, "", 1),
        baseline,
        "tauri-plugin production module changed outside the feature-gated W1 fixture"
    );
    let changed = Command::new("git")
        .args([
            "diff",
            "--name-only",
            BASELINE_COMMIT,
            "HEAD",
            "--",
            "crates/tauri-plugin-loom/src",
        ])
        .current_dir(repository)
        .output()
        .expect("enumerate tauri-plugin source delta");
    assert!(changed.status.success(), "enumerate test-only source delta");
    assert_eq!(
        String::from_utf8(changed.stdout).expect("changed paths are UTF-8"),
        concat!(
            "crates/tauri-plugin-loom/src/lib.rs\n",
            "crates/tauri-plugin-loom/src/w1_vertical_fixture.rs\n",
        ),
        "tauri-plugin source delta must remain confined to the W1 test module"
    );
}

fn git_blob(repository: &Path, revision_path: &str) -> String {
    let output = Command::new("git")
        .args(["show", revision_path])
        .current_dir(repository)
        .output()
        .expect("read source blob");
    assert!(
        output.status.success(),
        "source blob exists: {revision_path}"
    );
    String::from_utf8(output.stdout).expect("source blob is UTF-8")
}
