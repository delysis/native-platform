use std::path::{Path, PathBuf};
use std::str::FromStr;

use llama_native_types::{
    CapabilityDeclarationStatus, ExactModelCapabilities, MediaKind, ModelFingerprint, NativeDevice,
    NativeError, NativeErrorCode, NativeEvidenceCapabilities, NativeModelConfig,
    NativeModelDescriptor, ProbabilityStage, ProjectorRequirement,
};
use loom_types::{BlobId, ModelEnvironmentId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// llama.cpp rejects contexts below 512 cells.  Do not impose a larger product
// floor here: on a memory-starved machine, requesting an unaffordable 8K
// context makes model load less reliable rather than more useful.
const MINIMUM_CONTEXT_TOKENS: u32 = 512;
const DEFAULT_MAXIMUM_CONTEXT_TOKENS: u32 = 262_144;
// Gemma 4 12B allocates both full-attention and SWA caches. The observed
// f16 runtime uses 336 KiB per cell; reserve 384 KiB rather than undercounting
// the SWA cache and consuming system headroom.
const CONSERVATIVE_KV_BYTES_PER_TOKEN: u64 = 384 * 1024;
const MINIMUM_SYSTEM_HEADROOM_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalDevicePreference {
    #[default]
    Auto,
    Cpu,
    Metal,
}

impl From<LocalDevicePreference> for NativeDevice {
    fn from(value: LocalDevicePreference) -> Self {
        match value {
            LocalDevicePreference::Auto => Self::Auto,
            LocalDevicePreference::Cpu => Self::Cpu,
            LocalDevicePreference::Metal => Self::Metal,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocalModelProfile {
    pub model_id: String,
    pub model_path: PathBuf,
    /// Trusted content assertion for the model at `model_path`.
    ///
    /// This is a load-time assertion, not part of resident model identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_model_sha256: Option<String>,
    pub projector_path: Option<PathBuf>,
    /// Trusted content assertion for the projector at `projector_path`.
    ///
    /// This is a load-time assertion, not part of resident model identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_mmproj_sha256: Option<String>,
    pub device: LocalDevicePreference,
    pub context_tokens: u32,
    pub batch_tokens: u32,
    pub max_parallel_cases: u32,
    pub gpu_layers: i32,
}

impl LocalModelProfile {
    #[must_use]
    pub fn for_gguf(model_path: impl Into<PathBuf>) -> Self {
        let model_path = model_path.into();
        let model_id = model_path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("local-model")
            .to_string();
        Self {
            model_id,
            model_path,
            expected_model_sha256: None,
            projector_path: None,
            expected_mmproj_sha256: None,
            device: LocalDevicePreference::Auto,
            context_tokens: 8_192,
            batch_tokens: 512,
            max_parallel_cases: llama_native_types::MAX_PARALLEL_SEQUENCES,
            gpu_layers: -1,
        }
    }

    /// Selects a power-of-two context within system headroom and the host budget.
    /// Native inspection still clamps this request to the GGUF's trained
    /// context. The deliberately conservative KV estimate prevents the old
    /// fixed 8K default from wasting memory that is actually available.
    #[must_use]
    pub fn for_gguf_with_memory(
        model_path: impl Into<PathBuf>,
        model_file_bytes: u64,
        projector_file_bytes: u64,
        available_memory_bytes: u64,
        total_memory_bytes: u64,
        host_memory_budget_bytes: u64,
        maximum_context_tokens: Option<u32>,
    ) -> Self {
        let mut profile = Self::for_gguf(model_path);
        profile.context_tokens = adaptive_context_tokens(
            model_file_bytes,
            projector_file_bytes,
            available_memory_bytes,
            total_memory_bytes,
            host_memory_budget_bytes,
            maximum_context_tokens.unwrap_or(DEFAULT_MAXIMUM_CONTEXT_TOKENS),
        );
        profile
    }

    #[must_use]
    pub fn as_native_config(&self) -> NativeModelConfig {
        NativeModelConfig {
            model_id: self.model_id.clone(),
            model_path: self.model_path.clone(),
            expected_model_sha256: self.expected_model_sha256.clone(),
            mmproj_path: self.projector_path.clone(),
            expected_mmproj_sha256: self.expected_mmproj_sha256.clone(),
            device: self.device.into(),
            context_tokens: self.context_tokens,
            batch_tokens: self.batch_tokens,
            max_sequences: self.max_parallel_cases,
            gpu_layers: self.gpu_layers,
        }
    }
}

#[must_use]
pub fn adaptive_context_tokens(
    model_file_bytes: u64,
    projector_file_bytes: u64,
    available_memory_bytes: u64,
    total_memory_bytes: u64,
    host_memory_budget_bytes: u64,
    maximum_context_tokens: u32,
) -> u32 {
    let maximum = maximum_context_tokens.max(MINIMUM_CONTEXT_TOKENS);
    let runtime_without_kv = model_file_bytes
        .saturating_add((model_file_bytes / 2).max(384 * 1024 * 1024))
        .saturating_add(projector_file_bytes);
    let system_headroom = (total_memory_bytes / 8).max(MINIMUM_SYSTEM_HEADROOM_BYTES);
    // Physical availability never grants permission to exceed the host's
    // admission budget. System headroom belongs outside that host allocation.
    let kv_budget = available_memory_bytes
        .saturating_sub(system_headroom)
        .min(host_memory_budget_bytes)
        .saturating_sub(runtime_without_kv);
    let affordable = (kv_budget / CONSERVATIVE_KV_BYTES_PER_TOKEN).min(u64::from(maximum));
    let affordable = u32::try_from(affordable).unwrap_or(maximum);
    let tier = 1_u32 << affordable.max(MINIMUM_CONTEXT_TOKENS).ilog2();
    tier.clamp(MINIMUM_CONTEXT_TOKENS, maximum)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbabilitySemantics {
    RawModel,
    PostConstraint,
    PostGuidance,
    PostSampler,
}

impl From<ProbabilityStage> for ProbabilitySemantics {
    fn from(value: ProbabilityStage) -> Self {
        match value {
            ProbabilityStage::RawModel => Self::RawModel,
            ProbabilityStage::PostConstraint => Self::PostConstraint,
            ProbabilityStage::PostGuidance => Self::PostGuidance,
            ProbabilityStage::PostSampler => Self::PostSampler,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerifiedMediaCapability {
    pub kind: VerifiedMediaKind,
    pub projector_required: bool,
    pub accepted_mime_types: Option<Vec<String>>,
    pub max_objects_per_request: Option<u32>,
    pub max_bytes_per_object: Option<u64>,
    pub max_total_bytes_per_request: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySupport {
    Supported,
    Unsupported,
}

impl From<bool> for CapabilitySupport {
    fn from(value: bool) -> Self {
        if value {
            Self::Supported
        } else {
            Self::Unsupported
        }
    }
}

impl CapabilitySupport {
    pub const fn is_supported(self) -> bool {
        matches!(self, Self::Supported)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifiedMediaKind {
    Image,
    Audio,
}

impl From<MediaKind> for VerifiedMediaKind {
    fn from(value: MediaKind) -> Self {
        match value {
            MediaKind::Image => Self::Image,
            MediaKind::Audio => Self::Audio,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerifiedCapabilitySet {
    pub chat: CapabilitySupport,
    pub completion_text: CapabilitySupport,
    pub completion_token_ids: CapabilitySupport,
    pub fill_in_middle_contract_id: Option<String>,
    pub generated_token_ids: CapabilitySupport,
    pub token_observations: CapabilitySupport,
    pub probability_stages: Vec<ProbabilitySemantics>,
    pub log_probability_stages: Vec<ProbabilitySemantics>,
    pub max_cases: u32,
    pub ordered_outputs: CapabilitySupport,
    pub per_case_sampling: CapabilitySupport,
    pub per_case_cancellation: CapabilitySupport,
    pub sequence_snapshot: CapabilitySupport,
    pub sequence_restore: CapabilitySupport,
    pub per_case_restore: CapabilitySupport,
    pub token_exact_shared_prefix: CapabilitySupport,
    /// Exact inspected declarations for additive native evidence and controls.
    /// Nested `Unreported` values remain unreported; Loom never promotes them
    /// to unsupported or supported by inference.
    pub evidence: NativeEvidenceCapabilities,
    pub media: Vec<VerifiedMediaCapability>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerifiedModelDescriptor {
    pub model_environment_id: ModelEnvironmentId,
    pub stable_model_id: String,
    pub local_model_id: String,
    pub model_path: PathBuf,
    pub display_name: String,
    pub architecture: Option<String>,
    pub parameter_count: Option<u64>,
    pub model_file_bytes: u64,
    pub model_sha256: String,
    pub tokenizer_sha256: String,
    pub chat_template_sha256: String,
    pub projector_sha256: Option<String>,
    pub binding_version: String,
    pub build_id: String,
    pub backend: String,
    pub context_tokens: u32,
    pub batch_tokens: u32,
    pub max_parallel_cases: u32,
    pub rope_config_sha256: String,
    pub kv_layout_sha256: String,
    pub capabilities: VerifiedCapabilitySet,
}

#[derive(Clone, Debug)]
pub struct RuntimeModelInspection {
    /// Operational location reported by the live resident model.
    ///
    /// Paths are deliberately kept out of [`ModelFingerprint`] so content
    /// identity and cache keys remain independent of installation location.
    pub live_model_path: PathBuf,
    pub descriptor: NativeModelDescriptor,
    pub fingerprint: ModelFingerprint,
}

#[derive(Debug, Error)]
pub enum ModelInspectionError {
    #[error("native model inspection failed: {0}")]
    Native(#[from] NativeError),
    #[error("model descriptor did not include a fingerprint")]
    MissingFingerprint,
    #[error(
        "model identity mismatch: expected `{expected}`, descriptor `{descriptor}`, fingerprint `{fingerprint}`"
    )]
    IdentityMismatch {
        expected: String,
        descriptor: String,
        fingerprint: String,
    },
    #[error(
        "stable model identity `{reported}` does not match inspected model digest `{expected}`"
    )]
    StableIdentityMismatch { reported: String, expected: String },
    #[error("native model capabilities are legacy/unreported, not verified")]
    UnreportedCapabilities,
    #[error(
        "model inspection mismatch for {field}: descriptor `{descriptor}`, fingerprint `{fingerprint}`"
    )]
    InspectionMismatch {
        field: &'static str,
        descriptor: String,
        fingerprint: String,
    },
    #[error("model fingerprint field {field} is not a SHA-256 digest: {source}")]
    InvalidDigest {
        field: &'static str,
        #[source]
        source: loom_types::HashIdParseError,
    },
    #[error("live model path does not match the requested local model path")]
    ModelPathMismatch,
    #[error("configured {field} does not match the inspected content digest")]
    ExpectedDigestMismatch { field: &'static str },
    #[error("projector fingerprint presence does not match the requested projector path")]
    ProjectorMismatch,
    #[error("required raw-completion capability is unavailable: {0}")]
    RequiredCapability(&'static str),
    #[error("failed to canonicalize model environment: {0}")]
    CanonicalEnvironment(#[from] serde_json::Error),
}

pub(crate) fn native_missing_fingerprint() -> NativeError {
    NativeError::new(
        NativeErrorCode::Internal,
        "loaded model did not expose its fingerprint",
    )
}

pub fn verify_model_inspection(
    profile: &LocalModelProfile,
    inspection: RuntimeModelInspection,
) -> Result<VerifiedModelDescriptor, ModelInspectionError> {
    let RuntimeModelInspection {
        live_model_path,
        descriptor,
        fingerprint,
    } = inspection;
    validate_identity(profile, &descriptor, &fingerprint)?;
    validate_required_capabilities(&descriptor.capabilities.exact)?;
    validate_inspection_consistency(profile, &live_model_path, &descriptor, &fingerprint)?;
    validate_fingerprint_digests(&fingerprint)?;
    validate_expected_digests(profile, &fingerprint)?;
    let canonical_environment = serde_json::to_vec(&(&descriptor, &fingerprint))?;
    let model_environment_id = ModelEnvironmentId::digest(&canonical_environment);
    let exact = descriptor.capabilities.exact;
    let capabilities = map_capabilities(exact);
    let architecture = (!descriptor.architecture.trim().is_empty()
        && descriptor.architecture != "unknown")
        .then_some(descriptor.architecture);
    let parameter_count = (descriptor.parameter_count > 0).then_some(descriptor.parameter_count);

    Ok(VerifiedModelDescriptor {
        model_environment_id,
        stable_model_id: descriptor.stable_model_id,
        local_model_id: profile.model_id.clone(),
        model_path: live_model_path,
        display_name: descriptor.display_name,
        architecture,
        parameter_count,
        model_file_bytes: fingerprint.model_size,
        model_sha256: fingerprint.model_sha256,
        tokenizer_sha256: fingerprint.tokenizer_sha256,
        chat_template_sha256: fingerprint.chat_template_sha256,
        projector_sha256: fingerprint.multimodal_projector_sha256,
        binding_version: fingerprint.binding_version,
        build_id: fingerprint.build_id,
        backend: fingerprint.backend,
        context_tokens: fingerprint.context_tokens,
        batch_tokens: fingerprint.batch_tokens,
        max_parallel_cases: fingerprint.max_sequences,
        rope_config_sha256: fingerprint.rope_config_sha256,
        kv_layout_sha256: fingerprint.kv_layout_sha256,
        capabilities,
    })
}

fn validate_identity(
    profile: &LocalModelProfile,
    descriptor: &NativeModelDescriptor,
    fingerprint: &ModelFingerprint,
) -> Result<(), ModelInspectionError> {
    if descriptor.model_id != profile.model_id || fingerprint.model_id != profile.model_id {
        return Err(ModelInspectionError::IdentityMismatch {
            expected: profile.model_id.clone(),
            descriptor: descriptor.model_id.clone(),
            fingerprint: fingerprint.model_id.clone(),
        });
    }
    let expected_stable_model_id = format!("sha256:{}", fingerprint.model_sha256);
    if descriptor.stable_model_id != expected_stable_model_id {
        return Err(ModelInspectionError::StableIdentityMismatch {
            reported: descriptor.stable_model_id.clone(),
            expected: expected_stable_model_id,
        });
    }
    Ok(())
}

fn validate_inspection_consistency(
    profile: &LocalModelProfile,
    live_model_path: &Path,
    descriptor: &NativeModelDescriptor,
    fingerprint: &ModelFingerprint,
) -> Result<(), ModelInspectionError> {
    if live_model_path != profile.model_path {
        return Err(ModelInspectionError::ModelPathMismatch);
    }
    if profile.projector_path.is_some() != fingerprint.multimodal_projector_sha256.is_some() {
        return Err(ModelInspectionError::ProjectorMismatch);
    }
    ensure_inspection_field(
        "model_size",
        &descriptor.model_size,
        &fingerprint.model_size,
    )?;
    ensure_inspection_field(
        "context_tokens",
        &descriptor.context_tokens,
        &fingerprint.context_tokens,
    )?;
    ensure_inspection_field(
        "max_sequences",
        &descriptor.max_sequences,
        &fingerprint.max_sequences,
    )?;
    ensure_inspection_field("backend", &descriptor.backend, &fingerprint.backend)?;
    ensure_inspection_field(
        "capabilities.max_cases",
        &descriptor.capabilities.exact.batches.max_cases,
        &descriptor.max_sequences,
    )?;
    Ok(())
}

fn validate_expected_digests(
    profile: &LocalModelProfile,
    fingerprint: &ModelFingerprint,
) -> Result<(), ModelInspectionError> {
    if profile
        .expected_model_sha256
        .as_deref()
        .is_some_and(|expected| expected != fingerprint.model_sha256)
    {
        return Err(ModelInspectionError::ExpectedDigestMismatch {
            field: "expected_model_sha256",
        });
    }
    if profile
        .expected_mmproj_sha256
        .as_deref()
        .is_some_and(|expected| {
            fingerprint.multimodal_projector_sha256.as_deref() != Some(expected)
        })
    {
        return Err(ModelInspectionError::ExpectedDigestMismatch {
            field: "expected_mmproj_sha256",
        });
    }
    Ok(())
}

fn ensure_inspection_field<T>(
    field: &'static str,
    descriptor: &T,
    fingerprint: &T,
) -> Result<(), ModelInspectionError>
where
    T: std::fmt::Display + PartialEq + ?Sized,
{
    if descriptor == fingerprint {
        return Ok(());
    }
    Err(ModelInspectionError::InspectionMismatch {
        field,
        descriptor: descriptor.to_string(),
        fingerprint: fingerprint.to_string(),
    })
}

fn validate_fingerprint_digests(
    fingerprint: &ModelFingerprint,
) -> Result<(), ModelInspectionError> {
    validate_digest("model_sha256", &fingerprint.model_sha256)?;
    validate_digest("tokenizer_sha256", &fingerprint.tokenizer_sha256)?;
    validate_digest("chat_template_sha256", &fingerprint.chat_template_sha256)?;
    validate_digest("rope_config_sha256", &fingerprint.rope_config_sha256)?;
    validate_digest("kv_layout_sha256", &fingerprint.kv_layout_sha256)?;
    if let Some(projector) = &fingerprint.multimodal_projector_sha256 {
        validate_digest("multimodal_projector_sha256", projector)?;
    }
    Ok(())
}

fn validate_digest(field: &'static str, digest: &str) -> Result<(), ModelInspectionError> {
    BlobId::from_str(digest)
        .map(|_| ())
        .map_err(|source| ModelInspectionError::InvalidDigest { field, source })
}

fn validate_required_capabilities(
    exact: &ExactModelCapabilities,
) -> Result<(), ModelInspectionError> {
    if exact.declaration != CapabilityDeclarationStatus::Inspected {
        return Err(ModelInspectionError::UnreportedCapabilities);
    }
    if !exact.prompts.completion_text {
        return Err(ModelInspectionError::RequiredCapability(
            "exact completion text",
        ));
    }
    if !exact.outputs.generated_token_ids {
        return Err(ModelInspectionError::RequiredCapability(
            "generated token IDs",
        ));
    }
    if !exact.batches.ordered_outputs {
        return Err(ModelInspectionError::RequiredCapability("ordered outputs"));
    }
    if !exact.batches.per_case_sampling {
        return Err(ModelInspectionError::RequiredCapability(
            "per-case sampling",
        ));
    }
    if !exact.batches.per_case_cancellation {
        return Err(ModelInspectionError::RequiredCapability(
            "per-case cancellation",
        ));
    }
    if exact.batches.max_cases == 0 {
        return Err(ModelInspectionError::RequiredCapability(
            "positive batch case limit",
        ));
    }
    Ok(())
}

fn map_capabilities(exact: ExactModelCapabilities) -> VerifiedCapabilitySet {
    VerifiedCapabilitySet {
        chat: exact.prompts.chat.into(),
        completion_text: exact.prompts.completion_text.into(),
        completion_token_ids: exact.prompts.completion_token_ids.into(),
        fill_in_middle_contract_id: exact
            .prompts
            .fill_in_middle
            .map(|contract| contract.contract_id),
        generated_token_ids: exact.outputs.generated_token_ids.into(),
        token_observations: exact.outputs.token_observations.into(),
        probability_stages: exact
            .outputs
            .probability_stages
            .into_iter()
            .map(Into::into)
            .collect(),
        log_probability_stages: exact
            .outputs
            .log_probability_stages
            .into_iter()
            .map(Into::into)
            .collect(),
        max_cases: exact.batches.max_cases,
        ordered_outputs: exact.batches.ordered_outputs.into(),
        per_case_sampling: exact.batches.per_case_sampling.into(),
        per_case_cancellation: exact.batches.per_case_cancellation.into(),
        sequence_snapshot: exact.cache.sequence_snapshot.into(),
        sequence_restore: exact.cache.sequence_restore.into(),
        per_case_restore: exact.cache.per_case_restore.into(),
        token_exact_shared_prefix: exact.cache.token_exact_shared_prefix.into(),
        evidence: exact.evidence,
        media: exact
            .media
            .into_iter()
            .map(|media| VerifiedMediaCapability {
                kind: media.kind.into(),
                projector_required: media.projector == ProjectorRequirement::Required,
                accepted_mime_types: media.accepted_mime_types,
                max_objects_per_request: media.max_objects_per_request,
                max_bytes_per_object: media.max_bytes_per_object,
                max_total_bytes_per_request: media.max_total_bytes_per_request,
            })
            .collect(),
    }
}

#[must_use]
pub fn is_gguf_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gguf"))
}

#[cfg(test)]
mod context_tests {
    use super::*;

    #[test]
    fn default_gemma_context_fits_host_budget_on_a_large_memory_machine() {
        let gib = 1024_u64 * 1024 * 1024;
        // Official Gemma 4 12B QAT model and projector artifact sizes.
        let model_bytes = 6_975_879_296;
        let projector_bytes = 175_115_616;
        let host_budget = llama_native_host::NativeHostConfig::default().memory_budget_bytes;
        let profile = LocalModelProfile::for_gguf_with_memory(
            "unloaded-gemma4.gguf",
            model_bytes,
            projector_bytes,
            90 * gib,
            96 * gib,
            host_budget,
            Some(262_144),
        );
        let estimate = llama_native_engine::estimate_memory_reservation(
            &profile.as_native_config(),
            model_bytes,
            projector_bytes,
        );
        assert_eq!(
            estimate.basis,
            llama_native_engine::MemoryEstimateBasis::ConfigurationHeuristic
        );
        assert!(
            estimate.total_bytes <= host_budget,
            "{} context cells reserve {} bytes above the {} byte host budget",
            profile.context_tokens,
            estimate.total_bytes,
            host_budget,
        );
        assert_eq!(profile.context_tokens, 4_096);
    }

    #[test]
    fn context_selection_scales_in_power_of_two_tiers_and_respects_model_limit() {
        let gib = 1024_u64 * 1024 * 1024;
        assert_eq!(
            adaptive_context_tokens(7 * gib, 0, 14 * gib, 16 * gib, u64::MAX, 262_144),
            4_096
        );
        assert_eq!(
            adaptive_context_tokens(7 * gib, 0, 48 * gib, 64 * gib, u64::MAX, 262_144),
            65_536
        );
        assert_eq!(
            adaptive_context_tokens(7 * gib, 0, 56 * gib, 64 * gib, u64::MAX, 262_144),
            65_536
        );
        assert_eq!(
            adaptive_context_tokens(7 * gib, 0, 112 * gib, 128 * gib, u64::MAX, 262_144),
            131_072
        );
        assert_eq!(
            adaptive_context_tokens(7 * gib, 0, 112 * gib, 128 * gib, u64::MAX, 32_768),
            32_768
        );
    }

    #[test]
    fn context_selection_does_not_force_eight_k_when_memory_cannot_afford_it() {
        let gib = 1024_u64 * 1024 * 1024;
        assert_eq!(
            adaptive_context_tokens(7 * gib, 0, 8 * gib, 16 * gib, u64::MAX, 262_144),
            512
        );
    }

    #[test]
    fn context_selection_honors_a_trained_limit_below_eight_k() {
        let gib = 1024_u64 * 1024 * 1024;
        assert_eq!(
            adaptive_context_tokens(gib, 0, 48 * gib, 64 * gib, u64::MAX, 4_096),
            4_096
        );
    }
}
