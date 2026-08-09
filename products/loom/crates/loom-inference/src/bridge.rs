use std::{collections::HashSet, fmt, str::FromStr};

use llama_native_engine::{GenerationTicket, NativeModelHandle, VerifiedGenerationBatch};
use llama_native_types::{
    CompletionPrompt, GenerationBatchRequest, GenerationCase, GenerationEvent, GenerationEventKind,
    GenerationInput, GenerationMetrics, GenerationOutput, GenerationState, MAX_STOP_SEQUENCE_BYTES,
    MAX_STOP_SEQUENCES, ModelFingerprint, NativeError, NativeTransport, PreparedPrompt, PromptForm,
    PromptTokenPolicy, SamplingConfig, SpecialTokenPolicy, TokenObservation,
};
use loom_research_types::{
    BoundError, BoundedText, CallError, CallEvidenceClass, CallIdentity, CallScope, CallTerminal,
    CompiledBaseCompletionPrompt, CompletedCall, MAX_BACKEND_EVIDENCE_BYTES,
    MAX_BASE_WRITER_BATCH_CASES, MAX_GENERATED_TOKENS, ModelCall, ModelCallId, OutputProjection,
};
use loom_types::{BlobId, ProjectId};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub use crate::profile::ProfileError;
use crate::{
    BaseWriterBackend, BaseWriterBinding, CancelledBaseWriterDiagnostic, ExactPromptEvidence,
    VerifiedBaseWriterCall, VerifiedCaseOutcome, VerifiedInferenceOutcome,
    VerifiedRuntimeChargeEvidence,
    canonical::{model_fingerprint_id, sampling_fingerprint},
    persisted::{
        BatchCaseCommitment, BatchCommitment, CallVerificationCommitment, RequestCaseCommitment,
        RequestCommitment, derive_batch_verification_fingerprint,
        derive_call_verification_fingerprint, derive_request_id as derive_committed_request_id,
        uncontrolled_program_fingerprint,
    },
};

pub const NO_CONTROL_PROGRAM_DOMAIN: &str = "loom/native-uncontrolled-base-completion/v1";

const BACKEND_AUDIT_FORMAT: &str = "loom.native-base-writer-audit.v1";
const SANITIZED_MODEL_FINGERPRINT_FORMAT: &str = "loom.native-model-fingerprint.v1";
const EVENT_LEDGER_FORMAT: &str = "loom.native-base-writer-event-ledger.v1";
const CANCELLED_MESSAGE: &str = "native generation cancelled";

/// Native llama implementation of the backend-neutral base-writer contract.
pub struct NativeLlamaWriter {
    pub(super) handle: NativeModelHandle,
}

impl fmt::Debug for NativeLlamaWriter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeLlamaWriter")
            .finish_non_exhaustive()
    }
}

impl NativeLlamaWriter {
    pub const fn new(handle: NativeModelHandle) -> Self {
        Self { handle }
    }
}

impl BaseWriterBackend for NativeLlamaWriter {
    type PreparedPrompt = PreparedBaseCompletion;
    type CaseSpec = BaseWriterCaseSpec;
    type Ticket = VerifiedInferenceTicket;
    type VerifiedBatch = VerifiedInferenceOutcome;
    type Error = InferenceError;

    fn prepare_completion(
        &self,
        binding: BaseWriterBinding,
        prompt: CompiledBaseCompletionPrompt,
    ) -> Result<Self::PreparedPrompt, Self::Error> {
        PreparedBaseCompletion::prepare(self.handle.clone(), binding, prompt)
    }

    fn start(
        &self,
        prepared: Self::PreparedPrompt,
        cases: Vec<Self::CaseSpec>,
    ) -> Result<Self::Ticket, Self::Error> {
        prepared.start(cases)
    }

    fn wait(&self, ticket: Self::Ticket) -> Result<Self::VerifiedBatch, Self::Error> {
        ticket.wait()
    }
}

/// One validated occurrence in an exact base-writer batch.
///
/// The native request ID and case ID are derived internally; callers can
/// select neither. In particular, the random-default seed sentinel is rejected
/// before any native work starts.
pub struct BaseWriterCaseSpec {
    call_id: ModelCallId,
    sampling: SamplingConfig,
    sampler_fingerprint: BlobId,
}

impl fmt::Debug for BaseWriterCaseSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BaseWriterCaseSpec")
            .field("call_id", &self.call_id)
            .field("sampler_fingerprint", &self.sampler_fingerprint)
            .finish_non_exhaustive()
    }
}

impl BaseWriterCaseSpec {
    pub fn new(call_id: ModelCallId, sampling: SamplingConfig) -> Result<Self, InferenceError> {
        validate_sampling(&sampling)?;
        let sampler_fingerprint = sampling_fingerprint(&sampling);
        Ok(Self {
            call_id,
            sampling,
            sampler_fingerprint,
        })
    }

    pub const fn call_id(&self) -> ModelCallId {
        self.call_id
    }

    pub const fn sampling(&self) -> &SamplingConfig {
        &self.sampling
    }

    pub const fn sampler_fingerprint(&self) -> BlobId {
        self.sampler_fingerprint
    }
}

/// Exact completion prompt prepared by the same resident handle that will run
/// generation.
///
/// ```compile_fail
/// use loom_inference::native_llama::PreparedBaseCompletion;
/// fn duplicate(value: PreparedBaseCompletion) {
///     let _copy = value.clone();
/// }
/// ```
///
/// ```compile_fail
/// use loom_inference::native_llama::PreparedBaseCompletion;
/// fn require_default<T: Default>() {}
/// require_default::<PreparedBaseCompletion>();
/// ```
pub struct PreparedBaseCompletion {
    pub(super) handle: NativeModelHandle,
    pub(super) material: PreparedMaterial,
}

impl fmt::Debug for PreparedBaseCompletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedBaseCompletion")
            .field("project_id", &self.material.project_id)
            .field("binding", &self.material.binding)
            .field("prompt_evidence", &self.material.prompt_evidence)
            .finish_non_exhaustive()
    }
}

impl PreparedBaseCompletion {
    pub fn prepare(
        handle: NativeModelHandle,
        binding: BaseWriterBinding,
        prompt: CompiledBaseCompletionPrompt,
    ) -> Result<Self, InferenceError> {
        let project_id = prompt.project_id();
        let prompt_text = prompt.exact_text().to_owned();
        let status = handle.status();
        let model_fingerprint = status
            .fingerprint
            .ok_or(InferenceError::ResidentModelHasNoFingerprint)?;
        binding.verify_native_model(&model_fingerprint)?;

        let mut prepared = handle.prepare_input(GenerationInput::Completion {
            prompts: vec![CompletionPrompt::Text {
                text: prompt_text,
                special_tokens: SpecialTokenPolicy::NoBosParseSpecial,
            }],
        })?;
        if prepared.len() != 1 {
            return Err(InferenceError::PreparedPromptCount {
                actual: prepared.len(),
            });
        }
        let prepared = prepared
            .pop()
            .ok_or(InferenceError::PreparedPromptCount { actual: 0 })?;
        validate_prepared_prompt(&prepared, prompt.exact_bytes())?;
        if prepared.token_ids.len() >= model_fingerprint.context_tokens as usize {
            return Err(InferenceError::PromptConsumesContext {
                prompt_tokens: prepared.token_ids.len(),
                context_tokens: model_fingerprint.context_tokens,
            });
        }

        let exact_prompt_token_ids = prepared
            .token_ids
            .iter()
            .map(|token| u32::try_from(*token).map_err(|_| InferenceError::InvalidPreparedTokens))
            .collect::<Result<Vec<_>, _>>()?;
        let prompt_evidence =
            ExactPromptEvidence::verified_completion_no_bos(prompt, exact_prompt_token_ids);
        let model_fingerprint_id = model_fingerprint_id(&model_fingerprint);
        let tokenizer_fingerprint = BlobId::from_str(&model_fingerprint.tokenizer_sha256)
            .map_err(|_| InferenceError::MalformedTokenizerFingerprint)?;

        Ok(Self {
            handle,
            material: PreparedMaterial {
                project_id,
                binding,
                prompt_evidence,
                model_fingerprint,
                model_fingerprint_id,
                tokenizer_fingerprint,
            },
        })
    }

    pub const fn project_id(&self) -> ProjectId {
        self.material.project_id
    }

    pub const fn binding(&self) -> &BaseWriterBinding {
        &self.material.binding
    }

    pub fn exact_prompt_utf8(&self) -> &[u8] {
        self.material.prompt_evidence.raw_utf8()
    }

    pub const fn exact_prompt_blob_id(&self) -> BlobId {
        self.material.prompt_evidence.raw_blob_id()
    }

    /// Domain-separated compiled identity binding the exact source blob,
    /// Completion/NoBos semantics, and ordered prepared token IDs.
    pub const fn prompt_fingerprint(&self) -> BlobId {
        self.material.prompt_evidence.compiled_fingerprint()
    }

    /// Exact nonnegative token IDs suitable for `PromptRecipe` persistence.
    pub fn exact_prompt_token_ids(&self) -> &[u32] {
        self.material.prompt_evidence.ordered_token_ids()
    }

    pub const fn prompt_token_fingerprint(&self) -> BlobId {
        self.material.prompt_evidence.token_fingerprint()
    }

    pub fn prompt_token_count(&self) -> usize {
        self.material.prompt_evidence.ordered_token_ids().len()
    }

    pub fn start(
        self,
        cases: Vec<BaseWriterCaseSpec>,
    ) -> Result<VerifiedInferenceTicket, InferenceError> {
        let Self { handle, material } = self;
        let pending = PendingBatch::compile(material, cases)?;
        let native_ticket = handle.generate_batch(pending.request.clone())?;
        Ok(VerifiedInferenceTicket {
            native_ticket: Some(native_ticket),
            pending: Some(pending),
            // Keep the exact resident owner-worker alive until its seal is
            // consumed. A separately loaded handle is never substituted.
            handle: Some(handle),
        })
    }
}

/// Live generation ticket whose only success path consumes an owner-worker
/// [`VerifiedGenerationBatch`].
///
/// ```compile_fail
/// use loom_inference::native_llama::VerifiedInferenceTicket;
/// fn duplicate(value: VerifiedInferenceTicket) {
///     let _copy = value.clone();
/// }
/// ```
///
/// ```compile_fail
/// use loom_inference::native_llama::VerifiedInferenceTicket;
/// fn require_default<T: Default>() {}
/// require_default::<VerifiedInferenceTicket>();
/// ```
pub struct VerifiedInferenceTicket {
    native_ticket: Option<GenerationTicket>,
    pending: Option<PendingBatch>,
    handle: Option<NativeModelHandle>,
}

impl fmt::Debug for VerifiedInferenceTicket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedInferenceTicket")
            .field("active", &self.native_ticket.is_some())
            .field(
                "case_count",
                &self
                    .pending
                    .as_ref()
                    .map_or(0, |pending| pending.cases.len()),
            )
            .finish_non_exhaustive()
    }
}

impl VerifiedInferenceTicket {
    pub fn request_id(&self) -> &str {
        self.pending
            .as_ref()
            .map_or("", |pending| pending.request.request_id.as_str())
    }

    pub fn cancel_call(&self, call_id: ModelCallId) -> bool {
        let Some(pending) = &self.pending else {
            return false;
        };
        let Some(case) = pending.cases.iter().find(|case| case.call_id == call_id) else {
            return false;
        };
        self.native_ticket
            .as_ref()
            .is_some_and(|ticket| ticket.cancel_branch(&case.native_case_id))
    }

    pub fn cancel_all(&self) -> usize {
        self.native_ticket
            .as_ref()
            .map_or(0, GenerationTicket::cancel_all)
    }

    /// Read a best-effort live event. Final evidence never depends on this
    /// receiver: the native seal carries an independent retained ledger.
    pub fn try_event(&self) -> Option<GenerationEvent> {
        self.native_ticket
            .as_ref()
            .and_then(|ticket| ticket.events.try_recv().ok())
    }

    pub fn wait(self) -> Result<VerifiedInferenceOutcome, InferenceError> {
        let Self {
            native_ticket,
            pending,
            handle,
        } = self;
        let ticket = native_ticket.ok_or(InferenceError::TicketAlreadyConsumed)?;
        let pending = pending.ok_or(InferenceError::TicketAlreadyConsumed)?;
        let resident_handle = handle.ok_or(InferenceError::TicketAlreadyConsumed)?;
        let seal = ticket.wait_verified()?;
        let result = mint_envelope(pending, &seal);
        drop(resident_handle);
        result
    }

    pub fn try_wait(&mut self) -> Result<Option<VerifiedInferenceOutcome>, InferenceError> {
        let result = self
            .native_ticket
            .as_ref()
            .ok_or(InferenceError::TicketAlreadyConsumed)?
            .try_wait_verified();
        match result {
            Ok(None) => Ok(None),
            Ok(Some(seal)) => {
                let _ = self.native_ticket.take();
                let resident_handle = self
                    .handle
                    .take()
                    .ok_or(InferenceError::TicketAlreadyConsumed)?;
                let pending = self
                    .pending
                    .take()
                    .ok_or(InferenceError::TicketAlreadyConsumed)?;
                let result = mint_envelope(pending, &seal).map(Some);
                drop(resident_handle);
                result
            }
            Err(error) => {
                let _ = self.native_ticket.take();
                let _ = self.pending.take();
                let _ = self.handle.take();
                Err(error.into())
            }
        }
    }
}

#[derive(Error)]
pub enum InferenceError {
    #[error(transparent)]
    Native(#[from] NativeError),
    #[error(transparent)]
    Profile(#[from] ProfileError),
    #[error(transparent)]
    Call(#[from] CallError),
    #[error(transparent)]
    Bound(#[from] BoundError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("resident native model exposes no fingerprint")]
    ResidentModelHasNoFingerprint,
    #[error("native prompt preparation returned {actual} prompts instead of one")]
    PreparedPromptCount { actual: usize },
    #[error("native prompt preparation did not preserve completion/Text/NoBos semantics")]
    PreparedPromptSemantics,
    #[error("native prompt preparation did not preserve the exact prompt-byte digest")]
    PreparedPromptDigest,
    #[error("native prompt preparation produced no tokens or a negative token ID")]
    InvalidPreparedTokens,
    #[error(
        "prepared prompt uses {prompt_tokens} tokens, leaving no room in context {context_tokens}"
    )]
    PromptConsumesContext {
        prompt_tokens: usize,
        context_tokens: u32,
    },
    #[error("tokenizer fingerprint is not canonical SHA-256")]
    MalformedTokenizerFingerprint,
    #[error("base-writer batch is empty")]
    EmptyBatch,
    #[error("base-writer batch has {actual} cases; resident maximum is {maximum}")]
    TooManyCases { actual: usize, maximum: u32 },
    #[error("base-writer batch repeats model call ID {0}")]
    DuplicateCallId(ModelCallId),
    #[error("sampling must use an explicit seed, not the u32::MAX random sentinel")]
    DefaultSeed,
    #[error("invalid sampling configuration: {0}")]
    InvalidSampling(&'static str),
    #[error(
        "prompt plus maximum completion uses {requested} tokens; context capacity is {context}"
    )]
    ContextBudget { requested: u64, context: u32 },
    #[error("verified inference ticket was already consumed")]
    TicketAlreadyConsumed,
    #[error("native owner-worker seal does not match the prepared request")]
    SealRequestMismatch,
    #[error("native owner-worker seal does not match the prepared model fingerprint")]
    SealModelMismatch,
    #[error("native owner-worker seal has inconsistent output, terminal, or event counts")]
    SealCountMismatch,
    #[error("sealed output {index} has mismatched request, case, model, or ordering")]
    OutputIdentityMismatch { index: usize },
    #[error("sealed output {index} is not real in-process evidence")]
    OutputNotLive { index: usize },
    #[error("sealed output {index} has invalid prompt, completion, or cache metrics")]
    OutputMetricsMismatch { index: usize },
    #[error("sealed output {index} contains an invalid generated token ID")]
    InvalidGeneratedToken { index: usize },
    #[error("sealed event ledger for case {index} is malformed")]
    MalformedEventLedger { index: usize },
    #[error("sealed raw delta bytes for case {index} disagree with displayed output semantics")]
    OutputProjectionMismatch { index: usize },
    #[error("sealed terminal for case {index} is unsupported or inconsistent")]
    TerminalMismatch { index: usize },
    #[error("{kind} evidence has {actual} bytes; maximum is {maximum}")]
    EvidenceTooLarge {
        kind: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("verified case outcomes violated an internal admission invariant")]
    AdmissionInvariant,
}

impl fmt::Debug for InferenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Native(_) => "InferenceError::Native(..)",
            Self::Profile(_) => "InferenceError::Profile(..)",
            Self::Call(_) => "InferenceError::Call(..)",
            Self::Bound(_) => "InferenceError::Bound(..)",
            Self::Json(_) => "InferenceError::Json(..)",
            Self::ResidentModelHasNoFingerprint => "InferenceError::ResidentModelHasNoFingerprint",
            Self::PreparedPromptCount { .. } => "InferenceError::PreparedPromptCount { .. }",
            Self::PreparedPromptSemantics => "InferenceError::PreparedPromptSemantics",
            Self::PreparedPromptDigest => "InferenceError::PreparedPromptDigest",
            Self::InvalidPreparedTokens => "InferenceError::InvalidPreparedTokens",
            Self::PromptConsumesContext { .. } => "InferenceError::PromptConsumesContext { .. }",
            Self::MalformedTokenizerFingerprint => "InferenceError::MalformedTokenizerFingerprint",
            Self::EmptyBatch => "InferenceError::EmptyBatch",
            Self::TooManyCases { .. } => "InferenceError::TooManyCases { .. }",
            Self::DuplicateCallId(_) => "InferenceError::DuplicateCallId(..)",
            Self::DefaultSeed => "InferenceError::DefaultSeed",
            Self::InvalidSampling(_) => "InferenceError::InvalidSampling(..)",
            Self::ContextBudget { .. } => "InferenceError::ContextBudget { .. }",
            Self::TicketAlreadyConsumed => "InferenceError::TicketAlreadyConsumed",
            Self::SealRequestMismatch => "InferenceError::SealRequestMismatch",
            Self::SealModelMismatch => "InferenceError::SealModelMismatch",
            Self::SealCountMismatch => "InferenceError::SealCountMismatch",
            Self::OutputIdentityMismatch { .. } => "InferenceError::OutputIdentityMismatch { .. }",
            Self::OutputNotLive { .. } => "InferenceError::OutputNotLive { .. }",
            Self::OutputMetricsMismatch { .. } => "InferenceError::OutputMetricsMismatch { .. }",
            Self::InvalidGeneratedToken { .. } => "InferenceError::InvalidGeneratedToken { .. }",
            Self::MalformedEventLedger { .. } => "InferenceError::MalformedEventLedger { .. }",
            Self::OutputProjectionMismatch { .. } => {
                "InferenceError::OutputProjectionMismatch { .. }"
            }
            Self::TerminalMismatch { .. } => "InferenceError::TerminalMismatch { .. }",
            Self::EvidenceTooLarge { .. } => "InferenceError::EvidenceTooLarge { .. }",
            Self::AdmissionInvariant => "InferenceError::AdmissionInvariant",
        };
        formatter.write_str(name)
    }
}

#[derive(Debug)]
pub(super) struct PreparedMaterial {
    pub(super) project_id: ProjectId,
    pub(super) binding: BaseWriterBinding,
    pub(super) prompt_evidence: ExactPromptEvidence,
    pub(super) model_fingerprint: ModelFingerprint,
    pub(super) model_fingerprint_id: BlobId,
    pub(super) tokenizer_fingerprint: BlobId,
}

#[derive(Debug)]
struct PendingCase {
    call_id: ModelCallId,
    scope: CallScope,
    native_case_id: String,
    sampling: SamplingConfig,
    sampler_fingerprint: BlobId,
}

#[derive(Debug)]
struct PendingBatch {
    material: PreparedMaterial,
    request: GenerationBatchRequest,
    cases: Vec<PendingCase>,
}

impl PendingBatch {
    fn compile(
        material: PreparedMaterial,
        cases: Vec<BaseWriterCaseSpec>,
    ) -> Result<Self, InferenceError> {
        if cases.is_empty() {
            return Err(InferenceError::EmptyBatch);
        }
        let maximum_cases =
            (material.model_fingerprint.max_sequences as usize).min(MAX_BASE_WRITER_BATCH_CASES);
        if cases.len() > maximum_cases {
            return Err(InferenceError::TooManyCases {
                actual: cases.len(),
                maximum: u32::try_from(maximum_cases).unwrap_or(u32::MAX),
            });
        }
        let mut seen = HashSet::with_capacity(cases.len());
        let mut pending_cases = Vec::with_capacity(cases.len());
        for case in cases {
            if !seen.insert(case.call_id) {
                return Err(InferenceError::DuplicateCallId(case.call_id));
            }
            let requested = (material.prompt_evidence.ordered_token_ids().len() as u64)
                .checked_add(u64::from(case.sampling.max_tokens))
                .ok_or(InferenceError::ContextBudget {
                    requested: u64::MAX,
                    context: material.model_fingerprint.context_tokens,
                })?;
            if requested > u64::from(material.model_fingerprint.context_tokens) {
                return Err(InferenceError::ContextBudget {
                    requested,
                    context: material.model_fingerprint.context_tokens,
                });
            }
            pending_cases.push(PendingCase {
                call_id: case.call_id,
                scope: material.prompt_evidence.scope(),
                native_case_id: format!("call-{}", case.call_id),
                sampling: case.sampling,
                sampler_fingerprint: case.sampler_fingerprint,
            });
        }

        let native_prompt_token_ids = material
            .prompt_evidence
            .ordered_token_ids()
            .iter()
            .map(|token| i32::try_from(*token).map_err(|_| InferenceError::InvalidPreparedTokens))
            .collect::<Result<Vec<_>, _>>()?;
        let request_id = derive_request_id(&material, &pending_cases);
        let request = GenerationBatchRequest {
            request_id,
            model_id: material.model_fingerprint.model_id.clone(),
            cases: pending_cases
                .iter()
                .map(|case| GenerationCase {
                    case_id: case.native_case_id.clone(),
                    input: GenerationInput::Completion {
                        prompts: vec![CompletionPrompt::Tokens {
                            token_ids: native_prompt_token_ids.clone(),
                        }],
                    },
                    sampling: case.sampling.clone(),
                    cached_prefix: None,
                })
                .collect(),
        };
        Ok(Self {
            material,
            request,
            cases: pending_cases,
        })
    }
}

trait SealView {
    fn request(&self) -> &GenerationBatchRequest;
    fn model_fingerprint(&self) -> &ModelFingerprint;
    fn outputs(&self) -> &[GenerationOutput];
    fn terminal_sampled_token_ids(&self) -> &[Option<i32>];
    fn events(&self) -> &[GenerationEvent];
}

impl SealView for VerifiedGenerationBatch {
    fn request(&self) -> &GenerationBatchRequest {
        self.request()
    }

    fn model_fingerprint(&self) -> &ModelFingerprint {
        self.model_fingerprint()
    }

    fn outputs(&self) -> &[GenerationOutput] {
        self.outputs()
    }

    fn terminal_sampled_token_ids(&self) -> &[Option<i32>] {
        self.terminal_sampled_token_ids()
    }

    fn events(&self) -> &[GenerationEvent] {
        self.events()
    }
}

fn mint_envelope(
    pending: PendingBatch,
    seal: &impl SealView,
) -> Result<VerifiedInferenceOutcome, InferenceError> {
    if seal.request() != &pending.request {
        return Err(InferenceError::SealRequestMismatch);
    }
    if seal.model_fingerprint() != &pending.material.model_fingerprint {
        return Err(InferenceError::SealModelMismatch);
    }
    pending
        .material
        .binding
        .verify_native_model(seal.model_fingerprint())?;
    if seal.outputs().len() != pending.cases.len()
        || seal.terminal_sampled_token_ids().len() != pending.cases.len()
    {
        return Err(InferenceError::SealCountMismatch);
    }

    let ledgers = validate_and_partition_events(&pending, seal)?;
    let mut outcomes = Vec::with_capacity(pending.cases.len());
    let mut batch_commitments = Vec::with_capacity(pending.cases.len());

    for (index, ((case, output), terminal_sampled_token_id)) in pending
        .cases
        .iter()
        .zip(seal.outputs())
        .zip(seal.terminal_sampled_token_ids())
        .enumerate()
    {
        let evidence = derive_case_evidence(
            &pending,
            seal,
            &ledgers[index],
            case,
            output,
            *terminal_sampled_token_id,
            index,
        )?;
        let minted = mint_case(
            &pending,
            case,
            output,
            *terminal_sampled_token_id,
            evidence,
            index,
        )?;
        batch_commitments.push(BatchCaseCommitment {
            call_id: case.call_id,
            completed: minted.is_completed(),
            verification_fingerprint: minted.verification_fingerprint(),
        });
        outcomes.push(match minted {
            MintedCase::Completed(call) => VerifiedCaseOutcome::completed(index, call),
            MintedCase::Cancelled(diagnostic) => VerifiedCaseOutcome::cancelled(index, diagnostic),
        });
    }

    let verification_fingerprint = derive_batch_verification_fingerprint(&BatchCommitment {
        project_id: pending.material.project_id,
        binding_fingerprint: pending.material.binding.fingerprint(),
        source_prompt_fingerprint: pending.material.prompt_evidence.source_prompt_fingerprint(),
        compiled_prompt_fingerprint: pending.material.prompt_evidence.compiled_fingerprint(),
        request_id: &pending.request.request_id,
        cases: &batch_commitments,
    });
    VerifiedInferenceOutcome::mint(
        pending.material.project_id,
        pending.material.binding,
        pending.material.prompt_evidence,
        pending.request.request_id,
        outcomes,
        verification_fingerprint,
    )
    .map_err(|_| InferenceError::AdmissionInvariant)
}

struct CaseEvidence {
    raw_output: Vec<u8>,
    generated_token_ids: Vec<u32>,
    event_json: Vec<u8>,
    event_blob_id: BlobId,
    backend_audit_json: Vec<u8>,
    backend_receipt_blob_id: BlobId,
    verification_fingerprint: BlobId,
}

enum MintedCase {
    Completed(VerifiedBaseWriterCall),
    Cancelled(CancelledBaseWriterDiagnostic),
}

impl MintedCase {
    const fn is_completed(&self) -> bool {
        matches!(self, Self::Completed(_))
    }

    const fn verification_fingerprint(&self) -> BlobId {
        match self {
            Self::Completed(call) => call.verification_fingerprint,
            Self::Cancelled(diagnostic) => diagnostic.verification_fingerprint,
        }
    }
}

fn derive_case_evidence(
    pending: &PendingBatch,
    seal: &impl SealView,
    ledger: &ValidatedLedger<'_>,
    case: &PendingCase,
    output: &GenerationOutput,
    terminal_sampled_token_id: Option<i32>,
    index: usize,
) -> Result<CaseEvidence, InferenceError> {
    validate_output_identity(pending, case, output, index)?;
    let raw_output = ledger.raw_output.as_bytes().to_vec();
    validate_output_projection(case, output, terminal_sampled_token_id, &raw_output, index)?;
    let generated_token_ids = output
        .generated_token_ids
        .iter()
        .map(|token| {
            u32::try_from(*token).map_err(|_| InferenceError::InvalidGeneratedToken { index })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let compact_events = ledger
        .events
        .iter()
        .map(|event| CompactGenerationEvent {
            event_index: event.event_index,
            event: &event.event,
        })
        .collect::<Vec<_>>();
    let event_json = serialize_bounded_evidence(
        &EventLedgerRecord {
            format: EVENT_LEDGER_FORMAT,
            call_id: case.call_id,
            native_case_id: &case.native_case_id,
            input_index: index,
            events: &compact_events,
        },
        "native event ledger",
    )?;
    let event_blob_id = BlobId::digest(&event_json);
    let backend_audit_json = serialize_bounded_evidence(
        &BackendAuditRecord {
            format: BACKEND_AUDIT_FORMAT,
            project_id: pending.material.project_id,
            binding_id: pending.material.binding.binding_id(),
            call_id: case.call_id,
            scope: case.scope,
            exact_prompt_blob_id: pending.material.prompt_evidence.raw_blob_id(),
            source_prompt_fingerprint: pending.material.prompt_evidence.source_prompt_fingerprint(),
            prompt_content_fingerprint: pending.material.prompt_evidence.content_fingerprint(),
            treatment_recipe_fingerprint: pending
                .material
                .prompt_evidence
                .treatment_recipe_fingerprint(),
            compiled_prompt_fingerprint: pending.material.prompt_evidence.compiled_fingerprint(),
            prompt_token_fingerprint: pending.material.prompt_evidence.token_fingerprint(),
            native_request_id: &seal.request().request_id,
            native_case_id: &case.native_case_id,
            input_index: index,
            sampler_fingerprint: case.sampler_fingerprint,
            sampling: &case.sampling,
            model_fingerprint: SanitizedModelFingerprint::from(seal.model_fingerprint()),
            output: SanitizedGenerationOutput::new(output, &raw_output, &generated_token_ids),
            terminal_sampled_token_id,
            event_stream_blob_id: event_blob_id,
        },
        "native backend audit",
    )?;
    let backend_receipt_blob_id = BlobId::digest(&backend_audit_json);
    let verification_fingerprint = call_verification_fingerprint(
        pending,
        case,
        &raw_output,
        &generated_token_ids,
        event_blob_id,
        backend_receipt_blob_id,
        terminal_sampled_token_id,
    );
    Ok(CaseEvidence {
        raw_output,
        generated_token_ids,
        event_json,
        event_blob_id,
        backend_audit_json,
        backend_receipt_blob_id,
        verification_fingerprint,
    })
}

fn mint_case(
    pending: &PendingBatch,
    case: &PendingCase,
    output: &GenerationOutput,
    terminal_sampled_token_id: Option<i32>,
    evidence: CaseEvidence,
    index: usize,
) -> Result<MintedCase, InferenceError> {
    let identity = CallIdentity::new(
        case.scope,
        pending.material.model_fingerprint_id,
        pending.material.tokenizer_fingerprint,
        pending.material.prompt_evidence.compiled_fingerprint(),
        case.sampler_fingerprint,
        no_control_program_fingerprint(),
        u64::from(case.sampling.seed),
    );
    match output.state {
        GenerationState::Completed => {
            let completed = CompletedCall::new(
                &evidence.raw_output,
                &evidence.generated_token_ids,
                evidence.event_blob_id,
                Some(evidence.backend_receipt_blob_id),
            )?;
            let model_call = ModelCall::new(
                case.call_id,
                identity,
                CallEvidenceClass::LiveBaseWriterClaim,
                CallTerminal::Completed(completed),
            )?;
            let displayed_output = output.text.as_bytes().to_vec();
            let output_projection = (!displayed_output.is_empty())
                .then(|| {
                    OutputProjection::new(
                        &evidence.raw_output,
                        displayed_output.len() as u64,
                        displayed_output.len() as u64,
                    )
                })
                .transpose()?;
            let runtime_charge = VerifiedRuntimeChargeEvidence::mint(
                output.metrics.prompt_tokens as u64,
                output.metrics.completion_tokens as u64,
                output.metrics.duration_ms,
                evidence.verification_fingerprint,
            );
            Ok(MintedCase::Completed(VerifiedBaseWriterCall {
                model_call,
                raw_output: evidence.raw_output,
                generated_token_ids: evidence.generated_token_ids,
                event_json: evidence.event_json,
                backend_audit_json: evidence.backend_audit_json,
                displayed_output,
                output_projection,
                terminal_sampled_token_id,
                verification_fingerprint: evidence.verification_fingerprint,
                runtime_charge,
            }))
        }
        GenerationState::Cancelled => {
            let message = BoundedText::new(CANCELLED_MESSAGE)?;
            let model_call = ModelCall::new(
                case.call_id,
                identity,
                CallEvidenceClass::LiveBaseWriterClaim,
                CallTerminal::Cancelled { message },
            )?;
            Ok(MintedCase::Cancelled(CancelledBaseWriterDiagnostic {
                model_call,
                partial_raw_output: evidence.raw_output,
                generated_token_ids: evidence.generated_token_ids,
                event_json: evidence.event_json,
                backend_audit_json: evidence.backend_audit_json,
                verification_fingerprint: evidence.verification_fingerprint,
            }))
        }
        GenerationState::Queued
        | GenerationState::Prefilling
        | GenerationState::Generating
        | GenerationState::Failed => Err(InferenceError::TerminalMismatch { index }),
    }
}

#[derive(Serialize)]
struct BackendAuditRecord<'a> {
    format: &'static str,
    project_id: ProjectId,
    binding_id: &'a str,
    call_id: ModelCallId,
    scope: CallScope,
    exact_prompt_blob_id: BlobId,
    source_prompt_fingerprint: BlobId,
    prompt_content_fingerprint: BlobId,
    treatment_recipe_fingerprint: BlobId,
    compiled_prompt_fingerprint: BlobId,
    prompt_token_fingerprint: BlobId,
    native_request_id: &'a str,
    native_case_id: &'a str,
    input_index: usize,
    sampler_fingerprint: BlobId,
    sampling: &'a SamplingConfig,
    model_fingerprint: SanitizedModelFingerprint<'a>,
    output: SanitizedGenerationOutput<'a>,
    terminal_sampled_token_id: Option<i32>,
    event_stream_blob_id: BlobId,
}

/// Durable semantic model identity. The live full fingerprint is still
/// compared before minting, but the resident process label is not causal model
/// identity and never enters persisted audit bytes.
#[derive(Serialize)]
struct SanitizedModelFingerprint<'a> {
    format: &'static str,
    model_size: u64,
    model_sha256: &'a str,
    tokenizer_sha256: &'a str,
    chat_template_sha256: &'a str,
    multimodal_projector_sha256: Option<&'a str>,
    binding_version: &'a str,
    build_id: &'a str,
    backend: &'a str,
    context_tokens: u32,
    batch_tokens: u32,
    max_sequences: u32,
    rope_config_sha256: &'a str,
    kv_layout_sha256: &'a str,
}

impl<'a> From<&'a ModelFingerprint> for SanitizedModelFingerprint<'a> {
    fn from(fingerprint: &'a ModelFingerprint) -> Self {
        Self {
            format: SANITIZED_MODEL_FINGERPRINT_FORMAT,
            model_size: fingerprint.model_size,
            model_sha256: &fingerprint.model_sha256,
            tokenizer_sha256: &fingerprint.tokenizer_sha256,
            chat_template_sha256: &fingerprint.chat_template_sha256,
            multimodal_projector_sha256: fingerprint.multimodal_projector_sha256.as_deref(),
            binding_version: &fingerprint.binding_version,
            build_id: &fingerprint.build_id,
            backend: &fingerprint.backend,
            context_tokens: fingerprint.context_tokens,
            batch_tokens: fingerprint.batch_tokens,
            max_sequences: fingerprint.max_sequences,
            rope_config_sha256: &fingerprint.rope_config_sha256,
            kv_layout_sha256: &fingerprint.kv_layout_sha256,
        }
    }
}

#[derive(Serialize)]
struct SanitizedGenerationOutput<'a> {
    format: &'static str,
    request_id: &'a str,
    branch_id: &'a str,
    input_index: usize,
    displayed_output_blob_id: BlobId,
    displayed_output_byte_len: usize,
    raw_output_blob_id: BlobId,
    raw_output_byte_len: usize,
    generated_token_ids_blob_id: BlobId,
    generated_token_count: usize,
    token_observations: Option<&'a [TokenObservation]>,
    state: GenerationState,
    finish_reason: &'a str,
    metrics: &'a GenerationMetrics,
    real_engine_invoked: bool,
    fake_fixture: bool,
    transport: NativeTransport,
}

impl<'a> SanitizedGenerationOutput<'a> {
    fn new(output: &'a GenerationOutput, raw_output: &[u8], token_ids: &[u32]) -> Self {
        Self {
            format: "loom.native-base-writer-output-audit.v1",
            request_id: &output.request_id,
            branch_id: &output.branch_id,
            input_index: output.input_index,
            displayed_output_blob_id: BlobId::digest(output.text.as_bytes()),
            displayed_output_byte_len: output.text.len(),
            raw_output_blob_id: BlobId::digest(raw_output),
            raw_output_byte_len: raw_output.len(),
            generated_token_ids_blob_id: token_ids_blob_id(token_ids),
            generated_token_count: token_ids.len(),
            token_observations: output.token_observations.as_deref(),
            state: output.state,
            finish_reason: &output.finish_reason,
            metrics: &output.metrics,
            real_engine_invoked: output.real_engine_invoked,
            fake_fixture: output.fake_fixture,
            transport: output.transport,
        }
    }
}

#[derive(Serialize)]
struct EventLedgerRecord<'a> {
    format: &'static str,
    call_id: ModelCallId,
    native_case_id: &'a str,
    input_index: usize,
    events: &'a [CompactGenerationEvent<'a>],
}

/// A validated event can omit request/case/sequence identity repeated by the
/// native wire object: the ledger header carries it once and validation has
/// already proven every omitted field equal. The semantic event and original
/// monotonically increasing index remain exact.
#[derive(Serialize)]
struct CompactGenerationEvent<'a> {
    event_index: u64,
    event: &'a GenerationEventKind,
}

pub(super) fn serialize_bounded_evidence(
    value: &impl Serialize,
    kind: &'static str,
) -> Result<Vec<u8>, InferenceError> {
    serialize_evidence_with_limit(value, kind, MAX_BACKEND_EVIDENCE_BYTES)
}

fn serialize_evidence_with_limit(
    value: &impl Serialize,
    kind: &'static str,
    maximum: usize,
) -> Result<Vec<u8>, InferenceError> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > maximum {
        return Err(InferenceError::EvidenceTooLarge {
            kind,
            actual: bytes.len(),
            maximum,
        });
    }
    Ok(bytes)
}

pub(super) fn token_ids_blob_id(token_ids: &[u32]) -> BlobId {
    let mut digest = Sha256::new();
    for token_id in token_ids {
        digest.update(token_id.to_be_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}

struct ValidatedLedger<'a> {
    events: Vec<&'a GenerationEvent>,
    raw_output: String,
}

fn validate_and_partition_events<'a>(
    pending: &PendingBatch,
    seal: &'a impl SealView,
) -> Result<Vec<ValidatedLedger<'a>>, InferenceError> {
    let mut ledgers = (0..pending.cases.len())
        .map(|_| ValidatedLedger {
            events: Vec::new(),
            raw_output: String::new(),
        })
        .collect::<Vec<_>>();
    for event in seal.events() {
        let Some(case) = pending.cases.get(event.input_index) else {
            return Err(InferenceError::MalformedEventLedger {
                index: event.input_index,
            });
        };
        let sequence_id =
            i32::try_from(event.input_index).map_err(|_| InferenceError::MalformedEventLedger {
                index: event.input_index,
            })?;
        if event.request_id != pending.request.request_id
            || event.branch_id != case.native_case_id
            || event.sequence_id != sequence_id
        {
            return Err(InferenceError::MalformedEventLedger {
                index: event.input_index,
            });
        }
        ledgers[event.input_index].events.push(event);
    }

    for (index, (ledger, output)) in ledgers.iter_mut().zip(seal.outputs()).enumerate() {
        let mut terminal = None;
        for (expected_index, event) in ledger.events.iter().enumerate() {
            if event.event_index != expected_index as u64 || terminal.is_some() {
                return Err(InferenceError::MalformedEventLedger { index });
            }
            match &event.event {
                GenerationEventKind::State {
                    state: GenerationState::Prefilling,
                } if expected_index == 0 => {}
                GenerationEventKind::State {
                    state: GenerationState::Generating,
                } if expected_index == 1 => {}
                GenerationEventKind::Delta { text } if expected_index >= 2 && !text.is_empty() => {
                    ledger.raw_output.push_str(text);
                }
                GenerationEventKind::State { state }
                    if expected_index >= 2
                        && matches!(
                            state,
                            GenerationState::Completed | GenerationState::Cancelled
                        ) =>
                {
                    terminal = Some(*state);
                }
                GenerationEventKind::State { .. }
                | GenerationEventKind::Delta { .. }
                | GenerationEventKind::Warning { .. } => {
                    return Err(InferenceError::MalformedEventLedger { index });
                }
            }
        }
        if terminal != Some(output.state) {
            return Err(InferenceError::MalformedEventLedger { index });
        }
    }
    Ok(ledgers)
}

fn validate_output_identity(
    pending: &PendingBatch,
    case: &PendingCase,
    output: &GenerationOutput,
    index: usize,
) -> Result<(), InferenceError> {
    if output.request_id != pending.request.request_id
        || output.branch_id != case.native_case_id
        || output.input_index != index
        || output.model_id != pending.request.model_id
    {
        return Err(InferenceError::OutputIdentityMismatch { index });
    }
    if !output.real_engine_invoked
        || output.fake_fixture
        || output.transport != NativeTransport::InProcess
    {
        return Err(InferenceError::OutputNotLive { index });
    }
    let prompt_tokens = pending.material.prompt_evidence.ordered_token_ids().len();
    let expected_shared_prefix_tokens = if pending.cases.len() > 1 {
        prompt_tokens.saturating_sub(1)
    } else {
        0
    };
    if output.metrics.prompt_tokens != prompt_tokens
        || output.metrics.completion_tokens != output.generated_token_ids.len()
        || output.generated_token_ids.len() > case.sampling.max_tokens as usize
        || output.metrics.shared_prefix_tokens != expected_shared_prefix_tokens
        || output.metrics.cache.supplied_prefix_tokens != 0
        || output.metrics.cache.restored_prefix_tokens != 0
        || output.metrics.cache.batch_shared_prefix_tokens != expected_shared_prefix_tokens
    {
        return Err(InferenceError::OutputMetricsMismatch { index });
    }
    Ok(())
}

fn validate_output_projection(
    case: &PendingCase,
    output: &GenerationOutput,
    terminal_sampled_token_id: Option<i32>,
    raw_output: &[u8],
    index: usize,
) -> Result<(), InferenceError> {
    let displayed = output.text.as_bytes();
    match (output.state, output.finish_reason.as_str()) {
        (GenerationState::Cancelled, "cancelled") if terminal_sampled_token_id.is_none() => {
            if displayed != raw_output {
                return Err(InferenceError::OutputProjectionMismatch { index });
            }
        }
        (GenerationState::Completed, "end_of_generation")
            if terminal_sampled_token_id.is_some() =>
        {
            if displayed != raw_output {
                return Err(InferenceError::OutputProjectionMismatch { index });
            }
        }
        (GenerationState::Completed, "max_tokens")
            if terminal_sampled_token_id.is_none()
                && output.generated_token_ids.len() == case.sampling.max_tokens as usize =>
        {
            if displayed != raw_output {
                return Err(InferenceError::OutputProjectionMismatch { index });
            }
        }
        (GenerationState::Completed, "stop_sequence") if terminal_sampled_token_id.is_none() => {
            let Some(suffix) = raw_output.strip_prefix(displayed) else {
                return Err(InferenceError::OutputProjectionMismatch { index });
            };
            if suffix.is_empty()
                || !case
                    .sampling
                    .stop
                    .iter()
                    .any(|stop| stop.as_bytes() == suffix)
            {
                return Err(InferenceError::OutputProjectionMismatch { index });
            }
        }
        _ => return Err(InferenceError::TerminalMismatch { index }),
    }
    Ok(())
}

pub(super) fn validate_prepared_prompt(
    prepared: &PreparedPrompt,
    exact_prompt_utf8: &[u8],
) -> Result<(), InferenceError> {
    if prepared.input_index != 0
        || prepared.prompt_form != PromptForm::Completion
        || prepared.token_policy != PromptTokenPolicy::NoBosParseSpecial
    {
        return Err(InferenceError::PreparedPromptSemantics);
    }
    if prepared.source_sha256 != BlobId::digest(exact_prompt_utf8).to_hex() {
        return Err(InferenceError::PreparedPromptDigest);
    }
    if prepared.token_ids.is_empty() || prepared.token_ids.iter().any(|token| *token < 0) {
        return Err(InferenceError::InvalidPreparedTokens);
    }
    Ok(())
}

fn derive_request_id(material: &PreparedMaterial, cases: &[PendingCase]) -> String {
    let case_commitments = cases
        .iter()
        .map(|case| RequestCaseCommitment {
            call_id: case.call_id,
            scope: case.scope,
            sampler_fingerprint: case.sampler_fingerprint,
            seed: case.sampling.seed,
        })
        .collect::<Vec<_>>();
    derive_committed_request_id(&RequestCommitment {
        project_id: material.project_id,
        binding_fingerprint: material.binding.fingerprint(),
        model_fingerprint: material.model_fingerprint_id,
        source_prompt_fingerprint: material.prompt_evidence.source_prompt_fingerprint(),
        treatment_recipe_fingerprint: material.prompt_evidence.treatment_recipe_fingerprint(),
        raw_prompt_blob_id: material.prompt_evidence.raw_blob_id(),
        compiled_prompt_fingerprint: material.prompt_evidence.compiled_fingerprint(),
        prompt_token_fingerprint: material.prompt_evidence.token_fingerprint(),
        raw_prompt_byte_len: material.prompt_evidence.raw_utf8().len(),
        prompt_token_ids: material.prompt_evidence.ordered_token_ids(),
        cases: &case_commitments,
    })
}

fn call_verification_fingerprint(
    pending: &PendingBatch,
    case: &PendingCase,
    raw_output: &[u8],
    generated_token_ids: &[u32],
    event_blob_id: BlobId,
    backend_receipt_blob_id: BlobId,
    terminal_sampled_token_id: Option<i32>,
) -> BlobId {
    derive_call_verification_fingerprint(&CallVerificationCommitment {
        project_id: pending.material.project_id,
        request_id: &pending.request.request_id,
        call_id: case.call_id,
        scope: case.scope,
        model_fingerprint: pending.material.model_fingerprint_id,
        source_prompt_fingerprint: pending.material.prompt_evidence.source_prompt_fingerprint(),
        treatment_recipe_fingerprint: pending
            .material
            .prompt_evidence
            .treatment_recipe_fingerprint(),
        raw_prompt_blob_id: pending.material.prompt_evidence.raw_blob_id(),
        compiled_prompt_fingerprint: pending.material.prompt_evidence.compiled_fingerprint(),
        prompt_token_fingerprint: pending.material.prompt_evidence.token_fingerprint(),
        sampler_fingerprint: case.sampler_fingerprint,
        control_program_fingerprint: uncontrolled_program_fingerprint(),
        raw_output,
        generated_token_ids,
        event_blob_id,
        backend_receipt_blob_id,
        terminal_sampled_token_id,
    })
}

pub fn no_control_program_fingerprint() -> BlobId {
    uncontrolled_program_fingerprint()
}

pub(super) fn validate_sampling(sampling: &SamplingConfig) -> Result<(), InferenceError> {
    if sampling.seed == u32::MAX {
        return Err(InferenceError::DefaultSeed);
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
        return Err(InferenceError::InvalidSampling(
            "a floating-point sampler value is non-finite or out of range",
        ));
    }
    if sampling.top_k < 0
        || sampling.repeat_last_n < -1
        || sampling.dry_penalty_last_n < -1
        || sampling.dry_allowed_length < 0
    {
        return Err(InferenceError::InvalidSampling(
            "an integer sampler window is outside its documented range",
        ));
    }
    if sampling.max_tokens == 0 || sampling.max_tokens > MAX_GENERATED_TOKENS {
        return Err(InferenceError::InvalidSampling(
            "max_tokens is outside the Loom evidence bound",
        ));
    }
    if sampling
        .sampler_order
        .iter()
        .enumerate()
        .any(|(index, sampler)| sampling.sampler_order[..index].contains(sampler))
    {
        return Err(InferenceError::InvalidSampling(
            "sampler_order contains a duplicate",
        ));
    }
    if sampling.stop.len() > MAX_STOP_SEQUENCES {
        return Err(InferenceError::InvalidSampling("too many stop sequences"));
    }
    let mut stop_bytes = 0usize;
    for stop in &sampling.stop {
        if stop.is_empty() {
            return Err(InferenceError::InvalidSampling(
                "stop sequences cannot be empty",
            ));
        }
        stop_bytes = stop_bytes
            .checked_add(stop.len())
            .ok_or(InferenceError::InvalidSampling("stop bytes overflow"))?;
    }
    if stop_bytes > MAX_STOP_SEQUENCE_BYTES {
        return Err(InferenceError::InvalidSampling(
            "aggregate stop bytes exceed the native bound",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use llama_native_types::{GenerationCacheMetrics, GenerationMetrics};
    use loom_research_types::{
        CampaignId, CompletionPromptTail, ExactPromptSource, FrozenBaseCompletionPrompt,
        NonEmptyByteRange, StageAttemptId, StageId, TrialCaseId, compile_manifest,
    };
    use loom_types::RevisionId;

    use super::*;
    use crate::{
        CheckedPersistedBatchFacts, PersistedBindingEvidenceRef, PersistedCaseOutcomeRef,
        PersistedEvidenceError, PersistedInferenceBatchRef, PersistedInferenceCaseRef,
        PersistedPromptEvidenceRef, verify_persisted_batch_evidence,
    };

    const GEMMA_SHA256: &str = "aa0a9a03993440f45176f19f8189a2e84c210ff8628ec13dc6edf42d017f7670";
    const GEMMA_BYTES: u64 = 4_954_576_032;

    #[derive(Debug)]
    struct FixtureSeal {
        request: GenerationBatchRequest,
        model_fingerprint: ModelFingerprint,
        outputs: Vec<GenerationOutput>,
        terminal_sampled_token_ids: Vec<Option<i32>>,
        events: Vec<GenerationEvent>,
    }

    impl SealView for FixtureSeal {
        fn request(&self) -> &GenerationBatchRequest {
            &self.request
        }

        fn model_fingerprint(&self) -> &ModelFingerprint {
            &self.model_fingerprint
        }

        fn outputs(&self) -> &[GenerationOutput] {
            &self.outputs
        }

        fn terminal_sampled_token_ids(&self) -> &[Option<i32>] {
            &self.terminal_sampled_token_ids
        }

        fn events(&self) -> &[GenerationEvent] {
            &self.events
        }
    }

    fn expect_admitted(outcome: VerifiedInferenceOutcome) -> crate::VerifiedInferenceEnvelope {
        match outcome {
            VerifiedInferenceOutcome::Admitted(envelope) => envelope,
            VerifiedInferenceOutcome::DiagnosticOnly(_) => panic!("expected admitted outcome"),
        }
    }

    fn fixture_model() -> ModelFingerprint {
        ModelFingerprint {
            model_id: "gemma-4-e2b-base-q8".to_owned(),
            model_size: GEMMA_BYTES,
            model_sha256: GEMMA_SHA256.to_owned(),
            tokenizer_sha256: GEMMA_SHA256.to_owned(),
            chat_template_sha256: "11".repeat(32),
            multimodal_projector_sha256: None,
            binding_version: "fixture-binding".to_owned(),
            build_id: "fixture-build".to_owned(),
            backend: "cpu".to_owned(),
            context_tokens: 64,
            batch_tokens: 32,
            max_sequences: 4,
            rope_config_sha256: "22".repeat(32),
            kv_layout_sha256: "33".repeat(32),
        }
    }

    fn fixture_binding() -> BaseWriterBinding {
        let source = format!(
            r#"format = "loom.model-bindings.v1"
name = "fixture-models"
description = "Fixture binding"

[[bindings]]
id = "writer"
role = "base_writer"
model_sha256 = "{GEMMA_SHA256}"
model_bytes = {GEMMA_BYTES}
tokenizer_sha256 = "{GEMMA_SHA256}"
architecture = "gemma4"
context_tokens = 64
capabilities = ["completion", "logits"]
adapters = []
"#
        );
        let manifest = compile_manifest(source.as_bytes()).expect("fixture manifest");
        BaseWriterBinding::compile(&manifest, "writer").expect("fixture binding")
    }

    fn fixture_prompt(
        project_id: ProjectId,
        scope: CallScope,
        exact_bytes: &[u8],
    ) -> CompiledBaseCompletionPrompt {
        let revision_id = RevisionId::from_str("01ARZ3NDEKTSV4RRFFQ69G5FAV")
            .expect("fixture revision ID is canonical");
        let tail = CompletionPromptTail::live_manuscript(
            revision_id,
            BlobId::digest(exact_bytes),
            NonEmptyByteRange::new(0, exact_bytes.len() as u64).expect("nonempty fixture tail"),
        )
        .expect("bounded fixture tail");
        FrozenBaseCompletionPrompt::new(
            project_id,
            scope,
            BlobId::digest(b"fixture-treatment"),
            Vec::new(),
            tail,
        )
        .expect("fixture prompt specification")
        .compile(
            exact_bytes.to_vec(),
            &[ExactPromptSource::new(revision_id, exact_bytes)],
        )
        .expect("exact fixture prompt")
    }

    fn fixture_material(
        project_id: ProjectId,
        scope: CallScope,
        prompt_tokens: &[i32],
    ) -> PreparedMaterial {
        fixture_material_with_model(project_id, scope, prompt_tokens, fixture_model())
    }

    fn fixture_material_with_model(
        project_id: ProjectId,
        scope: CallScope,
        prompt_tokens: &[i32],
        model_fingerprint: ModelFingerprint,
    ) -> PreparedMaterial {
        let exact_prompt_token_ids = prompt_tokens
            .iter()
            .map(|token| u32::try_from(*token).expect("fixture tokens are nonnegative"))
            .collect();
        PreparedMaterial {
            project_id,
            binding: fixture_binding(),
            prompt_evidence: ExactPromptEvidence::verified_completion_no_bos(
                fixture_prompt(project_id, scope, b"Once upon a verified time"),
                exact_prompt_token_ids,
            ),
            model_fingerprint_id: model_fingerprint_id(&model_fingerprint),
            tokenizer_fingerprint: BlobId::from_str(GEMMA_SHA256).expect("pinned SHA parses"),
            model_fingerprint,
        }
    }

    fn fixture_scope() -> CallScope {
        CallScope::new(
            CampaignId::new(),
            StageId::new(),
            StageAttemptId::new(),
            TrialCaseId::new(),
        )
    }

    fn sampling(seed: u32) -> SamplingConfig {
        SamplingConfig {
            seed,
            max_tokens: 8,
            ..SamplingConfig::default()
        }
    }

    fn pending_with(
        project_id: ProjectId,
        scope: CallScope,
        call_id: ModelCallId,
        sampling: SamplingConfig,
        prompt_tokens: &[i32],
    ) -> PendingBatch {
        let case = BaseWriterCaseSpec::new(call_id, sampling).expect("valid case");
        PendingBatch::compile(
            fixture_material(project_id, scope, prompt_tokens),
            vec![case],
        )
        .expect("valid pending batch")
    }

    fn output_for(
        pending: &PendingBatch,
        index: usize,
        state: GenerationState,
        finish_reason: &str,
        displayed: &str,
        generated_token_ids: &[i32],
    ) -> GenerationOutput {
        let case = &pending.cases[index];
        let prompt_tokens = pending.material.prompt_evidence.ordered_token_ids().len();
        let shared_prefix_tokens = if pending.cases.len() > 1 {
            prompt_tokens.saturating_sub(1)
        } else {
            0
        };
        GenerationOutput {
            request_id: pending.request.request_id.clone(),
            branch_id: case.native_case_id.clone(),
            input_index: index,
            model_id: pending.request.model_id.clone(),
            text: displayed.to_owned(),
            generated_token_ids: generated_token_ids.to_owned(),
            token_observations: None,
            state,
            finish_reason: finish_reason.to_owned(),
            metrics: GenerationMetrics {
                prompt_tokens,
                completion_tokens: generated_token_ids.len(),
                shared_prefix_tokens,
                duration_ms: 1,
                first_token_ms: Some(1),
                tokens_per_second: 1.0,
                cache: GenerationCacheMetrics {
                    batch_shared_prefix_tokens: shared_prefix_tokens,
                    ..GenerationCacheMetrics::default()
                },
            },
            real_engine_invoked: true,
            fake_fixture: false,
            transport: NativeTransport::InProcess,
        }
    }

    fn events_for(
        pending: &PendingBatch,
        index: usize,
        raw_output: &str,
        terminal: GenerationState,
    ) -> Vec<GenerationEvent> {
        let case = &pending.cases[index];
        let sequence_id = i32::try_from(index).expect("fixture index fits native sequence ID");
        let mut events = vec![
            GenerationEvent {
                request_id: pending.request.request_id.clone(),
                branch_id: case.native_case_id.clone(),
                sequence_id,
                input_index: index,
                event_index: 0,
                event: GenerationEventKind::State {
                    state: GenerationState::Prefilling,
                },
            },
            GenerationEvent {
                request_id: pending.request.request_id.clone(),
                branch_id: case.native_case_id.clone(),
                sequence_id,
                input_index: index,
                event_index: 1,
                event: GenerationEventKind::State {
                    state: GenerationState::Generating,
                },
            },
        ];
        if !raw_output.is_empty() {
            events.push(GenerationEvent {
                request_id: pending.request.request_id.clone(),
                branch_id: case.native_case_id.clone(),
                sequence_id,
                input_index: index,
                event_index: 2,
                event: GenerationEventKind::Delta {
                    text: raw_output.to_owned(),
                },
            });
        }
        events.push(GenerationEvent {
            request_id: pending.request.request_id.clone(),
            branch_id: case.native_case_id.clone(),
            sequence_id,
            input_index: index,
            event_index: events.len() as u64,
            event: GenerationEventKind::State { state: terminal },
        });
        events
    }

    fn completed_fixture(pending: &PendingBatch) -> FixtureSeal {
        FixtureSeal {
            request: pending.request.clone(),
            model_fingerprint: pending.material.model_fingerprint.clone(),
            outputs: vec![output_for(
                pending,
                0,
                GenerationState::Completed,
                "end_of_generation",
                " a model sentence",
                &[10, 11, 12],
            )],
            terminal_sampled_token_ids: vec![Some(99)],
            events: events_for(pending, 0, " a model sentence", GenerationState::Completed),
        }
    }

    fn replay_persisted_cases(
        envelope: &crate::VerifiedInferenceEnvelope,
        cases: &[PersistedInferenceCaseRef<'_>],
        backend_request_id: &str,
        verification_fingerprint: BlobId,
    ) -> Result<CheckedPersistedBatchFacts, PersistedEvidenceError> {
        let binding = envelope.binding();
        let prompt = envelope.prompt_evidence();
        let Some(first_case) = cases.first() else {
            return Err(PersistedEvidenceError::Batch("test replay omitted cases"));
        };
        let runtime_model_fingerprint = first_case.model_call.identity().model_fingerprint();
        verify_persisted_batch_evidence(&PersistedInferenceBatchRef {
            binding: PersistedBindingEvidenceRef {
                binding_id: binding.binding_id(),
                binding_fingerprint: binding.fingerprint(),
                model_sha256: binding.model_sha256(),
                model_byte_len: binding.model_bytes(),
                tokenizer_sha256: binding.tokenizer_sha256(),
                multimodal_projector_sha256: binding.multimodal_projector_sha256(),
                context_tokens: binding.context_tokens(),
            },
            prompt: PersistedPromptEvidenceRef {
                project_id: prompt.project_id(),
                scope: prompt.scope(),
                source_prompt_fingerprint: prompt.source_prompt_fingerprint(),
                content_fingerprint: prompt.content_fingerprint(),
                treatment_recipe_fingerprint: prompt.treatment_recipe_fingerprint(),
                raw_utf8: prompt.raw_utf8(),
                raw_blob_id: prompt.raw_blob_id(),
                form: prompt.form(),
                token_policy: prompt.token_policy(),
                ordered_token_ids: prompt.ordered_token_ids(),
                token_fingerprint: prompt.token_fingerprint(),
                compiled_fingerprint: prompt.compiled_fingerprint(),
            },
            runtime_model_fingerprint,
            backend_request_id,
            cases,
            verification_fingerprint,
        })
    }

    fn replay_single_persisted_case(
        envelope: &crate::VerifiedInferenceEnvelope,
        case: PersistedInferenceCaseRef<'_>,
        backend_request_id: &str,
        verification_fingerprint: BlobId,
    ) -> Result<CheckedPersistedBatchFacts, PersistedEvidenceError> {
        replay_persisted_cases(
            envelope,
            std::slice::from_ref(&case),
            backend_request_id,
            verification_fingerprint,
        )
    }

    fn assert_admitted_parts_round_trip(
        envelope: crate::VerifiedInferenceEnvelope,
        project_id: ProjectId,
        call_id: ModelCallId,
    ) {
        envelope.into_parts().consume(
            |parts_project,
             parts_binding,
             prompt_evidence,
             request_id,
             mut outcomes,
             envelope_fingerprint| {
                assert_eq!(parts_project, project_id);
                assert_eq!(parts_binding.binding_id(), "writer");
                assert_eq!(prompt_evidence.raw_utf8(), b"Once upon a verified time");
                assert_eq!(prompt_evidence.ordered_token_ids(), &[1, 2, 3]);
                assert_eq!(
                    prompt_evidence.raw_blob_id(),
                    BlobId::digest(b"Once upon a verified time")
                );
                assert!(request_id.starts_with("loom-base-v1-"));
                assert_eq!(outcomes.len(), 1);
                assert_ne!(envelope_fingerprint, BlobId::digest(b"unrelated"));
                let outcome = outcomes.pop().expect("one move-only outcome");
                outcome.consume(
                    |input_index, parts| {
                        assert_eq!(input_index, 0);
                        parts.consume(
                            |model_call,
                             raw_output,
                             token_ids,
                             event_json,
                             backend_audit_json,
                             displayed_output,
                             projection,
                             terminal,
                             verification_fingerprint| {
                                assert_eq!(model_call.id(), call_id);
                                assert_eq!(raw_output, b" a model sentence");
                                assert_eq!(token_ids, vec![10, 11, 12]);
                                assert!(!event_json.is_empty());
                                assert!(!backend_audit_json.is_empty());
                                assert_eq!(displayed_output, b" a model sentence");
                                assert!(projection.is_some());
                                assert_eq!(terminal, Some(99));
                                assert_ne!(verification_fingerprint, BlobId::digest(b"unrelated"));
                            },
                        );
                    },
                    |_, _| panic!("completed fixture became cancelled"),
                );
            },
        );
    }

    #[test]
    fn matching_private_seal_mints_exact_live_call() {
        let project_id = ProjectId::new();
        let call_id = ModelCallId::new();
        let pending = pending_with(
            project_id,
            fixture_scope(),
            call_id,
            sampling(7),
            &[1, 2, 3],
        );
        let seal = completed_fixture(&pending);

        let envelope = expect_admitted(mint_envelope(pending, &seal).expect("seal must verify"));

        assert_eq!(envelope.project_id(), project_id);
        assert_eq!(envelope.binding().binding_id(), "writer");
        assert_eq!(
            envelope.prompt_evidence().raw_utf8(),
            b"Once upon a verified time"
        );
        assert_eq!(envelope.prompt_evidence().ordered_token_ids(), &[1, 2, 3]);
        assert_eq!(envelope.completed_calls().count(), 1);
        assert_eq!(envelope.cancelled_diagnostics().count(), 0);
        let call = envelope.completed_calls().next().expect("one call");
        assert_eq!(call.model_call().id(), call_id);
        assert_eq!(
            call.model_call().evidence_class(),
            CallEvidenceClass::LiveBaseWriterClaim
        );
        assert_eq!(
            call.model_call().identity().scope(),
            envelope.prompt_evidence().scope()
        );
        assert_eq!(call.raw_output(), b" a model sentence");
        assert_eq!(call.displayed_output(), b" a model sentence");
        assert_eq!(call.generated_token_ids(), &[10, 11, 12]);
        assert!(call.output_projection().is_some());
        assert_eq!(call.token_boundaries_fingerprint(), None);
        assert_eq!(call.terminal_sampled_token_id(), Some(99));
        let audit: serde_json::Value =
            serde_json::from_slice(call.backend_audit_json()).expect("audit is exact JSON");
        assert_eq!(audit["format"], BACKEND_AUDIT_FORMAT);
        assert_eq!(audit["sampling"]["seed"], 7);
        assert_eq!(audit["output"]["raw_output_byte_len"], 17);
        assert_eq!(audit["output"]["generated_token_count"], 3);
        assert!(audit.get("request").is_none());
        assert!(audit["output"].get("text").is_none());
        assert!(audit["output"].get("generated_token_ids").is_none());
        assert!(
            !std::str::from_utf8(call.backend_audit_json())
                .expect("audit is UTF-8 JSON")
                .contains("/models/gemma-4-e2b-base-q8.gguf")
        );
        assert!(
            !std::str::from_utf8(call.backend_audit_json())
                .expect("audit is UTF-8 JSON")
                .contains("gemma-4-e2b-base-q8")
        );
        assert!(
            std::str::from_utf8(call.event_json())
                .expect("event record is UTF-8 JSON")
                .contains(EVENT_LEDGER_FORMAT)
        );
        call.model_call()
            .completed()
            .expect("completed record")
            .token_evidence()
            .verify(call.generated_token_ids())
            .expect("exact token evidence");

        assert_admitted_parts_round_trip(envelope, project_id, call_id);
    }

    #[test]
    fn persisted_batch_replay_checks_the_exact_live_receipt_graph() {
        let pending = pending_with(
            ProjectId::new(),
            fixture_scope(),
            ModelCallId::new(),
            sampling(7),
            &[1, 2, 3],
        );
        let seal = completed_fixture(&pending);
        let envelope = expect_admitted(mint_envelope(pending, &seal).expect("live fixture"));
        let call = envelope
            .completed_calls()
            .next()
            .expect("one completed call");
        let case = PersistedInferenceCaseRef {
            input_index: 0,
            model_call: call.model_call(),
            raw_output: call.raw_output(),
            generated_token_ids: call.generated_token_ids(),
            event_json: call.event_json(),
            backend_audit_json: call.backend_audit_json(),
            terminal_sampled_token_id: call.terminal_sampled_token_id(),
            outcome: PersistedCaseOutcomeRef::Completed {
                displayed_output: call.displayed_output(),
                output_projection: call.output_projection(),
            },
            verification_fingerprint: call.verification_fingerprint(),
        };

        let checked = replay_single_persisted_case(
            &envelope,
            case,
            envelope.backend_request_id(),
            envelope.verification_fingerprint(),
        )
        .expect("exact persisted evidence replays");
        assert_eq!(checked.request_id(), envelope.backend_request_id());
        assert_eq!(checked.completed_case_count(), 1);
        assert_eq!(checked.cancelled_case_count(), 0);
        assert_eq!(checked.cases().len(), 1);
        assert_eq!(
            checked.cases()[0].raw_output_blob_id(),
            BlobId::digest(call.raw_output())
        );
        assert_eq!(
            checked.cases()[0].backend_receipt_blob_id(),
            BlobId::digest(call.backend_audit_json())
        );
    }

    #[test]
    fn persisted_batch_replay_rejects_malformed_truncated_and_extra_json() {
        let make_envelope = || {
            let pending = pending_with(
                ProjectId::new(),
                fixture_scope(),
                ModelCallId::new(),
                sampling(7),
                &[1, 2, 3],
            );
            let seal = completed_fixture(&pending);
            expect_admitted(mint_envelope(pending, &seal).expect("live fixture"))
        };
        let envelope = make_envelope();
        let call = envelope
            .completed_calls()
            .next()
            .expect("one completed call");

        let mut extra_audit =
            call.backend_audit_json()[..call.backend_audit_json().len() - 1].to_vec();
        extra_audit.extend_from_slice(b",\"unexpected\":true}");
        let mut noncompact_audit = call.backend_audit_json().to_vec();
        noncompact_audit.push(b' ');
        let mut extra_events = call.event_json()[..call.event_json().len() - 1].to_vec();
        extra_events.extend_from_slice(b",\"unexpected\":true}");
        let audit_variants = [
            &call.backend_audit_json()[..call.backend_audit_json().len() - 1],
            extra_audit.as_slice(),
            noncompact_audit.as_slice(),
        ];
        for backend_audit_json in audit_variants {
            let case = PersistedInferenceCaseRef {
                input_index: 0,
                model_call: call.model_call(),
                raw_output: call.raw_output(),
                generated_token_ids: call.generated_token_ids(),
                event_json: call.event_json(),
                backend_audit_json,
                terminal_sampled_token_id: call.terminal_sampled_token_id(),
                outcome: PersistedCaseOutcomeRef::Completed {
                    displayed_output: call.displayed_output(),
                    output_projection: call.output_projection(),
                },
                verification_fingerprint: call.verification_fingerprint(),
            };
            assert!(
                replay_single_persisted_case(
                    &envelope,
                    case,
                    envelope.backend_request_id(),
                    envelope.verification_fingerprint(),
                )
                .is_err()
            );
        }
        let event_variants = [
            &call.event_json()[..call.event_json().len() - 1],
            extra_events.as_slice(),
        ];
        for event_json in event_variants {
            let case = PersistedInferenceCaseRef {
                input_index: 0,
                model_call: call.model_call(),
                raw_output: call.raw_output(),
                generated_token_ids: call.generated_token_ids(),
                event_json,
                backend_audit_json: call.backend_audit_json(),
                terminal_sampled_token_id: call.terminal_sampled_token_id(),
                outcome: PersistedCaseOutcomeRef::Completed {
                    displayed_output: call.displayed_output(),
                    output_projection: call.output_projection(),
                },
                verification_fingerprint: call.verification_fingerprint(),
            };
            assert!(
                replay_single_persisted_case(
                    &envelope,
                    case,
                    envelope.backend_request_id(),
                    envelope.verification_fingerprint(),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn seal_request_and_model_substitution_fail_closed() {
        let make_pending = || {
            pending_with(
                ProjectId::new(),
                fixture_scope(),
                ModelCallId::new(),
                sampling(7),
                &[1, 2, 3],
            )
        };

        let pending = make_pending();
        let mut seal = completed_fixture(&pending);
        seal.request.cases[0].sampling.seed = 8;
        assert!(matches!(
            mint_envelope(pending, &seal),
            Err(InferenceError::SealRequestMismatch)
        ));

        let pending = make_pending();
        let mut seal = completed_fixture(&pending);
        seal.model_fingerprint.build_id.push_str("-altered");
        assert!(matches!(
            mint_envelope(pending, &seal),
            Err(InferenceError::SealModelMismatch)
        ));
    }

    #[test]
    fn output_and_event_tampering_fail_closed() {
        let make_pending = || {
            pending_with(
                ProjectId::new(),
                fixture_scope(),
                ModelCallId::new(),
                sampling(7),
                &[1, 2, 3],
            )
        };

        let pending = make_pending();
        let mut seal = completed_fixture(&pending);
        seal.outputs[0].fake_fixture = true;
        assert!(matches!(
            mint_envelope(pending, &seal),
            Err(InferenceError::OutputNotLive { index: 0 })
        ));

        let pending = make_pending();
        let mut seal = completed_fixture(&pending);
        seal.events[2].event = GenerationEventKind::Delta {
            text: " inserted prose".to_owned(),
        };
        assert!(matches!(
            mint_envelope(pending, &seal),
            Err(InferenceError::OutputProjectionMismatch { index: 0 })
        ));

        let pending = make_pending();
        let mut seal = completed_fixture(&pending);
        seal.events[1].branch_id.push_str("-other");
        assert!(matches!(
            mint_envelope(pending, &seal),
            Err(InferenceError::MalformedEventLedger { index: 0 })
        ));
    }

    #[test]
    fn stop_suffix_stays_in_raw_evidence_but_not_displayed_projection() {
        let sampling = SamplingConfig {
            seed: 9,
            max_tokens: 8,
            stop: vec!["<END>".to_owned()],
            ..SamplingConfig::default()
        };
        let pending = pending_with(
            ProjectId::new(),
            fixture_scope(),
            ModelCallId::new(),
            sampling,
            &[1, 2, 3],
        );
        let seal = FixtureSeal {
            request: pending.request.clone(),
            model_fingerprint: pending.material.model_fingerprint.clone(),
            outputs: vec![output_for(
                &pending,
                0,
                GenerationState::Completed,
                "stop_sequence",
                " prose",
                &[10, 11],
            )],
            terminal_sampled_token_ids: vec![None],
            events: events_for(&pending, 0, " prose<END>", GenerationState::Completed),
        };

        let envelope = expect_admitted(mint_envelope(pending, &seal).expect("stop seal verifies"));
        let call = envelope.completed_calls().next().expect("completed call");
        assert_eq!(call.raw_output(), b" prose<END>");
        assert_eq!(call.displayed_output(), b" prose");
        let projection = call.output_projection().expect("nonempty projection");
        assert_eq!(projection.displayed().end(), 6);
        assert_eq!(projection.trimmed_stop_suffix().start(), 6);
        assert_eq!(projection.trimmed_stop_suffix().end(), 11);
    }

    #[test]
    fn every_public_native_debug_view_is_content_redacted() {
        const PROMPT_SECRET: &str = "PRIVATE_PROMPT_/private/models/writer.gguf";
        const OUTPUT_SECRET: &str = " PRIVATE_OUTPUT";
        const STOP_SECRET: &str = "<PRIVATE_STOP>";

        let sampling = SamplingConfig {
            seed: 9,
            max_tokens: 8,
            stop: vec![STOP_SECRET.to_owned()],
            ..SamplingConfig::default()
        };
        let scope = fixture_scope();
        let case = BaseWriterCaseSpec::new(ModelCallId::new(), sampling).expect("valid case");
        let case_debug = format!("{case:?}");
        let project_id = ProjectId::new();
        let mut material = fixture_material(project_id, scope, &[1, 2, 3]);
        material.prompt_evidence = ExactPromptEvidence::verified_completion_no_bos(
            fixture_prompt(project_id, scope, PROMPT_SECRET.as_bytes()),
            vec![1, 2, 3],
        );
        let pending = PendingBatch::compile(material, vec![case]).expect("pending batch");
        let seal = FixtureSeal {
            request: pending.request.clone(),
            model_fingerprint: pending.material.model_fingerprint.clone(),
            outputs: vec![output_for(
                &pending,
                0,
                GenerationState::Completed,
                "stop_sequence",
                OUTPUT_SECRET,
                &[41, 42],
            )],
            terminal_sampled_token_ids: vec![None],
            events: events_for(
                &pending,
                0,
                &format!("{OUTPUT_SECRET}{STOP_SECRET}"),
                GenerationState::Completed,
            ),
        };

        let outcome = mint_envelope(pending, &seal).expect("verified outcome");
        let outcome_debug = format!("{outcome:?}");
        let envelope = expect_admitted(outcome);
        let call_debug = format!(
            "{:?}",
            envelope.completed_calls().next().expect("completed call")
        );
        let parts = envelope.into_parts();
        let parts_debug = format!("{parts:?}");
        let sampling_error_debug = format!("{:?}", InferenceError::InvalidSampling(STOP_SECRET));
        let json_error = serde_json::from_slice::<serde_json::Value>(b"{PRIVATE_JSON")
            .expect_err("malformed private JSON");
        let json_error_debug = format!("{:?}", InferenceError::Json(json_error));

        for rendered in [
            case_debug,
            outcome_debug,
            call_debug,
            parts_debug,
            sampling_error_debug,
            json_error_debug,
        ] {
            for secret in [
                PROMPT_SECRET,
                OUTPUT_SECRET,
                STOP_SECRET,
                "/private/models/writer.gguf",
                "[41, 42]",
                BACKEND_AUDIT_FORMAT,
                EVENT_LEDGER_FORMAT,
                "PRIVATE_JSON",
            ] {
                assert!(!rendered.contains(secret), "Debug leaked {secret}");
            }
        }
    }

    #[test]
    fn cancelled_sibling_remains_diagnostic_only() {
        let project_id = ProjectId::new();
        let scope = fixture_scope();
        let material = fixture_material(project_id, scope, &[1, 2, 3]);
        let first = BaseWriterCaseSpec::new(ModelCallId::new(), sampling(7)).expect("first case");
        let second = BaseWriterCaseSpec::new(ModelCallId::new(), sampling(8)).expect("second case");
        let pending = PendingBatch::compile(material, vec![first, second]).expect("batch");
        let mut events = events_for(&pending, 0, " finished", GenerationState::Completed);
        events.extend(events_for(
            &pending,
            1,
            " partial",
            GenerationState::Cancelled,
        ));
        let seal = FixtureSeal {
            request: pending.request.clone(),
            model_fingerprint: pending.material.model_fingerprint.clone(),
            outputs: vec![
                output_for(
                    &pending,
                    0,
                    GenerationState::Completed,
                    "end_of_generation",
                    " finished",
                    &[10],
                ),
                output_for(
                    &pending,
                    1,
                    GenerationState::Cancelled,
                    "cancelled",
                    " partial",
                    &[11],
                ),
            ],
            terminal_sampled_token_ids: vec![Some(99), None],
            events,
        };

        let envelope =
            expect_admitted(mint_envelope(pending, &seal).expect("mixed batch verifies"));
        assert_eq!(envelope.completed_calls().count(), 1);
        assert_eq!(envelope.cancelled_diagnostics().count(), 1);
        let completed = envelope
            .completed_calls()
            .next()
            .expect("one completed call");
        let diagnostic = envelope
            .cancelled_diagnostics()
            .next()
            .expect("one diagnostic");
        assert!(matches!(
            diagnostic.model_call().terminal(),
            CallTerminal::Cancelled { .. }
        ));
        assert_eq!(diagnostic.partial_raw_output(), b" partial");
        assert_eq!(envelope.outcomes()[0].input_index(), 0);
        assert!(envelope.outcomes()[0].completed_call().is_some());
        assert_eq!(envelope.outcomes()[1].input_index(), 1);
        assert!(envelope.outcomes()[1].cancelled_diagnostic().is_some());

        let replay_cases = [
            PersistedInferenceCaseRef {
                input_index: 0,
                model_call: completed.model_call(),
                raw_output: completed.raw_output(),
                generated_token_ids: completed.generated_token_ids(),
                event_json: completed.event_json(),
                backend_audit_json: completed.backend_audit_json(),
                terminal_sampled_token_id: completed.terminal_sampled_token_id(),
                outcome: PersistedCaseOutcomeRef::Completed {
                    displayed_output: completed.displayed_output(),
                    output_projection: completed.output_projection(),
                },
                verification_fingerprint: completed.verification_fingerprint(),
            },
            PersistedInferenceCaseRef {
                input_index: 1,
                model_call: diagnostic.model_call(),
                raw_output: diagnostic.partial_raw_output(),
                generated_token_ids: diagnostic.generated_token_ids(),
                event_json: diagnostic.event_json(),
                backend_audit_json: diagnostic.backend_audit_json(),
                terminal_sampled_token_id: None,
                outcome: PersistedCaseOutcomeRef::Cancelled,
                verification_fingerprint: diagnostic.verification_fingerprint(),
            },
        ];
        let checked = replay_persisted_cases(
            &envelope,
            &replay_cases,
            envelope.backend_request_id(),
            envelope.verification_fingerprint(),
        )
        .expect("producer-minted mixed batch replays");
        assert_eq!(checked.completed_case_count(), 1);
        assert_eq!(checked.cancelled_case_count(), 1);
    }

    #[test]
    fn all_cancelled_batch_is_verified_diagnostic_only() {
        let pending = pending_with(
            ProjectId::new(),
            fixture_scope(),
            ModelCallId::new(),
            sampling(7),
            &[1, 2, 3],
        );
        let seal = FixtureSeal {
            request: pending.request.clone(),
            model_fingerprint: pending.material.model_fingerprint.clone(),
            outputs: vec![output_for(
                &pending,
                0,
                GenerationState::Cancelled,
                "cancelled",
                " partial",
                &[10],
            )],
            terminal_sampled_token_ids: vec![None],
            events: events_for(&pending, 0, " partial", GenerationState::Cancelled),
        };

        let outcome = mint_envelope(pending, &seal).expect("cancelled seal verifies");
        let VerifiedInferenceOutcome::DiagnosticOnly(envelope) = outcome else {
            panic!("all-cancelled batch gained admitted authority");
        };
        assert_eq!(envelope.outcomes().len(), 1);
        assert_eq!(envelope.outcomes()[0].input_index(), 0);
        assert!(envelope.outcomes()[0].completed_call().is_none());
        assert_eq!(envelope.cancelled_diagnostics().count(), 1);
        assert_eq!(
            envelope.prompt_evidence().raw_utf8(),
            b"Once upon a verified time"
        );
        envelope
            .into_parts()
            .consume(|_, binding, prompt, _, mut outcomes, _| {
                assert_eq!(binding.binding_id(), "writer");
                assert_eq!(prompt.ordered_token_ids(), &[1, 2, 3]);
                let outcome = outcomes.pop().expect("one diagnostic outcome");
                assert!(outcomes.is_empty());
                outcome.consume(
                    |_, _| panic!("diagnostic-only parts exposed a completed call"),
                    |input_index, diagnostic| {
                        assert_eq!(input_index, 0);
                        diagnostic.consume(
                            |model_call, partial, tokens, event_json, audit_json, _| {
                                assert!(matches!(
                                    model_call.terminal(),
                                    CallTerminal::Cancelled { .. }
                                ));
                                assert_eq!(partial, b" partial");
                                assert_eq!(tokens, vec![10]);
                                assert!(!event_json.is_empty());
                                assert!(!audit_json.is_empty());
                            },
                        );
                    },
                );
            });
    }

    #[test]
    fn compiled_prompt_identity_binds_policy_and_prepared_tokens() {
        let bytes = b"the same raw prompt";
        let project_id = ProjectId::new();
        let scope = fixture_scope();
        let baseline = ExactPromptEvidence::verified_completion_no_bos(
            fixture_prompt(project_id, scope, bytes),
            vec![1, 2, 3],
        );
        let altered_tokens = ExactPromptEvidence::verified_completion_no_bos(
            fixture_prompt(project_id, scope, bytes),
            vec![1, 2, 4],
        );
        assert_ne!(
            baseline.compiled_fingerprint(),
            altered_tokens.compiled_fingerprint()
        );
        assert_eq!(baseline.form(), crate::PromptFormEvidence::Completion);
        assert_eq!(
            baseline.token_policy(),
            crate::PromptTokenPolicyEvidence::NoBosParseSpecial
        );

        let invalid_policy = PreparedPrompt {
            input_index: 0,
            prompt_form: PromptForm::Completion,
            token_policy: PromptTokenPolicy::AddBosParseSpecial,
            source_sha256: BlobId::digest(bytes).to_hex(),
            token_ids: vec![1, 2, 3],
        };
        assert!(matches!(
            validate_prepared_prompt(&invalid_policy, bytes),
            Err(InferenceError::PreparedPromptSemantics)
        ));
    }

    #[test]
    fn semantic_model_identity_is_resident_id_independent() {
        let first = fixture_model();
        let mut relocated = first.clone();
        relocated.model_id = "private-filename-stem".to_owned();

        assert_eq!(
            model_fingerprint_id(&first),
            model_fingerprint_id(&relocated)
        );
        let sanitized = serde_json::to_string(&SanitizedModelFingerprint::from(&relocated))
            .expect("sanitized fingerprint serializes");
        assert!(!sanitized.contains("private-filename-stem"));
        assert!(sanitized.contains(SANITIZED_MODEL_FINGERPRINT_FORMAT));

        let scope = fixture_scope();
        let case =
            BaseWriterCaseSpec::new(ModelCallId::new(), sampling(7)).expect("renamed model case");
        let pending = PendingBatch::compile(
            fixture_material_with_model(ProjectId::new(), scope, &[1, 2, 3], relocated),
            vec![case],
        )
        .expect("renamed model pending batch");
        let seal = completed_fixture(&pending);
        let envelope =
            expect_admitted(mint_envelope(pending, &seal).expect("renamed model seal verifies"));
        let audit = std::str::from_utf8(
            envelope
                .completed_calls()
                .next()
                .expect("completed call")
                .backend_audit_json(),
        )
        .expect("audit is UTF-8");
        assert!(!audit.contains("private-filename-stem"));
    }

    #[test]
    fn request_identity_binds_project_scope_call_prompt_and_sampler() {
        let project = ProjectId::new();
        let scope = fixture_scope();
        let call = ModelCallId::new();
        let baseline = pending_with(project, scope, call, sampling(7), &[1, 2, 3]);
        let baseline_id = baseline.request.request_id;

        let different_project =
            pending_with(ProjectId::new(), scope, call, sampling(7), &[1, 2, 3]);
        let different_scope = pending_with(project, fixture_scope(), call, sampling(7), &[1, 2, 3]);
        let different_call =
            pending_with(project, scope, ModelCallId::new(), sampling(7), &[1, 2, 3]);
        let different_prompt = pending_with(project, scope, call, sampling(7), &[1, 2, 4]);
        let different_sampler = pending_with(project, scope, call, sampling(8), &[1, 2, 3]);

        for changed in [
            different_project,
            different_scope,
            different_call,
            different_prompt,
            different_sampler,
        ] {
            assert_ne!(baseline_id, changed.request.request_id);
        }
    }

    #[test]
    fn invalid_or_ambiguous_case_specs_are_rejected_before_native_work() {
        let default_seed = BaseWriterCaseSpec::new(ModelCallId::new(), SamplingConfig::default());
        assert!(matches!(default_seed, Err(InferenceError::DefaultSeed)));

        let call_id = ModelCallId::new();
        let scope = fixture_scope();
        let first = BaseWriterCaseSpec::new(call_id, sampling(1)).expect("case");
        let second = BaseWriterCaseSpec::new(call_id, sampling(2)).expect("case");
        let duplicate = PendingBatch::compile(
            fixture_material(ProjectId::new(), scope, &[1, 2, 3]),
            vec![first, second],
        );
        assert!(matches!(
            duplicate,
            Err(InferenceError::DuplicateCallId(value)) if value == call_id
        ));
    }

    #[test]
    fn producer_rejects_serialized_evidence_beyond_the_admission_bound() {
        let value = serde_json::json!({"evidence": "bounded"});
        let exact = serde_json::to_vec(&value).expect("fixture serializes");
        assert_eq!(
            serialize_evidence_with_limit(&value, "fixture", exact.len()).expect("exact boundary"),
            exact
        );
        assert!(matches!(
            serialize_evidence_with_limit(&value, "fixture", exact.len() - 1),
            Err(InferenceError::EvidenceTooLarge {
                kind: "fixture",
                actual,
                maximum,
            }) if actual == exact.len() && maximum + 1 == exact.len()
        ));
    }
}
