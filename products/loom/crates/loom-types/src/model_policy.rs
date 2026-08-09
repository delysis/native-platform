use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{BlobId, ModelRole, PromptMode};

pub const BUILD_MODEL_POLICY_SCHEMA_VERSION: u32 = 1;
pub const GEMMA_4_E2B_BASE_Q8_PROFILE_ID: &str = "gemma_4_e2b_base_q8_loom_v1";
pub const GEMMA_4_E2B_BASE_Q8_SHA256: &str =
    "aa0a9a03993440f45176f19f8189a2e84c210ff8628ec13dc6edf42d017f7670";
pub const GEMMA_4_E2B_BASE_Q8_FILE_BYTES: u64 = 4_954_576_032;

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
}

impl BuildModelPolicyName {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoneV1 => "none-v1",
            Self::WriterGemma4BaseV1 => "writer-gemma4-base-v1",
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
    profile_id: String,
    role: ModelRole,
    prompt_mode: PromptMode,
    model_sha256: BlobId,
    model_file_bytes: u64,
}

impl BuildWriterModelPolicyV1 {
    pub fn profile_id(&self) -> &str {
        &self.profile_id
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

    fn writer_gemma4_base() -> Self {
        Self {
            format: BuildModelPolicyFormat::LoomBuildModelPolicy,
            schema_version: BUILD_MODEL_POLICY_SCHEMA_VERSION,
            name: BuildModelPolicyName::WriterGemma4BaseV1,
            activation: SuggestionActivation::ProjectOptIn,
            inference_boundary: InferenceBoundary::LocalOnly,
            hosted_fallback: HostedFallback::Forbidden,
            writers: vec![BuildWriterModelPolicyV1 {
                profile_id: GEMMA_4_E2B_BASE_Q8_PROFILE_ID.to_owned(),
                role: ModelRole::Writer,
                prompt_mode: PromptMode::Completion,
                model_sha256: GEMMA_4_E2B_BASE_Q8_SHA256
                    .parse()
                    .expect("the source-controlled Gemma policy digest is valid"),
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
            BuildModelPolicyName::WriterGemma4BaseV1 => Self::writer_gemma4_base(),
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

impl BuildModelPolicy {
    pub fn none_v1() -> Self {
        Self::V1(BuildModelPolicyV1::none())
    }

    pub fn writer_gemma4_base_v1() -> Self {
        Self::V1(BuildModelPolicyV1::writer_gemma4_base())
    }

    pub const fn as_v1(&self) -> &BuildModelPolicyV1 {
        match self {
            Self::V1(policy) => policy,
        }
    }

    pub const fn name(&self) -> BuildModelPolicyName {
        self.as_v1().name()
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
            .find(|(_, writer)| writer.profile_id == profile_id)
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
    const NONE_CANONICAL_SHA256: &str =
        "ce3bdf5e3dbcac6f7bcc164ec4cc5c78b4a7b5bef7c49b3cd52c61e123b75fe0";
    const GEMMA_CANONICAL_SHA256: &str =
        "c0492fb2285ad0922f89ab7288d63ef68fd17f5133f00ea4276622a15c2dc4e6";

    #[test]
    fn checked_in_policies_match_their_exact_typed_contracts() {
        let none = BuildModelPolicy::from_json_slice(NONE_POLICY).expect("parse none policy");
        let gemma = BuildModelPolicy::from_json_slice(GEMMA_POLICY).expect("parse Gemma policy");

        assert_eq!(none, BuildModelPolicy::none_v1());
        assert_eq!(gemma, BuildModelPolicy::writer_gemma4_base_v1());
        assert!(none.writers().is_empty());
        assert_eq!(gemma.writers().len(), 1);
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
