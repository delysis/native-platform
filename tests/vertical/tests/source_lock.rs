use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

const ROOT_LOCK: &str = include_str!("../../../Cargo.lock");
const PACKAGE_GROUPS: &str = include_str!("../../../ci/package-groups.json");
const LLAMA_CPP_REPOSITORY: &str = "https://github.com/delysis/llama-cpp-rs";
const LLAMA_CPP_REVISION: &str = "a3cf95eb1d4fa748480eb780e6fcbfc1a5c1c391";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageGroups {
    schema_version: u64,
    primary: BTreeMap<String, Vec<String>>,
    secondary: BTreeMap<String, Vec<String>>,
}

fn lock_packages() -> Vec<toml::Value> {
    let lock: toml::Value = toml::from_str(ROOT_LOCK).expect("root Cargo.lock TOML");
    lock["package"]
        .as_array()
        .expect("lock packages")
        .to_owned()
}

#[test]
fn every_primary_package_is_local_in_the_root_lock() {
    let groups: PackageGroups = serde_json::from_str(PACKAGE_GROUPS).expect("package group JSON");
    assert_eq!(groups.schema_version, 1);
    assert!(!groups.secondary.is_empty());

    let packages = lock_packages();
    let mut seen = BTreeSet::new();
    for (group, names) in groups.primary {
        assert!(!names.is_empty(), "empty primary package group: {group}");
        for name in names {
            assert!(
                seen.insert(name.clone()),
                "duplicate primary package: {name}"
            );
            let matches = packages
                .iter()
                .filter(|package| package["name"].as_str() == Some(name.as_str()))
                .collect::<Vec<_>>();
            assert!(!matches.is_empty(), "package absent from root lock: {name}");
            assert!(
                matches
                    .iter()
                    .any(|package| package.get("source").is_none()),
                "first-party package is not path-local: {name}"
            );
        }
    }
}

#[test]
fn llama_cpp_is_the_only_delysis_git_boundary_and_is_exact() {
    let packages = lock_packages();
    let prefix = "git+https://github.com/delysis/";
    let mut actual = BTreeSet::new();

    for source in packages
        .iter()
        .filter_map(|package| package.get("source").and_then(toml::Value::as_str))
        .filter(|source| source.starts_with(prefix))
    {
        let (location, revision) = source
            .split_once('#')
            .unwrap_or_else(|| panic!("Delysis Git source lacks locked revision: {source}"));
        let (repository, query) = location
            .strip_prefix("git+")
            .and_then(|location| location.split_once('?'))
            .unwrap_or_else(|| panic!("Delysis Git source lacks exact rev query: {source}"));
        assert_eq!(query, format!("rev={revision}"));
        actual.insert((
            repository.trim_end_matches(".git").to_owned(),
            revision.to_owned(),
        ));
    }

    assert_eq!(
        actual,
        BTreeSet::from([(
            LLAMA_CPP_REPOSITORY.to_owned(),
            LLAMA_CPP_REVISION.to_owned(),
        )])
    );
}
