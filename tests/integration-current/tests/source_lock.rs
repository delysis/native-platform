use std::collections::BTreeSet;

const ROOT_LOCK: &str = include_str!("../../../Cargo.lock");

const EXACT_PACKAGES: [(&str, &str, &str); 11] = [
    (
        "llama-cpp-2",
        "https://github.com/delysis/llama-cpp-rs",
        "a3cf95eb1d4fa748480eb780e6fcbfc1a5c1c391",
    ),
    (
        "llama-native-types",
        "https://github.com/delysis/llama-native-kit",
        "16168bd76a09f74fdee41d0e2fb0441e79ac1005",
    ),
    (
        "fte-types",
        "https://github.com/delysis/free-token-energy",
        "67814e76659688fef61f311db588d17eddee0a66",
    ),
    (
        "mom-llama-runtime",
        "https://github.com/delysis/mom-llama",
        "3cf57941af6d523378e7fa8b24f5c24c8e50363f",
    ),
    (
        "attachment-native-types",
        "https://github.com/delysis/attachment-native-kit",
        "2a8d3a9a1828162a51185d207822ceb1ba6283a8",
    ),
    (
        "speech-native-types",
        "https://github.com/delysis/speech-native-kit",
        "b836318f10a7e11f433ec3ea8dfa48707adc9b06",
    ),
    (
        "information-native-types",
        "https://github.com/delysis/information-native-kit",
        "7cb255a6f8dda1db7d8e7242f3aa256be06e1bfe",
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

const MOM_TRANSITIVE_BASELINES: [(&str, &str, &str); 2] = [
    (
        "llama-native-types",
        "https://github.com/delysis/llama-native-kit",
        "f7a69316c64d857b99bd847dd44cd852fc5b4ca4",
    ),
    (
        "attachment-native-types",
        "https://github.com/delysis/attachment-native-kit",
        "472900732ded5bcfb5cc639c49b3a4f77feece27",
    ),
];

const ALLOWED_FIRST_PARTY_REVISIONS: [(&str, &str); 11] = [
    (
        "https://github.com/delysis/llama-cpp-rs",
        "a3cf95eb1d4fa748480eb780e6fcbfc1a5c1c391",
    ),
    (
        "https://github.com/delysis/llama-native-kit",
        "16168bd76a09f74fdee41d0e2fb0441e79ac1005",
    ),
    (
        "https://github.com/delysis/llama-native-kit",
        "f7a69316c64d857b99bd847dd44cd852fc5b4ca4",
    ),
    (
        "https://github.com/delysis/free-token-energy",
        "67814e76659688fef61f311db588d17eddee0a66",
    ),
    (
        "https://github.com/delysis/mom-llama",
        "3cf57941af6d523378e7fa8b24f5c24c8e50363f",
    ),
    (
        "https://github.com/delysis/attachment-native-kit",
        "2a8d3a9a1828162a51185d207822ceb1ba6283a8",
    ),
    (
        "https://github.com/delysis/attachment-native-kit",
        "472900732ded5bcfb5cc639c49b3a4f77feece27",
    ),
    (
        "https://github.com/delysis/speech-native-kit",
        "b836318f10a7e11f433ec3ea8dfa48707adc9b06",
    ),
    (
        "https://github.com/delysis/information-native-kit",
        "7cb255a6f8dda1db7d8e7242f3aa256be06e1bfe",
    ),
    (
        "https://github.com/delysis/loom-native",
        "223110bee4be72386d79306b444517371e4a9930",
    ),
    (
        "https://github.com/delysis/w1-platform-contracts",
        "3ed1f3235edb6d481c324f05fe83b2379e3431e6",
    ),
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
    assert_eq!(matched.len(), 9, "expected nine distinct source revisions");
}

#[test]
fn mom_transitive_pre_cutover_pins_remain_visible() {
    let lock: toml::Value = toml::from_str(ROOT_LOCK).expect("root Cargo.lock TOML");
    let packages = lock["package"].as_array().expect("lock packages");
    for (name, repository, revision) in MOM_TRANSITIVE_BASELINES {
        let exact_fragment = format!("?rev={revision}#{revision}");
        let present = packages.iter().any(|package| {
            package["name"].as_str() == Some(name)
                && package["source"].as_str().is_some_and(|source| {
                    source.starts_with(&format!("git+{repository}"))
                        && source.ends_with(&exact_fragment)
                })
        });
        assert!(
            present,
            "missing Mom transitive baseline {name} at {revision}"
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
        actual.insert((repository.to_owned(), revision.to_owned()));
    }

    let expected = ALLOWED_FIRST_PARTY_REVISIONS
        .into_iter()
        .map(|(repository, revision)| (repository.to_owned(), revision.to_owned()))
        .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected, "first-party Git revision set drifted");
}
