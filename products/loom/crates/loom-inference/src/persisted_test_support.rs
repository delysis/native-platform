//! Synthetic persisted evidence for downstream tests.
//!
//! These values are deliberately nonauthorizing. They prove that storage and
//! replay code preserve the closed receipt graph, but they do not prove that
//! inference occurred. This module cannot construct a live inference outcome,
//! envelope, admission lease, generated span, assembly, or promotion authority.

use std::{fmt, str::FromStr};

use llama_native_types::{GenerationState, ModelFingerprint, NativeTransport, SamplingConfig};
use loom_research_types::{
    CallEvidenceClass, CallIdentity, CallScope, CallTerminal, CampaignId, CompletedCall,
    MAX_BASE_WRITER_BATCH_CASES, MAX_GENERATED_TOKENS, ModelCall, ModelCallId, OutputProjection,
    StageAttemptId, StageId, TerminalMessage, TrialCaseId,
};
use loom_types::{BlobId, ProjectId};
use serde::Serialize;

use crate::{PromptFormEvidence, PromptTokenPolicyEvidence};

use super::{
    BACKEND_AUDIT_FORMAT, BatchCaseCommitment, BatchCommitment, CANCELLED_MESSAGE,
    CallVerificationCommitment, CheckedPersistedBatchFacts, EVENT_LEDGER_FORMAT,
    MAX_RECEIPT_ID_BYTES, OUTPUT_AUDIT_FORMAT, PersistedBindingEvidenceRef,
    PersistedCaseOutcomeRef, PersistedEvidenceError, PersistedInferenceBatchRef,
    PersistedInferenceCaseRef, PersistedPromptEvidenceRef, RequestCaseCommitment,
    RequestCommitment, SANITIZED_MODEL_FINGERPRINT_FORMAT, StrictBackendAuditRecord,
    StrictCacheMetrics, StrictCompactGenerationEvent, StrictEventLedgerRecord,
    StrictGenerationEventKind, StrictGenerationMetrics, StrictGenerationOutput,
    StrictModelFingerprint, StrictSamplingConfig, compiled_prompt_fingerprint,
    derive_batch_verification_fingerprint, derive_call_verification_fingerprint, derive_request_id,
    generated_token_ids_blob_id, model_fingerprint_id, prompt_token_fingerprint,
    sampling_fingerprint, uncontrolled_program_fingerprint, validate_prompt, validate_sampling,
    verify_persisted_batch_evidence,
};

const TEST_BINDING_ID: &str = "nonauthorizing-test-binding";

/// Owned recompiled-binding facts for a synthetic, nonauthorizing test vector.
pub struct NonauthorizingPersistedBindingTestVector {
    pub binding_id: String,
    pub binding_fingerprint: BlobId,
    pub model_sha256: BlobId,
    pub model_byte_len: u64,
    pub tokenizer_sha256: BlobId,
    pub multimodal_projector_sha256: Option<BlobId>,
    pub context_tokens: u32,
}

impl NonauthorizingPersistedBindingTestVector {
    pub fn evidence_ref(&self) -> PersistedBindingEvidenceRef<'_> {
        PersistedBindingEvidenceRef {
            binding_id: &self.binding_id,
            binding_fingerprint: self.binding_fingerprint,
            model_sha256: self.model_sha256,
            model_byte_len: self.model_byte_len,
            tokenizer_sha256: self.tokenizer_sha256,
            multimodal_projector_sha256: self.multimodal_projector_sha256,
            context_tokens: self.context_tokens,
        }
    }
}

impl fmt::Debug for NonauthorizingPersistedBindingTestVector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.evidence_ref().fmt(formatter)
    }
}

/// Owned exact prompt facts for a synthetic, nonauthorizing test vector.
pub struct NonauthorizingPersistedPromptTestVector {
    pub project_id: ProjectId,
    pub scope: CallScope,
    pub source_prompt_fingerprint: BlobId,
    pub content_fingerprint: BlobId,
    pub treatment_recipe_fingerprint: BlobId,
    pub raw_utf8: Vec<u8>,
    pub raw_blob_id: BlobId,
    pub form: PromptFormEvidence,
    pub token_policy: PromptTokenPolicyEvidence,
    pub ordered_token_ids: Vec<u32>,
    pub token_fingerprint: BlobId,
    pub compiled_fingerprint: BlobId,
}

impl NonauthorizingPersistedPromptTestVector {
    pub fn evidence_ref(&self) -> PersistedPromptEvidenceRef<'_> {
        PersistedPromptEvidenceRef {
            project_id: self.project_id,
            scope: self.scope,
            source_prompt_fingerprint: self.source_prompt_fingerprint,
            content_fingerprint: self.content_fingerprint,
            treatment_recipe_fingerprint: self.treatment_recipe_fingerprint,
            raw_utf8: &self.raw_utf8,
            raw_blob_id: self.raw_blob_id,
            form: self.form,
            token_policy: self.token_policy,
            ordered_token_ids: &self.ordered_token_ids,
            token_fingerprint: self.token_fingerprint,
            compiled_fingerprint: self.compiled_fingerprint,
        }
    }
}

impl fmt::Debug for NonauthorizingPersistedPromptTestVector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.evidence_ref().fmt(formatter)
    }
}

/// Owned terminal projection for a synthetic, nonauthorizing persisted case.
pub enum NonauthorizingPersistedCaseOutcomeTestVector {
    Completed {
        displayed_output: Vec<u8>,
        output_projection: Option<OutputProjection>,
    },
    Cancelled,
}

impl NonauthorizingPersistedCaseOutcomeTestVector {
    fn evidence_ref(&self) -> PersistedCaseOutcomeRef<'_> {
        match self {
            Self::Completed {
                displayed_output,
                output_projection,
            } => PersistedCaseOutcomeRef::Completed {
                displayed_output,
                output_projection: output_projection.as_ref(),
            },
            Self::Cancelled => PersistedCaseOutcomeRef::Cancelled,
        }
    }
}

impl fmt::Debug for NonauthorizingPersistedCaseOutcomeTestVector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.evidence_ref().fmt(formatter)
    }
}

/// One owned synthetic persisted case.
///
/// Its `LiveBaseWriterClaim` is intentionally only a serializable claim. The
/// vector exposes no native seal and cannot be promoted to live authority.
pub struct NonauthorizingPersistedCaseTestVector {
    pub input_index: usize,
    pub model_call: ModelCall,
    pub raw_output: Vec<u8>,
    pub generated_token_ids: Vec<u32>,
    pub event_json: Vec<u8>,
    pub backend_audit_json: Vec<u8>,
    pub terminal_sampled_token_id: Option<i32>,
    pub outcome: NonauthorizingPersistedCaseOutcomeTestVector,
    pub verification_fingerprint: BlobId,
}

impl NonauthorizingPersistedCaseTestVector {
    pub fn evidence_ref(&self) -> PersistedInferenceCaseRef<'_> {
        PersistedInferenceCaseRef {
            input_index: self.input_index,
            model_call: &self.model_call,
            raw_output: &self.raw_output,
            generated_token_ids: &self.generated_token_ids,
            event_json: &self.event_json,
            backend_audit_json: &self.backend_audit_json,
            terminal_sampled_token_id: self.terminal_sampled_token_id,
            outcome: self.outcome.evidence_ref(),
            verification_fingerprint: self.verification_fingerprint,
        }
    }
}

impl fmt::Debug for NonauthorizingPersistedCaseTestVector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.evidence_ref().fmt(formatter)
    }
}

/// Complete owned synthetic persisted graph for storage and replay tests.
///
/// This type is only available with `loom-inference/test-support`. It is not a
/// producer receipt and is never evidence that a model ran. Strict replay of
/// the vector returns cloneable checked facts, not any admission authority.
pub struct NonauthorizingPersistedEvidenceTestVector {
    pub binding: NonauthorizingPersistedBindingTestVector,
    pub prompt: NonauthorizingPersistedPromptTestVector,
    pub runtime_model_fingerprint: BlobId,
    pub backend_request_id: String,
    pub cases: Vec<NonauthorizingPersistedCaseTestVector>,
    pub verification_fingerprint: BlobId,
}

impl NonauthorizingPersistedEvidenceTestVector {
    pub fn case_refs(&self) -> Vec<PersistedInferenceCaseRef<'_>> {
        self.cases
            .iter()
            .map(NonauthorizingPersistedCaseTestVector::evidence_ref)
            .collect()
    }

    pub fn batch_ref<'a>(
        &'a self,
        cases: &'a [PersistedInferenceCaseRef<'a>],
    ) -> PersistedInferenceBatchRef<'a> {
        PersistedInferenceBatchRef {
            binding: self.binding.evidence_ref(),
            prompt: self.prompt.evidence_ref(),
            runtime_model_fingerprint: self.runtime_model_fingerprint,
            backend_request_id: &self.backend_request_id,
            cases,
            verification_fingerprint: self.verification_fingerprint,
        }
    }

    /// Strictly replays this synthetic graph into nonauthorizing checked facts.
    pub fn replay(&self) -> Result<CheckedPersistedBatchFacts, PersistedEvidenceError> {
        let cases = self.case_refs();
        verify_persisted_batch_evidence(&self.batch_ref(&cases))
    }
}

impl fmt::Debug for NonauthorizingPersistedEvidenceTestVector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NonauthorizingPersistedEvidenceTestVector")
            .field("binding", &self.binding)
            .field("prompt", &self.prompt)
            .field("runtime_model_fingerprint", &self.runtime_model_fingerprint)
            .field("backend_request_id", &self.backend_request_id)
            .field("case_count", &self.cases.len())
            .field("verification_fingerprint", &self.verification_fingerprint)
            .finish()
    }
}

fn fixed_id<T>(value: &str, field: &'static str) -> Result<T, PersistedEvidenceError>
where
    T: FromStr,
{
    T::from_str(value).map_err(|_| PersistedEvidenceError::Batch(field))
}

fn encode_json<T: Serialize>(
    value: &T,
    kind: &'static str,
) -> Result<Vec<u8>, PersistedEvidenceError> {
    serde_json::to_vec(value).map_err(|source| PersistedEvidenceError::Json { kind, source })
}

fn strict_sampling(sampling: &SamplingConfig) -> StrictSamplingConfig {
    StrictSamplingConfig {
        seed: sampling.seed,
        temperature: sampling.temperature,
        dynamic_temperature_range: sampling.dynamic_temperature_range,
        dynamic_temperature_exponent: sampling.dynamic_temperature_exponent,
        top_k: sampling.top_k,
        top_p: sampling.top_p,
        min_p: sampling.min_p,
        typical_p: sampling.typical_p,
        xtc_probability: sampling.xtc_probability,
        xtc_threshold: sampling.xtc_threshold,
        repeat_last_n: sampling.repeat_last_n,
        repeat_penalty: sampling.repeat_penalty,
        frequency_penalty: sampling.frequency_penalty,
        presence_penalty: sampling.presence_penalty,
        dry_multiplier: sampling.dry_multiplier,
        dry_base: sampling.dry_base,
        dry_allowed_length: sampling.dry_allowed_length,
        dry_penalty_last_n: sampling.dry_penalty_last_n,
        sampler_order: sampling.sampler_order.clone(),
        max_tokens: sampling.max_tokens,
        stop: sampling.stop.clone(),
    }
}

fn strict_model(model: &ModelFingerprint) -> StrictModelFingerprint {
    StrictModelFingerprint {
        format: SANITIZED_MODEL_FINGERPRINT_FORMAT.to_owned(),
        model_size: model.model_size,
        model_sha256: model.model_sha256.clone(),
        tokenizer_sha256: model.tokenizer_sha256.clone(),
        chat_template_sha256: model.chat_template_sha256.clone(),
        multimodal_projector_sha256: model.multimodal_projector_sha256.clone(),
        binding_version: model.binding_version.clone(),
        build_id: model.build_id.clone(),
        backend: model.backend.clone(),
        context_tokens: model.context_tokens,
        batch_tokens: model.batch_tokens,
        max_sequences: model.max_sequences,
        rope_config_sha256: model.rope_config_sha256.clone(),
        kv_layout_sha256: model.kv_layout_sha256.clone(),
    }
}

/// Requested terminal shape for one synthetic, nonauthorizing persisted case.
///
/// The builder binds every supplied field into the same request, receipt, call,
/// and batch commitments used by native production evidence. These specs never
/// enter the native engine and cannot mint inference or admission authority.
#[derive(Clone)]
pub enum NonauthorizingPersistedCaseTestSpec {
    Completed {
        call_id: ModelCallId,
        sampling: SamplingConfig,
        raw_output: Vec<u8>,
        generated_token_ids: Vec<u32>,
    },
    Cancelled {
        call_id: ModelCallId,
        sampling: SamplingConfig,
        partial_raw_output: Vec<u8>,
        generated_token_ids: Vec<u32>,
    },
}

impl NonauthorizingPersistedCaseTestSpec {
    /// Creates a completed case without exposing native sampler types.
    pub fn completed(
        call_id: ModelCallId,
        seed: u32,
        raw_output: &[u8],
        generated_token_ids: &[u32],
    ) -> Result<Self, PersistedEvidenceError> {
        Ok(Self::Completed {
            call_id,
            sampling: simple_sampling(seed, generated_token_ids.len())?,
            raw_output: raw_output.to_vec(),
            generated_token_ids: generated_token_ids.to_vec(),
        })
    }

    /// Creates a cancelled case without exposing native sampler types.
    pub fn cancelled(
        call_id: ModelCallId,
        seed: u32,
        partial_raw_output: &[u8],
        generated_token_ids: &[u32],
    ) -> Result<Self, PersistedEvidenceError> {
        Ok(Self::Cancelled {
            call_id,
            sampling: simple_sampling(seed, generated_token_ids.len())?,
            partial_raw_output: partial_raw_output.to_vec(),
            generated_token_ids: generated_token_ids.to_vec(),
        })
    }

    fn call_id(&self) -> ModelCallId {
        match self {
            Self::Completed { call_id, .. } | Self::Cancelled { call_id, .. } => *call_id,
        }
    }

    fn sampling(&self) -> &SamplingConfig {
        match self {
            Self::Completed { sampling, .. } | Self::Cancelled { sampling, .. } => sampling,
        }
    }

    fn raw_output(&self) -> &[u8] {
        match self {
            Self::Completed { raw_output, .. } => raw_output,
            Self::Cancelled {
                partial_raw_output, ..
            } => partial_raw_output,
        }
    }

    fn generated_token_ids(&self) -> &[u32] {
        match self {
            Self::Completed {
                generated_token_ids,
                ..
            }
            | Self::Cancelled {
                generated_token_ids,
                ..
            } => generated_token_ids,
        }
    }

    fn is_completed(&self) -> bool {
        matches!(self, Self::Completed { .. })
    }
}

fn simple_sampling(
    seed: u32,
    generated_token_count: usize,
) -> Result<SamplingConfig, PersistedEvidenceError> {
    let generated_token_count = u32::try_from(generated_token_count)
        .map_err(|_| PersistedEvidenceError::Batch("test token count exceeds u32"))?;
    if seed == u32::MAX || generated_token_count > MAX_GENERATED_TOKENS {
        return Err(PersistedEvidenceError::Batch(
            "test seed or generated token count is outside its bound",
        ));
    }
    Ok(SamplingConfig {
        seed,
        max_tokens: generated_token_count.max(1),
        ..SamplingConfig::default()
    })
}

impl fmt::Debug for NonauthorizingPersistedCaseTestSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct(match self {
                Self::Completed { .. } => "CompletedNonauthorizingPersistedCaseTestSpec",
                Self::Cancelled { .. } => "CancelledNonauthorizingPersistedCaseTestSpec",
            })
            .field("call_id", &self.call_id())
            .field("sampling", self.sampling())
            .field("raw_output_bytes", &self.raw_output().len())
            .field("generated_token_count", &self.generated_token_ids().len())
            .finish()
    }
}

struct SyntheticInputs {
    project_id: ProjectId,
    scope: CallScope,
    binding: NonauthorizingPersistedBindingTestVector,
    model: ModelFingerprint,
    runtime_model_fingerprint: BlobId,
    prompt: NonauthorizingPersistedPromptTestVector,
    case_specs: Vec<NonauthorizingPersistedCaseTestSpec>,
    backend_request_id: String,
}

fn synthetic_model(
    binding: &NonauthorizingPersistedBindingTestVector,
    max_sequences: usize,
) -> Result<ModelFingerprint, PersistedEvidenceError> {
    let max_sequences = u32::try_from(max_sequences)
        .map_err(|_| PersistedEvidenceError::Batch("test case count exceeds u32"))?;
    Ok(ModelFingerprint {
        model_id: "nonauthorizing-test-vector".to_owned(),
        model_size: binding.model_byte_len,
        model_sha256: binding.model_sha256.to_hex(),
        tokenizer_sha256: binding.tokenizer_sha256.to_hex(),
        chat_template_sha256: BlobId::digest(b"nonauthorizing chat template").to_hex(),
        multimodal_projector_sha256: binding.multimodal_projector_sha256.map(BlobId::to_hex),
        binding_version: "nonauthorizing-test-vector-v1".to_owned(),
        build_id: "nonauthorizing-test-build".to_owned(),
        backend: "synthetic-test-only".to_owned(),
        context_tokens: binding.context_tokens,
        batch_tokens: binding.context_tokens.min(64),
        max_sequences,
        rope_config_sha256: BlobId::digest(b"nonauthorizing rope config").to_hex(),
        kv_layout_sha256: BlobId::digest(b"nonauthorizing kv layout").to_hex(),
    })
}

fn synthetic_prompt(
    project_id: ProjectId,
    scope: CallScope,
) -> NonauthorizingPersistedPromptTestVector {
    let raw_utf8 = b"Once upon a persisted test vector".to_vec();
    let ordered_token_ids = vec![1_u32, 2, 3];
    let mut prompt = NonauthorizingPersistedPromptTestVector {
        project_id,
        scope,
        source_prompt_fingerprint: BlobId::digest(b"nonauthorizing source prompt"),
        content_fingerprint: BlobId::digest(b"nonauthorizing prompt content"),
        treatment_recipe_fingerprint: BlobId::digest(b"nonauthorizing treatment recipe"),
        raw_blob_id: BlobId::digest(&raw_utf8),
        raw_utf8,
        form: PromptFormEvidence::Completion,
        token_policy: PromptTokenPolicyEvidence::NoBosParseSpecial,
        token_fingerprint: prompt_token_fingerprint(&ordered_token_ids),
        ordered_token_ids,
        compiled_fingerprint: BlobId::digest(b"filled below"),
    };
    prompt.compiled_fingerprint = compiled_prompt_fingerprint(&prompt.evidence_ref());
    prompt
}

fn default_binding() -> NonauthorizingPersistedBindingTestVector {
    NonauthorizingPersistedBindingTestVector {
        binding_id: TEST_BINDING_ID.to_owned(),
        binding_fingerprint: BlobId::digest(b"nonauthorizing synthetic binding"),
        model_sha256: BlobId::digest(b"nonauthorizing synthetic model bytes"),
        model_byte_len: 1_048_576,
        tokenizer_sha256: BlobId::digest(b"nonauthorizing synthetic tokenizer"),
        multimodal_projector_sha256: None,
        context_tokens: 128,
    }
}

fn default_case_specs() -> Result<Vec<NonauthorizingPersistedCaseTestSpec>, PersistedEvidenceError>
{
    Ok(vec![NonauthorizingPersistedCaseTestSpec::completed(
        fixed_id::<ModelCallId>("01ARZ3NDEKTSV4RRFFQ69G5FAE", "invalid test model-call ID")?,
        7,
        b" continuation",
        &[10_u32],
    )?])
}

fn synthetic_inputs_for(
    binding: NonauthorizingPersistedBindingTestVector,
    prompt: NonauthorizingPersistedPromptTestVector,
    case_specs: Vec<NonauthorizingPersistedCaseTestSpec>,
) -> Result<SyntheticInputs, PersistedEvidenceError> {
    if binding.binding_id.is_empty()
        || binding.binding_id.len() > MAX_RECEIPT_ID_BYTES
        || binding.binding_id.chars().any(char::is_control)
        || binding.model_byte_len == 0
        || binding.context_tokens == 0
    {
        return Err(PersistedEvidenceError::Batch(
            "test binding facts are malformed",
        ));
    }
    if case_specs.is_empty() || case_specs.len() > MAX_BASE_WRITER_BATCH_CASES {
        return Err(PersistedEvidenceError::Batch(
            "test case count is outside its bound",
        ));
    }
    validate_prompt(&prompt.evidence_ref())?;
    for (index, spec) in case_specs.iter().enumerate() {
        validate_sampling(spec.sampling(), index)?;
    }
    let project_id = prompt.project_id;
    let scope = prompt.scope;
    let model = synthetic_model(&binding, case_specs.len())?;
    let runtime_model_fingerprint = model_fingerprint_id(&model);
    let request_cases = case_specs
        .iter()
        .map(|spec| RequestCaseCommitment {
            call_id: spec.call_id(),
            scope,
            sampler_fingerprint: sampling_fingerprint(spec.sampling()),
            seed: spec.sampling().seed,
        })
        .collect::<Vec<_>>();
    let backend_request_id = derive_request_id(&RequestCommitment {
        project_id,
        binding_fingerprint: binding.binding_fingerprint,
        model_fingerprint: runtime_model_fingerprint,
        source_prompt_fingerprint: prompt.source_prompt_fingerprint,
        treatment_recipe_fingerprint: prompt.treatment_recipe_fingerprint,
        raw_prompt_blob_id: prompt.raw_blob_id,
        compiled_prompt_fingerprint: prompt.compiled_fingerprint,
        prompt_token_fingerprint: prompt.token_fingerprint,
        raw_prompt_byte_len: prompt.raw_utf8.len(),
        prompt_token_ids: &prompt.ordered_token_ids,
        cases: &request_cases,
    });

    Ok(SyntheticInputs {
        project_id,
        scope,
        binding,
        model,
        runtime_model_fingerprint,
        prompt,
        case_specs,
        backend_request_id,
    })
}

fn synthetic_inputs() -> Result<SyntheticInputs, PersistedEvidenceError> {
    let project_id = fixed_id("01ARZ3NDEKTSV4RRFFQ69G5FAV", "invalid test project ID")?;
    let scope = CallScope::new(
        fixed_id::<CampaignId>("01ARZ3NDEKTSV4RRFFQ69G5FAA", "invalid test campaign ID")?,
        fixed_id::<StageId>("01ARZ3NDEKTSV4RRFFQ69G5FAB", "invalid test stage ID")?,
        fixed_id::<StageAttemptId>("01ARZ3NDEKTSV4RRFFQ69G5FAC", "invalid test attempt ID")?,
        fixed_id::<TrialCaseId>("01ARZ3NDEKTSV4RRFFQ69G5FAD", "invalid test case ID")?,
    );
    synthetic_inputs_for(
        default_binding(),
        synthetic_prompt(project_id, scope),
        default_case_specs()?,
    )
}

fn native_case_id(spec: &NonauthorizingPersistedCaseTestSpec) -> String {
    format!("call-{}", spec.call_id())
}

fn terminal_state(spec: &NonauthorizingPersistedCaseTestSpec) -> GenerationState {
    if spec.is_completed() {
        GenerationState::Completed
    } else {
        GenerationState::Cancelled
    }
}

fn terminal_sampled_token_id(spec: &NonauthorizingPersistedCaseTestSpec) -> Option<i32> {
    match spec {
        NonauthorizingPersistedCaseTestSpec::Completed {
            sampling,
            generated_token_ids,
            ..
        } if generated_token_ids.len() < sampling.max_tokens as usize => Some(0),
        NonauthorizingPersistedCaseTestSpec::Completed { .. }
        | NonauthorizingPersistedCaseTestSpec::Cancelled { .. } => None,
    }
}

fn synthetic_event_json(
    index: usize,
    spec: &NonauthorizingPersistedCaseTestSpec,
) -> Result<Vec<u8>, PersistedEvidenceError> {
    let mut events = vec![
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
    ];
    if !spec.raw_output().is_empty() {
        let delta = std::str::from_utf8(spec.raw_output())
            .map_err(|_| PersistedEvidenceError::Batch("test output is not UTF-8"))?;
        events.push(StrictCompactGenerationEvent {
            event_index: events.len() as u64,
            event: StrictGenerationEventKind::Delta {
                text: delta.to_owned(),
            },
        });
    }
    events.push(StrictCompactGenerationEvent {
        event_index: events.len() as u64,
        event: StrictGenerationEventKind::State {
            state: terminal_state(spec),
        },
    });
    encode_json(
        &StrictEventLedgerRecord {
            format: EVENT_LEDGER_FORMAT.to_owned(),
            call_id: spec.call_id(),
            native_case_id: native_case_id(spec),
            input_index: index,
            events,
        },
        "nonauthorizing test event ledger",
    )
}

fn synthetic_output(
    inputs: &SyntheticInputs,
    index: usize,
    spec: &NonauthorizingPersistedCaseTestSpec,
) -> StrictGenerationOutput {
    let completed = spec.is_completed();
    let finish_reason = if completed {
        if terminal_sampled_token_id(spec).is_some() {
            "end_of_generation"
        } else {
            "max_tokens"
        }
    } else {
        "cancelled"
    };
    let shared_prefix_tokens = if inputs.case_specs.len() > 1 {
        inputs.prompt.ordered_token_ids.len().saturating_sub(1)
    } else {
        0
    };
    StrictGenerationOutput {
        format: OUTPUT_AUDIT_FORMAT.to_owned(),
        request_id: inputs.backend_request_id.clone(),
        branch_id: native_case_id(spec),
        input_index: index,
        displayed_output_blob_id: BlobId::digest(spec.raw_output()),
        displayed_output_byte_len: spec.raw_output().len(),
        raw_output_blob_id: BlobId::digest(spec.raw_output()),
        raw_output_byte_len: spec.raw_output().len(),
        generated_token_ids_blob_id: generated_token_ids_blob_id(spec.generated_token_ids()),
        generated_token_count: spec.generated_token_ids().len(),
        token_observations: None,
        state: terminal_state(spec),
        finish_reason: finish_reason.to_owned(),
        metrics: StrictGenerationMetrics {
            prompt_tokens: inputs.prompt.ordered_token_ids.len(),
            completion_tokens: spec.generated_token_ids().len(),
            shared_prefix_tokens,
            duration_ms: 1,
            first_token_ms: (!spec.generated_token_ids().is_empty()).then_some(1),
            tokens_per_second: f64::from(!spec.generated_token_ids().is_empty()),
            cache: StrictCacheMetrics {
                supplied_prefix_tokens: 0,
                restored_prefix_tokens: 0,
                batch_shared_prefix_tokens: shared_prefix_tokens,
            },
        },
        real_engine_invoked: true,
        fake_fixture: false,
        transport: NativeTransport::InProcess,
    }
}

fn synthetic_audit_json(
    inputs: &SyntheticInputs,
    index: usize,
    spec: &NonauthorizingPersistedCaseTestSpec,
    event_blob_id: BlobId,
) -> Result<Vec<u8>, PersistedEvidenceError> {
    encode_json(
        &StrictBackendAuditRecord {
            format: BACKEND_AUDIT_FORMAT.to_owned(),
            project_id: inputs.project_id,
            binding_id: inputs.binding.binding_id.clone(),
            call_id: spec.call_id(),
            scope: inputs.scope,
            exact_prompt_blob_id: inputs.prompt.raw_blob_id,
            source_prompt_fingerprint: inputs.prompt.source_prompt_fingerprint,
            prompt_content_fingerprint: inputs.prompt.content_fingerprint,
            treatment_recipe_fingerprint: inputs.prompt.treatment_recipe_fingerprint,
            compiled_prompt_fingerprint: inputs.prompt.compiled_fingerprint,
            prompt_token_fingerprint: inputs.prompt.token_fingerprint,
            native_request_id: inputs.backend_request_id.clone(),
            native_case_id: native_case_id(spec),
            input_index: index,
            sampler_fingerprint: sampling_fingerprint(spec.sampling()),
            sampling: strict_sampling(spec.sampling()),
            model_fingerprint: strict_model(&inputs.model),
            output: synthetic_output(inputs, index, spec),
            terminal_sampled_token_id: terminal_sampled_token_id(spec),
            event_stream_blob_id: event_blob_id,
        },
        "nonauthorizing test backend audit",
    )
}

fn synthetic_model_call(
    inputs: &SyntheticInputs,
    spec: &NonauthorizingPersistedCaseTestSpec,
    event_blob_id: BlobId,
    backend_receipt_blob_id: BlobId,
) -> Result<ModelCall, PersistedEvidenceError> {
    let terminal = match spec {
        NonauthorizingPersistedCaseTestSpec::Completed { .. } => {
            CallTerminal::Completed(CompletedCall::new(
                spec.raw_output(),
                spec.generated_token_ids(),
                event_blob_id,
                Some(backend_receipt_blob_id),
            )?)
        }
        NonauthorizingPersistedCaseTestSpec::Cancelled { .. } => CallTerminal::Cancelled {
            message: TerminalMessage::new(CANCELLED_MESSAGE)
                .map_err(|_| PersistedEvidenceError::Batch("cancelled test message is invalid"))?,
        },
    };
    Ok(ModelCall::new(
        spec.call_id(),
        CallIdentity::new(
            inputs.scope,
            inputs.runtime_model_fingerprint,
            inputs.binding.tokenizer_sha256,
            inputs.prompt.compiled_fingerprint,
            sampling_fingerprint(spec.sampling()),
            uncontrolled_program_fingerprint(),
            u64::from(spec.sampling().seed),
        ),
        CallEvidenceClass::LiveBaseWriterClaim,
        terminal,
    )?)
}

fn synthetic_case_fingerprint(
    inputs: &SyntheticInputs,
    spec: &NonauthorizingPersistedCaseTestSpec,
    event_blob_id: BlobId,
    backend_receipt_blob_id: BlobId,
) -> BlobId {
    derive_call_verification_fingerprint(&CallVerificationCommitment {
        project_id: inputs.project_id,
        request_id: &inputs.backend_request_id,
        call_id: spec.call_id(),
        scope: inputs.scope,
        model_fingerprint: inputs.runtime_model_fingerprint,
        source_prompt_fingerprint: inputs.prompt.source_prompt_fingerprint,
        treatment_recipe_fingerprint: inputs.prompt.treatment_recipe_fingerprint,
        raw_prompt_blob_id: inputs.prompt.raw_blob_id,
        compiled_prompt_fingerprint: inputs.prompt.compiled_fingerprint,
        prompt_token_fingerprint: inputs.prompt.token_fingerprint,
        sampler_fingerprint: sampling_fingerprint(spec.sampling()),
        control_program_fingerprint: uncontrolled_program_fingerprint(),
        raw_output: spec.raw_output(),
        generated_token_ids: spec.generated_token_ids(),
        event_blob_id,
        backend_receipt_blob_id,
        terminal_sampled_token_id: terminal_sampled_token_id(spec),
    })
}

fn synthetic_batch_fingerprint(
    inputs: &SyntheticInputs,
    cases: &[NonauthorizingPersistedCaseTestVector],
) -> BlobId {
    let batch_cases = cases
        .iter()
        .map(|case| BatchCaseCommitment {
            call_id: case.model_call.id(),
            completed: matches!(
                case.outcome,
                NonauthorizingPersistedCaseOutcomeTestVector::Completed { .. }
            ),
            verification_fingerprint: case.verification_fingerprint,
        })
        .collect::<Vec<_>>();
    derive_batch_verification_fingerprint(&BatchCommitment {
        project_id: inputs.project_id,
        binding_fingerprint: inputs.binding.binding_fingerprint,
        source_prompt_fingerprint: inputs.prompt.source_prompt_fingerprint,
        compiled_prompt_fingerprint: inputs.prompt.compiled_fingerprint,
        request_id: &inputs.backend_request_id,
        cases: &batch_cases,
    })
}

fn synthetic_case(
    inputs: &SyntheticInputs,
    index: usize,
    spec: &NonauthorizingPersistedCaseTestSpec,
) -> Result<NonauthorizingPersistedCaseTestVector, PersistedEvidenceError> {
    let event_json = synthetic_event_json(index, spec)?;
    let event_blob_id = BlobId::digest(&event_json);
    let backend_audit_json = synthetic_audit_json(inputs, index, spec, event_blob_id)?;
    let backend_receipt_blob_id = BlobId::digest(&backend_audit_json);
    let model_call = synthetic_model_call(inputs, spec, event_blob_id, backend_receipt_blob_id)?;
    let verification_fingerprint =
        synthetic_case_fingerprint(inputs, spec, event_blob_id, backend_receipt_blob_id);
    let outcome = if spec.is_completed() {
        let output_projection = if spec.raw_output().is_empty() {
            None
        } else {
            Some(OutputProjection::new(
                spec.raw_output(),
                spec.raw_output().len() as u64,
                spec.raw_output().len() as u64,
            )?)
        };
        NonauthorizingPersistedCaseOutcomeTestVector::Completed {
            displayed_output: spec.raw_output().to_vec(),
            output_projection,
        }
    } else {
        NonauthorizingPersistedCaseOutcomeTestVector::Cancelled
    };
    Ok(NonauthorizingPersistedCaseTestVector {
        input_index: index,
        model_call,
        raw_output: spec.raw_output().to_vec(),
        generated_token_ids: spec.generated_token_ids().to_vec(),
        event_json,
        backend_audit_json,
        terminal_sampled_token_id: terminal_sampled_token_id(spec),
        outcome,
        verification_fingerprint,
    })
}

fn finish_synthetic_vector(
    inputs: SyntheticInputs,
) -> Result<NonauthorizingPersistedEvidenceTestVector, PersistedEvidenceError> {
    let cases = inputs
        .case_specs
        .iter()
        .enumerate()
        .map(|(index, spec)| synthetic_case(&inputs, index, spec))
        .collect::<Result<Vec<_>, _>>()?;
    let verification_fingerprint = synthetic_batch_fingerprint(&inputs, &cases);
    let vector = NonauthorizingPersistedEvidenceTestVector {
        binding: inputs.binding,
        prompt: inputs.prompt,
        runtime_model_fingerprint: inputs.runtime_model_fingerprint,
        backend_request_id: inputs.backend_request_id,
        cases,
        verification_fingerprint,
    };
    let _ = vector.replay()?;
    Ok(vector)
}

/// Builds a stable, synthetic persisted graph for downstream tests.
///
/// The audit bytes deliberately satisfy the receipt schema's live-claim fields
/// so adversarial storage tests can demonstrate that replay alone cannot mint
/// authority. No inference backend, native owner seal, verified outcome, store
/// lease, span admission, assembly, or promotion is constructed here.
pub fn nonauthorizing_persisted_evidence_test_vector()
-> Result<NonauthorizingPersistedEvidenceTestVector, PersistedEvidenceError> {
    finish_synthetic_vector(synthetic_inputs()?)
}

fn own_binding(
    binding: PersistedBindingEvidenceRef<'_>,
) -> NonauthorizingPersistedBindingTestVector {
    NonauthorizingPersistedBindingTestVector {
        binding_id: binding.binding_id.to_owned(),
        binding_fingerprint: binding.binding_fingerprint,
        model_sha256: binding.model_sha256,
        model_byte_len: binding.model_byte_len,
        tokenizer_sha256: binding.tokenizer_sha256,
        multimodal_projector_sha256: binding.multimodal_projector_sha256,
        context_tokens: binding.context_tokens,
    }
}

fn own_prompt(prompt: &PersistedPromptEvidenceRef<'_>) -> NonauthorizingPersistedPromptTestVector {
    NonauthorizingPersistedPromptTestVector {
        project_id: prompt.project_id,
        scope: prompt.scope,
        source_prompt_fingerprint: prompt.source_prompt_fingerprint,
        content_fingerprint: prompt.content_fingerprint,
        treatment_recipe_fingerprint: prompt.treatment_recipe_fingerprint,
        raw_utf8: prompt.raw_utf8.to_vec(),
        raw_blob_id: prompt.raw_blob_id,
        form: prompt.form,
        token_policy: prompt.token_policy,
        ordered_token_ids: prompt.ordered_token_ids.to_vec(),
        token_fingerprint: prompt.token_fingerprint,
        compiled_fingerprint: prompt.compiled_fingerprint,
    }
}

/// Builds one completed synthetic receipt around caller-supplied exact facts.
///
/// Binding and prompt fields are copied byte-for-byte. The resulting graph can
/// test persistence against a real frozen source/prompt graph, but it remains
/// synthetic, nonauthorizing, and unavailable without `test-support`.
pub fn nonauthorizing_persisted_evidence_test_vector_for(
    binding: PersistedBindingEvidenceRef<'_>,
    prompt: PersistedPromptEvidenceRef<'_>,
) -> Result<NonauthorizingPersistedEvidenceTestVector, PersistedEvidenceError> {
    finish_synthetic_vector(synthetic_inputs_for(
        own_binding(binding),
        own_prompt(&prompt),
        default_case_specs()?,
    )?)
}

/// Builds ordered completed/cancelled synthetic receipts around exact facts.
///
/// The case slice is bounded by the production batch limit, participates in
/// exact sibling-order commitments, and may represent completed, mixed, or
/// all-cancelled batches. Strict replay occurs before this function returns.
/// No native seal or inference/admission authority is constructed.
pub fn nonauthorizing_persisted_evidence_test_vector_for_cases(
    binding: PersistedBindingEvidenceRef<'_>,
    prompt: PersistedPromptEvidenceRef<'_>,
    cases: &[NonauthorizingPersistedCaseTestSpec],
) -> Result<NonauthorizingPersistedEvidenceTestVector, PersistedEvidenceError> {
    finish_synthetic_vector(synthetic_inputs_for(
        own_binding(binding),
        own_prompt(&prompt),
        cases.to_vec(),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_nonauthorizing_vector_strictly_replays() {
        let vector = nonauthorizing_persisted_evidence_test_vector().expect("test vector builds");
        let checked = vector.replay().expect("test vector strictly replays");
        assert_eq!(checked.request_id(), vector.backend_request_id);
        assert_eq!(checked.completed_case_count(), 1);
        assert_eq!(checked.cancelled_case_count(), 0);
        assert_eq!(checked.cases().len(), 1);
        assert_eq!(
            checked.cases()[0].call_id(),
            vector.cases[0].model_call.id()
        );
        assert_eq!(
            vector.backend_request_id,
            "loom-base-v1-19b3be97c3a0176819776a5b39915de1e3567cbdf67f9eb7bf00215bdd64bcc5"
        );
        assert_eq!(
            vector.cases[0].verification_fingerprint.to_hex(),
            "0391f1e1a277e742a9568762323f4c91be3c200b0505faedd0a5414341b144b8"
        );
        assert_eq!(
            vector.verification_fingerprint.to_hex(),
            "e07d7946f76b81ad79c01ef2e570888cc51b360b1f53c7840a94a49d376a5ecf"
        );
        assert_eq!(
            checked.cases()[0].event_blob_id().to_hex(),
            "60fe01b6ac43cad4aea0087e67c8bbdd3bb3a4af6bd530b9b7a27c1b21c0ea34"
        );
        assert_eq!(
            checked.cases()[0].backend_receipt_blob_id().to_hex(),
            "4799c591669e00cffed03ee7d7070d54a2da3ec92980d68b870cdfe084a123a1"
        );
    }

    #[test]
    fn parameterized_vector_preserves_exact_binding_and_prompt_facts() {
        let source = nonauthorizing_persisted_evidence_test_vector().expect("source vector");
        let rebuilt = nonauthorizing_persisted_evidence_test_vector_for(
            source.binding.evidence_ref(),
            source.prompt.evidence_ref(),
        )
        .expect("parameterized vector");
        assert_eq!(rebuilt.binding.binding_id, source.binding.binding_id);
        assert_eq!(
            rebuilt.binding.binding_fingerprint,
            source.binding.binding_fingerprint
        );
        assert_eq!(rebuilt.prompt.raw_utf8, source.prompt.raw_utf8);
        assert_eq!(
            rebuilt.prompt.source_prompt_fingerprint,
            source.prompt.source_prompt_fingerprint
        );
        assert_eq!(
            rebuilt.prompt.content_fingerprint,
            source.prompt.content_fingerprint
        );
        assert_eq!(
            rebuilt.prompt.compiled_fingerprint,
            source.prompt.compiled_fingerprint
        );
        assert_eq!(rebuilt.backend_request_id, source.backend_request_id);
        assert_eq!(
            rebuilt.verification_fingerprint,
            source.verification_fingerprint
        );
    }

    #[test]
    fn replay_rejects_prompt_content_substitution() {
        let mut vector =
            nonauthorizing_persisted_evidence_test_vector().expect("test vector builds");
        vector.prompt.content_fingerprint = BlobId::digest(b"substituted prompt content");
        assert!(
            vector.replay().is_err(),
            "prompt content must remain bound to the exact backend audit"
        );
    }

    #[test]
    fn parameterized_vector_strictly_replays_mixed_and_all_cancelled_batches() {
        let source = nonauthorizing_persisted_evidence_test_vector().expect("source vector");
        let complete_id = fixed_id::<ModelCallId>(
            "01ARZ3NDEKTSV4RRFFQ69G5FB1",
            "invalid mixed completed call ID",
        )
        .expect("completed call ID");
        let cancelled_id = fixed_id::<ModelCallId>(
            "01ARZ3NDEKTSV4RRFFQ69G5FB2",
            "invalid mixed cancelled call ID",
        )
        .expect("cancelled call ID");
        let mixed_specs = [
            NonauthorizingPersistedCaseTestSpec::completed(complete_id, 11, b" completed", &[11])
                .expect("completed test spec"),
            NonauthorizingPersistedCaseTestSpec::cancelled(cancelled_id, 12, b" partial", &[12])
                .expect("cancelled test spec"),
        ];
        let mixed = nonauthorizing_persisted_evidence_test_vector_for_cases(
            source.binding.evidence_ref(),
            source.prompt.evidence_ref(),
            &mixed_specs,
        )
        .expect("mixed vector");
        let checked = mixed.replay().expect("mixed vector replays");
        assert_eq!(checked.completed_case_count(), 1);
        assert_eq!(checked.cancelled_case_count(), 1);
        assert_eq!(checked.cases()[0].call_id(), complete_id);
        assert_eq!(checked.cases()[1].call_id(), cancelled_id);

        let all_cancelled = nonauthorizing_persisted_evidence_test_vector_for_cases(
            source.binding.evidence_ref(),
            source.prompt.evidence_ref(),
            &mixed_specs[1..],
        )
        .expect("all-cancelled vector");
        let checked = all_cancelled
            .replay()
            .expect("all-cancelled vector replays");
        assert_eq!(checked.completed_case_count(), 0);
        assert_eq!(checked.cancelled_case_count(), 1);
    }

    #[test]
    fn parameterized_vector_fails_closed_on_empty_or_duplicate_case_sets() {
        let source = nonauthorizing_persisted_evidence_test_vector().expect("source vector");
        assert!(
            nonauthorizing_persisted_evidence_test_vector_for_cases(
                source.binding.evidence_ref(),
                source.prompt.evidence_ref(),
                &[],
            )
            .is_err()
        );
        let mut duplicate = default_case_specs().expect("default case spec");
        duplicate.push(duplicate[0].clone());
        assert!(
            nonauthorizing_persisted_evidence_test_vector_for_cases(
                source.binding.evidence_ref(),
                source.prompt.evidence_ref(),
                &duplicate,
            )
            .is_err()
        );
    }
}
