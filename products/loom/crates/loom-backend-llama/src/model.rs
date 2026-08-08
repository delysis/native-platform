use std::path::{Path, PathBuf};
use std::str::FromStr;

use llama_native_types::{
    CapabilityDeclarationStatus, ExactModelCapabilities, MediaKind, ModelFingerprint, NativeDevice,
    NativeError, NativeErrorCode, NativeModelConfig, NativeModelDescriptor, ProbabilityStage,
    ProjectorRequirement,
};
use loom_types::{BlobId, ModelEnvironmentId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

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
    pub projector_path: Option<PathBuf>,
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
            projector_path: None,
            device: LocalDevicePreference::Auto,
            context_tokens: 8_192,
            batch_tokens: 512,
            max_parallel_cases: llama_native_types::MAX_PARALLEL_SEQUENCES,
            gpu_layers: -1,
        }
    }

    #[must_use]
    pub fn as_native_config(&self) -> NativeModelConfig {
        NativeModelConfig {
            model_id: self.model_id.clone(),
            model_path: self.model_path.clone(),
            mmproj_path: self.projector_path.clone(),
            device: self.device.into(),
            context_tokens: self.context_tokens,
            batch_tokens: self.batch_tokens,
            max_sequences: self.max_parallel_cases,
            gpu_layers: self.gpu_layers,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbabilitySemantics {
    RawModel,
    PostConstraint,
    PostSampler,
}

impl From<ProbabilityStage> for ProbabilitySemantics {
    fn from(value: ProbabilityStage) -> Self {
        match value {
            ProbabilityStage::RawModel => Self::RawModel,
            ProbabilityStage::PostConstraint => Self::PostConstraint,
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
    #[error("model fingerprint path does not match the requested local model path")]
    ModelPathMismatch,
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
        descriptor,
        fingerprint,
    } = inspection;
    validate_identity(profile, &descriptor, &fingerprint)?;
    validate_required_capabilities(&descriptor.capabilities.exact)?;
    validate_inspection_consistency(profile, &descriptor, &fingerprint)?;
    validate_fingerprint_digests(&fingerprint)?;
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
        model_path: fingerprint.model_path,
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
    descriptor: &NativeModelDescriptor,
    fingerprint: &ModelFingerprint,
) -> Result<(), ModelInspectionError> {
    if fingerprint.model_path != profile.model_path {
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
