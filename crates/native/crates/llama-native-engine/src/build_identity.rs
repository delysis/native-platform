#![forbid(unsafe_code)]

use sha2::{Digest, Sha256};
use std::ffi::OsStr;

const PRIVATE_FIELD_DOMAIN: &[u8] = b"llama-native-private-build-field-v1\0";
const PRIVATE_MANIFEST_DOMAIN: &[u8] = b"llama-native-private-build-manifest-v3\0";
pub(crate) const BINDING_EVIDENCE_FORMAT: &str = "llama.native-build-evidence.v1";
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

#[cfg(test)]
mod tests {
    use super::*;

    fn reversible_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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
