use serde::{Deserialize, Serialize};
use std::path::PathBuf;

mod controlled_generation;
mod exact_token_budget;
mod sampling_fingerprint;

pub use controlled_generation::*;
pub use exact_token_budget::*;
pub use sampling_fingerprint::{SAMPLING_CONFIG_FINGERPRINT_DOMAIN, SamplingConfigFingerprint};

pub const MAX_PARALLEL_SEQUENCES: u32 = 4;
pub const MAX_EMBEDDING_BATCH_INPUTS: usize = 64;
pub const MAX_EMBEDDING_INPUT_TOKENS: usize = 262_144;
pub const MAX_EMBEDDING_BATCH_TOKENS: usize = 1_048_576;
pub const MAX_EMBEDDING_DIMENSIONS: u32 = 65_536;
pub const MAX_EMBEDDING_VALUES_PER_OUTPUT: usize = 4_194_304;
pub const MAX_EMBEDDING_BATCH_VALUES: usize = 16_777_216;
pub const MAX_CONSTRAINT_ARTIFACT_BYTES: u32 = 4 * 1024 * 1024;
pub const MAX_DISTRIBUTION_OBSERVATION_TOP_K: u16 = 256;
pub const MAX_DISTRIBUTION_OBSERVATIONS_PER_TOKEN: usize = 12;
pub const MAX_TOKEN_PIECE_BYTES: usize = 4_096;
/// Maximum UTF-8 bytes retained for one generated output before stop trimming.
///
/// This fixed process-wide ceiling bounds both output strings and the retained
/// strict-authority delta ledger. It is not caller-controlled; changing it
/// changes the native source/build fingerprint.
pub const MAX_GENERATED_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_SPARSE_LOGIT_BIAS_ENTRIES: usize = 4_096;
pub const MAX_EXTENDED_SAMPLERS: usize = 8;
pub const MAX_ABS_LOGIT_BIAS: f32 = 100.0;
pub const MAX_TOP_N_SIGMA: f32 = 100.0;
pub const MAX_MIROSTAT_V1_FILTER_WINDOW: i32 = 262_144;

const MAX_DTO_ID_BYTES: usize = 256;
const MAX_MIROSTAT_TARGET_SURPRISE: f32 = 100.0;

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
    /// Optional trusted digest supplied by the caller. When present, the held
    /// file is hashed before the llama.cpp load attempt and a mismatch aborts
    /// that attempt. On platforms with advisory file locking, later artifact
    /// checks determine strict-authority eligibility; this field alone does not
    /// claim that an uncooperative writer could not race the parser.
    #[serde(default)]
    pub expected_model_sha256: Option<String>,
    pub mmproj_path: Option<PathBuf>,
    /// Optional trusted digest for the multimodal projector, with the same
    /// advisory-lock limitations as `expected_model_sha256`.
    #[serde(default)]
    pub expected_mmproj_sha256: Option<String>,
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
            expected_model_sha256: None,
            mmproj_path: None,
            expected_mmproj_sha256: None,
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

/// Version-one Mirostat parameters.
///
/// Construction and deserialization reject non-finite and out-of-contract
/// values. The type is inert until a backend explicitly declares and applies
/// the matching extended sampler.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(try_from = "MirostatV1ConfigWire")]
pub struct MirostatV1Config {
    tau: f32,
    eta: f32,
    m: i32,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct MirostatV1ConfigWire {
    tau: f32,
    eta: f32,
    m: i32,
}

impl MirostatV1Config {
    pub fn new(tau: f32, eta: f32, m: i32) -> Result<Self, NativeError> {
        validate_finite_inclusive(
            "mirostat_v1.tau",
            tau,
            f32::MIN_POSITIVE,
            MAX_MIROSTAT_TARGET_SURPRISE,
        )?;
        validate_finite_inclusive("mirostat_v1.eta", eta, f32::MIN_POSITIVE, 1.0)?;
        if !(1..=MAX_MIROSTAT_V1_FILTER_WINDOW).contains(&m) {
            return Err(invalid_config(format!(
                "mirostat_v1.m must be between 1 and {MAX_MIROSTAT_V1_FILTER_WINDOW}"
            )));
        }
        Ok(Self { tau, eta, m })
    }

    #[must_use]
    pub const fn tau(self) -> f32 {
        self.tau
    }

    #[must_use]
    pub const fn eta(self) -> f32 {
        self.eta
    }

    #[must_use]
    pub const fn m(self) -> i32 {
        self.m
    }
}

impl TryFrom<MirostatV1ConfigWire> for MirostatV1Config {
    type Error = NativeError;

    fn try_from(value: MirostatV1ConfigWire) -> Result<Self, Self::Error> {
        Self::new(value.tau, value.eta, value.m)
    }
}

/// Version-two Mirostat parameters.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(try_from = "MirostatV2ConfigWire")]
pub struct MirostatV2Config {
    tau: f32,
    eta: f32,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct MirostatV2ConfigWire {
    tau: f32,
    eta: f32,
}

impl MirostatV2Config {
    pub fn new(tau: f32, eta: f32) -> Result<Self, NativeError> {
        validate_finite_inclusive(
            "mirostat_v2.tau",
            tau,
            f32::MIN_POSITIVE,
            MAX_MIROSTAT_TARGET_SURPRISE,
        )?;
        validate_finite_inclusive("mirostat_v2.eta", eta, f32::MIN_POSITIVE, 1.0)?;
        Ok(Self { tau, eta })
    }

    #[must_use]
    pub const fn tau(self) -> f32 {
        self.tau
    }

    #[must_use]
    pub const fn eta(self) -> f32 {
        self.eta
    }
}

impl TryFrom<MirostatV2ConfigWire> for MirostatV2Config {
    type Error = NativeError;

    fn try_from(value: MirostatV2ConfigWire) -> Result<Self, Self::Error> {
        Self::new(value.tau, value.eta)
    }
}

/// Entropy-adaptive eta cutoff in the open interval `(0, 1]`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(try_from = "f32")]
pub struct EtaCutoff(f32);

impl EtaCutoff {
    pub fn new(cutoff: f32) -> Result<Self, NativeError> {
        validate_finite_inclusive("eta_cutoff", cutoff, f32::MIN_POSITIVE, 1.0)?;
        Ok(Self(cutoff))
    }

    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

impl TryFrom<f32> for EtaCutoff {
    type Error = NativeError;

    fn try_from(value: f32) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// Top-n-sigma threshold expressed as a positive number of standard deviations.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(try_from = "f32")]
pub struct TopNSigma(f32);

impl TopNSigma {
    pub fn new(sigma: f32) -> Result<Self, NativeError> {
        validate_finite_inclusive("top_n_sigma", sigma, f32::MIN_POSITIVE, MAX_TOP_N_SIGMA)?;
        Ok(Self(sigma))
    }

    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

impl TryFrom<f32> for TopNSigma {
    type Error = NativeError;

    fn try_from(value: f32) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct TokenLogitBias {
    pub token_id: i32,
    pub bias: f32,
}

/// A bounded, duplicate-free sparse token-to-logit map.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(try_from = "Vec<TokenLogitBias>")]
pub struct SparseLogitBias(Vec<TokenLogitBias>);

impl SparseLogitBias {
    pub fn new(entries: Vec<TokenLogitBias>) -> Result<Self, NativeError> {
        if entries.len() > MAX_SPARSE_LOGIT_BIAS_ENTRIES {
            return Err(invalid_config(format!(
                "sparse_logit_bias cannot contain more than {MAX_SPARSE_LOGIT_BIAS_ENTRIES} entries"
            )));
        }
        let mut token_ids = std::collections::BTreeSet::new();
        for entry in &entries {
            if entry.token_id < 0 {
                return Err(invalid_config(
                    "sparse_logit_bias token IDs must be non-negative",
                ));
            }
            if !entry.bias.is_finite() || entry.bias.abs() > MAX_ABS_LOGIT_BIAS {
                return Err(invalid_config(format!(
                    "sparse_logit_bias values must be finite and between -{MAX_ABS_LOGIT_BIAS} and {MAX_ABS_LOGIT_BIAS}"
                )));
            }
            if !token_ids.insert(entry.token_id) {
                return Err(invalid_config(format!(
                    "sparse_logit_bias contains duplicate token ID {}",
                    entry.token_id
                )));
            }
        }
        Ok(Self(entries))
    }

    #[must_use]
    pub fn as_slice(&self) -> &[TokenLogitBias] {
        &self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl TryFrom<Vec<TokenLogitBias>> for SparseLogitBias {
    type Error = NativeError;

    fn try_from(value: Vec<TokenLogitBias>) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// Product-neutral extended sampling operation.
///
/// This deliberately does not alter `SamplingConfig`: existing backends keep
/// their exact behavior until they opt into a separate, capability-checked
/// extended-sampling path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExtendedSampler {
    MirostatV1 { config: MirostatV1Config },
    MirostatV2 { config: MirostatV2Config },
    EtaCutoff { cutoff: EtaCutoff },
    SparseLogitBias { biases: SparseLogitBias },
    TopNSigma { sigma: TopNSigma },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum ExtendedSamplerKind {
    MirostatV1,
    MirostatV2,
    EtaCutoff,
    SparseLogitBias,
    TopNSigma,
}

impl ExtendedSampler {
    #[must_use]
    pub const fn kind(&self) -> ExtendedSamplerKind {
        match self {
            Self::MirostatV1 { .. } => ExtendedSamplerKind::MirostatV1,
            Self::MirostatV2 { .. } => ExtendedSamplerKind::MirostatV2,
            Self::EtaCutoff { .. } => ExtendedSamplerKind::EtaCutoff,
            Self::SparseLogitBias { .. } => ExtendedSamplerKind::SparseLogitBias,
            Self::TopNSigma { .. } => ExtendedSamplerKind::TopNSigma,
        }
    }
}

/// An ordered, bounded extended-sampler program.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(try_from = "Vec<ExtendedSampler>")]
pub struct ExtendedSamplerProgram(Vec<ExtendedSampler>);

impl ExtendedSamplerProgram {
    pub fn new(samplers: Vec<ExtendedSampler>) -> Result<Self, NativeError> {
        if samplers.len() > MAX_EXTENDED_SAMPLERS {
            return Err(invalid_config(format!(
                "extended sampler program cannot contain more than {MAX_EXTENDED_SAMPLERS} operations"
            )));
        }
        let mut kinds = std::collections::BTreeSet::new();
        let mut has_mirostat = false;
        for (index, sampler) in samplers.iter().enumerate() {
            let kind = sampler.kind();
            if !kinds.insert(kind) {
                return Err(invalid_config(format!(
                    "extended sampler program contains duplicate {kind:?} operation"
                )));
            }
            if matches!(
                kind,
                ExtendedSamplerKind::MirostatV1 | ExtendedSamplerKind::MirostatV2
            ) {
                if has_mirostat {
                    return Err(invalid_config(
                        "extended sampler program cannot combine Mirostat v1 and v2",
                    ));
                }
                has_mirostat = true;
                if index + 1 != samplers.len() {
                    return Err(invalid_config(
                        "Mirostat is a terminal selector and must be the final extended sampler operation",
                    ));
                }
            }
            if let ExtendedSampler::SparseLogitBias { biases } = sampler
                && biases.is_empty()
            {
                return Err(invalid_config(
                    "sparse logit bias operation must contain at least one entry",
                ));
            }
            if let ExtendedSampler::SparseLogitBias { biases } = sampler {
                if index != 0 {
                    return Err(invalid_config(
                        "sparse logit bias must be the first extended sampler so it cannot be hidden behind truncation",
                    ));
                }
                if biases.as_slice().iter().any(|entry| entry.bias == 0.0) {
                    return Err(invalid_config("sparse logit bias entries must be non-zero"));
                }
            }
        }
        Ok(Self(samplers))
    }

    #[must_use]
    pub fn as_slice(&self) -> &[ExtendedSampler] {
        &self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl TryFrom<Vec<ExtendedSampler>> for ExtendedSamplerProgram {
    type Error = NativeError;

    fn try_from(value: Vec<ExtendedSampler>) -> Result<Self, Self::Error> {
        Self::new(value)
    }
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
    PostGuidance,
    PostSampler,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum DistributionValueKind {
    Logit,
    Probability,
    LogProbability,
}

/// A duplicate-free ordered set of requested distribution stages.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "Vec<ProbabilityStage>")]
pub struct ProbabilityStageSet(Vec<ProbabilityStage>);

impl ProbabilityStageSet {
    pub fn new(stages: Vec<ProbabilityStage>) -> Result<Self, NativeError> {
        let mut seen = std::collections::HashSet::new();
        for stage in &stages {
            if !seen.insert(*stage) {
                return Err(invalid_config(format!(
                    "distribution observation policy contains duplicate {stage:?} stage"
                )));
            }
        }
        Ok(Self(stages))
    }

    #[must_use]
    pub fn as_slice(&self) -> &[ProbabilityStage] {
        &self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl TryFrom<Vec<ProbabilityStage>> for ProbabilityStageSet {
    type Error = NativeError;

    fn try_from(value: Vec<ProbabilityStage>) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// A duplicate-free ordered set of requested numeric representations.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "Vec<DistributionValueKind>")]
pub struct DistributionValueKindSet(Vec<DistributionValueKind>);

impl DistributionValueKindSet {
    pub fn new(values: Vec<DistributionValueKind>) -> Result<Self, NativeError> {
        let mut seen = std::collections::BTreeSet::new();
        for value in &values {
            if !seen.insert(*value) {
                return Err(invalid_config(format!(
                    "distribution observation policy contains duplicate {value:?} value kind"
                )));
            }
        }
        Ok(Self(values))
    }

    #[must_use]
    pub fn as_slice(&self) -> &[DistributionValueKind] {
        &self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl TryFrom<Vec<DistributionValueKind>> for DistributionValueKindSet {
    type Error = NativeError;

    fn try_from(value: Vec<DistributionValueKind>) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// Bounded request for causal distribution evidence.
///
/// `top_k` is the maximum number of globally ranked finite-support candidates
/// requested. A constrained distribution may honestly return fewer. A selected
/// token observation is carried separately so sampling may choose a token
/// outside that set. The default requests no evidence; absence must never be
/// interpreted as backend support.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "DistributionObservationPolicyWire")]
pub struct DistributionObservationPolicy {
    stages: ProbabilityStageSet,
    value_kinds: DistributionValueKindSet,
    include_selected_token: bool,
    top_k: u16,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DistributionObservationPolicyWire {
    #[serde(default)]
    stages: ProbabilityStageSet,
    #[serde(default)]
    value_kinds: DistributionValueKindSet,
    #[serde(default)]
    include_selected_token: bool,
    #[serde(default)]
    top_k: u16,
}

impl DistributionObservationPolicy {
    pub fn new(
        stages: ProbabilityStageSet,
        value_kinds: DistributionValueKindSet,
        include_selected_token: bool,
        top_k: u16,
    ) -> Result<Self, NativeError> {
        if top_k > MAX_DISTRIBUTION_OBSERVATION_TOP_K {
            return Err(invalid_config(format!(
                "distribution observation top_k cannot exceed {MAX_DISTRIBUTION_OBSERVATION_TOP_K}"
            )));
        }
        let enabled = include_selected_token || top_k > 0;
        if enabled && stages.is_empty() {
            return Err(invalid_config(
                "enabled distribution observations require at least one declared stage",
            ));
        }
        if enabled && value_kinds.is_empty() {
            return Err(invalid_config(
                "enabled distribution observations require at least one value kind",
            ));
        }
        if !enabled && (!stages.is_empty() || !value_kinds.is_empty()) {
            return Err(invalid_config(
                "disabled distribution observations cannot declare stages or value kinds",
            ));
        }
        Ok(Self {
            stages,
            value_kinds,
            include_selected_token,
            top_k,
        })
    }

    #[must_use]
    pub fn stages(&self) -> &[ProbabilityStage] {
        self.stages.as_slice()
    }

    #[must_use]
    pub fn value_kinds(&self) -> &[DistributionValueKind] {
        self.value_kinds.as_slice()
    }

    #[must_use]
    pub const fn include_selected_token(&self) -> bool {
        self.include_selected_token
    }

    #[must_use]
    pub const fn top_k(&self) -> u16 {
        self.top_k
    }

    #[must_use]
    pub fn is_disabled(&self) -> bool {
        !self.include_selected_token && self.top_k == 0
    }
}

impl TryFrom<DistributionObservationPolicyWire> for DistributionObservationPolicy {
    type Error = NativeError;

    fn try_from(value: DistributionObservationPolicyWire) -> Result<Self, Self::Error> {
        Self::new(
            value.stages,
            value.value_kinds,
            value.include_selected_token,
            value.top_k,
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingPooling {
    None,
    Mean,
    Cls,
    Last,
    Rank,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingNormalization {
    None,
    L2,
}

/// One exact-token embedding input.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "EmbeddingInputWire")]
pub struct EmbeddingInput {
    input_id: String,
    token_ids: Vec<i32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddingInputWire {
    input_id: String,
    token_ids: Vec<i32>,
}

impl EmbeddingInput {
    pub fn new(input_id: String, token_ids: Vec<i32>) -> Result<Self, NativeError> {
        validate_id("embedding input_id", &input_id)?;
        if token_ids.is_empty() {
            return Err(invalid_config(
                "embedding input must contain at least one exact token ID",
            ));
        }
        if token_ids.len() > MAX_EMBEDDING_INPUT_TOKENS {
            return Err(invalid_config(format!(
                "embedding input cannot exceed {MAX_EMBEDDING_INPUT_TOKENS} token IDs"
            )));
        }
        if token_ids.iter().any(|token_id| *token_id < 0) {
            return Err(invalid_config(
                "embedding input token IDs must be non-negative",
            ));
        }
        Ok(Self {
            input_id,
            token_ids,
        })
    }

    #[must_use]
    pub fn input_id(&self) -> &str {
        &self.input_id
    }

    #[must_use]
    pub fn token_ids(&self) -> &[i32] {
        &self.token_ids
    }
}

impl TryFrom<EmbeddingInputWire> for EmbeddingInput {
    type Error = NativeError;

    fn try_from(value: EmbeddingInputWire) -> Result<Self, Self::Error> {
        Self::new(value.input_id, value.token_ids)
    }
}

/// Ordered batch of exact-token embedding inputs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "EmbeddingBatchRequestWire")]
pub struct EmbeddingBatchRequest {
    request_id: String,
    model_id: String,
    inputs: Vec<EmbeddingInput>,
    pooling: EmbeddingPooling,
    normalization: EmbeddingNormalization,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddingBatchRequestWire {
    request_id: String,
    model_id: String,
    inputs: Vec<EmbeddingInput>,
    pooling: EmbeddingPooling,
    normalization: EmbeddingNormalization,
}

impl EmbeddingBatchRequest {
    pub fn new(
        request_id: String,
        model_id: String,
        inputs: Vec<EmbeddingInput>,
        pooling: EmbeddingPooling,
        normalization: EmbeddingNormalization,
    ) -> Result<Self, NativeError> {
        validate_id("embedding request_id", &request_id)?;
        validate_id("embedding model_id", &model_id)?;
        validate_embedding_inputs(&inputs)?;
        Ok(Self {
            request_id,
            model_id,
            inputs,
            pooling,
            normalization,
        })
    }

    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    #[must_use]
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    #[must_use]
    pub fn inputs(&self) -> &[EmbeddingInput] {
        &self.inputs
    }

    #[must_use]
    pub const fn pooling(&self) -> EmbeddingPooling {
        self.pooling
    }

    #[must_use]
    pub const fn normalization(&self) -> EmbeddingNormalization {
        self.normalization
    }
}

impl TryFrom<EmbeddingBatchRequestWire> for EmbeddingBatchRequest {
    type Error = NativeError;

    fn try_from(value: EmbeddingBatchRequestWire) -> Result<Self, Self::Error> {
        Self::new(
            value.request_id,
            value.model_id,
            value.inputs,
            value.pooling,
            value.normalization,
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "EmbeddingOutputConfigWire")]
pub struct EmbeddingOutputConfig {
    pooling: EmbeddingPooling,
    normalization: EmbeddingNormalization,
    dimensions: u32,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddingOutputConfigWire {
    pooling: EmbeddingPooling,
    normalization: EmbeddingNormalization,
    dimensions: u32,
}

impl EmbeddingOutputConfig {
    pub fn new(
        pooling: EmbeddingPooling,
        normalization: EmbeddingNormalization,
        dimensions: u32,
    ) -> Result<Self, NativeError> {
        if dimensions == 0 || dimensions > MAX_EMBEDDING_DIMENSIONS {
            return Err(invalid_config(format!(
                "embedding dimensions must be between 1 and {MAX_EMBEDDING_DIMENSIONS}"
            )));
        }
        Ok(Self {
            pooling,
            normalization,
            dimensions,
        })
    }

    #[must_use]
    pub const fn pooling(self) -> EmbeddingPooling {
        self.pooling
    }

    #[must_use]
    pub const fn normalization(self) -> EmbeddingNormalization {
        self.normalization
    }

    #[must_use]
    pub const fn dimensions(self) -> u32 {
        self.dimensions
    }
}

impl TryFrom<EmbeddingOutputConfigWire> for EmbeddingOutputConfig {
    type Error = NativeError;

    fn try_from(value: EmbeddingOutputConfigWire) -> Result<Self, Self::Error> {
        Self::new(value.pooling, value.normalization, value.dimensions)
    }
}

/// Flattened row-major embedding result for one exact-token input.
///
/// Pooled results have one row. Unpooled results have one row per input token.
/// `EmbeddingBatchOutput` validates the shape against its declared dimensions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(try_from = "EmbeddingVectorOutputWire")]
pub struct EmbeddingVectorOutput {
    input_id: String,
    input_index: usize,
    token_ids: Vec<i32>,
    row_count: u32,
    values: Vec<f32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddingVectorOutputWire {
    input_id: String,
    input_index: usize,
    token_ids: Vec<i32>,
    row_count: u32,
    values: Vec<f32>,
}

impl EmbeddingVectorOutput {
    pub fn new(
        input_id: String,
        input_index: usize,
        token_ids: Vec<i32>,
        row_count: u32,
        values: Vec<f32>,
    ) -> Result<Self, NativeError> {
        EmbeddingInput::new(input_id.clone(), token_ids.clone())?;
        if row_count == 0 {
            return Err(invalid_config(
                "embedding output row_count must be greater than zero",
            ));
        }
        if values.is_empty() || values.len() > MAX_EMBEDDING_VALUES_PER_OUTPUT {
            return Err(invalid_config(format!(
                "embedding output values must contain between 1 and {MAX_EMBEDDING_VALUES_PER_OUTPUT} elements"
            )));
        }
        if values.iter().any(|value| !value.is_finite()) {
            return Err(invalid_config("embedding output values must be finite"));
        }
        Ok(Self {
            input_id,
            input_index,
            token_ids,
            row_count,
            values,
        })
    }

    #[must_use]
    pub fn input_id(&self) -> &str {
        &self.input_id
    }

    #[must_use]
    pub const fn input_index(&self) -> usize {
        self.input_index
    }

    #[must_use]
    pub fn token_ids(&self) -> &[i32] {
        &self.token_ids
    }

    #[must_use]
    pub const fn row_count(&self) -> u32 {
        self.row_count
    }

    #[must_use]
    pub fn values(&self) -> &[f32] {
        &self.values
    }
}

impl TryFrom<EmbeddingVectorOutputWire> for EmbeddingVectorOutput {
    type Error = NativeError;

    fn try_from(value: EmbeddingVectorOutputWire) -> Result<Self, Self::Error> {
        Self::new(
            value.input_id,
            value.input_index,
            value.token_ids,
            value.row_count,
            value.values,
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "EmbeddingTransportEvidenceWire")]
pub struct EmbeddingTransportEvidence {
    transport: NativeTransport,
    real_engine_invoked: bool,
    fake_fixture: bool,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddingTransportEvidenceWire {
    transport: NativeTransport,
    real_engine_invoked: bool,
    fake_fixture: bool,
}

impl EmbeddingTransportEvidence {
    pub fn new(
        transport: NativeTransport,
        real_engine_invoked: bool,
        fake_fixture: bool,
    ) -> Result<Self, NativeError> {
        let coherent = match transport {
            NativeTransport::InProcess => real_engine_invoked && !fake_fixture,
            NativeTransport::FakeFixture => !real_engine_invoked && fake_fixture,
        };
        if !coherent {
            return Err(invalid_config(
                "embedding transport and engine/fixture evidence disagree",
            ));
        }
        Ok(Self {
            transport,
            real_engine_invoked,
            fake_fixture,
        })
    }

    #[must_use]
    pub const fn transport(self) -> NativeTransport {
        self.transport
    }

    #[must_use]
    pub const fn real_engine_invoked(self) -> bool {
        self.real_engine_invoked
    }

    #[must_use]
    pub const fn fake_fixture(self) -> bool {
        self.fake_fixture
    }
}

impl TryFrom<EmbeddingTransportEvidenceWire> for EmbeddingTransportEvidence {
    type Error = NativeError;

    fn try_from(value: EmbeddingTransportEvidenceWire) -> Result<Self, Self::Error> {
        Self::new(
            value.transport,
            value.real_engine_invoked,
            value.fake_fixture,
        )
    }
}

/// Bounded embedding output carrying exact input tokens and runtime identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(try_from = "EmbeddingBatchOutputWire")]
pub struct EmbeddingBatchOutput {
    request_id: String,
    model_id: String,
    config: EmbeddingOutputConfig,
    outputs: Vec<EmbeddingVectorOutput>,
    model_fingerprint: ModelFingerprint,
    evidence: EmbeddingTransportEvidence,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddingBatchOutputWire {
    request_id: String,
    model_id: String,
    config: EmbeddingOutputConfig,
    outputs: Vec<EmbeddingVectorOutput>,
    model_fingerprint: ModelFingerprint,
    evidence: EmbeddingTransportEvidence,
}

impl EmbeddingBatchOutput {
    pub fn new(
        request_id: String,
        model_id: String,
        config: EmbeddingOutputConfig,
        outputs: Vec<EmbeddingVectorOutput>,
        model_fingerprint: ModelFingerprint,
        evidence: EmbeddingTransportEvidence,
    ) -> Result<Self, NativeError> {
        validate_id("embedding output request_id", &request_id)?;
        validate_id("embedding output model_id", &model_id)?;
        validate_embedding_outputs(config, &outputs)?;
        Ok(Self {
            request_id,
            model_id,
            config,
            outputs,
            model_fingerprint,
            evidence,
        })
    }

    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    #[must_use]
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    #[must_use]
    pub const fn config(&self) -> EmbeddingOutputConfig {
        self.config
    }

    #[must_use]
    pub fn outputs(&self) -> &[EmbeddingVectorOutput] {
        &self.outputs
    }

    #[must_use]
    pub const fn model_fingerprint(&self) -> &ModelFingerprint {
        &self.model_fingerprint
    }

    #[must_use]
    pub const fn evidence(&self) -> EmbeddingTransportEvidence {
        self.evidence
    }
}

impl TryFrom<EmbeddingBatchOutputWire> for EmbeddingBatchOutput {
    type Error = NativeError;

    fn try_from(value: EmbeddingBatchOutputWire) -> Result<Self, Self::Error> {
        Self::new(
            value.request_id,
            value.model_id,
            value.config,
            value.outputs,
            value.model_fingerprint,
            value.evidence,
        )
    }
}

/// Content-addressed reference to a structured constraint artifact.
///
/// The grammar or schema body is intentionally absent from this DTO. Products
/// resolve the reference through their own bounded immutable artifact store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "ConstraintArtifactReferenceWire")]
pub struct ConstraintArtifactReference {
    artifact_id: String,
    sha256: String,
    byte_len: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConstraintArtifactReferenceWire {
    artifact_id: String,
    sha256: String,
    byte_len: u32,
}

impl ConstraintArtifactReference {
    pub fn new(artifact_id: String, sha256: String, byte_len: u32) -> Result<Self, NativeError> {
        validate_id("constraint artifact_id", &artifact_id)?;
        validate_sha256("constraint sha256", &sha256)?;
        if byte_len == 0 || byte_len > MAX_CONSTRAINT_ARTIFACT_BYTES {
            return Err(invalid_config(format!(
                "constraint artifact byte_len must be between 1 and {MAX_CONSTRAINT_ARTIFACT_BYTES}"
            )));
        }
        Ok(Self {
            artifact_id,
            sha256,
            byte_len,
        })
    }

    #[must_use]
    pub fn artifact_id(&self) -> &str {
        &self.artifact_id
    }

    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    #[must_use]
    pub const fn byte_len(&self) -> u32 {
        self.byte_len
    }
}

impl TryFrom<ConstraintArtifactReferenceWire> for ConstraintArtifactReference {
    type Error = NativeError;

    fn try_from(value: ConstraintArtifactReferenceWire) -> Result<Self, Self::Error> {
        Self::new(value.artifact_id, value.sha256, value.byte_len)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StructuredConstraintKind {
    Gbnf,
    JsonSchema,
}

/// A structured-output constraint by immutable artifact reference.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StructuredConstraint {
    Gbnf {
        reference: ConstraintArtifactReference,
    },
    JsonSchema {
        reference: ConstraintArtifactReference,
    },
}

impl StructuredConstraint {
    #[must_use]
    pub const fn kind(&self) -> StructuredConstraintKind {
        match self {
            Self::Gbnf { .. } => StructuredConstraintKind::Gbnf,
            Self::JsonSchema { .. } => StructuredConstraintKind::JsonSchema,
        }
    }

    #[must_use]
    pub const fn reference(&self) -> &ConstraintArtifactReference {
        match self {
            Self::Gbnf { reference } | Self::JsonSchema { reference } => reference,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenProbabilityObservation {
    pub stage: ProbabilityStage,
    pub probability: f32,
}

/// One finite value for a token at a declared distribution stage.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(try_from = "DistributionTokenValueWire")]
pub struct DistributionTokenValue {
    token_id: i32,
    token_bytes: Option<Vec<u8>>,
    value: f32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DistributionTokenValueWire {
    token_id: i32,
    #[serde(default)]
    token_bytes: Option<Vec<u8>>,
    value: f32,
}

impl DistributionTokenValue {
    pub fn new(
        token_id: i32,
        token_bytes: Option<Vec<u8>>,
        value: f32,
    ) -> Result<Self, NativeError> {
        if token_id < 0 {
            return Err(invalid_config(
                "distribution observation token IDs must be non-negative",
            ));
        }
        if token_bytes
            .as_ref()
            .is_some_and(|bytes| bytes.len() > MAX_TOKEN_PIECE_BYTES)
        {
            return Err(invalid_config(format!(
                "distribution observation token bytes cannot exceed {MAX_TOKEN_PIECE_BYTES} bytes"
            )));
        }
        if !value.is_finite() {
            return Err(invalid_config(
                "distribution observation values must be finite",
            ));
        }
        Ok(Self {
            token_id,
            token_bytes,
            value,
        })
    }

    #[must_use]
    pub const fn token_id(&self) -> i32 {
        self.token_id
    }

    #[must_use]
    pub fn token_bytes(&self) -> Option<&[u8]> {
        self.token_bytes.as_deref()
    }

    #[must_use]
    pub const fn value(&self) -> f32 {
        self.value
    }
}

impl TryFrom<DistributionTokenValueWire> for DistributionTokenValue {
    type Error = NativeError;

    fn try_from(value: DistributionTokenValueWire) -> Result<Self, Self::Error> {
        Self::new(value.token_id, value.token_bytes, value.value)
    }
}

/// One candidate at its true one-based rank in the observed distribution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(try_from = "RankedDistributionCandidateWire")]
pub struct RankedDistributionCandidate {
    rank: u16,
    token: DistributionTokenValue,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RankedDistributionCandidateWire {
    rank: u16,
    token: DistributionTokenValue,
}

impl RankedDistributionCandidate {
    pub fn new(rank: u16, token: DistributionTokenValue) -> Result<Self, NativeError> {
        if rank == 0 || rank > MAX_DISTRIBUTION_OBSERVATION_TOP_K {
            return Err(invalid_config(format!(
                "distribution candidate rank must be between 1 and {MAX_DISTRIBUTION_OBSERVATION_TOP_K}"
            )));
        }
        Ok(Self { rank, token })
    }

    #[must_use]
    pub const fn rank(&self) -> u16 {
        self.rank
    }

    #[must_use]
    pub const fn token(&self) -> &DistributionTokenValue {
        &self.token
    }
}

impl TryFrom<RankedDistributionCandidateWire> for RankedDistributionCandidate {
    type Error = NativeError;

    fn try_from(value: RankedDistributionCandidateWire) -> Result<Self, Self::Error> {
        Self::new(value.rank, value.token)
    }
}

/// Selected-token value plus bounded globally ranked candidates at one stage.
///
/// The selected token appears in `ranked_candidates` exactly when it belongs to
/// the reported top-k, with identical bytes and value. It remains separate so
/// an outside-top-k sampled token is never lost.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(try_from = "StageDistributionObservationWire")]
pub struct StageDistributionObservation {
    stage: ProbabilityStage,
    value_kind: DistributionValueKind,
    selected: DistributionTokenValue,
    ranked_candidates: Vec<RankedDistributionCandidate>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct StageDistributionObservationWire {
    stage: ProbabilityStage,
    value_kind: DistributionValueKind,
    selected: DistributionTokenValue,
    #[serde(default)]
    ranked_candidates: Vec<RankedDistributionCandidate>,
}

impl StageDistributionObservation {
    pub fn new(
        stage: ProbabilityStage,
        value_kind: DistributionValueKind,
        selected: DistributionTokenValue,
        ranked_candidates: Vec<RankedDistributionCandidate>,
    ) -> Result<Self, NativeError> {
        if ranked_candidates.len() > usize::from(MAX_DISTRIBUTION_OBSERVATION_TOP_K) {
            return Err(invalid_config(format!(
                "distribution observation cannot contain more than {MAX_DISTRIBUTION_OBSERVATION_TOP_K} ranked candidates"
            )));
        }
        validate_distribution_value(value_kind, selected.value())?;
        let mut token_ids = std::collections::HashSet::new();
        let mut previous_value: Option<f32> = None;
        let mut selected_is_ranked = false;
        for (index, candidate) in ranked_candidates.iter().enumerate() {
            let expected_rank = u16::try_from(index + 1)
                .map_err(|_| invalid_config("distribution candidate rank overflow"))?;
            if candidate.rank() != expected_rank {
                return Err(invalid_config(
                    "distribution candidates must have contiguous global one-based ranks",
                ));
            }
            if !token_ids.insert(candidate.token().token_id()) {
                return Err(invalid_config(
                    "distribution observation contains a duplicate token ID",
                ));
            }
            validate_distribution_value(value_kind, candidate.token().value())?;
            if candidate.token().token_id() == selected.token_id() && candidate.token() != &selected
            {
                return Err(invalid_config(
                    "ranked selected-token evidence must exactly match its separate value and bytes",
                ));
            }
            selected_is_ranked |= candidate.token().token_id() == selected.token_id();
            if previous_value.is_some_and(|value| candidate.token().value() > value) {
                return Err(invalid_config(
                    "distribution candidates must be ordered by descending value",
                ));
            }
            previous_value = Some(candidate.token().value());
        }
        if !selected_is_ranked
            && previous_value.is_some_and(|last_value| selected.value() > last_value)
        {
            return Err(invalid_config(
                "selected token exceeds the final ranked candidate and cannot be omitted from top-k evidence",
            ));
        }
        Ok(Self {
            stage,
            value_kind,
            selected,
            ranked_candidates,
        })
    }

    #[must_use]
    pub const fn stage(&self) -> ProbabilityStage {
        self.stage
    }

    #[must_use]
    pub const fn value_kind(&self) -> DistributionValueKind {
        self.value_kind
    }

    #[must_use]
    pub const fn selected(&self) -> &DistributionTokenValue {
        &self.selected
    }

    #[must_use]
    pub fn ranked_candidates(&self) -> &[RankedDistributionCandidate] {
        &self.ranked_candidates
    }
}

impl TryFrom<StageDistributionObservationWire> for StageDistributionObservation {
    type Error = NativeError;

    fn try_from(value: StageDistributionObservationWire) -> Result<Self, Self::Error> {
        Self::new(
            value.stage,
            value.value_kind,
            value.selected,
            value.ranked_candidates,
        )
    }
}

/// Evidence-rich counterpart to the legacy `TokenObservation`.
///
/// It repeats `generated_index` and `token_id` so callers can join it to a
/// `GenerationOutput` without changing the legacy DTO or relabeling the
/// distribution stage. Each stage/value-kind pair may appear at most once.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(try_from = "TokenDistributionObservationWire")]
pub struct TokenDistributionObservation {
    generated_index: usize,
    token_id: i32,
    observations: Vec<StageDistributionObservation>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenDistributionObservationWire {
    generated_index: usize,
    token_id: i32,
    observations: Vec<StageDistributionObservation>,
}

impl TokenDistributionObservation {
    pub fn new(
        generated_index: usize,
        token_id: i32,
        observations: Vec<StageDistributionObservation>,
    ) -> Result<Self, NativeError> {
        if token_id < 0 {
            return Err(invalid_config(
                "selected distribution observation token ID must be non-negative",
            ));
        }
        if observations.is_empty() || observations.len() > MAX_DISTRIBUTION_OBSERVATIONS_PER_TOKEN {
            return Err(invalid_config(format!(
                "token distribution evidence must contain between 1 and {MAX_DISTRIBUTION_OBSERVATIONS_PER_TOKEN} observations"
            )));
        }
        let mut stage_value_pairs = std::collections::HashSet::new();
        let mut known_token_bytes = std::collections::BTreeMap::<i32, &[u8]>::new();
        for observation in &observations {
            if observation.selected().token_id() != token_id {
                return Err(invalid_config(
                    "stage observation selected token does not match its causal token ID",
                ));
            }
            if !stage_value_pairs.insert((observation.stage(), observation.value_kind())) {
                return Err(invalid_config(
                    "token distribution evidence contains a duplicate stage/value-kind pair",
                ));
            }
            validate_consistent_token_bytes(&mut known_token_bytes, observation.selected())?;
            for candidate in observation.ranked_candidates() {
                validate_consistent_token_bytes(&mut known_token_bytes, candidate.token())?;
            }
        }
        Ok(Self {
            generated_index,
            token_id,
            observations,
        })
    }

    #[must_use]
    pub const fn generated_index(&self) -> usize {
        self.generated_index
    }

    #[must_use]
    pub const fn token_id(&self) -> i32 {
        self.token_id
    }

    #[must_use]
    pub fn observations(&self) -> &[StageDistributionObservation] {
        &self.observations
    }
}

impl TryFrom<TokenDistributionObservationWire> for TokenDistributionObservation {
    type Error = NativeError;

    fn try_from(value: TokenDistributionObservationWire) -> Result<Self, Self::Error> {
        Self::new(value.generated_index, value.token_id, value.observations)
    }
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

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmbeddingPoolingSupport {
    pub none: bool,
    pub mean: bool,
    pub cls: bool,
    pub last: bool,
    pub rank: bool,
}

impl EmbeddingPoolingSupport {
    #[must_use]
    pub const fn supports(self, pooling: EmbeddingPooling) -> bool {
        match pooling {
            EmbeddingPooling::None => self.none,
            EmbeddingPooling::Mean => self.mean,
            EmbeddingPooling::Cls => self.cls,
            EmbeddingPooling::Last => self.last,
            EmbeddingPooling::Rank => self.rank,
        }
    }

    const fn any(self) -> bool {
        self.none || self.mean || self.cls || self.last || self.rank
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmbeddingNormalizationSupport {
    pub none: bool,
    pub l2: bool,
}

impl EmbeddingNormalizationSupport {
    #[must_use]
    pub const fn supports(self, normalization: EmbeddingNormalization) -> bool {
        match normalization {
            EmbeddingNormalization::None => self.none,
            EmbeddingNormalization::L2 => self.l2,
        }
    }

    const fn any(self) -> bool {
        self.none || self.l2
    }
}

/// Inspected embedding capability facts. Default is explicitly unreported.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "EmbeddingCapabilitiesWire")]
pub struct EmbeddingCapabilities {
    declaration: CapabilityDeclarationStatus,
    pooling: EmbeddingPoolingSupport,
    normalization: EmbeddingNormalizationSupport,
    max_batch_inputs: Option<u16>,
    max_input_tokens: Option<u32>,
    dimensions: Option<u32>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddingCapabilitiesWire {
    #[serde(default)]
    declaration: CapabilityDeclarationStatus,
    #[serde(default)]
    pooling: EmbeddingPoolingSupport,
    #[serde(default)]
    normalization: EmbeddingNormalizationSupport,
    #[serde(default)]
    max_batch_inputs: Option<u16>,
    #[serde(default)]
    max_input_tokens: Option<u32>,
    #[serde(default)]
    dimensions: Option<u32>,
}

impl EmbeddingCapabilities {
    pub fn new(
        declaration: CapabilityDeclarationStatus,
        pooling: EmbeddingPoolingSupport,
        normalization: EmbeddingNormalizationSupport,
        max_batch_inputs: Option<u16>,
        max_input_tokens: Option<u32>,
        dimensions: Option<u32>,
    ) -> Result<Self, NativeError> {
        let has_details = pooling.any()
            || normalization.any()
            || max_batch_inputs.is_some()
            || max_input_tokens.is_some()
            || dimensions.is_some();
        reject_details_when_unreported("embedding", declaration, has_details)?;
        if max_batch_inputs
            .is_some_and(|value| value == 0 || usize::from(value) > MAX_EMBEDDING_BATCH_INPUTS)
        {
            return Err(invalid_config(format!(
                "embedding max_batch_inputs must be between 1 and {MAX_EMBEDDING_BATCH_INPUTS}"
            )));
        }
        if max_input_tokens
            .is_some_and(|value| value == 0 || value as usize > MAX_EMBEDDING_INPUT_TOKENS)
        {
            return Err(invalid_config(format!(
                "embedding max_input_tokens must be between 1 and {MAX_EMBEDDING_INPUT_TOKENS}"
            )));
        }
        if dimensions.is_some_and(|value| value == 0 || value > MAX_EMBEDDING_DIMENSIONS) {
            return Err(invalid_config(format!(
                "embedding dimensions must be between 1 and {MAX_EMBEDDING_DIMENSIONS}"
            )));
        }
        Ok(Self {
            declaration,
            pooling,
            normalization,
            max_batch_inputs,
            max_input_tokens,
            dimensions,
        })
    }

    #[must_use]
    pub const fn declaration(self) -> CapabilityDeclarationStatus {
        self.declaration
    }

    #[must_use]
    pub const fn pooling(self) -> EmbeddingPoolingSupport {
        self.pooling
    }

    #[must_use]
    pub const fn normalization(self) -> EmbeddingNormalizationSupport {
        self.normalization
    }

    #[must_use]
    pub const fn max_batch_inputs(self) -> Option<u16> {
        self.max_batch_inputs
    }

    #[must_use]
    pub const fn max_input_tokens(self) -> Option<u32> {
        self.max_input_tokens
    }

    #[must_use]
    pub const fn dimensions(self) -> Option<u32> {
        self.dimensions
    }
}

impl Default for EmbeddingCapabilities {
    fn default() -> Self {
        Self {
            declaration: CapabilityDeclarationStatus::Unreported,
            pooling: EmbeddingPoolingSupport::default(),
            normalization: EmbeddingNormalizationSupport::default(),
            max_batch_inputs: None,
            max_input_tokens: None,
            dimensions: None,
        }
    }
}

impl TryFrom<EmbeddingCapabilitiesWire> for EmbeddingCapabilities {
    type Error = NativeError;

    fn try_from(value: EmbeddingCapabilitiesWire) -> Result<Self, Self::Error> {
        Self::new(
            value.declaration,
            value.pooling,
            value.normalization,
            value.max_batch_inputs,
            value.max_input_tokens,
            value.dimensions,
        )
    }
}

/// Inspected structured-constraint support. Default is unreported, not false.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "StructuredConstraintCapabilitiesWire")]
pub struct StructuredConstraintCapabilities {
    declaration: CapabilityDeclarationStatus,
    gbnf: bool,
    json_schema: bool,
    max_artifact_bytes: Option<u32>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct StructuredConstraintCapabilitiesWire {
    #[serde(default)]
    declaration: CapabilityDeclarationStatus,
    #[serde(default)]
    gbnf: bool,
    #[serde(default)]
    json_schema: bool,
    #[serde(default)]
    max_artifact_bytes: Option<u32>,
}

impl StructuredConstraintCapabilities {
    pub fn new(
        declaration: CapabilityDeclarationStatus,
        gbnf: bool,
        json_schema: bool,
        max_artifact_bytes: Option<u32>,
    ) -> Result<Self, NativeError> {
        reject_details_when_unreported(
            "structured constraint",
            declaration,
            gbnf || json_schema || max_artifact_bytes.is_some(),
        )?;
        if max_artifact_bytes
            .is_some_and(|value| value == 0 || value > MAX_CONSTRAINT_ARTIFACT_BYTES)
        {
            return Err(invalid_config(format!(
                "constraint max_artifact_bytes must be between 1 and {MAX_CONSTRAINT_ARTIFACT_BYTES}"
            )));
        }
        if !gbnf && !json_schema && max_artifact_bytes.is_some() {
            return Err(invalid_config(
                "constraint size limit cannot be declared when no format is supported",
            ));
        }
        Ok(Self {
            declaration,
            gbnf,
            json_schema,
            max_artifact_bytes,
        })
    }

    #[must_use]
    pub const fn declaration(self) -> CapabilityDeclarationStatus {
        self.declaration
    }

    #[must_use]
    pub const fn supports(self, kind: StructuredConstraintKind) -> bool {
        match kind {
            StructuredConstraintKind::Gbnf => self.gbnf,
            StructuredConstraintKind::JsonSchema => self.json_schema,
        }
    }

    #[must_use]
    pub const fn max_artifact_bytes(self) -> Option<u32> {
        self.max_artifact_bytes
    }
}

impl Default for StructuredConstraintCapabilities {
    fn default() -> Self {
        Self {
            declaration: CapabilityDeclarationStatus::Unreported,
            gbnf: false,
            json_schema: false,
            max_artifact_bytes: None,
        }
    }
}

impl TryFrom<StructuredConstraintCapabilitiesWire> for StructuredConstraintCapabilities {
    type Error = NativeError;

    fn try_from(value: StructuredConstraintCapabilitiesWire) -> Result<Self, Self::Error> {
        Self::new(
            value.declaration,
            value.gbnf,
            value.json_schema,
            value.max_artifact_bytes,
        )
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProbabilityStageSupport {
    pub raw_model: bool,
    pub post_constraint: bool,
    pub post_guidance: bool,
    pub post_sampler: bool,
}

impl ProbabilityStageSupport {
    #[must_use]
    pub const fn supports(self, stage: ProbabilityStage) -> bool {
        match stage {
            ProbabilityStage::RawModel => self.raw_model,
            ProbabilityStage::PostConstraint => self.post_constraint,
            ProbabilityStage::PostGuidance => self.post_guidance,
            ProbabilityStage::PostSampler => self.post_sampler,
        }
    }

    const fn any(self) -> bool {
        self.raw_model || self.post_constraint || self.post_guidance || self.post_sampler
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DistributionValueKindSupport {
    pub logits: bool,
    pub probabilities: bool,
    pub log_probabilities: bool,
}

impl DistributionValueKindSupport {
    #[must_use]
    pub const fn supports(self, kind: DistributionValueKind) -> bool {
        match kind {
            DistributionValueKind::Logit => self.logits,
            DistributionValueKind::Probability => self.probabilities,
            DistributionValueKind::LogProbability => self.log_probabilities,
        }
    }

    const fn any(self) -> bool {
        self.logits || self.probabilities || self.log_probabilities
    }
}

/// Inspected evidence availability. Default is explicitly unreported.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "DistributionObservationCapabilitiesWire")]
pub struct DistributionObservationCapabilities {
    declaration: CapabilityDeclarationStatus,
    stages: ProbabilityStageSupport,
    value_kinds: DistributionValueKindSupport,
    selected_token: bool,
    max_top_k: Option<u16>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct DistributionObservationCapabilitiesWire {
    #[serde(default)]
    declaration: CapabilityDeclarationStatus,
    #[serde(default)]
    stages: ProbabilityStageSupport,
    #[serde(default)]
    value_kinds: DistributionValueKindSupport,
    #[serde(default)]
    selected_token: bool,
    #[serde(default)]
    max_top_k: Option<u16>,
}

impl DistributionObservationCapabilities {
    pub fn new(
        declaration: CapabilityDeclarationStatus,
        stages: ProbabilityStageSupport,
        value_kinds: DistributionValueKindSupport,
        selected_token: bool,
        max_top_k: Option<u16>,
    ) -> Result<Self, NativeError> {
        reject_details_when_unreported(
            "distribution observation",
            declaration,
            stages.any() || value_kinds.any() || selected_token || max_top_k.is_some(),
        )?;
        if max_top_k.is_some_and(|value| value > MAX_DISTRIBUTION_OBSERVATION_TOP_K) {
            return Err(invalid_config(format!(
                "distribution observation max_top_k cannot exceed {MAX_DISTRIBUTION_OBSERVATION_TOP_K}"
            )));
        }
        if (selected_token || max_top_k.is_some_and(|value| value > 0))
            && (!stages.any() || !value_kinds.any())
        {
            return Err(invalid_config(
                "distribution evidence support requires at least one stage and value kind",
            ));
        }
        Ok(Self {
            declaration,
            stages,
            value_kinds,
            selected_token,
            max_top_k,
        })
    }

    #[must_use]
    pub const fn declaration(self) -> CapabilityDeclarationStatus {
        self.declaration
    }

    #[must_use]
    pub const fn stages(self) -> ProbabilityStageSupport {
        self.stages
    }

    #[must_use]
    pub const fn value_kinds(self) -> DistributionValueKindSupport {
        self.value_kinds
    }

    #[must_use]
    pub const fn selected_token(self) -> bool {
        self.selected_token
    }

    #[must_use]
    pub const fn max_top_k(self) -> Option<u16> {
        self.max_top_k
    }
}

impl Default for DistributionObservationCapabilities {
    fn default() -> Self {
        Self {
            declaration: CapabilityDeclarationStatus::Unreported,
            stages: ProbabilityStageSupport::default(),
            value_kinds: DistributionValueKindSupport::default(),
            selected_token: false,
            max_top_k: None,
        }
    }
}

impl TryFrom<DistributionObservationCapabilitiesWire> for DistributionObservationCapabilities {
    type Error = NativeError;

    fn try_from(value: DistributionObservationCapabilitiesWire) -> Result<Self, Self::Error> {
        Self::new(
            value.declaration,
            value.stages,
            value.value_kinds,
            value.selected_token,
            value.max_top_k,
        )
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtendedSamplerSupport {
    pub mirostat_v1: bool,
    pub mirostat_v2: bool,
    pub eta_cutoff: bool,
    pub sparse_logit_bias: bool,
    pub top_n_sigma: bool,
}

impl ExtendedSamplerSupport {
    #[must_use]
    pub const fn supports(self, kind: ExtendedSamplerKind) -> bool {
        match kind {
            ExtendedSamplerKind::MirostatV1 => self.mirostat_v1,
            ExtendedSamplerKind::MirostatV2 => self.mirostat_v2,
            ExtendedSamplerKind::EtaCutoff => self.eta_cutoff,
            ExtendedSamplerKind::SparseLogitBias => self.sparse_logit_bias,
            ExtendedSamplerKind::TopNSigma => self.top_n_sigma,
        }
    }

    const fn any(self) -> bool {
        self.mirostat_v1
            || self.mirostat_v2
            || self.eta_cutoff
            || self.sparse_logit_bias
            || self.top_n_sigma
    }
}

/// Inspected support for the additive extended-sampler program.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "ExtendedSamplingCapabilitiesWire")]
pub struct ExtendedSamplingCapabilities {
    declaration: CapabilityDeclarationStatus,
    samplers: ExtendedSamplerSupport,
    max_sparse_logit_bias_entries: Option<u16>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtendedSamplingCapabilitiesWire {
    #[serde(default)]
    declaration: CapabilityDeclarationStatus,
    #[serde(default)]
    samplers: ExtendedSamplerSupport,
    #[serde(default)]
    max_sparse_logit_bias_entries: Option<u16>,
}

impl ExtendedSamplingCapabilities {
    pub fn new(
        declaration: CapabilityDeclarationStatus,
        samplers: ExtendedSamplerSupport,
        max_sparse_logit_bias_entries: Option<u16>,
    ) -> Result<Self, NativeError> {
        reject_details_when_unreported(
            "extended sampling",
            declaration,
            samplers.any() || max_sparse_logit_bias_entries.is_some(),
        )?;
        if max_sparse_logit_bias_entries
            .is_some_and(|value| usize::from(value) > MAX_SPARSE_LOGIT_BIAS_ENTRIES)
        {
            return Err(invalid_config(format!(
                "max_sparse_logit_bias_entries cannot exceed {MAX_SPARSE_LOGIT_BIAS_ENTRIES}"
            )));
        }
        if !samplers.sparse_logit_bias && max_sparse_logit_bias_entries.is_some() {
            return Err(invalid_config(
                "sparse logit bias limit cannot be declared without sampler support",
            ));
        }
        Ok(Self {
            declaration,
            samplers,
            max_sparse_logit_bias_entries,
        })
    }

    #[must_use]
    pub const fn declaration(self) -> CapabilityDeclarationStatus {
        self.declaration
    }

    #[must_use]
    pub const fn samplers(self) -> ExtendedSamplerSupport {
        self.samplers
    }

    #[must_use]
    pub const fn max_sparse_logit_bias_entries(self) -> Option<u16> {
        self.max_sparse_logit_bias_entries
    }
}

impl Default for ExtendedSamplingCapabilities {
    fn default() -> Self {
        Self {
            declaration: CapabilityDeclarationStatus::Unreported,
            samplers: ExtendedSamplerSupport::default(),
            max_sparse_logit_bias_entries: None,
        }
    }
}

impl TryFrom<ExtendedSamplingCapabilitiesWire> for ExtendedSamplingCapabilities {
    type Error = NativeError;

    fn try_from(value: ExtendedSamplingCapabilitiesWire) -> Result<Self, Self::Error> {
        Self::new(
            value.declaration,
            value.samplers,
            value.max_sparse_logit_bias_entries,
        )
    }
}

/// Additive capability envelope for evidence and sampler APIs not implemented
/// by the legacy generation surface. Every nested default is unreported.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct NativeEvidenceCapabilities {
    #[serde(default)]
    pub embeddings: EmbeddingCapabilities,
    #[serde(default)]
    pub structured_constraints: StructuredConstraintCapabilities,
    #[serde(default)]
    pub distribution_observations: DistributionObservationCapabilities,
    #[serde(default)]
    pub extended_sampling: ExtendedSamplingCapabilities,
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
    /// Additive evidence/control surface. Legacy descriptors deserialize every
    /// nested declaration as `Unreported`; no support is inferred.
    #[serde(default)]
    pub evidence: NativeEvidenceCapabilities,
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

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResidentModelStatus {
    pub model_id: String,
    /// Operational location of this live resident. Paths are deliberately not
    /// part of [`ModelFingerprint`] and are redacted from `Debug` output.
    #[serde(default)]
    pub model_path: PathBuf,
    pub state: ModelRuntimeState,
    pub fingerprint: Option<ModelFingerprint>,
    #[serde(default)]
    pub descriptor: Option<NativeModelDescriptor>,
    pub active_sequences: usize,
    pub max_sequences: u32,
}

impl std::fmt::Debug for ResidentModelStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResidentModelStatus")
            .field("model_id", &self.model_id)
            .field("model_path", &"<redacted>")
            .field("state", &self.state)
            .field("fingerprint", &self.fingerprint)
            .field("descriptor", &self.descriptor)
            .field("active_sequences", &self.active_sequences)
            .field("max_sequences", &self.max_sequences)
            .finish()
    }
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

fn invalid_config(message: impl Into<String>) -> NativeError {
    NativeError::new(NativeErrorCode::InvalidConfig, message)
}

fn validate_finite_inclusive(
    field: &str,
    value: f32,
    minimum: f32,
    maximum: f32,
) -> Result<(), NativeError> {
    if !value.is_finite() || value < minimum || value > maximum {
        return Err(invalid_config(format!(
            "{field} must be finite and between {minimum} and {maximum}"
        )));
    }
    Ok(())
}

fn validate_distribution_value(
    value_kind: DistributionValueKind,
    value: f32,
) -> Result<(), NativeError> {
    if !value.is_finite() {
        return Err(invalid_config(
            "distribution observation values must be finite",
        ));
    }
    match value_kind {
        DistributionValueKind::Logit => Ok(()),
        DistributionValueKind::Probability if (0.0..=1.0).contains(&value) => Ok(()),
        DistributionValueKind::LogProbability if value <= 0.0 => Ok(()),
        DistributionValueKind::Probability => Err(invalid_config(
            "values labeled probability must be between 0 and 1",
        )),
        DistributionValueKind::LogProbability => Err(invalid_config(
            "values labeled log_probability must not be positive",
        )),
    }
}

fn validate_consistent_token_bytes<'a>(
    known: &mut std::collections::BTreeMap<i32, &'a [u8]>,
    token: &'a DistributionTokenValue,
) -> Result<(), NativeError> {
    let Some(bytes) = token.token_bytes() else {
        return Ok(());
    };
    if let Some(previous) = known.get(&token.token_id()) {
        if *previous != bytes {
            return Err(invalid_config(
                "one token ID has conflicting byte evidence across observations",
            ));
        }
    } else {
        known.insert(token.token_id(), bytes);
    }
    Ok(())
}

fn validate_id(field: &str, value: &str) -> Result<(), NativeError> {
    if value.is_empty() || value.len() > MAX_DTO_ID_BYTES || value.chars().any(char::is_control) {
        return Err(invalid_config(format!(
            "{field} must contain 1 to {MAX_DTO_ID_BYTES} UTF-8 bytes and no control characters"
        )));
    }
    Ok(())
}

fn validate_sha256(field: &str, value: &str) -> Result<(), NativeError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid_config(format!(
            "{field} must be 64 lowercase hexadecimal characters"
        )));
    }
    Ok(())
}

fn reject_details_when_unreported(
    capability: &str,
    declaration: CapabilityDeclarationStatus,
    has_details: bool,
) -> Result<(), NativeError> {
    if declaration == CapabilityDeclarationStatus::Unreported && has_details {
        return Err(invalid_config(format!(
            "unreported {capability} capability cannot contain support details"
        )));
    }
    Ok(())
}

fn validate_embedding_inputs(inputs: &[EmbeddingInput]) -> Result<(), NativeError> {
    if inputs.is_empty() || inputs.len() > MAX_EMBEDDING_BATCH_INPUTS {
        return Err(invalid_config(format!(
            "embedding batch must contain between 1 and {MAX_EMBEDDING_BATCH_INPUTS} inputs"
        )));
    }
    let mut input_ids = std::collections::HashSet::new();
    let mut total_tokens = 0usize;
    for input in inputs {
        if !input_ids.insert(input.input_id()) {
            return Err(invalid_config(format!(
                "embedding batch contains duplicate input_id {}",
                input.input_id()
            )));
        }
        total_tokens = total_tokens
            .checked_add(input.token_ids().len())
            .ok_or_else(|| invalid_config("embedding batch token count overflow"))?;
    }
    if total_tokens > MAX_EMBEDDING_BATCH_TOKENS {
        return Err(invalid_config(format!(
            "embedding batch cannot exceed {MAX_EMBEDDING_BATCH_TOKENS} total token IDs"
        )));
    }
    Ok(())
}

fn validate_embedding_outputs(
    config: EmbeddingOutputConfig,
    outputs: &[EmbeddingVectorOutput],
) -> Result<(), NativeError> {
    if outputs.is_empty() || outputs.len() > MAX_EMBEDDING_BATCH_INPUTS {
        return Err(invalid_config(format!(
            "embedding output batch must contain between 1 and {MAX_EMBEDDING_BATCH_INPUTS} outputs"
        )));
    }
    let mut input_ids = std::collections::HashSet::new();
    let mut total_tokens = 0usize;
    let mut total_values = 0usize;
    for (expected_index, output) in outputs.iter().enumerate() {
        if output.input_index() != expected_index {
            return Err(invalid_config(
                "embedding outputs must have contiguous indexes in submitted order",
            ));
        }
        if !input_ids.insert(output.input_id()) {
            return Err(invalid_config(format!(
                "embedding outputs contain duplicate input_id {}",
                output.input_id()
            )));
        }
        total_tokens = total_tokens
            .checked_add(output.token_ids().len())
            .ok_or_else(|| invalid_config("embedding output token count overflow"))?;
        total_values = total_values
            .checked_add(output.values().len())
            .ok_or_else(|| invalid_config("embedding output value count overflow"))?;

        let expected_rows = match config.pooling() {
            EmbeddingPooling::None => u32::try_from(output.token_ids().len())
                .map_err(|_| invalid_config("embedding output row count overflow"))?,
            EmbeddingPooling::Mean
            | EmbeddingPooling::Cls
            | EmbeddingPooling::Last
            | EmbeddingPooling::Rank => 1,
        };
        if output.row_count() != expected_rows {
            return Err(invalid_config(format!(
                "embedding output {} has {} rows; expected {expected_rows} for {:?} pooling",
                output.input_id(),
                output.row_count(),
                config.pooling()
            )));
        }
        let expected_values = usize::try_from(output.row_count())
            .ok()
            .and_then(|rows| rows.checked_mul(config.dimensions() as usize))
            .ok_or_else(|| invalid_config("embedding output shape overflow"))?;
        if output.values().len() != expected_values {
            return Err(invalid_config(format!(
                "embedding output {} has {} values; expected {expected_values}",
                output.input_id(),
                output.values().len()
            )));
        }
    }
    if total_tokens > MAX_EMBEDDING_BATCH_TOKENS {
        return Err(invalid_config(format!(
            "embedding output batch cannot exceed {MAX_EMBEDDING_BATCH_TOKENS} total token IDs"
        )));
    }
    if total_values > MAX_EMBEDDING_BATCH_VALUES {
        return Err(invalid_config(format!(
            "embedding output batch cannot exceed {MAX_EMBEDDING_BATCH_VALUES} total values"
        )));
    }
    Ok(())
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
    #[error("model_in_use")]
    ModelInUse,
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
    #[error("queue_full")]
    QueueFull,
    #[error("duplicate_active_request")]
    DuplicateActiveRequest,
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

    fn test_model_fingerprint() -> ModelFingerprint {
        ModelFingerprint {
            model_id: "model".to_string(),
            model_size: 1024,
            model_sha256: "a".repeat(64),
            tokenizer_sha256: "b".repeat(64),
            chat_template_sha256: "c".repeat(64),
            multimodal_projector_sha256: None,
            binding_version: "binding".to_string(),
            build_id: "build".to_string(),
            backend: "llama.cpp".to_string(),
            context_tokens: 8192,
            batch_tokens: 512,
            max_sequences: 4,
            rope_config_sha256: "d".repeat(64),
            kv_layout_sha256: "e".repeat(64),
        }
    }

    #[test]
    fn native_defaults_bound_parallelism() {
        let config = NativeModelConfig::local(PathBuf::from("model.gguf"));
        assert_eq!(config.max_sequences, MAX_PARALLEL_SEQUENCES);
        assert_eq!(config.device, NativeDevice::Auto);
        assert!(config.expected_model_sha256.is_none());
        assert!(config.expected_mmproj_sha256.is_none());
    }

    #[test]
    fn model_fingerprints_are_path_free_and_resident_debug_redacts_live_paths() {
        let fingerprint = test_model_fingerprint();
        let encoded = serde_json::to_value(&fingerprint).expect("serialize fingerprint");
        assert!(encoded.get("model_path").is_none());

        let status = ResidentModelStatus {
            model_id: "model".to_string(),
            model_path: PathBuf::from("/private/models/writer.gguf"),
            state: ModelRuntimeState::Ready,
            fingerprint: Some(fingerprint),
            descriptor: None,
            active_sequences: 0,
            max_sequences: 1,
        };
        let debug = format!("{status:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("writer.gguf"));
        assert_eq!(
            serde_json::to_value(&status)
                .expect("serialize live status")
                .get("model_path"),
            Some(&serde_json::json!("/private/models/writer.gguf"))
        );
    }

    #[test]
    fn legacy_model_config_defaults_trusted_artifact_hashes_to_absent() {
        let config: NativeModelConfig = serde_json::from_value(serde_json::json!({
            "model_id": "model",
            "model_path": "model.gguf",
            "mmproj_path": null,
            "device": "cpu",
            "context_tokens": 8192,
            "batch_tokens": 512,
            "max_sequences": 4,
            "gpu_layers": 0
        }))
        .expect("legacy config remains readable");
        assert!(config.expected_model_sha256.is_none());
        assert!(config.expected_mmproj_sha256.is_none());
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
        assert_eq!(
            capabilities.exact.evidence,
            NativeEvidenceCapabilities::default()
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
        let object = json.as_object().expect("token observation JSON object");
        assert_eq!(object.len(), 3, "legacy JSON must not gain evidence fields");
        assert!(object.contains_key("generated_index"));
        assert!(object.contains_key("token_id"));
        assert!(object.contains_key("probabilities"));
        assert_eq!(json["probabilities"][0]["stage"], "raw_model");
        assert_eq!(json["probabilities"][1]["stage"], "post_sampler");
    }

    #[test]
    fn distribution_observation_policy_is_explicit_bounded_and_disabled_by_default() {
        let disabled: DistributionObservationPolicy =
            serde_json::from_str("{}").expect("deserialize disabled policy");
        assert!(disabled.is_disabled());
        assert!(disabled.stages().is_empty());
        assert!(disabled.value_kinds().is_empty());

        let policy = DistributionObservationPolicy::new(
            ProbabilityStageSet::new(vec![
                ProbabilityStage::RawModel,
                ProbabilityStage::PostGuidance,
            ])
            .expect("unique stages"),
            DistributionValueKindSet::new(vec![
                DistributionValueKind::Logit,
                DistributionValueKind::LogProbability,
            ])
            .expect("unique value kinds"),
            true,
            MAX_DISTRIBUTION_OBSERVATION_TOP_K,
        )
        .expect("valid observation policy");
        let json = serde_json::to_string(&policy).expect("serialize observation policy");
        let decoded: DistributionObservationPolicy =
            serde_json::from_str(&json).expect("deserialize observation policy");
        assert_eq!(decoded, policy);
        assert_eq!(decoded.stages()[1], ProbabilityStage::PostGuidance);

        let over_limit = format!(
            r#"{{"stages":["raw_model"],"value_kinds":["logit"],"top_k":{}}}"#,
            u32::from(MAX_DISTRIBUTION_OBSERVATION_TOP_K) + 1
        );
        assert!(
            serde_json::from_str::<DistributionObservationPolicy>(&over_limit).is_err(),
            "top-k evidence above the public cap must fail"
        );
        assert!(
            ProbabilityStageSet::new(vec![ProbabilityStage::RawModel, ProbabilityStage::RawModel,])
                .is_err(),
            "duplicate stage declarations must fail"
        );
    }

    #[test]
    fn token_distribution_evidence_is_ranked_typed_and_fail_closed() {
        let selected_probability = DistributionTokenValue::new(42, Some(b"chosen".to_vec()), 0.4)
            .expect("valid selected probability");
        let raw_probability = StageDistributionObservation::new(
            ProbabilityStage::RawModel,
            DistributionValueKind::Probability,
            selected_probability,
            vec![
                RankedDistributionCandidate::new(
                    1,
                    DistributionTokenValue::new(42, Some(b"chosen".to_vec()), 0.4)
                        .expect("valid selected top candidate"),
                )
                .expect("valid selected rank"),
                RankedDistributionCandidate::new(
                    2,
                    DistributionTokenValue::new(7, Some(b"first".to_vec()), 0.3)
                        .expect("valid second candidate"),
                )
                .expect("valid second rank"),
                RankedDistributionCandidate::new(
                    3,
                    DistributionTokenValue::new(9, Some(b"second".to_vec()), 0.2)
                        .expect("valid third candidate"),
                )
                .expect("valid third rank"),
            ],
        )
        .expect("valid ranked distribution");
        let guided_log_probability = StageDistributionObservation::new(
            ProbabilityStage::PostGuidance,
            DistributionValueKind::LogProbability,
            DistributionTokenValue::new(42, Some(b"chosen".to_vec()), -0.8)
                .expect("valid selected log probability"),
            vec![
                RankedDistributionCandidate::new(
                    1,
                    DistributionTokenValue::new(42, Some(b"chosen".to_vec()), -0.8)
                        .expect("valid selected ranked log probability"),
                )
                .expect("valid selected rank"),
                RankedDistributionCandidate::new(
                    2,
                    DistributionTokenValue::new(7, Some(b"first".to_vec()), -1.2)
                        .expect("valid candidate log probability"),
                )
                .expect("valid candidate rank"),
            ],
        )
        .expect("valid guided distribution");
        let evidence = TokenDistributionObservation::new(
            3,
            42,
            vec![raw_probability.clone(), guided_log_probability],
        )
        .expect("valid token distribution evidence");
        let json = serde_json::to_string(&evidence).expect("serialize distribution evidence");
        let decoded: TokenDistributionObservation =
            serde_json::from_str(&json).expect("deserialize distribution evidence");
        assert_eq!(decoded, evidence);
        assert_eq!(decoded.generated_index(), 3);
        assert_eq!(decoded.observations()[0].ranked_candidates()[2].rank(), 3);

        assert!(
            StageDistributionObservation::new(
                ProbabilityStage::RawModel,
                DistributionValueKind::Probability,
                DistributionTokenValue::new(42, None, 1.1)
                    .expect("finite token value before semantic labeling"),
                Vec::new(),
            )
            .is_err(),
            "a value outside [0, 1] cannot be labeled probability"
        );
        assert!(
            StageDistributionObservation::new(
                ProbabilityStage::RawModel,
                DistributionValueKind::Probability,
                DistributionTokenValue::new(42, None, 0.4).expect("valid selected token"),
                vec![
                    RankedDistributionCandidate::new(
                        1,
                        DistributionTokenValue::new(42, None, 0.3)
                            .expect("locally valid inconsistent selected token"),
                    )
                    .expect("valid rank")
                ],
            )
            .is_err(),
            "ranked selected-token evidence must match its separate value"
        );
        assert!(
            StageDistributionObservation::new(
                ProbabilityStage::RawModel,
                DistributionValueKind::Probability,
                DistributionTokenValue::new(42, None, 0.4).expect("valid selected token"),
                vec![
                    RankedDistributionCandidate::new(
                        1,
                        DistributionTokenValue::new(7, None, 0.3).expect("valid first candidate"),
                    )
                    .expect("valid first rank"),
                    RankedDistributionCandidate::new(
                        2,
                        DistributionTokenValue::new(9, None, 0.2).expect("valid second candidate"),
                    )
                    .expect("valid second rank"),
                ],
            )
            .is_err(),
            "a selected token above the top-k floor cannot be silently omitted"
        );
        assert!(
            TokenDistributionObservation::new(
                0,
                42,
                vec![raw_probability.clone(), raw_probability],
            )
            .is_err(),
            "duplicate stage/value-kind evidence must fail"
        );
        assert!(
            RankedDistributionCandidate::new(
                MAX_DISTRIBUTION_OBSERVATION_TOP_K + 1,
                DistributionTokenValue::new(1, None, 0.0).expect("valid finite value"),
            )
            .is_err(),
            "candidate ranks above the top-k cap must fail"
        );
        assert!(
            DistributionTokenValue::new(1, None, f32::INFINITY).is_err(),
            "non-finite values must fail before serialization"
        );
    }

    #[test]
    fn embedding_request_round_trip_preserves_exact_tokens_and_order() {
        let inputs = vec![
            EmbeddingInput::new("first".to_string(), vec![1, 2, 3]).expect("valid first input"),
            EmbeddingInput::new("second".to_string(), vec![4, 5]).expect("valid second input"),
        ];
        let request = EmbeddingBatchRequest::new(
            "embedding-request".to_string(),
            "model".to_string(),
            inputs,
            EmbeddingPooling::Mean,
            EmbeddingNormalization::L2,
        )
        .expect("valid embedding request");
        let json = serde_json::to_string(&request).expect("serialize embedding request");
        let decoded: EmbeddingBatchRequest =
            serde_json::from_str(&json).expect("deserialize embedding request");
        assert_eq!(decoded, request);
        assert_eq!(decoded.inputs()[0].token_ids(), &[1, 2, 3]);
        assert_eq!(decoded.inputs()[1].input_id(), "second");

        let duplicate = vec![
            EmbeddingInput::new("same".to_string(), vec![1]).expect("valid input"),
            EmbeddingInput::new("same".to_string(), vec![2]).expect("valid input"),
        ];
        assert!(
            EmbeddingBatchRequest::new(
                "request".to_string(),
                "model".to_string(),
                duplicate,
                EmbeddingPooling::Last,
                EmbeddingNormalization::None,
            )
            .is_err(),
            "duplicate causal input identities must fail"
        );
        assert!(
            serde_json::from_value::<EmbeddingInput>(serde_json::json!({
                "input_id": "empty",
                "token_ids": []
            }))
            .is_err(),
            "empty exact-token inputs must fail at the serde boundary"
        );
    }

    #[test]
    fn embedding_output_validates_shape_finiteness_and_transport() {
        let config =
            EmbeddingOutputConfig::new(EmbeddingPooling::Mean, EmbeddingNormalization::L2, 3)
                .expect("valid output config");
        let result =
            EmbeddingVectorOutput::new("first".to_string(), 0, vec![1, 2], 1, vec![0.1, 0.2, 0.3])
                .expect("valid pooled embedding");
        let evidence = EmbeddingTransportEvidence::new(NativeTransport::InProcess, true, false)
            .expect("coherent transport evidence");
        let output = EmbeddingBatchOutput::new(
            "request".to_string(),
            "model".to_string(),
            config,
            vec![result],
            test_model_fingerprint(),
            evidence,
        )
        .expect("valid embedding batch output");
        let json = serde_json::to_string(&output).expect("serialize embedding output");
        let decoded: EmbeddingBatchOutput =
            serde_json::from_str(&json).expect("deserialize embedding output");
        assert_eq!(decoded, output);
        assert_eq!(decoded.outputs()[0].values(), &[0.1, 0.2, 0.3]);

        let wrong_shape =
            EmbeddingVectorOutput::new("wrong".to_string(), 0, vec![1], 1, vec![0.1, 0.2])
                .expect("locally bounded output");
        assert!(
            EmbeddingBatchOutput::new(
                "request".to_string(),
                "model".to_string(),
                config,
                vec![wrong_shape],
                test_model_fingerprint(),
                evidence,
            )
            .is_err(),
            "batch output must reject a dimensions mismatch"
        );
        assert!(
            EmbeddingVectorOutput::new("nan".to_string(), 0, vec![1], 1, vec![f32::NAN],).is_err(),
            "non-finite embedding values must fail"
        );
        assert!(
            EmbeddingTransportEvidence::new(NativeTransport::InProcess, false, true).is_err(),
            "fixture evidence cannot masquerade as in-process inference"
        );
    }

    #[test]
    fn structured_constraints_are_bounded_content_references_not_raw_json() {
        let reference = ConstraintArtifactReference::new(
            "constraint:scene-card".to_string(),
            "a".repeat(64),
            512,
        )
        .expect("valid constraint reference");
        let constraint = StructuredConstraint::JsonSchema {
            reference: reference.clone(),
        };
        let json = serde_json::to_value(&constraint).expect("serialize constraint reference");
        assert_eq!(json["kind"], "json_schema");
        assert_eq!(json["reference"]["sha256"], "a".repeat(64));
        assert!(json.get("schema").is_none());
        let decoded: StructuredConstraint =
            serde_json::from_value(json).expect("deserialize constraint reference");
        assert_eq!(decoded, constraint);

        assert!(
            ConstraintArtifactReference::new(
                "constraint".to_string(),
                "not-a-digest".to_string(),
                10,
            )
            .is_err(),
            "unverifiable content identities must fail"
        );
        assert!(
            serde_json::from_value::<StructuredConstraint>(serde_json::json!({
                "kind": "json_schema",
                "reference": {
                    "artifact_id": "constraint",
                    "sha256": "a".repeat(64),
                    "byte_len": 10
                },
                "schema": {"type": "object"}
            }))
            .is_err(),
            "raw schema bodies must not be accepted beside a reference"
        );
    }

    #[test]
    fn extended_sampler_program_is_typed_bounded_and_legacy_neutral() {
        let biases = SparseLogitBias::new(vec![
            TokenLogitBias {
                token_id: 7,
                bias: -2.5,
            },
            TokenLogitBias {
                token_id: 9,
                bias: 1.25,
            },
        ])
        .expect("valid sparse logit biases");
        let program = ExtendedSamplerProgram::new(vec![
            ExtendedSampler::SparseLogitBias { biases },
            ExtendedSampler::TopNSigma {
                sigma: TopNSigma::new(2.0).expect("valid sigma"),
            },
            ExtendedSampler::MirostatV2 {
                config: MirostatV2Config::new(5.0, 0.1).expect("valid Mirostat v2"),
            },
        ])
        .expect("valid extended sampler program");
        let json = serde_json::to_string(&program).expect("serialize sampler program");
        let decoded: ExtendedSamplerProgram =
            serde_json::from_str(&json).expect("deserialize sampler program");
        assert_eq!(decoded, program);
        assert_eq!(decoded.as_slice()[1].kind(), ExtendedSamplerKind::TopNSigma);

        let both_mirostat = ExtendedSamplerProgram::new(vec![
            ExtendedSampler::MirostatV1 {
                config: MirostatV1Config::new(5.0, 0.1, 100).expect("valid Mirostat v1"),
            },
            ExtendedSampler::MirostatV2 {
                config: MirostatV2Config::new(5.0, 0.1).expect("valid Mirostat v2"),
            },
        ]);
        assert!(both_mirostat.is_err());
        let non_terminal_mirostat = ExtendedSamplerProgram::new(vec![
            ExtendedSampler::MirostatV2 {
                config: MirostatV2Config::new(5.0, 0.1).expect("valid Mirostat v2"),
            },
            ExtendedSampler::TopNSigma {
                sigma: TopNSigma::new(2.0).expect("valid sigma"),
            },
        ]);
        assert!(
            non_terminal_mirostat.is_err(),
            "Mirostat cannot be followed by another transform"
        );
        assert!(
            MirostatV1Config::new(5.0, 0.1, 0).is_err(),
            "Mirostat v1 filter window must be positive"
        );
        assert!(
            SparseLogitBias::new(vec![
                TokenLogitBias {
                    token_id: 1,
                    bias: 1.0,
                },
                TokenLogitBias {
                    token_id: 1,
                    bias: 2.0,
                },
            ])
            .is_err(),
            "duplicate token biases must fail"
        );

        let legacy: SamplingConfig = serde_json::from_value(serde_json::json!({
            "seed": 1,
            "temperature": 0.7,
            "top_k": 40,
            "top_p": 0.95,
            "min_p": 0.0,
            "repeat_penalty": 1.0,
            "max_tokens": 8,
            "stop": []
        }))
        .expect("legacy sampling config remains readable");
        assert_eq!(
            legacy,
            SamplingConfig {
                seed: 1,
                max_tokens: 8,
                ..SamplingConfig::default()
            }
        );
    }

    #[test]
    fn evidence_capabilities_default_to_unreported_and_reject_implied_support() {
        let defaults: NativeEvidenceCapabilities =
            serde_json::from_str("{}").expect("default capability envelope");
        assert_eq!(
            defaults.embeddings.declaration(),
            CapabilityDeclarationStatus::Unreported
        );
        assert_eq!(
            defaults.structured_constraints.declaration(),
            CapabilityDeclarationStatus::Unreported
        );
        assert_eq!(
            defaults.distribution_observations.declaration(),
            CapabilityDeclarationStatus::Unreported
        );
        assert_eq!(
            defaults.extended_sampling.declaration(),
            CapabilityDeclarationStatus::Unreported
        );
        assert!(
            !defaults
                .embeddings
                .pooling()
                .supports(EmbeddingPooling::Mean)
        );
        assert!(
            !defaults
                .structured_constraints
                .supports(StructuredConstraintKind::Gbnf)
        );
        assert!(
            !defaults
                .distribution_observations
                .stages()
                .supports(ProbabilityStage::RawModel)
        );
        assert!(
            !defaults
                .extended_sampling
                .samplers()
                .supports(ExtendedSamplerKind::MirostatV2)
        );

        assert!(
            serde_json::from_value::<EmbeddingCapabilities>(serde_json::json!({
                "pooling": {"mean": true}
            }))
            .is_err(),
            "unreported capability details must fail rather than imply support"
        );

        let inspected = ExtendedSamplingCapabilities::new(
            CapabilityDeclarationStatus::Inspected,
            ExtendedSamplerSupport {
                mirostat_v2: true,
                sparse_logit_bias: true,
                top_n_sigma: true,
                ..ExtendedSamplerSupport::default()
            },
            Some(512),
        )
        .expect("valid inspected sampler capabilities");
        let json = serde_json::to_string(&inspected).expect("serialize capabilities");
        let decoded: ExtendedSamplingCapabilities =
            serde_json::from_str(&json).expect("deserialize capabilities");
        assert_eq!(decoded, inspected);
        assert!(decoded.samplers().supports(ExtendedSamplerKind::MirostatV2));
        assert!(!decoded.samplers().supports(ExtendedSamplerKind::MirostatV1));
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
