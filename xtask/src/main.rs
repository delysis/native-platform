#![forbid(unsafe_code)]

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const GROUPS: [&str; 6] = [
    "portable",
    "platform",
    "product",
    "research",
    "diagnostic",
    "real-hardware",
];

#[derive(Deserialize)]
struct Ledger {
    status: String,
    production_source_imported: bool,
    source_history_imported: bool,
    entries: Vec<LedgerEntry>,
}

#[derive(Deserialize)]
struct LedgerEntry {
    source_repository: String,
    import_commit: Option<String>,
    path_dependency_cutover_commit: Option<String>,
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
    if command != "policy" {
        bail!("usage: cargo xtask policy");
    }
    check_policy(&workspace_root())
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
    check_adrs(root)?;
    println!("native-platform shell policy: pass");
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
        cargo_manifests == BTreeSet::from([root.join("Cargo.toml"), root.join("xtask/Cargo.toml")])
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
        find_extension(root, "rs")? == BTreeSet::from([root.join("xtask/src/main.rs")]),
        "shell must contain no imported Rust source"
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
    ensure!(ledger.status == "shell_no_imports");
    ensure!(!ledger.production_source_imported);
    ensure!(!ledger.source_history_imported);
    ensure!(ledger.entries.len() == 9);
    let repositories = ledger
        .entries
        .iter()
        .map(|entry| entry.source_repository.as_str())
        .collect::<BTreeSet<_>>();
    ensure!(repositories.len() == 9);
    ensure!(repositories.contains("delysis/llama-cpp-rs"));
    ensure!(repositories.contains("delysis/w1-platform-contracts"));
    ensure!(
        ledger
            .entries
            .iter()
            .all(|entry| entry.import_commit.is_none())
    );
    ensure!(
        ledger
            .entries
            .iter()
            .all(|entry| entry.path_dependency_cutover_commit.is_none())
    );
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
