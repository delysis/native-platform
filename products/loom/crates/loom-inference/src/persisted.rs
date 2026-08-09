//! Pure replay of persisted native inference evidence.
//!
//! Everything in this module is reconstructible from stored bytes. Successful
//! verification proves internal consistency only; it never recreates live
//! inference, store-admission, promotion, treatment, or benchmark authority.

use std::{collections::HashSet, fmt, str::FromStr};

use llama_native_types::{
    ControlledGenerationBatchOutput, GenerationState, MAX_DISTRIBUTION_OBSERVATIONS_PER_TOKEN,
    MAX_STOP_SEQUENCE_BYTES, MAX_STOP_SEQUENCES, ModelFingerprint, NativeTransport,
    ProbabilityStage, SamplerKind, SamplingConfig, TokenDistributionObservation,
};
use loom_research_types::{
    CallError, CallEvidenceClass, CallScope, CallTerminal, MAX_BACKEND_EVIDENCE_BYTES,
    MAX_BASE_WRITER_BATCH_CASES, MAX_GENERATED_TOKENS, MAX_RAW_OUTPUT_BYTES, ModelCall,
    ModelCallId, OutputProjection,
};
use loom_types::{BlobId, ProjectId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    PromptFormEvidence, PromptTokenPolicyEvidence,
    canonical::{CanonicalDigest, model_fingerprint_id, sampling_fingerprint},
};

const REQUEST_DOMAIN: &str = "loom/native-base-writer-request/v1";
const CONTROLLED_REQUEST_DOMAIN: &str = "loom/native-controlled-base-writer-request/v1";
const VERIFICATION_DOMAIN: &str = "loom/native-base-writer-verification/v1";
const ENVELOPE_DOMAIN: &str = "loom/native-base-writer-envelope/v1";
const COMPILED_PROMPT_DOMAIN: &str = "loom/compiled-base-completion-prompt/v1";
const PROMPT_TOKEN_DOMAIN: &str = "loom/exact-native-token-ids/v1";
const NO_CONTROL_PROGRAM_DOMAIN: &str = "loom/native-uncontrolled-base-completion/v1";
const BACKEND_AUDIT_FORMAT: &str = "loom.native-base-writer-audit.v1";
const CONTROLLED_BACKEND_AUDIT_FORMAT: &str = "loom.native-controlled-base-writer-audit.v1";
const SANITIZED_MODEL_FINGERPRINT_FORMAT: &str = "loom.native-model-fingerprint.v1";
const OUTPUT_AUDIT_FORMAT: &str = "loom.native-base-writer-output-audit.v1";
const EVENT_LEDGER_FORMAT: &str = "loom.native-base-writer-event-ledger.v1";
const CANCELLED_MESSAGE: &str = "native generation cancelled";
const MAX_RECEIPT_ID_BYTES: usize = 256;

/// Recompiled binding facts needed to replay native persisted evidence.
///
/// This is caller-provided diagnostic input, not a model-selection authority.
#[derive(Clone, Copy)]
pub struct PersistedBindingEvidenceRef<'a> {
    pub binding_id: &'a str,
    pub binding_fingerprint: BlobId,
    pub model_sha256: BlobId,
    pub model_byte_len: u64,
    pub tokenizer_sha256: BlobId,
    pub multimodal_projector_sha256: Option<BlobId>,
    pub context_tokens: u32,
}

impl fmt::Debug for PersistedBindingEvidenceRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersistedBindingEvidenceRef")
            .field("binding_id", &self.binding_id)
            .field("binding_fingerprint", &self.binding_fingerprint)
            .field("model_sha256", &self.model_sha256)
            .field("model_byte_len", &self.model_byte_len)
            .field("tokenizer_sha256", &self.tokenizer_sha256)
            .field(
                "has_multimodal_projector",
                &self.multimodal_projector_sha256.is_some(),
            )
            .field("context_tokens", &self.context_tokens)
            .finish()
    }
}

/// Exact prompt fields retained with a persisted native batch.
///
/// The verifier rehashes the bytes and token IDs. It does not assert that the
/// source prompt or treatment was authorized by a campaign.
#[derive(Clone, Copy)]
pub struct PersistedPromptEvidenceRef<'a> {
    pub project_id: ProjectId,
    pub scope: CallScope,
    pub source_prompt_fingerprint: BlobId,
    pub content_fingerprint: BlobId,
    pub treatment_recipe_fingerprint: BlobId,
    pub raw_utf8: &'a [u8],
    pub raw_blob_id: BlobId,
    pub form: PromptFormEvidence,
    pub token_policy: PromptTokenPolicyEvidence,
    pub ordered_token_ids: &'a [u32],
    pub token_fingerprint: BlobId,
    pub compiled_fingerprint: BlobId,
}

impl fmt::Debug for PersistedPromptEvidenceRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersistedPromptEvidenceRef")
            .field("project_id", &self.project_id)
            .field("scope", &self.scope)
            .field("source_prompt_fingerprint", &self.source_prompt_fingerprint)
            .field("content_fingerprint", &self.content_fingerprint)
            .field(
                "treatment_recipe_fingerprint",
                &self.treatment_recipe_fingerprint,
            )
            .field("raw_byte_len", &self.raw_utf8.len())
            .field("raw_blob_id", &self.raw_blob_id)
            .field("form", &self.form)
            .field("token_policy", &self.token_policy)
            .field("token_count", &self.ordered_token_ids.len())
            .field("token_fingerprint", &self.token_fingerprint)
            .field("compiled_fingerprint", &self.compiled_fingerprint)
            .finish()
    }
}

/// Persisted terminal class and projection evidence for one case.
#[derive(Clone, Copy)]
pub enum PersistedCaseOutcomeRef<'a> {
    Completed {
        displayed_output: &'a [u8],
        output_projection: Option<&'a OutputProjection>,
    },
    Cancelled,
}

impl fmt::Debug for PersistedCaseOutcomeRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Completed {
                displayed_output,
                output_projection,
            } => formatter
                .debug_struct("Completed")
                .field("displayed_output_bytes", &displayed_output.len())
                .field("has_output_projection", &output_projection.is_some())
                .finish(),
            Self::Cancelled => formatter.write_str("Cancelled"),
        }
    }
}

/// Borrowed exact bytes for one ordered persisted case.
#[derive(Clone, Copy)]
pub struct PersistedInferenceCaseRef<'a> {
    pub input_index: usize,
    pub model_call: &'a ModelCall,
    pub raw_output: &'a [u8],
    pub generated_token_ids: &'a [u32],
    pub event_json: &'a [u8],
    pub backend_audit_json: &'a [u8],
    pub terminal_sampled_token_id: Option<i32>,
    pub outcome: PersistedCaseOutcomeRef<'a>,
    pub verification_fingerprint: BlobId,
}

impl fmt::Debug for PersistedInferenceCaseRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersistedInferenceCaseRef")
            .field("input_index", &self.input_index)
            .field("call_id", &self.model_call.id())
            .field("raw_output_bytes", &self.raw_output.len())
            .field("generated_token_count", &self.generated_token_ids.len())
            .field("event_json_bytes", &self.event_json.len())
            .field("backend_audit_json_bytes", &self.backend_audit_json.len())
            .field(
                "has_terminal_sampled_token",
                &self.terminal_sampled_token_id.is_some(),
            )
            .field("outcome", &self.outcome)
            .field("verification_fingerprint", &self.verification_fingerprint)
            .finish()
    }
}

/// Complete reconstructible evidence for one persisted native batch.
///
/// This type deliberately carries no opaque capability. A caller may construct
/// it freely; success only returns diagnostic checked facts.
pub struct PersistedInferenceBatchRef<'a> {
    pub binding: PersistedBindingEvidenceRef<'a>,
    pub prompt: PersistedPromptEvidenceRef<'a>,
    pub runtime_model_fingerprint: BlobId,
    pub backend_request_id: &'a str,
    pub cases: &'a [PersistedInferenceCaseRef<'a>],
    pub verification_fingerprint: BlobId,
}

impl fmt::Debug for PersistedInferenceBatchRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersistedInferenceBatchRef")
            .field("binding", &self.binding)
            .field("prompt", &self.prompt)
            .field("runtime_model_fingerprint", &self.runtime_model_fingerprint)
            .field("backend_request_id", &self.backend_request_id)
            .field("case_count", &self.cases.len())
            .field("verification_fingerprint", &self.verification_fingerprint)
            .finish()
    }
}

/// Checked facts for one persisted case. This is data, never authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CheckedPersistedCaseFacts {
    input_index: usize,
    call_id: ModelCallId,
    completed: bool,
    sampler_fingerprint: BlobId,
    raw_output_blob_id: BlobId,
    displayed_output_blob_id: BlobId,
    generated_token_ids_blob_id: BlobId,
    verification_fingerprint: BlobId,
    event_blob_id: BlobId,
    backend_receipt_blob_id: BlobId,
}

impl CheckedPersistedCaseFacts {
    pub const fn input_index(&self) -> usize {
        self.input_index
    }

    pub const fn call_id(&self) -> ModelCallId {
        self.call_id
    }

    pub const fn is_completed(&self) -> bool {
        self.completed
    }

    pub const fn sampler_fingerprint(&self) -> BlobId {
        self.sampler_fingerprint
    }

    pub const fn raw_output_blob_id(&self) -> BlobId {
        self.raw_output_blob_id
    }

    pub const fn displayed_output_blob_id(&self) -> BlobId {
        self.displayed_output_blob_id
    }

    pub const fn generated_token_ids_blob_id(&self) -> BlobId {
        self.generated_token_ids_blob_id
    }

    pub const fn verification_fingerprint(&self) -> BlobId {
        self.verification_fingerprint
    }

    pub const fn event_blob_id(&self) -> BlobId {
        self.event_blob_id
    }

    pub const fn backend_receipt_blob_id(&self) -> BlobId {
        self.backend_receipt_blob_id
    }
}

/// Reconstructible result of a complete persisted-evidence replay.
///
/// This value intentionally implements `Clone`: it is checked diagnostic data,
/// not a live inference seal, admission lease, or research-eligibility proof.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedPersistedBatchFacts {
    request_id: String,
    runtime_model_fingerprint: BlobId,
    verification_fingerprint: BlobId,
    completed_case_count: usize,
    cancelled_case_count: usize,
    cases: Vec<CheckedPersistedCaseFacts>,
}

impl CheckedPersistedBatchFacts {
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    pub const fn runtime_model_fingerprint(&self) -> BlobId {
        self.runtime_model_fingerprint
    }

    pub const fn verification_fingerprint(&self) -> BlobId {
        self.verification_fingerprint
    }

    pub const fn completed_case_count(&self) -> usize {
        self.completed_case_count
    }

    pub const fn cancelled_case_count(&self) -> usize {
        self.cancelled_case_count
    }

    pub fn cases(&self) -> &[CheckedPersistedCaseFacts] {
        &self.cases
    }
}

#[derive(Debug, Error)]
pub enum PersistedEvidenceError {
    #[error("{kind} JSON is malformed or violates its closed schema")]
    Json {
        kind: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("persisted batch evidence is invalid: {0}")]
    Batch(&'static str),
    #[error("persisted case {index} evidence is invalid: {field}")]
    Case { index: usize, field: &'static str },
    #[error("persisted call evidence is invalid")]
    Call(#[source] CallError),
}

impl From<CallError> for PersistedEvidenceError {
    fn from(error: CallError) -> Self {
        Self::Call(error)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StrictSamplingConfig {
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

impl StrictSamplingConfig {
    fn to_native(&self) -> SamplingConfig {
        SamplingConfig {
            seed: self.seed,
            temperature: self.temperature,
            dynamic_temperature_range: self.dynamic_temperature_range,
            dynamic_temperature_exponent: self.dynamic_temperature_exponent,
            top_k: self.top_k,
            top_p: self.top_p,
            min_p: self.min_p,
            typical_p: self.typical_p,
            xtc_probability: self.xtc_probability,
            xtc_threshold: self.xtc_threshold,
            repeat_last_n: self.repeat_last_n,
            repeat_penalty: self.repeat_penalty,
            frequency_penalty: self.frequency_penalty,
            presence_penalty: self.presence_penalty,
            dry_multiplier: self.dry_multiplier,
            dry_base: self.dry_base,
            dry_allowed_length: self.dry_allowed_length,
            dry_penalty_last_n: self.dry_penalty_last_n,
            sampler_order: self.sampler_order.clone(),
            max_tokens: self.max_tokens,
            stop: self.stop.clone(),
        }
    }
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct StrictModelFingerprint {
    format: String,
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

impl StrictModelFingerprint {
    fn as_native(&self) -> ModelFingerprint {
        ModelFingerprint {
            // The semantic fingerprint deliberately excludes this operational
            // resident label, and persisted receipts intentionally omit it.
            model_id: String::new(),
            model_size: self.model_size,
            model_sha256: self.model_sha256.clone(),
            tokenizer_sha256: self.tokenizer_sha256.clone(),
            chat_template_sha256: self.chat_template_sha256.clone(),
            multimodal_projector_sha256: self.multimodal_projector_sha256.clone(),
            binding_version: self.binding_version.clone(),
            build_id: self.build_id.clone(),
            backend: self.backend.clone(),
            context_tokens: self.context_tokens,
            batch_tokens: self.batch_tokens,
            max_sequences: self.max_sequences,
            rope_config_sha256: self.rope_config_sha256.clone(),
            kv_layout_sha256: self.kv_layout_sha256.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StrictTokenProbabilityObservation {
    stage: ProbabilityStage,
    probability: f32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StrictTokenObservation {
    generated_index: usize,
    token_id: i32,
    probabilities: Vec<StrictTokenProbabilityObservation>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_field_names)]
struct StrictCacheMetrics {
    supplied_prefix_tokens: usize,
    restored_prefix_tokens: usize,
    batch_shared_prefix_tokens: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StrictGenerationMetrics {
    prompt_tokens: usize,
    completion_tokens: usize,
    shared_prefix_tokens: usize,
    duration_ms: u128,
    first_token_ms: Option<u128>,
    tokens_per_second: f64,
    cache: StrictCacheMetrics,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StrictGenerationOutput {
    format: String,
    request_id: String,
    branch_id: String,
    input_index: usize,
    displayed_output_blob_id: BlobId,
    displayed_output_byte_len: usize,
    raw_output_blob_id: BlobId,
    raw_output_byte_len: usize,
    generated_token_ids_blob_id: BlobId,
    generated_token_count: usize,
    token_observations: Option<Vec<StrictTokenObservation>>,
    state: GenerationState,
    finish_reason: String,
    metrics: StrictGenerationMetrics,
    real_engine_invoked: bool,
    fake_fixture: bool,
    transport: NativeTransport,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StrictBackendAuditRecord {
    format: String,
    project_id: ProjectId,
    binding_id: String,
    call_id: ModelCallId,
    scope: CallScope,
    exact_prompt_blob_id: BlobId,
    source_prompt_fingerprint: BlobId,
    prompt_content_fingerprint: BlobId,
    treatment_recipe_fingerprint: BlobId,
    compiled_prompt_fingerprint: BlobId,
    prompt_token_fingerprint: BlobId,
    native_request_id: String,
    native_case_id: String,
    input_index: usize,
    sampler_fingerprint: BlobId,
    sampling: StrictSamplingConfig,
    model_fingerprint: StrictModelFingerprint,
    output: StrictGenerationOutput,
    terminal_sampled_token_id: Option<i32>,
    event_stream_blob_id: BlobId,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StrictControlPromptAudit {
    raw_utf8: Vec<u8>,
    raw_blob_id: BlobId,
    source_prompt_fingerprint: BlobId,
    compiled_prompt_fingerprint: BlobId,
    token_fingerprint: BlobId,
    token_ids: Vec<u32>,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct StrictRuntimeCostAudit {
    conditional_shared_prefix_tokens: usize,
    unconditional_shared_prefix_tokens: usize,
    physical_prompt_evaluations: u64,
    reserved_physical_context_cells: u64,
    sequence_slots: u32,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StrictControlledBackendAuditRecord {
    format: String,
    project_id: ProjectId,
    binding_id: String,
    call_id: ModelCallId,
    scope: CallScope,
    exact_prompt_blob_id: BlobId,
    source_prompt_fingerprint: BlobId,
    prompt_content_fingerprint: BlobId,
    treatment_recipe_fingerprint: BlobId,
    compiled_prompt_fingerprint: BlobId,
    prompt_token_fingerprint: BlobId,
    native_request_id: String,
    native_case_id: String,
    input_index: usize,
    sampler_fingerprint: BlobId,
    sampling: StrictSamplingConfig,
    control_program_fingerprint: BlobId,
    control_exact_float_bits_fingerprint: BlobId,
    unconditional_prompt: Option<StrictControlPromptAudit>,
    model_fingerprint: StrictModelFingerprint,
    output: StrictGenerationOutput,
    distribution_observations: Vec<TokenDistributionObservation>,
    native_output: ControlledGenerationBatchOutput,
    sealed_request_sha256: String,
    sealed_output_sha256: String,
    sealed_event_stream_sha256: String,
    sealed_runtime_operation_ledger_sha256: String,
    sealed_ledger_sha256: String,
    owner_call_sequence: u64,
    runtime_cost: StrictRuntimeCostAudit,
    terminal_sampled_token_id: Option<i32>,
    event_stream_blob_id: BlobId,
}

#[derive(Debug)]
enum StrictBackendAudit {
    Uncontrolled(Box<StrictBackendAuditRecord>),
    Controlled(Box<StrictControlledBackendAuditRecord>),
}

impl StrictBackendAudit {
    fn sampling(&self) -> &StrictSamplingConfig {
        match self {
            Self::Uncontrolled(value) => &value.sampling,
            Self::Controlled(value) => &value.sampling,
        }
    }

    fn control_program_fingerprint(&self) -> BlobId {
        match self {
            Self::Uncontrolled(_) => uncontrolled_program_fingerprint(),
            Self::Controlled(value) => value.control_program_fingerprint,
        }
    }

    const fn is_controlled(&self) -> bool {
        matches!(self, Self::Controlled(_))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
enum StrictGenerationEventKind {
    State { state: GenerationState },
    Delta { text: String },
    Warning { code: String, message: String },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StrictCompactGenerationEvent {
    event_index: u64,
    event: StrictGenerationEventKind,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StrictEventLedgerRecord {
    format: String,
    call_id: ModelCallId,
    native_case_id: String,
    input_index: usize,
    events: Vec<StrictCompactGenerationEvent>,
}

#[derive(Clone, Copy)]
pub(crate) struct RequestCaseCommitment {
    pub(crate) call_id: ModelCallId,
    pub(crate) scope: CallScope,
    pub(crate) sampler_fingerprint: BlobId,
    pub(crate) seed: u32,
}

pub(crate) struct RequestCommitment<'a> {
    pub(crate) project_id: ProjectId,
    pub(crate) binding_fingerprint: BlobId,
    pub(crate) model_fingerprint: BlobId,
    pub(crate) source_prompt_fingerprint: BlobId,
    pub(crate) treatment_recipe_fingerprint: BlobId,
    pub(crate) raw_prompt_blob_id: BlobId,
    pub(crate) compiled_prompt_fingerprint: BlobId,
    pub(crate) prompt_token_fingerprint: BlobId,
    pub(crate) raw_prompt_byte_len: usize,
    pub(crate) prompt_token_ids: &'a [u32],
    pub(crate) cases: &'a [RequestCaseCommitment],
}

pub(crate) fn derive_request_id(commitment: &RequestCommitment<'_>) -> String {
    let mut digest = CanonicalDigest::new(REQUEST_DOMAIN);
    digest.project_id(commitment.project_id);
    digest.blob(commitment.binding_fingerprint);
    digest.blob(commitment.model_fingerprint);
    digest.blob(commitment.source_prompt_fingerprint);
    digest.blob(commitment.treatment_recipe_fingerprint);
    digest.blob(commitment.raw_prompt_blob_id);
    digest.blob(commitment.compiled_prompt_fingerprint);
    digest.blob(commitment.prompt_token_fingerprint);
    digest.u64(commitment.raw_prompt_byte_len as u64);
    digest.token_ids_u32(commitment.prompt_token_ids);
    digest.u64(commitment.cases.len() as u64);
    for case in commitment.cases {
        digest.model_call_id(case.call_id);
        digest.scope(case.scope);
        digest.blob(case.sampler_fingerprint);
        digest.u32(case.seed);
    }
    format!("loom-base-v1-{}", hex_digest(digest.finish()))
}

#[derive(Clone, Copy)]
pub(crate) struct ControlledRequestCaseCommitment {
    pub(crate) call_id: ModelCallId,
    pub(crate) scope: CallScope,
    pub(crate) sampler_fingerprint: BlobId,
    pub(crate) seed: u32,
    pub(crate) unconditional_source_prompt_fingerprint: Option<BlobId>,
    pub(crate) unconditional_raw_prompt_blob_id: Option<BlobId>,
    pub(crate) unconditional_raw_prompt_byte_len: Option<usize>,
    pub(crate) unconditional_prompt_fingerprint: Option<BlobId>,
    pub(crate) unconditional_token_fingerprint: Option<BlobId>,
}

pub(crate) struct ControlledRequestCommitment<'a> {
    pub(crate) project_id: ProjectId,
    pub(crate) binding_fingerprint: BlobId,
    pub(crate) model_fingerprint: BlobId,
    pub(crate) tokenizer_fingerprint: BlobId,
    pub(crate) source_prompt_fingerprint: BlobId,
    pub(crate) treatment_recipe_fingerprint: BlobId,
    pub(crate) raw_prompt_blob_id: BlobId,
    pub(crate) compiled_prompt_fingerprint: BlobId,
    pub(crate) prompt_token_fingerprint: BlobId,
    pub(crate) raw_prompt_byte_len: usize,
    pub(crate) prompt_token_ids: &'a [u32],
    pub(crate) control_program_fingerprint: BlobId,
    pub(crate) control_exact_float_bits_fingerprint: BlobId,
    pub(crate) cases: &'a [ControlledRequestCaseCommitment],
}

pub(crate) fn derive_controlled_request_id(commitment: &ControlledRequestCommitment<'_>) -> String {
    let mut digest = CanonicalDigest::new(CONTROLLED_REQUEST_DOMAIN);
    digest.project_id(commitment.project_id);
    digest.blob(commitment.binding_fingerprint);
    digest.blob(commitment.model_fingerprint);
    digest.blob(commitment.tokenizer_fingerprint);
    digest.blob(commitment.source_prompt_fingerprint);
    digest.blob(commitment.treatment_recipe_fingerprint);
    digest.blob(commitment.raw_prompt_blob_id);
    digest.blob(commitment.compiled_prompt_fingerprint);
    digest.blob(commitment.prompt_token_fingerprint);
    digest.u64(commitment.raw_prompt_byte_len as u64);
    digest.token_ids_u32(commitment.prompt_token_ids);
    digest.blob(commitment.control_program_fingerprint);
    digest.blob(commitment.control_exact_float_bits_fingerprint);
    digest.u64(commitment.cases.len() as u64);
    for case in commitment.cases {
        digest.model_call_id(case.call_id);
        digest.scope(case.scope);
        digest.blob(case.sampler_fingerprint);
        digest.u32(case.seed);
        digest.bool(case.unconditional_source_prompt_fingerprint.is_some());
        if let Some(value) = case.unconditional_source_prompt_fingerprint {
            digest.blob(value);
        }
        digest.bool(case.unconditional_raw_prompt_blob_id.is_some());
        if let Some(value) = case.unconditional_raw_prompt_blob_id {
            digest.blob(value);
        }
        digest.bool(case.unconditional_raw_prompt_byte_len.is_some());
        if let Some(value) = case.unconditional_raw_prompt_byte_len {
            digest.u64(value as u64);
        }
        digest.bool(case.unconditional_prompt_fingerprint.is_some());
        if let Some(value) = case.unconditional_prompt_fingerprint {
            digest.blob(value);
        }
        digest.bool(case.unconditional_token_fingerprint.is_some());
        if let Some(value) = case.unconditional_token_fingerprint {
            digest.blob(value);
        }
    }
    format!("loom-controlled-v1-{}", hex_digest(digest.finish()))
}

pub(crate) struct CallVerificationCommitment<'a> {
    pub(crate) project_id: ProjectId,
    pub(crate) request_id: &'a str,
    pub(crate) call_id: ModelCallId,
    pub(crate) scope: CallScope,
    pub(crate) model_fingerprint: BlobId,
    pub(crate) source_prompt_fingerprint: BlobId,
    pub(crate) treatment_recipe_fingerprint: BlobId,
    pub(crate) raw_prompt_blob_id: BlobId,
    pub(crate) compiled_prompt_fingerprint: BlobId,
    pub(crate) prompt_token_fingerprint: BlobId,
    pub(crate) sampler_fingerprint: BlobId,
    pub(crate) control_program_fingerprint: BlobId,
    pub(crate) raw_output: &'a [u8],
    pub(crate) generated_token_ids: &'a [u32],
    pub(crate) event_blob_id: BlobId,
    pub(crate) backend_receipt_blob_id: BlobId,
    pub(crate) terminal_sampled_token_id: Option<i32>,
}

pub(crate) fn derive_call_verification_fingerprint(
    commitment: &CallVerificationCommitment<'_>,
) -> BlobId {
    let mut digest = CanonicalDigest::new(VERIFICATION_DOMAIN);
    digest.project_id(commitment.project_id);
    digest.str(commitment.request_id);
    digest.model_call_id(commitment.call_id);
    digest.scope(commitment.scope);
    digest.blob(commitment.model_fingerprint);
    digest.blob(commitment.source_prompt_fingerprint);
    digest.blob(commitment.treatment_recipe_fingerprint);
    digest.blob(commitment.raw_prompt_blob_id);
    digest.blob(commitment.compiled_prompt_fingerprint);
    digest.blob(commitment.prompt_token_fingerprint);
    digest.blob(commitment.sampler_fingerprint);
    digest.blob(commitment.control_program_fingerprint);
    digest.blob(BlobId::digest(commitment.raw_output));
    digest.u64(commitment.generated_token_ids.len() as u64);
    for token in commitment.generated_token_ids {
        digest.u32(*token);
    }
    digest.blob(commitment.event_blob_id);
    digest.blob(commitment.backend_receipt_blob_id);
    digest.bool(commitment.terminal_sampled_token_id.is_some());
    if let Some(token) = commitment.terminal_sampled_token_id {
        digest.i32(token);
    }
    digest.finish_blob()
}

#[derive(Clone, Copy)]
pub(crate) struct BatchCaseCommitment {
    pub(crate) call_id: ModelCallId,
    pub(crate) completed: bool,
    pub(crate) verification_fingerprint: BlobId,
}

pub(crate) struct BatchCommitment<'a> {
    pub(crate) project_id: ProjectId,
    pub(crate) binding_fingerprint: BlobId,
    pub(crate) source_prompt_fingerprint: BlobId,
    pub(crate) compiled_prompt_fingerprint: BlobId,
    pub(crate) request_id: &'a str,
    pub(crate) cases: &'a [BatchCaseCommitment],
}

pub(crate) fn derive_batch_verification_fingerprint(commitment: &BatchCommitment<'_>) -> BlobId {
    let mut digest = CanonicalDigest::new(ENVELOPE_DOMAIN);
    digest.project_id(commitment.project_id);
    digest.blob(commitment.binding_fingerprint);
    digest.blob(commitment.source_prompt_fingerprint);
    digest.blob(commitment.compiled_prompt_fingerprint);
    digest.str(commitment.request_id);
    digest.u64(commitment.cases.len() as u64);
    for case in commitment.cases {
        digest.model_call_id(case.call_id);
        digest.bool(case.completed);
        digest.blob(case.verification_fingerprint);
    }
    digest.finish_blob()
}

pub(crate) fn uncontrolled_program_fingerprint() -> BlobId {
    BlobId::digest(NO_CONTROL_PROGRAM_DOMAIN.as_bytes())
}

fn prompt_token_fingerprint(token_ids: &[u32]) -> BlobId {
    let mut digest = CanonicalDigest::new(PROMPT_TOKEN_DOMAIN);
    digest.token_ids_u32(token_ids);
    digest.finish_blob()
}

fn compiled_prompt_fingerprint(prompt: &PersistedPromptEvidenceRef<'_>) -> BlobId {
    compiled_completion_fingerprint(
        prompt.source_prompt_fingerprint,
        prompt.raw_blob_id,
        prompt.ordered_token_ids,
    )
}

fn compiled_completion_fingerprint(
    source_prompt_fingerprint: BlobId,
    raw_blob_id: BlobId,
    ordered_token_ids: &[u32],
) -> BlobId {
    let mut digest = CanonicalDigest::new(COMPILED_PROMPT_DOMAIN);
    digest.blob(source_prompt_fingerprint);
    digest.blob(raw_blob_id);
    digest.u32(1); // PromptFormEvidence::Completion
    digest.u32(1); // PromptTokenPolicyEvidence::NoBosParseSpecial
    digest.token_ids_u32(ordered_token_ids);
    digest.finish_blob()
}

#[cfg(feature = "test-support")]
#[path = "persisted_test_support.rs"]
mod test_support;

#[cfg(feature = "test-support")]
pub use test_support::{
    NonauthorizingPersistedBindingTestVector, NonauthorizingPersistedCaseOutcomeTestVector,
    NonauthorizingPersistedCaseTestSpec, NonauthorizingPersistedCaseTestVector,
    NonauthorizingPersistedEvidenceTestVector, NonauthorizingPersistedPromptTestVector,
    nonauthorizing_persisted_evidence_test_vector,
    nonauthorizing_persisted_evidence_test_vector_for,
    nonauthorizing_persisted_evidence_test_vector_for_cases,
};

fn generated_token_ids_blob_id(token_ids: &[u32]) -> BlobId {
    let mut digest = Sha256::new();
    for token_id in token_ids {
        digest.update(token_id.to_be_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn hex_digest(bytes: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn case_error(index: usize, field: &'static str) -> PersistedEvidenceError {
    PersistedEvidenceError::Case { index, field }
}

fn parse_json<T>(
    bytes: &[u8],
    kind: &'static str,
    index: usize,
) -> Result<T, PersistedEvidenceError>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    if bytes.is_empty() || bytes.len() > MAX_BACKEND_EVIDENCE_BYTES {
        return Err(case_error(index, "evidence JSON is empty or oversized"));
    }
    let value = serde_json::from_slice(bytes)
        .map_err(|source| PersistedEvidenceError::Json { kind, source })?;
    let canonical = serde_json::to_vec(&value)
        .map_err(|source| PersistedEvidenceError::Json { kind, source })?;
    if canonical != bytes {
        return Err(case_error(
            index,
            "evidence JSON is not canonical compact JSON",
        ));
    }
    Ok(value)
}

fn parse_backend_audit(
    bytes: &[u8],
    index: usize,
) -> Result<StrictBackendAudit, PersistedEvidenceError> {
    if bytes.is_empty() || bytes.len() > MAX_BACKEND_EVIDENCE_BYTES {
        return Err(case_error(index, "evidence JSON is empty or oversized"));
    }
    // Decode only far enough to select the closed record type.  Do not run the
    // canonical-byte check through `Value`: its map ordering is unrelated to
    // the declaration order used when the typed producer serialized the
    // record.  `parse_json` below performs the one authoritative typed
    // round-trip and rejects unknown fields and non-canonical bytes.
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|source| PersistedEvidenceError::Json {
            kind: "backend audit",
            source,
        })?;
    let format = value
        .get("format")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| case_error(index, "backend audit format is absent"))?;
    match format {
        BACKEND_AUDIT_FORMAT => parse_json(bytes, "backend audit", index)
            .map(Box::new)
            .map(StrictBackendAudit::Uncontrolled),
        CONTROLLED_BACKEND_AUDIT_FORMAT => parse_json(bytes, "controlled backend audit", index)
            .map(Box::new)
            .map(StrictBackendAudit::Controlled),
        _ => Err(case_error(index, "backend audit format is unsupported")),
    }
}

fn parse_sha256(
    value: &str,
    index: usize,
    field: &'static str,
) -> Result<BlobId, PersistedEvidenceError> {
    BlobId::from_str(value).map_err(|_| case_error(index, field))
}

fn validate_prompt(prompt: &PersistedPromptEvidenceRef<'_>) -> Result<(), PersistedEvidenceError> {
    if prompt.raw_utf8.is_empty()
        || prompt.raw_utf8.len() > loom_research_types::MAX_COMPLETION_PROMPT_BYTES
        || std::str::from_utf8(prompt.raw_utf8).is_err()
    {
        return Err(PersistedEvidenceError::Batch(
            "exact prompt is empty, oversized, or not UTF-8",
        ));
    }
    if BlobId::digest(prompt.raw_utf8) != prompt.raw_blob_id {
        return Err(PersistedEvidenceError::Batch(
            "exact prompt bytes differ from their blob ID",
        ));
    }
    if prompt.ordered_token_ids.is_empty()
        || prompt.ordered_token_ids.len() > MAX_GENERATED_TOKENS as usize
        || prompt
            .ordered_token_ids
            .iter()
            .any(|token| *token > i32::MAX as u32)
    {
        return Err(PersistedEvidenceError::Batch(
            "exact prompt token IDs are empty, oversized, or non-native",
        ));
    }
    if prompt_token_fingerprint(prompt.ordered_token_ids) != prompt.token_fingerprint {
        return Err(PersistedEvidenceError::Batch(
            "prompt token fingerprint mismatch",
        ));
    }
    if compiled_prompt_fingerprint(prompt) != prompt.compiled_fingerprint {
        return Err(PersistedEvidenceError::Batch(
            "compiled prompt fingerprint mismatch",
        ));
    }
    Ok(())
}

fn validate_sampling(
    sampling: &SamplingConfig,
    index: usize,
) -> Result<(), PersistedEvidenceError> {
    if sampling.seed == u32::MAX {
        return Err(case_error(index, "sampling seed is the random sentinel"));
    }
    let bounded = [
        (sampling.temperature, 0.0, 100.0),
        (sampling.dynamic_temperature_range, 0.0, 100.0),
        (
            sampling.dynamic_temperature_exponent,
            f32::MIN_POSITIVE,
            100.0,
        ),
        (sampling.top_p, 0.0, 1.0),
        (sampling.min_p, 0.0, 1.0),
        (sampling.typical_p, 0.0, 1.0),
        (sampling.xtc_probability, 0.0, 1.0),
        (sampling.xtc_threshold, 0.0, 1.0),
        (sampling.repeat_penalty, 0.0, 100.0),
        (sampling.frequency_penalty, -100.0, 100.0),
        (sampling.presence_penalty, -100.0, 100.0),
        (sampling.dry_multiplier, 0.0, 100.0),
        (sampling.dry_base, f32::MIN_POSITIVE, 100.0),
    ];
    if bounded
        .iter()
        .any(|(value, minimum, maximum)| !value.is_finite() || value < minimum || value > maximum)
    {
        return Err(case_error(index, "sampling float is outside its bound"));
    }
    if sampling.top_k < 0
        || sampling.repeat_last_n < -1
        || sampling.dry_penalty_last_n < -1
        || sampling.dry_allowed_length < 0
    {
        return Err(case_error(index, "sampling integer is outside its bound"));
    }
    if sampling.max_tokens == 0 || sampling.max_tokens > MAX_GENERATED_TOKENS {
        return Err(case_error(
            index,
            "sampling token budget is outside its bound",
        ));
    }
    if sampling.sampler_order.len() > 8
        || sampling
            .sampler_order
            .iter()
            .enumerate()
            .any(|(position, sampler)| sampling.sampler_order[..position].contains(sampler))
    {
        return Err(case_error(index, "sampler order contains a duplicate"));
    }
    if sampling.stop.len() > MAX_STOP_SEQUENCES {
        return Err(case_error(index, "sampling has too many stop sequences"));
    }
    let mut stop_bytes = 0_usize;
    for stop in &sampling.stop {
        if stop.is_empty() {
            return Err(case_error(
                index,
                "sampling contains an empty stop sequence",
            ));
        }
        stop_bytes = stop_bytes
            .checked_add(stop.len())
            .ok_or_else(|| case_error(index, "sampling stop bytes overflow"))?;
    }
    if stop_bytes > MAX_STOP_SEQUENCE_BYTES {
        return Err(case_error(
            index,
            "sampling stop bytes exceed the native bound",
        ));
    }
    Ok(())
}

fn validate_model_fingerprint(
    model: &StrictModelFingerprint,
    expected: PersistedBindingEvidenceRef<'_>,
    expected_runtime_fingerprint: BlobId,
    index: usize,
) -> Result<(), PersistedEvidenceError> {
    if model.format != SANITIZED_MODEL_FINGERPRINT_FORMAT
        || model.model_size == 0
        || model.context_tokens == 0
        || model.batch_tokens == 0
        || model.max_sequences == 0
        || model.binding_version.is_empty()
        || model.binding_version.len() > MAX_RECEIPT_ID_BYTES
        || model.build_id.is_empty()
        || model.build_id.len() > MAX_RECEIPT_ID_BYTES
        || model.backend.is_empty()
        || model.backend.len() > MAX_RECEIPT_ID_BYTES
    {
        return Err(case_error(index, "runtime model fingerprint is malformed"));
    }
    let model_sha = parse_sha256(&model.model_sha256, index, "model digest is malformed")?;
    let tokenizer_sha = parse_sha256(
        &model.tokenizer_sha256,
        index,
        "tokenizer digest is malformed",
    )?;
    let _ = parse_sha256(
        &model.chat_template_sha256,
        index,
        "chat-template digest is malformed",
    )?;
    let projector_sha = model
        .multimodal_projector_sha256
        .as_deref()
        .map(|value| parse_sha256(value, index, "projector digest is malformed"))
        .transpose()?;
    let _ = parse_sha256(&model.rope_config_sha256, index, "RoPE digest is malformed")?;
    let _ = parse_sha256(
        &model.kv_layout_sha256,
        index,
        "KV-layout digest is malformed",
    )?;
    if model_sha != expected.model_sha256
        || model.model_size != expected.model_byte_len
        || tokenizer_sha != expected.tokenizer_sha256
        || projector_sha != expected.multimodal_projector_sha256
        || expected.context_tokens == 0
        || model.context_tokens < expected.context_tokens
    {
        return Err(case_error(
            index,
            "runtime model differs from the recompiled binding",
        ));
    }
    if model_fingerprint_id(&model.as_native()) != expected_runtime_fingerprint {
        return Err(case_error(index, "runtime model fingerprint mismatch"));
    }
    Ok(())
}

fn validate_event_ledger(
    ledger: &StrictEventLedgerRecord,
    case: PersistedInferenceCaseRef<'_>,
    native_case_id: &str,
    terminal_state: GenerationState,
) -> Result<(), PersistedEvidenceError> {
    let index = case.input_index;
    if ledger.format != EVENT_LEDGER_FORMAT
        || ledger.call_id != case.model_call.id()
        || ledger.native_case_id != native_case_id
        || ledger.input_index != index
        || ledger.events.len() < 3
        || ledger.events.len() > case.generated_token_ids.len().saturating_add(3)
    {
        return Err(case_error(index, "event ledger header or count mismatch"));
    }
    let mut raw_output = String::new();
    let mut terminal = None;
    for (expected_index, event) in ledger.events.iter().enumerate() {
        if event.event_index != expected_index as u64 || terminal.is_some() {
            return Err(case_error(
                index,
                "event indexes or terminal ordering mismatch",
            ));
        }
        match &event.event {
            StrictGenerationEventKind::State {
                state: GenerationState::Prefilling,
            } if expected_index == 0 => {}
            StrictGenerationEventKind::State {
                state: GenerationState::Generating,
            } if expected_index == 1 => {}
            StrictGenerationEventKind::Delta { text }
                if expected_index >= 2 && !text.is_empty() =>
            {
                if raw_output
                    .len()
                    .checked_add(text.len())
                    .is_none_or(|length| length as u64 > MAX_RAW_OUTPUT_BYTES)
                {
                    return Err(case_error(index, "event delta text exceeds its bound"));
                }
                raw_output.push_str(text);
            }
            StrictGenerationEventKind::State { state }
                if expected_index >= 2
                    && matches!(
                        state,
                        GenerationState::Completed | GenerationState::Cancelled
                    ) =>
            {
                terminal = Some(*state);
            }
            StrictGenerationEventKind::Warning { code, message } => {
                let _ = (code, message);
                return Err(case_error(index, "event ledger contains a warning"));
            }
            StrictGenerationEventKind::State { .. } | StrictGenerationEventKind::Delta { .. } => {
                return Err(case_error(index, "event transition is impossible"));
            }
        }
    }
    if terminal != Some(terminal_state) || raw_output.as_bytes() != case.raw_output {
        return Err(case_error(
            index,
            "event terminal or reconstructed raw output mismatch",
        ));
    }
    Ok(())
}

fn validate_token_observations(
    observations: Option<&[StrictTokenObservation]>,
    generated_token_ids: &[u32],
    index: usize,
) -> Result<(), PersistedEvidenceError> {
    let Some(observations) = observations else {
        return Ok(());
    };
    if observations.len() > generated_token_ids.len() {
        return Err(case_error(index, "too many token observations"));
    }
    let mut observed_indexes = HashSet::with_capacity(observations.len());
    for observation in observations {
        let Some(expected_token) = generated_token_ids.get(observation.generated_index) else {
            return Err(case_error(
                index,
                "token observation index is outside output",
            ));
        };
        if !observed_indexes.insert(observation.generated_index)
            || observation.token_id < 0
            || u32::try_from(observation.token_id).ok() != Some(*expected_token)
            || observation.probabilities.is_empty()
            || observation.probabilities.len() > MAX_DISTRIBUTION_OBSERVATIONS_PER_TOKEN
        {
            return Err(case_error(
                index,
                "token observation cardinality or identity mismatch",
            ));
        }
        let mut stages = HashSet::with_capacity(observation.probabilities.len());
        for probability in &observation.probabilities {
            if !stages.insert(probability.stage)
                || !probability.probability.is_finite()
                || !(0.0..=1.0).contains(&probability.probability)
            {
                return Err(case_error(
                    index,
                    "token observation stage or probability is invalid",
                ));
            }
        }
    }
    Ok(())
}

fn validate_model_call(
    case: PersistedInferenceCaseRef<'_>,
    prompt: &PersistedPromptEvidenceRef<'_>,
    binding: PersistedBindingEvidenceRef<'_>,
    runtime_model_fingerprint: BlobId,
    sampler_fingerprint: BlobId,
    control_program_fingerprint: BlobId,
    sampling: &SamplingConfig,
) -> Result<(), PersistedEvidenceError> {
    let index = case.input_index;
    let call = case.model_call;
    let identity = call.identity();
    if call.evidence_class() != CallEvidenceClass::LiveBaseWriterClaim {
        return Err(case_error(index, "call is not a live base-writer claim"));
    }
    if identity.scope() != prompt.scope
        || identity.model_fingerprint() != runtime_model_fingerprint
        || identity.tokenizer_fingerprint() != binding.tokenizer_sha256
        || identity.prompt_fingerprint() != prompt.compiled_fingerprint
        || identity.sampler_fingerprint() != sampler_fingerprint
        || identity.control_program_fingerprint() != control_program_fingerprint
        || identity.seed() != u64::from(sampling.seed)
    {
        return Err(case_error(index, "model-call identity mismatch"));
    }
    if case.raw_output.len() as u64 > MAX_RAW_OUTPUT_BYTES
        || std::str::from_utf8(case.raw_output).is_err()
        || case.generated_token_ids.len() > MAX_GENERATED_TOKENS as usize
        || case
            .generated_token_ids
            .iter()
            .any(|token| *token > i32::MAX as u32)
        || case
            .terminal_sampled_token_id
            .is_some_and(|token| token < 0)
    {
        return Err(case_error(index, "raw output or token evidence is invalid"));
    }
    Ok(())
}

struct OutputValidationContext<'a> {
    sampling: &'a SamplingConfig,
    request_id: &'a str,
    native_case_id: &'a str,
    prompt_tokens: usize,
    shared_prefix_tokens: usize,
    event_blob_id: BlobId,
    backend_receipt_blob_id: BlobId,
}

fn validate_output_header_and_metrics(
    case: PersistedInferenceCaseRef<'_>,
    output: &StrictGenerationOutput,
    context: &OutputValidationContext<'_>,
) -> Result<(), PersistedEvidenceError> {
    let index = case.input_index;
    if output.format != OUTPUT_AUDIT_FORMAT
        || output.request_id != context.request_id
        || output.branch_id != context.native_case_id
        || output.input_index != index
        || output.raw_output_blob_id != BlobId::digest(case.raw_output)
        || output.raw_output_byte_len != case.raw_output.len()
        || output.generated_token_ids_blob_id
            != generated_token_ids_blob_id(case.generated_token_ids)
        || output.generated_token_count != case.generated_token_ids.len()
    {
        return Err(case_error(index, "output audit identity or blob mismatch"));
    }
    if !output.real_engine_invoked
        || output.fake_fixture
        || output.transport != NativeTransport::InProcess
    {
        return Err(case_error(index, "output is not real in-process evidence"));
    }
    if output.metrics.prompt_tokens != context.prompt_tokens
        || output.metrics.completion_tokens != case.generated_token_ids.len()
        || output.metrics.shared_prefix_tokens != context.shared_prefix_tokens
        || output.metrics.cache.supplied_prefix_tokens != 0
        || output.metrics.cache.restored_prefix_tokens != 0
        || output.metrics.cache.batch_shared_prefix_tokens != context.shared_prefix_tokens
        || case.generated_token_ids.len() > context.sampling.max_tokens as usize
        || !output.metrics.tokens_per_second.is_finite()
        || output.metrics.tokens_per_second < 0.0
        || output
            .metrics
            .first_token_ms
            .is_some_and(|first| first > output.metrics.duration_ms)
    {
        return Err(case_error(index, "output metrics mismatch"));
    }
    validate_token_observations(
        output.token_observations.as_deref(),
        case.generated_token_ids,
        index,
    )
}

fn validate_persisted_outcome<'a>(
    case: PersistedInferenceCaseRef<'a>,
    context: &OutputValidationContext<'_>,
) -> Result<(bool, &'a [u8]), PersistedEvidenceError> {
    let index = case.input_index;
    match case.outcome {
        PersistedCaseOutcomeRef::Completed {
            displayed_output,
            output_projection,
        } => {
            let CallTerminal::Completed(completed) = case.model_call.terminal() else {
                return Err(case_error(
                    index,
                    "completed outcome has non-completed call",
                ));
            };
            if completed.raw_output_blob_id() != BlobId::digest(case.raw_output)
                || completed.raw_output_byte_len() != case.raw_output.len() as u64
                || completed.raw_event_stream_blob_id() != context.event_blob_id
                || completed.backend_receipt_blob_id() != Some(context.backend_receipt_blob_id)
            {
                return Err(case_error(index, "completed call receipt mismatch"));
            }
            completed
                .token_evidence()
                .verify(case.generated_token_ids)?;
            if std::str::from_utf8(displayed_output).is_err() {
                return Err(case_error(index, "displayed output is not UTF-8"));
            }
            match (displayed_output.is_empty(), output_projection) {
                (true, None) => {}
                (false, Some(projection)) => {
                    projection.verify_raw_bytes(case.raw_output)?;
                    if projection.displayed_str(case.raw_output)?.as_bytes() != displayed_output
                        || projection.endpoint_excluded_tail().start()
                            != projection.endpoint_excluded_tail().end()
                        || projection.displayed().end() != displayed_output.len() as u64
                    {
                        return Err(case_error(index, "output projection mismatch"));
                    }
                }
                (true, Some(_)) | (false, None) => {
                    return Err(case_error(index, "output projection presence mismatch"));
                }
            }
            Ok((true, displayed_output))
        }
        PersistedCaseOutcomeRef::Cancelled => {
            let CallTerminal::Cancelled { message } = case.model_call.terminal() else {
                return Err(case_error(
                    index,
                    "cancelled outcome has non-cancelled call",
                ));
            };
            if message.as_str() != CANCELLED_MESSAGE {
                return Err(case_error(index, "cancelled call message mismatch"));
            }
            Ok((false, case.raw_output))
        }
    }
}

fn validate_output_terminal(
    case: PersistedInferenceCaseRef<'_>,
    output: &StrictGenerationOutput,
    sampling: &SamplingConfig,
    completed: bool,
    displayed_output: &[u8],
) -> Result<(), PersistedEvidenceError> {
    let index = case.input_index;
    if output.displayed_output_blob_id != BlobId::digest(displayed_output)
        || output.displayed_output_byte_len != displayed_output.len()
    {
        return Err(case_error(index, "displayed output blob mismatch"));
    }

    match (output.state, output.finish_reason.as_str(), completed) {
        (GenerationState::Cancelled, "cancelled", false)
            if case.terminal_sampled_token_id.is_none() && displayed_output == case.raw_output => {}
        (GenerationState::Completed, "end_of_generation", true)
            if case.terminal_sampled_token_id.is_some() && displayed_output == case.raw_output => {}
        (GenerationState::Completed, "max_tokens", true)
            if case.terminal_sampled_token_id.is_none()
                && displayed_output == case.raw_output
                && case.generated_token_ids.len() == sampling.max_tokens as usize => {}
        (GenerationState::Completed, "stop_sequence", true)
            if case.terminal_sampled_token_id.is_none()
                && case
                    .raw_output
                    .strip_prefix(displayed_output)
                    .is_some_and(|suffix| {
                        !suffix.is_empty()
                            && sampling.stop.iter().any(|stop| stop.as_bytes() == suffix)
                    }) => {}
        _ => {
            return Err(case_error(
                index,
                "terminal state or output projection mismatch",
            ));
        }
    }
    Ok(())
}

fn validate_output_and_terminal(
    case: PersistedInferenceCaseRef<'_>,
    output: &StrictGenerationOutput,
    context: &OutputValidationContext<'_>,
) -> Result<(bool, BlobId), PersistedEvidenceError> {
    validate_output_header_and_metrics(case, output, context)?;
    let (completed, displayed_output) = validate_persisted_outcome(case, context)?;
    validate_output_terminal(case, output, context.sampling, completed, displayed_output)?;
    Ok((completed, BlobId::digest(displayed_output)))
}

fn validate_uncontrolled_case_audit(
    batch: &PersistedInferenceBatchRef<'_>,
    case: PersistedInferenceCaseRef<'_>,
    audit: &StrictBackendAuditRecord,
    ledger: &StrictEventLedgerRecord,
    sampling: &SamplingConfig,
    expected_shared_prefix_tokens: usize,
) -> Result<CheckedPersistedCaseFacts, PersistedEvidenceError> {
    let index = case.input_index;
    let native_case_id = format!("call-{}", case.model_call.id());
    let sampler_fingerprint = sampling_fingerprint(sampling);
    if audit.format != BACKEND_AUDIT_FORMAT
        || audit.project_id != batch.prompt.project_id
        || audit.binding_id != batch.binding.binding_id
        || audit.call_id != case.model_call.id()
        || audit.scope != batch.prompt.scope
        || audit.exact_prompt_blob_id != batch.prompt.raw_blob_id
        || audit.source_prompt_fingerprint != batch.prompt.source_prompt_fingerprint
        || audit.prompt_content_fingerprint != batch.prompt.content_fingerprint
        || audit.treatment_recipe_fingerprint != batch.prompt.treatment_recipe_fingerprint
        || audit.compiled_prompt_fingerprint != batch.prompt.compiled_fingerprint
        || audit.prompt_token_fingerprint != batch.prompt.token_fingerprint
        || audit.native_request_id != batch.backend_request_id
        || audit.native_case_id != native_case_id
        || audit.input_index != index
        || audit.sampler_fingerprint != sampler_fingerprint
        || audit.terminal_sampled_token_id != case.terminal_sampled_token_id
    {
        return Err(case_error(index, "backend audit identity mismatch"));
    }
    validate_model_fingerprint(
        &audit.model_fingerprint,
        batch.binding,
        batch.runtime_model_fingerprint,
        index,
    )?;
    if batch.cases.len() > audit.model_fingerprint.max_sequences as usize {
        return Err(case_error(index, "batch exceeds runtime sequence capacity"));
    }
    let requested_tokens = batch
        .prompt
        .ordered_token_ids
        .len()
        .checked_add(sampling.max_tokens as usize)
        .ok_or_else(|| case_error(index, "request token budget overflow"))?;
    if requested_tokens > audit.model_fingerprint.context_tokens as usize {
        return Err(case_error(index, "request exceeds runtime context"));
    }

    let event_blob_id = BlobId::digest(case.event_json);
    if audit.event_stream_blob_id != event_blob_id {
        return Err(case_error(index, "event stream blob mismatch"));
    }
    validate_event_ledger(ledger, case, &native_case_id, audit.output.state)?;
    let backend_receipt_blob_id = BlobId::digest(case.backend_audit_json);
    let (completed, displayed_output_blob_id) = validate_output_and_terminal(
        case,
        &audit.output,
        &OutputValidationContext {
            sampling,
            request_id: batch.backend_request_id,
            native_case_id: &native_case_id,
            prompt_tokens: batch.prompt.ordered_token_ids.len(),
            shared_prefix_tokens: expected_shared_prefix_tokens,
            event_blob_id,
            backend_receipt_blob_id,
        },
    )?;
    let verification_fingerprint =
        derive_call_verification_fingerprint(&CallVerificationCommitment {
            project_id: batch.prompt.project_id,
            request_id: batch.backend_request_id,
            call_id: case.model_call.id(),
            scope: batch.prompt.scope,
            model_fingerprint: batch.runtime_model_fingerprint,
            source_prompt_fingerprint: batch.prompt.source_prompt_fingerprint,
            treatment_recipe_fingerprint: batch.prompt.treatment_recipe_fingerprint,
            raw_prompt_blob_id: batch.prompt.raw_blob_id,
            compiled_prompt_fingerprint: batch.prompt.compiled_fingerprint,
            prompt_token_fingerprint: batch.prompt.token_fingerprint,
            sampler_fingerprint,
            control_program_fingerprint: uncontrolled_program_fingerprint(),
            raw_output: case.raw_output,
            generated_token_ids: case.generated_token_ids,
            event_blob_id,
            backend_receipt_blob_id,
            terminal_sampled_token_id: case.terminal_sampled_token_id,
        });
    if verification_fingerprint != case.verification_fingerprint {
        return Err(case_error(index, "case verification fingerprint mismatch"));
    }
    Ok(CheckedPersistedCaseFacts {
        input_index: index,
        call_id: case.model_call.id(),
        completed,
        sampler_fingerprint,
        raw_output_blob_id: BlobId::digest(case.raw_output),
        displayed_output_blob_id,
        generated_token_ids_blob_id: generated_token_ids_blob_id(case.generated_token_ids),
        verification_fingerprint,
        event_blob_id,
        backend_receipt_blob_id,
    })
}

fn validate_controlled_case_audit(
    batch: &PersistedInferenceBatchRef<'_>,
    case: PersistedInferenceCaseRef<'_>,
    audit: &StrictControlledBackendAuditRecord,
    ledger: &StrictEventLedgerRecord,
    sampling: &SamplingConfig,
) -> Result<CheckedPersistedCaseFacts, PersistedEvidenceError> {
    let index = case.input_index;
    let native_case_id = format!("call-{}", case.model_call.id());
    let sampler_fingerprint = sampling_fingerprint(sampling);
    validate_controlled_case_identity_and_native_copy(
        batch,
        case,
        audit,
        sampling,
        &native_case_id,
        sampler_fingerprint,
    )?;

    let event_blob_id = BlobId::digest(case.event_json);
    if audit.event_stream_blob_id != event_blob_id {
        return Err(case_error(index, "controlled event stream blob mismatch"));
    }
    validate_event_ledger(ledger, case, &native_case_id, audit.output.state)?;
    let backend_receipt_blob_id = BlobId::digest(case.backend_audit_json);
    let (completed, displayed_output_blob_id) = validate_output_and_terminal(
        case,
        &audit.output,
        &OutputValidationContext {
            sampling,
            request_id: batch.backend_request_id,
            native_case_id: &native_case_id,
            prompt_tokens: batch.prompt.ordered_token_ids.len(),
            shared_prefix_tokens: audit.runtime_cost.conditional_shared_prefix_tokens,
            event_blob_id,
            backend_receipt_blob_id,
        },
    )?;
    if !completed {
        return Err(case_error(
            index,
            "verified controlled generation cannot carry a cancelled terminal",
        ));
    }
    finish_controlled_case_facts(
        batch,
        case,
        audit,
        sampler_fingerprint,
        ControlledCaseReceiptFacts {
            displayed: displayed_output_blob_id,
            event: event_blob_id,
            backend_receipt: backend_receipt_blob_id,
        },
    )
}

fn validate_controlled_case_identity_and_native_copy(
    batch: &PersistedInferenceBatchRef<'_>,
    case: PersistedInferenceCaseRef<'_>,
    audit: &StrictControlledBackendAuditRecord,
    sampling: &SamplingConfig,
    native_case_id: &str,
    sampler_fingerprint: BlobId,
) -> Result<(), PersistedEvidenceError> {
    let index = case.input_index;
    if audit.format != CONTROLLED_BACKEND_AUDIT_FORMAT
        || audit.project_id != batch.prompt.project_id
        || audit.binding_id != batch.binding.binding_id
        || audit.call_id != case.model_call.id()
        || audit.scope != batch.prompt.scope
        || audit.exact_prompt_blob_id != batch.prompt.raw_blob_id
        || audit.source_prompt_fingerprint != batch.prompt.source_prompt_fingerprint
        || audit.prompt_content_fingerprint != batch.prompt.content_fingerprint
        || audit.treatment_recipe_fingerprint != batch.prompt.treatment_recipe_fingerprint
        || audit.compiled_prompt_fingerprint != batch.prompt.compiled_fingerprint
        || audit.prompt_token_fingerprint != batch.prompt.token_fingerprint
        || audit.native_request_id != batch.backend_request_id
        || audit.native_case_id != native_case_id
        || audit.input_index != index
        || audit.sampler_fingerprint != sampler_fingerprint
        || audit.terminal_sampled_token_id != case.terminal_sampled_token_id
    {
        return Err(case_error(
            index,
            "controlled backend audit identity mismatch",
        ));
    }
    validate_model_fingerprint(
        &audit.model_fingerprint,
        batch.binding,
        batch.runtime_model_fingerprint,
        index,
    )?;
    let request = audit.native_output.request();
    let native_case = request
        .cases()
        .get(index)
        .ok_or_else(|| case_error(index, "controlled native request case is absent"))?;
    let native_case_output = audit
        .native_output
        .cases()
        .get(index)
        .ok_or_else(|| case_error(index, "controlled native output case is absent"))?;
    if request.request_id() != batch.backend_request_id
        || request.cases().len() != batch.cases.len()
        || audit.native_output.cases().len() != batch.cases.len()
        || native_case.case_id() != native_case_id
        || native_case_output.case_id() != native_case_id
        || native_case.sampling() != sampling
        || native_case.conditional_prompt().token_ids()
            != to_i32_persisted_tokens(batch.prompt.ordered_token_ids, index)?.as_slice()
        || parse_sha256(
            &request.control().fingerprint_sha256(),
            index,
            "controlled program fingerprint is malformed",
        )? != audit.control_program_fingerprint
        || parse_sha256(
            &request.exact_float_bits_sha256(),
            index,
            "controlled float-bits fingerprint is malformed",
        )? != audit.control_exact_float_bits_fingerprint
        || model_fingerprint_id(request.control().writer().fingerprint())
            != batch.runtime_model_fingerprint
        || request
            .control()
            .writer()
            .token_contract()
            .tokenizer_sha256()
            != batch.binding.tokenizer_sha256.to_hex()
    {
        return Err(case_error(index, "controlled native request mismatch"));
    }
    validate_control_prompt(audit, native_case, index)?;
    validate_controlled_seal_diagnostics(audit, index)?;
    if native_case_output.distribution_observations() != audit.distribution_observations {
        return Err(case_error(
            index,
            "controlled distribution observations changed",
        ));
    }
    validate_native_generation_copy(&audit.output, native_case_output.generation(), index)?;
    Ok(())
}

#[derive(Clone, Copy)]
struct ControlledCaseReceiptFacts {
    displayed: BlobId,
    event: BlobId,
    backend_receipt: BlobId,
}

fn finish_controlled_case_facts(
    batch: &PersistedInferenceBatchRef<'_>,
    case: PersistedInferenceCaseRef<'_>,
    audit: &StrictControlledBackendAuditRecord,
    sampler_fingerprint: BlobId,
    receipt: ControlledCaseReceiptFacts,
) -> Result<CheckedPersistedCaseFacts, PersistedEvidenceError> {
    let index = case.input_index;
    let verification_fingerprint =
        derive_call_verification_fingerprint(&CallVerificationCommitment {
            project_id: batch.prompt.project_id,
            request_id: batch.backend_request_id,
            call_id: case.model_call.id(),
            scope: batch.prompt.scope,
            model_fingerprint: batch.runtime_model_fingerprint,
            source_prompt_fingerprint: batch.prompt.source_prompt_fingerprint,
            treatment_recipe_fingerprint: batch.prompt.treatment_recipe_fingerprint,
            raw_prompt_blob_id: batch.prompt.raw_blob_id,
            compiled_prompt_fingerprint: batch.prompt.compiled_fingerprint,
            prompt_token_fingerprint: batch.prompt.token_fingerprint,
            sampler_fingerprint,
            control_program_fingerprint: audit.control_program_fingerprint,
            raw_output: case.raw_output,
            generated_token_ids: case.generated_token_ids,
            event_blob_id: receipt.event,
            backend_receipt_blob_id: receipt.backend_receipt,
            terminal_sampled_token_id: case.terminal_sampled_token_id,
        });
    if verification_fingerprint != case.verification_fingerprint {
        return Err(case_error(
            index,
            "controlled case verification fingerprint mismatch",
        ));
    }
    Ok(CheckedPersistedCaseFacts {
        input_index: index,
        call_id: case.model_call.id(),
        completed: true,
        sampler_fingerprint,
        raw_output_blob_id: BlobId::digest(case.raw_output),
        displayed_output_blob_id: receipt.displayed,
        generated_token_ids_blob_id: generated_token_ids_blob_id(case.generated_token_ids),
        verification_fingerprint,
        event_blob_id: receipt.event,
        backend_receipt_blob_id: receipt.backend_receipt,
    })
}

fn validate_control_prompt(
    audit: &StrictControlledBackendAuditRecord,
    native_case: &llama_native_types::ControlledGenerationCase,
    index: usize,
) -> Result<(), PersistedEvidenceError> {
    match (
        &audit.unconditional_prompt,
        native_case.unconditional_prompt(),
    ) {
        (None, None) => Ok(()),
        (Some(prompt), Some(native)) => {
            if prompt.raw_utf8.is_empty()
                || prompt.raw_utf8.len() > loom_research_types::MAX_COMPLETION_PROMPT_BYTES
                || std::str::from_utf8(&prompt.raw_utf8).is_err()
                || BlobId::digest(&prompt.raw_utf8) != prompt.raw_blob_id
                || prompt.token_ids.is_empty()
                || prompt.token_fingerprint != prompt_token_fingerprint(&prompt.token_ids)
                || prompt.compiled_prompt_fingerprint
                    != compiled_completion_fingerprint(
                        prompt.source_prompt_fingerprint,
                        prompt.raw_blob_id,
                        &prompt.token_ids,
                    )
                || native.token_ids()
                    != to_i32_persisted_tokens(&prompt.token_ids, index)?.as_slice()
            {
                return Err(case_error(index, "unconditional control prompt mismatch"));
            }
            Ok(())
        }
        (None, Some(_)) | (Some(_), None) => Err(case_error(
            index,
            "unconditional control prompt presence mismatch",
        )),
    }
}

fn validate_controlled_seal_diagnostics(
    audit: &StrictControlledBackendAuditRecord,
    index: usize,
) -> Result<(), PersistedEvidenceError> {
    for (value, field) in [
        (&audit.sealed_request_sha256, "sealed request digest"),
        (&audit.sealed_output_sha256, "sealed output digest"),
        (&audit.sealed_event_stream_sha256, "sealed event digest"),
        (
            &audit.sealed_runtime_operation_ledger_sha256,
            "sealed operation-ledger digest",
        ),
        (
            &audit.sealed_ledger_sha256,
            "sealed authority-ledger digest",
        ),
    ] {
        let _ = parse_sha256(value, index, field)?;
    }
    let request = audit.native_output.request();
    let receipt = audit.native_output.receipt();
    if audit.sealed_request_sha256 != request.fingerprint_sha256()
        || audit.sealed_request_sha256 != receipt.request_sha256()
        || audit.sealed_output_sha256 != receipt.output_sha256()
        || receipt.requested_control_sha256() != audit.control_program_fingerprint.to_hex()
        || receipt.exact_float_bits_sha256() != audit.control_exact_float_bits_fingerprint.to_hex()
        || audit.owner_call_sequence == 0
        || audit.runtime_cost.sequence_slots != request.cost().total_sequence_slots()
        || audit.runtime_cost.reserved_physical_context_cells
            > request.cost().maximum_context_token_positions()
        || audit.runtime_cost.physical_prompt_evaluations > request.cost().exact_prompt_tokens()
        || (!request.control().uses_same_model_cfg()
            && audit.runtime_cost.unconditional_shared_prefix_tokens != 0)
    {
        return Err(case_error(index, "controlled sealed diagnostics mismatch"));
    }
    Ok(())
}

fn validate_native_generation_copy(
    audit: &StrictGenerationOutput,
    native: &llama_native_types::GenerationOutput,
    index: usize,
) -> Result<(), PersistedEvidenceError> {
    let native_tokens = native
        .generated_token_ids
        .iter()
        .map(|token| u32::try_from(*token).map_err(|_| case_error(index, "negative native token")))
        .collect::<Result<Vec<_>, _>>()?;
    let token_observations_match = match (&audit.token_observations, &native.token_observations) {
        (None, None) => true,
        (Some(audit_values), Some(native_values)) => {
            audit_values.len() == native_values.len()
                && audit_values
                    .iter()
                    .zip(native_values)
                    .all(|(audit, native)| {
                        audit.generated_index == native.generated_index
                            && audit.token_id == native.token_id
                            && audit.probabilities.len() == native.probabilities.len()
                            && audit.probabilities.iter().zip(&native.probabilities).all(
                                |(audit, native)| {
                                    audit.stage == native.stage
                                        && audit.probability.to_bits()
                                            == native.probability.to_bits()
                                },
                            )
                    })
        }
        (None, Some(_)) | (Some(_), None) => false,
    };
    if audit.request_id != native.request_id
        || audit.branch_id != native.branch_id
        || audit.input_index != native.input_index
        || audit.displayed_output_blob_id != BlobId::digest(native.text.as_bytes())
        || audit.displayed_output_byte_len != native.text.len()
        || audit.generated_token_ids_blob_id != generated_token_ids_blob_id(&native_tokens)
        || audit.generated_token_count != native_tokens.len()
        || !token_observations_match
        || audit.state != native.state
        || audit.finish_reason != native.finish_reason
        || audit.metrics.prompt_tokens != native.metrics.prompt_tokens
        || audit.metrics.completion_tokens != native.metrics.completion_tokens
        || audit.metrics.shared_prefix_tokens != native.metrics.shared_prefix_tokens
        || audit.metrics.duration_ms != native.metrics.duration_ms
        || audit.metrics.first_token_ms != native.metrics.first_token_ms
        || audit.metrics.tokens_per_second.to_bits() != native.metrics.tokens_per_second.to_bits()
        || audit.metrics.cache.supplied_prefix_tokens != native.metrics.cache.supplied_prefix_tokens
        || audit.metrics.cache.restored_prefix_tokens != native.metrics.cache.restored_prefix_tokens
        || audit.metrics.cache.batch_shared_prefix_tokens
            != native.metrics.cache.batch_shared_prefix_tokens
        || audit.real_engine_invoked != native.real_engine_invoked
        || audit.fake_fixture != native.fake_fixture
        || audit.transport != native.transport
    {
        return Err(case_error(index, "controlled generation copy mismatch"));
    }
    Ok(())
}

fn to_i32_persisted_tokens(
    values: &[u32],
    index: usize,
) -> Result<Vec<i32>, PersistedEvidenceError> {
    values
        .iter()
        .map(|value| i32::try_from(*value).map_err(|_| case_error(index, "token ID exceeds i32")))
        .collect()
}

fn validate_case_audit(
    batch: &PersistedInferenceBatchRef<'_>,
    case: PersistedInferenceCaseRef<'_>,
    audit: &StrictBackendAudit,
    ledger: &StrictEventLedgerRecord,
    sampling: &SamplingConfig,
    expected_shared_prefix_tokens: usize,
) -> Result<CheckedPersistedCaseFacts, PersistedEvidenceError> {
    match audit {
        StrictBackendAudit::Uncontrolled(value) => validate_uncontrolled_case_audit(
            batch,
            case,
            value,
            ledger,
            sampling,
            expected_shared_prefix_tokens,
        ),
        StrictBackendAudit::Controlled(value) => {
            validate_controlled_case_audit(batch, case, value, ledger, sampling)
        }
    }
}

struct ParsedPersistedBatch {
    audits: Vec<StrictBackendAudit>,
    ledgers: Vec<StrictEventLedgerRecord>,
    samplings: Vec<SamplingConfig>,
    request_cases: Vec<RequestCaseCommitment>,
    controlled_request_cases: Vec<ControlledRequestCaseCommitment>,
    controlled: bool,
}

fn validate_batch_header(
    batch: &PersistedInferenceBatchRef<'_>,
) -> Result<(), PersistedEvidenceError> {
    if batch.binding.binding_id.is_empty()
        || batch.binding.binding_id.len() > MAX_RECEIPT_ID_BYTES
        || batch.binding.binding_id.chars().any(char::is_control)
        || batch.binding.model_byte_len == 0
        || batch.binding.context_tokens == 0
    {
        return Err(PersistedEvidenceError::Batch(
            "recompiled binding facts are malformed",
        ));
    }
    if batch.cases.is_empty() || batch.cases.len() > MAX_BASE_WRITER_BATCH_CASES {
        return Err(PersistedEvidenceError::Batch(
            "persisted batch case count is outside its bound",
        ));
    }
    let canonical_request_id = ["loom-base-v1-", "loom-controlled-v1-"]
        .into_iter()
        .find_map(|prefix| batch.backend_request_id.strip_prefix(prefix))
        .is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .as_bytes()
                    .iter()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        });
    if !canonical_request_id {
        return Err(PersistedEvidenceError::Batch(
            "native request ID is not canonical",
        ));
    }
    let total_evidence_bytes = batch.cases.iter().try_fold(0_usize, |total, case| {
        total
            .checked_add(case.event_json.len())?
            .checked_add(case.backend_audit_json.len())
    });
    if total_evidence_bytes.is_none_or(|total| total > MAX_BACKEND_EVIDENCE_BYTES) {
        return Err(PersistedEvidenceError::Batch(
            "aggregate persisted evidence exceeds its bound",
        ));
    }
    validate_prompt(&batch.prompt)
}

fn parse_persisted_batch(
    batch: &PersistedInferenceBatchRef<'_>,
) -> Result<ParsedPersistedBatch, PersistedEvidenceError> {
    let mut call_ids = HashSet::with_capacity(batch.cases.len());
    let mut parsed = ParsedPersistedBatch {
        audits: Vec::with_capacity(batch.cases.len()),
        ledgers: Vec::with_capacity(batch.cases.len()),
        samplings: Vec::with_capacity(batch.cases.len()),
        request_cases: Vec::with_capacity(batch.cases.len()),
        controlled_request_cases: Vec::with_capacity(batch.cases.len()),
        controlled: false,
    };
    let mut mode = None;
    for (expected_index, case) in batch.cases.iter().copied().enumerate() {
        if case.input_index != expected_index || !call_ids.insert(case.model_call.id()) {
            return Err(case_error(
                expected_index,
                "case ordering or call ID uniqueness mismatch",
            ));
        }
        let audit = parse_backend_audit(case.backend_audit_json, expected_index)?;
        if mode.is_some_and(|controlled| controlled != audit.is_controlled()) {
            return Err(case_error(
                expected_index,
                "persisted batch mixes controlled and uncontrolled audits",
            ));
        }
        mode = Some(audit.is_controlled());
        let ledger = parse_json(case.event_json, "event ledger", expected_index)?;
        let sampling = audit.sampling().to_native();
        validate_sampling(&sampling, expected_index)?;
        let sampler_fingerprint = sampling_fingerprint(&sampling);
        let control_program_fingerprint = audit.control_program_fingerprint();
        validate_model_call(
            case,
            &batch.prompt,
            batch.binding,
            batch.runtime_model_fingerprint,
            sampler_fingerprint,
            control_program_fingerprint,
            &sampling,
        )?;
        push_request_case_commitment(
            &mut parsed,
            batch,
            case,
            &audit,
            sampler_fingerprint,
            sampling.seed,
            expected_index,
        )?;
        parsed.audits.push(audit);
        parsed.ledgers.push(ledger);
        parsed.samplings.push(sampling);
    }
    parsed.controlled = mode.unwrap_or(false);
    Ok(parsed)
}

fn push_request_case_commitment(
    parsed: &mut ParsedPersistedBatch,
    batch: &PersistedInferenceBatchRef<'_>,
    case: PersistedInferenceCaseRef<'_>,
    audit: &StrictBackendAudit,
    sampler_fingerprint: BlobId,
    seed: u32,
    index: usize,
) -> Result<(), PersistedEvidenceError> {
    match audit {
        StrictBackendAudit::Uncontrolled(_) => {
            parsed.request_cases.push(RequestCaseCommitment {
                call_id: case.model_call.id(),
                scope: batch.prompt.scope,
                sampler_fingerprint,
                seed,
            });
        }
        StrictBackendAudit::Controlled(value) => {
            if let Some(StrictBackendAudit::Controlled(first)) = parsed.audits.first()
                && !same_controlled_batch_receipt(first, value)
            {
                return Err(case_error(
                    index,
                    "controlled batch-level sealed evidence changed between cases",
                ));
            }
            let prompt = value.unconditional_prompt.as_ref();
            parsed
                .controlled_request_cases
                .push(ControlledRequestCaseCommitment {
                    call_id: case.model_call.id(),
                    scope: batch.prompt.scope,
                    sampler_fingerprint,
                    seed,
                    unconditional_source_prompt_fingerprint: prompt
                        .map(|value| value.source_prompt_fingerprint),
                    unconditional_raw_prompt_blob_id: prompt.map(|value| value.raw_blob_id),
                    unconditional_raw_prompt_byte_len: prompt.map(|value| value.raw_utf8.len()),
                    unconditional_prompt_fingerprint: prompt
                        .map(|value| value.compiled_prompt_fingerprint),
                    unconditional_token_fingerprint: prompt.map(|value| value.token_fingerprint),
                });
        }
    }
    Ok(())
}

fn same_controlled_batch_receipt(
    left: &StrictControlledBackendAuditRecord,
    right: &StrictControlledBackendAuditRecord,
) -> bool {
    left.native_output == right.native_output
        && left.control_program_fingerprint == right.control_program_fingerprint
        && left.control_exact_float_bits_fingerprint == right.control_exact_float_bits_fingerprint
        && left.sealed_request_sha256 == right.sealed_request_sha256
        && left.sealed_output_sha256 == right.sealed_output_sha256
        && left.sealed_event_stream_sha256 == right.sealed_event_stream_sha256
        && left.sealed_runtime_operation_ledger_sha256
            == right.sealed_runtime_operation_ledger_sha256
        && left.sealed_ledger_sha256 == right.sealed_ledger_sha256
        && left.owner_call_sequence == right.owner_call_sequence
        && left.runtime_cost == right.runtime_cost
}

fn validate_request_commitment(
    batch: &PersistedInferenceBatchRef<'_>,
    parsed: &ParsedPersistedBatch,
) -> Result<String, PersistedEvidenceError> {
    let request_id = if parsed.controlled {
        let Some(StrictBackendAudit::Controlled(audit)) = parsed.audits.first() else {
            return Err(PersistedEvidenceError::Batch(
                "controlled batch has no controlled audit",
            ));
        };
        derive_controlled_request_id(&ControlledRequestCommitment {
            project_id: batch.prompt.project_id,
            binding_fingerprint: batch.binding.binding_fingerprint,
            model_fingerprint: batch.runtime_model_fingerprint,
            tokenizer_fingerprint: batch.binding.tokenizer_sha256,
            source_prompt_fingerprint: batch.prompt.source_prompt_fingerprint,
            treatment_recipe_fingerprint: batch.prompt.treatment_recipe_fingerprint,
            raw_prompt_blob_id: batch.prompt.raw_blob_id,
            compiled_prompt_fingerprint: batch.prompt.compiled_fingerprint,
            prompt_token_fingerprint: batch.prompt.token_fingerprint,
            raw_prompt_byte_len: batch.prompt.raw_utf8.len(),
            prompt_token_ids: batch.prompt.ordered_token_ids,
            control_program_fingerprint: audit.control_program_fingerprint,
            control_exact_float_bits_fingerprint: audit.control_exact_float_bits_fingerprint,
            cases: &parsed.controlled_request_cases,
        })
    } else {
        derive_request_id(&RequestCommitment {
            project_id: batch.prompt.project_id,
            binding_fingerprint: batch.binding.binding_fingerprint,
            model_fingerprint: batch.runtime_model_fingerprint,
            source_prompt_fingerprint: batch.prompt.source_prompt_fingerprint,
            treatment_recipe_fingerprint: batch.prompt.treatment_recipe_fingerprint,
            raw_prompt_blob_id: batch.prompt.raw_blob_id,
            compiled_prompt_fingerprint: batch.prompt.compiled_fingerprint,
            prompt_token_fingerprint: batch.prompt.token_fingerprint,
            raw_prompt_byte_len: batch.prompt.raw_utf8.len(),
            prompt_token_ids: batch.prompt.ordered_token_ids,
            cases: &parsed.request_cases,
        })
    };
    if request_id != batch.backend_request_id {
        return Err(PersistedEvidenceError::Batch(
            "native request ID does not match exact ordered request semantics",
        ));
    }
    Ok(request_id)
}

fn replay_parsed_cases(
    batch: &PersistedInferenceBatchRef<'_>,
    parsed: &ParsedPersistedBatch,
) -> Result<Vec<CheckedPersistedCaseFacts>, PersistedEvidenceError> {
    let shared_prefix_tokens = if batch.cases.len() > 1 {
        batch.prompt.ordered_token_ids.len().saturating_sub(1)
    } else {
        0
    };
    batch
        .cases
        .iter()
        .copied()
        .zip(&parsed.audits)
        .zip(&parsed.ledgers)
        .zip(&parsed.samplings)
        .map(|(((case, audit), ledger), sampling)| {
            validate_case_audit(batch, case, audit, ledger, sampling, shared_prefix_tokens)
        })
        .collect()
}

fn finish_checked_batch(
    batch: &PersistedInferenceBatchRef<'_>,
    request_id: String,
    checked_cases: Vec<CheckedPersistedCaseFacts>,
) -> Result<CheckedPersistedBatchFacts, PersistedEvidenceError> {
    let batch_cases = checked_cases
        .iter()
        .map(|case| BatchCaseCommitment {
            call_id: case.call_id,
            completed: case.completed,
            verification_fingerprint: case.verification_fingerprint,
        })
        .collect::<Vec<_>>();
    let verification_fingerprint = derive_batch_verification_fingerprint(&BatchCommitment {
        project_id: batch.prompt.project_id,
        binding_fingerprint: batch.binding.binding_fingerprint,
        source_prompt_fingerprint: batch.prompt.source_prompt_fingerprint,
        compiled_prompt_fingerprint: batch.prompt.compiled_fingerprint,
        request_id: batch.backend_request_id,
        cases: &batch_cases,
    });
    if verification_fingerprint != batch.verification_fingerprint {
        return Err(PersistedEvidenceError::Batch(
            "batch verification fingerprint mismatch",
        ));
    }
    let completed_case_count = checked_cases.iter().filter(|case| case.completed).count();
    Ok(CheckedPersistedBatchFacts {
        request_id,
        runtime_model_fingerprint: batch.runtime_model_fingerprint,
        verification_fingerprint,
        completed_case_count,
        cancelled_case_count: checked_cases.len() - completed_case_count,
        cases: checked_cases,
    })
}

/// Replays a complete persisted native batch without minting any authority.
///
/// Success proves that the supplied persisted fields form the exact closed
/// receipt graph emitted by this crate's native bridge. It does not prove that
/// inference happened, that a source/treatment was authorized, or that any
/// call is eligible for admission, promotion, a treatment, or a benchmark.
pub fn verify_persisted_batch_evidence(
    batch: &PersistedInferenceBatchRef<'_>,
) -> Result<CheckedPersistedBatchFacts, PersistedEvidenceError> {
    validate_batch_header(batch)?;
    let parsed = parse_persisted_batch(batch)?;
    let request_id = validate_request_commitment(batch, &parsed)?;
    let checked_cases = replay_parsed_cases(batch, &parsed)?;
    finish_checked_batch(batch, request_id, checked_cases)
}

#[cfg(test)]
mod tests {
    use loom_research_types::{
        BoundedText, CallIdentity, CampaignId, CompletedCall, StageAttemptId, StageId, TrialCaseId,
    };

    use super::*;

    fn blob(label: &[u8]) -> BlobId {
        BlobId::digest(label)
    }

    fn fixed_scope() -> CallScope {
        CallScope::new(
            CampaignId::from_str("01ARZ3NDEKTSV4RRFFQ69G5FAA").expect("campaign ID"),
            StageId::from_str("01ARZ3NDEKTSV4RRFFQ69G5FAB").expect("stage ID"),
            StageAttemptId::from_str("01ARZ3NDEKTSV4RRFFQ69G5FAC").expect("attempt ID"),
            TrialCaseId::from_str("01ARZ3NDEKTSV4RRFFQ69G5FAD").expect("case ID"),
        )
    }

    fn alternate_project_id() -> ProjectId {
        ProjectId::from_str("01ARZ3NDEKTSV4RRFFQ69G5FB0").expect("alternate project ID")
    }

    fn alternate_call_id() -> ModelCallId {
        ModelCallId::from_str("01ARZ3NDEKTSV4RRFFQ69G5FB1").expect("alternate call ID")
    }

    fn alternate_scope() -> CallScope {
        CallScope::new(
            CampaignId::from_str("01ARZ3NDEKTSV4RRFFQ69G5FB2").expect("campaign ID"),
            StageId::from_str("01ARZ3NDEKTSV4RRFFQ69G5FB3").expect("stage ID"),
            StageAttemptId::from_str("01ARZ3NDEKTSV4RRFFQ69G5FB4").expect("attempt ID"),
            TrialCaseId::from_str("01ARZ3NDEKTSV4RRFFQ69G5FB5").expect("case ID"),
        )
    }

    #[derive(Clone)]
    struct OwnedRequestCommitment {
        project_id: ProjectId,
        binding_fingerprint: BlobId,
        model_fingerprint: BlobId,
        source_prompt_fingerprint: BlobId,
        treatment_recipe_fingerprint: BlobId,
        raw_prompt_blob_id: BlobId,
        compiled_prompt_fingerprint: BlobId,
        prompt_token_fingerprint: BlobId,
        raw_prompt_byte_len: usize,
        prompt_token_ids: Vec<u32>,
        cases: Vec<RequestCaseCommitment>,
    }

    impl OwnedRequestCommitment {
        fn request_id(&self) -> String {
            derive_request_id(&RequestCommitment {
                project_id: self.project_id,
                binding_fingerprint: self.binding_fingerprint,
                model_fingerprint: self.model_fingerprint,
                source_prompt_fingerprint: self.source_prompt_fingerprint,
                treatment_recipe_fingerprint: self.treatment_recipe_fingerprint,
                raw_prompt_blob_id: self.raw_prompt_blob_id,
                compiled_prompt_fingerprint: self.compiled_prompt_fingerprint,
                prompt_token_fingerprint: self.prompt_token_fingerprint,
                raw_prompt_byte_len: self.raw_prompt_byte_len,
                prompt_token_ids: &self.prompt_token_ids,
                cases: &self.cases,
            })
        }
    }

    #[derive(Clone)]
    struct OwnedCallCommitment {
        project_id: ProjectId,
        request_id: String,
        call_id: ModelCallId,
        scope: CallScope,
        model_fingerprint: BlobId,
        source_prompt_fingerprint: BlobId,
        treatment_recipe_fingerprint: BlobId,
        raw_prompt_blob_id: BlobId,
        compiled_prompt_fingerprint: BlobId,
        prompt_token_fingerprint: BlobId,
        sampler_fingerprint: BlobId,
        control_program_fingerprint: BlobId,
        raw_output: Vec<u8>,
        generated_token_ids: Vec<u32>,
        event_blob_id: BlobId,
        backend_receipt_blob_id: BlobId,
        terminal_sampled_token_id: Option<i32>,
    }

    impl OwnedCallCommitment {
        fn fingerprint(&self) -> BlobId {
            derive_call_verification_fingerprint(&CallVerificationCommitment {
                project_id: self.project_id,
                request_id: &self.request_id,
                call_id: self.call_id,
                scope: self.scope,
                model_fingerprint: self.model_fingerprint,
                source_prompt_fingerprint: self.source_prompt_fingerprint,
                treatment_recipe_fingerprint: self.treatment_recipe_fingerprint,
                raw_prompt_blob_id: self.raw_prompt_blob_id,
                compiled_prompt_fingerprint: self.compiled_prompt_fingerprint,
                prompt_token_fingerprint: self.prompt_token_fingerprint,
                sampler_fingerprint: self.sampler_fingerprint,
                control_program_fingerprint: self.control_program_fingerprint,
                raw_output: &self.raw_output,
                generated_token_ids: &self.generated_token_ids,
                event_blob_id: self.event_blob_id,
                backend_receipt_blob_id: self.backend_receipt_blob_id,
                terminal_sampled_token_id: self.terminal_sampled_token_id,
            })
        }
    }

    #[derive(Clone)]
    struct OwnedBatchCommitment {
        project_id: ProjectId,
        binding_fingerprint: BlobId,
        source_prompt_fingerprint: BlobId,
        compiled_prompt_fingerprint: BlobId,
        request_id: String,
        cases: Vec<BatchCaseCommitment>,
    }

    impl OwnedBatchCommitment {
        fn fingerprint(&self) -> BlobId {
            derive_batch_verification_fingerprint(&BatchCommitment {
                project_id: self.project_id,
                binding_fingerprint: self.binding_fingerprint,
                source_prompt_fingerprint: self.source_prompt_fingerprint,
                compiled_prompt_fingerprint: self.compiled_prompt_fingerprint,
                request_id: &self.request_id,
                cases: &self.cases,
            })
        }
    }

    type RequestMutation = (&'static str, fn(&mut OwnedRequestCommitment));
    type CallMutation = (&'static str, fn(&mut OwnedCallCommitment));
    type BatchMutation = (&'static str, fn(&mut OwnedBatchCommitment));
    type SamplingMutation = (&'static str, fn(&mut SamplingConfig));

    #[test]
    fn canonical_commitment_reference_vectors_are_fixed() {
        let project_id = ProjectId::from_str("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("project ID");
        let call_id = ModelCallId::from_str("01ARZ3NDEKTSV4RRFFQ69G5FAE").expect("model-call ID");
        let scope = fixed_scope();
        let request_cases = [RequestCaseCommitment {
            call_id,
            scope,
            sampler_fingerprint: blob(b"sampler"),
            seed: 17,
        }];
        let prompt_tokens = [1_u32, 2, 65_537];
        let request_id = derive_request_id(&RequestCommitment {
            project_id,
            binding_fingerprint: blob(b"binding"),
            model_fingerprint: blob(b"model"),
            source_prompt_fingerprint: blob(b"source"),
            treatment_recipe_fingerprint: blob(b"treatment"),
            raw_prompt_blob_id: blob(b"raw prompt"),
            compiled_prompt_fingerprint: blob(b"compiled"),
            prompt_token_fingerprint: blob(b"prompt tokens"),
            raw_prompt_byte_len: 10,
            prompt_token_ids: &prompt_tokens,
            cases: &request_cases,
        });
        assert_eq!(
            request_id,
            "loom-base-v1-f127f2601e6e8aa09dc413f67f76c7cf1ecb05542c968150905c8f1307fc046e"
        );

        let call_fingerprint = derive_call_verification_fingerprint(&CallVerificationCommitment {
            project_id,
            request_id: &request_id,
            call_id,
            scope,
            model_fingerprint: blob(b"model"),
            source_prompt_fingerprint: blob(b"source"),
            treatment_recipe_fingerprint: blob(b"treatment"),
            raw_prompt_blob_id: blob(b"raw prompt"),
            compiled_prompt_fingerprint: blob(b"compiled"),
            prompt_token_fingerprint: blob(b"prompt tokens"),
            sampler_fingerprint: blob(b"sampler"),
            control_program_fingerprint: uncontrolled_program_fingerprint(),
            raw_output: b"output",
            generated_token_ids: &[7, 8, 9],
            event_blob_id: blob(b"events"),
            backend_receipt_blob_id: blob(b"receipt"),
            terminal_sampled_token_id: Some(2),
        });
        assert_eq!(
            call_fingerprint.to_hex(),
            "4d5e4b5044dddd276cea204d062b7960730559673eacff513c9e85fbe4b05591"
        );

        let cases = [BatchCaseCommitment {
            call_id,
            completed: true,
            verification_fingerprint: call_fingerprint,
        }];
        let batch_fingerprint = derive_batch_verification_fingerprint(&BatchCommitment {
            project_id,
            binding_fingerprint: blob(b"binding"),
            source_prompt_fingerprint: blob(b"source"),
            compiled_prompt_fingerprint: blob(b"compiled"),
            request_id: &request_id,
            cases: &cases,
        });
        assert_eq!(
            batch_fingerprint.to_hex(),
            "9e9e4ee8584133e39a914b6e83ae21b9a9ea2019e866195f6a7f3445a6e817e6"
        );
    }

    #[test]
    fn every_request_commitment_field_changes_the_request_id() {
        let call_id = ModelCallId::from_str("01ARZ3NDEKTSV4RRFFQ69G5FAE").expect("model-call ID");
        let base = OwnedRequestCommitment {
            project_id: ProjectId::from_str("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("project ID"),
            binding_fingerprint: blob(b"binding"),
            model_fingerprint: blob(b"model"),
            source_prompt_fingerprint: blob(b"source"),
            treatment_recipe_fingerprint: blob(b"treatment"),
            raw_prompt_blob_id: blob(b"raw prompt"),
            compiled_prompt_fingerprint: blob(b"compiled"),
            prompt_token_fingerprint: blob(b"prompt tokens"),
            raw_prompt_byte_len: 10,
            prompt_token_ids: vec![1, 2, 3],
            cases: vec![
                RequestCaseCommitment {
                    call_id,
                    scope: fixed_scope(),
                    sampler_fingerprint: blob(b"sampler one"),
                    seed: 17,
                },
                RequestCaseCommitment {
                    call_id: alternate_call_id(),
                    scope: alternate_scope(),
                    sampler_fingerprint: blob(b"sampler two"),
                    seed: 18,
                },
            ],
        };
        let baseline = base.request_id();
        let mutations: &[RequestMutation] = &[
            ("project", |value| value.project_id = alternate_project_id()),
            ("binding", |value| {
                value.binding_fingerprint = blob(b"binding changed");
            }),
            ("model", |value| {
                value.model_fingerprint = blob(b"model changed");
            }),
            ("source", |value| {
                value.source_prompt_fingerprint = blob(b"source changed");
            }),
            ("treatment", |value| {
                value.treatment_recipe_fingerprint = blob(b"treatment changed");
            }),
            ("raw prompt blob", |value| {
                value.raw_prompt_blob_id = blob(b"raw changed");
            }),
            ("compiled prompt", |value| {
                value.compiled_prompt_fingerprint = blob(b"compiled changed");
            }),
            ("prompt tokens fingerprint", |value| {
                value.prompt_token_fingerprint = blob(b"tokens changed");
            }),
            ("prompt byte length", |value| value.raw_prompt_byte_len += 1),
            ("prompt token IDs", |value| value.prompt_token_ids[0] += 1),
            ("case count", |value| {
                value.cases.pop();
            }),
            ("case order", |value| value.cases.swap(0, 1)),
            ("case call ID", |value| {
                value.cases[0].call_id = alternate_call_id();
            }),
            ("case scope", |value| {
                value.cases[0].scope = alternate_scope();
            }),
            ("case sampler", |value| {
                value.cases[0].sampler_fingerprint = blob(b"sampler changed");
            }),
            ("case seed", |value| value.cases[0].seed += 1),
        ];
        for (field, mutate) in mutations {
            let mut changed = base.clone();
            mutate(&mut changed);
            assert_ne!(
                baseline,
                changed.request_id(),
                "unbound request field: {field}"
            );
        }
    }

    #[test]
    fn every_call_commitment_field_changes_the_call_fingerprint() {
        let base = OwnedCallCommitment {
            project_id: ProjectId::from_str("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("project ID"),
            request_id: "loom-base-v1-request".to_owned(),
            call_id: ModelCallId::from_str("01ARZ3NDEKTSV4RRFFQ69G5FAE").expect("model-call ID"),
            scope: fixed_scope(),
            model_fingerprint: blob(b"model"),
            source_prompt_fingerprint: blob(b"source"),
            treatment_recipe_fingerprint: blob(b"treatment"),
            raw_prompt_blob_id: blob(b"raw prompt"),
            compiled_prompt_fingerprint: blob(b"compiled"),
            prompt_token_fingerprint: blob(b"prompt tokens"),
            sampler_fingerprint: blob(b"sampler"),
            control_program_fingerprint: blob(b"control"),
            raw_output: b"output".to_vec(),
            generated_token_ids: vec![7, 8, 9],
            event_blob_id: blob(b"events"),
            backend_receipt_blob_id: blob(b"receipt"),
            terminal_sampled_token_id: Some(2),
        };
        let baseline = base.fingerprint();
        let mutations: &[CallMutation] = &[
            ("project", |value| value.project_id = alternate_project_id()),
            ("request", |value| value.request_id.push('x')),
            ("call ID", |value| value.call_id = alternate_call_id()),
            ("scope", |value| value.scope = alternate_scope()),
            ("model", |value| {
                value.model_fingerprint = blob(b"model changed");
            }),
            ("source", |value| {
                value.source_prompt_fingerprint = blob(b"source changed");
            }),
            ("treatment", |value| {
                value.treatment_recipe_fingerprint = blob(b"treatment changed");
            }),
            ("raw prompt", |value| {
                value.raw_prompt_blob_id = blob(b"raw changed");
            }),
            ("compiled prompt", |value| {
                value.compiled_prompt_fingerprint = blob(b"compiled changed");
            }),
            ("prompt tokens", |value| {
                value.prompt_token_fingerprint = blob(b"tokens changed");
            }),
            ("sampler", |value| {
                value.sampler_fingerprint = blob(b"sampler changed");
            }),
            ("control", |value| {
                value.control_program_fingerprint = blob(b"control changed");
            }),
            ("raw output", |value| value.raw_output.push(b'x')),
            ("generated token count", |value| {
                value.generated_token_ids.pop();
            }),
            ("generated token value", |value| {
                value.generated_token_ids[0] += 1;
            }),
            ("event blob", |value| {
                value.event_blob_id = blob(b"events changed");
            }),
            ("receipt blob", |value| {
                value.backend_receipt_blob_id = blob(b"receipt changed");
            }),
            ("terminal presence", |value| {
                value.terminal_sampled_token_id = None;
            }),
            ("terminal value", |value| {
                value.terminal_sampled_token_id = Some(3);
            }),
        ];
        for (field, mutate) in mutations {
            let mut changed = base.clone();
            mutate(&mut changed);
            assert_ne!(
                baseline,
                changed.fingerprint(),
                "unbound call field: {field}"
            );
        }
    }

    #[test]
    fn every_batch_commitment_field_and_sibling_order_changes_the_fingerprint() {
        let first =
            ModelCallId::from_str("01ARZ3NDEKTSV4RRFFQ69G5FAE").expect("first model-call ID");
        let second = alternate_call_id();
        let base = OwnedBatchCommitment {
            project_id: ProjectId::from_str("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("project ID"),
            binding_fingerprint: blob(b"binding"),
            source_prompt_fingerprint: blob(b"source"),
            compiled_prompt_fingerprint: blob(b"compiled"),
            request_id: "loom-base-v1-request".to_owned(),
            cases: vec![
                BatchCaseCommitment {
                    call_id: first,
                    completed: true,
                    verification_fingerprint: blob(b"first"),
                },
                BatchCaseCommitment {
                    call_id: second,
                    completed: false,
                    verification_fingerprint: blob(b"second"),
                },
            ],
        };
        let baseline = base.fingerprint();
        let mutations: &[BatchMutation] = &[
            ("project", |value| value.project_id = alternate_project_id()),
            ("binding", |value| {
                value.binding_fingerprint = blob(b"binding changed");
            }),
            ("source", |value| {
                value.source_prompt_fingerprint = blob(b"source changed");
            }),
            ("compiled prompt", |value| {
                value.compiled_prompt_fingerprint = blob(b"compiled changed");
            }),
            ("request", |value| value.request_id.push('x')),
            ("case count", |value| {
                value.cases.pop();
            }),
            ("sibling order", |value| value.cases.swap(0, 1)),
            ("case ID", |value| {
                value.cases[0].call_id = alternate_call_id();
            }),
            ("completion class", |value| value.cases[0].completed = false),
            ("case fingerprint", |value| {
                value.cases[0].verification_fingerprint = blob(b"changed");
            }),
        ];
        for (field, mutate) in mutations {
            let mut changed = base.clone();
            mutate(&mut changed);
            assert_ne!(
                baseline,
                changed.fingerprint(),
                "unbound batch field: {field}"
            );
        }
    }

    fn valid_sampling() -> SamplingConfig {
        SamplingConfig {
            seed: 7,
            max_tokens: 1,
            ..SamplingConfig::default()
        }
    }

    fn test_identity() -> CallIdentity {
        CallIdentity::new(
            fixed_scope(),
            blob(b"model"),
            blob(b"tokenizer"),
            blob(b"prompt"),
            sampling_fingerprint(&valid_sampling()),
            uncontrolled_program_fingerprint(),
            7,
        )
    }

    fn completed_test_call(
        raw_output: &[u8],
        token_ids: &[u32],
        event_blob_id: BlobId,
        receipt_blob_id: BlobId,
    ) -> ModelCall {
        ModelCall::new(
            ModelCallId::from_str("01ARZ3NDEKTSV4RRFFQ69G5FAE").expect("model-call ID"),
            test_identity(),
            CallEvidenceClass::LiveBaseWriterClaim,
            CallTerminal::Completed(
                CompletedCall::new(raw_output, token_ids, event_blob_id, Some(receipt_blob_id))
                    .expect("completed test evidence"),
            ),
        )
        .expect("completed test call")
    }

    #[test]
    fn sampling_replay_rejects_each_ambiguous_or_out_of_range_family() {
        let base = valid_sampling();
        validate_sampling(&base, 0).expect("baseline sampling is valid");
        let mutations: &[SamplingMutation] = &[
            ("random seed sentinel", |value| value.seed = u32::MAX),
            ("non-finite temperature", |value| {
                value.temperature = f32::NAN;
            }),
            ("negative temperature", |value| value.temperature = -0.1),
            ("zero dynamic exponent", |value| {
                value.dynamic_temperature_exponent = 0.0;
            }),
            ("negative top-k", |value| value.top_k = -1),
            ("top-p above one", |value| value.top_p = 1.1),
            ("repeat window below sentinel", |value| {
                value.repeat_last_n = -2;
            }),
            ("zero completion budget", |value| value.max_tokens = 0),
            ("duplicate sampler", |value| {
                value.sampler_order.push(SamplerKind::TopK);
            }),
            ("empty stop", |value| value.stop.push(String::new())),
            ("too many stops", |value| {
                value.stop = (0..=MAX_STOP_SEQUENCES)
                    .map(|index| format!("s{index}"))
                    .collect();
            }),
            ("too many stop bytes", |value| {
                value.stop = vec!["x".repeat(MAX_STOP_SEQUENCE_BYTES + 1)];
            }),
        ];
        for (field, mutate) in mutations {
            let mut changed = base.clone();
            mutate(&mut changed);
            assert!(
                validate_sampling(&changed, 0).is_err(),
                "invalid sampling accepted: {field}"
            );
        }
    }

    #[test]
    fn token_observation_replay_rejects_cardinality_identity_stage_and_probability_errors() {
        let tokens = [10_u32, 11];
        let valid = vec![StrictTokenObservation {
            generated_index: 0,
            token_id: 10,
            probabilities: vec![
                StrictTokenProbabilityObservation {
                    stage: ProbabilityStage::RawModel,
                    probability: 0.25,
                },
                StrictTokenProbabilityObservation {
                    stage: ProbabilityStage::PostSampler,
                    probability: 0.5,
                },
            ],
        }];
        validate_token_observations(Some(&valid), &tokens, 0).expect("valid observation");

        let mut duplicate_index = valid.clone();
        duplicate_index.push(valid[0].clone());
        assert!(validate_token_observations(Some(&duplicate_index), &tokens, 0).is_err());
        let mut outside = valid.clone();
        outside[0].generated_index = tokens.len();
        assert!(validate_token_observations(Some(&outside), &tokens, 0).is_err());
        let mut wrong_token = valid.clone();
        wrong_token[0].token_id = 12;
        assert!(validate_token_observations(Some(&wrong_token), &tokens, 0).is_err());
        let mut duplicate_stage = valid.clone();
        let repeated_stage = duplicate_stage[0].probabilities[0].clone();
        duplicate_stage[0].probabilities.push(repeated_stage);
        assert!(validate_token_observations(Some(&duplicate_stage), &tokens, 0).is_err());
        let mut nonfinite = valid.clone();
        nonfinite[0].probabilities[0].probability = f32::NAN;
        assert!(validate_token_observations(Some(&nonfinite), &tokens, 0).is_err());
        let mut above_one = valid;
        above_one[0].probabilities[0].probability = 1.1;
        assert!(validate_token_observations(Some(&above_one), &tokens, 0).is_err());
    }

    fn valid_test_event_ledger(call_id: ModelCallId) -> StrictEventLedgerRecord {
        StrictEventLedgerRecord {
            format: EVENT_LEDGER_FORMAT.to_owned(),
            call_id,
            native_case_id: format!("call-{call_id}"),
            input_index: 0,
            events: vec![
                StrictCompactGenerationEvent {
                    event_index: 0,
                    event: StrictGenerationEventKind::State {
                        state: GenerationState::Prefilling,
                    },
                },
                StrictCompactGenerationEvent {
                    event_index: 1,
                    event: StrictGenerationEventKind::State {
                        state: GenerationState::Generating,
                    },
                },
                StrictCompactGenerationEvent {
                    event_index: 2,
                    event: StrictGenerationEventKind::Delta {
                        text: "hi".to_owned(),
                    },
                },
                StrictCompactGenerationEvent {
                    event_index: 3,
                    event: StrictGenerationEventKind::State {
                        state: GenerationState::Completed,
                    },
                },
            ],
        }
    }

    #[test]
    fn event_replay_rejects_sequence_transition_terminal_and_raw_reconstruction_errors() {
        let raw = b"hi";
        let tokens = [10_u32];
        let event_blob_id = blob(b"events");
        let receipt_blob_id = blob(b"receipt");
        let call = completed_test_call(raw, &tokens, event_blob_id, receipt_blob_id);
        let projection = OutputProjection::new(raw, 2, 2).expect("projection");
        let case = PersistedInferenceCaseRef {
            input_index: 0,
            model_call: &call,
            raw_output: raw,
            generated_token_ids: &tokens,
            event_json: b"{}",
            backend_audit_json: b"{}",
            terminal_sampled_token_id: None,
            outcome: PersistedCaseOutcomeRef::Completed {
                displayed_output: raw,
                output_projection: Some(&projection),
            },
            verification_fingerprint: blob(b"verification"),
        };
        let native_case_id = format!("call-{}", call.id());
        let valid = valid_test_event_ledger(call.id());
        validate_event_ledger(&valid, case, &native_case_id, GenerationState::Completed)
            .expect("valid event ledger");

        let mut bad_index = valid.clone();
        bad_index.events[2].event_index = 3;
        assert!(
            validate_event_ledger(
                &bad_index,
                case,
                &native_case_id,
                GenerationState::Completed
            )
            .is_err()
        );
        let mut wrong_raw = valid.clone();
        wrong_raw.events[2].event = StrictGenerationEventKind::Delta {
            text: "no".to_owned(),
        };
        assert!(
            validate_event_ledger(
                &wrong_raw,
                case,
                &native_case_id,
                GenerationState::Completed
            )
            .is_err()
        );
        let mut missing_terminal = valid.clone();
        missing_terminal.events.pop();
        assert!(
            validate_event_ledger(
                &missing_terminal,
                case,
                &native_case_id,
                GenerationState::Completed,
            )
            .is_err()
        );
        let mut warning = valid;
        warning.events[2].event = StrictGenerationEventKind::Warning {
            code: "warning".to_owned(),
            message: "untrusted".to_owned(),
        };
        assert!(
            validate_event_ledger(&warning, case, &native_case_id, GenerationState::Completed)
                .is_err()
        );
    }

    fn valid_test_output(
        request_id: &str,
        native_case_id: &str,
        raw: &[u8],
        tokens: &[u32],
    ) -> StrictGenerationOutput {
        StrictGenerationOutput {
            format: OUTPUT_AUDIT_FORMAT.to_owned(),
            request_id: request_id.to_owned(),
            branch_id: native_case_id.to_owned(),
            input_index: 0,
            displayed_output_blob_id: BlobId::digest(raw),
            displayed_output_byte_len: raw.len(),
            raw_output_blob_id: BlobId::digest(raw),
            raw_output_byte_len: raw.len(),
            generated_token_ids_blob_id: generated_token_ids_blob_id(tokens),
            generated_token_count: tokens.len(),
            token_observations: None,
            state: GenerationState::Completed,
            finish_reason: "max_tokens".to_owned(),
            metrics: StrictGenerationMetrics {
                prompt_tokens: 3,
                completion_tokens: tokens.len(),
                shared_prefix_tokens: 0,
                duration_ms: 2,
                first_token_ms: Some(1),
                tokens_per_second: 1.0,
                cache: StrictCacheMetrics {
                    supplied_prefix_tokens: 0,
                    restored_prefix_tokens: 0,
                    batch_shared_prefix_tokens: 0,
                },
            },
            real_engine_invoked: true,
            fake_fixture: false,
            transport: NativeTransport::InProcess,
        }
    }

    fn validate_test_output(
        case: PersistedInferenceCaseRef<'_>,
        output: &StrictGenerationOutput,
        sampling: &SamplingConfig,
        event_blob_id: BlobId,
        receipt_blob_id: BlobId,
    ) -> Result<(bool, BlobId), PersistedEvidenceError> {
        validate_output_and_terminal(
            case,
            output,
            &OutputValidationContext {
                sampling,
                request_id: "loom-base-v1-test",
                native_case_id: "call-01ARZ3NDEKTSV4RRFFQ69G5FAE",
                prompt_tokens: 3,
                shared_prefix_tokens: 0,
                event_blob_id,
                backend_receipt_blob_id: receipt_blob_id,
            },
        )
    }

    #[test]
    fn output_replay_rejects_metrics_transport_terminal_and_projection_substitution() {
        let raw = b"hi";
        let tokens = [10_u32];
        let request_id = "loom-base-v1-test";
        let native_case_id = "call-01ARZ3NDEKTSV4RRFFQ69G5FAE";
        let event_blob_id = blob(b"events");
        let receipt_blob_id = blob(b"receipt");
        let call = completed_test_call(raw, &tokens, event_blob_id, receipt_blob_id);
        let projection = OutputProjection::new(raw, 2, 2).expect("projection");
        let case = PersistedInferenceCaseRef {
            input_index: 0,
            model_call: &call,
            raw_output: raw,
            generated_token_ids: &tokens,
            event_json: b"{}",
            backend_audit_json: b"{}",
            terminal_sampled_token_id: None,
            outcome: PersistedCaseOutcomeRef::Completed {
                displayed_output: raw,
                output_projection: Some(&projection),
            },
            verification_fingerprint: blob(b"verification"),
        };
        let sampling = valid_sampling();
        let output = valid_test_output(request_id, native_case_id, raw, &tokens);
        validate_test_output(case, &output, &sampling, event_blob_id, receipt_blob_id)
            .expect("valid output");

        let mut fake = output.clone();
        fake.fake_fixture = true;
        assert!(
            validate_test_output(case, &fake, &sampling, event_blob_id, receipt_blob_id).is_err()
        );
        let mut nonfinite = output.clone();
        nonfinite.metrics.tokens_per_second = f64::NAN;
        assert!(
            validate_test_output(case, &nonfinite, &sampling, event_blob_id, receipt_blob_id)
                .is_err()
        );
        let mut bad_cache = output.clone();
        bad_cache.metrics.cache.batch_shared_prefix_tokens = 1;
        assert!(
            validate_test_output(case, &bad_cache, &sampling, event_blob_id, receipt_blob_id)
                .is_err()
        );
        let mut bad_terminal = output.clone();
        bad_terminal.finish_reason = "end_of_generation".to_owned();
        assert!(
            validate_test_output(
                case,
                &bad_terminal,
                &sampling,
                event_blob_id,
                receipt_blob_id,
            )
            .is_err()
        );
        let short_projection = OutputProjection::new(raw, 1, 1).expect("short projection");
        let bad_projection_case = PersistedInferenceCaseRef {
            outcome: PersistedCaseOutcomeRef::Completed {
                displayed_output: raw,
                output_projection: Some(&short_projection),
            },
            ..case
        };
        assert!(
            validate_test_output(
                bad_projection_case,
                &output,
                &sampling,
                event_blob_id,
                receipt_blob_id,
            )
            .is_err()
        );

        let cancelled_call = ModelCall::new(
            call.id(),
            test_identity(),
            CallEvidenceClass::LiveBaseWriterClaim,
            CallTerminal::Cancelled {
                message: BoundedText::new(CANCELLED_MESSAGE).expect("cancel message"),
            },
        )
        .expect("cancelled call");
        let cancelled_case = PersistedInferenceCaseRef {
            model_call: &cancelled_call,
            outcome: PersistedCaseOutcomeRef::Cancelled,
            ..case
        };
        let mut cancelled_output = output;
        cancelled_output.state = GenerationState::Cancelled;
        cancelled_output.finish_reason = "cancelled".to_owned();
        validate_test_output(
            cancelled_case,
            &cancelled_output,
            &sampling,
            event_blob_id,
            receipt_blob_id,
        )
        .expect("valid cancelled output");
    }
}
