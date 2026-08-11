use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{BlobId, ModelRole, PromptMode};

pub const BUILD_MODEL_POLICY_SCHEMA_VERSION: u32 = 1;
pub const GEMMA_4_E2B_BASE_Q8_PROFILE_ID: &str = "gemma_4_e2b_base_q8_loom_v1";
pub const GEMMA_4_E2B_BASE_Q8_SHA256: &str =
    "aa0a9a03993440f45176f19f8189a2e84c210ff8628ec13dc6edf42d017f7670";
pub const GEMMA_4_E2B_BASE_Q8_FILE_BYTES: u64 = 4_954_576_032;
pub const GEMMA_4_E2B_BASE_Q8_BLOB_ID: BlobId = BlobId::from_bytes([
    0xaa, 0x0a, 0x9a, 0x03, 0x99, 0x34, 0x40, 0xf4, 0x51, 0x76, 0xf1, 0x9f, 0x81, 0x89, 0xa2, 0xe8,
    0x4c, 0x21, 0x0f, 0xf8, 0x62, 0x8e, 0xc1, 0x3d, 0xc6, 0xed, 0xf4, 0x2d, 0x01, 0x7f, 0x76, 0x70,
]);

const NONE_V1_CANONICAL_SHA256: BlobId = BlobId::from_bytes([
    0xce, 0x3b, 0xdf, 0x5e, 0x3d, 0xbc, 0xac, 0x6f, 0x7b, 0xcc, 0x16, 0x4e, 0xc4, 0xcc, 0x5c, 0x78,
    0xb4, 0xa7, 0xb5, 0xbe, 0xf7, 0xc4, 0x9b, 0x3c, 0xd5, 0x2c, 0x61, 0xe1, 0x23, 0xb7, 0x5f, 0xe0,
]);
const WRITER_GEMMA4_BASE_V1_CANONICAL_SHA256: BlobId = BlobId::from_bytes([
    0xc0, 0x49, 0x2f, 0xb2, 0x28, 0x5a, 0xd0, 0x92, 0x2f, 0x89, 0xab, 0x72, 0x88, 0xd6, 0x3e, 0xf6,
    0x8f, 0xd1, 0x7f, 0x51, 0x33, 0xf0, 0x0e, 0xa4, 0x27, 0x66, 0x22, 0xa1, 0x5c, 0x2d, 0xc4, 0xe6,
]);
const WRITER_GEMMA4_BASE_V2_CANONICAL_SHA256: BlobId = BlobId::from_bytes([
    0x2d, 0x40, 0x2d, 0x21, 0x3b, 0x60, 0xba, 0x65, 0xc4, 0xd0, 0x18, 0x90, 0x7e, 0x9e, 0xba, 0x67,
    0xcc, 0xfb, 0xc1, 0xe9, 0x70, 0x81, 0xcc, 0x05, 0x05, 0xf9, 0x71, 0x3a, 0xe2, 0xdd, 0x89, 0xd2,
]);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum BuildModelPolicyFormat {
    #[serde(rename = "loom-build-model-policy")]
    LoomBuildModelPolicy,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum BuildModelPolicyName {
    #[serde(rename = "none-v1")]
    NoneV1,
    #[serde(rename = "writer-gemma4-base-v1")]
    WriterGemma4BaseV1,
    #[serde(rename = "writer-gemma4-base-v2")]
    WriterGemma4BaseV2,
}

pub const DEFAULT_DESKTOP_BUILD_MODEL_POLICY_NAME: BuildModelPolicyName =
    BuildModelPolicyName::WriterGemma4BaseV2;

impl BuildModelPolicyName {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoneV1 => "none-v1",
            Self::WriterGemma4BaseV1 => "writer-gemma4-base-v1",
            Self::WriterGemma4BaseV2 => "writer-gemma4-base-v2",
        }
    }
}

impl fmt::Display for BuildModelPolicyName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionActivation {
    ProjectOptIn,
    QuietDefault,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum BuildWriterProfileId {
    #[serde(rename = "gemma_4_e2b_base_q8_loom_v1")]
    Gemma4E2bBaseQ8LoomV1,
}

impl BuildWriterProfileId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gemma4E2bBaseQ8LoomV1 => GEMMA_4_E2B_BASE_Q8_PROFILE_ID,
        }
    }
}

impl fmt::Display for BuildWriterProfileId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceBoundary {
    LocalOnly,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostedFallback {
    Forbidden,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildWriterModelPolicyV1 {
    profile_id: BuildWriterProfileId,
    role: ModelRole,
    prompt_mode: PromptMode,
    model_sha256: BlobId,
    model_file_bytes: u64,
}

impl BuildWriterModelPolicyV1 {
    pub fn profile_id(&self) -> &str {
        self.profile_id.as_str()
    }

    pub const fn typed_profile_id(&self) -> BuildWriterProfileId {
        self.profile_id
    }

    pub const fn role(&self) -> ModelRole {
        self.role
    }

    pub const fn prompt_mode(&self) -> PromptMode {
        self.prompt_mode
    }

    pub const fn model_sha256(&self) -> BlobId {
        self.model_sha256
    }

    pub const fn model_file_bytes(&self) -> u64 {
        self.model_file_bytes
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RankedBuildWriterModelPolicyV1<'a> {
    rank: u32,
    writer: &'a BuildWriterModelPolicyV1,
}

impl<'a> RankedBuildWriterModelPolicyV1<'a> {
    pub const fn rank(self) -> u32 {
        self.rank
    }

    pub const fn writer(self) -> &'a BuildWriterModelPolicyV1 {
        self.writer
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BuildModelPolicyV1 {
    format: BuildModelPolicyFormat,
    schema_version: u32,
    name: BuildModelPolicyName,
    activation: SuggestionActivation,
    inference_boundary: InferenceBoundary,
    hosted_fallback: HostedFallback,
    writers: Vec<BuildWriterModelPolicyV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildModelPolicyV1Wire {
    format: BuildModelPolicyFormat,
    schema_version: u32,
    name: BuildModelPolicyName,
    activation: SuggestionActivation,
    inference_boundary: InferenceBoundary,
    hosted_fallback: HostedFallback,
    writers: Vec<BuildWriterModelPolicyV1>,
}

impl BuildModelPolicyV1 {
    fn none() -> Self {
        Self {
            format: BuildModelPolicyFormat::LoomBuildModelPolicy,
            schema_version: BUILD_MODEL_POLICY_SCHEMA_VERSION,
            name: BuildModelPolicyName::NoneV1,
            activation: SuggestionActivation::ProjectOptIn,
            inference_boundary: InferenceBoundary::LocalOnly,
            hosted_fallback: HostedFallback::Forbidden,
            writers: Vec::new(),
        }
    }

    fn writer_gemma4_base_v1() -> Self {
        Self::writer_gemma4_base(
            BuildModelPolicyName::WriterGemma4BaseV1,
            SuggestionActivation::ProjectOptIn,
        )
    }

    fn writer_gemma4_base_v2() -> Self {
        Self::writer_gemma4_base(
            BuildModelPolicyName::WriterGemma4BaseV2,
            SuggestionActivation::QuietDefault,
        )
    }

    fn writer_gemma4_base(name: BuildModelPolicyName, activation: SuggestionActivation) -> Self {
        Self {
            format: BuildModelPolicyFormat::LoomBuildModelPolicy,
            schema_version: BUILD_MODEL_POLICY_SCHEMA_VERSION,
            name,
            activation,
            inference_boundary: InferenceBoundary::LocalOnly,
            hosted_fallback: HostedFallback::Forbidden,
            writers: vec![BuildWriterModelPolicyV1 {
                profile_id: BuildWriterProfileId::Gemma4E2bBaseQ8LoomV1,
                role: ModelRole::Writer,
                prompt_mode: PromptMode::Completion,
                model_sha256: GEMMA_4_E2B_BASE_Q8_BLOB_ID,
                model_file_bytes: GEMMA_4_E2B_BASE_Q8_FILE_BYTES,
            }],
        }
    }

    fn validate(self) -> Result<Self, &'static str> {
        if self.schema_version != BUILD_MODEL_POLICY_SCHEMA_VERSION {
            return Err("unsupported build-model policy schema version");
        }
        let expected = match self.name {
            BuildModelPolicyName::NoneV1 => Self::none(),
            BuildModelPolicyName::WriterGemma4BaseV1 => Self::writer_gemma4_base_v1(),
            BuildModelPolicyName::WriterGemma4BaseV2 => Self::writer_gemma4_base_v2(),
        };
        if self != expected {
            return Err("the named build-model policy does not match its allow-listed contract");
        }
        Ok(self)
    }

    pub const fn format(&self) -> BuildModelPolicyFormat {
        self.format
    }

    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub const fn name(&self) -> BuildModelPolicyName {
        self.name
    }

    pub const fn activation(&self) -> SuggestionActivation {
        self.activation
    }

    pub const fn inference_boundary(&self) -> InferenceBoundary {
        self.inference_boundary
    }

    pub const fn hosted_fallback(&self) -> HostedFallback {
        self.hosted_fallback
    }

    pub fn writers(&self) -> &[BuildWriterModelPolicyV1] {
        &self.writers
    }
}

impl<'de> Deserialize<'de> for BuildModelPolicyV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = BuildModelPolicyV1Wire::deserialize(deserializer)?;
        Self {
            format: wire.format,
            schema_version: wire.schema_version,
            name: wire.name,
            activation: wire.activation,
            inference_boundary: wire.inference_boundary,
            hosted_fallback: wire.hosted_fallback,
            writers: wire.writers,
        }
        .validate()
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BuildModelPolicy {
    V1(BuildModelPolicyV1),
}

/// Read-only identity exposed to renderers. Construction remains private so
/// callers cannot pair a policy name with activation or digest from a
/// different allow-listed contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct BuildModelPolicyIdentity {
    name: BuildModelPolicyName,
    activation: SuggestionActivation,
    canonical_sha256: BlobId,
}

impl BuildModelPolicyIdentity {
    pub const fn name(self) -> BuildModelPolicyName {
        self.name
    }

    pub const fn activation(self) -> SuggestionActivation {
        self.activation
    }

    pub const fn canonical_sha256(self) -> BlobId {
        self.canonical_sha256
    }
}

impl BuildModelPolicyName {
    /// Returns the closed, source-controlled identity of this named policy.
    ///
    /// Adding or changing a policy variant requires updating this exhaustive
    /// match. Canonical serialization is checked against these constants at
    /// the embedded JSON boundary and is never part of renderer IPC.
    pub const fn identity(self) -> BuildModelPolicyIdentity {
        match self {
            Self::NoneV1 => BuildModelPolicyIdentity {
                name: Self::NoneV1,
                activation: SuggestionActivation::ProjectOptIn,
                canonical_sha256: NONE_V1_CANONICAL_SHA256,
            },
            Self::WriterGemma4BaseV1 => BuildModelPolicyIdentity {
                name: Self::WriterGemma4BaseV1,
                activation: SuggestionActivation::ProjectOptIn,
                canonical_sha256: WRITER_GEMMA4_BASE_V1_CANONICAL_SHA256,
            },
            Self::WriterGemma4BaseV2 => BuildModelPolicyIdentity {
                name: Self::WriterGemma4BaseV2,
                activation: SuggestionActivation::QuietDefault,
                canonical_sha256: WRITER_GEMMA4_BASE_V2_CANONICAL_SHA256,
            },
        }
    }
}

impl BuildModelPolicy {
    pub fn none_v1() -> Self {
        Self::V1(BuildModelPolicyV1::none())
    }

    pub fn writer_gemma4_base_v1() -> Self {
        Self::V1(BuildModelPolicyV1::writer_gemma4_base_v1())
    }

    pub fn writer_gemma4_base_v2() -> Self {
        Self::V1(BuildModelPolicyV1::writer_gemma4_base_v2())
    }

    pub const fn as_v1(&self) -> &BuildModelPolicyV1 {
        match self {
            Self::V1(policy) => policy,
        }
    }

    pub const fn name(&self) -> BuildModelPolicyName {
        self.as_v1().name()
    }

    pub const fn activation(&self) -> SuggestionActivation {
        self.as_v1().activation()
    }

    pub fn writers(&self) -> &[BuildWriterModelPolicyV1] {
        self.as_v1().writers()
    }

    pub fn writer_by_profile_id(
        &self,
        profile_id: &str,
    ) -> Option<RankedBuildWriterModelPolicyV1<'_>> {
        self.writers()
            .iter()
            .enumerate()
            .find(|(_, writer)| writer.profile_id.as_str() == profile_id)
            .and_then(|(rank, writer)| {
                u32::try_from(rank)
                    .ok()
                    .map(|rank| RankedBuildWriterModelPolicyV1 { rank, writer })
            })
    }

    pub fn matching_writer(
        &self,
        model_sha256: &str,
        model_file_bytes: u64,
    ) -> Option<RankedBuildWriterModelPolicyV1<'_>> {
        let digest = model_sha256.parse::<BlobId>().ok()?;
        self.writers()
            .iter()
            .enumerate()
            .find(|(_, writer)| {
                writer.role == ModelRole::Writer
                    && writer.prompt_mode == PromptMode::Completion
                    && writer.model_sha256 == digest
                    && writer.model_file_bytes == model_file_bytes
            })
            .and_then(|(rank, writer)| {
                u32::try_from(rank)
                    .ok()
                    .map(|rank| RankedBuildWriterModelPolicyV1 { rank, writer })
            })
    }

    pub fn matching_writer_profile(
        &self,
        model_sha256: &str,
        model_file_bytes: u64,
    ) -> Option<&str> {
        self.matching_writer(model_sha256, model_file_bytes)
            .map(|matched| matched.writer.profile_id.as_str())
    }

    /// Returns a policy profile that is worth identity-checking for a local
    /// file of this size. Size is only a bounded discovery hint; callers must
    /// still prove the exact SHA-256 before treating the model as selected.
    pub fn unverified_size_candidate(
        &self,
        model_file_bytes: u64,
    ) -> Option<RankedBuildWriterModelPolicyV1<'_>> {
        let mut matches = self
            .writers()
            .iter()
            .enumerate()
            .filter(|(_, writer)| writer.model_file_bytes == model_file_bytes);
        let (rank, writer) = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some(RankedBuildWriterModelPolicyV1 {
            rank: u32::try_from(rank).ok()?,
            writer,
        })
    }

    pub fn unverified_size_candidate_profile(&self, model_file_bytes: u64) -> Option<&str> {
        self.unverified_size_candidate(model_file_bytes)
            .map(|candidate| candidate.writer.profile_id.as_str())
    }

    pub fn from_json_slice(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    pub fn canonical_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    pub fn canonical_digest(&self) -> Result<BlobId, serde_json::Error> {
        self.canonical_json().map(|json| BlobId::digest(&json))
    }

    pub const fn identity(&self) -> BuildModelPolicyIdentity {
        self.name().identity()
    }
}

impl Default for BuildModelPolicy {
    fn default() -> Self {
        Self::none_v1()
    }
}

impl Serialize for BuildModelPolicy {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.as_v1().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for BuildModelPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        BuildModelPolicyV1::deserialize(deserializer).map(Self::V1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE_POLICY: &[u8] = include_bytes!("../../../model-policies/none-v1.json");
    const GEMMA_POLICY: &[u8] =
        include_bytes!("../../../model-policies/writer-gemma4-base-v1.json");
    const GEMMA_QUIET_POLICY: &[u8] =
        include_bytes!("../../../model-policies/writer-gemma4-base-v2.json");
    const NONE_CANONICAL_SHA256: &str =
        "ce3bdf5e3dbcac6f7bcc164ec4cc5c78b4a7b5bef7c49b3cd52c61e123b75fe0";
    const GEMMA_CANONICAL_SHA256: &str =
        "c0492fb2285ad0922f89ab7288d63ef68fd17f5133f00ea4276622a15c2dc4e6";
    const GEMMA_QUIET_CANONICAL_SHA256: &str =
        "2d402d213b60ba65c4d018907e9eba67ccfbc1e97081cc0505f9713ae2dd89d2";

    #[test]
    fn checked_in_policies_match_their_exact_typed_contracts() {
        let none = BuildModelPolicy::from_json_slice(NONE_POLICY).expect("parse none policy");
        let gemma = BuildModelPolicy::from_json_slice(GEMMA_POLICY).expect("parse Gemma policy");
        let gemma_quiet = BuildModelPolicy::from_json_slice(GEMMA_QUIET_POLICY)
            .expect("parse quiet Gemma policy");

        assert_eq!(none, BuildModelPolicy::none_v1());
        assert_eq!(gemma, BuildModelPolicy::writer_gemma4_base_v1());
        assert_eq!(gemma_quiet, BuildModelPolicy::writer_gemma4_base_v2());
        assert_eq!(
            DEFAULT_DESKTOP_BUILD_MODEL_POLICY_NAME,
            BuildModelPolicyName::WriterGemma4BaseV2
        );
        assert_eq!(none.activation(), SuggestionActivation::ProjectOptIn);
        assert_eq!(gemma.activation(), SuggestionActivation::ProjectOptIn);
        assert_eq!(gemma_quiet.activation(), SuggestionActivation::QuietDefault);
        assert_eq!(
            GEMMA_4_E2B_BASE_Q8_BLOB_ID.to_string(),
            GEMMA_4_E2B_BASE_Q8_SHA256
        );
        assert!(none.writers().is_empty());
        assert_eq!(gemma.writers().len(), 1);
        assert_eq!(gemma_quiet.writers().len(), 1);
        assert_eq!(
            gemma_quiet.writers()[0].typed_profile_id(),
            BuildWriterProfileId::Gemma4E2bBaseQ8LoomV1
        );
        assert_eq!(
            none.canonical_digest()
                .expect("digest canonical none policy")
                .to_string(),
            NONE_CANONICAL_SHA256
        );
        assert_eq!(
            gemma
                .canonical_digest()
                .expect("digest canonical Gemma policy")
                .to_string(),
            GEMMA_CANONICAL_SHA256
        );
        assert_eq!(
            gemma_quiet
                .canonical_digest()
                .expect("digest canonical quiet Gemma policy")
                .to_string(),
            GEMMA_QUIET_CANONICAL_SHA256
        );
        assert_eq!(
            gemma.matching_writer_profile(
                GEMMA_4_E2B_BASE_Q8_SHA256,
                GEMMA_4_E2B_BASE_Q8_FILE_BYTES
            ),
            Some(GEMMA_4_E2B_BASE_Q8_PROFILE_ID)
        );
        assert_eq!(
            gemma.unverified_size_candidate_profile(GEMMA_4_E2B_BASE_Q8_FILE_BYTES),
            Some(GEMMA_4_E2B_BASE_Q8_PROFILE_ID)
        );
        assert_eq!(gemma.unverified_size_candidate_profile(1), None);

        let quiet_identity = gemma_quiet.identity();
        assert_eq!(
            quiet_identity.name(),
            BuildModelPolicyName::WriterGemma4BaseV2
        );
        assert_eq!(
            quiet_identity.activation(),
            SuggestionActivation::QuietDefault
        );
        assert_eq!(
            quiet_identity.canonical_sha256().to_string(),
            GEMMA_QUIET_CANONICAL_SHA256
        );
        assert_eq!(
            serde_json::to_value(quiet_identity).expect("serialize policy identity"),
            serde_json::json!({
                "name": "writer-gemma4-base-v2",
                "activation": "quiet_default",
                "canonical_sha256": GEMMA_QUIET_CANONICAL_SHA256,
            })
        );
    }

    #[test]
    fn closed_policy_identities_equal_canonical_embedded_json() {
        for policy in [
            BuildModelPolicy::none_v1(),
            BuildModelPolicy::writer_gemma4_base_v1(),
            BuildModelPolicy::writer_gemma4_base_v2(),
        ] {
            let identity = policy.identity();
            assert_eq!(identity.name(), policy.name());
            assert_eq!(identity.activation(), policy.activation());
            assert_eq!(
                identity.canonical_sha256(),
                policy.canonical_digest().expect("canonical policy digest")
            );
        }
    }

    #[test]
    fn policy_parsing_rejects_unknown_fields_and_contract_drift() {
        let with_acquisition = String::from_utf8(GEMMA_POLICY.to_vec())
            .expect("policy is UTF-8")
            .replacen(
                "\"writers\"",
                "\"acquisition\":{\"url\":\"https://example.invalid/model.gguf\"},\"writers\"",
                1,
            );
        assert!(BuildModelPolicy::from_json_slice(with_acquisition.as_bytes()).is_err());

        let wrong_size = String::from_utf8(GEMMA_POLICY.to_vec())
            .expect("policy is UTF-8")
            .replace("4954576032", "4954576031");
        assert!(BuildModelPolicy::from_json_slice(wrong_size.as_bytes()).is_err());

        let unknown_version = String::from_utf8(NONE_POLICY.to_vec())
            .expect("policy is UTF-8")
            .replace("\"schema_version\": 1", "\"schema_version\": 2");
        assert!(BuildModelPolicy::from_json_slice(unknown_version.as_bytes()).is_err());

        let silently_changed_v1 = String::from_utf8(GEMMA_POLICY.to_vec())
            .expect("policy is UTF-8")
            .replace("project_opt_in", "quiet_default");
        assert!(BuildModelPolicy::from_json_slice(silently_changed_v1.as_bytes()).is_err());

        let silently_changed_v2 = String::from_utf8(GEMMA_QUIET_POLICY.to_vec())
            .expect("policy is UTF-8")
            .replace("quiet_default", "project_opt_in");
        assert!(BuildModelPolicy::from_json_slice(silently_changed_v2.as_bytes()).is_err());

        let unknown_profile = String::from_utf8(GEMMA_QUIET_POLICY.to_vec())
            .expect("policy is UTF-8")
            .replace(GEMMA_4_E2B_BASE_Q8_PROFILE_ID, "unreviewed-writer");
        assert!(BuildModelPolicy::from_json_slice(unknown_profile.as_bytes()).is_err());

        let writer_with_path = String::from_utf8(GEMMA_QUIET_POLICY.to_vec())
            .expect("policy is UTF-8")
            .replacen(
                "\"model_file_bytes\"",
                "\"model_path\":\"/builder/model.gguf\",\"model_file_bytes\"",
                1,
            );
        assert!(BuildModelPolicy::from_json_slice(writer_with_path.as_bytes()).is_err());
    }

    #[test]
    fn canonical_policy_bytes_and_digest_ignore_source_formatting() {
        let checked_in = BuildModelPolicy::from_json_slice(GEMMA_POLICY).expect("parse policy");
        let reordered = br#"{
            "writers":[{"model_file_bytes":4954576032,"model_sha256":"aa0a9a03993440f45176f19f8189a2e84c210ff8628ec13dc6edf42d017f7670","prompt_mode":"completion","role":"writer","profile_id":"gemma_4_e2b_base_q8_loom_v1"}],
            "hosted_fallback":"forbidden",
            "inference_boundary":"local_only",
            "activation":"project_opt_in",
            "name":"writer-gemma4-base-v1",
            "schema_version":1,
            "format":"loom-build-model-policy"
        }"#;
        let reordered = BuildModelPolicy::from_json_slice(reordered).expect("parse reordered");

        assert_eq!(
            checked_in.canonical_json().expect("canonical checked-in"),
            reordered.canonical_json().expect("canonical reordered")
        );
        assert_eq!(
            checked_in.canonical_digest().expect("digest checked-in"),
            reordered.canonical_digest().expect("digest reordered")
        );
    }

    #[test]
    fn matching_requires_both_exact_digest_and_exact_size() {
        let policy = BuildModelPolicy::writer_gemma4_base_v1();
        assert!(
            policy
                .matching_writer_profile(GEMMA_4_E2B_BASE_Q8_SHA256, 1)
                .is_none()
        );
        assert!(
            policy
                .matching_writer_profile(
                    "0000000000000000000000000000000000000000000000000000000000000000",
                    GEMMA_4_E2B_BASE_Q8_FILE_BYTES,
                )
                .is_none()
        );
    }
}
