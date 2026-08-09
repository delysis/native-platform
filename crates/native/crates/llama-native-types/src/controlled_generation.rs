//! Strict, product-neutral contracts for controlled generation.
//!
//! This module is additive. The legacy generation DTOs remain unchanged and a
//! backend must opt into this surface explicitly. Every controlled request is
//! completion-shaped, exact-token, fingerprint-bound, and self-describing.

use super::{
    DistributionObservationPolicy, DistributionValueKind, ExtendedSampler, ExtendedSamplerKind,
    ExtendedSamplerProgram, GenerationCacheMetrics, GenerationMetrics, GenerationOutput,
    GenerationState, MAX_EXTENDED_SAMPLERS, ModelFingerprint, NativeError, NativeErrorCode,
    NativeTransport, ProbabilityStage, SAMPLING_CONFIG_FINGERPRINT_DOMAIN, SamplerKind,
    SamplingConfig, StructuredConstraint, TokenDistributionObservation, TokenObservation,
    TokenProbabilityObservation,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::sampling_fingerprint::sampling_float_bits;

pub const MAX_CONTROLLED_BATCH_CASES: usize = 4;
pub const MAX_CONTROL_MODEL_PARTICIPANTS: usize = 8;
pub const MAX_CONTROL_PROMPT_TOKENS: usize = 262_144;
pub const MAX_CONTROL_BATCH_PROMPT_TOKENS: usize = 1_048_576;
pub const MAX_GUIDANCE_CONTROLS: usize = 3;
pub const MAX_STATIC_CONTROL_PROFILES: usize = 32;
pub const MAX_STATIC_ADAPTERS_PER_PROFILE: usize = 32;
pub const MAX_CONTROL_APPLICATION_REPORTS: usize = 48;
pub const MAX_STOP_SEQUENCES: usize = 64;
pub const MAX_STOP_SEQUENCE_BYTES: usize = 16_384;
pub const MAX_CONTROL_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024 * 1024;
pub const CONTROL_PROGRAM_FORMAT: &str = "llama-native.control-program.v1";

const MAX_ID_BYTES: usize = 256;
const MAX_ABS_CONTROL_SCALAR: f32 = 100.0;
const MAX_GENERATION_TOKENS_PER_CASE: u32 = 1_048_576;

fn invalid(message: impl Into<String>) -> NativeError {
    NativeError::new(NativeErrorCode::InvalidConfig, message)
}

fn validate_id(field: &str, value: &str) -> Result<(), NativeError> {
    if value.is_empty() || value.len() > MAX_ID_BYTES || value.chars().any(char::is_control) {
        return Err(invalid(format!(
            "{field} must contain 1 to {MAX_ID_BYTES} UTF-8 bytes and no control characters"
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
        return Err(invalid(format!(
            "{field} must be 64 lowercase hexadecimal characters"
        )));
    }
    Ok(())
}

/// A finite IEEE-754 value serialized by exact bits rather than decimal text.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "ExactF32Wire")]
pub struct ExactF32 {
    bits: u32,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactF32Wire {
    bits: u32,
}

impl ExactF32 {
    pub fn new(value: f32) -> Result<Self, NativeError> {
        if !value.is_finite() {
            return Err(invalid("exact f32 value must be finite"));
        }
        Ok(Self {
            bits: value.to_bits(),
        })
    }

    #[must_use]
    pub const fn get(self) -> f32 {
        f32::from_bits(self.bits)
    }

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.bits
    }
}

impl TryFrom<ExactF32Wire> for ExactF32 {
    type Error = NativeError;

    fn try_from(value: ExactF32Wire) -> Result<Self, Self::Error> {
        Self::new(f32::from_bits(value.bits))
    }
}

/// Exact tokenizer and vocabulary semantics required for logit arithmetic.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "TokenContractIdentityWire")]
pub struct TokenContractIdentity {
    tokenizer_sha256: String,
    vocabulary_sha256: String,
    special_tokens_sha256: String,
    token_bytes_sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenContractIdentityWire {
    tokenizer_sha256: String,
    vocabulary_sha256: String,
    special_tokens_sha256: String,
    token_bytes_sha256: String,
}

impl TokenContractIdentity {
    pub fn new(
        tokenizer_sha256: String,
        vocabulary_sha256: String,
        special_tokens_sha256: String,
        token_bytes_sha256: String,
    ) -> Result<Self, NativeError> {
        validate_sha256("tokenizer_sha256", &tokenizer_sha256)?;
        validate_sha256("vocabulary_sha256", &vocabulary_sha256)?;
        validate_sha256("special_tokens_sha256", &special_tokens_sha256)?;
        validate_sha256("token_bytes_sha256", &token_bytes_sha256)?;
        Ok(Self {
            tokenizer_sha256,
            vocabulary_sha256,
            special_tokens_sha256,
            token_bytes_sha256,
        })
    }

    #[must_use]
    pub fn tokenizer_sha256(&self) -> &str {
        &self.tokenizer_sha256
    }

    #[must_use]
    pub fn vocabulary_sha256(&self) -> &str {
        &self.vocabulary_sha256
    }

    #[must_use]
    pub fn special_tokens_sha256(&self) -> &str {
        &self.special_tokens_sha256
    }

    #[must_use]
    pub fn token_bytes_sha256(&self) -> &str {
        &self.token_bytes_sha256
    }
}

impl TryFrom<TokenContractIdentityWire> for TokenContractIdentity {
    type Error = NativeError;

    fn try_from(value: TokenContractIdentityWire) -> Result<Self, Self::Error> {
        Self::new(
            value.tokenizer_sha256,
            value.vocabulary_sha256,
            value.special_tokens_sha256,
            value.token_bytes_sha256,
        )
    }
}

/// One model context participating in controlled generation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "ControlledModelIdentityWire")]
pub struct ControlledModelIdentity {
    participant_id: String,
    fingerprint: ModelFingerprint,
    token_contract: TokenContractIdentity,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlledModelIdentityWire {
    participant_id: String,
    fingerprint: StrictModelFingerprintWire,
    token_contract: TokenContractIdentity,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictModelFingerprintWire {
    model_id: String,
    model_size: u64,
    model_sha256: String,
    tokenizer_sha256: String,
    chat_template_sha256: String,
    multimodal_projector_sha256: Option<String>,
    binding_version: String,
    build_id: String,
    backend: String,
    context_tokens: u32,
    batch_tokens: u32,
    max_sequences: u32,
    rope_config_sha256: String,
    kv_layout_sha256: String,
}

impl From<StrictModelFingerprintWire> for ModelFingerprint {
    fn from(value: StrictModelFingerprintWire) -> Self {
        Self {
            model_id: value.model_id,
            model_size: value.model_size,
            model_sha256: value.model_sha256,
            tokenizer_sha256: value.tokenizer_sha256,
            chat_template_sha256: value.chat_template_sha256,
            multimodal_projector_sha256: value.multimodal_projector_sha256,
            binding_version: value.binding_version,
            build_id: value.build_id,
            backend: value.backend,
            context_tokens: value.context_tokens,
            batch_tokens: value.batch_tokens,
            max_sequences: value.max_sequences,
            rope_config_sha256: value.rope_config_sha256,
            kv_layout_sha256: value.kv_layout_sha256,
        }
    }
}

impl ControlledModelIdentity {
    pub fn new(
        participant_id: String,
        fingerprint: ModelFingerprint,
        token_contract: TokenContractIdentity,
    ) -> Result<Self, NativeError> {
        validate_id("control participant_id", &participant_id)?;
        validate_model_fingerprint(&fingerprint)?;
        if fingerprint.tokenizer_sha256 != token_contract.tokenizer_sha256 {
            return Err(invalid(
                "model fingerprint and token contract tokenizer digests disagree",
            ));
        }
        Ok(Self {
            participant_id,
            fingerprint,
            token_contract,
        })
    }

    #[must_use]
    pub fn participant_id(&self) -> &str {
        &self.participant_id
    }

    #[must_use]
    pub const fn fingerprint(&self) -> &ModelFingerprint {
        &self.fingerprint
    }

    #[must_use]
    pub const fn token_contract(&self) -> &TokenContractIdentity {
        &self.token_contract
    }

    #[must_use]
    pub fn identity_sha256(&self) -> String {
        let mut digest = StableDigest::new("controlled-model-identity-v1");
        hash_model_identity(&mut digest, self);
        digest.finish()
    }
}

impl TryFrom<ControlledModelIdentityWire> for ControlledModelIdentity {
    type Error = NativeError;

    fn try_from(value: ControlledModelIdentityWire) -> Result<Self, Self::Error> {
        Self::new(
            value.participant_id,
            value.fingerprint.into(),
            value.token_contract,
        )
    }
}

fn validate_model_fingerprint(fingerprint: &ModelFingerprint) -> Result<(), NativeError> {
    validate_id("model fingerprint model_id", &fingerprint.model_id)?;
    validate_sha256("model fingerprint model_sha256", &fingerprint.model_sha256)?;
    validate_sha256(
        "model fingerprint tokenizer_sha256",
        &fingerprint.tokenizer_sha256,
    )?;
    validate_sha256(
        "model fingerprint chat_template_sha256",
        &fingerprint.chat_template_sha256,
    )?;
    if let Some(projector) = &fingerprint.multimodal_projector_sha256 {
        validate_sha256("model fingerprint projector sha256", projector)?;
    }
    validate_sha256(
        "model fingerprint rope_config_sha256",
        &fingerprint.rope_config_sha256,
    )?;
    validate_sha256(
        "model fingerprint kv_layout_sha256",
        &fingerprint.kv_layout_sha256,
    )?;
    validate_id(
        "model fingerprint binding_version",
        &fingerprint.binding_version,
    )?;
    validate_id("model fingerprint build_id", &fingerprint.build_id)?;
    validate_id("model fingerprint backend", &fingerprint.backend)?;
    if fingerprint.model_size == 0
        || fingerprint.context_tokens == 0
        || fingerprint.batch_tokens == 0
        || fingerprint.max_sequences == 0
    {
        return Err(invalid(
            "controlled model fingerprints require non-zero size and runtime limits",
        ));
    }
    Ok(())
}

/// A non-empty, completion-shaped prompt in exact token IDs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "ExactTokenPromptWire")]
pub struct ExactTokenPrompt {
    token_ids: Vec<i32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactTokenPromptWire {
    token_ids: Vec<i32>,
}

impl ExactTokenPrompt {
    pub fn new(token_ids: Vec<i32>) -> Result<Self, NativeError> {
        if token_ids.is_empty() || token_ids.len() > MAX_CONTROL_PROMPT_TOKENS {
            return Err(invalid(format!(
                "controlled token prompt must contain between 1 and {MAX_CONTROL_PROMPT_TOKENS} tokens"
            )));
        }
        if token_ids.iter().any(|token_id| *token_id < 0) {
            return Err(invalid(
                "controlled token prompt IDs must all be non-negative",
            ));
        }
        Ok(Self { token_ids })
    }

    #[must_use]
    pub fn token_ids(&self) -> &[i32] {
        &self.token_ids
    }
}

impl TryFrom<ExactTokenPromptWire> for ExactTokenPrompt {
    type Error = NativeError;

    fn try_from(value: ExactTokenPromptWire) -> Result<Self, Self::Error> {
        Self::new(value.token_ids)
    }
}

/// One controlled batch case. No chat template or text re-tokenization exists
/// on this surface.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(try_from = "ControlledGenerationCaseWire")]
pub struct ControlledGenerationCase {
    case_id: String,
    conditional_prompt: ExactTokenPrompt,
    unconditional_prompt: Option<ExactTokenPrompt>,
    sampling: SamplingConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlledGenerationCaseWire {
    case_id: String,
    conditional_prompt: ExactTokenPrompt,
    #[serde(default)]
    unconditional_prompt: Option<ExactTokenPrompt>,
    sampling: StrictSamplingConfigWire,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictSamplingConfigWire {
    seed: u32,
    temperature: f32,
    dynamic_temperature_range: f32,
    dynamic_temperature_exponent: f32,
    top_k: i32,
    top_p: f32,
    min_p: f32,
    typical_p: f32,
    xtc_probability: f32,
    xtc_threshold: f32,
    repeat_last_n: i32,
    repeat_penalty: f32,
    frequency_penalty: f32,
    presence_penalty: f32,
    dry_multiplier: f32,
    dry_base: f32,
    dry_allowed_length: i32,
    dry_penalty_last_n: i32,
    sampler_order: Vec<SamplerKind>,
    max_tokens: u32,
    stop: Vec<String>,
}

impl From<StrictSamplingConfigWire> for SamplingConfig {
    fn from(value: StrictSamplingConfigWire) -> Self {
        Self {
            seed: value.seed,
            temperature: value.temperature,
            dynamic_temperature_range: value.dynamic_temperature_range,
            dynamic_temperature_exponent: value.dynamic_temperature_exponent,
            top_k: value.top_k,
            top_p: value.top_p,
            min_p: value.min_p,
            typical_p: value.typical_p,
            xtc_probability: value.xtc_probability,
            xtc_threshold: value.xtc_threshold,
            repeat_last_n: value.repeat_last_n,
            repeat_penalty: value.repeat_penalty,
            frequency_penalty: value.frequency_penalty,
            presence_penalty: value.presence_penalty,
            dry_multiplier: value.dry_multiplier,
            dry_base: value.dry_base,
            dry_allowed_length: value.dry_allowed_length,
            dry_penalty_last_n: value.dry_penalty_last_n,
            sampler_order: value.sampler_order,
            max_tokens: value.max_tokens,
            stop: value.stop,
        }
    }
}

impl ControlledGenerationCase {
    pub fn new(
        case_id: String,
        conditional_prompt: ExactTokenPrompt,
        unconditional_prompt: Option<ExactTokenPrompt>,
        sampling: SamplingConfig,
    ) -> Result<Self, NativeError> {
        validate_id("controlled case_id", &case_id)?;
        validate_sampling(&sampling)?;
        Ok(Self {
            case_id,
            conditional_prompt,
            unconditional_prompt,
            sampling,
        })
    }

    #[must_use]
    pub fn case_id(&self) -> &str {
        &self.case_id
    }

    #[must_use]
    pub const fn conditional_prompt(&self) -> &ExactTokenPrompt {
        &self.conditional_prompt
    }

    #[must_use]
    pub const fn unconditional_prompt(&self) -> Option<&ExactTokenPrompt> {
        self.unconditional_prompt.as_ref()
    }

    #[must_use]
    pub const fn sampling(&self) -> &SamplingConfig {
        &self.sampling
    }
}

impl TryFrom<ControlledGenerationCaseWire> for ControlledGenerationCase {
    type Error = NativeError;

    fn try_from(value: ControlledGenerationCaseWire) -> Result<Self, Self::Error> {
        Self::new(
            value.case_id,
            value.conditional_prompt,
            value.unconditional_prompt,
            value.sampling.into(),
        )
    }
}

fn validate_sampling(sampling: &SamplingConfig) -> Result<(), NativeError> {
    let bounded = [
        ("temperature", sampling.temperature, 0.0, 100.0),
        (
            "dynamic_temperature_range",
            sampling.dynamic_temperature_range,
            0.0,
            100.0,
        ),
        (
            "dynamic_temperature_exponent",
            sampling.dynamic_temperature_exponent,
            f32::MIN_POSITIVE,
            100.0,
        ),
        ("top_p", sampling.top_p, 0.0, 1.0),
        ("min_p", sampling.min_p, 0.0, 1.0),
        ("typical_p", sampling.typical_p, 0.0, 1.0),
        ("xtc_probability", sampling.xtc_probability, 0.0, 1.0),
        ("xtc_threshold", sampling.xtc_threshold, 0.0, 1.0),
        ("repeat_penalty", sampling.repeat_penalty, 0.0, 100.0),
        (
            "frequency_penalty",
            sampling.frequency_penalty,
            -100.0,
            100.0,
        ),
        ("presence_penalty", sampling.presence_penalty, -100.0, 100.0),
        ("dry_multiplier", sampling.dry_multiplier, 0.0, 100.0),
        ("dry_base", sampling.dry_base, f32::MIN_POSITIVE, 100.0),
    ];
    for (field, value, minimum, maximum) in bounded {
        if !value.is_finite() || value < minimum || value > maximum {
            return Err(invalid(format!(
                "controlled sampling {field} must be finite and between {minimum} and {maximum}"
            )));
        }
    }
    if sampling.top_k < 0 || sampling.repeat_last_n < -1 || sampling.dry_penalty_last_n < -1 {
        return Err(invalid(
            "controlled sampling integer windows must be non-negative or the documented -1 sentinel",
        ));
    }
    if sampling.dry_allowed_length < 0 {
        return Err(invalid(
            "controlled sampling dry_allowed_length must be non-negative",
        ));
    }
    if sampling.max_tokens == 0 || sampling.max_tokens > MAX_GENERATION_TOKENS_PER_CASE {
        return Err(invalid(format!(
            "controlled sampling max_tokens must be between 1 and {MAX_GENERATION_TOKENS_PER_CASE}"
        )));
    }
    if sampling
        .sampler_order
        .iter()
        .enumerate()
        .any(|(index, kind)| sampling.sampler_order[..index].contains(kind))
    {
        return Err(invalid(
            "controlled sampling sampler_order cannot contain duplicates",
        ));
    }
    if sampling.stop.len() > MAX_STOP_SEQUENCES {
        return Err(invalid(format!(
            "controlled sampling cannot contain more than {MAX_STOP_SEQUENCES} stop sequences"
        )));
    }
    let mut stop_bytes = 0usize;
    for stop in &sampling.stop {
        if stop.is_empty() {
            return Err(invalid(
                "controlled sampling stop sequences cannot be empty",
            ));
        }
        stop_bytes = stop_bytes
            .checked_add(stop.len())
            .ok_or_else(|| invalid("controlled sampling stop bytes overflow"))?;
    }
    if stop_bytes > MAX_STOP_SEQUENCE_BYTES {
        return Err(invalid(format!(
            "controlled sampling stop bytes cannot exceed {MAX_STOP_SEQUENCE_BYTES}"
        )));
    }
    Ok(())
}

/// Immutable content-addressed artifact metadata. Artifact bodies are resolved
/// by the embedding product and never travel in generation requests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "StaticControlArtifactWire")]
pub struct StaticControlArtifact {
    artifact_id: String,
    sha256: String,
    byte_len: u64,
    derivation_sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct StaticControlArtifactWire {
    artifact_id: String,
    sha256: String,
    byte_len: u64,
    derivation_sha256: String,
}

impl StaticControlArtifact {
    pub fn new(
        artifact_id: String,
        sha256: String,
        byte_len: u64,
        derivation_sha256: String,
    ) -> Result<Self, NativeError> {
        validate_id("static control artifact_id", &artifact_id)?;
        validate_sha256("static control artifact sha256", &sha256)?;
        validate_sha256("static control derivation_sha256", &derivation_sha256)?;
        if byte_len == 0 || byte_len > MAX_CONTROL_ARTIFACT_BYTES {
            return Err(invalid(format!(
                "static control artifact byte_len must be between 1 and {MAX_CONTROL_ARTIFACT_BYTES}"
            )));
        }
        Ok(Self {
            artifact_id,
            sha256,
            byte_len,
            derivation_sha256,
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
    pub const fn byte_len(&self) -> u64 {
        self.byte_len
    }

    #[must_use]
    pub fn derivation_sha256(&self) -> &str {
        &self.derivation_sha256
    }
}

impl TryFrom<StaticControlArtifactWire> for StaticControlArtifact {
    type Error = NativeError;

    fn try_from(value: StaticControlArtifactWire) -> Result<Self, Self::Error> {
        Self::new(
            value.artifact_id,
            value.sha256,
            value.byte_len,
            value.derivation_sha256,
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StaticAdapter {
    pub artifact: StaticControlArtifact,
    pub scale: ExactF32,
}

/// Static profiles are declarations only. They do not imply dynamic adapters,
/// arbitrary hidden-state projection, Jacobian control, or KV editing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StaticControlProfile {
    AdapterStack {
        profile_id: String,
        participant_id: String,
        adapters: Vec<StaticAdapter>,
    },
    ActivationVector {
        profile_id: String,
        participant_id: String,
        artifact: StaticControlArtifact,
        model_identity_sha256: String,
        layer_start: u32,
        layer_end_inclusive: u32,
        dimensions: u32,
        scale: ExactF32,
    },
}

impl StaticControlProfile {
    #[must_use]
    pub fn profile_id(&self) -> &str {
        match self {
            Self::AdapterStack { profile_id, .. } | Self::ActivationVector { profile_id, .. } => {
                profile_id
            }
        }
    }

    #[must_use]
    pub fn participant_id(&self) -> &str {
        match self {
            Self::AdapterStack { participant_id, .. }
            | Self::ActivationVector { participant_id, .. } => participant_id,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalSelector {
    /// Sample from the final ordinary distribution using the legacy seed.
    Distribution,
    /// Choose the greatest final logit; temperature is not a hidden alias.
    Greedy,
    MirostatV1,
    MirostatV2,
}

/// Ordered pre-sampler logit arithmetic. Every scalar is carried by exact
/// IEEE bits; the formulas below are the API, not marketing aliases.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GuidanceControl {
    /// `u + scale * (c - u)`, followed by optional variance rescaling toward
    /// `c` by `rescale` in `[0, 1]`.
    SameModelCfg {
        scale: ExactF32,
        rescale: Option<ExactF32>,
    },
    /// `primary_coefficient * primary + amateur_coefficient * amateur`.
    ContrastiveExpertAmateur {
        amateur_participant_id: String,
        primary_coefficient: ExactF32,
        amateur_coefficient: ExactF32,
    },
    /// `base * current + expert * expert_logits + anti * anti_logits`.
    DExperts {
        expert_participant_id: String,
        anti_expert_participant_id: String,
        base_coefficient: ExactF32,
        expert_coefficient: ExactF32,
        anti_expert_coefficient: ExactF32,
    },
    /// `base * current + reward * reward_model_logits`.
    GenArm {
        reward_participant_id: String,
        base_coefficient: ExactF32,
        reward_coefficient: ExactF32,
    },
    /// Raise the normalized pre-sampler distribution to `exponent` and
    /// renormalize. This is explicitly probability/logit exponentiation.
    PowerSampling { exponent: ExactF32 },
}

impl GuidanceControl {
    fn phase(&self) -> u8 {
        match self {
            Self::SameModelCfg { .. } => 0,
            Self::ContrastiveExpertAmateur { .. } | Self::DExperts { .. } | Self::GenArm { .. } => {
                1
            }
            Self::PowerSampling { .. } => 2,
        }
    }
}

/// A fully bound control program. The writer is the primary distribution;
/// auxiliaries exist only when referenced by exact logit arithmetic.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(try_from = "ControlProgramWire")]
pub struct ControlProgram {
    format: String,
    writer: ControlledModelIdentity,
    auxiliary_models: Vec<ControlledModelIdentity>,
    constraint: Option<StructuredConstraint>,
    guidance: Vec<GuidanceControl>,
    extended_samplers: ExtendedSamplerProgram,
    terminal_selector: TerminalSelector,
    observations: DistributionObservationPolicy,
    static_profiles: Vec<StaticControlProfile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlProgramWire {
    format: String,
    writer: ControlledModelIdentity,
    #[serde(default)]
    auxiliary_models: Vec<ControlledModelIdentity>,
    #[serde(default)]
    constraint: Option<StructuredConstraint>,
    #[serde(default)]
    guidance: Vec<GuidanceControl>,
    #[serde(default)]
    extended_samplers: ExtendedSamplerProgram,
    terminal_selector: TerminalSelector,
    #[serde(default)]
    observations: DistributionObservationPolicy,
    #[serde(default)]
    static_profiles: Vec<StaticControlProfile>,
}

impl ControlProgram {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        writer: ControlledModelIdentity,
        auxiliary_models: Vec<ControlledModelIdentity>,
        constraint: Option<StructuredConstraint>,
        guidance: Vec<GuidanceControl>,
        extended_samplers: ExtendedSamplerProgram,
        terminal_selector: TerminalSelector,
        observations: DistributionObservationPolicy,
        static_profiles: Vec<StaticControlProfile>,
    ) -> Result<Self, NativeError> {
        let program = Self {
            format: CONTROL_PROGRAM_FORMAT.to_string(),
            writer,
            auxiliary_models,
            constraint,
            guidance,
            extended_samplers,
            terminal_selector,
            observations,
            static_profiles,
        };
        program.validate()?;
        Ok(program)
    }

    fn validate(&self) -> Result<(), NativeError> {
        if self.format != CONTROL_PROGRAM_FORMAT {
            return Err(invalid(format!(
                "unsupported control program format {}",
                self.format
            )));
        }
        if self.auxiliary_models.len() + 1 > MAX_CONTROL_MODEL_PARTICIPANTS {
            return Err(invalid(format!(
                "control program cannot contain more than {MAX_CONTROL_MODEL_PARTICIPANTS} model participants"
            )));
        }
        if self.guidance.len() > MAX_GUIDANCE_CONTROLS {
            return Err(invalid(format!(
                "control program cannot contain more than {MAX_GUIDANCE_CONTROLS} guidance controls"
            )));
        }
        if self.extended_samplers.as_slice().len() > MAX_EXTENDED_SAMPLERS {
            return Err(invalid("extended sampler program exceeds its public bound"));
        }
        if self.static_profiles.len() > MAX_STATIC_CONTROL_PROFILES {
            return Err(invalid(format!(
                "control program cannot contain more than {MAX_STATIC_CONTROL_PROFILES} static profiles"
            )));
        }

        let mut participant_ids = BTreeSet::new();
        participant_ids.insert(self.writer.participant_id().to_string());
        for model in &self.auxiliary_models {
            if !participant_ids.insert(model.participant_id().to_string()) {
                return Err(invalid(format!(
                    "duplicate control participant_id {}",
                    model.participant_id()
                )));
            }
            if model.token_contract() != self.writer.token_contract() {
                return Err(invalid(
                    "logit-combining models require identical tokenizer, vocabulary, special-token, and token-byte contracts",
                ));
            }
        }

        self.validate_terminal_selector()?;
        let referenced = self.validate_guidance(&participant_ids)?;
        for auxiliary in &self.auxiliary_models {
            if !referenced.contains(auxiliary.participant_id()) {
                return Err(invalid(format!(
                    "auxiliary model {} is not referenced by logit arithmetic",
                    auxiliary.participant_id()
                )));
            }
        }
        self.validate_static_profiles(&participant_ids)?;
        Ok(())
    }

    fn validate_terminal_selector(&self) -> Result<(), NativeError> {
        let mirostat = self
            .extended_samplers
            .as_slice()
            .last()
            .and_then(|sampler| match sampler.kind() {
                ExtendedSamplerKind::MirostatV1 => Some(TerminalSelector::MirostatV1),
                ExtendedSamplerKind::MirostatV2 => Some(TerminalSelector::MirostatV2),
                _ => None,
            });
        let matches = matches!(
            (self.terminal_selector, mirostat),
            (
                TerminalSelector::MirostatV1,
                Some(TerminalSelector::MirostatV1)
            ) | (
                TerminalSelector::MirostatV2,
                Some(TerminalSelector::MirostatV2)
            ) | (
                TerminalSelector::Distribution | TerminalSelector::Greedy,
                None
            )
        );
        if !matches {
            return Err(invalid(
                "terminal selector must exactly match the extended program's terminal Mirostat operation",
            ));
        }
        Ok(())
    }

    fn validate_guidance(
        &self,
        participant_ids: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, NativeError> {
        let mut previous_phase = 0u8;
        let mut seen_cfg = false;
        let mut seen_combination = false;
        let mut seen_power = false;
        let mut referenced = BTreeSet::new();
        for (index, control) in self.guidance.iter().enumerate() {
            let phase = control.phase();
            if index > 0 && phase < previous_phase {
                return Err(invalid(
                    "guidance controls must be ordered CFG, model arithmetic, then power sampling",
                ));
            }
            previous_phase = phase;
            match control {
                GuidanceControl::SameModelCfg { scale, rescale } => {
                    if std::mem::replace(&mut seen_cfg, true) {
                        return Err(invalid("control program contains duplicate same-model CFG"));
                    }
                    validate_exact_range("CFG scale", *scale, 0.0, MAX_ABS_CONTROL_SCALAR)?;
                    if let Some(rescale) = rescale {
                        validate_exact_range("CFG rescale", *rescale, 0.0, 1.0)?;
                    }
                }
                GuidanceControl::ContrastiveExpertAmateur {
                    amateur_participant_id,
                    primary_coefficient,
                    amateur_coefficient,
                } => {
                    reject_second_combination(&mut seen_combination)?;
                    validate_auxiliary_reference(
                        participant_ids,
                        self.writer.participant_id(),
                        amateur_participant_id,
                    )?;
                    validate_exact_range(
                        "contrastive primary coefficient",
                        *primary_coefficient,
                        f32::MIN_POSITIVE,
                        MAX_ABS_CONTROL_SCALAR,
                    )?;
                    validate_exact_range(
                        "contrastive amateur coefficient",
                        *amateur_coefficient,
                        -MAX_ABS_CONTROL_SCALAR,
                        -f32::MIN_POSITIVE,
                    )?;
                    referenced.insert(amateur_participant_id.clone());
                }
                GuidanceControl::DExperts {
                    expert_participant_id,
                    anti_expert_participant_id,
                    base_coefficient,
                    expert_coefficient,
                    anti_expert_coefficient,
                } => {
                    reject_second_combination(&mut seen_combination)?;
                    if expert_participant_id == anti_expert_participant_id {
                        return Err(invalid(
                            "DExperts expert and anti-expert must be distinct participants",
                        ));
                    }
                    for participant_id in [expert_participant_id, anti_expert_participant_id] {
                        validate_auxiliary_reference(
                            participant_ids,
                            self.writer.participant_id(),
                            participant_id,
                        )?;
                        referenced.insert(participant_id.clone());
                    }
                    validate_exact_range(
                        "DExperts base coefficient",
                        *base_coefficient,
                        f32::MIN_POSITIVE,
                        MAX_ABS_CONTROL_SCALAR,
                    )?;
                    validate_exact_range(
                        "DExperts expert coefficient",
                        *expert_coefficient,
                        f32::MIN_POSITIVE,
                        MAX_ABS_CONTROL_SCALAR,
                    )?;
                    validate_exact_range(
                        "DExperts anti-expert coefficient",
                        *anti_expert_coefficient,
                        -MAX_ABS_CONTROL_SCALAR,
                        -f32::MIN_POSITIVE,
                    )?;
                }
                GuidanceControl::GenArm {
                    reward_participant_id,
                    base_coefficient,
                    reward_coefficient,
                } => {
                    reject_second_combination(&mut seen_combination)?;
                    validate_auxiliary_reference(
                        participant_ids,
                        self.writer.participant_id(),
                        reward_participant_id,
                    )?;
                    validate_exact_range(
                        "GenARM base coefficient",
                        *base_coefficient,
                        f32::MIN_POSITIVE,
                        MAX_ABS_CONTROL_SCALAR,
                    )?;
                    validate_exact_nonzero("GenARM reward coefficient", *reward_coefficient)?;
                    referenced.insert(reward_participant_id.clone());
                }
                GuidanceControl::PowerSampling { exponent } => {
                    if std::mem::replace(&mut seen_power, true) {
                        return Err(invalid("control program contains duplicate power sampling"));
                    }
                    validate_exact_range(
                        "power sampling exponent",
                        *exponent,
                        f32::MIN_POSITIVE,
                        MAX_ABS_CONTROL_SCALAR,
                    )?;
                }
            }
        }
        Ok(referenced)
    }

    fn validate_static_profiles(
        &self,
        participant_ids: &BTreeSet<String>,
    ) -> Result<(), NativeError> {
        let identities = self
            .participants()
            .map(|model| (model.participant_id(), model.identity_sha256()))
            .collect::<BTreeMap<_, _>>();
        let mut profile_ids = BTreeSet::new();
        for profile in &self.static_profiles {
            validate_id("static control profile_id", profile.profile_id())?;
            if !profile_ids.insert(profile.profile_id()) {
                return Err(invalid(format!(
                    "duplicate static control profile_id {}",
                    profile.profile_id()
                )));
            }
            if !participant_ids.contains(profile.participant_id()) {
                return Err(invalid(format!(
                    "static profile references unknown participant {}",
                    profile.participant_id()
                )));
            }
            match profile {
                StaticControlProfile::AdapterStack { adapters, .. } => {
                    if adapters.is_empty() || adapters.len() > MAX_STATIC_ADAPTERS_PER_PROFILE {
                        return Err(invalid(format!(
                            "adapter stack must contain between 1 and {MAX_STATIC_ADAPTERS_PER_PROFILE} adapters"
                        )));
                    }
                    let mut artifacts = BTreeSet::new();
                    for adapter in adapters {
                        if !artifacts.insert(adapter.artifact.sha256()) {
                            return Err(invalid(
                                "adapter stack cannot contain the same artifact twice",
                            ));
                        }
                        validate_exact_nonzero("adapter scale", adapter.scale)?;
                    }
                }
                StaticControlProfile::ActivationVector {
                    participant_id,
                    model_identity_sha256,
                    layer_start,
                    layer_end_inclusive,
                    dimensions,
                    scale,
                    ..
                } => {
                    validate_sha256(
                        "activation vector model_identity_sha256",
                        model_identity_sha256,
                    )?;
                    if identities.get(participant_id.as_str()) != Some(model_identity_sha256) {
                        return Err(invalid(
                            "activation vector model identity does not match its participant",
                        ));
                    }
                    if layer_start > layer_end_inclusive || *dimensions == 0 {
                        return Err(invalid(
                            "activation vector requires an ordered non-empty layer range and positive dimensions",
                        ));
                    }
                    validate_exact_nonzero("activation vector scale", *scale)?;
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn format(&self) -> &str {
        &self.format
    }

    #[must_use]
    pub const fn writer(&self) -> &ControlledModelIdentity {
        &self.writer
    }

    pub fn participants(&self) -> impl Iterator<Item = &ControlledModelIdentity> {
        std::iter::once(&self.writer).chain(self.auxiliary_models.iter())
    }

    #[must_use]
    pub fn auxiliary_models(&self) -> &[ControlledModelIdentity] {
        &self.auxiliary_models
    }

    #[must_use]
    pub const fn constraint(&self) -> Option<&StructuredConstraint> {
        self.constraint.as_ref()
    }

    #[must_use]
    pub fn guidance(&self) -> &[GuidanceControl] {
        &self.guidance
    }

    #[must_use]
    pub const fn extended_samplers(&self) -> &ExtendedSamplerProgram {
        &self.extended_samplers
    }

    #[must_use]
    pub const fn terminal_selector(&self) -> TerminalSelector {
        self.terminal_selector
    }

    #[must_use]
    pub const fn observations(&self) -> &DistributionObservationPolicy {
        &self.observations
    }

    #[must_use]
    pub fn static_profiles(&self) -> &[StaticControlProfile] {
        &self.static_profiles
    }

    #[must_use]
    pub fn uses_same_model_cfg(&self) -> bool {
        self.guidance
            .iter()
            .any(|control| matches!(control, GuidanceControl::SameModelCfg { .. }))
    }

    #[must_use]
    pub fn fingerprint_sha256(&self) -> String {
        let mut digest = StableDigest::new("control-program-v1");
        hash_control_program(&mut digest, self);
        digest.finish()
    }
}

impl TryFrom<ControlProgramWire> for ControlProgram {
    type Error = NativeError;

    fn try_from(value: ControlProgramWire) -> Result<Self, Self::Error> {
        let program = Self {
            format: value.format,
            writer: value.writer,
            auxiliary_models: value.auxiliary_models,
            constraint: value.constraint,
            guidance: value.guidance,
            extended_samplers: value.extended_samplers,
            terminal_selector: value.terminal_selector,
            observations: value.observations,
            static_profiles: value.static_profiles,
        };
        program.validate()?;
        Ok(program)
    }
}

fn reject_second_combination(seen: &mut bool) -> Result<(), NativeError> {
    if std::mem::replace(seen, true) {
        return Err(invalid(
            "control program may contain only one multi-model logit combination",
        ));
    }
    Ok(())
}

fn validate_auxiliary_reference(
    participants: &BTreeSet<String>,
    writer_id: &str,
    participant_id: &str,
) -> Result<(), NativeError> {
    validate_id("guidance participant_id", participant_id)?;
    if participant_id == writer_id || !participants.contains(participant_id) {
        return Err(invalid(format!(
            "guidance references unknown or primary participant {participant_id} as an auxiliary"
        )));
    }
    Ok(())
}

fn validate_exact_range(
    field: &str,
    value: ExactF32,
    minimum: f32,
    maximum: f32,
) -> Result<(), NativeError> {
    let value = value.get();
    if value < minimum || value > maximum {
        return Err(invalid(format!(
            "{field} must be between {minimum} and {maximum}"
        )));
    }
    Ok(())
}

fn validate_exact_nonzero(field: &str, value: ExactF32) -> Result<(), NativeError> {
    if value.get() == 0.0 || value.get().abs() > MAX_ABS_CONTROL_SCALAR {
        return Err(invalid(format!(
            "{field} must be non-zero with magnitude at most {MAX_ABS_CONTROL_SCALAR}"
        )));
    }
    Ok(())
}

/// Derived maximum resource reservation for one model context family.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModelParticipationCost {
    participant_id: String,
    conditional_sequences: u32,
    unconditional_sequences: u32,
    exact_prompt_tokens: u64,
    maximum_generated_evaluations: u64,
    maximum_context_token_positions: u64,
}

impl ModelParticipationCost {
    #[must_use]
    pub fn participant_id(&self) -> &str {
        &self.participant_id
    }

    #[must_use]
    pub const fn conditional_sequences(&self) -> u32 {
        self.conditional_sequences
    }

    #[must_use]
    pub const fn unconditional_sequences(&self) -> u32 {
        self.unconditional_sequences
    }

    #[must_use]
    pub const fn exact_prompt_tokens(&self) -> u64 {
        self.exact_prompt_tokens
    }

    #[must_use]
    pub const fn maximum_generated_evaluations(&self) -> u64 {
        self.maximum_generated_evaluations
    }

    #[must_use]
    pub const fn maximum_context_token_positions(&self) -> u64 {
        self.maximum_context_token_positions
    }
}

/// Cost is computed from cases and participants, serialized, and revalidated
/// on deserialize. A caller cannot understate doubled CFG or auxiliary models.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlCostAccounting {
    participants: Vec<ModelParticipationCost>,
    total_model_contexts: u32,
    total_sequence_slots: u32,
    exact_prompt_tokens: u64,
    maximum_generated_evaluations: u64,
    maximum_context_token_positions: u64,
}

impl ControlCostAccounting {
    #[must_use]
    pub fn participants(&self) -> &[ModelParticipationCost] {
        &self.participants
    }

    #[must_use]
    pub const fn total_model_contexts(&self) -> u32 {
        self.total_model_contexts
    }

    #[must_use]
    pub const fn total_sequence_slots(&self) -> u32 {
        self.total_sequence_slots
    }

    #[must_use]
    pub const fn exact_prompt_tokens(&self) -> u64 {
        self.exact_prompt_tokens
    }

    #[must_use]
    pub const fn maximum_generated_evaluations(&self) -> u64 {
        self.maximum_generated_evaluations
    }

    #[must_use]
    pub const fn maximum_context_token_positions(&self) -> u64 {
        self.maximum_context_token_positions
    }
}

/// Ordered controlled request with a derived, serialized reservation.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ControlledGenerationBatchRequest {
    request_id: String,
    cases: Vec<ControlledGenerationCase>,
    control: ControlProgram,
    cost: ControlCostAccounting,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlledGenerationBatchRequestWire {
    request_id: String,
    cases: Vec<ControlledGenerationCase>,
    control: ControlProgram,
    cost: ControlCostAccounting,
}

impl ControlledGenerationBatchRequest {
    pub fn new(
        request_id: String,
        cases: Vec<ControlledGenerationCase>,
        control: ControlProgram,
    ) -> Result<Self, NativeError> {
        validate_id("controlled request_id", &request_id)?;
        validate_cases(&cases, &control)?;
        let cost = derive_cost(&cases, &control)?;
        Ok(Self {
            request_id,
            cases,
            control,
            cost,
        })
    }

    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    #[must_use]
    pub fn cases(&self) -> &[ControlledGenerationCase] {
        &self.cases
    }

    #[must_use]
    pub const fn control(&self) -> &ControlProgram {
        &self.control
    }

    #[must_use]
    pub const fn cost(&self) -> &ControlCostAccounting {
        &self.cost
    }

    #[must_use]
    pub fn fingerprint_sha256(&self) -> String {
        let mut digest = StableDigest::new("controlled-generation-request-v1");
        digest.text(&self.request_id);
        hash_control_program(&mut digest, &self.control);
        for case in &self.cases {
            hash_case(&mut digest, case);
        }
        hash_cost(&mut digest, &self.cost);
        digest.finish()
    }

    #[must_use]
    pub fn exact_float_bits_sha256(&self) -> String {
        let mut digest = StableDigest::new("controlled-generation-float-bits-v1");
        hash_control_float_bits(&mut digest, &self.control);
        for case in &self.cases {
            for bits in sampling_float_bits(&case.sampling) {
                digest.u32(bits);
            }
        }
        digest.finish()
    }
}

impl<'de> Deserialize<'de> for ControlledGenerationBatchRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = ControlledGenerationBatchRequestWire::deserialize(deserializer)?;
        let request = Self::new(wire.request_id, wire.cases, wire.control)
            .map_err(serde::de::Error::custom)?;
        if request.cost != wire.cost {
            return Err(serde::de::Error::custom(
                "serialized controlled-generation cost does not match derived resource use",
            ));
        }
        Ok(request)
    }
}

fn validate_cases(
    cases: &[ControlledGenerationCase],
    control: &ControlProgram,
) -> Result<(), NativeError> {
    if cases.is_empty() || cases.len() > MAX_CONTROLLED_BATCH_CASES {
        return Err(invalid(format!(
            "controlled batch must contain between 1 and {MAX_CONTROLLED_BATCH_CASES} cases"
        )));
    }
    let mut case_ids = BTreeSet::new();
    let mut prompt_tokens = 0usize;
    let logical_case_count =
        u32::try_from(cases.len()).map_err(|_| invalid("controlled batch case count overflow"))?;
    let writer_sequences = logical_case_count
        .checked_mul(1 + u32::from(control.uses_same_model_cfg()))
        .ok_or_else(|| invalid("controlled writer sequence count overflow"))?;
    if writer_sequences > control.writer().fingerprint().max_sequences {
        return Err(invalid(
            "controlled writer logical sequences exceed its fingerprinted sequence capacity",
        ));
    }
    if control
        .auxiliary_models()
        .iter()
        .any(|model| logical_case_count > model.fingerprint().max_sequences)
    {
        return Err(invalid(
            "controlled auxiliary logical sequences exceed a fingerprinted sequence capacity",
        ));
    }
    for case in cases {
        if !case_ids.insert(case.case_id()) {
            return Err(invalid(format!(
                "controlled batch contains duplicate case_id {}",
                case.case_id()
            )));
        }
        let has_unconditional = case.unconditional_prompt().is_some();
        if has_unconditional != control.uses_same_model_cfg() {
            return Err(invalid(
                "every case must carry an unconditional prompt exactly when same-model CFG is active",
            ));
        }
        let generated = usize::try_from(case.sampling().max_tokens)
            .map_err(|_| invalid("controlled max_tokens conversion overflow"))?;
        for model in control.participants() {
            let context = usize::try_from(model.fingerprint().context_tokens)
                .map_err(|_| invalid("controlled model context conversion overflow"))?;
            if case
                .conditional_prompt()
                .token_ids()
                .len()
                .checked_add(generated)
                .is_none_or(|required| required > context)
            {
                return Err(invalid(format!(
                    "case {} conditional prompt and completion exceed participant {} context",
                    case.case_id(),
                    model.participant_id()
                )));
            }
        }
        if let Some(unconditional) = case.unconditional_prompt() {
            let writer_context = usize::try_from(control.writer().fingerprint().context_tokens)
                .map_err(|_| invalid("controlled writer context conversion overflow"))?;
            if unconditional
                .token_ids()
                .len()
                .checked_add(generated)
                .is_none_or(|required| required > writer_context)
            {
                return Err(invalid(format!(
                    "case {} unconditional prompt and completion exceed writer context",
                    case.case_id()
                )));
            }
        }
        prompt_tokens = prompt_tokens
            .checked_add(case.conditional_prompt().token_ids().len())
            .and_then(|count| {
                case.unconditional_prompt().map_or(Some(count), |prompt| {
                    count.checked_add(prompt.token_ids().len())
                })
            })
            .ok_or_else(|| invalid("controlled batch prompt token count overflow"))?;
    }
    if prompt_tokens > MAX_CONTROL_BATCH_PROMPT_TOKENS {
        return Err(invalid(format!(
            "controlled batch prompts cannot exceed {MAX_CONTROL_BATCH_PROMPT_TOKENS} total token IDs"
        )));
    }
    Ok(())
}

fn derive_cost(
    cases: &[ControlledGenerationCase],
    control: &ControlProgram,
) -> Result<ControlCostAccounting, NativeError> {
    let conditional_sequences = u32::try_from(cases.len())
        .map_err(|_| invalid("controlled batch sequence count overflow"))?;
    let conditional_prompt_tokens = sum_u64(
        cases
            .iter()
            .map(|case| case.conditional_prompt().token_ids().len() as u64),
    )?;
    let unconditional_prompt_tokens = sum_u64(cases.iter().filter_map(|case| {
        case.unconditional_prompt()
            .map(|prompt| prompt.token_ids().len() as u64)
    }))?;
    let generated = sum_u64(
        cases
            .iter()
            .map(|case| u64::from(case.sampling().max_tokens)),
    )?;
    let mut participants = Vec::with_capacity(control.auxiliary_models().len() + 1);
    for (index, model) in control.participants().enumerate() {
        let unconditional_sequences = if index == 0 && control.uses_same_model_cfg() {
            conditional_sequences
        } else {
            0
        };
        let extra_prompt = if unconditional_sequences > 0 {
            unconditional_prompt_tokens
        } else {
            0
        };
        let multiplier = u64::from(1 + u32::from(unconditional_sequences > 0));
        let max_generated = generated
            .checked_mul(multiplier)
            .ok_or_else(|| invalid("controlled generation evaluation count overflow"))?;
        let prompt = conditional_prompt_tokens
            .checked_add(extra_prompt)
            .ok_or_else(|| invalid("controlled generation prompt cost overflow"))?;
        participants.push(ModelParticipationCost {
            participant_id: model.participant_id().to_string(),
            conditional_sequences,
            unconditional_sequences,
            exact_prompt_tokens: prompt,
            maximum_generated_evaluations: max_generated,
            maximum_context_token_positions: prompt
                .checked_add(max_generated)
                .ok_or_else(|| invalid("controlled generation context cost overflow"))?,
        });
    }
    let total_model_contexts = u32::try_from(participants.len())
        .map_err(|_| invalid("controlled model context count overflow"))?;
    let total_sequence_slots = sum_u64(participants.iter().map(|cost| {
        u64::from(cost.conditional_sequences) + u64::from(cost.unconditional_sequences)
    }))?;
    Ok(ControlCostAccounting {
        total_model_contexts,
        total_sequence_slots: u32::try_from(total_sequence_slots)
            .map_err(|_| invalid("controlled sequence slot count overflow"))?,
        exact_prompt_tokens: sum_u64(participants.iter().map(|cost| cost.exact_prompt_tokens))?,
        maximum_generated_evaluations: sum_u64(
            participants
                .iter()
                .map(|cost| cost.maximum_generated_evaluations),
        )?,
        maximum_context_token_positions: sum_u64(
            participants
                .iter()
                .map(|cost| cost.maximum_context_token_positions),
        )?,
        participants,
    })
}

fn sum_u64(mut values: impl Iterator<Item = u64>) -> Result<u64, NativeError> {
    values.try_fold(0u64, |sum, value| {
        sum.checked_add(value)
            .ok_or_else(|| invalid("controlled cost total overflow"))
    })
}

/// Rich output for one controlled case. Distribution observations are kept
/// separate from the compatibility `GenerationOutput` so no legacy field is
/// relabeled.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ControlledGenerationCaseOutput {
    case_id: String,
    generation: GenerationOutput,
    distribution_observations: Vec<TokenDistributionObservation>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlledGenerationCaseOutputWire {
    case_id: String,
    generation: StrictGenerationOutputWire,
    distribution_observations: Vec<TokenDistributionObservation>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictGenerationOutputWire {
    request_id: String,
    branch_id: String,
    input_index: usize,
    model_id: String,
    text: String,
    generated_token_ids: Vec<i32>,
    #[serde(default)]
    token_observations: Option<Vec<StrictTokenObservationWire>>,
    state: GenerationState,
    finish_reason: String,
    metrics: StrictGenerationMetricsWire,
    real_engine_invoked: bool,
    fake_fixture: bool,
    transport: NativeTransport,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictTokenObservationWire {
    generated_index: usize,
    token_id: i32,
    probabilities: Vec<StrictTokenProbabilityObservationWire>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictTokenProbabilityObservationWire {
    stage: ProbabilityStage,
    probability: f32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictGenerationMetricsWire {
    prompt_tokens: usize,
    completion_tokens: usize,
    shared_prefix_tokens: usize,
    duration_ms: u128,
    first_token_ms: Option<u128>,
    tokens_per_second: f64,
    cache: StrictGenerationCacheMetricsWire,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictGenerationCacheMetricsWire {
    supplied_prefix_tokens: usize,
    restored_prefix_tokens: usize,
    batch_shared_prefix_tokens: usize,
}

impl From<StrictGenerationOutputWire> for GenerationOutput {
    fn from(value: StrictGenerationOutputWire) -> Self {
        Self {
            request_id: value.request_id,
            branch_id: value.branch_id,
            input_index: value.input_index,
            model_id: value.model_id,
            text: value.text,
            generated_token_ids: value.generated_token_ids,
            token_observations: value.token_observations.map(|observations| {
                observations
                    .into_iter()
                    .map(|observation| TokenObservation {
                        generated_index: observation.generated_index,
                        token_id: observation.token_id,
                        probabilities: observation
                            .probabilities
                            .into_iter()
                            .map(|probability| TokenProbabilityObservation {
                                stage: probability.stage,
                                probability: probability.probability,
                            })
                            .collect(),
                    })
                    .collect()
            }),
            state: value.state,
            finish_reason: value.finish_reason,
            metrics: GenerationMetrics {
                prompt_tokens: value.metrics.prompt_tokens,
                completion_tokens: value.metrics.completion_tokens,
                shared_prefix_tokens: value.metrics.shared_prefix_tokens,
                duration_ms: value.metrics.duration_ms,
                first_token_ms: value.metrics.first_token_ms,
                tokens_per_second: value.metrics.tokens_per_second,
                cache: GenerationCacheMetrics {
                    supplied_prefix_tokens: value.metrics.cache.supplied_prefix_tokens,
                    restored_prefix_tokens: value.metrics.cache.restored_prefix_tokens,
                    batch_shared_prefix_tokens: value.metrics.cache.batch_shared_prefix_tokens,
                },
            },
            real_engine_invoked: value.real_engine_invoked,
            fake_fixture: value.fake_fixture,
            transport: value.transport,
        }
    }
}

impl ControlledGenerationCaseOutput {
    pub fn new(
        case_id: String,
        generation: GenerationOutput,
        distribution_observations: Vec<TokenDistributionObservation>,
    ) -> Result<Self, NativeError> {
        validate_id("controlled output case_id", &case_id)?;
        if distribution_observations.len() > MAX_GENERATION_TOKENS_PER_CASE as usize {
            return Err(invalid(format!(
                "controlled case output cannot contain more than {MAX_GENERATION_TOKENS_PER_CASE} distribution observations"
            )));
        }
        Ok(Self {
            case_id,
            generation,
            distribution_observations,
        })
    }

    #[must_use]
    pub fn case_id(&self) -> &str {
        &self.case_id
    }

    #[must_use]
    pub const fn generation(&self) -> &GenerationOutput {
        &self.generation
    }

    #[must_use]
    pub fn distribution_observations(&self) -> &[TokenDistributionObservation] {
        &self.distribution_observations
    }
}

impl<'de> Deserialize<'de> for ControlledGenerationCaseOutput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = ControlledGenerationCaseOutputWire::deserialize(deserializer)?;
        Self::new(
            wire.case_id,
            wire.generation.into(),
            wire.distribution_observations,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum AppliedControlKind {
    StructuredConstraint,
    SameModelCfg,
    ContrastiveExpertAmateur,
    DExperts,
    GenArm,
    PowerSampling,
    EtaCutoff,
    SparseLogitBias,
    TopNSigma,
    MirostatV1,
    MirostatV2,
    OrdinaryDistributionSelector,
    GreedySelector,
    AdapterStack,
    StaticActivationVector,
    DistributionObservation,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlApplicationStage {
    ModelLoad,
    ModelEvaluation,
    Constraint,
    Guidance,
    Sampler,
    EvidenceCapture,
}

/// One operation a backend must report independently. This is a plan, not
/// evidence that the operation ran.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExpectedControlApplication {
    operation_index: u16,
    kind: AppliedControlKind,
    stage: ControlApplicationStage,
}

impl ExpectedControlApplication {
    #[must_use]
    pub const fn operation_index(self) -> u16 {
        self.operation_index
    }

    #[must_use]
    pub const fn kind(self) -> AppliedControlKind {
        self.kind
    }

    #[must_use]
    pub const fn stage(self) -> ControlApplicationStage {
        self.stage
    }
}

impl ControlProgram {
    /// Exact ordered application plan a backend must independently account for.
    #[must_use]
    pub fn expected_application_plan(&self) -> Vec<ExpectedControlApplication> {
        expected_application_plan(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "BackendParticipantReportWire")]
pub struct BackendParticipantReport {
    participant_id: String,
    identity_sha256: String,
    model_load_evidence_sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BackendParticipantReportWire {
    participant_id: String,
    identity_sha256: String,
    model_load_evidence_sha256: String,
}

impl BackendParticipantReport {
    pub fn new(
        participant_id: String,
        identity_sha256: String,
        model_load_evidence_sha256: String,
    ) -> Result<Self, NativeError> {
        validate_id("backend participant_id", &participant_id)?;
        validate_sha256("backend participant identity_sha256", &identity_sha256)?;
        validate_sha256(
            "backend participant model_load_evidence_sha256",
            &model_load_evidence_sha256,
        )?;
        Ok(Self {
            participant_id,
            identity_sha256,
            model_load_evidence_sha256,
        })
    }

    #[must_use]
    pub fn participant_id(&self) -> &str {
        &self.participant_id
    }

    #[must_use]
    pub fn identity_sha256(&self) -> &str {
        &self.identity_sha256
    }

    #[must_use]
    pub fn model_load_evidence_sha256(&self) -> &str {
        &self.model_load_evidence_sha256
    }
}

impl TryFrom<BackendParticipantReportWire> for BackendParticipantReport {
    type Error = NativeError;

    fn try_from(value: BackendParticipantReportWire) -> Result<Self, Self::Error> {
        Self::new(
            value.participant_id,
            value.identity_sha256,
            value.model_load_evidence_sha256,
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "AppliedControlReportWire")]
pub struct AppliedControlReport {
    operation_index: u16,
    kind: AppliedControlKind,
    stage: ControlApplicationStage,
    operation_evidence_sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppliedControlReportWire {
    operation_index: u16,
    kind: AppliedControlKind,
    stage: ControlApplicationStage,
    operation_evidence_sha256: String,
}

impl AppliedControlReport {
    pub fn new(
        operation_index: u16,
        kind: AppliedControlKind,
        stage: ControlApplicationStage,
        operation_evidence_sha256: String,
    ) -> Result<Self, NativeError> {
        validate_sha256(
            "applied operation evidence sha256",
            &operation_evidence_sha256,
        )?;
        Ok(Self {
            operation_index,
            kind,
            stage,
            operation_evidence_sha256,
        })
    }

    #[must_use]
    pub const fn operation_index(&self) -> u16 {
        self.operation_index
    }

    #[must_use]
    pub const fn kind(&self) -> AppliedControlKind {
        self.kind
    }

    #[must_use]
    pub const fn stage(&self) -> ControlApplicationStage {
        self.stage
    }

    #[must_use]
    pub fn operation_evidence_sha256(&self) -> &str {
        &self.operation_evidence_sha256
    }
}

impl TryFrom<AppliedControlReportWire> for AppliedControlReport {
    type Error = NativeError;

    fn try_from(value: AppliedControlReportWire) -> Result<Self, Self::Error> {
        Self::new(
            value.operation_index,
            value.kind,
            value.stage,
            value.operation_evidence_sha256,
        )
    }
}

/// Raw backend declaration. It is deliberately named unverified: construction
/// does not confer admission or prove that an operation ran. A downstream
/// backend/event verifier must bind these digests before it may construct a
/// verified inference envelope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "UnverifiedBackendControlDeclarationWire")]
pub struct UnverifiedBackendControlDeclaration {
    transport: NativeTransport,
    real_engine_invoked: bool,
    fake_fixture: bool,
    applied_control_sha256: String,
    backend_event_stream_sha256: String,
    participant_reports: Vec<BackendParticipantReport>,
    applied_operations: Vec<AppliedControlReport>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnverifiedBackendControlDeclarationWire {
    transport: NativeTransport,
    real_engine_invoked: bool,
    fake_fixture: bool,
    applied_control_sha256: String,
    backend_event_stream_sha256: String,
    participant_reports: Vec<BackendParticipantReport>,
    applied_operations: Vec<AppliedControlReport>,
}

impl UnverifiedBackendControlDeclaration {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        transport: NativeTransport,
        real_engine_invoked: bool,
        fake_fixture: bool,
        applied_control_sha256: String,
        backend_event_stream_sha256: String,
        participant_reports: Vec<BackendParticipantReport>,
        applied_operations: Vec<AppliedControlReport>,
    ) -> Result<Self, NativeError> {
        let coherent = match transport {
            NativeTransport::InProcess => real_engine_invoked && !fake_fixture,
            NativeTransport::FakeFixture => !real_engine_invoked && fake_fixture,
        };
        if !coherent {
            return Err(invalid(
                "backend declaration transport and engine/fixture fields disagree",
            ));
        }
        validate_sha256("backend applied_control_sha256", &applied_control_sha256)?;
        validate_sha256("backend event stream sha256", &backend_event_stream_sha256)?;
        if participant_reports.is_empty()
            || participant_reports.len() > MAX_CONTROL_MODEL_PARTICIPANTS
        {
            return Err(invalid(format!(
                "backend declaration must report between 1 and {MAX_CONTROL_MODEL_PARTICIPANTS} model participants"
            )));
        }
        if applied_operations.len() > MAX_CONTROL_APPLICATION_REPORTS {
            return Err(invalid(format!(
                "backend declaration cannot contain more than {MAX_CONTROL_APPLICATION_REPORTS} applied-operation reports"
            )));
        }
        let mut participant_ids = BTreeSet::new();
        if participant_reports
            .iter()
            .any(|report| !participant_ids.insert(report.participant_id()))
        {
            return Err(invalid(
                "backend declaration contains duplicate participant reports",
            ));
        }
        let mut expected_index = 0u16;
        for report in &applied_operations {
            if report.operation_index() != expected_index {
                return Err(invalid(
                    "backend applied-operation reports require contiguous zero-based indexes",
                ));
            }
            expected_index = expected_index
                .checked_add(1)
                .ok_or_else(|| invalid("backend applied-operation index overflow"))?;
        }
        Ok(Self {
            transport,
            real_engine_invoked,
            fake_fixture,
            applied_control_sha256,
            backend_event_stream_sha256,
            participant_reports,
            applied_operations,
        })
    }

    #[must_use]
    pub const fn transport(&self) -> NativeTransport {
        self.transport
    }

    #[must_use]
    pub const fn real_engine_invoked(&self) -> bool {
        self.real_engine_invoked
    }

    #[must_use]
    pub const fn fake_fixture(&self) -> bool {
        self.fake_fixture
    }

    #[must_use]
    pub fn applied_control_sha256(&self) -> &str {
        &self.applied_control_sha256
    }

    #[must_use]
    pub fn backend_event_stream_sha256(&self) -> &str {
        &self.backend_event_stream_sha256
    }

    #[must_use]
    pub fn participant_reports(&self) -> &[BackendParticipantReport] {
        &self.participant_reports
    }

    #[must_use]
    pub fn applied_operations(&self) -> &[AppliedControlReport] {
        &self.applied_operations
    }
}

impl TryFrom<UnverifiedBackendControlDeclarationWire> for UnverifiedBackendControlDeclaration {
    type Error = NativeError;

    fn try_from(value: UnverifiedBackendControlDeclarationWire) -> Result<Self, Self::Error> {
        Self::new(
            value.transport,
            value.real_engine_invoked,
            value.fake_fixture,
            value.applied_control_sha256,
            value.backend_event_stream_sha256,
            value.participant_reports,
            value.applied_operations,
        )
    }
}

/// Immutable but explicitly unverified receipt binding raw backend declarations
/// to exact request and output bytes. This is not an admission credential.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UnverifiedAppliedControlReceipt {
    request_sha256: String,
    requested_control_sha256: String,
    exact_float_bits_sha256: String,
    output_sha256: String,
    backend_declaration_sha256: String,
    backend_declaration: UnverifiedBackendControlDeclaration,
}

impl UnverifiedAppliedControlReceipt {
    #[must_use]
    pub fn request_sha256(&self) -> &str {
        &self.request_sha256
    }

    #[must_use]
    pub fn requested_control_sha256(&self) -> &str {
        &self.requested_control_sha256
    }

    #[must_use]
    pub fn exact_float_bits_sha256(&self) -> &str {
        &self.exact_float_bits_sha256
    }

    #[must_use]
    pub fn output_sha256(&self) -> &str {
        &self.output_sha256
    }

    #[must_use]
    pub fn backend_declaration_sha256(&self) -> &str {
        &self.backend_declaration_sha256
    }

    #[must_use]
    pub const fn backend_declaration(&self) -> &UnverifiedBackendControlDeclaration {
        &self.backend_declaration
    }
}

/// Self-contained controlled output. Carrying the frozen request permits
/// deserialization to verify all receipt digests rather than trusting labels.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ControlledGenerationBatchOutput {
    request: ControlledGenerationBatchRequest,
    cases: Vec<ControlledGenerationCaseOutput>,
    receipt: UnverifiedAppliedControlReceipt,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlledGenerationBatchOutputWire {
    request: ControlledGenerationBatchRequest,
    cases: Vec<ControlledGenerationCaseOutput>,
    receipt: UnverifiedAppliedControlReceipt,
}

impl ControlledGenerationBatchOutput {
    pub fn new(
        request: ControlledGenerationBatchRequest,
        cases: Vec<ControlledGenerationCaseOutput>,
        backend_declaration: UnverifiedBackendControlDeclaration,
    ) -> Result<Self, NativeError> {
        validate_backend_declaration(&request, &backend_declaration)?;
        validate_outputs(&request, &cases, &backend_declaration)?;
        let receipt = build_unverified_receipt(&request, &cases, backend_declaration);
        Ok(Self {
            request,
            cases,
            receipt,
        })
    }

    #[must_use]
    pub const fn request(&self) -> &ControlledGenerationBatchRequest {
        &self.request
    }

    #[must_use]
    pub fn cases(&self) -> &[ControlledGenerationCaseOutput] {
        &self.cases
    }

    #[must_use]
    pub const fn receipt(&self) -> &UnverifiedAppliedControlReceipt {
        &self.receipt
    }
}

impl<'de> Deserialize<'de> for ControlledGenerationBatchOutput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = ControlledGenerationBatchOutputWire::deserialize(deserializer)?;
        let expected = Self::new(
            wire.request,
            wire.cases,
            wire.receipt.backend_declaration.clone(),
        )
        .map_err(serde::de::Error::custom)?;
        if expected.receipt != wire.receipt {
            return Err(serde::de::Error::custom(
                "controlled-generation receipt does not match request, controls, identities, float bits, or output",
            ));
        }
        Ok(expected)
    }
}

fn validate_outputs(
    request: &ControlledGenerationBatchRequest,
    outputs: &[ControlledGenerationCaseOutput],
    backend: &UnverifiedBackendControlDeclaration,
) -> Result<(), NativeError> {
    if outputs.len() != request.cases().len() {
        return Err(invalid(
            "controlled output must contain exactly one result per request case",
        ));
    }
    for (index, (case, output)) in request.cases().iter().zip(outputs).enumerate() {
        if output.case_id() != case.case_id()
            || output.generation().branch_id != case.case_id()
            || output.generation().input_index != index
            || output.generation().request_id != request.request_id()
        {
            return Err(invalid(
                "controlled outputs must preserve request case IDs and exact submitted order",
            ));
        }
        if output.generation().model_id != request.control().writer().fingerprint().model_id {
            return Err(invalid(
                "controlled output model_id does not match the exact writer identity",
            ));
        }
        if output.generation().transport != backend.transport()
            || output.generation().real_engine_invoked != backend.real_engine_invoked()
            || output.generation().fake_fixture != backend.fake_fixture()
        {
            return Err(invalid(
                "controlled output transport evidence disagrees with its receipt",
            ));
        }
        if !matches!(
            output.generation().state,
            GenerationState::Completed | GenerationState::Cancelled | GenerationState::Failed
        ) {
            return Err(invalid("controlled output state must be terminal"));
        }
        if output.generation().generated_token_ids.len() > case.sampling().max_tokens as usize
            || output
                .generation()
                .generated_token_ids
                .iter()
                .any(|token_id| *token_id < 0)
        {
            return Err(invalid(
                "controlled output token evidence exceeds its limit or contains a negative ID",
            ));
        }
        if output.generation().metrics.completion_tokens
            != output.generation().generated_token_ids.len()
            || !output.generation().metrics.tokens_per_second.is_finite()
            || output.generation().metrics.tokens_per_second < 0.0
            || output
                .generation()
                .metrics
                .first_token_ms
                .is_some_and(|first| first > output.generation().metrics.duration_ms)
        {
            return Err(invalid(
                "controlled output metrics must be finite and agree with token evidence and timing",
            ));
        }
        let cache = &output.generation().metrics.cache;
        let reused = cache
            .restored_prefix_tokens
            .checked_add(cache.batch_shared_prefix_tokens)
            .ok_or_else(|| invalid("controlled output cache accounting overflow"))?;
        if cache.restored_prefix_tokens > cache.supplied_prefix_tokens
            || reused != output.generation().metrics.shared_prefix_tokens
            || reused > output.generation().metrics.prompt_tokens
        {
            return Err(invalid(
                "controlled output cache metrics must reconcile exactly",
            ));
        }
        validate_legacy_token_observations(output.generation())?;
        validate_distribution_observations(
            request.control().observations(),
            &output.generation().generated_token_ids,
            output.distribution_observations(),
        )?;
    }
    Ok(())
}

fn validate_legacy_token_observations(generation: &GenerationOutput) -> Result<(), NativeError> {
    let Some(observations) = &generation.token_observations else {
        return Ok(());
    };
    if observations.len() != generation.generated_token_ids.len() {
        return Err(invalid(
            "legacy token observations must cover every generated token when present",
        ));
    }
    for (index, (token_id, observation)) in generation
        .generated_token_ids
        .iter()
        .zip(observations)
        .enumerate()
    {
        if observation.generated_index != index || observation.token_id != *token_id {
            return Err(invalid(
                "legacy token observations must join exactly to generated token IDs",
            ));
        }
        let mut stages = HashSet::new();
        for probability in &observation.probabilities {
            if !stages.insert(probability.stage)
                || !probability.probability.is_finite()
                || !(0.0..=1.0).contains(&probability.probability)
            {
                return Err(invalid(
                    "legacy token probabilities must be finite, bounded, and unique by stage",
                ));
            }
        }
    }
    Ok(())
}

fn validate_distribution_observations(
    policy: &DistributionObservationPolicy,
    generated_token_ids: &[i32],
    observations: &[TokenDistributionObservation],
) -> Result<(), NativeError> {
    if policy.is_disabled() {
        if !observations.is_empty() {
            return Err(invalid(
                "disabled distribution observation policy cannot produce evidence",
            ));
        }
        return Ok(());
    }
    if observations.len() != generated_token_ids.len() {
        return Err(invalid(
            "enabled distribution observations require one evidence record per generated token",
        ));
    }
    let expected_pairs = policy
        .stages()
        .iter()
        .flat_map(|stage| policy.value_kinds().iter().map(move |kind| (*stage, *kind)))
        .collect::<HashSet<_>>();
    for (index, (token_id, observation)) in generated_token_ids.iter().zip(observations).enumerate()
    {
        if observation.generated_index() != index || observation.token_id() != *token_id {
            return Err(invalid(
                "distribution observations must join exactly to generated token evidence",
            ));
        }
        let actual_pairs = observation
            .observations()
            .iter()
            .map(|stage| (stage.stage(), stage.value_kind()))
            .collect::<HashSet<_>>();
        if actual_pairs != expected_pairs {
            return Err(invalid(
                "distribution evidence does not contain the exact requested stage/value-kind product",
            ));
        }
        if observation
            .observations()
            .iter()
            .any(|stage| stage.ranked_candidates().len() != usize::from(policy.top_k()))
        {
            return Err(invalid(
                "distribution evidence top-k width does not match the request policy",
            ));
        }
    }
    Ok(())
}

fn validate_backend_declaration(
    request: &ControlledGenerationBatchRequest,
    backend: &UnverifiedBackendControlDeclaration,
) -> Result<(), NativeError> {
    if backend.applied_control_sha256() != request.control().fingerprint_sha256() {
        return Err(invalid(
            "backend applied-control digest does not match the frozen control program",
        ));
    }
    let expected_participants = request
        .control()
        .participants()
        .map(|participant| (participant.participant_id(), participant.identity_sha256()))
        .collect::<Vec<_>>();
    if backend.participant_reports().len() != expected_participants.len()
        || backend
            .participant_reports()
            .iter()
            .zip(expected_participants)
            .any(|(report, (participant_id, identity_sha256))| {
                report.participant_id() != participant_id
                    || report.identity_sha256() != identity_sha256
            })
    {
        return Err(invalid(
            "backend participant reports do not match the frozen ordered model identities",
        ));
    }
    let expected_plan = request.control().expected_application_plan();
    if backend.applied_operations().len() != expected_plan.len()
        || backend
            .applied_operations()
            .iter()
            .zip(expected_plan)
            .any(|(report, expected)| {
                report.operation_index() != expected.operation_index()
                    || report.kind() != expected.kind()
                    || report.stage() != expected.stage()
            })
    {
        return Err(invalid(
            "backend must independently report every requested operation at its declared stage",
        ));
    }
    Ok(())
}

fn build_unverified_receipt(
    request: &ControlledGenerationBatchRequest,
    outputs: &[ControlledGenerationCaseOutput],
    backend_declaration: UnverifiedBackendControlDeclaration,
) -> UnverifiedAppliedControlReceipt {
    let control_sha256 = request.control().fingerprint_sha256();
    UnverifiedAppliedControlReceipt {
        request_sha256: request.fingerprint_sha256(),
        requested_control_sha256: control_sha256,
        exact_float_bits_sha256: request.exact_float_bits_sha256(),
        output_sha256: output_digest(request, outputs),
        backend_declaration_sha256: backend_declaration_digest(&backend_declaration),
        backend_declaration,
    }
}

fn expected_application_plan(control: &ControlProgram) -> Vec<ExpectedControlApplication> {
    let mut operations = Vec::new();
    let mut push = |kind, stage| {
        debug_assert!(operations.len() < usize::from(u16::MAX));
        let operation_index = operations.len() as u16;
        operations.push(ExpectedControlApplication {
            operation_index,
            kind,
            stage,
        });
    };
    for profile in control.static_profiles() {
        match profile {
            StaticControlProfile::AdapterStack { .. } => {
                push(
                    AppliedControlKind::AdapterStack,
                    ControlApplicationStage::ModelLoad,
                );
            }
            StaticControlProfile::ActivationVector { .. } => push(
                AppliedControlKind::StaticActivationVector,
                ControlApplicationStage::ModelEvaluation,
            ),
        }
    }
    if control.constraint().is_some() {
        push(
            AppliedControlKind::StructuredConstraint,
            ControlApplicationStage::Constraint,
        );
    }
    for guidance in control.guidance() {
        let kind = match guidance {
            GuidanceControl::SameModelCfg { .. } => AppliedControlKind::SameModelCfg,
            GuidanceControl::ContrastiveExpertAmateur { .. } => {
                AppliedControlKind::ContrastiveExpertAmateur
            }
            GuidanceControl::DExperts { .. } => AppliedControlKind::DExperts,
            GuidanceControl::GenArm { .. } => AppliedControlKind::GenArm,
            GuidanceControl::PowerSampling { .. } => AppliedControlKind::PowerSampling,
        };
        push(kind, ControlApplicationStage::Guidance);
    }
    for sampler in control.extended_samplers().as_slice() {
        let kind = match sampler {
            ExtendedSampler::MirostatV1 { .. } => AppliedControlKind::MirostatV1,
            ExtendedSampler::MirostatV2 { .. } => AppliedControlKind::MirostatV2,
            ExtendedSampler::EtaCutoff { .. } => AppliedControlKind::EtaCutoff,
            ExtendedSampler::SparseLogitBias { .. } => AppliedControlKind::SparseLogitBias,
            ExtendedSampler::TopNSigma { .. } => AppliedControlKind::TopNSigma,
        };
        push(kind, ControlApplicationStage::Sampler);
    }
    match control.terminal_selector() {
        TerminalSelector::Distribution => push(
            AppliedControlKind::OrdinaryDistributionSelector,
            ControlApplicationStage::Sampler,
        ),
        TerminalSelector::Greedy => push(
            AppliedControlKind::GreedySelector,
            ControlApplicationStage::Sampler,
        ),
        TerminalSelector::MirostatV1 | TerminalSelector::MirostatV2 => {}
    }
    if !control.observations().is_disabled() {
        push(
            AppliedControlKind::DistributionObservation,
            ControlApplicationStage::EvidenceCapture,
        );
    }
    operations
}

/// The public API deliberately has no supported representation for arbitrary
/// J-space projection or K-space/KV editing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedInterventionKind {
    JSpaceProjection,
    KSpaceKvEditing,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedInterventionReason {
    NotExposedByUpstreamApi,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UnsupportedIntervention {
    pub kind: UnsupportedInterventionKind,
    pub reason: UnsupportedInterventionReason,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "ControlledGenerationCapabilitiesWire")]
pub struct ControlledGenerationCapabilities {
    same_model_cfg: bool,
    multi_model_logit_arithmetic: bool,
    power_sampling: bool,
    static_adapter_profiles: bool,
    static_activation_vectors: bool,
    unavailable_interventions: Vec<UnsupportedIntervention>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlledGenerationCapabilitiesWire {
    #[serde(default)]
    same_model_cfg: bool,
    #[serde(default)]
    multi_model_logit_arithmetic: bool,
    #[serde(default)]
    power_sampling: bool,
    #[serde(default)]
    static_adapter_profiles: bool,
    #[serde(default)]
    static_activation_vectors: bool,
    unavailable_interventions: Vec<UnsupportedIntervention>,
}

impl ControlledGenerationCapabilities {
    #[must_use]
    pub fn inspected(
        same_model_cfg: bool,
        multi_model_logit_arithmetic: bool,
        power_sampling: bool,
        static_adapter_profiles: bool,
        static_activation_vectors: bool,
    ) -> Self {
        Self {
            same_model_cfg,
            multi_model_logit_arithmetic,
            power_sampling,
            static_adapter_profiles,
            static_activation_vectors,
            unavailable_interventions: unavailable_interventions(),
        }
    }

    #[must_use]
    pub const fn same_model_cfg(&self) -> bool {
        self.same_model_cfg
    }

    #[must_use]
    pub const fn multi_model_logit_arithmetic(&self) -> bool {
        self.multi_model_logit_arithmetic
    }

    #[must_use]
    pub const fn power_sampling(&self) -> bool {
        self.power_sampling
    }

    #[must_use]
    pub const fn static_adapter_profiles(&self) -> bool {
        self.static_adapter_profiles
    }

    #[must_use]
    pub const fn static_activation_vectors(&self) -> bool {
        self.static_activation_vectors
    }

    #[must_use]
    pub fn unavailable_interventions(&self) -> &[UnsupportedIntervention] {
        &self.unavailable_interventions
    }
}

impl TryFrom<ControlledGenerationCapabilitiesWire> for ControlledGenerationCapabilities {
    type Error = NativeError;

    fn try_from(value: ControlledGenerationCapabilitiesWire) -> Result<Self, Self::Error> {
        if value.unavailable_interventions != unavailable_interventions() {
            return Err(invalid(
                "controlled capabilities must report J-space and K-space operations as unavailable upstream",
            ));
        }
        Ok(Self {
            same_model_cfg: value.same_model_cfg,
            multi_model_logit_arithmetic: value.multi_model_logit_arithmetic,
            power_sampling: value.power_sampling,
            static_adapter_profiles: value.static_adapter_profiles,
            static_activation_vectors: value.static_activation_vectors,
            unavailable_interventions: value.unavailable_interventions,
        })
    }
}

fn unavailable_interventions() -> Vec<UnsupportedIntervention> {
    vec![
        UnsupportedIntervention {
            kind: UnsupportedInterventionKind::JSpaceProjection,
            reason: UnsupportedInterventionReason::NotExposedByUpstreamApi,
        },
        UnsupportedIntervention {
            kind: UnsupportedInterventionKind::KSpaceKvEditing,
            reason: UnsupportedInterventionReason::NotExposedByUpstreamApi,
        },
    ]
}

struct StableDigest(Sha256);

impl StableDigest {
    fn new(domain: &str) -> Self {
        let mut digest = Self(Sha256::new());
        digest.text(domain);
        digest
    }

    fn bytes(&mut self, value: &[u8]) {
        self.0.update((value.len() as u64).to_le_bytes());
        self.0.update(value);
    }

    fn text(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    fn bool(&mut self, value: bool) {
        self.0.update([u8::from(value)]);
    }

    fn u32(&mut self, value: u32) {
        self.0.update(value.to_le_bytes());
    }

    fn i32(&mut self, value: i32) {
        self.0.update(value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.0.update(value.to_le_bytes());
    }

    fn u128(&mut self, value: u128) {
        self.0.update(value.to_le_bytes());
    }

    fn usize(&mut self, value: usize) {
        self.u64(value as u64);
    }

    fn f32(&mut self, value: f32) {
        self.u32(value.to_bits());
    }

    fn f64(&mut self, value: f64) {
        self.u64(value.to_bits());
    }

    fn finish(self) -> String {
        let bytes = self.0.finalize();
        let mut output = String::with_capacity(64);
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in bytes {
            output.push(char::from(HEX[usize::from(byte >> 4)]));
            output.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        output
    }
}

fn hash_model_identity(digest: &mut StableDigest, identity: &ControlledModelIdentity) {
    let fingerprint = identity.fingerprint();
    digest.text(identity.participant_id());
    digest.text(&fingerprint.model_id);
    digest.u64(fingerprint.model_size);
    digest.text(&fingerprint.model_sha256);
    digest.text(&fingerprint.tokenizer_sha256);
    digest.text(&fingerprint.chat_template_sha256);
    match &fingerprint.multimodal_projector_sha256 {
        Some(value) => {
            digest.bool(true);
            digest.text(value);
        }
        None => digest.bool(false),
    }
    digest.text(&fingerprint.binding_version);
    digest.text(&fingerprint.build_id);
    digest.text(&fingerprint.backend);
    digest.u32(fingerprint.context_tokens);
    digest.u32(fingerprint.batch_tokens);
    digest.u32(fingerprint.max_sequences);
    digest.text(&fingerprint.rope_config_sha256);
    digest.text(&fingerprint.kv_layout_sha256);
    digest.text(identity.token_contract().tokenizer_sha256());
    digest.text(identity.token_contract().vocabulary_sha256());
    digest.text(identity.token_contract().special_tokens_sha256());
    digest.text(identity.token_contract().token_bytes_sha256());
}

fn hash_control_program(digest: &mut StableDigest, control: &ControlProgram) {
    digest.text(control.format());
    hash_model_identity(digest, control.writer());
    digest.usize(control.auxiliary_models().len());
    for model in control.auxiliary_models() {
        hash_model_identity(digest, model);
    }
    match control.constraint() {
        Some(constraint) => {
            digest.bool(true);
            digest.u32(match constraint.kind() {
                super::StructuredConstraintKind::Gbnf => 0,
                super::StructuredConstraintKind::JsonSchema => 1,
            });
            digest.text(constraint.reference().artifact_id());
            digest.text(constraint.reference().sha256());
            digest.u32(constraint.reference().byte_len());
        }
        None => digest.bool(false),
    }
    hash_guidance(digest, control.guidance());
    hash_extended_samplers(digest, control.extended_samplers());
    digest.u32(match control.terminal_selector() {
        TerminalSelector::Distribution => 0,
        TerminalSelector::Greedy => 1,
        TerminalSelector::MirostatV1 => 2,
        TerminalSelector::MirostatV2 => 3,
    });
    hash_observation_policy(digest, control.observations());
    digest.usize(control.static_profiles().len());
    for profile in control.static_profiles() {
        hash_static_profile(digest, profile);
    }
}

fn hash_guidance(digest: &mut StableDigest, guidance: &[GuidanceControl]) {
    digest.usize(guidance.len());
    for control in guidance {
        match control {
            GuidanceControl::SameModelCfg { scale, rescale } => {
                digest.u32(0);
                digest.u32(scale.bits());
                match rescale {
                    Some(value) => {
                        digest.bool(true);
                        digest.u32(value.bits());
                    }
                    None => digest.bool(false),
                }
            }
            GuidanceControl::ContrastiveExpertAmateur {
                amateur_participant_id,
                primary_coefficient,
                amateur_coefficient,
            } => {
                digest.u32(1);
                digest.text(amateur_participant_id);
                digest.u32(primary_coefficient.bits());
                digest.u32(amateur_coefficient.bits());
            }
            GuidanceControl::DExperts {
                expert_participant_id,
                anti_expert_participant_id,
                base_coefficient,
                expert_coefficient,
                anti_expert_coefficient,
            } => {
                digest.u32(2);
                digest.text(expert_participant_id);
                digest.text(anti_expert_participant_id);
                digest.u32(base_coefficient.bits());
                digest.u32(expert_coefficient.bits());
                digest.u32(anti_expert_coefficient.bits());
            }
            GuidanceControl::GenArm {
                reward_participant_id,
                base_coefficient,
                reward_coefficient,
            } => {
                digest.u32(3);
                digest.text(reward_participant_id);
                digest.u32(base_coefficient.bits());
                digest.u32(reward_coefficient.bits());
            }
            GuidanceControl::PowerSampling { exponent } => {
                digest.u32(4);
                digest.u32(exponent.bits());
            }
        }
    }
}

fn hash_extended_samplers(digest: &mut StableDigest, program: &ExtendedSamplerProgram) {
    digest.usize(program.as_slice().len());
    for sampler in program.as_slice() {
        match sampler {
            ExtendedSampler::MirostatV1 { config } => {
                digest.u32(0);
                digest.f32(config.tau());
                digest.f32(config.eta());
                digest.i32(config.m());
            }
            ExtendedSampler::MirostatV2 { config } => {
                digest.u32(1);
                digest.f32(config.tau());
                digest.f32(config.eta());
            }
            ExtendedSampler::EtaCutoff { cutoff } => {
                digest.u32(2);
                digest.f32(cutoff.get());
            }
            ExtendedSampler::SparseLogitBias { biases } => {
                digest.u32(3);
                digest.usize(biases.as_slice().len());
                for bias in biases.as_slice() {
                    digest.i32(bias.token_id);
                    digest.f32(bias.bias);
                }
            }
            ExtendedSampler::TopNSigma { sigma } => {
                digest.u32(4);
                digest.f32(sigma.get());
            }
        }
    }
}

fn hash_observation_policy(digest: &mut StableDigest, policy: &DistributionObservationPolicy) {
    digest.usize(policy.stages().len());
    for stage in policy.stages() {
        digest.u32(match stage {
            ProbabilityStage::RawModel => 0,
            ProbabilityStage::PostConstraint => 1,
            ProbabilityStage::PostGuidance => 2,
            ProbabilityStage::PostSampler => 3,
        });
    }
    digest.usize(policy.value_kinds().len());
    for kind in policy.value_kinds() {
        digest.u32(match kind {
            DistributionValueKind::Logit => 0,
            DistributionValueKind::Probability => 1,
            DistributionValueKind::LogProbability => 2,
        });
    }
    digest.bool(policy.include_selected_token());
    digest.u32(u32::from(policy.top_k()));
}

fn hash_static_profile(digest: &mut StableDigest, profile: &StaticControlProfile) {
    match profile {
        StaticControlProfile::AdapterStack {
            profile_id,
            participant_id,
            adapters,
        } => {
            digest.u32(0);
            digest.text(profile_id);
            digest.text(participant_id);
            digest.usize(adapters.len());
            for adapter in adapters {
                hash_artifact(digest, &adapter.artifact);
                digest.u32(adapter.scale.bits());
            }
        }
        StaticControlProfile::ActivationVector {
            profile_id,
            participant_id,
            artifact,
            model_identity_sha256,
            layer_start,
            layer_end_inclusive,
            dimensions,
            scale,
        } => {
            digest.u32(1);
            digest.text(profile_id);
            digest.text(participant_id);
            hash_artifact(digest, artifact);
            digest.text(model_identity_sha256);
            digest.u32(*layer_start);
            digest.u32(*layer_end_inclusive);
            digest.u32(*dimensions);
            digest.u32(scale.bits());
        }
    }
}

fn hash_artifact(digest: &mut StableDigest, artifact: &StaticControlArtifact) {
    digest.text(artifact.artifact_id());
    digest.text(artifact.sha256());
    digest.u64(artifact.byte_len());
    digest.text(artifact.derivation_sha256());
}

fn hash_case(digest: &mut StableDigest, case: &ControlledGenerationCase) {
    digest.text(case.case_id());
    hash_prompt(digest, case.conditional_prompt());
    match case.unconditional_prompt() {
        Some(prompt) => {
            digest.bool(true);
            hash_prompt(digest, prompt);
        }
        None => digest.bool(false),
    }
    hash_sampling(digest, case.sampling());
}

fn hash_prompt(digest: &mut StableDigest, prompt: &ExactTokenPrompt) {
    digest.usize(prompt.token_ids().len());
    for token_id in prompt.token_ids() {
        digest.i32(*token_id);
    }
}

fn hash_sampling(digest: &mut StableDigest, sampling: &SamplingConfig) {
    digest.text(SAMPLING_CONFIG_FINGERPRINT_DOMAIN);
    digest.bytes(sampling.fingerprint().as_bytes());
}

fn hash_control_float_bits(digest: &mut StableDigest, control: &ControlProgram) {
    hash_guidance(digest, control.guidance());
    hash_extended_samplers(digest, control.extended_samplers());
    for profile in control.static_profiles() {
        hash_static_profile(digest, profile);
    }
}

fn hash_cost(digest: &mut StableDigest, cost: &ControlCostAccounting) {
    digest.usize(cost.participants.len());
    for participant in &cost.participants {
        digest.text(&participant.participant_id);
        digest.u32(participant.conditional_sequences);
        digest.u32(participant.unconditional_sequences);
        digest.u64(participant.exact_prompt_tokens);
        digest.u64(participant.maximum_generated_evaluations);
        digest.u64(participant.maximum_context_token_positions);
    }
    digest.u32(cost.total_model_contexts);
    digest.u32(cost.total_sequence_slots);
    digest.u64(cost.exact_prompt_tokens);
    digest.u64(cost.maximum_generated_evaluations);
    digest.u64(cost.maximum_context_token_positions);
}

fn output_digest(
    request: &ControlledGenerationBatchRequest,
    outputs: &[ControlledGenerationCaseOutput],
) -> String {
    let mut digest = StableDigest::new("controlled-generation-output-v1");
    digest.text(&request.fingerprint_sha256());
    for output in outputs {
        let generation = output.generation();
        digest.text(output.case_id());
        digest.text(&generation.request_id);
        digest.text(&generation.branch_id);
        digest.usize(generation.input_index);
        digest.text(&generation.model_id);
        digest.text(&generation.text);
        digest.usize(generation.generated_token_ids.len());
        for token_id in &generation.generated_token_ids {
            digest.i32(*token_id);
        }
        match &generation.token_observations {
            Some(observations) => {
                digest.bool(true);
                digest.usize(observations.len());
                for observation in observations {
                    digest.usize(observation.generated_index);
                    digest.i32(observation.token_id);
                    digest.usize(observation.probabilities.len());
                    for probability in &observation.probabilities {
                        digest.u32(match probability.stage {
                            ProbabilityStage::RawModel => 0,
                            ProbabilityStage::PostConstraint => 1,
                            ProbabilityStage::PostGuidance => 2,
                            ProbabilityStage::PostSampler => 3,
                        });
                        digest.f32(probability.probability);
                    }
                }
            }
            None => digest.bool(false),
        }
        digest.u32(generation.state as u32);
        digest.text(&generation.finish_reason);
        digest.usize(generation.metrics.prompt_tokens);
        digest.usize(generation.metrics.completion_tokens);
        digest.usize(generation.metrics.shared_prefix_tokens);
        digest.u128(generation.metrics.duration_ms);
        match generation.metrics.first_token_ms {
            Some(value) => {
                digest.bool(true);
                digest.u128(value);
            }
            None => digest.bool(false),
        }
        digest.f64(generation.metrics.tokens_per_second);
        digest.usize(generation.metrics.cache.supplied_prefix_tokens);
        digest.usize(generation.metrics.cache.restored_prefix_tokens);
        digest.usize(generation.metrics.cache.batch_shared_prefix_tokens);
        digest.bool(generation.real_engine_invoked);
        digest.bool(generation.fake_fixture);
        digest.u32(generation.transport as u32);
        digest.usize(output.distribution_observations().len());
        for observation in output.distribution_observations() {
            hash_distribution_observation(&mut digest, observation);
        }
    }
    digest.finish()
}

fn backend_declaration_digest(backend: &UnverifiedBackendControlDeclaration) -> String {
    let mut digest = StableDigest::new("unverified-backend-control-declaration-v1");
    digest.u32(backend.transport() as u32);
    digest.bool(backend.real_engine_invoked());
    digest.bool(backend.fake_fixture());
    digest.text(backend.applied_control_sha256());
    digest.text(backend.backend_event_stream_sha256());
    digest.usize(backend.participant_reports().len());
    for participant in backend.participant_reports() {
        digest.text(participant.participant_id());
        digest.text(participant.identity_sha256());
        digest.text(participant.model_load_evidence_sha256());
    }
    digest.usize(backend.applied_operations().len());
    for operation in backend.applied_operations() {
        digest.u32(u32::from(operation.operation_index()));
        digest.u32(operation.kind() as u32);
        digest.u32(operation.stage() as u32);
        digest.text(operation.operation_evidence_sha256());
    }
    digest.finish()
}

fn hash_distribution_observation(
    digest: &mut StableDigest,
    observation: &TokenDistributionObservation,
) {
    digest.usize(observation.generated_index());
    digest.i32(observation.token_id());
    digest.usize(observation.observations().len());
    for stage in observation.observations() {
        digest.u32(match stage.stage() {
            ProbabilityStage::RawModel => 0,
            ProbabilityStage::PostConstraint => 1,
            ProbabilityStage::PostGuidance => 2,
            ProbabilityStage::PostSampler => 3,
        });
        digest.u32(match stage.value_kind() {
            DistributionValueKind::Logit => 0,
            DistributionValueKind::Probability => 1,
            DistributionValueKind::LogProbability => 2,
        });
        hash_distribution_token(digest, stage.selected());
        digest.usize(stage.ranked_candidates().len());
        for candidate in stage.ranked_candidates() {
            digest.u32(u32::from(candidate.rank()));
            hash_distribution_token(digest, candidate.token());
        }
    }
}

fn hash_distribution_token(digest: &mut StableDigest, token: &super::DistributionTokenValue) {
    digest.i32(token.token_id());
    match token.token_bytes() {
        Some(bytes) => {
            digest.bool(true);
            digest.bytes(bytes);
        }
        None => digest.bool(false),
    }
    digest.f32(token.value());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DistributionValueKindSet, GenerationCacheMetrics, GenerationMetrics, MirostatV2Config,
        ProbabilityStageSet, StageDistributionObservation,
    };
    fn fingerprint(model_id: &str, model_hash: char, tokenizer_hash: char) -> ModelFingerprint {
        ModelFingerprint {
            model_id: model_id.to_string(),
            model_size: 1024,
            model_sha256: model_hash.to_string().repeat(64),
            tokenizer_sha256: tokenizer_hash.to_string().repeat(64),
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

    fn token_contract(tokenizer_hash: char) -> TokenContractIdentity {
        TokenContractIdentity::new(
            tokenizer_hash.to_string().repeat(64),
            "1".repeat(64),
            "2".repeat(64),
            "3".repeat(64),
        )
        .expect("valid token contract")
    }

    fn model(participant: &str, model_hash: char) -> ControlledModelIdentity {
        ControlledModelIdentity::new(
            participant.to_string(),
            fingerprint(participant, model_hash, 'b'),
            token_contract('b'),
        )
        .expect("valid model identity")
    }

    fn case(case_id: &str, unconditional: bool) -> ControlledGenerationCase {
        ControlledGenerationCase::new(
            case_id.to_string(),
            ExactTokenPrompt::new(vec![1, 2, 3]).expect("conditional prompt"),
            unconditional.then(|| ExactTokenPrompt::new(vec![4, 5]).expect("unconditional")),
            SamplingConfig {
                max_tokens: 2,
                ..SamplingConfig::default()
            },
        )
        .expect("valid controlled case")
    }

    fn request_with_sampling(sampling: SamplingConfig) -> ControlledGenerationBatchRequest {
        ControlledGenerationBatchRequest::new(
            "sampling-fingerprint".to_string(),
            vec![
                ControlledGenerationCase::new(
                    "case".to_string(),
                    ExactTokenPrompt::new(vec![1]).expect("prompt"),
                    None,
                    sampling,
                )
                .expect("case"),
            ],
            ControlProgram::new(
                model("writer", 'a'),
                Vec::new(),
                None,
                Vec::new(),
                ExtendedSamplerProgram::default(),
                TerminalSelector::Distribution,
                DistributionObservationPolicy::default(),
                Vec::new(),
            )
            .expect("program"),
        )
        .expect("request")
    }

    fn cfg_program() -> ControlProgram {
        ControlProgram::new(
            model("writer", 'a'),
            Vec::new(),
            None,
            vec![GuidanceControl::SameModelCfg {
                scale: ExactF32::new(1.5).expect("scale"),
                rescale: Some(ExactF32::new(0.7).expect("rescale")),
            }],
            ExtendedSamplerProgram::default(),
            TerminalSelector::Distribution,
            DistributionObservationPolicy::default(),
            Vec::new(),
        )
        .expect("valid CFG program")
    }

    fn fixture_generation(request_id: &str, case_id: &str) -> GenerationOutput {
        GenerationOutput {
            request_id: request_id.to_string(),
            branch_id: case_id.to_string(),
            input_index: 0,
            model_id: "writer".to_string(),
            text: "two tokens".to_string(),
            generated_token_ids: vec![10, 11],
            token_observations: None,
            state: GenerationState::Completed,
            finish_reason: "length".to_string(),
            metrics: GenerationMetrics {
                prompt_tokens: 3,
                completion_tokens: 2,
                shared_prefix_tokens: 0,
                duration_ms: 1,
                first_token_ms: Some(1),
                tokens_per_second: 2.0,
                cache: GenerationCacheMetrics::default(),
            },
            real_engine_invoked: false,
            fake_fixture: true,
            transport: NativeTransport::FakeFixture,
        }
    }

    fn backend_declaration(
        request: &ControlledGenerationBatchRequest,
    ) -> UnverifiedBackendControlDeclaration {
        let participants = request
            .control()
            .participants()
            .map(|participant| {
                BackendParticipantReport::new(
                    participant.participant_id().to_string(),
                    participant.identity_sha256(),
                    "8".repeat(64),
                )
                .expect("participant declaration")
            })
            .collect();
        let operations = request
            .control()
            .expected_application_plan()
            .into_iter()
            .map(|operation| {
                AppliedControlReport::new(
                    operation.operation_index(),
                    operation.kind(),
                    operation.stage(),
                    "9".repeat(64),
                )
                .expect("operation declaration")
            })
            .collect();
        UnverifiedBackendControlDeclaration::new(
            NativeTransport::FakeFixture,
            false,
            true,
            request.control().fingerprint_sha256(),
            "7".repeat(64),
            participants,
            operations,
        )
        .expect("backend declaration")
    }

    #[test]
    fn request_round_trip_binds_exact_tokens_models_floats_and_cfg_cost() {
        let request = ControlledGenerationBatchRequest::new(
            "request".to_string(),
            vec![case("case", true)],
            cfg_program(),
        )
        .expect("valid controlled request");
        assert_eq!(request.cost().total_model_contexts(), 1);
        assert_eq!(request.cost().total_sequence_slots(), 2);
        assert_eq!(request.cost().exact_prompt_tokens(), 5);
        assert_eq!(request.cost().maximum_generated_evaluations(), 4);
        assert_eq!(request.cost().maximum_context_token_positions(), 9);

        let json = serde_json::to_string(&request).expect("serialize request");
        assert!(
            !json.contains("model_path"),
            "semantic controlled-model identity must not serialize an operational path"
        );
        let decoded: ControlledGenerationBatchRequest =
            serde_json::from_str(&json).expect("deserialize request");
        assert_eq!(decoded, request);
        assert_eq!(decoded.fingerprint_sha256(), request.fingerprint_sha256());
        assert_eq!(
            decoded.exact_float_bits_sha256(),
            request.exact_float_bits_sha256()
        );

        let mut tampered = serde_json::to_value(&request).expect("request JSON");
        tampered["cost"]["total_sequence_slots"] = serde_json::json!(1);
        assert!(
            serde_json::from_value::<ControlledGenerationBatchRequest>(tampered).is_err(),
            "caller-authored CFG cost understatement must fail"
        );

        let mut unknown_sampling = serde_json::to_value(&request).expect("request JSON");
        unknown_sampling["cases"][0]["sampling"]["hidden_sampler"] = serde_json::json!(true);
        assert!(
            serde_json::from_value::<ControlledGenerationBatchRequest>(unknown_sampling).is_err(),
            "controlled nested sampling fields must be closed-world"
        );
        let mut unknown_fingerprint = serde_json::to_value(&request).expect("request JSON");
        unknown_fingerprint["control"]["writer"]["fingerprint"]["j_space"] =
            serde_json::json!(true);
        assert!(
            serde_json::from_value::<ControlledGenerationBatchRequest>(unknown_fingerprint)
                .is_err(),
            "controlled nested model identities must reject unknown claims"
        );
        let mut leaked_path = serde_json::to_value(&request).expect("request JSON");
        leaked_path["control"]["writer"]["fingerprint"]["model_path"] =
            serde_json::json!("/private/models/writer.gguf");
        assert!(
            serde_json::from_value::<ControlledGenerationBatchRequest>(leaked_path).is_err(),
            "strict semantic model identities must reject operational paths"
        );

        assert!(
            ControlledGenerationBatchRequest::new(
                "missing-unconditional".to_string(),
                vec![case("case", false)],
                cfg_program(),
            )
            .is_err(),
            "CFG cannot silently reuse the conditional prompt as an unconditional branch"
        );
    }

    #[test]
    fn request_fingerprints_commit_exact_sampling_float_bits() {
        let positive_zero = request_with_sampling(SamplingConfig {
            seed: 7,
            temperature: 0.0,
            max_tokens: 1,
            ..SamplingConfig::default()
        });
        let negative_zero = request_with_sampling(SamplingConfig {
            seed: 7,
            temperature: -0.0,
            max_tokens: 1,
            ..SamplingConfig::default()
        });
        assert_ne!(
            positive_zero.fingerprint_sha256(),
            negative_zero.fingerprint_sha256()
        );
        assert_ne!(
            positive_zero.exact_float_bits_sha256(),
            negative_zero.exact_float_bits_sha256()
        );
    }

    #[test]
    fn exact_float_bits_receipt_ignores_non_floats_and_commits_every_float() {
        let baseline_sampling = SamplingConfig {
            seed: 7,
            max_tokens: 1,
            ..SamplingConfig::default()
        };
        let baseline = request_with_sampling(baseline_sampling.clone());
        let baseline_float_bits = baseline.exact_float_bits_sha256();

        let mut changed_seed = baseline_sampling.clone();
        changed_seed.seed = 8;
        let mut changed_top_k = baseline_sampling.clone();
        changed_top_k.top_k = 41;
        let mut changed_stop = baseline_sampling.clone();
        changed_stop.stop = vec!["stop here".to_string()];
        for changed in [changed_seed, changed_top_k, changed_stop] {
            let request = request_with_sampling(changed);
            assert_eq!(request.exact_float_bits_sha256(), baseline_float_bits);
            assert_ne!(request.fingerprint_sha256(), baseline.fingerprint_sha256());
        }

        type SamplingMutation = (&'static str, fn(&mut SamplingConfig));
        let mutations: [SamplingMutation; 13] = [
            ("temperature", |value| {
                value.temperature = f32::from_bits(value.temperature.to_bits() ^ 1);
            }),
            ("dynamic_temperature_range", |value| {
                value.dynamic_temperature_range =
                    f32::from_bits(value.dynamic_temperature_range.to_bits() ^ 1);
            }),
            ("dynamic_temperature_exponent", |value| {
                value.dynamic_temperature_exponent =
                    f32::from_bits(value.dynamic_temperature_exponent.to_bits() ^ 1);
            }),
            ("top_p", |value| {
                value.top_p = f32::from_bits(value.top_p.to_bits() ^ 1);
            }),
            ("min_p", |value| {
                value.min_p = f32::from_bits(value.min_p.to_bits() ^ 1);
            }),
            ("typical_p", |value| {
                value.typical_p = f32::from_bits(value.typical_p.to_bits() - 1);
            }),
            ("xtc_probability", |value| {
                value.xtc_probability = f32::from_bits(value.xtc_probability.to_bits() ^ 1);
            }),
            ("xtc_threshold", |value| {
                value.xtc_threshold = f32::from_bits(value.xtc_threshold.to_bits() ^ 1);
            }),
            ("repeat_penalty", |value| {
                value.repeat_penalty = f32::from_bits(value.repeat_penalty.to_bits() ^ 1);
            }),
            ("frequency_penalty", |value| {
                value.frequency_penalty = f32::from_bits(value.frequency_penalty.to_bits() ^ 1);
            }),
            ("presence_penalty", |value| {
                value.presence_penalty = f32::from_bits(value.presence_penalty.to_bits() ^ 1);
            }),
            ("dry_multiplier", |value| {
                value.dry_multiplier = f32::from_bits(value.dry_multiplier.to_bits() ^ 1);
            }),
            ("dry_base", |value| {
                value.dry_base = f32::from_bits(value.dry_base.to_bits() ^ 1);
            }),
        ];
        for (field, mutate) in mutations {
            let mut changed = baseline_sampling.clone();
            mutate(&mut changed);
            assert_ne!(
                request_with_sampling(changed).exact_float_bits_sha256(),
                baseline_float_bits,
                "float bit change was omitted for {field}"
            );
        }
    }

    #[test]
    fn control_program_rejects_token_contract_mismatch_and_ambiguous_selectors() {
        let mismatched = ControlledModelIdentity::new(
            "amateur".to_string(),
            fingerprint("amateur", 'f', '9'),
            token_contract('9'),
        )
        .expect("internally coherent mismatch");
        let control = ControlProgram::new(
            model("writer", 'a'),
            vec![mismatched],
            None,
            vec![GuidanceControl::ContrastiveExpertAmateur {
                amateur_participant_id: "amateur".to_string(),
                primary_coefficient: ExactF32::new(1.5).expect("positive primary"),
                amateur_coefficient: ExactF32::new(-0.5).expect("negative amateur"),
            }],
            ExtendedSamplerProgram::default(),
            TerminalSelector::Distribution,
            DistributionObservationPolicy::default(),
            Vec::new(),
        );
        assert!(control.is_err(), "mismatched token contracts must fail");

        let program = ExtendedSamplerProgram::new(vec![ExtendedSampler::MirostatV2 {
            config: MirostatV2Config::new(5.0, 0.1).expect("Mirostat config"),
        }])
        .expect("valid terminal sampler");
        assert!(
            ControlProgram::new(
                model("writer", 'a'),
                Vec::new(),
                None,
                Vec::new(),
                program,
                TerminalSelector::Greedy,
                DistributionObservationPolicy::default(),
                Vec::new(),
            )
            .is_err(),
            "Mirostat cannot hide behind a greedy label"
        );
    }

    #[test]
    fn output_round_trip_recomputes_receipt_and_rejects_tampering() {
        let request = ControlledGenerationBatchRequest::new(
            "request".to_string(),
            vec![case("case", true)],
            cfg_program(),
        )
        .expect("valid request");
        let case_output = ControlledGenerationCaseOutput::new(
            "case".to_string(),
            fixture_generation("request", "case"),
            Vec::new(),
        )
        .expect("valid local output");
        let output = ControlledGenerationBatchOutput::new(
            request.clone(),
            vec![case_output],
            backend_declaration(&request),
        )
        .expect("valid controlled output");
        assert_eq!(
            output.receipt().requested_control_sha256(),
            output
                .receipt()
                .backend_declaration()
                .applied_control_sha256()
        );
        let json = serde_json::to_string(&output).expect("serialize output");
        let decoded: ControlledGenerationBatchOutput =
            serde_json::from_str(&json).expect("deserialize output");
        assert_eq!(decoded, output);

        let mut tampered = serde_json::to_value(&output).expect("output JSON");
        tampered["cases"][0]["generation"]["text"] = serde_json::json!("altered");
        assert!(
            serde_json::from_value::<ControlledGenerationBatchOutput>(tampered).is_err(),
            "altered output bytes must invalidate the receipt"
        );
        let mut tampered = serde_json::to_value(&output).expect("output JSON");
        tampered["receipt"]["backend_declaration"]["applied_control_sha256"] =
            serde_json::json!("f".repeat(64));
        assert!(
            serde_json::from_value::<ControlledGenerationBatchOutput>(tampered).is_err(),
            "an applied program label cannot diverge from the frozen request"
        );
        let mut tampered = serde_json::to_value(&output).expect("output JSON");
        tampered["receipt"]["backend_declaration"]["applied_operations"][0]["operation_evidence_sha256"] =
            serde_json::json!("6".repeat(64));
        assert!(
            serde_json::from_value::<ControlledGenerationBatchOutput>(tampered).is_err(),
            "operation evidence mutation must invalidate the declaration digest"
        );
        let mut unknown_metric = serde_json::to_value(&output).expect("output JSON");
        unknown_metric["cases"][0]["generation"]["metrics"]["untracked_cost"] =
            serde_json::json!(1);
        assert!(
            serde_json::from_value::<ControlledGenerationBatchOutput>(unknown_metric).is_err(),
            "controlled nested output evidence must reject unknown fields"
        );
    }

    #[test]
    fn requested_but_unreported_control_cannot_be_labeled_applied() {
        let request = ControlledGenerationBatchRequest::new(
            "request".to_string(),
            vec![case("case", true)],
            cfg_program(),
        )
        .expect("valid request");
        let case_output = ControlledGenerationCaseOutput::new(
            "case".to_string(),
            fixture_generation("request", "case"),
            Vec::new(),
        )
        .expect("valid local output");
        let complete = backend_declaration(&request);
        let incomplete = UnverifiedBackendControlDeclaration::new(
            complete.transport(),
            complete.real_engine_invoked(),
            complete.fake_fixture(),
            complete.applied_control_sha256().to_string(),
            complete.backend_event_stream_sha256().to_string(),
            complete.participant_reports().to_vec(),
            complete.applied_operations()[..1].to_vec(),
        )
        .expect("locally well-formed but incomplete declaration");
        assert!(
            ControlledGenerationBatchOutput::new(request, vec![case_output], incomplete).is_err(),
            "the requested CFG and selector need independent applied-stage reports"
        );
    }

    #[test]
    fn requested_distribution_evidence_is_complete_and_exactly_joined() {
        let policy = DistributionObservationPolicy::new(
            ProbabilityStageSet::new(vec![ProbabilityStage::RawModel]).expect("stage set"),
            DistributionValueKindSet::new(vec![DistributionValueKind::Logit]).expect("value set"),
            true,
            0,
        )
        .expect("observation policy");
        let program = ControlProgram::new(
            model("writer", 'a'),
            Vec::new(),
            None,
            Vec::new(),
            ExtendedSamplerProgram::default(),
            TerminalSelector::Distribution,
            policy,
            Vec::new(),
        )
        .expect("valid evidence program");
        let request = ControlledGenerationBatchRequest::new(
            "request".to_string(),
            vec![case("case", false)],
            program,
        )
        .expect("request");
        let observations = vec![10, 11]
            .into_iter()
            .enumerate()
            .map(|(index, token_id)| {
                TokenDistributionObservation::new(
                    index,
                    token_id,
                    vec![
                        StageDistributionObservation::new(
                            ProbabilityStage::RawModel,
                            DistributionValueKind::Logit,
                            crate::DistributionTokenValue::new(token_id, None, 0.5)
                                .expect("selected token"),
                            Vec::new(),
                        )
                        .expect("stage observation"),
                    ],
                )
                .expect("token observation")
            })
            .collect();
        let output = ControlledGenerationCaseOutput::new(
            "case".to_string(),
            fixture_generation("request", "case"),
            observations,
        )
        .expect("case output");
        ControlledGenerationBatchOutput::new(
            request.clone(),
            vec![output],
            backend_declaration(&request),
        )
        .expect("complete evidence must pass");
    }

    #[test]
    fn capabilities_cannot_claim_j_or_k_space_support() {
        let capabilities =
            ControlledGenerationCapabilities::inspected(true, true, true, true, true);
        assert_eq!(capabilities.unavailable_interventions().len(), 2);
        let json = serde_json::to_string(&capabilities).expect("serialize capabilities");
        let decoded: ControlledGenerationCapabilities =
            serde_json::from_str(&json).expect("deserialize capabilities");
        assert_eq!(decoded, capabilities);

        let mut tampered = serde_json::to_value(capabilities).expect("capability JSON");
        tampered["unavailable_interventions"] = serde_json::json!([]);
        assert!(
            serde_json::from_value::<ControlledGenerationCapabilities>(tampered).is_err(),
            "capabilities must keep explicit upstream J/K-space unavailability"
        );
    }

    #[test]
    fn legacy_generation_json_remains_control_neutral() {
        let legacy = crate::GenerationBatchRequest {
            request_id: "legacy".to_string(),
            model_id: "writer".to_string(),
            cases: vec![crate::GenerationCase {
                case_id: "case".to_string(),
                input: crate::GenerationInput::Completion {
                    prompts: vec![crate::CompletionPrompt::Tokens {
                        token_ids: vec![1, 2, 3],
                    }],
                },
                sampling: SamplingConfig::default(),
                cached_prefix: None,
            }],
        };
        let json = serde_json::to_value(legacy).expect("serialize legacy request");
        assert_eq!(json.as_object().expect("legacy object").len(), 3);
        assert!(json.get("control").is_none());
        assert!(json.get("cost").is_none());
        assert!(json.get("observations").is_none());
    }

    #[test]
    fn unknown_fields_and_non_finite_exact_scalars_fail_at_serde_boundary() {
        assert!(
            serde_json::from_value::<ExactF32>(serde_json::json!({
                "bits": f32::NAN.to_bits()
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ExactTokenPrompt>(serde_json::json!({
                "token_ids": [1],
                "text": "hidden re-tokenization"
            }))
            .is_err()
        );
    }
}
