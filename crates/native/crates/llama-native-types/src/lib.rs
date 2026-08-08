use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const MAX_PARALLEL_SEQUENCES: u32 = 4;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeDevice {
    #[default]
    Auto,
    Cpu,
    Metal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NativeModelConfig {
    pub model_id: String,
    pub model_path: PathBuf,
    pub mmproj_path: Option<PathBuf>,
    pub device: NativeDevice,
    pub context_tokens: u32,
    pub batch_tokens: u32,
    pub max_sequences: u32,
    pub gpu_layers: i32,
}

impl NativeModelConfig {
    pub fn local(model_path: PathBuf) -> Self {
        let model_id = model_path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("local-model")
            .to_string();
        Self {
            model_id,
            model_path,
            mmproj_path: None,
            device: NativeDevice::Auto,
            context_tokens: 8192,
            batch_tokens: 512,
            max_sequences: MAX_PARALLEL_SEQUENCES,
            gpu_layers: -1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SamplingConfig {
    pub seed: u32,
    pub temperature: f32,
    #[serde(default)]
    pub dynamic_temperature_range: f32,
    #[serde(default = "default_dynamic_temperature_exponent")]
    pub dynamic_temperature_exponent: f32,
    pub top_k: i32,
    pub top_p: f32,
    pub min_p: f32,
    #[serde(default = "default_typical_p")]
    pub typical_p: f32,
    #[serde(default)]
    pub xtc_probability: f32,
    #[serde(default = "default_xtc_threshold")]
    pub xtc_threshold: f32,
    #[serde(default = "default_repeat_last_n")]
    pub repeat_last_n: i32,
    pub repeat_penalty: f32,
    #[serde(default)]
    pub frequency_penalty: f32,
    #[serde(default)]
    pub presence_penalty: f32,
    #[serde(default)]
    pub dry_multiplier: f32,
    #[serde(default = "default_dry_base")]
    pub dry_base: f32,
    #[serde(default = "default_dry_allowed_length")]
    pub dry_allowed_length: i32,
    #[serde(default = "default_dry_penalty_last_n")]
    pub dry_penalty_last_n: i32,
    #[serde(default = "default_sampler_order")]
    pub sampler_order: Vec<SamplerKind>,
    pub max_tokens: u32,
    pub stop: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SamplerKind {
    Penalties,
    Dry,
    TopK,
    TypicalP,
    TopP,
    MinP,
    Xtc,
    Temperature,
}

impl Default for SamplingConfig {
    fn default() -> Self {
        Self {
            seed: u32::MAX,
            temperature: 0.7,
            dynamic_temperature_range: 0.0,
            dynamic_temperature_exponent: default_dynamic_temperature_exponent(),
            top_k: 40,
            top_p: 0.95,
            min_p: 0.0,
            typical_p: default_typical_p(),
            xtc_probability: 0.0,
            xtc_threshold: default_xtc_threshold(),
            repeat_last_n: default_repeat_last_n(),
            repeat_penalty: 1.0,
            frequency_penalty: 0.0,
            presence_penalty: 0.0,
            dry_multiplier: 0.0,
            dry_base: default_dry_base(),
            dry_allowed_length: default_dry_allowed_length(),
            dry_penalty_last_n: default_dry_penalty_last_n(),
            sampler_order: default_sampler_order(),
            max_tokens: 128,
            stop: Vec::new(),
        }
    }
}

const fn default_dynamic_temperature_exponent() -> f32 {
    1.0
}

const fn default_typical_p() -> f32 {
    1.0
}

const fn default_xtc_threshold() -> f32 {
    0.1
}

const fn default_repeat_last_n() -> i32 {
    64
}

const fn default_dry_base() -> f32 {
    1.75
}

const fn default_dry_allowed_length() -> i32 {
    2
}

const fn default_dry_penalty_last_n() -> i32 {
    -1
}

fn default_sampler_order() -> Vec<SamplerKind> {
    vec![
        SamplerKind::Penalties,
        SamplerKind::Dry,
        SamplerKind::TopK,
        SamplerKind::TypicalP,
        SamplerKind::TopP,
        SamplerKind::MinP,
        SamplerKind::Xtc,
        SamplerKind::Temperature,
    ]
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "template", rename_all = "snake_case")]
pub enum ChatTemplateChoice {
    #[default]
    ModelDefault,
    Override(String),
}

/// The exact prompt semantics for one native generation request.
///
/// Callers must choose a mode. In particular, completion prompts are never
/// passed through a chat template and token prompts are consumed without
/// decoding and re-tokenizing them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GenerationInput {
    Chat {
        messages: Vec<ChatMessage>,
        #[serde(default)]
        template: ChatTemplateChoice,
    },
    Completion {
        prompts: Vec<CompletionPrompt>,
    },
    FillInMiddle {
        prefix: String,
        suffix: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompletionPrompt {
    Text {
        text: String,
        #[serde(default)]
        special_tokens: SpecialTokenPolicy,
    },
    Tokens {
        token_ids: Vec<i32>,
    },
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpecialTokenPolicy {
    #[default]
    NoBosParseSpecial,
    AddBosParseSpecial,
}

impl GenerationInput {
    #[must_use]
    pub const fn kind(&self) -> PromptForm {
        match self {
            Self::Chat { .. } => PromptForm::Chat,
            Self::Completion { .. } => PromptForm::Completion,
            Self::FillInMiddle { .. } => PromptForm::FillInMiddle,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptForm {
    #[default]
    Chat,
    Completion,
    FillInMiddle,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptTokenPolicy {
    #[default]
    ChatTemplate,
    NoBosParseSpecial,
    AddBosParseSpecial,
    ExactTokenIds,
    FillInMiddleModelTokens,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Image,
    Audio,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaInput {
    pub id: String,
    pub kind: MediaKind,
    pub mime: String,
    pub sha256: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GenerationRequest {
    pub request_id: String,
    pub model_id: String,
    pub input: GenerationInput,
    pub sampling: SamplingConfig,
    #[serde(default)]
    pub media: Vec<MediaInput>,
    #[serde(default)]
    pub cached_prefix: Option<SequenceStateBlob>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BranchRequest {
    pub branch_id: String,
    pub label: String,
    pub instruction: String,
    pub sampling: SamplingConfig,
    #[serde(default)]
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub cached_prefix: Option<SequenceStateBlob>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SharedPrefixBatchRequest {
    pub request_id: String,
    pub model_id: String,
    pub common_messages: Vec<ChatMessage>,
    #[serde(default)]
    pub chat_template: ChatTemplateChoice,
    pub branches: Vec<BranchRequest>,
    #[serde(default)]
    pub cached_prefix: Option<SequenceStateBlob>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GenerationState {
    Queued,
    Prefilling,
    Generating,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GenerationEventKind {
    State { state: GenerationState },
    Delta { text: String },
    Warning { code: String, message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GenerationEvent {
    pub request_id: String,
    pub branch_id: String,
    pub sequence_id: i32,
    /// Stable zero-based index of this input in the submitted batch.
    #[serde(default)]
    pub input_index: usize,
    pub event_index: u64,
    pub event: GenerationEventKind,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GenerationMetrics {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub shared_prefix_tokens: usize,
    pub duration_ms: u128,
    pub first_token_ms: Option<u128>,
    pub tokens_per_second: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GenerationOutput {
    pub request_id: String,
    pub branch_id: String,
    /// Stable zero-based index of this output in the submitted batch.
    #[serde(default)]
    pub input_index: usize,
    pub model_id: String,
    pub text: String,
    pub state: GenerationState,
    pub finish_reason: String,
    pub metrics: GenerationMetrics,
    pub real_engine_invoked: bool,
    pub fake_fixture: bool,
    pub transport: NativeTransport,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeTransport {
    InProcess,
    FakeFixture,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelFingerprint {
    pub model_id: String,
    pub model_path: PathBuf,
    pub model_size: u64,
    pub model_sha256: String,
    pub tokenizer_sha256: String,
    pub chat_template_sha256: String,
    pub multimodal_projector_sha256: Option<String>,
    pub binding_version: String,
    pub build_id: String,
    pub backend: String,
    #[serde(default)]
    pub context_tokens: u32,
    #[serde(default)]
    pub batch_tokens: u32,
    #[serde(default)]
    pub max_sequences: u32,
    #[serde(default)]
    pub rope_config_sha256: String,
    #[serde(default)]
    pub kv_layout_sha256: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SamplingParameter {
    Seed,
    Temperature,
    DynamicTemperature,
    TopK,
    TopP,
    MinP,
    TypicalP,
    Xtc,
    RepeatPenalty,
    FrequencyPenalty,
    PresencePenalty,
    Dry,
    SamplerOrder,
    MaxTokens,
    Stop,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub prompt_forms: Vec<PromptForm>,
    pub chat_template_available: bool,
    pub multimodal: bool,
    /// Exact input modalities reported by the loaded multimodal projector.
    /// An empty list means no media is accepted, even when an mmproj path was
    /// configured but could not establish a concrete capability.
    #[serde(default)]
    pub media_kinds: Vec<MediaKind>,
    pub streaming: bool,
    pub cancellation: bool,
    pub max_batch_inputs: u32,
    pub sampling_parameters: Vec<SamplingParameter>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NativeModelDescriptor {
    /// Path-independent content identity suitable for router/cache keys.
    pub stable_model_id: String,
    /// Caller-selected local alias. It is not part of the stable identity.
    pub model_id: String,
    pub display_name: String,
    pub architecture: String,
    pub parameter_count: u64,
    pub model_size: u64,
    pub context_tokens: u32,
    pub max_sequences: u32,
    pub backend: String,
    pub capabilities: ModelCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResidentModelStatus {
    pub model_id: String,
    pub state: ModelRuntimeState,
    pub fingerprint: Option<ModelFingerprint>,
    #[serde(default)]
    pub descriptor: Option<NativeModelDescriptor>,
    pub active_sequences: usize,
    pub max_sequences: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelRuntimeState {
    Loading,
    Ready,
    Failed,
    Stopping,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SequenceStateBlob {
    pub sequence_id: i32,
    pub token_count: usize,
    pub bytes: Vec<u8>,
    #[serde(default)]
    pub token_ids: Vec<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenizedPrompt {
    pub rendered_sha256: String,
    pub token_ids: Vec<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreparedPrompt {
    pub input_index: usize,
    pub prompt_form: PromptForm,
    pub token_policy: PromptTokenPolicy,
    /// Hash of the exact submitted text bytes or little-endian token IDs.
    pub source_sha256: String,
    pub token_ids: Vec<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NativeError {
    pub code: NativeErrorCode,
    pub message: String,
}

impl NativeError {
    pub fn new(code: NativeErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for NativeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for NativeError {}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum NativeErrorCode {
    #[error("invalid_config")]
    InvalidConfig,
    #[error("model_missing")]
    ModelMissing,
    #[error("model_invalid")]
    ModelInvalid,
    #[error("model_load_failed")]
    ModelLoadFailed,
    #[error("model_not_loaded")]
    ModelNotLoaded,
    #[error("model_slots_full")]
    ModelSlotsFull,
    #[error("memory_budget_exceeded")]
    MemoryBudgetExceeded,
    #[error("context_create_failed")]
    ContextCreateFailed,
    #[error("prompt_too_large")]
    PromptTooLarge,
    #[error("unsupported_prompt_form")]
    UnsupportedPromptForm,
    #[error("unsupported_parameter")]
    UnsupportedParameter,
    #[error("unsupported_media")]
    UnsupportedMedia,
    #[error("decode_failed")]
    DecodeFailed,
    #[error("cancelled")]
    Cancelled,
    #[error("worker_stopped")]
    WorkerStopped,
    #[error("cache_incompatible")]
    CacheIncompatible,
    #[error("internal")]
    Internal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_defaults_bound_parallelism() {
        let config = NativeModelConfig::local(PathBuf::from("model.gguf"));
        assert_eq!(config.max_sequences, MAX_PARALLEL_SEQUENCES);
        assert_eq!(config.device, NativeDevice::Auto);
    }

    #[test]
    fn native_error_is_machine_and_human_readable() {
        let error = NativeError::new(NativeErrorCode::ModelMissing, "Choose a model.");
        assert_eq!(error.to_string(), "model_missing: Choose a model.");
    }

    #[test]
    fn legacy_capabilities_default_exact_media_kinds_to_empty() {
        let capabilities: ModelCapabilities = serde_json::from_value(serde_json::json!({
            "prompt_forms": ["chat"],
            "chat_template_available": true,
            "multimodal": true,
            "streaming": true,
            "cancellation": true,
            "max_batch_inputs": 1,
            "sampling_parameters": []
        }))
        .expect("legacy capabilities must remain readable");

        assert!(capabilities.multimodal);
        assert!(capabilities.media_kinds.is_empty());
    }
}
