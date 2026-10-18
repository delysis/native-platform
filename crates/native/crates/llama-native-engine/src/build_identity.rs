#![forbid(unsafe_code)]

use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;

const PRIVATE_FIELD_DOMAIN: &[u8] = b"llama-native-private-build-field-v1\0";
const PRIVATE_MANIFEST_DOMAIN: &[u8] = b"llama-native-private-build-manifest-v4\0";
pub(crate) const BINDING_EVIDENCE_FORMAT: &str = "llama.native-build-evidence.v1";
const CARGO_CLOSURE_DOMAIN: &[u8] = b"llama-native-cargo-closure-v1\0";
const REVIEWED_STATIC_LINKAGE: &str =
    "local=static;ggml-origin=vendored;ggml=static;backends=linked";
const BASE_REQUIRED_ARTIFACT_GROUPS: &[&[&str]] = &[
    &["common/common/link", "common/llama-common/link"],
    &["common/llama_cpp_sys_2_common_wrapper/link"],
    &["ggml/ggml-base/link"],
    &["ggml/ggml-cpu/link"],
    &["ggml/ggml/link"],
    &["llama/llama/link"],
    &["mtmd/mtmd/link"],
];
const METAL_REQUIRED_ARTIFACT_GROUPS: &[&[&str]] = &[
    &["common/common/link", "common/llama-common/link"],
    &["common/llama_cpp_sys_2_common_wrapper/link"],
    &["ggml/ggml-base/link"],
    &["ggml/ggml-cpu/link"],
    &["ggml/ggml-metal/link"],
    &["ggml/ggml/link"],
    &["llama/llama/link"],
    &["mtmd/mtmd/link"],
];

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct CargoPackageKey {
    name: String,
    version: String,
    source: Option<String>,
}

pub(crate) fn reviewed_binding_profile(
    target: &str,
) -> (&'static [&'static str], &'static [&'static [&'static str]]) {
    if target == "aarch64-apple-darwin" {
        (
            &["common", "default", "metal", "mtmd"],
            METAL_REQUIRED_ARTIFACT_GROUPS,
        )
    } else {
        (
            &["common", "default", "mtmd"],
            BASE_REQUIRED_ARTIFACT_GROUPS,
        )
    }
}

pub(crate) fn validate_reviewed_cmake_cache(cache: &[u8]) -> Result<&str, String> {
    let cache =
        std::str::from_utf8(cache).map_err(|_| "llama.cpp CMake cache is not UTF-8".to_string())?;
    for (key, expected_type, expected_value) in [
        ("BUILD_SHARED_LIBS", "BOOL", "OFF"),
        ("GGML_BACKEND_DL", "BOOL", "OFF"),
        ("GGML_CPU_ALL_VARIANTS", "BOOL", "OFF"),
        ("LLAMA_USE_SYSTEM_GGML", "BOOL", "OFF"),
    ] {
        if unique_cmake_cache_value(cache, key, expected_type)? != Some(expected_value) {
            return Err(
                "effective llama.cpp linkage is dynamic, external, or otherwise unreviewed"
                    .to_string(),
            );
        }
    }
    match unique_cmake_cache_value(cache, "CMAKE_GENERATOR", "INTERNAL")? {
        Some(
            generator @ ("Ninja"
            | "Unix Makefiles"
            | "Visual Studio 17 2022"
            | "Visual Studio 18 2026"),
        ) => Ok(generator),
        _ => Err("effective llama.cpp CMake generator is not reviewed".to_string()),
    }
}

fn unique_cmake_cache_value<'a>(
    cache: &'a str,
    key: &str,
    expected_type: &str,
) -> Result<Option<&'a str>, String> {
    let mut found = None;
    for line in cache.lines() {
        let Some((left, value)) = line.split_once('=') else {
            continue;
        };
        let Some((candidate, value_type)) = left.split_once(':') else {
            continue;
        };
        if candidate != key {
            continue;
        }
        if value_type != expected_type || found.replace(value).is_some() {
            return Err("effective llama.cpp CMake cache is ambiguous or malformed".to_string());
        }
    }
    Ok(found)
}

/// Hash-only compiler input accumulator.
///
/// Values may contain local paths, tool output, or credentials accidentally
/// placed in build flags. They are never serialized. Each value is first
/// domain-separated from every other field and only its digest enters the
/// private manifest digest embedded by `build.rs`.
pub(crate) struct PrivateBuildIdentity {
    manifest: Sha256,
}

impl PrivateBuildIdentity {
    pub(crate) fn new() -> Self {
        let mut manifest = Sha256::new();
        manifest.update(PRIVATE_MANIFEST_DOMAIN);
        Self { manifest }
    }

    pub(crate) fn add_bytes(&mut self, field: &str, value: &[u8]) -> Result<(), String> {
        validate_field(field)?;
        let mut field_hash = Sha256::new();
        field_hash.update(PRIVATE_FIELD_DOMAIN);
        hash_frame(&mut field_hash, field.as_bytes());
        field_hash.update([1]);
        hash_frame(&mut field_hash, value);
        let digest = field_hash.finalize();
        self.add_field_digest(field, &digest);
        Ok(())
    }

    pub(crate) fn add_optional_os(
        &mut self,
        field: &str,
        value: Option<&OsStr>,
    ) -> Result<(), String> {
        validate_field(field)?;
        let mut field_hash = Sha256::new();
        field_hash.update(PRIVATE_FIELD_DOMAIN);
        hash_frame(&mut field_hash, field.as_bytes());
        match value {
            Some(value) => {
                field_hash.update([1]);
                hash_frame(&mut field_hash, value.as_encoded_bytes());
            }
            None => field_hash.update([0]),
        }
        let digest = field_hash.finalize();
        self.add_field_digest(field, &digest);
        Ok(())
    }

    pub(crate) fn finish_hex(self) -> String {
        format!("{:x}", self.manifest.finalize())
    }

    fn add_field_digest(&mut self, field: &str, digest: &[u8]) {
        hash_frame(&mut self.manifest, field.as_bytes());
        hash_frame(&mut self.manifest, digest);
    }
}

pub(crate) fn rustc_digest_directive(digest: &str) -> Result<String, String> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("private build identity did not produce a SHA-256 digest".to_string());
    }
    Ok(format!(
        "cargo:rustc-env=LLAMA_NATIVE_BUILD_MANIFEST_SHA256={digest}"
    ))
}

pub(crate) fn validate_binding_build_evidence(
    format: &str,
    features: &str,
    linkage: &str,
    artifacts: &[String],
    reported_sha256: &str,
    expected_features: &[&str],
    required_artifact_groups: &[&[&str]],
) -> Result<String, String> {
    if format != BINDING_EVIDENCE_FORMAT || linkage != REVIEWED_STATIC_LINKAGE {
        return Err("binding build evidence uses an unsupported format or linkage".to_string());
    }
    if !is_lower_sha256(reported_sha256) {
        return Err("binding build evidence digest is malformed".to_string());
    }

    let parsed_features = features.split(',').collect::<Vec<_>>();
    if parsed_features != expected_features
        || parsed_features.is_empty()
        || parsed_features.iter().any(|feature| {
            feature.is_empty()
                || !feature
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
        || !parsed_features.windows(2).all(|pair| pair[0] < pair[1])
    {
        return Err("binding build evidence feature set is not the reviewed set".to_string());
    }
    if artifacts.is_empty() || artifacts.len() > 64 {
        return Err("binding build evidence artifact count is out of bounds".to_string());
    }

    let mut previous_name: Option<&str> = None;
    for artifact in artifacts {
        let mut fields = artifact.split('|');
        let name = fields.next().unwrap_or_default();
        let length_text = fields.next().unwrap_or_default();
        let sha256 = fields.next().unwrap_or_default();
        if fields.next().is_some()
            || name.is_empty()
            || name.len() > 96
            || !name.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-' | b'/')
            })
            || name
                .split('/')
                .any(|component| component.is_empty() || matches!(component, "." | ".."))
            || previous_name.is_some_and(|previous| previous >= name)
            || !canonical_positive_u64(length_text)
            || !is_lower_sha256(sha256)
        {
            return Err("binding build evidence artifact entry is malformed".to_string());
        }
        previous_name = Some(name);
    }
    if required_artifact_groups.iter().any(|alternatives| {
        alternatives.is_empty()
            || !alternatives.iter().any(|required| {
                artifacts
                    .binary_search_by(|artifact| {
                        artifact
                            .split_once('|')
                            .map_or("", |(name, _)| name)
                            .cmp(required)
                    })
                    .is_ok()
            })
    }) {
        return Err("binding build evidence omits a required static artifact family".to_string());
    }

    let mut digest = Sha256::new();
    digest.update(format.as_bytes());
    digest.update([0]);
    for feature in parsed_features {
        digest.update(feature.as_bytes());
        digest.update([0]);
    }
    digest.update([0]);
    digest.update(linkage.as_bytes());
    digest.update([0]);
    for artifact in artifacts {
        digest.update(artifact.as_bytes());
        digest.update([0]);
    }
    let computed = format!("{:x}", digest.finalize());
    if computed != reported_sha256 {
        return Err("binding build evidence digest does not match its fields".to_string());
    }
    Ok(computed)
}

fn canonical_positive_u64(value: &str) -> bool {
    value
        .parse::<u64>()
        .ok()
        .is_some_and(|parsed| parsed > 0 && parsed.to_string() == value)
}

fn is_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_field(field: &str) -> Result<(), String> {
    if field.is_empty()
        || !field.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
    {
        return Err("private build identity used an invalid structural field".to_string());
    }
    Ok(())
}

fn hash_frame(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// Returns a canonical, private serialization of the resolved dependency
/// closure for one package. Workspace membership and unrelated lock entries
/// are deliberately excluded; package identities, checksums, and resolved
/// dependency edges remain bound into the native build identity.
pub(crate) fn cargo_dependency_closure(
    lock_text: &str,
    root_name: &str,
    root_version: &str,
) -> Result<Vec<u8>, String> {
    let lock: toml::Value = toml::from_str(lock_text)
        .map_err(|_| "Cargo lock could not be parsed for native dependency identity".to_string())?;
    if lock
        .get("version")
        .and_then(toml::Value::as_integer)
        .is_none_or(|version| !matches!(version, 3 | 4))
    {
        return Err("Cargo lock version is unsupported for native dependency identity".to_string());
    }
    let package_values = lock
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| "Cargo lock package list is missing or malformed".to_string())?;
    if package_values.is_empty() {
        return Err("Cargo lock package list is empty".to_string());
    }

    #[derive(Clone, Debug)]
    struct PackageRecord {
        key: CargoPackageKey,
        checksum: Option<String>,
        dependency_specs: Vec<String>,
    }

    let mut packages = BTreeMap::<CargoPackageKey, PackageRecord>::new();
    for value in package_values {
        let name = required_lock_string(value, "name")?;
        let version = required_lock_string(value, "version")?;
        if !valid_package_component(&name) || !valid_package_component(&version) {
            return Err("Cargo lock contains an invalid package identity".to_string());
        }
        let source = optional_lock_string(value, "source")?;
        let checksum = optional_lock_string(value, "checksum")?;
        if checksum
            .as_deref()
            .is_some_and(|value| !is_lower_sha256(value))
        {
            return Err("Cargo lock contains a malformed package checksum".to_string());
        }
        if source
            .as_deref()
            .is_some_and(|source| source.starts_with("registry+"))
            && checksum.is_none()
        {
            return Err("Cargo lock registry package omits its checksum".to_string());
        }
        let dependency_specs = match value.get("dependencies") {
            None => Vec::new(),
            Some(value) => value
                .as_array()
                .ok_or_else(|| "Cargo lock dependencies list is malformed".to_string())?
                .iter()
                .map(|dependency| {
                    dependency
                        .as_str()
                        .map(str::to_string)
                        .ok_or_else(|| "Cargo lock dependency entry is malformed".to_string())
                })
                .collect::<Result<Vec<_>, _>>()?,
        };
        let key = CargoPackageKey {
            name,
            version,
            source,
        };
        let record = PackageRecord {
            key: key.clone(),
            checksum,
            dependency_specs,
        };
        if packages.insert(key, record).is_some() {
            return Err("Cargo lock contains an ambiguous duplicate package identity".to_string());
        }
    }

    let roots = packages
        .keys()
        .filter(|key| key.name == root_name && key.version == root_version && key.source.is_none())
        .cloned()
        .collect::<Vec<_>>();
    let root = match roots.as_slice() {
        [root] => root.clone(),
        [] => return Err("Cargo lock omits the native engine package".to_string()),
        _ => return Err("Cargo lock contains an ambiguous native engine package".to_string()),
    };

    let mut reachable = BTreeSet::new();
    let mut edges = BTreeSet::new();
    let mut pending = vec![root.clone()];
    while let Some(parent) = pending.pop() {
        if !reachable.insert(parent.clone()) {
            continue;
        }
        let record = packages
            .get(&parent)
            .ok_or_else(|| "Cargo lock dependency closure is incomplete".to_string())?;
        for dependency in &record.dependency_specs {
            let spec = parse_dependency_spec(dependency)?;
            let matches = packages
                .keys()
                .filter(|candidate| {
                    candidate.name == spec.name
                        && spec
                            .version
                            .as_ref()
                            .is_none_or(|version| &candidate.version == version)
                        && spec
                            .source
                            .as_ref()
                            .is_none_or(|source| candidate.source.as_ref() == Some(source))
                })
                .cloned()
                .collect::<Vec<_>>();
            let target = match matches.as_slice() {
                [target] => target.clone(),
                [] => {
                    return Err(
                        "Cargo lock dependency closure references a missing package".to_string()
                    );
                }
                _ => {
                    return Err(
                        "Cargo lock dependency closure has an ambiguous package reference"
                            .to_string(),
                    );
                }
            };
            edges.insert((parent.clone(), target.clone()));
            if !reachable.contains(&target) {
                pending.push(target);
            }
        }
    }

    let mut canonical = Vec::new();
    canonical.extend_from_slice(CARGO_CLOSURE_DOMAIN);
    canonical.extend_from_slice(&(reachable.len() as u64).to_le_bytes());
    for key in &reachable {
        let record = packages
            .get(key)
            .ok_or_else(|| "Cargo lock dependency closure is incomplete".to_string())?;
        append_closure_field(&mut canonical, record.key.name.as_bytes());
        append_closure_field(&mut canonical, record.key.version.as_bytes());
        append_closure_field(
            &mut canonical,
            record.key.source.as_deref().unwrap_or("").as_bytes(),
        );
        append_closure_field(
            &mut canonical,
            record.checksum.as_deref().unwrap_or("").as_bytes(),
        );
    }
    canonical.extend_from_slice(&(edges.len() as u64).to_le_bytes());
    for (parent, target) in edges {
        append_package_key(&mut canonical, &parent);
        append_package_key(&mut canonical, &target);
    }
    Ok(canonical)
}

/// Canonicalizes only workspace settings inherited by the native engine or
/// its DTO crate. Workspace membership and unrelated shared dependencies are
/// intentionally excluded.
pub(crate) fn cargo_workspace_build_inputs(
    workspace_manifest: &str,
    engine_manifest: &str,
    types_manifest: &str,
) -> Result<Vec<u8>, String> {
    let workspace: toml::Value = toml::from_str(workspace_manifest).map_err(|_| {
        "workspace manifest could not be parsed for native build identity".to_string()
    })?;
    let engine: toml::Value = toml::from_str(engine_manifest)
        .map_err(|_| "engine manifest could not be parsed for native build identity".to_string())?;
    let types: toml::Value = toml::from_str(types_manifest)
        .map_err(|_| "types manifest could not be parsed for native build identity".to_string())?;

    let workspace_package = workspace
        .get("workspace")
        .and_then(|value| value.get("package"))
        .and_then(toml::Value::as_table)
        .ok_or_else(|| "workspace package settings are missing or malformed".to_string())?;
    let workspace_dependencies = workspace
        .get("workspace")
        .and_then(|value| value.get("dependencies"))
        .and_then(toml::Value::as_table)
        .ok_or_else(|| "workspace dependencies are missing or malformed".to_string())?;
    let workspace_table = workspace
        .get("workspace")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| "workspace table is missing or malformed".to_string())?;

    let mut output = Vec::new();
    output.extend_from_slice(b"llama-native-workspace-inputs-v1\0");
    let resolver = workspace_table
        .get("resolver")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| "workspace dependency resolver is missing or malformed".to_string())?;
    append_closure_field(&mut output, b"resolver");
    append_closure_field(&mut output, resolver.as_bytes());
    let mut inherited_package = BTreeSet::new();
    let mut inherited_dependencies = BTreeSet::new();
    let mut inherits_workspace_lints = false;
    for manifest in [&engine, &types] {
        let package = manifest
            .get("package")
            .and_then(toml::Value::as_table)
            .ok_or_else(|| "native package manifest is missing its package table".to_string())?;
        for (key, value) in package {
            if value
                .as_table()
                .and_then(|table| table.get("workspace"))
                .and_then(toml::Value::as_bool)
                == Some(true)
            {
                inherited_package.insert(key.clone());
            }
        }
        collect_workspace_dependencies(manifest, &mut inherited_dependencies)?;
        inherits_workspace_lints |= manifest
            .get("lints")
            .and_then(|value| value.get("workspace"))
            .and_then(toml::Value::as_bool)
            == Some(true);
    }

    append_closure_field(&mut output, b"package");
    append_closure_field(&mut output, &(inherited_package.len() as u64).to_le_bytes());
    for key in inherited_package {
        let value = workspace_package
            .get(&key)
            .ok_or_else(|| "inherited workspace package setting is missing".to_string())?;
        append_closure_field(&mut output, key.as_bytes());
        append_toml_value(&mut output, value);
    }

    append_closure_field(&mut output, b"dependencies");
    append_closure_field(
        &mut output,
        &(inherited_dependencies.len() as u64).to_le_bytes(),
    );
    for name in inherited_dependencies {
        let value = workspace_dependencies
            .get(&name)
            .ok_or_else(|| "inherited workspace dependency is missing".to_string())?;
        append_closure_field(&mut output, name.as_bytes());
        append_toml_value(&mut output, value);
    }

    append_closure_field(&mut output, b"lints");
    if inherits_workspace_lints {
        let value = workspace_table
            .get("lints")
            .ok_or_else(|| "inherited workspace lints are missing".to_string())?;
        output.push(1);
        append_toml_value(&mut output, value);
    } else {
        output.push(0);
    }
    Ok(output)
}

#[cfg(test)]
mod cargo_workspace_tests {
    use super::cargo_workspace_build_inputs;

    fn manifests(
        edition: &str,
        resolver: &str,
        serde_dependency: &str,
        extra_member: &str,
        unrelated_dependency: &str,
    ) -> Vec<u8> {
        let workspace = format!(
            r#"[workspace]
members = ["engine", "types"{extra_member}]
resolver = "{resolver}"

[workspace.package]
edition = "{edition}"

[workspace.dependencies]
serde = {serde_dependency}
{unrelated_dependency}

[workspace.lints.rust]
unsafe_code = "forbid"
"#
        );
        let engine = r#"[package]
edition.workspace = true

[dependencies]
serde.workspace = true
"#;
        let types = r#"[package]
edition.workspace = true

[dependencies]
serde.workspace = true

[lints]
workspace = true
"#;
        cargo_workspace_build_inputs(&workspace, engine, types).expect("workspace inputs")
    }

    #[test]
    fn workspace_projection_ignores_members_and_unreferenced_dependencies() {
        let base = manifests(
            "2024",
            "3",
            "{ version = \"1\", default-features = false, features = [\"derive\"] }",
            "",
            "",
        );
        let unrelated = manifests(
            "2024",
            "3",
            "{ version = \"1\", default-features = false, features = [\"derive\"] }",
            ", \"pocket-backend\"",
            "pocket-backend = \"0.1\"",
        );
        assert_eq!(base, unrelated);
    }

    #[test]
    fn workspace_projection_tracks_inherited_package_and_dependency_features() {
        let baseline = manifests(
            "2024",
            "3",
            "{ version = \"1\", default-features = false, features = [\"derive\"] }",
            "",
            "",
        );
        let changed_edition = manifests(
            "2021",
            "3",
            "{ version = \"1\", default-features = false, features = [\"derive\"] }",
            "",
            "",
        );
        let changed_features = manifests(
            "2024",
            "3",
            "{ version = \"1\", default-features = true, features = [\"derive\"] }",
            "",
            "",
        );
        assert_ne!(baseline, changed_edition);
        assert_ne!(baseline, changed_features);
        let changed_resolver = manifests(
            "2024",
            "2",
            "{ version = \"1\", default-features = false, features = [\"derive\"] }",
            "",
            "",
        );
        assert_ne!(baseline, changed_resolver);
    }

    #[test]
    fn workspace_projection_is_independent_of_toml_table_key_order() {
        let first = manifests(
            "2024",
            "3",
            "{ version = \"1\", default-features = false, features = [\"derive\"] }",
            "",
            "",
        );
        let reordered = manifests(
            "2024",
            "3",
            "{ features = [\"derive\"], version = \"1\", default-features = false }",
            "",
            "",
        );
        assert_eq!(first, reordered);
    }
}

fn collect_workspace_dependencies(
    manifest: &toml::Value,
    output: &mut BTreeSet<String>,
) -> Result<(), String> {
    for section in ["dependencies", "build-dependencies", "dev-dependencies"] {
        if let Some(table) = manifest.get(section).and_then(toml::Value::as_table) {
            collect_workspace_dependency_table(table, output)?;
        }
    }
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for target in targets.values() {
            for section in ["dependencies", "build-dependencies", "dev-dependencies"] {
                if let Some(table) = target.get(section).and_then(toml::Value::as_table) {
                    collect_workspace_dependency_table(table, output)?;
                }
            }
        }
    }
    Ok(())
}

fn collect_workspace_dependency_table(
    table: &toml::map::Map<String, toml::Value>,
    output: &mut BTreeSet<String>,
) -> Result<(), String> {
    for (name, value) in table {
        if value
            .as_table()
            .and_then(|table| table.get("workspace"))
            .and_then(toml::Value::as_bool)
            == Some(true)
        {
            output.insert(name.clone());
        }
    }
    Ok(())
}

fn append_toml_value(output: &mut Vec<u8>, value: &toml::Value) {
    match value {
        toml::Value::String(value) => {
            output.push(1);
            append_closure_field(output, value.as_bytes());
        }
        toml::Value::Integer(value) => {
            output.push(2);
            output.extend_from_slice(&value.to_le_bytes());
        }
        toml::Value::Float(value) => {
            output.push(3);
            append_closure_field(output, value.to_string().as_bytes());
        }
        toml::Value::Boolean(value) => {
            output.extend_from_slice(&[4, u8::from(*value)]);
        }
        toml::Value::Datetime(value) => {
            output.push(5);
            append_closure_field(output, value.to_string().as_bytes());
        }
        toml::Value::Array(values) => {
            output.push(6);
            append_closure_field(output, &(values.len() as u64).to_le_bytes());
            for value in values {
                append_toml_value(output, value);
            }
        }
        toml::Value::Table(values) => {
            output.push(7);
            append_closure_field(output, &(values.len() as u64).to_le_bytes());
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            for (key, value) in entries {
                append_closure_field(output, key.as_bytes());
                append_toml_value(output, value);
            }
        }
    }
}

struct DependencySpec {
    name: String,
    version: Option<String>,
    source: Option<String>,
}

fn parse_dependency_spec(value: &str) -> Result<DependencySpec, String> {
    let (identity, source) = if let Some(without_close) = value.strip_suffix(')') {
        if let Some((identity, source)) = without_close.split_once(" (") {
            if source.is_empty()
                || source
                    .chars()
                    .any(|character| matches!(character, '(' | ')' | '\n' | '\r'))
            {
                return Err("Cargo lock dependency source is malformed".to_string());
            }
            (identity, Some(source.to_string()))
        } else {
            (value, None)
        }
    } else {
        (value, None)
    };
    let fields = identity.split_whitespace().collect::<Vec<_>>();
    let (name, version) = match fields.as_slice() {
        [name] => (*name, None),
        [name, version] => (*name, Some((*version).to_string())),
        _ => return Err("Cargo lock dependency identity is malformed".to_string()),
    };
    if !valid_package_component(name)
        || version
            .as_deref()
            .is_some_and(|version| !valid_package_component(version))
    {
        return Err("Cargo lock dependency identity is malformed".to_string());
    }
    Ok(DependencySpec {
        name: name.to_string(),
        version,
        source,
    })
}

fn required_lock_string(value: &toml::Value, field: &str) -> Result<String, String> {
    value
        .get(field)
        .and_then(toml::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "Cargo lock contains a missing or malformed package field".to_string())
}

fn optional_lock_string(value: &toml::Value, field: &str) -> Result<Option<String>, String> {
    match value.get(field) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .filter(|value| !value.is_empty())
            .map(|value| Some(value.to_string()))
            .ok_or_else(|| "Cargo lock contains a malformed optional package field".to_string()),
    }
}

fn valid_package_component(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+' | b':')
        })
}

fn append_closure_field(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_le_bytes());
    output.extend_from_slice(value);
}

fn append_package_key(output: &mut Vec<u8>, key: &CargoPackageKey) {
    append_closure_field(output, key.name.as_bytes());
    append_closure_field(output, key.version.as_bytes());
    append_closure_field(output, key.source.as_deref().unwrap_or("").as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reversible_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn fixture_lock(root_dependencies: &str, checksum: &str, extra: &str) -> String {
        format!(
            r#"version = 4

[[package]]
name = "llama-native-engine"
version = "1.0.0"
dependencies = [{root_dependencies}]

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{checksum}"
dependencies = ["serde_core 1.0.0 (registry+https://github.com/rust-lang/crates.io-index)"]

[[package]]
name = "serde_core"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{}"

{extra}
"#,
            "b".repeat(64)
        )
    }

    fn fixture_closure(lock: &str) -> Result<Vec<u8>, String> {
        cargo_dependency_closure(lock, "llama-native-engine", "1.0.0")
    }

    #[test]
    fn cargo_closure_ignores_unrelated_workspace_packages() {
        let base = fixture_lock(
            "\"serde 1.0.0 (registry+https://github.com/rust-lang/crates.io-index)\"",
            &"a".repeat(64),
            "",
        );
        let with_unrelated_pocket_member = fixture_lock(
            "\"serde 1.0.0 (registry+https://github.com/rust-lang/crates.io-index)\"",
            &"a".repeat(64),
            r#"[[package]]
name = "speech-native-backend-pocket"
version = "0.1.0"
dependencies = ["ndarray 0.16.1 (registry+https://github.com/rust-lang/crates.io-index)"]

[[package]]
name = "ndarray"
version = "0.16.1"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
"#,
        );

        assert_eq!(
            fixture_closure(&base).expect("base closure"),
            fixture_closure(&with_unrelated_pocket_member).expect("closure with unrelated member")
        );
    }

    #[test]
    fn cargo_closure_ignores_lock_reference_qualification_for_same_resolution() {
        let unqualified = fixture_lock("\"serde\"", &"a".repeat(64), "");
        let qualified_with_duplicate_unrelated_version = fixture_lock(
            "\"serde 1.0.0 (registry+https://github.com/rust-lang/crates.io-index)\"",
            &"a".repeat(64),
            r#"[[package]]
name = "serde"
version = "2.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
"#,
        );

        assert_eq!(
            fixture_closure(&unqualified).expect("unqualified reference resolves uniquely"),
            fixture_closure(&qualified_with_duplicate_unrelated_version)
                .expect("qualified reference resolves despite unrelated duplicate version")
        );
    }

    #[test]
    fn cargo_closure_tracks_reachable_package_version_and_checksum() {
        let base = fixture_lock(
            "\"serde 1.0.0 (registry+https://github.com/rust-lang/crates.io-index)\"",
            &"a".repeat(64),
            "",
        );
        let checksum_changed = fixture_lock(
            "\"serde 1.0.0 (registry+https://github.com/rust-lang/crates.io-index)\"",
            &"d".repeat(64),
            "",
        );
        assert_ne!(
            fixture_closure(&base).expect("base closure"),
            fixture_closure(&checksum_changed).expect("changed checksum closure")
        );

        let version_changed = base
            .replace("serde 1.0.0 (registry+", "serde 1.1.0 (registry+")
            .replace(
                "name = \"serde\"\nversion = \"1.0.0\"",
                "name = \"serde\"\nversion = \"1.1.0\"",
            );
        assert_ne!(
            fixture_closure(&base).expect("base closure"),
            fixture_closure(&version_changed).expect("changed version closure")
        );
    }

    #[test]
    fn cargo_closure_rejects_missing_and_ambiguous_resolution() {
        let missing = fixture_lock("\"missing 1.0.0\"", &"a".repeat(64), "");
        assert!(
            fixture_closure(&missing)
                .expect_err("missing lock package must fail closed")
                .contains("missing package")
        );

        let ambiguous = fixture_lock(
            "\"serde\"",
            &"a".repeat(64),
            r#"[[package]]
name = "serde"
version = "2.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
"#,
        );
        assert!(
            fixture_closure(&ambiguous)
                .expect_err("ambiguous lock package must fail closed")
                .contains("ambiguous package reference")
        );
    }

    #[test]
    fn cargo_closure_rejects_malformed_lockfile() {
        assert!(
            fixture_closure("version = [invalid")
                .expect_err("malformed lockfile must fail closed")
                .contains("could not be parsed")
        );
    }

    #[test]
    fn generated_compile_metadata_cannot_reveal_private_values() {
        const SENTINEL: &str = "/Users/private/SDK/token=sentinel-build-secret";
        let mut identity = PrivateBuildIdentity::new();
        identity
            .add_optional_os("environment.cflags", Some(OsStr::new(SENTINEL)))
            .expect("field is allowlisted structurally");
        let generated =
            rustc_digest_directive(&identity.finish_hex()).expect("digest directive must be valid");

        assert!(!generated.contains(SENTINEL));
        assert!(!generated.contains(&reversible_hex(SENTINEL.as_bytes())));
        assert_eq!(
            generated.len(),
            "cargo:rustc-env=LLAMA_NATIVE_BUILD_MANIFEST_SHA256=".len() + 64
        );
    }

    #[test]
    fn changing_one_allowlisted_input_changes_the_private_digest() {
        let digest = |flags: &[u8]| {
            let mut identity = PrivateBuildIdentity::new();
            identity
                .add_bytes("environment.cflags", flags)
                .expect("field is allowlisted structurally");
            identity.finish_hex()
        };

        assert_eq!(digest(b"-O2"), digest(b"-O2"));
        assert_ne!(digest(b"-O2"), digest(b"-O3"));
    }

    #[test]
    fn structural_field_names_cannot_smuggle_values() {
        let mut identity = PrivateBuildIdentity::new();
        let error = identity
            .add_bytes("environment.path=/private", b"value")
            .expect_err("field names are a closed safe alphabet");
        assert!(!error.contains("/private"));
    }

    fn evidence_digest(features: &str, linkage: &str, artifacts: &[String]) -> String {
        let mut digest = Sha256::new();
        digest.update(BINDING_EVIDENCE_FORMAT.as_bytes());
        digest.update([0]);
        for feature in features.split(',') {
            digest.update(feature.as_bytes());
            digest.update([0]);
        }
        digest.update([0]);
        digest.update(linkage.as_bytes());
        digest.update([0]);
        for artifact in artifacts {
            digest.update(artifact.as_bytes());
            digest.update([0]);
        }
        format!("{:x}", digest.finalize())
    }

    #[test]
    fn binding_evidence_is_recomputed_and_linkage_is_fail_closed() {
        let features = "common,default,metal,mtmd";
        let artifacts = vec![
            format!("ggml/ggml-base/link|17|{}", "a".repeat(64)),
            format!("llama/llama/link|29|{}", "b".repeat(64)),
        ];
        let digest = evidence_digest(features, REVIEWED_STATIC_LINKAGE, &artifacts);
        assert_eq!(
            validate_binding_build_evidence(
                BINDING_EVIDENCE_FORMAT,
                features,
                REVIEWED_STATIC_LINKAGE,
                &artifacts,
                &digest,
                &["common", "default", "metal", "mtmd"],
                &[&["ggml/ggml-base/link"], &["llama/llama/link"],],
            )
            .expect("canonical reviewed evidence is accepted"),
            digest
        );

        let dynamic = "local=shared;ggml-origin=vendored;ggml=shared;backends=linked";
        let dynamic_digest = evidence_digest(features, dynamic, &artifacts);
        assert!(
            validate_binding_build_evidence(
                BINDING_EVIDENCE_FORMAT,
                features,
                dynamic,
                &artifacts,
                &dynamic_digest,
                &["common", "default", "metal", "mtmd"],
                &[&["ggml/ggml-base/link"], &["llama/llama/link"],],
            )
            .is_err()
        );
        assert!(
            validate_binding_build_evidence(
                BINDING_EVIDENCE_FORMAT,
                features,
                REVIEWED_STATIC_LINKAGE,
                &artifacts,
                &"c".repeat(64),
                &["common", "default", "metal", "mtmd"],
                &[&["ggml/ggml-base/link"], &["llama/llama/link"],],
            )
            .is_err()
        );

        let dynamic_features = "common,default,dynamic-link,metal,mtmd";
        let dynamic_features_digest =
            evidence_digest(dynamic_features, REVIEWED_STATIC_LINKAGE, &artifacts);
        assert!(
            validate_binding_build_evidence(
                BINDING_EVIDENCE_FORMAT,
                dynamic_features,
                REVIEWED_STATIC_LINKAGE,
                &artifacts,
                &dynamic_features_digest,
                &["common", "default", "metal", "mtmd"],
                &[&["ggml/ggml-base/link"], &["llama/llama/link"]],
            )
            .is_err()
        );
    }

    #[test]
    fn binding_evidence_rejects_noncanonical_or_incomplete_packets() {
        let features = "common,default,metal,mtmd";
        let canonical = vec![
            format!("ggml/ggml-base/link|17|{}", "a".repeat(64)),
            format!("llama/llama/link|29|{}", "b".repeat(64)),
        ];
        let required: &[&[&str]] = &[&["ggml/ggml-base/link"], &["llama/llama/link"]];

        let mut unsorted = canonical.clone();
        unsorted.reverse();
        let unsorted_digest = evidence_digest(features, REVIEWED_STATIC_LINKAGE, &unsorted);
        assert!(
            validate_binding_build_evidence(
                BINDING_EVIDENCE_FORMAT,
                features,
                REVIEWED_STATIC_LINKAGE,
                &unsorted,
                &unsorted_digest,
                &["common", "default", "metal", "mtmd"],
                required,
            )
            .is_err()
        );

        let noncanonical_length = vec![
            format!("ggml/ggml-base/link|017|{}", "a".repeat(64)),
            canonical[1].clone(),
        ];
        let length_digest =
            evidence_digest(features, REVIEWED_STATIC_LINKAGE, &noncanonical_length);
        assert!(
            validate_binding_build_evidence(
                BINDING_EVIDENCE_FORMAT,
                features,
                REVIEWED_STATIC_LINKAGE,
                &noncanonical_length,
                &length_digest,
                &["common", "default", "metal", "mtmd"],
                required,
            )
            .is_err()
        );

        let digest = evidence_digest(features, REVIEWED_STATIC_LINKAGE, &canonical);
        assert!(
            validate_binding_build_evidence(
                BINDING_EVIDENCE_FORMAT,
                features,
                REVIEWED_STATIC_LINKAGE,
                &canonical,
                &digest,
                &["common", "default", "metal", "mtmd"],
                &[&["mtmd/mtmd/link"]],
            )
            .is_err()
        );
    }

    #[test]
    fn binding_evidence_accepts_only_one_member_of_each_required_family() {
        let features = "common,default,mtmd";
        let requirements: &[&[&str]] = &[
            &["common/common/link", "common/llama-common/link"],
            &["ggml/ggml/link"],
        ];
        for common_name in ["common/common/link", "common/llama-common/link"] {
            let artifacts = vec![
                format!("{common_name}|17|{}", "a".repeat(64)),
                format!("ggml/ggml/link|29|{}", "b".repeat(64)),
            ];
            let digest = evidence_digest(features, REVIEWED_STATIC_LINKAGE, &artifacts);
            validate_binding_build_evidence(
                BINDING_EVIDENCE_FORMAT,
                features,
                REVIEWED_STATIC_LINKAGE,
                &artifacts,
                &digest,
                &["common", "default", "mtmd"],
                requirements,
            )
            .expect("either reviewed common-library spelling satisfies the family");
        }
    }

    #[test]
    fn reviewed_platform_profiles_require_the_static_cross_platform_core() {
        let core_artifacts = [
            "common/common/link",
            "common/llama_cpp_sys_2_common_wrapper/link",
            "ggml/ggml-base/link",
            "ggml/ggml-cpu/link",
            "ggml/ggml/link",
            "llama/llama/link",
            "mtmd/mtmd/link",
        ];
        for target in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
            let (expected_features, required) = reviewed_binding_profile(target);
            assert_eq!(expected_features, &["common", "default", "mtmd"]);
            let features = expected_features.join(",");
            let artifacts = core_artifacts
                .iter()
                .enumerate()
                .map(|(index, name)| format!("{name}|{}|{}", index + 1, "a".repeat(64)))
                .collect::<Vec<_>>();
            let digest = evidence_digest(&features, REVIEWED_STATIC_LINKAGE, &artifacts);
            validate_binding_build_evidence(
                BINDING_EVIDENCE_FORMAT,
                &features,
                REVIEWED_STATIC_LINKAGE,
                &artifacts,
                &digest,
                expected_features,
                required,
            )
            .expect("reviewed static Linux and Windows artifact names satisfy the profile");
        }

        let (features, required) = reviewed_binding_profile("aarch64-apple-darwin");
        assert_eq!(features, &["common", "default", "metal", "mtmd"]);
        assert!(required.contains(&&["ggml/ggml-metal/link"][..]));
    }

    #[test]
    fn effective_cmake_cache_is_unique_static_vendored_and_reviewed() {
        let accepted: &[u8] = b"BUILD_SHARED_LIBS:BOOL=OFF\n\
            GGML_BACKEND_DL:BOOL=OFF\n\
            GGML_CPU_ALL_VARIANTS:BOOL=OFF\n\
            LLAMA_USE_SYSTEM_GGML:BOOL=OFF\n\
            CMAKE_GENERATOR:INTERNAL=Ninja\n";
        assert_eq!(
            validate_reviewed_cmake_cache(accepted).expect("reviewed cache is accepted"),
            "Ninja"
        );

        let windows = replace_once(
            accepted,
            b"CMAKE_GENERATOR:INTERNAL=Ninja",
            b"CMAKE_GENERATOR:INTERNAL=Visual Studio 17 2022",
        );
        assert_eq!(
            validate_reviewed_cmake_cache(&windows)
                .expect("the pinned Windows runner generator is accepted"),
            "Visual Studio 17 2022"
        );

        let windows_2026 = replace_once(
            accepted,
            b"CMAKE_GENERATOR:INTERNAL=Ninja",
            b"CMAKE_GENERATOR:INTERNAL=Visual Studio 18 2026",
        );
        assert_eq!(
            validate_reviewed_cmake_cache(&windows_2026)
                .expect("the current pinned Windows runner generator is accepted"),
            "Visual Studio 18 2026"
        );

        for rejected in [
            replace_once(
                accepted,
                b"BUILD_SHARED_LIBS:BOOL=OFF",
                b"BUILD_SHARED_LIBS:BOOL=ON",
            ),
            replace_once(
                accepted,
                b"GGML_BACKEND_DL:BOOL=OFF",
                b"GGML_BACKEND_DL:BOOL=ON",
            ),
            replace_once(
                accepted,
                b"LLAMA_USE_SYSTEM_GGML:BOOL=OFF",
                b"LLAMA_USE_SYSTEM_GGML:BOOL=ON",
            ),
            replace_once(
                accepted,
                b"CMAKE_GENERATOR:INTERNAL=Ninja",
                b"CMAKE_GENERATOR:INTERNAL=Xcode",
            ),
            [accepted, b"BUILD_SHARED_LIBS:BOOL=OFF\n"].concat(),
            replace_once(
                accepted,
                b"BUILD_SHARED_LIBS:BOOL=OFF",
                b"BUILD_SHARED_LIBS:STRING=OFF",
            ),
        ] {
            validate_reviewed_cmake_cache(&rejected)
                .expect_err("unreviewed or ambiguous cache must fail closed");
        }
    }

    fn replace_once(bytes: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
        let position = bytes
            .windows(from.len())
            .position(|window| window == from)
            .expect("test fixture contains replacement needle");
        let mut replaced = Vec::with_capacity(bytes.len() - from.len() + to.len());
        replaced.extend_from_slice(&bytes[..position]);
        replaced.extend_from_slice(to);
        replaced.extend_from_slice(&bytes[position + from.len()..]);
        replaced
    }
}
