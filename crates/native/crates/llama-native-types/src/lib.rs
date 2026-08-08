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

/// One independently sampled occurrence in a product-neutral generation batch.
///
/// A case is deliberately not a content identity: callers may submit identical
/// inputs and seeds under different case IDs and receive distinct causal
/// occurrences. Completion inputs must contain exactly one prompt so one case
/// always maps to one ordered output and one cancellation key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GenerationCase {
    pub case_id: String,
    pub input: GenerationInput,
    pub sampling: SamplingConfig,
    #[serde(default)]
    pub cached_prefix: Option<SequenceStateBlob>,
}

/// An ordered family of independently sampled generation cases.
///
/// The engine preserves `cases` order in outputs, detects token-exact prefixes
/// across cases, and exposes each `case_id` as the branch cancellation key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GenerationBatchRequest {
    pub request_id: String,
    pub model_id: String,
    pub cases: Vec<GenerationCase>,
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
    /// Compatibility total for callers predating `cache`.
    ///
    /// This is the number of prompt tokens whose KV work was reused either
    /// from a supplied state or from token-exact sharing inside the batch.
    pub shared_prefix_tokens: usize,
    pub duration_ms: u128,
    pub first_token_ms: Option<u128>,
    pub tokens_per_second: f64,
    #[serde(default)]
    pub cache: GenerationCacheMetrics,
}

/// Per-case cache accounting. Values describe work actually accepted by the
/// engine; an incompatible supplied state fails instead of being counted.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GenerationCacheMetrics {
    /// Token count declared by a supplied sequence state.
    pub supplied_prefix_tokens: usize,
    /// Supplied prefix tokens restored into this case's sequence.
    pub restored_prefix_tokens: usize,
    /// Token-exact prefix tokens decoded once and copied within this batch.
    pub batch_shared_prefix_tokens: usize,
}

/// The probability distribution to which an observation belongs.
///
/// These stages are not interchangeable. In particular, post-sampler values
/// must never be presented as raw-model confidence.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProbabilityStage {
    RawModel,
    PostConstraint,
    PostSampler,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenProbabilityObservation {
    pub stage: ProbabilityStage,
    pub probability: f32,
}

/// Optional evidence about one generated token.
///
/// The current llama.cpp adapter always returns generated token IDs, but only
/// populates this richer record when it can report the declared probability
/// stage without approximation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenObservation {
    pub generated_index: usize,
    pub token_id: i32,
    #[serde(default)]
    pub probabilities: Vec<TokenProbabilityObservation>,
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
    /// Sampled token IDs in generation order. Stop-sequence tokens remain in
    /// this evidence even when the compatibility `text` projection trims the
    /// matching stop suffix.
    #[serde(default)]
    pub generated_token_ids: Vec<i32>,
    /// Absent when the backend cannot provide typed observations honestly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_observations: Option<Vec<TokenObservation>>,
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

/// Whether the exact declaration was populated by an inspecting backend or
/// defaulted while reading an older serialized descriptor.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityDeclarationStatus {
    #[default]
    Unreported,
    Inspected,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptInputCapabilities {
    pub chat: bool,
    pub completion_text: bool,
    pub completion_token_ids: bool,
    /// Present only when the backend has a verified model-specific FIM token
    /// contract. `None` is unsupported or unverified, never an approximation.
    pub fill_in_middle: Option<FillInMiddleCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FillInMiddleCapability {
    pub contract_id: String,
    pub prefix_token_id: i32,
    pub suffix_token_id: i32,
    pub middle_token_id: i32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GenerationOutputCapabilities {
    pub generated_token_ids: bool,
    pub token_observations: bool,
    /// Probability stages the backend can populate without inference or
    /// relabeling. An empty list means probability records are unavailable.
    #[serde(default)]
    pub probability_stages: Vec<ProbabilityStage>,
    /// Log-probability stages available as typed observations. Kept separate
    /// because a probability and its logarithm are different public values.
    #[serde(default)]
    pub log_probability_stages: Vec<ProbabilityStage>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GenerationBatchCapabilities {
    pub max_cases: u32,
    pub ordered_outputs: bool,
    pub per_case_sampling: bool,
    pub per_case_cancellation: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheOperationCapabilities {
    pub sequence_snapshot: bool,
    pub sequence_restore: bool,
    pub per_case_restore: bool,
    pub token_exact_shared_prefix: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectorRequirement {
    Required,
}

/// Exact media facts available from the loaded projector.
///
/// Optional limits and MIME lists remain absent when llama.cpp does not expose
/// a trustworthy fixed contract; absence must not be interpreted as unlimited.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaInputCapability {
    pub kind: MediaKind,
    pub projector: ProjectorRequirement,
    pub accepted_mime_types: Option<Vec<String>>,
    pub max_objects_per_request: Option<u32>,
    pub max_bytes_per_object: Option<u64>,
    pub max_total_bytes_per_request: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExactModelCapabilities {
    pub declaration: CapabilityDeclarationStatus,
    pub prompts: PromptInputCapabilities,
    pub outputs: GenerationOutputCapabilities,
    pub batches: GenerationBatchCapabilities,
    pub cache: CacheOperationCapabilities,
    #[serde(default)]
    pub media: Vec<MediaInputCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub prompt_forms: Vec<PromptForm>,
    pub chat_template_available: bool,
    /// Compatibility summary retained for one deprecation window. New callers
    /// must use `exact.media` instead.
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
    /// Authoritative capability declarations for newly inspected models.
    /// Older serialized descriptors deserialize as `Unreported`.
    #[serde(default)]
    pub exact: ExactModelCapabilities,
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
        assert_eq!(
            capabilities.exact.declaration,
            CapabilityDeclarationStatus::Unreported
        );
    }

    #[test]
    fn probability_observations_preserve_distribution_semantics() {
        let observation = TokenObservation {
            generated_index: 0,
            token_id: 42,
            probabilities: vec![
                TokenProbabilityObservation {
                    stage: ProbabilityStage::RawModel,
                    probability: 0.4,
                },
                TokenProbabilityObservation {
                    stage: ProbabilityStage::PostSampler,
                    probability: 0.7,
                },
            ],
        };

        let json = serde_json::to_value(&observation).expect("serialize token observation");
        assert_eq!(json["probabilities"][0]["stage"], "raw_model");
        assert_eq!(json["probabilities"][1]["stage"], "post_sampler");
    }

    #[test]
    fn generation_batch_round_trip_preserves_case_order_and_exact_tokens() {
        let request = GenerationBatchRequest {
            request_id: "family".to_string(),
            model_id: "model".to_string(),
            cases: vec![
                GenerationCase {
                    case_id: "first".to_string(),
                    input: GenerationInput::Completion {
                        prompts: vec![CompletionPrompt::Tokens {
                            token_ids: vec![1, 2, 3],
                        }],
                    },
                    sampling: SamplingConfig {
                        seed: 10,
                        ..SamplingConfig::default()
                    },
                    cached_prefix: None,
                },
                GenerationCase {
                    case_id: "second".to_string(),
                    input: GenerationInput::Completion {
                        prompts: vec![CompletionPrompt::Text {
                            text: "Exact bytes".to_string(),
                            special_tokens: SpecialTokenPolicy::NoBosParseSpecial,
                        }],
                    },
                    sampling: SamplingConfig {
                        seed: 11,
                        ..SamplingConfig::default()
                    },
                    cached_prefix: None,
                },
            ],
        };

        let json = serde_json::to_string(&request).expect("serialize batch request");
        let decoded: GenerationBatchRequest =
            serde_json::from_str(&json).expect("deserialize batch request");
        assert_eq!(decoded, request);
        assert_eq!(decoded.cases[0].case_id, "first");
        assert_eq!(decoded.cases[0].sampling.seed, 10);
        assert_eq!(decoded.cases[1].case_id, "second");
    }

    #[test]
    fn legacy_generation_output_defaults_new_evidence_fields_to_absent() {
        let output: GenerationOutput = serde_json::from_value(serde_json::json!({
            "request_id": "request",
            "branch_id": "assistant",
            "model_id": "model",
            "text": "answer",
            "state": "completed",
            "finish_reason": "max_tokens",
            "metrics": {
                "prompt_tokens": 2,
                "completion_tokens": 1,
                "shared_prefix_tokens": 0,
                "duration_ms": 10,
                "first_token_ms": 5,
                "tokens_per_second": 100.0
            },
            "real_engine_invoked": true,
            "fake_fixture": false,
            "transport": "in_process"
        }))
        .expect("legacy output must remain readable");

        assert!(output.generated_token_ids.is_empty());
        assert!(output.token_observations.is_none());
        assert_eq!(output.metrics.cache, GenerationCacheMetrics::default());
        assert_eq!(output.input_index, 0);
    }
}
