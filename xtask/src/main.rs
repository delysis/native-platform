#![forbid(unsafe_code)]

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const GROUPS: [&str; 6] = [
    "portable",
    "platform",
    "product",
    "research",
    "diagnostic",
    "real-hardware",
];

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Ledger {
    schema_version: u64,
    goal: String,
    status: String,
    production_source_imported: bool,
    source_history_imported: bool,
    integration_candidate: IntegrationCandidate,
    accepted_w1_receipt: AcceptedW1Receipt,
    entries: Vec<LedgerEntry>,
    excluded_repository: ExcludedRepository,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct IntegrationCandidate {
    accepted: bool,
    pushed: bool,
    product_releases_modified: bool,
    source_imported: bool,
    history_imported: bool,
    manifest: String,
    coverage: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AcceptedW1Receipt {
    goal: String,
    decision: String,
    accepted_at: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct LedgerEntry {
    source_repository: String,
    source_main: String,
    source_tags: Vec<SourceTag>,
    destination_prefix: Option<String>,
    import_commit: Option<String>,
    path_dependency_cutover_commit: Option<String>,
    old_repo_status: String,
    migration_role: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SourceTag {
    name: String,
    peeled_commit: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ExcludedRepository {
    source_repository: String,
    source_main: String,
    source_tag: String,
    source_tag_peeled_commit: String,
    status: String,
    reason: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct LoomReconciliation {
    schema_version: u64,
    source_repository: String,
    autoresearch_source: String,
    common_ancestor: String,
    quiet_editor_parent: String,
    reconciliation_merge: String,
    phase_one_r4_r5_lineage: String,
    accepted_current_main: String,
    path_classification: LoomPathClassification,
    resolution: String,
    native_platform_history_imported: bool,
    native_platform_source_imported: bool,
    claims_not_established: Vec<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct LoomPathClassification {
    source_changed_paths: u64,
    byte_identical_paths: u64,
    evolved_descendant_paths: u64,
    absent_paths: u64,
}

#[derive(Deserialize)]
struct AdrReceipt {
    algorithm: String,
    files: Vec<AdrFile>,
}

#[derive(Deserialize)]
struct AdrFile {
    path: String,
    length: u64,
    sha256: String,
}

fn main() -> Result<()> {
    let command = env::args().nth(1).unwrap_or_else(|| "policy".to_owned());
    match command.as_str() {
        "policy" => check_policy(&workspace_root()),
        "loom-probe" => run_loom_probe(&workspace_root()),
        _ => bail!("usage: cargo xtask <policy|loom-probe>"),
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask must have a workspace parent")
        .to_owned()
}

fn check_policy(root: &Path) -> Result<()> {
    check_workspace(root)?;
    check_ledger(root)?;
    check_loom_reconciliation(root)?;
    check_evidence_files(root)?;
    check_adrs(root)?;
    println!("native-platform shell policy: pass");
    Ok(())
}

fn check_evidence_files(root: &Path) -> Result<()> {
    check_sha256(
        &root.join("tests/integration-current/graph-boundaries.json"),
        "ab92a68b4596c0797d1464002a6234b53957f6396884cb073c219f290f7805e3",
    )?;
    check_sha256(
        &root.join("migration/exact-head-ci-evidence.json"),
        "b6cea6491114274f5794efc1e628af1fd51a357a9a7f8bcdc1dec5bea6ec753a",
    )?;
    check_sha256(
        &root.join("tests/integration-current/loom-probe/Cargo.toml.in"),
        "6b6b6520169a13ae064667725c2bad6ccea93e8ab6cda890b87fb947df0b7823",
    )?;
    check_sha256(
        &root.join("tests/integration-current/loom-probe/lib.rs.in"),
        "b8e310876e4a00fc3d8125ccf3219165fcde299d51e0c1b368927b6796ea3fa3",
    )?;
    check_sha256(
        &root.join("tests/integration-current/loom-probe/Loom.Cargo.lock"),
        "fcc81ace8ae20b798cb5b362265776c8208eebe05402493baf8bcf67e229f06b",
    )
}

fn check_sha256(path: &Path, expected: &str) -> Result<()> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    ensure!(format!("{:x}", Sha256::digest(bytes)) == expected);
    Ok(())
}

fn run_loom_probe(root: &Path) -> Result<()> {
    check_evidence_files(root)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before Unix epoch")?
        .as_nanos();
    let probe = root.join("target").join(format!(
        "native-platform-loom-probe-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(probe.join("src"))?;
    let fixture = root.join("tests/integration-current/loom-probe");
    fs::copy(fixture.join("Cargo.toml.in"), probe.join("Cargo.toml"))?;
    fs::copy(fixture.join("Loom.Cargo.lock"), probe.join("Cargo.lock"))?;
    fs::copy(fixture.join("lib.rs.in"), probe.join("src/lib.rs"))?;

    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .args(["check", "--locked"])
        .current_dir(&probe)
        .status()
        .context("run isolated Loom graph probe")?;
    let cleanup = fs::remove_dir_all(&probe);
    ensure!(status.success(), "isolated Loom graph probe failed");
    cleanup.with_context(|| format!("remove temporary probe {}", probe.display()))?;
    Ok(())
}

fn check_workspace(root: &Path) -> Result<()> {
    let cargo_text = read_text(&root.join("Cargo.toml"))?;
    let cargo: toml::Value = toml::from_str(&cargo_text).context("parse root Cargo.toml")?;
    ensure!(cargo["workspace"]["resolver"].as_str() == Some("3"));
    ensure!(cargo["workspace"]["package"]["edition"].as_str() == Some("2024"));
    ensure!(cargo["workspace"]["package"]["rust-version"].as_str() == Some("1.92"));
    let groups = cargo["workspace"]["metadata"]["native-platform"]["package-groups"]
        .as_table()
        .context("workspace package-groups table")?;
    ensure!(groups.keys().map(String::as_str).collect::<BTreeSet<_>>() == BTreeSet::from(GROUPS));

    let cargo_manifests = find_named(root, "Cargo.toml")?;
    ensure!(
        cargo_manifests
            == BTreeSet::from([
                root.join("Cargo.toml"),
                root.join("tests/integration-current/Cargo.toml"),
                root.join("xtask/Cargo.toml"),
            ])
    );
    ensure!(find_named(root, "Cargo.lock")? == BTreeSet::from([root.join("Cargo.lock")]));
    ensure!(
        find_named(root, "rust-toolchain.toml")?
            == BTreeSet::from([root.join("rust-toolchain.toml")])
    );
    ensure!(
        find_named(root, "pnpm-workspace.yaml")?
            == BTreeSet::from([root.join("pnpm-workspace.yaml")])
    );
    ensure!(find_named(root, "pnpm-lock.yaml")? == BTreeSet::from([root.join("pnpm-lock.yaml")]));
    let xtask_manifest: toml::Value = toml::from_str(&read_text(&root.join("xtask/Cargo.toml"))?)
        .context("parse xtask Cargo.toml")?;
    ensure!(xtask_manifest.get("workspace").is_none());
    ensure!(
        find_extension(root, "rs")?
            == BTreeSet::from([
                root.join("tests/integration-current/src/lib.rs"),
                root.join("tests/integration-current/tests/lifecycle.rs"),
                root.join("tests/integration-current/tests/source_lock.rs"),
                root.join("tests/integration-current/tests/verticals.rs"),
                root.join("xtask/src/main.rs"),
            ]),
        "repository must contain only diagnostic harness Rust source"
    );
    for forbidden in ["apps", "crates", "packages", "products"] {
        ensure!(
            !root.join(forbidden).exists(),
            "shell must not contain production directory {forbidden}"
        );
    }
    Ok(())
}

fn check_ledger(root: &Path) -> Result<()> {
    let ledger: Ledger = serde_json::from_str(&read_text(&root.join("migration/ledger.json"))?)
        .context("parse migration ledger")?;
    let expected = Ledger {
        schema_version: 1,
        goal: "W2-INTEGRATION".into(),
        status: "integration_candidate_no_imports".into(),
        production_source_imported: false,
        source_history_imported: false,
        integration_candidate: IntegrationCandidate {
            accepted: false,
            pushed: true,
            product_releases_modified: false,
            source_imported: false,
            history_imported: false,
            manifest: "tests/integration-current/Cargo.toml".into(),
            coverage: "tests/integration-current/COVERAGE.md".into(),
        },
        accepted_w1_receipt: AcceptedW1Receipt {
            goal: "W1-VERTICALS".into(),
            decision: "accepted".into(),
            accepted_at: "2026-08-12T17:50:09Z".into(),
        },
        entries: vec![
            expected_entry(
                "delysis/llama-cpp-rs",
                "a3cf95eb1d4fa748480eb780e6fcbfc1a5c1c391",
                &[(
                    "phase1-audit-2026-08-11",
                    "a3cf95eb1d4fa748480eb780e6fcbfc1a5c1c391",
                )],
                None,
                "external_unsafe_upstream_boundary",
            ),
            expected_entry(
                "delysis/llama-native-kit",
                "16168bd76a09f74fdee41d0e2fb0441e79ac1005",
                &[(
                    "phase1-audit-2026-08-11",
                    "7ad18cdd616d97145564ee925235d27e2369319f",
                )],
                Some("crates/native"),
                "first_party_import",
            ),
            expected_entry(
                "delysis/free-token-energy",
                "67814e76659688fef61f311db588d17eddee0a66",
                &[(
                    "phase1-audit-2026-08-11",
                    "c451e23244787fc8de7646a88a4fd4ae10f16f94",
                )],
                Some("products/fte"),
                "first_party_import",
            ),
            expected_entry(
                "delysis/mom-llama",
                "3cf57941af6d523378e7fa8b24f5c24c8e50363f",
                &[(
                    "phase1-audit-2026-08-11",
                    "00895a0762b463ac54b75911c59b7d10181e3a22",
                )],
                Some("products/mom"),
                "first_party_import",
            ),
            expected_entry(
                "delysis/attachment-native-kit",
                "2a8d3a9a1828162a51185d207822ceb1ba6283a8",
                &[(
                    "phase1-audit-2026-08-11",
                    "2a8d3a9a1828162a51185d207822ceb1ba6283a8",
                )],
                Some("crates/services/attachment"),
                "first_party_import",
            ),
            expected_entry(
                "delysis/speech-native-kit",
                "b836318f10a7e11f433ec3ea8dfa48707adc9b06",
                &[(
                    "phase1-audit-2026-08-11",
                    "c78f59fd9ee2c5a27baf89aea96946c6e3a79b97",
                )],
                Some("crates/services/speech"),
                "first_party_import",
            ),
            expected_entry(
                "delysis/information-native-kit",
                "7cb255a6f8dda1db7d8e7242f3aa256be06e1bfe",
                &[(
                    "phase1-audit-2026-08-11",
                    "028c63f5a77f158c56221736ae3cf10c8034edb4",
                )],
                Some("crates/services/information"),
                "first_party_import",
            ),
            expected_entry(
                "delysis/loom-native",
                "223110bee4be72386d79306b444517371e4a9930",
                &[(
                    "phase1-audit-2026-08-11",
                    "41fb51b94f7a8fb2c0ee2aba1b4b3f047754dff4",
                )],
                Some("products/loom"),
                "first_party_import",
            ),
            expected_entry(
                "delysis/w1-platform-contracts",
                "3ed1f3235edb6d481c324f05fe83b2379e3431e6",
                &[
                    (
                        "w1-contracts-v0-2026-08-12-r3",
                        "cbab33555ab9355a6ac453d659c55ec9e0666821",
                    ),
                    (
                        "w1-verticals-v0-2026-08-12",
                        "3ed1f3235edb6d481c324f05fe83b2379e3431e6",
                    ),
                ],
                Some("crates/platform/contracts"),
                "first_party_import",
            ),
        ],
        excluded_repository: ExcludedRepository {
            source_repository: "delysis/fiction-autoresearch-harness".into(),
            source_main: "1a9cf7c1e814b2831a8b358d00565d1b31605017".into(),
            source_tag: "phase1-audit-2026-08-11".into(),
            source_tag_peeled_commit: "1a9cf7c1e814b2831a8b358d00565d1b31605017".into(),
            status: "archived".into(),
            reason: "diagnostic historical input only; excluded from native-platform imports"
                .into(),
        },
    };
    ensure!(ledger == expected, "migration ledger drift");
    Ok(())
}

fn expected_entry(
    repository: &str,
    main: &str,
    tags: &[(&str, &str)],
    destination: Option<&str>,
    role: &str,
) -> LedgerEntry {
    LedgerEntry {
        source_repository: repository.into(),
        source_main: main.into(),
        source_tags: tags
            .iter()
            .map(|(name, peeled_commit)| SourceTag {
                name: (*name).into(),
                peeled_commit: (*peeled_commit).into(),
            })
            .collect(),
        destination_prefix: destination.map(Into::into),
        import_commit: None,
        path_dependency_cutover_commit: None,
        old_repo_status: "active".into(),
        migration_role: role.into(),
    }
}

fn check_loom_reconciliation(root: &Path) -> Result<()> {
    let ledger: LoomReconciliation = serde_json::from_str(&read_text(
        &root.join("migration/loom-ce041-reconciliation.json"),
    )?)
    .context("parse Loom reconciliation ledger")?;
    let expected = LoomReconciliation {
        schema_version: 1,
        source_repository: "delysis/loom-native".into(),
        autoresearch_source: "ce041eb76919f2568c91912b7317eca287a80866".into(),
        common_ancestor: "1e39e05b31d04f70af50721f2225631b68587106".into(),
        quiet_editor_parent: "5c4e0a8ff9be37b448552f9d26e22a35770f5312".into(),
        reconciliation_merge: "72465d22b0cbfd9d914d02e0167d759bb73460b4".into(),
        phase_one_r4_r5_lineage: "d0aca6ff4883ac51514fea5e5fb75ffbb3c8c264".into(),
        accepted_current_main: "223110bee4be72386d79306b444517371e4a9930".into(),
        path_classification: LoomPathClassification {
            source_changed_paths: 130,
            byte_identical_paths: 96,
            evolved_descendant_paths: 34,
            absent_paths: 0,
        },
        resolution: "reconciled_in_source_history".into(),
        native_platform_history_imported: false,
        native_platform_source_imported: false,
        claims_not_established: vec![
            "No source or history has been imported into native-platform.".into(),
            "This ledger does not replay or independently reimplement ce041.".into(),
            "This ledger makes no branch-retirement or remote-deletion claim.".into(),
        ],
    };
    ensure!(ledger == expected, "Loom reconciliation ledger drift");
    Ok(())
}

fn check_adrs(root: &Path) -> Result<()> {
    let directory = root.join("docs/architecture/adr");
    let receipt: AdrReceipt = serde_json::from_str(&read_text(&directory.join("SHA256SUMS.json"))?)
        .context("parse ADR receipt")?;
    ensure!(receipt.algorithm == "sha256");
    ensure!(receipt.files.len() == 15);
    let mut paths = BTreeSet::new();
    for expected in receipt.files {
        ensure!(paths.insert(expected.path.clone()), "duplicate ADR path");
        let bytes = fs::read(directory.join(&expected.path))
            .with_context(|| format!("read ADR {}", expected.path))?;
        ensure!(bytes.len() as u64 == expected.length, "ADR length drift");
        let digest = format!("{:x}", Sha256::digest(&bytes));
        ensure!(digest == expected.sha256, "ADR digest drift");
    }
    Ok(())
}

fn find_named(root: &Path, name: &str) -> Result<BTreeSet<PathBuf>> {
    let mut found = BTreeSet::new();
    visit(root, name, &mut found)?;
    Ok(found)
}

fn find_extension(root: &Path, extension: &str) -> Result<BTreeSet<PathBuf>> {
    let mut found = BTreeSet::new();
    visit_extension(root, extension, &mut found)?;
    Ok(found)
}

fn visit_extension(directory: &Path, extension: &str, found: &mut BTreeSet<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory).with_context(|| format!("read {}", directory.display()))? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            let ignored = entry.file_name() == ".git"
                || entry.file_name() == "target"
                || entry.file_name() == "node_modules";
            if !ignored {
                visit_extension(&path, extension, found)?;
            }
        } else if path.extension().and_then(|value| value.to_str()) == Some(extension) {
            found.insert(path);
        }
    }
    Ok(())
}

fn visit(directory: &Path, name: &str, found: &mut BTreeSet<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory).with_context(|| format!("read {}", directory.display()))? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            let ignored = entry.file_name() == ".git"
                || entry.file_name() == "target"
                || entry.file_name() == "node_modules";
            if !ignored {
                visit(&path, name, found)?;
            }
        } else if entry.file_name() == name {
            found.insert(path);
        }
    }
    Ok(())
}

fn read_text(path: &Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("read {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_shell_satisfies_policy() {
        check_policy(&workspace_root()).expect("workspace shell policy");
    }
}
