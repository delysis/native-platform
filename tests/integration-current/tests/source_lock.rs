use std::collections::BTreeSet;

const ROOT_LOCK: &str = include_str!("../../../Cargo.lock");
const IMPORTED_SERVICE_LOCKS: [&str; 3] = [
    include_str!("../../../crates/services/attachment/Cargo.lock"),
    include_str!("../../../crates/services/information/Cargo.lock"),
    include_str!("../../../crates/services/speech/Cargo.lock"),
];

const EXACT_PACKAGES: [(&str, &str, &str); 6] = [
    (
        "llama-cpp-2",
        "https://github.com/delysis/llama-cpp-rs",
        "a3cf95eb1d4fa748480eb780e6fcbfc1a5c1c391",
    ),
    (
        "mom-llama-runtime",
        "https://github.com/delysis/mom-llama",
        "3cf57941af6d523378e7fa8b24f5c24c8e50363f",
    ),
    (
        "loom-types",
        "https://github.com/delysis/loom-native",
        "223110bee4be72386d79306b444517371e4a9930",
    ),
    (
        "platform-contracts-v0",
        "https://github.com/delysis/w1-platform-contracts",
        "3ed1f3235edb6d481c324f05fe83b2379e3431e6",
    ),
    (
        "platform-contract-testkit",
        "https://github.com/delysis/w1-platform-contracts",
        "3ed1f3235edb6d481c324f05fe83b2379e3431e6",
    ),
    (
        "platform-vertical-fixtures-v0",
        "https://github.com/delysis/w1-platform-contracts",
        "3ed1f3235edb6d481c324f05fe83b2379e3431e6",
    ),
];

const ALLOWED_FIRST_PARTY_REVISIONS: [(&str, &str); 6] = [
    (
        "https://github.com/delysis/llama-cpp-rs",
        "a3cf95eb1d4fa748480eb780e6fcbfc1a5c1c391",
    ),
    (
        "https://github.com/delysis/mom-llama",
        "3cf57941af6d523378e7fa8b24f5c24c8e50363f",
    ),
    (
        "https://github.com/delysis/loom-native",
        "223110bee4be72386d79306b444517371e4a9930",
    ),
    (
        "https://github.com/delysis/w1-platform-contracts",
        "3ed1f3235edb6d481c324f05fe83b2379e3431e6",
    ),
    (
        "https://github.com/delysis/w1-platform-contracts",
        "cbab33555ab9355a6ac453d659c55ec9e0666821",
    ),
    (
        "https://github.com/delysis/w1-platform-contracts",
        "fc24ffff08c52690390b4460f44617d5d9732563",
    ),
];

const IMPORTED_NATIVE_PACKAGES: [&str; 5] = [
    "command-evidence",
    "llama-native-cache",
    "llama-native-engine",
    "llama-native-host",
    "llama-native-types",
];

const IMPORTED_FTE_PACKAGES: [&str; 9] = [
    "free-token-energy",
    "fte-backend-llama",
    "fte-loopback",
    "fte-protocols",
    "fte-providers",
    "fte-router",
    "fte-store",
    "fte-types",
    "tauri-plugin-free-token-energy",
];

const IMPORTED_SERVICE_PACKAGES: [&str; 24] = [
    "attachment-native-cli",
    "attachment-native-document",
    "attachment-native-host",
    "attachment-native-inspect",
    "attachment-native-plan",
    "attachment-native-types",
    "information-native-acquire",
    "information-native-backend-community",
    "information-native-backend-encyclopedia",
    "information-native-backend-scripture",
    "information-native-backend-sqlite",
    "information-native-catalog",
    "information-native-cli",
    "information-native-host",
    "information-native-retrieval",
    "information-native-store",
    "information-native-types",
    "speech-native-backend-parakeet",
    "speech-native-host",
    "speech-native-platform",
    "speech-native-router",
    "speech-native-types",
    "tauri-plugin-information-native",
    "tauri-plugin-speech-native",
];

const IMPORTED_SERVICE_REPOSITORIES: [&str; 3] = [
    "github.com/delysis/attachment-native-kit",
    "github.com/delysis/information-native-kit",
    "github.com/delysis/speech-native-kit",
];

// These binaries/plugins have no outer consumer. Keeping them out of the root
// patch set avoids non-deterministic `[[patch.unused]]` lock records; their
// preserved nested workspace locks still prove that they are local imports.
const NESTED_ONLY_SERVICE_LEAVES: [&str; 4] = [
    "attachment-native-cli",
    "information-native-cli",
    "tauri-plugin-information-native",
    "tauri-plugin-speech-native",
];

#[test]
fn root_lock_contains_every_exact_current_source() {
    let lock: toml::Value = toml::from_str(ROOT_LOCK).expect("root Cargo.lock TOML");
    let packages = lock["package"].as_array().expect("lock packages");
    let mut matched = BTreeSet::new();
    for (name, repository, revision) in EXACT_PACKAGES {
        let exact_fragment = format!("?rev={revision}#{revision}");
        let present = packages.iter().any(|package| {
            package["name"].as_str() == Some(name)
                && package["source"].as_str().is_some_and(|source| {
                    source.starts_with(&format!("git+{repository}"))
                        && source.ends_with(&exact_fragment)
                })
        });
        assert!(present, "missing exact locked package {name} at {revision}");
        matched.insert((repository, revision));
    }
    assert_eq!(matched.len(), 4, "expected four distinct source revisions");
}

#[test]
fn imported_fte_packages_are_path_rebound() {
    let lock: toml::Value = toml::from_str(ROOT_LOCK).expect("root Cargo.lock TOML");
    let packages = lock["package"].as_array().expect("lock packages");
    assert!(packages.iter().all(|package| {
        package
            .get("source")
            .and_then(toml::Value::as_str)
            .is_none_or(|source| !source.contains("github.com/delysis/free-token-energy"))
    }));
    for name in IMPORTED_FTE_PACKAGES {
        let local = packages
            .iter()
            .filter(|package| package["name"].as_str() == Some(name))
            .any(|package| package.get("source").is_none());
        assert!(local, "missing path-rebound FTE package {name}");
    }
}

#[test]
fn imported_native_packages_are_path_rebound() {
    let lock: toml::Value = toml::from_str(ROOT_LOCK).expect("root Cargo.lock TOML");
    let packages = lock["package"].as_array().expect("lock packages");
    assert!(packages.iter().all(|package| {
        package
            .get("source")
            .and_then(toml::Value::as_str)
            .is_none_or(|source| !source.contains("github.com/delysis/llama-native-kit"))
    }));
    for name in IMPORTED_NATIVE_PACKAGES {
        let local = packages
            .iter()
            .filter(|package| package["name"].as_str() == Some(name))
            .any(|package| package.get("source").is_none());
        assert!(local, "missing path-rebound native package {name}");
    }
}

#[test]
fn imported_service_packages_are_path_rebound() {
    let lock: toml::Value = toml::from_str(ROOT_LOCK).expect("root Cargo.lock TOML");
    let packages = lock["package"].as_array().expect("lock packages");

    for repository in IMPORTED_SERVICE_REPOSITORIES {
        assert!(
            packages.iter().all(|package| {
                package
                    .get("source")
                    .and_then(toml::Value::as_str)
                    .is_none_or(|source| !source.contains(repository))
            }),
            "imported service Git source remains in root lock: {repository}"
        );
    }

    let imported_locks = IMPORTED_SERVICE_LOCKS
        .map(|text| toml::from_str::<toml::Value>(text).expect("imported service Cargo.lock TOML"));
    for name in IMPORTED_SERVICE_PACKAGES {
        let root_matches = packages
            .iter()
            .filter(|package| package["name"].as_str() == Some(name))
            .collect::<Vec<_>>();
        if NESTED_ONLY_SERVICE_LEAVES.contains(&name) {
            assert!(
                root_matches.is_empty(),
                "nested-only service leaf unexpectedly entered root lock: {name}"
            );
        } else {
            assert!(
                root_matches
                    .iter()
                    .any(|package| package.get("source").is_none()),
                "missing path-rebound root service package {name}"
            );
            assert!(
                root_matches
                    .iter()
                    .all(|package| package.get("source").is_none()),
                "non-local imported service package in root lock: {name}"
            );
        }

        let present_in_imported_workspace = imported_locks.iter().any(|lock| {
            lock["package"]
                .as_array()
                .expect("imported service lock packages")
                .iter()
                .any(|package| {
                    package["name"].as_str() == Some(name) && package.get("source").is_none()
                })
        });
        assert!(
            present_in_imported_workspace,
            "missing local imported service package {name}"
        );
    }
}

#[test]
fn first_party_git_revisions_are_closed_and_exact() {
    let lock: toml::Value = toml::from_str(ROOT_LOCK).expect("root Cargo.lock TOML");
    let packages = lock["package"].as_array().expect("lock packages");
    let prefix = "git+https://github.com/delysis/";
    let mut actual = BTreeSet::new();

    for source in packages
        .iter()
        .filter_map(|package| package.get("source").and_then(toml::Value::as_str))
        .filter(|source| source.starts_with(prefix))
    {
        let (location, revision) = source
            .split_once('#')
            .unwrap_or_else(|| panic!("first-party Git source lacks locked revision: {source}"));
        let (repository, query) = location
            .strip_prefix("git+")
            .and_then(|location| location.split_once('?'))
            .unwrap_or_else(|| panic!("first-party Git source lacks exact rev query: {source}"));
        assert_eq!(
            query,
            format!("rev={revision}"),
            "first-party Git query and locked revision differ"
        );
        actual.insert((
            repository.trim_end_matches(".git").to_owned(),
            revision.to_owned(),
        ));
    }

    let expected = ALLOWED_FIRST_PARTY_REVISIONS
        .into_iter()
        .map(|(repository, revision)| (repository.to_owned(), revision.to_owned()))
        .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected, "first-party Git revision set drifted");
}
