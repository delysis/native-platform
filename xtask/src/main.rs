#![forbid(unsafe_code)]

mod lean;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    println!("native-platform policy: pass");
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
    check_package_groups(groups, &workspace_packages)?;

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

fn check_package_groups(
    groups: PackageGroups,
    workspace_packages: &BTreeSet<String>,
) -> Result<()> {
    ensure!(groups.schema_version == 1);
    ensure!(
        groups
            .primary
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            == BTreeSet::from([
                "native",
                "gateway",
                "service-attachment",
                "service-information",
                "service-speech",
                "product-fte",
                "product-mom",
                "product-loom",
                "diagnostic",
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
        &primary_packages == workspace_packages,
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

    fn package_groups() -> (PackageGroups, BTreeSet<String>) {
        let primary = [
            "native",
            "gateway",
            "service-attachment",
            "service-information",
            "service-speech",
            "product-fte",
            "product-mom",
            "product-loom",
            "diagnostic",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, group)| (group.to_owned(), vec![format!("package-{index}")]))
        .collect::<BTreeMap<_, _>>();
        let secondary = [
            "portable",
            "platform-linux",
            "platform-macos",
            "platform-windows",
            "frontend",
            "fuzz",
            "real-hardware",
            "release",
        ]
        .into_iter()
        .map(|group| (group.to_owned(), Vec::new()))
        .collect::<BTreeMap<_, _>>();
        let packages = primary.values().flatten().cloned().collect();
        (
            PackageGroups {
                schema_version: 1,
                primary,
                secondary,
            },
            packages,
        )
    }

    #[test]
    fn repository_shell_satisfies_policy() {
        check_policy(&workspace_root()).expect("workspace shell policy");
    }

    #[test]
    fn package_groups_must_cover_every_workspace_package_once() {
        let (mut groups, packages) = package_groups();
        groups
            .primary
            .get_mut("native")
            .expect("native group")
            .clear();
        let error = check_package_groups(groups, &packages).expect_err("empty group must fail");
        assert!(error.to_string().contains("empty primary package group"));
    }

    #[test]
    fn secondary_groups_cannot_name_foreign_packages() {
        let (mut groups, packages) = package_groups();
        groups
            .secondary
            .get_mut("portable")
            .expect("portable group")
            .push("not-in-workspace".to_owned());
        let error = check_package_groups(groups, &packages).expect_err("foreign package must fail");
        assert!(
            error
                .to_string()
                .contains("unknown package in secondary group")
        );
    }

    #[test]
    fn first_party_git_dependencies_are_rejected() {
        let manifest = toml::from_str(
            r#"dependency = { git = "https://github.com/delysis/mom-llama", rev = "deadbeef" }"#,
        )
        .expect("test manifest parses");
        let error = check_git_dependencies(&manifest, Path::new("Cargo.toml"))
            .expect_err("first-party Git source must fail");
        assert!(error.to_string().contains("forbidden Git dependency"));
    }

    #[test]
    fn external_ffi_git_dependency_requires_the_exact_revision() {
        let manifest = toml::from_str(
            r#"dependency = { git = "https://github.com/delysis/llama-cpp-rs", rev = "deadbeef" }"#,
        )
        .expect("test manifest parses");
        let error = check_git_dependencies(&manifest, Path::new("Cargo.toml"))
            .expect_err("moving external revision must fail");
        assert!(
            error
                .to_string()
                .contains("unsealed llama-cpp-rs dependency")
        );
    }
}
