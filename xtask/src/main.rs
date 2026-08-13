#![forbid(unsafe_code)]

mod lean;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SealManifest {
    schema_version: u64,
    algorithm: String,
    entries: Vec<SealEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SealEntry {
    path: String,
    length: u64,
    sha256: String,
    purpose: String,
    source: Option<SealSource>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SealSource {
    repository: String,
    revision: String,
    tag: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageGroups {
    schema_version: u64,
    primary: BTreeMap<String, Vec<String>>,
    secondary: BTreeMap<String, Vec<String>>,
}

fn main() -> Result<()> {
    let mut arguments = env::args().skip(1);
    let command = arguments.next().unwrap_or_else(|| "policy".to_owned());
    match command.as_str() {
        "policy" => check_policy(&workspace_root()),
        "lean" => lean::run(&workspace_root(), arguments.collect()),
        _ => bail!("usage: cargo xtask <policy|lean>"),
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
    lean::run(root, vec!["verify".to_owned()])?;
    check_ledger(root)?;
    check_loom_reconciliation(root)?;
    check_evidence_files(root)?;
    check_adrs(root)?;
    println!("native-platform migration policy: pass");
    Ok(())
}

fn check_evidence_files(root: &Path) -> Result<()> {
    let manifest: SealManifest =
        serde_json::from_str(&read_text(&root.join("migration/seal-manifest.json"))?)
            .context("parse migration seal manifest")?;
    ensure!(manifest.schema_version == 1);
    ensure!(manifest.algorithm == "sha256");
    ensure!(!manifest.entries.is_empty());

    let mut paths = BTreeSet::new();
    for entry in manifest.entries {
        ensure!(!entry.purpose.trim().is_empty(), "empty seal purpose");
        ensure!(
            entry.sha256.len() == 64
                && entry
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
        let relative = Path::new(&entry.path);
        ensure!(
            !relative.is_absolute()
                && relative
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_))),
            "unconfined seal path: {}",
            entry.path
        );
        ensure!(paths.insert(entry.path.clone()), "duplicate seal path");
        let bytes = fs::read(root.join(relative))
            .with_context(|| format!("read sealed evidence {}", entry.path))?;
        ensure!(
            bytes.len() as u64 == entry.length,
            "sealed length drift: {}",
            entry.path
        );
        ensure!(
            format!("{:x}", Sha256::digest(&bytes)) == entry.sha256,
            "sealed digest drift: {}",
            entry.path
        );
        if let Some(source) = entry.source {
            ensure!(source.repository.starts_with("delysis/"));
            ensure!(
                source.revision.len() == 40
                    && source
                        .revision
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            );
            if let Some(tag) = source.tag {
                ensure!(!tag.trim().is_empty());
            }
        }
    }
    Ok(())
}

fn check_workspace(root: &Path) -> Result<()> {
    let cargo_text = read_text(&root.join("Cargo.toml"))?;
    let cargo: toml::Value = toml::from_str(&cargo_text).context("parse root Cargo.toml")?;
    ensure!(cargo["workspace"]["resolver"].as_str() == Some("3"));
    ensure!(cargo["workspace"]["package"]["edition"].as_str() == Some("2024"));
    ensure!(cargo["workspace"]["package"]["rust-version"].as_str() == Some("1.92"));
    ensure!(
        cargo["workspace"]["exclude"]
            .as_array()
            .is_some_and(|exclude| {
                exclude.len() == 1 && exclude[0].as_str() == Some("crates/services/attachment/fuzz")
            }),
        "only the Attachment fuzz workspace may be excluded"
    );

    let output = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["metadata", "--locked", "--no-deps", "--format-version", "1"])
        .current_dir(root)
        .output()
        .context("run cargo metadata")?;
    ensure!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("parse cargo metadata")?;
    let member_ids = metadata["workspace_members"]
        .as_array()
        .context("metadata workspace members")?
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect::<BTreeSet<_>>();
    let workspace_packages = metadata["packages"]
        .as_array()
        .context("metadata packages")?
        .iter()
        .filter(|package| {
            package["id"]
                .as_str()
                .is_some_and(|id| member_ids.contains(id))
        })
        .map(|package| {
            package["name"]
                .as_str()
                .context("metadata package name")
                .map(str::to_owned)
        })
        .collect::<Result<BTreeSet<_>>>()?;

    let groups: PackageGroups =
        serde_json::from_str(&read_text(&root.join("ci/package-groups.json"))?)
            .context("parse package groups")?;
    ensure!(groups.schema_version == 1);
    ensure!(
        groups
            .primary
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            == BTreeSet::from([
                "core",
                "native",
                "gateway",
                "service-attachment",
                "service-information",
                "service-speech",
                "product-fte",
                "product-mom",
                "product-loom",
                "diagnostic",
                "testkit",
            ])
    );
    ensure!(
        groups
            .secondary
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            == BTreeSet::from([
                "portable",
                "platform-linux",
                "platform-macos",
                "platform-windows",
                "frontend",
                "fuzz",
                "real-hardware",
                "release",
            ])
    );
    let mut primary_packages = BTreeSet::new();
    for (group, packages) in groups.primary {
        ensure!(!packages.is_empty(), "empty primary package group: {group}");
        for package in packages {
            ensure!(
                primary_packages.insert(package),
                "duplicate primary package"
            );
        }
    }
    ensure!(
        primary_packages == workspace_packages,
        "primary package coverage drift"
    );
    for (group, packages) in groups.secondary {
        let mut seen = BTreeSet::new();
        for package in packages {
            ensure!(
                workspace_packages.contains(&package),
                "unknown package in secondary group {group}: {package}"
            );
            ensure!(
                seen.insert(package),
                "duplicate package in secondary group {group}"
            );
        }
    }

    let cargo_manifests = find_named(root, "Cargo.toml")?;
    let workspace_roots = cargo_manifests
        .iter()
        .filter_map(|path| {
            toml::from_str::<toml::Value>(&read_text(path).ok()?)
                .ok()?
                .get("workspace")
                .map(|_| path.clone())
        })
        .collect::<BTreeSet<_>>();
    ensure!(
        workspace_roots
            == BTreeSet::from([
                root.join("Cargo.toml"),
                root.join("crates/services/attachment/fuzz/Cargo.toml"),
            ]),
        "unknown nested Cargo workspace"
    );
    ensure!(
        find_named(root, "Cargo.lock")?
            == BTreeSet::from([
                root.join("Cargo.lock"),
                root.join("crates/services/attachment/fuzz/Cargo.lock"),
            ]),
        "unknown nested Cargo lock"
    );
    ensure!(
        find_named(root, "pnpm-workspace.yaml")?
            == BTreeSet::from([root.join("pnpm-workspace.yaml")])
    );
    ensure!(find_named(root, "pnpm-lock.yaml")? == BTreeSet::from([root.join("pnpm-lock.yaml")]));

    for manifest in cargo_manifests {
        let value: toml::Value = toml::from_str(&read_text(&manifest)?)
            .with_context(|| format!("parse {}", manifest.display()))?;
        check_git_dependencies(&value, &manifest)?;
    }
    let package: serde_json::Value = serde_json::from_str(&read_text(&root.join("package.json"))?)
        .context("parse root package.json")?;
    ensure!(package["packageManager"].as_str() == Some("pnpm@11.16.0"));
    let pnpm_workspace = read_text(&root.join("pnpm-workspace.yaml"))?;
    for package_glob in ["products/fte/**", "products/loom/**", "products/mom/**"] {
        ensure!(
            pnpm_workspace.contains(package_glob),
            "missing pnpm package glob: {package_glob}"
        );
    }
    for workflow in find_extension(root, "yml")?
        .into_iter()
        .chain(find_extension(root, "yaml")?)
        .filter(|path| {
            path.components()
                .any(|component| component.as_os_str() == "workflows")
        })
    {
        ensure!(
            workflow.starts_with(root.join(".github/workflows")),
            "nested active workflow: {}",
            workflow.display()
        );
    }
    Ok(())
}

fn check_git_dependencies(value: &toml::Value, manifest: &Path) -> Result<()> {
    match value {
        toml::Value::Array(values) => {
            for value in values {
                check_git_dependencies(value, manifest)?;
            }
        }
        toml::Value::Table(table) => {
            if let Some(repository) = table.get("git").and_then(toml::Value::as_str) {
                ensure!(
                    repository.trim_end_matches(".git")
                        == "https://github.com/delysis/llama-cpp-rs",
                    "forbidden Git dependency in {}: {repository}",
                    manifest.display()
                );
                ensure!(
                    table.get("rev").and_then(toml::Value::as_str)
                        == Some("a3cf95eb1d4fa748480eb780e6fcbfc1a5c1c391"),
                    "unsealed llama-cpp-rs dependency: {}",
                    manifest.display()
                );
            }
            for value in table.values() {
                check_git_dependencies(value, manifest)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn check_ledger(root: &Path) -> Result<()> {
    let ledger: Ledger = serde_json::from_str(&read_text(&root.join("migration/ledger.json"))?)
        .context("parse migration ledger")?;
    let mut expected = Ledger {
        schema_version: 1,
        goal: "W8-ONE-WORKSPACE".into(),
        status: "local_candidate".into(),
        production_source_imported: true,
        source_history_imported: true,
        integration_candidate: IntegrationCandidate {
            accepted: true,
            pushed: true,
            product_releases_modified: false,
            source_imported: true,
            history_imported: true,
            manifest: "tests/vertical/Cargo.toml".into(),
            coverage: "tests/vertical/COVERAGE.md".into(),
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
    let native = expected
        .entries
        .iter_mut()
        .find(|entry| entry.source_repository == "delysis/llama-native-kit")
        .context("expected native migration entry")?;
    native.import_commit = Some("152a0dda9ba0d1096022d11ddbd08489f524ab31".into());
    native.path_dependency_cutover_commit = Some("c35c6b2d42f60939f3a3478212743c9c82f28b80".into());
    native.source_tags.push(SourceTag {
        name: "native-platform-v2-horizon-b-2026-08-12".into(),
        peeled_commit: "16168bd76a09f74fdee41d0e2fb0441e79ac1005".into(),
    });
    native.old_repo_status = "frozen_unarchived_two_release_retirement".into();
    let gateway = expected
        .entries
        .iter_mut()
        .find(|entry| entry.source_repository == "delysis/free-token-energy")
        .context("expected gateway migration entry")?;
    gateway.import_commit = Some("8e5c9282314bc85140ac1c7f0421caaed2dc3e93".into());
    gateway.path_dependency_cutover_commit =
        Some("a76a13066936e219ca10ecc5fc0080395b725fcc".into());
    gateway.source_tags.push(SourceTag {
        name: "native-platform-v2-horizon-b-2026-08-12".into(),
        peeled_commit: "67814e76659688fef61f311db588d17eddee0a66".into(),
    });
    gateway.old_repo_status = "frozen_unarchived_two_release_retirement".into();
    let mom = expected
        .entries
        .iter_mut()
        .find(|entry| entry.source_repository == "delysis/mom-llama")
        .context("expected Mom migration entry")?;
    mom.import_commit = Some("cfa2d3c40e74e1d692c0cdb9354cc272249fd4ab".into());
    mom.path_dependency_cutover_commit = Some("5b12072e91dc44f2f93f6dfc0b869d3cc58c26f1".into());
    mom.source_tags.push(SourceTag {
        name: "native-platform-v2-horizon-b-2026-08-12".into(),
        peeled_commit: "3cf57941af6d523378e7fa8b24f5c24c8e50363f".into(),
    });
    mom.old_repo_status = "frozen_unarchived_two_release_retirement".into();
    let loom = expected
        .entries
        .iter_mut()
        .find(|entry| entry.source_repository == "delysis/loom-native")
        .context("expected Loom migration entry")?;
    loom.import_commit = Some("19147c74bbe6335331f3fdad256663906c122dc3".into());
    loom.path_dependency_cutover_commit = Some("6cf468d277a88f085242bdaef017305e1148efda".into());
    loom.source_tags.push(SourceTag {
        name: "native-platform-v2-horizon-b-2026-08-12".into(),
        peeled_commit: "223110bee4be72386d79306b444517371e4a9930".into(),
    });
    loom.old_repo_status = "frozen_unarchived_two_release_retirement".into();
    for (repository, import_commit) in [
        (
            "delysis/attachment-native-kit",
            "5e82ed646bad0f57480f809cedf0cc2745b39dc6",
        ),
        (
            "delysis/information-native-kit",
            "b73feb2649c2096505f6489023acf325117c267c",
        ),
        (
            "delysis/speech-native-kit",
            "4a45947508cf33fb0f8043e0507f2dda86d5d75c",
        ),
    ] {
        let service = expected
            .entries
            .iter_mut()
            .find(|entry| entry.source_repository == repository)
            .with_context(|| format!("expected service migration entry {repository}"))?;
        service.import_commit = Some(import_commit.into());
        service.path_dependency_cutover_commit =
            Some("a73df93428334dcfd5b302b598e7b9d7be1539ab".into());
        service.source_tags.push(SourceTag {
            name: "native-platform-v2-horizon-b-2026-08-12".into(),
            peeled_commit: service.source_main.clone(),
        });
        service.old_repo_status = "frozen_unarchived_two_release_retirement".into();
    }
    let contracts = expected
        .entries
        .iter_mut()
        .find(|entry| entry.source_repository == "delysis/w1-platform-contracts")
        .context("expected W1 contract migration entry")?;
    contracts.import_commit = Some("018aa483dbe34ecb3a62f70adc6bfebe99684acc".into());
    contracts.path_dependency_cutover_commit =
        Some("1c79381f9111dfd2d266291db243c7a5091a7fe4".into());
    contracts.old_repo_status = "frozen_unarchived_two_release_retirement".into();
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
        native_platform_history_imported: true,
        native_platform_source_imported: true,
        claims_not_established: vec![
            "This ledger does not replay or independently reimplement ce041.".into(),
            "The source repository is not claimed frozen until the protected W7 tag exists.".into(),
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
