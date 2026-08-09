//! Owner-worker-sealed controlled base-writer inference and exact-token embeddings.
//!
//! Serializable native requests and outputs are retained as diagnostic bytes,
//! but never accepted as authority. Strict admission consumes the opaque native
//! owner-worker seal immediately. The seal remains available as opaque lineage
//! for optional binding to the one eventual joined worker at campaign teardown.

use std::{collections::HashSet, fmt, str::FromStr};

use llama_native_engine::{
    ControlledGenerationSubmission, ControlledGenerationTicket, EmbeddingTicket, JoinedNativeModel,
    LLAMA_NATIVE_BUILD_MANIFEST_SHA256, NativeModelHandle, VerifiedControlledGenerationBatch,
    VerifiedControlledGenerationTerminal, VerifiedEmbeddingBatch, VerifiedEmbeddingTerminal,
};
use llama_native_types::{
    ControlProgram, ControlledGenerationBatchOutput, ControlledGenerationBatchRequest,
    ControlledGenerationCase, DistributionObservationPolicy, EmbeddingBatchRequest, EmbeddingInput,
    EmbeddingNormalization, EmbeddingPooling, ExactTokenPrompt, ExtendedSamplerProgram,
    GenerationEvent, GenerationEventKind, GenerationMetrics, GenerationOutput, GenerationState,
    MAX_EMBEDDING_BATCH_INPUTS, MAX_EMBEDDING_BATCH_TOKENS, MAX_EMBEDDING_BATCH_VALUES,
    MAX_EMBEDDING_DIMENSIONS, MAX_EMBEDDING_INPUT_TOKENS, NativeError, NativeTransport,
    SamplingConfig, StructuredConstraint, TerminalSelector,
};
use loom_research_types::{
    BoundError, CallError, CallEvidenceClass, CallIdentity, CallScope, CallTerminal,
    CompiledBaseCompletionPrompt, CompletedCall, ModelCall, ModelCallId, OutputProjection,
};
use loom_types::{BlobId, ProjectId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    BaseWriterBinding, ControlledBaseWriterBackend, ExactEmbeddingBackend, ExactPromptEvidence,
    VerifiedBaseWriterCall, VerifiedCaseOutcome, VerifiedInferenceOutcome,
    VerifiedRuntimeChargeEvidence,
    canonical::{model_fingerprint_id, sampling_fingerprint},
    native_llama::{
        InferenceError, NativeLlamaWriter, PreparedBaseCompletion, PreparedMaterial,
        serialize_bounded_evidence, token_ids_blob_id, validate_sampling,
    },
    persisted::{
        BatchCaseCommitment, BatchCommitment, CallVerificationCommitment,
        ControlledRequestCaseCommitment, ControlledRequestCommitment,
        derive_batch_verification_fingerprint, derive_call_verification_fingerprint,
        derive_controlled_request_id,
    },
};

const CONTROLLED_AUDIT_FORMAT: &str = "loom.native-controlled-base-writer-audit.v1";
const EVENT_LEDGER_FORMAT: &str = "loom.native-base-writer-event-ledger.v1";
const OUTPUT_AUDIT_FORMAT: &str = "loom.native-base-writer-output-audit.v1";
const EMBEDDING_REQUEST_DOMAIN: &str = "loom/native-exact-embedding-request/v1";
const EMBEDDING_DIAGNOSTIC_RECORD_DOMAIN: &str = "loom/native-exact-embedding-diagnostic-record/v1";
const EMBEDDING_DIAGNOSTIC_RECORD_FORMAT: &str = "loom.native-exact-embedding-diagnostic.v1";
const MAX_EMBEDDING_INPUT_ID_BYTES: usize = 256;

/// Explicitly unavailable hidden-state intervention families.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnavailableHiddenIntervention {
    JSpaceProjection,
    KSpaceKvEditing,
}

/// Errors at the controlled/embedding authority boundary.
#[derive(Error)]
pub enum ControlledInferenceError {
    #[error(transparent)]
    Native(#[from] NativeError),
    #[error(transparent)]
    Baseline(#[from] InferenceError),
    #[error(transparent)]
    Call(#[from] CallError),
    #[error(transparent)]
    Bound(#[from] BoundError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("controlled program writer identity does not match the live resident worker")]
    WriterIdentityMismatch,
    #[error("this resident worker does not implement multi-model logit arithmetic")]
    MultiModelUnavailable,
    #[error("this resident worker does not implement static adapter or activation profiles")]
    StaticProfileUnavailable,
    #[error("{0:?} is unavailable because upstream exposes no required operation")]
    HiddenInterventionUnavailable(UnavailableHiddenIntervention),
    #[error("controlled constraint body is absent or disagrees with its immutable reference")]
    ConstraintAttachmentMismatch,
    #[error("controlled case prompt provenance disagrees with the conditional prompt")]
    UnconditionalPromptScopeMismatch,
    #[error("controlled batch is empty, repeats a call ID, or exceeds the Loom case bound")]
    InvalidCaseSet,
    #[error("native controlled seal does not match the frozen request, model, or case order")]
    ControlledSealMismatch,
    #[error("native controlled output or event evidence is malformed at case {0}")]
    ControlledOutputMismatch(usize),
    #[error("controlled lineage belongs to a different joined owner worker")]
    JoinedWorkerMismatch,
    #[error("controlled ticket or sealed result was already consumed")]
    AlreadyConsumed,
    #[error("controlled evidence violated an internal admission invariant")]
    AdmissionInvariant,
    #[error("embedding request has no exact inputs or contains an invalid token ID")]
    InvalidEmbeddingInput,
    #[error("native embedding seal does not match the exact request or model binding")]
    EmbeddingSealMismatch,
}

impl fmt::Debug for ControlledInferenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

/// Errors while encoding or replay-checking diagnostic embedding bytes.
///
/// Replay validates canonical form and internal consistency only. It never
/// recreates the consumed native seal, source authority, a learning-dataset
/// lease, or ranker activation authority.
#[derive(Error)]
pub enum EmbeddingDiagnosticRecordError {
    #[error("embedding diagnostic JSON is malformed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("embedding diagnostic bytes are empty or exceed the evidence bound")]
    EmptyOrOversized,
    #[error("embedding diagnostic JSON is not canonical compact JSON")]
    NonCanonical,
    #[error("embedding diagnostic invariant failed: {0}")]
    Invalid(&'static str),
}

impl fmt::Debug for EmbeddingDiagnosticRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

/// One controlled generation case. The optional unconditional prompt is a
/// separately source-compiled completion prompt and is required exactly when
/// same-model CFG is active.
pub struct ControlledBaseWriterCaseSpec {
    call_id: ModelCallId,
    sampling: SamplingConfig,
    sampler_fingerprint: BlobId,
    unconditional_prompt: Option<CompiledBaseCompletionPrompt>,
}

impl fmt::Debug for ControlledBaseWriterCaseSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControlledBaseWriterCaseSpec")
            .field("call_id", &self.call_id)
            .field("sampler_fingerprint", &self.sampler_fingerprint)
            .field(
                "has_unconditional_prompt",
                &self.unconditional_prompt.is_some(),
            )
            .finish_non_exhaustive()
    }
}

impl ControlledBaseWriterCaseSpec {
    pub fn new(
        call_id: ModelCallId,
        sampling: SamplingConfig,
        unconditional_prompt: Option<CompiledBaseCompletionPrompt>,
    ) -> Result<Self, ControlledInferenceError> {
        validate_sampling(&sampling)?;
        let sampler_fingerprint = sampling_fingerprint(&sampling);
        Ok(Self {
            call_id,
            sampling,
            sampler_fingerprint,
            unconditional_prompt,
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

/// A control program bound to the live writer identity. The constraint body is
/// ephemeral and content-checked again by native queue admission.
///
/// ```compile_fail
/// use loom_inference::native_controlled::NativeControlProgram;
/// fn duplicate(value: NativeControlProgram) { let _ = value.clone(); }
/// ```
pub struct NativeControlProgram {
    program: ControlProgram,
    constraint_body: Option<String>,
    fingerprint: BlobId,
}

impl fmt::Debug for NativeControlProgram {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeControlProgram")
            .field("fingerprint", &self.fingerprint)
            .field("has_constraint", &self.program.constraint().is_some())
            .field("guidance_count", &self.program.guidance().len())
            .field(
                "extended_sampler_count",
                &self.program.extended_samplers().as_slice().len(),
            )
            .finish_non_exhaustive()
    }
}

impl NativeControlProgram {
    pub fn bind(
        writer: &NativeLlamaWriter,
        program: ControlProgram,
        constraint_body: Option<String>,
    ) -> Result<Self, ControlledInferenceError> {
        let live_identity = writer
            .handle
            .controlled_model_identity(program.writer().participant_id())?;
        if program.writer() != &live_identity {
            return Err(ControlledInferenceError::WriterIdentityMismatch);
        }
        if !program.auxiliary_models().is_empty() {
            return Err(ControlledInferenceError::MultiModelUnavailable);
        }
        if !program.static_profiles().is_empty() {
            return Err(ControlledInferenceError::StaticProfileUnavailable);
        }
        validate_constraint_attachment(program.constraint(), constraint_body.as_deref())?;
        let fingerprint = parse_sha256(&program.fingerprint_sha256())?;
        Ok(Self {
            program,
            constraint_body,
            fingerprint,
        })
    }

    pub fn disabled(
        writer: &NativeLlamaWriter,
        participant_id: &str,
    ) -> Result<Self, ControlledInferenceError> {
        let identity = writer
            .handle
            .controlled_model_identity(participant_id.to_string())?;
        let program = ControlProgram::new(
            identity,
            Vec::new(),
            None,
            Vec::new(),
            ExtendedSamplerProgram::default(),
            TerminalSelector::Distribution,
            DistributionObservationPolicy::default(),
            Vec::new(),
        )?;
        Self::bind(writer, program, None)
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }
}

fn validate_constraint_attachment(
    constraint: Option<&StructuredConstraint>,
    body: Option<&str>,
) -> Result<(), ControlledInferenceError> {
    match (constraint, body) {
        (None, None) => Ok(()),
        (Some(constraint), Some(body)) => {
            let reference = constraint.reference();
            if body.is_empty()
                || body.len() != reference.byte_len() as usize
                || BlobId::digest(body.as_bytes()).to_hex() != reference.sha256()
            {
                return Err(ControlledInferenceError::ConstraintAttachmentMismatch);
            }
            Ok(())
        }
        (None, Some(_)) | (Some(_), None) => {
            Err(ControlledInferenceError::ConstraintAttachmentMismatch)
        }
    }
}

impl NativeLlamaWriter {
    /// Inspect the exact upstream-controlled feature declaration. The returned
    /// declaration explicitly includes J-space projection and K-space/KV
    /// editing as unavailable because upstream exposes neither operation.
    pub fn controlled_generation_capabilities(
        &self,
    ) -> llama_native_types::ControlledGenerationCapabilities {
        self.handle.controlled_generation_capabilities()
    }

    pub fn controlled_model_identity(
        &self,
        participant_id: &str,
    ) -> Result<llama_native_types::ControlledModelIdentity, ControlledInferenceError> {
        self.handle
            .controlled_model_identity(participant_id.to_string())
            .map_err(Into::into)
    }

    pub fn require_hidden_intervention(
        &self,
        intervention: UnavailableHiddenIntervention,
    ) -> Result<(), ControlledInferenceError> {
        let _ = self;
        Err(ControlledInferenceError::HiddenInterventionUnavailable(
            intervention,
        ))
    }
}

/// Exact conditional prompt prepared for controlled generation.
///
/// ```compile_fail
/// use loom_inference::native_controlled::PreparedControlledCompletion;
/// fn duplicate(value: PreparedControlledCompletion) { let _ = value.clone(); }
/// ```
pub struct PreparedControlledCompletion(PreparedBaseCompletion);

impl fmt::Debug for PreparedControlledCompletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedControlledCompletion")
            .field("project_id", &self.0.project_id())
            .field("prompt_fingerprint", &self.0.prompt_fingerprint())
            .finish_non_exhaustive()
    }
}

/// A live controlled ticket. Its only success path consumes the native
/// owner-worker seal.
pub struct ControlledInferenceTicket {
    native_ticket: Option<ControlledGenerationTicket>,
    pending: Option<PendingControlledBatch>,
    handle: Option<NativeModelHandle>,
}

impl fmt::Debug for ControlledInferenceTicket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControlledInferenceTicket")
            .field("active", &self.native_ticket.is_some())
            .field(
                "case_count",
                &self.pending.as_ref().map_or(0, |value| value.cases.len()),
            )
            .finish_non_exhaustive()
    }
}

impl ControlledInferenceTicket {
    pub fn cancel_call(&self, call_id: ModelCallId) -> bool {
        let Some(pending) = &self.pending else {
            return false;
        };
        let Some(case) = pending.cases.iter().find(|case| case.call_id == call_id) else {
            return false;
        };
        self.native_ticket
            .as_ref()
            .is_some_and(|ticket| ticket.cancel_case(&case.native_case_id))
    }

    pub fn cancel_all(&self) -> usize {
        self.native_ticket
            .as_ref()
            .map_or(0, ControlledGenerationTicket::cancel_all)
    }

    pub fn wait(self) -> Result<VerifiedControlledInference, ControlledInferenceError> {
        let Self {
            native_ticket,
            pending,
            handle,
        } = self;
        let seal = native_ticket
            .ok_or(ControlledInferenceError::AlreadyConsumed)?
            .wait_verified()?;
        let pending = pending.ok_or(ControlledInferenceError::AlreadyConsumed)?;
        let handle = handle.ok_or(ControlledInferenceError::AlreadyConsumed)?;
        let inference = mint_controlled_envelope(pending, &seal)?;
        Ok(VerifiedControlledInference {
            inference,
            lineage: ControlledWorkerLineage { seal, handle },
        })
    }
}

/// Admitted controlled inference plus an opaque worker-lineage witness.
///
/// ```compile_fail
/// use loom_inference::native_controlled::VerifiedControlledInference;
/// fn duplicate(value: VerifiedControlledInference) { let _ = value.clone(); }
/// ```
pub struct VerifiedControlledInference {
    inference: VerifiedInferenceOutcome,
    lineage: ControlledWorkerLineage,
}

impl fmt::Debug for VerifiedControlledInference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedControlledInference")
            .field("inference", &self.inference)
            .field("owner_call_sequence", &self.lineage.owner_call_sequence())
            .finish_non_exhaustive()
    }
}

impl VerifiedControlledInference {
    pub const fn inference(&self) -> &VerifiedInferenceOutcome {
        &self.inference
    }

    pub const fn owner_call_sequence(&self) -> u64 {
        self.lineage.owner_call_sequence()
    }

    /// Discard optional lifecycle lineage and retain per-call authorship
    /// authority. Campaign/benchmark callers should prefer `into_parts` and
    /// retain the lineage until final worker shutdown.
    pub fn into_inference(self) -> VerifiedInferenceOutcome {
        self.inference
    }

    pub fn into_parts(self) -> (VerifiedInferenceOutcome, ControlledWorkerLineage) {
        (self.inference, self.lineage)
    }

    pub fn bind_joined(
        self,
        joined: &JoinedNativeModel,
    ) -> Result<JoinedControlledInference, ControlledInferenceError> {
        let proof = self.lineage.bind_joined(joined)?;
        Ok(JoinedControlledInference {
            inference: self.inference,
            proof,
        })
    }
}

/// Opaque lineage retained after per-call admission. It is safe to keep many
/// such values while one resident performs a campaign.
pub struct ControlledWorkerLineage {
    seal: VerifiedControlledGenerationBatch,
    handle: NativeModelHandle,
}

impl fmt::Debug for ControlledWorkerLineage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControlledWorkerLineage")
            .field("owner_call_sequence", &self.owner_call_sequence())
            .finish_non_exhaustive()
    }
}

impl ControlledWorkerLineage {
    pub const fn owner_call_sequence(&self) -> u64 {
        self.seal.owner_call_sequence()
    }

    /// Check a later joined-worker token without consuming this lineage.
    /// This permits a fail-closed mismatch check before the one successful
    /// linear conversion.
    pub fn verify_joined_worker(
        &self,
        joined: &JoinedNativeModel,
    ) -> Result<(), ControlledInferenceError> {
        if !joined.belongs_to(&self.handle) || !self.seal.belongs_to_joined_model(joined) {
            return Err(ControlledInferenceError::JoinedWorkerMismatch);
        }
        Ok(())
    }

    pub fn bind_joined(
        self,
        joined: &JoinedNativeModel,
    ) -> Result<JoinedControlledLineage, ControlledInferenceError> {
        self.verify_joined_worker(joined)?;
        Ok(JoinedControlledLineage {
            owner_call_sequence: self.seal.owner_call_sequence(),
        })
    }
}

/// Move-only proof that one admitted controlled call came from the worker
/// consumed by the later joined-model token.
pub struct JoinedControlledLineage {
    owner_call_sequence: u64,
}

impl fmt::Debug for JoinedControlledLineage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JoinedControlledLineage")
            .field("owner_call_sequence", &self.owner_call_sequence)
            .finish()
    }
}

impl JoinedControlledLineage {
    pub const fn owner_call_sequence(&self) -> u64 {
        self.owner_call_sequence
    }
}

pub struct JoinedControlledInference {
    inference: VerifiedInferenceOutcome,
    proof: JoinedControlledLineage,
}

impl fmt::Debug for JoinedControlledInference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JoinedControlledInference")
            .field("inference", &self.inference)
            .field("proof", &self.proof)
            .finish()
    }
}

impl JoinedControlledInference {
    pub const fn inference(&self) -> &VerifiedInferenceOutcome {
        &self.inference
    }

    pub const fn lineage(&self) -> &JoinedControlledLineage {
        &self.proof
    }

    pub fn into_inference(self) -> VerifiedInferenceOutcome {
        self.inference
    }
}

impl ControlledBaseWriterBackend for NativeLlamaWriter {
    type PreparedPrompt = PreparedControlledCompletion;
    type CaseSpec = ControlledBaseWriterCaseSpec;
    type ControlProgram = NativeControlProgram;
    type Ticket = ControlledInferenceTicket;
    type VerifiedBatch = VerifiedControlledInference;
    type Error = ControlledInferenceError;

    fn prepare_controlled_completion(
        &self,
        binding: BaseWriterBinding,
        prompt: CompiledBaseCompletionPrompt,
    ) -> Result<Self::PreparedPrompt, Self::Error> {
        PreparedBaseCompletion::prepare(self.handle.clone(), binding, prompt)
            .map(PreparedControlledCompletion)
            .map_err(Into::into)
    }

    fn start_controlled(
        &self,
        prepared: Self::PreparedPrompt,
        cases: Vec<Self::CaseSpec>,
        control: Self::ControlProgram,
    ) -> Result<Self::Ticket, Self::Error> {
        start_controlled(prepared, cases, control)
    }

    fn wait_controlled(&self, ticket: Self::Ticket) -> Result<Self::VerifiedBatch, Self::Error> {
        ticket.wait()
    }
}

fn start_controlled(
    prepared: PreparedControlledCompletion,
    cases: Vec<ControlledBaseWriterCaseSpec>,
    control: NativeControlProgram,
) -> Result<ControlledInferenceTicket, ControlledInferenceError> {
    let PreparedControlledCompletion(PreparedBaseCompletion { handle, material }) = prepared;
    if cases.is_empty() || cases.len() > loom_research_types::MAX_BASE_WRITER_BATCH_CASES {
        return Err(ControlledInferenceError::InvalidCaseSet);
    }
    let (pending_cases, native_cases) = compile_controlled_cases(&handle, &material, cases)?;
    let placeholder = ControlledGenerationBatchRequest::new(
        "loom-controlled-preflight".to_string(),
        native_cases.clone(),
        control.program.clone(),
    )?;
    let exact_float_bits_fingerprint = parse_sha256(&placeholder.exact_float_bits_sha256())?;
    let request_cases = controlled_request_case_commitments(&pending_cases);
    let request_id = derive_controlled_request_id(&ControlledRequestCommitment {
        project_id: material.project_id,
        binding_fingerprint: material.binding.fingerprint(),
        model_fingerprint: material.model_fingerprint_id,
        tokenizer_fingerprint: material.tokenizer_fingerprint,
        source_prompt_fingerprint: material.prompt_evidence.source_prompt_fingerprint(),
        treatment_recipe_fingerprint: material.prompt_evidence.treatment_recipe_fingerprint(),
        raw_prompt_blob_id: material.prompt_evidence.raw_blob_id(),
        compiled_prompt_fingerprint: material.prompt_evidence.compiled_fingerprint(),
        prompt_token_fingerprint: material.prompt_evidence.token_fingerprint(),
        raw_prompt_byte_len: material.prompt_evidence.raw_utf8().len(),
        prompt_token_ids: material.prompt_evidence.ordered_token_ids(),
        control_program_fingerprint: control.fingerprint,
        control_exact_float_bits_fingerprint: exact_float_bits_fingerprint,
        cases: &request_cases,
    });
    let request = ControlledGenerationBatchRequest::new(request_id, native_cases, control.program)?;
    if parse_sha256(&request.exact_float_bits_sha256())? != exact_float_bits_fingerprint {
        return Err(ControlledInferenceError::ControlledSealMismatch);
    }
    let native_ticket = handle.generate_controlled(ControlledGenerationSubmission::new(
        request.clone(),
        control.constraint_body,
    )?)?;
    Ok(ControlledInferenceTicket {
        native_ticket: Some(native_ticket),
        pending: Some(PendingControlledBatch {
            material,
            request,
            cases: pending_cases,
            control_program_fingerprint: control.fingerprint,
            exact_float_bits_fingerprint,
        }),
        handle: Some(handle),
    })
}

fn compile_controlled_cases(
    handle: &NativeModelHandle,
    material: &PreparedMaterial,
    cases: Vec<ControlledBaseWriterCaseSpec>,
) -> Result<(Vec<PendingControlledCase>, Vec<ControlledGenerationCase>), ControlledInferenceError> {
    let mut seen = HashSet::with_capacity(cases.len());
    let conditional_tokens = to_i32_tokens(material.prompt_evidence.ordered_token_ids())?;
    let mut pending_cases = Vec::with_capacity(cases.len());
    let mut native_cases = Vec::with_capacity(cases.len());
    for case in cases {
        if !seen.insert(case.call_id) {
            return Err(ControlledInferenceError::InvalidCaseSet);
        }
        let unconditional = case
            .unconditional_prompt
            .map(|prompt| prepare_control_prompt(handle, material, prompt))
            .transpose()?;
        let native_case_id = format!("call-{}", case.call_id);
        let native_unconditional = unconditional
            .as_ref()
            .map(|prompt| to_i32_tokens(prompt.ordered_token_ids()))
            .transpose()?
            .map(ExactTokenPrompt::new)
            .transpose()?;
        native_cases.push(ControlledGenerationCase::new(
            native_case_id.clone(),
            ExactTokenPrompt::new(conditional_tokens.clone())?,
            native_unconditional,
            case.sampling.clone(),
        )?);
        pending_cases.push(PendingControlledCase {
            call_id: case.call_id,
            scope: material.prompt_evidence.scope(),
            native_case_id,
            sampling: case.sampling,
            sampler_fingerprint: case.sampler_fingerprint,
            unconditional,
        });
    }
    Ok((pending_cases, native_cases))
}

fn controlled_request_case_commitments(
    pending_cases: &[PendingControlledCase],
) -> Vec<ControlledRequestCaseCommitment> {
    pending_cases
        .iter()
        .map(|case| ControlledRequestCaseCommitment {
            call_id: case.call_id,
            scope: case.scope,
            sampler_fingerprint: case.sampler_fingerprint,
            seed: case.sampling.seed,
            unconditional_source_prompt_fingerprint: case
                .unconditional
                .as_ref()
                .map(ExactPromptEvidence::source_prompt_fingerprint),
            unconditional_raw_prompt_blob_id: case
                .unconditional
                .as_ref()
                .map(ExactPromptEvidence::raw_blob_id),
            unconditional_raw_prompt_byte_len: case
                .unconditional
                .as_ref()
                .map(|prompt| prompt.raw_utf8().len()),
            unconditional_prompt_fingerprint: case
                .unconditional
                .as_ref()
                .map(ExactPromptEvidence::compiled_fingerprint),
            unconditional_token_fingerprint: case
                .unconditional
                .as_ref()
                .map(ExactPromptEvidence::token_fingerprint),
        })
        .collect()
}

fn prepare_control_prompt(
    handle: &NativeModelHandle,
    conditional: &PreparedMaterial,
    prompt: CompiledBaseCompletionPrompt,
) -> Result<ExactPromptEvidence, ControlledInferenceError> {
    let prepared =
        PreparedBaseCompletion::prepare(handle.clone(), conditional.binding.clone(), prompt)?;
    if prepared.material.project_id != conditional.project_id
        || prepared.material.prompt_evidence.scope() != conditional.prompt_evidence.scope()
        || prepared
            .material
            .prompt_evidence
            .treatment_recipe_fingerprint()
            != conditional.prompt_evidence.treatment_recipe_fingerprint()
    {
        return Err(ControlledInferenceError::UnconditionalPromptScopeMismatch);
    }
    Ok(prepared.material.prompt_evidence)
}

fn to_i32_tokens(tokens: &[u32]) -> Result<Vec<i32>, ControlledInferenceError> {
    tokens
        .iter()
        .map(|token| {
            i32::try_from(*token).map_err(|_| ControlledInferenceError::InvalidEmbeddingInput)
        })
        .collect()
}

#[derive(Debug)]
struct PendingControlledCase {
    call_id: ModelCallId,
    scope: CallScope,
    native_case_id: String,
    sampling: SamplingConfig,
    sampler_fingerprint: BlobId,
    unconditional: Option<ExactPromptEvidence>,
}

#[derive(Debug)]
struct PendingControlledBatch {
    material: PreparedMaterial,
    request: ControlledGenerationBatchRequest,
    cases: Vec<PendingControlledCase>,
    control_program_fingerprint: BlobId,
    exact_float_bits_fingerprint: BlobId,
}

fn mint_controlled_envelope(
    pending: PendingControlledBatch,
    seal: &VerifiedControlledGenerationBatch,
) -> Result<VerifiedInferenceOutcome, ControlledInferenceError> {
    if seal.terminal() != VerifiedControlledGenerationTerminal::Completed
        || seal.output().request() != &pending.request
        || seal.model_fingerprint() != &pending.material.model_fingerprint
        || seal.output().cases().len() != pending.cases.len()
        || seal.terminal_sampled_token_ids().len() != pending.cases.len()
        || parse_sha256(seal.request_sha256())?
            != parse_sha256(&pending.request.fingerprint_sha256())?
    {
        return Err(ControlledInferenceError::ControlledSealMismatch);
    }
    pending
        .material
        .binding
        .verify_native_model(seal.model_fingerprint())
        .map_err(InferenceError::from)?;
    let ledgers = validate_controlled_events(&pending, seal)?;
    let mut outcomes = Vec::with_capacity(pending.cases.len());
    let mut batch_commitments = Vec::with_capacity(pending.cases.len());
    for (index, ((case, output), terminal)) in pending
        .cases
        .iter()
        .zip(seal.output().cases())
        .zip(seal.terminal_sampled_token_ids())
        .enumerate()
    {
        let call = mint_controlled_call(
            &pending,
            ControlledCallSource {
                seal,
                case,
                output: output.generation(),
                distribution_observations: output.distribution_observations(),
                terminal_sampled_token_id: *terminal,
                ledger: &ledgers[index],
                index,
            },
        )?;
        batch_commitments.push(BatchCaseCommitment {
            call_id: case.call_id,
            completed: true,
            verification_fingerprint: call.verification_fingerprint(),
        });
        outcomes.push(VerifiedCaseOutcome::completed(index, call));
    }
    let verification_fingerprint = derive_batch_verification_fingerprint(&BatchCommitment {
        project_id: pending.material.project_id,
        binding_fingerprint: pending.material.binding.fingerprint(),
        source_prompt_fingerprint: pending.material.prompt_evidence.source_prompt_fingerprint(),
        compiled_prompt_fingerprint: pending.material.prompt_evidence.compiled_fingerprint(),
        request_id: pending.request.request_id(),
        cases: &batch_commitments,
    });
    VerifiedInferenceOutcome::mint(
        pending.material.project_id,
        pending.material.binding,
        pending.material.prompt_evidence,
        pending.request.request_id().to_string(),
        outcomes,
        verification_fingerprint,
    )
    .map_err(|_| ControlledInferenceError::AdmissionInvariant)
}

struct ControlledLedger<'a> {
    events: Vec<&'a GenerationEvent>,
    raw_output: String,
}

fn validate_controlled_events<'a>(
    pending: &PendingControlledBatch,
    seal: &'a VerifiedControlledGenerationBatch,
) -> Result<Vec<ControlledLedger<'a>>, ControlledInferenceError> {
    let mut ledgers = (0..pending.cases.len())
        .map(|_| ControlledLedger {
            events: Vec::new(),
            raw_output: String::new(),
        })
        .collect::<Vec<_>>();
    for event in seal.events() {
        let Some(case) = pending.cases.get(event.input_index) else {
            return Err(ControlledInferenceError::ControlledOutputMismatch(
                event.input_index,
            ));
        };
        if event.request_id != pending.request.request_id()
            || event.branch_id != case.native_case_id
            || event.sequence_id != i32::try_from(event.input_index).unwrap_or(-1)
        {
            return Err(ControlledInferenceError::ControlledOutputMismatch(
                event.input_index,
            ));
        }
        ledgers[event.input_index].events.push(event);
    }
    for (index, (ledger, output)) in ledgers.iter_mut().zip(seal.output().cases()).enumerate() {
        let mut terminal = false;
        for (expected, event) in ledger.events.iter().enumerate() {
            if event.event_index != expected as u64 || terminal {
                return Err(ControlledInferenceError::ControlledOutputMismatch(index));
            }
            match &event.event {
                GenerationEventKind::State {
                    state: GenerationState::Prefilling,
                } if expected == 0 => {}
                GenerationEventKind::State {
                    state: GenerationState::Generating,
                } if expected == 1 => {}
                GenerationEventKind::Delta { text } if expected >= 2 && !text.is_empty() => {
                    ledger.raw_output.push_str(text);
                }
                GenerationEventKind::State {
                    state: GenerationState::Completed,
                } if expected >= 2 => terminal = true,
                GenerationEventKind::State { .. }
                | GenerationEventKind::Delta { .. }
                | GenerationEventKind::Warning { .. } => {
                    return Err(ControlledInferenceError::ControlledOutputMismatch(index));
                }
            }
        }
        if !terminal || output.generation().state != GenerationState::Completed {
            return Err(ControlledInferenceError::ControlledOutputMismatch(index));
        }
    }
    Ok(ledgers)
}

#[derive(Clone, Copy)]
struct ControlledCallSource<'a> {
    seal: &'a VerifiedControlledGenerationBatch,
    case: &'a PendingControlledCase,
    output: &'a GenerationOutput,
    distribution_observations: &'a [llama_native_types::TokenDistributionObservation],
    terminal_sampled_token_id: Option<i32>,
    ledger: &'a ControlledLedger<'a>,
    index: usize,
}

struct MintedControlledCallEvidence {
    raw_output: Vec<u8>,
    generated_token_ids: Vec<u32>,
    event_json: Vec<u8>,
    event_blob_id: BlobId,
    backend_audit_json: Vec<u8>,
    backend_receipt_blob_id: BlobId,
    verification_fingerprint: BlobId,
}

fn mint_controlled_call(
    pending: &PendingControlledBatch,
    source: ControlledCallSource<'_>,
) -> Result<VerifiedBaseWriterCall, ControlledInferenceError> {
    validate_controlled_output(pending, source)?;
    let evidence = compile_controlled_call_evidence(pending, source)?;
    mint_authoritative_controlled_call(pending, source, evidence)
}

fn compile_controlled_call_evidence(
    pending: &PendingControlledBatch,
    source: ControlledCallSource<'_>,
) -> Result<MintedControlledCallEvidence, ControlledInferenceError> {
    let raw_output = source.ledger.raw_output.as_bytes().to_vec();
    let generated_token_ids = source
        .output
        .generated_token_ids
        .iter()
        .map(|token| {
            u32::try_from(*token)
                .map_err(|_| ControlledInferenceError::ControlledOutputMismatch(source.index))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let event_json = serialize_controlled_event_ledger(source)?;
    let event_blob_id = BlobId::digest(&event_json);
    let backend_audit_json = serialize_controlled_backend_audit(
        pending,
        source,
        &raw_output,
        &generated_token_ids,
        event_blob_id,
    )?;
    let backend_receipt_blob_id = BlobId::digest(&backend_audit_json);
    let verification_fingerprint = derive_controlled_call_verification(
        pending,
        source,
        &raw_output,
        &generated_token_ids,
        event_blob_id,
        backend_receipt_blob_id,
    );
    Ok(MintedControlledCallEvidence {
        raw_output,
        generated_token_ids,
        event_json,
        event_blob_id,
        backend_audit_json,
        backend_receipt_blob_id,
        verification_fingerprint,
    })
}

fn serialize_controlled_event_ledger(
    source: ControlledCallSource<'_>,
) -> Result<Vec<u8>, ControlledInferenceError> {
    let compact_events = source
        .ledger
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
            call_id: source.case.call_id,
            native_case_id: &source.case.native_case_id,
            input_index: source.index,
            events: &compact_events,
        },
        "controlled event ledger",
    )?;
    Ok(event_json)
}

fn serialize_controlled_backend_audit(
    pending: &PendingControlledBatch,
    source: ControlledCallSource<'_>,
    raw_output: &[u8],
    generated_token_ids: &[u32],
    event_blob_id: BlobId,
) -> Result<Vec<u8>, ControlledInferenceError> {
    let case = source.case;
    let audit = ControlledBackendAuditRecord {
        format: CONTROLLED_AUDIT_FORMAT,
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
        native_request_id: pending.request.request_id(),
        native_case_id: &case.native_case_id,
        input_index: source.index,
        sampler_fingerprint: case.sampler_fingerprint,
        sampling: &case.sampling,
        control_program_fingerprint: pending.control_program_fingerprint,
        control_exact_float_bits_fingerprint: pending.exact_float_bits_fingerprint,
        unconditional_prompt: case.unconditional.as_ref().map(ControlPromptAudit::from),
        model_fingerprint: ControlledSanitizedModelFingerprint::from(
            source.seal.model_fingerprint(),
        ),
        output: ControlledSanitizedGenerationOutput::new(
            source.output,
            raw_output,
            generated_token_ids,
        ),
        distribution_observations: source.distribution_observations,
        native_output: source.seal.output(),
        sealed_request_sha256: source.seal.request_sha256(),
        sealed_output_sha256: source.seal.output_sha256(),
        sealed_event_stream_sha256: source.seal.event_stream_sha256(),
        sealed_runtime_operation_ledger_sha256: source.seal.runtime_operation_ledger_sha256(),
        sealed_ledger_sha256: source.seal.ledger_sha256(),
        owner_call_sequence: source.seal.owner_call_sequence(),
        runtime_cost: RuntimeCostAudit::from(source.seal.runtime_cost()),
        terminal_sampled_token_id: source.terminal_sampled_token_id,
        event_stream_blob_id: event_blob_id,
    };
    serialize_bounded_evidence(&audit, "controlled backend audit").map_err(Into::into)
}

fn derive_controlled_call_verification(
    pending: &PendingControlledBatch,
    source: ControlledCallSource<'_>,
    raw_output: &[u8],
    generated_token_ids: &[u32],
    event_blob_id: BlobId,
    backend_receipt_blob_id: BlobId,
) -> BlobId {
    derive_call_verification_fingerprint(&CallVerificationCommitment {
        project_id: pending.material.project_id,
        request_id: pending.request.request_id(),
        call_id: source.case.call_id,
        scope: source.case.scope,
        model_fingerprint: pending.material.model_fingerprint_id,
        source_prompt_fingerprint: pending.material.prompt_evidence.source_prompt_fingerprint(),
        treatment_recipe_fingerprint: pending
            .material
            .prompt_evidence
            .treatment_recipe_fingerprint(),
        raw_prompt_blob_id: pending.material.prompt_evidence.raw_blob_id(),
        compiled_prompt_fingerprint: pending.material.prompt_evidence.compiled_fingerprint(),
        prompt_token_fingerprint: pending.material.prompt_evidence.token_fingerprint(),
        sampler_fingerprint: source.case.sampler_fingerprint,
        control_program_fingerprint: pending.control_program_fingerprint,
        raw_output,
        generated_token_ids,
        event_blob_id,
        backend_receipt_blob_id,
        terminal_sampled_token_id: source.terminal_sampled_token_id,
    })
}

fn mint_authoritative_controlled_call(
    pending: &PendingControlledBatch,
    source: ControlledCallSource<'_>,
    evidence: MintedControlledCallEvidence,
) -> Result<VerifiedBaseWriterCall, ControlledInferenceError> {
    let identity = CallIdentity::new(
        source.case.scope,
        pending.material.model_fingerprint_id,
        pending.material.tokenizer_fingerprint,
        pending.material.prompt_evidence.compiled_fingerprint(),
        source.case.sampler_fingerprint,
        pending.control_program_fingerprint,
        u64::from(source.case.sampling.seed),
    );
    let completed = CompletedCall::new(
        &evidence.raw_output,
        &evidence.generated_token_ids,
        evidence.event_blob_id,
        Some(evidence.backend_receipt_blob_id),
    )?;
    let model_call = ModelCall::new(
        source.case.call_id,
        identity,
        CallEvidenceClass::LiveBaseWriterClaim,
        CallTerminal::Completed(completed),
    )?;
    let displayed_output = source.output.text.as_bytes().to_vec();
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
        source.output.metrics.prompt_tokens as u64,
        source.output.metrics.completion_tokens as u64,
        source.output.metrics.duration_ms,
        evidence.verification_fingerprint,
    );
    Ok(VerifiedBaseWriterCall {
        model_call,
        raw_output: evidence.raw_output,
        generated_token_ids: evidence.generated_token_ids,
        event_json: evidence.event_json,
        backend_audit_json: evidence.backend_audit_json,
        displayed_output,
        output_projection,
        terminal_sampled_token_id: source.terminal_sampled_token_id,
        verification_fingerprint: evidence.verification_fingerprint,
        runtime_charge,
    })
}

fn validate_controlled_output(
    pending: &PendingControlledBatch,
    source: ControlledCallSource<'_>,
) -> Result<(), ControlledInferenceError> {
    let case = source.case;
    let output = source.output;
    let shared = source
        .seal
        .runtime_cost()
        .conditional_shared_prefix_tokens();
    if output.request_id != pending.request.request_id()
        || output.branch_id != case.native_case_id
        || output.input_index != source.index
        || output.model_id != pending.material.model_fingerprint.model_id
        || !output.real_engine_invoked
        || output.fake_fixture
        || output.transport != NativeTransport::InProcess
        || output.state != GenerationState::Completed
        || output.metrics.prompt_tokens
            != pending.material.prompt_evidence.ordered_token_ids().len()
        || output.metrics.completion_tokens != output.generated_token_ids.len()
        || output.metrics.shared_prefix_tokens != shared
        || output.metrics.cache.supplied_prefix_tokens != 0
        || output.metrics.cache.restored_prefix_tokens != 0
        || output.metrics.cache.batch_shared_prefix_tokens != shared
        || output.generated_token_ids.len() > case.sampling.max_tokens as usize
    {
        return Err(ControlledInferenceError::ControlledOutputMismatch(
            source.index,
        ));
    }
    let displayed = output.text.as_bytes();
    let raw = source.ledger.raw_output.as_bytes();
    let valid_terminal = match output.finish_reason.as_str() {
        "end_of_generation" => source.terminal_sampled_token_id.is_some() && displayed == raw,
        "max_tokens" => {
            source.terminal_sampled_token_id.is_none()
                && output.generated_token_ids.len() == case.sampling.max_tokens as usize
                && displayed == raw
        }
        "stop_sequence" => {
            source.terminal_sampled_token_id.is_none()
                && raw.strip_prefix(displayed).is_some_and(|suffix| {
                    !suffix.is_empty()
                        && case
                            .sampling
                            .stop
                            .iter()
                            .any(|stop| stop.as_bytes() == suffix)
                })
        }
        _ => false,
    };
    if !valid_terminal {
        return Err(ControlledInferenceError::ControlledOutputMismatch(
            source.index,
        ));
    }
    Ok(())
}

#[derive(Serialize)]
struct ControlledBackendAuditRecord<'a> {
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
    control_program_fingerprint: BlobId,
    control_exact_float_bits_fingerprint: BlobId,
    unconditional_prompt: Option<ControlPromptAudit>,
    model_fingerprint: ControlledSanitizedModelFingerprint<'a>,
    output: ControlledSanitizedGenerationOutput<'a>,
    distribution_observations: &'a [llama_native_types::TokenDistributionObservation],
    native_output: &'a ControlledGenerationBatchOutput,
    sealed_request_sha256: &'a str,
    sealed_output_sha256: &'a str,
    sealed_event_stream_sha256: &'a str,
    sealed_runtime_operation_ledger_sha256: &'a str,
    sealed_ledger_sha256: &'a str,
    owner_call_sequence: u64,
    runtime_cost: RuntimeCostAudit,
    terminal_sampled_token_id: Option<i32>,
    event_stream_blob_id: BlobId,
}

#[derive(Serialize)]
struct ControlledSanitizedModelFingerprint<'a> {
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

impl<'a> From<&'a llama_native_types::ModelFingerprint>
    for ControlledSanitizedModelFingerprint<'a>
{
    fn from(value: &'a llama_native_types::ModelFingerprint) -> Self {
        Self {
            format: "loom.native-model-fingerprint.v1",
            model_size: value.model_size,
            model_sha256: &value.model_sha256,
            tokenizer_sha256: &value.tokenizer_sha256,
            chat_template_sha256: &value.chat_template_sha256,
            multimodal_projector_sha256: value.multimodal_projector_sha256.as_deref(),
            binding_version: &value.binding_version,
            build_id: &value.build_id,
            backend: &value.backend,
            context_tokens: value.context_tokens,
            batch_tokens: value.batch_tokens,
            max_sequences: value.max_sequences,
            rope_config_sha256: &value.rope_config_sha256,
            kv_layout_sha256: &value.kv_layout_sha256,
        }
    }
}

#[derive(Serialize)]
struct ControlPromptAudit {
    raw_utf8: Vec<u8>,
    raw_blob_id: BlobId,
    source_prompt_fingerprint: BlobId,
    compiled_prompt_fingerprint: BlobId,
    token_fingerprint: BlobId,
    token_ids: Vec<u32>,
}

impl From<&ExactPromptEvidence> for ControlPromptAudit {
    fn from(value: &ExactPromptEvidence) -> Self {
        Self {
            raw_utf8: value.raw_utf8().to_vec(),
            raw_blob_id: value.raw_blob_id(),
            source_prompt_fingerprint: value.source_prompt_fingerprint(),
            compiled_prompt_fingerprint: value.compiled_fingerprint(),
            token_fingerprint: value.token_fingerprint(),
            token_ids: value.ordered_token_ids().to_vec(),
        }
    }
}

#[derive(Serialize)]
struct RuntimeCostAudit {
    conditional_shared_prefix_tokens: usize,
    unconditional_shared_prefix_tokens: usize,
    physical_prompt_evaluations: u64,
    reserved_physical_context_cells: u64,
    sequence_slots: u32,
}

impl From<llama_native_engine::ControlledRuntimeCostEvidence> for RuntimeCostAudit {
    fn from(value: llama_native_engine::ControlledRuntimeCostEvidence) -> Self {
        Self {
            conditional_shared_prefix_tokens: value.conditional_shared_prefix_tokens(),
            unconditional_shared_prefix_tokens: value.unconditional_shared_prefix_tokens(),
            physical_prompt_evaluations: value.physical_prompt_evaluations(),
            reserved_physical_context_cells: value.reserved_physical_context_cells(),
            sequence_slots: value.sequence_slots(),
        }
    }
}

#[derive(Serialize)]
struct ControlledSanitizedGenerationOutput<'a> {
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
    token_observations: Option<&'a [llama_native_types::TokenObservation]>,
    state: GenerationState,
    finish_reason: &'a str,
    metrics: &'a GenerationMetrics,
    real_engine_invoked: bool,
    fake_fixture: bool,
    transport: NativeTransport,
}

impl<'a> ControlledSanitizedGenerationOutput<'a> {
    fn new(output: &'a GenerationOutput, raw: &[u8], tokens: &[u32]) -> Self {
        Self {
            format: OUTPUT_AUDIT_FORMAT,
            request_id: &output.request_id,
            branch_id: &output.branch_id,
            input_index: output.input_index,
            displayed_output_blob_id: BlobId::digest(output.text.as_bytes()),
            displayed_output_byte_len: output.text.len(),
            raw_output_blob_id: BlobId::digest(raw),
            raw_output_byte_len: raw.len(),
            generated_token_ids_blob_id: token_ids_blob_id(tokens),
            generated_token_count: tokens.len(),
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

#[derive(Serialize)]
struct CompactGenerationEvent<'a> {
    event_index: u64,
    event: &'a GenerationEventKind,
}

/// Backend-neutral pooling declaration for an exact embedding request.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingPoolingMode {
    None,
    Mean,
    Cls,
    Last,
    Rank,
}

impl From<EmbeddingPoolingMode> for EmbeddingPooling {
    fn from(value: EmbeddingPoolingMode) -> Self {
        match value {
            EmbeddingPoolingMode::None => Self::None,
            EmbeddingPoolingMode::Mean => Self::Mean,
            EmbeddingPoolingMode::Cls => Self::Cls,
            EmbeddingPoolingMode::Last => Self::Last,
            EmbeddingPoolingMode::Rank => Self::Rank,
        }
    }
}

/// Backend-neutral normalization declaration for an exact embedding request.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingNormalizationMode {
    None,
    L2,
}

impl From<EmbeddingNormalizationMode> for EmbeddingNormalization {
    fn from(value: EmbeddingNormalizationMode) -> Self {
        match value {
            EmbeddingNormalizationMode::None => Self::None,
            EmbeddingNormalizationMode::L2 => Self::L2,
        }
    }
}

/// One exact-token embedding input. It is request data, not verification
/// authority.
#[derive(Debug)]
pub struct ExactEmbeddingInput {
    input_id: String,
    token_ids: Vec<u32>,
}

impl ExactEmbeddingInput {
    pub fn new(
        input_id: impl Into<String>,
        token_ids: Vec<u32>,
    ) -> Result<Self, ControlledInferenceError> {
        let input_id = input_id.into();
        if input_id.is_empty()
            || input_id.len() > MAX_EMBEDDING_INPUT_ID_BYTES
            || input_id.chars().any(char::is_control)
            || token_ids.is_empty()
            || token_ids.len() > MAX_EMBEDDING_INPUT_TOKENS
            || token_ids.iter().any(|token| *token > i32::MAX as u32)
        {
            return Err(ControlledInferenceError::InvalidEmbeddingInput);
        }
        Ok(Self {
            input_id,
            token_ids,
        })
    }

    pub fn input_id(&self) -> &str {
        &self.input_id
    }

    pub fn token_ids(&self) -> &[u32] {
        &self.token_ids
    }
}

/// Move-only request binding exact inputs to a compiled model binding.
pub struct ExactEmbeddingRequest {
    binding: BaseWriterBinding,
    inputs: Vec<ExactEmbeddingInput>,
    pooling: EmbeddingPoolingMode,
    normalization: EmbeddingNormalizationMode,
}

impl fmt::Debug for ExactEmbeddingRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExactEmbeddingRequest")
            .field("binding", &self.binding)
            .field("input_count", &self.inputs.len())
            .field("pooling", &self.pooling)
            .field("normalization", &self.normalization)
            .finish()
    }
}

impl ExactEmbeddingRequest {
    pub fn new(
        binding: BaseWriterBinding,
        inputs: Vec<ExactEmbeddingInput>,
        pooling: EmbeddingPoolingMode,
        normalization: EmbeddingNormalizationMode,
    ) -> Result<Self, ControlledInferenceError> {
        let mut ids = HashSet::with_capacity(inputs.len());
        let total_tokens = inputs.iter().try_fold(0_usize, |total, input| {
            ids.insert(input.input_id.as_str())
                .then(|| total.checked_add(input.token_ids.len()))
                .flatten()
        });
        if inputs.is_empty()
            || inputs.len() > MAX_EMBEDDING_BATCH_INPUTS
            || total_tokens.is_none_or(|total| total > MAX_EMBEDDING_BATCH_TOKENS)
        {
            return Err(ControlledInferenceError::InvalidEmbeddingInput);
        }
        Ok(Self {
            binding,
            inputs,
            pooling,
            normalization,
        })
    }
}

pub struct VerifiedEmbeddingTicket {
    native_ticket: Option<EmbeddingTicket>,
    pending: Option<PendingEmbedding>,
    handle: Option<NativeModelHandle>,
}

impl fmt::Debug for VerifiedEmbeddingTicket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedEmbeddingTicket")
            .field("active", &self.native_ticket.is_some())
            .finish_non_exhaustive()
    }
}

impl VerifiedEmbeddingTicket {
    pub fn cancel(&self) {
        if let Some(ticket) = &self.native_ticket {
            ticket.cancel();
        }
    }

    pub fn wait(self) -> Result<VerifiedEmbeddingDiagnostic, ControlledInferenceError> {
        let Self {
            native_ticket,
            pending,
            handle,
        } = self;
        let seal = native_ticket
            .ok_or(ControlledInferenceError::AlreadyConsumed)?
            .wait_verified()?;
        let pending = pending.ok_or(ControlledInferenceError::AlreadyConsumed)?;
        let handle = handle.ok_or(ControlledInferenceError::AlreadyConsumed)?;
        let data = mint_embedding_diagnostic(pending, &seal)?;
        Ok(VerifiedEmbeddingDiagnostic {
            data,
            lineage: EmbeddingWorkerLineage { seal, handle },
        })
    }
}

/// Opaque lifecycle lineage for one verified embedding call.
pub struct EmbeddingWorkerLineage {
    seal: VerifiedEmbeddingBatch,
    handle: NativeModelHandle,
}

impl fmt::Debug for EmbeddingWorkerLineage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EmbeddingWorkerLineage")
            .field("owner_call_sequence", &self.owner_call_sequence())
            .finish_non_exhaustive()
    }
}

impl EmbeddingWorkerLineage {
    pub const fn owner_call_sequence(&self) -> u64 {
        self.seal.owner_call_sequence()
    }

    pub fn verify_joined_worker(
        &self,
        joined: &JoinedNativeModel,
    ) -> Result<(), ControlledInferenceError> {
        if !joined.belongs_to(&self.handle) || !self.seal.belongs_to_joined_model(joined) {
            return Err(ControlledInferenceError::JoinedWorkerMismatch);
        }
        Ok(())
    }

    pub fn bind_joined(
        self,
        joined: &JoinedNativeModel,
    ) -> Result<JoinedEmbeddingLineage, ControlledInferenceError> {
        self.verify_joined_worker(joined)?;
        Ok(JoinedEmbeddingLineage {
            owner_call_sequence: self.seal.owner_call_sequence(),
        })
    }
}

pub struct JoinedEmbeddingLineage {
    owner_call_sequence: u64,
}

impl fmt::Debug for JoinedEmbeddingLineage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JoinedEmbeddingLineage")
            .field("owner_call_sequence", &self.owner_call_sequence)
            .finish()
    }
}

impl JoinedEmbeddingLineage {
    pub const fn owner_call_sequence(&self) -> u64 {
        self.owner_call_sequence
    }
}

#[derive(Debug)]
struct PendingEmbedding {
    binding: BaseWriterBinding,
    request: EmbeddingBatchRequest,
    resident_model_fingerprint: llama_native_types::ModelFingerprint,
    pooling: EmbeddingPoolingMode,
    normalization: EmbeddingNormalizationMode,
}

/// One verified embedding output vector. The values are diagnostic data; this
/// type carries no learning, promotion, or benchmark lease.
#[derive(Debug)]
pub struct VerifiedEmbeddingVector {
    input_id: String,
    input_index: usize,
    exact_token_ids: Vec<u32>,
    row_count: u32,
    values: Vec<f32>,
}

impl VerifiedEmbeddingVector {
    pub fn input_id(&self) -> &str {
        &self.input_id
    }

    pub const fn input_index(&self) -> usize {
        self.input_index
    }

    pub fn exact_token_ids(&self) -> &[u32] {
        &self.exact_token_ids
    }

    pub const fn row_count(&self) -> u32 {
        self.row_count
    }

    pub fn values(&self) -> &[f32] {
        &self.values
    }
}

/// Move-only, owner-worker-sealed embedding diagnostic.
///
/// ```compile_fail
/// use loom_inference::native_controlled::VerifiedEmbeddingDiagnostic;
/// fn duplicate(value: VerifiedEmbeddingDiagnostic) { let _ = value.clone(); }
/// ```
///
/// ```compile_fail
/// use loom_inference::native_controlled::VerifiedEmbeddingDiagnostic;
/// fn require_deserialize<T: for<'de> serde::Deserialize<'de>>() {}
/// require_deserialize::<VerifiedEmbeddingDiagnostic>();
/// ```
pub struct EmbeddingDiagnosticData {
    request_id: String,
    output_model_id: String,
    binding: BaseWriterBinding,
    resident_model: PersistedEmbeddingModelFingerprint,
    execution_model: PersistedEmbeddingModelFingerprint,
    resident_model_fingerprint: BlobId,
    execution_model_fingerprint: BlobId,
    tokenizer_fingerprint: BlobId,
    pooling: EmbeddingPoolingMode,
    normalization: EmbeddingNormalizationMode,
    dimensions: u32,
    outputs: Vec<VerifiedEmbeddingVector>,
    request_fingerprint: BlobId,
    output_bits_fingerprint: BlobId,
    ledger_fingerprint: BlobId,
    owner_call_sequence: u64,
}

impl fmt::Debug for EmbeddingDiagnosticData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EmbeddingDiagnosticData")
            .field("request_id", &self.request_id)
            .field("binding", &self.binding)
            .field(
                "resident_model_fingerprint",
                &self.resident_model_fingerprint,
            )
            .field(
                "execution_model_fingerprint",
                &self.execution_model_fingerprint,
            )
            .field("dimensions", &self.dimensions)
            .field("output_count", &self.outputs.len())
            .finish_non_exhaustive()
    }
}

impl EmbeddingDiagnosticData {
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    pub const fn binding(&self) -> &BaseWriterBinding {
        &self.binding
    }

    pub const fn resident_model_fingerprint(&self) -> BlobId {
        self.resident_model_fingerprint
    }

    pub const fn execution_model_fingerprint(&self) -> BlobId {
        self.execution_model_fingerprint
    }

    pub const fn tokenizer_fingerprint(&self) -> BlobId {
        self.tokenizer_fingerprint
    }

    pub const fn pooling(&self) -> EmbeddingPoolingMode {
        self.pooling
    }

    pub const fn normalization(&self) -> EmbeddingNormalizationMode {
        self.normalization
    }

    pub const fn dimensions(&self) -> u32 {
        self.dimensions
    }

    pub fn outputs(&self) -> &[VerifiedEmbeddingVector] {
        &self.outputs
    }

    pub const fn request_fingerprint(&self) -> BlobId {
        self.request_fingerprint
    }

    pub const fn output_bits_fingerprint(&self) -> BlobId {
        self.output_bits_fingerprint
    }

    pub const fn ledger_fingerprint(&self) -> BlobId {
        self.ledger_fingerprint
    }

    pub const fn owner_call_sequence(&self) -> u64 {
        self.owner_call_sequence
    }

    /// Encode an immutable, bounded, canonical diagnostic replay record.
    ///
    /// The record retains exact token IDs and exact `f32` bits. It is useful
    /// for audit and later source-binding, but it is deliberately not a
    /// verified learning example: this crate issues no learning-dataset or
    /// ranker-activation authority from embedding bytes.
    pub fn canonical_diagnostic_record(&self) -> Result<Vec<u8>, EmbeddingDiagnosticRecordError> {
        let body = PersistedEmbeddingDiagnosticBody::from(self);
        let body_bytes = serde_json::to_vec(&body)?;
        let record = PersistedEmbeddingDiagnosticRecord {
            format: EMBEDDING_DIAGNOSTIC_RECORD_FORMAT.to_owned(),
            record_fingerprint: embedding_record_fingerprint(&body_bytes),
            body,
        };
        let bytes = serde_json::to_vec(&record)?;
        if bytes.is_empty() || bytes.len() > loom_research_types::MAX_BACKEND_EVIDENCE_BYTES {
            return Err(EmbeddingDiagnosticRecordError::EmptyOrOversized);
        }
        Ok(bytes)
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedEmbeddingDiagnosticRecord {
    format: String,
    record_fingerprint: BlobId,
    body: PersistedEmbeddingDiagnosticBody,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedEmbeddingDiagnosticBody {
    request_id: String,
    output_model_id: String,
    binding: PersistedEmbeddingBinding,
    resident_model: PersistedEmbeddingModelFingerprint,
    execution_model: PersistedEmbeddingModelFingerprint,
    resident_model_fingerprint: BlobId,
    execution_model_fingerprint: BlobId,
    tokenizer_fingerprint: BlobId,
    pooling: EmbeddingPoolingMode,
    normalization: EmbeddingNormalizationMode,
    dimensions: u32,
    outputs: Vec<PersistedEmbeddingVector>,
    request_fingerprint: BlobId,
    output_bits_fingerprint: BlobId,
    ledger_fingerprint: BlobId,
    owner_call_sequence: u64,
}

impl From<&EmbeddingDiagnosticData> for PersistedEmbeddingDiagnosticBody {
    fn from(value: &EmbeddingDiagnosticData) -> Self {
        Self {
            request_id: value.request_id.clone(),
            output_model_id: value.output_model_id.clone(),
            binding: PersistedEmbeddingBinding::from(&value.binding),
            resident_model: value.resident_model.clone(),
            execution_model: value.execution_model.clone(),
            resident_model_fingerprint: value.resident_model_fingerprint,
            execution_model_fingerprint: value.execution_model_fingerprint,
            tokenizer_fingerprint: value.tokenizer_fingerprint,
            pooling: value.pooling,
            normalization: value.normalization,
            dimensions: value.dimensions,
            outputs: value
                .outputs
                .iter()
                .map(PersistedEmbeddingVector::from)
                .collect(),
            request_fingerprint: value.request_fingerprint,
            output_bits_fingerprint: value.output_bits_fingerprint,
            ledger_fingerprint: value.ledger_fingerprint,
            owner_call_sequence: value.owner_call_sequence,
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedEmbeddingModelFingerprint {
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

impl From<&llama_native_types::ModelFingerprint> for PersistedEmbeddingModelFingerprint {
    fn from(value: &llama_native_types::ModelFingerprint) -> Self {
        Self {
            model_id: value.model_id.clone(),
            model_size: value.model_size,
            model_sha256: value.model_sha256.clone(),
            tokenizer_sha256: value.tokenizer_sha256.clone(),
            chat_template_sha256: value.chat_template_sha256.clone(),
            multimodal_projector_sha256: value.multimodal_projector_sha256.clone(),
            binding_version: value.binding_version.clone(),
            build_id: value.build_id.clone(),
            backend: value.backend.clone(),
            context_tokens: value.context_tokens,
            batch_tokens: value.batch_tokens,
            max_sequences: value.max_sequences,
            rope_config_sha256: value.rope_config_sha256.clone(),
            kv_layout_sha256: value.kv_layout_sha256.clone(),
        }
    }
}

impl PersistedEmbeddingModelFingerprint {
    fn as_native(&self) -> llama_native_types::ModelFingerprint {
        llama_native_types::ModelFingerprint {
            model_id: self.model_id.clone(),
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

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedEmbeddingBinding {
    binding_id: String,
    binding_fingerprint: BlobId,
    manifest_fingerprint: BlobId,
    model_sha256: BlobId,
    model_byte_len: u64,
    tokenizer_sha256: BlobId,
    multimodal_projector_sha256: Option<BlobId>,
    architecture: String,
    context_tokens: u32,
}

impl From<&BaseWriterBinding> for PersistedEmbeddingBinding {
    fn from(value: &BaseWriterBinding) -> Self {
        Self {
            binding_id: value.binding_id().to_owned(),
            binding_fingerprint: value.fingerprint(),
            manifest_fingerprint: value.manifest_fingerprint().as_blob_id(),
            model_sha256: value.model_sha256(),
            model_byte_len: value.model_bytes(),
            tokenizer_sha256: value.tokenizer_sha256(),
            multimodal_projector_sha256: value.multimodal_projector_sha256(),
            architecture: value.architecture().to_owned(),
            context_tokens: value.context_tokens(),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedEmbeddingVector {
    input_id: String,
    input_index: usize,
    exact_token_ids: Vec<u32>,
    row_count: u32,
    value_bits: Vec<u32>,
}

impl From<&VerifiedEmbeddingVector> for PersistedEmbeddingVector {
    fn from(value: &VerifiedEmbeddingVector) -> Self {
        Self {
            input_id: value.input_id.clone(),
            input_index: value.input_index,
            exact_token_ids: value.exact_token_ids.clone(),
            row_count: value.row_count,
            value_bits: value.values.iter().map(|value| value.to_bits()).collect(),
        }
    }
}

/// One internally consistent vector from persisted diagnostic bytes.
///
/// This is checked data, not proof of model execution, source membership, or
/// permission to train or activate a learned component.
pub struct CheckedPersistedEmbeddingVector {
    input_id: String,
    input_index: usize,
    exact_token_ids: Vec<u32>,
    row_count: u32,
    values: Vec<f32>,
}

impl fmt::Debug for CheckedPersistedEmbeddingVector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CheckedPersistedEmbeddingVector")
            .field("input_id", &self.input_id)
            .field("input_index", &self.input_index)
            .field("token_count", &self.exact_token_ids.len())
            .field("row_count", &self.row_count)
            .field("value_count", &self.values.len())
            .finish()
    }
}

impl CheckedPersistedEmbeddingVector {
    pub fn input_id(&self) -> &str {
        &self.input_id
    }

    pub const fn input_index(&self) -> usize {
        self.input_index
    }

    pub fn exact_token_ids(&self) -> &[u32] {
        &self.exact_token_ids
    }

    pub const fn row_count(&self) -> u32 {
        self.row_count
    }

    pub fn values(&self) -> &[f32] {
        &self.values
    }
}

/// Checked, self-consistent persisted embedding diagnostics.
///
/// Replaying JSON can never reconstruct the native owner-worker seal. No API
/// in this crate converts this type into a learning dataset, reward/ranker
/// activation, inference admission, or promotion authority.
pub struct CheckedPersistedEmbeddingDiagnostic {
    record_fingerprint: BlobId,
    request_id: String,
    binding_fingerprint: BlobId,
    resident_model_fingerprint: BlobId,
    execution_model_fingerprint: BlobId,
    tokenizer_fingerprint: BlobId,
    pooling: EmbeddingPoolingMode,
    normalization: EmbeddingNormalizationMode,
    dimensions: u32,
    outputs: Vec<CheckedPersistedEmbeddingVector>,
    request_fingerprint: BlobId,
    output_bits_fingerprint: BlobId,
    ledger_fingerprint: BlobId,
    owner_call_sequence: u64,
}

impl fmt::Debug for CheckedPersistedEmbeddingDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CheckedPersistedEmbeddingDiagnostic")
            .field("record_fingerprint", &self.record_fingerprint)
            .field("request_id", &self.request_id)
            .field("binding_fingerprint", &self.binding_fingerprint)
            .field("dimensions", &self.dimensions)
            .field("output_count", &self.outputs.len())
            .field("owner_call_sequence", &self.owner_call_sequence)
            .finish_non_exhaustive()
    }
}

impl CheckedPersistedEmbeddingDiagnostic {
    pub const fn record_fingerprint(&self) -> BlobId {
        self.record_fingerprint
    }

    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    pub const fn binding_fingerprint(&self) -> BlobId {
        self.binding_fingerprint
    }

    pub const fn resident_model_fingerprint(&self) -> BlobId {
        self.resident_model_fingerprint
    }

    pub const fn execution_model_fingerprint(&self) -> BlobId {
        self.execution_model_fingerprint
    }

    pub const fn tokenizer_fingerprint(&self) -> BlobId {
        self.tokenizer_fingerprint
    }

    pub const fn pooling(&self) -> EmbeddingPoolingMode {
        self.pooling
    }

    pub const fn normalization(&self) -> EmbeddingNormalizationMode {
        self.normalization
    }

    pub const fn dimensions(&self) -> u32 {
        self.dimensions
    }

    pub fn outputs(&self) -> &[CheckedPersistedEmbeddingVector] {
        &self.outputs
    }

    pub const fn request_fingerprint(&self) -> BlobId {
        self.request_fingerprint
    }

    pub const fn output_bits_fingerprint(&self) -> BlobId {
        self.output_bits_fingerprint
    }

    pub const fn ledger_fingerprint(&self) -> BlobId {
        self.ledger_fingerprint
    }

    pub const fn owner_call_sequence(&self) -> u64 {
        self.owner_call_sequence
    }
}

/// Strictly replay canonical embedding diagnostics without minting authority.
pub fn verify_persisted_embedding_diagnostic(
    bytes: &[u8],
) -> Result<CheckedPersistedEmbeddingDiagnostic, EmbeddingDiagnosticRecordError> {
    if bytes.is_empty() || bytes.len() > loom_research_types::MAX_BACKEND_EVIDENCE_BYTES {
        return Err(EmbeddingDiagnosticRecordError::EmptyOrOversized);
    }
    let record: PersistedEmbeddingDiagnosticRecord = serde_json::from_slice(bytes)?;
    if serde_json::to_vec(&record)? != bytes {
        return Err(EmbeddingDiagnosticRecordError::NonCanonical);
    }
    if record.format != EMBEDDING_DIAGNOSTIC_RECORD_FORMAT {
        return Err(EmbeddingDiagnosticRecordError::Invalid(
            "record format is unsupported",
        ));
    }
    let body_bytes = serde_json::to_vec(&record.body)?;
    if embedding_record_fingerprint(&body_bytes) != record.record_fingerprint {
        return Err(EmbeddingDiagnosticRecordError::Invalid(
            "record fingerprint mismatch",
        ));
    }
    validate_persisted_embedding_body(record.record_fingerprint, record.body)
}

fn validate_persisted_embedding_body(
    record_fingerprint: BlobId,
    body: PersistedEmbeddingDiagnosticBody,
) -> Result<CheckedPersistedEmbeddingDiagnostic, EmbeddingDiagnosticRecordError> {
    validate_embedding_binding(&body)?;
    validate_embedding_model_evidence(&body)?;
    validate_embedding_output_header(&body)?;
    validate_embedding_request_identity(&body)?;
    let checked = validate_embedding_vectors(&body)?;
    validate_embedding_native_receipts(&body)?;

    Ok(CheckedPersistedEmbeddingDiagnostic {
        record_fingerprint,
        request_id: body.request_id,
        binding_fingerprint: body.binding.binding_fingerprint,
        resident_model_fingerprint: body.resident_model_fingerprint,
        execution_model_fingerprint: body.execution_model_fingerprint,
        tokenizer_fingerprint: body.tokenizer_fingerprint,
        pooling: body.pooling,
        normalization: body.normalization,
        dimensions: body.dimensions,
        outputs: checked,
        request_fingerprint: body.request_fingerprint,
        output_bits_fingerprint: body.output_bits_fingerprint,
        ledger_fingerprint: body.ledger_fingerprint,
        owner_call_sequence: body.owner_call_sequence,
    })
}

fn validate_embedding_binding(
    body: &PersistedEmbeddingDiagnosticBody,
) -> Result<(), EmbeddingDiagnosticRecordError> {
    if body.binding.binding_id.is_empty()
        || body.binding.binding_id.len() > MAX_EMBEDDING_INPUT_ID_BYTES
        || body.binding.binding_id.chars().any(char::is_control)
        || body.binding.architecture.is_empty()
        || body.binding.architecture.len() > MAX_EMBEDDING_INPUT_ID_BYTES
        || body.binding.architecture.chars().any(char::is_control)
        || body.binding.model_byte_len == 0
        || body.binding.context_tokens == 0
    {
        return Err(EmbeddingDiagnosticRecordError::Invalid(
            "compiled binding diagnostics are malformed",
        ));
    }
    if body.binding.tokenizer_sha256 != body.tokenizer_fingerprint
        || body.binding.model_sha256.to_hex() != body.resident_model.model_sha256
        || body.binding.model_byte_len != body.resident_model.model_size
        || body.binding.tokenizer_sha256.to_hex() != body.resident_model.tokenizer_sha256
        || body.binding.multimodal_projector_sha256.map(BlobId::to_hex)
            != body.resident_model.multimodal_projector_sha256
        || body.binding.context_tokens > body.resident_model.context_tokens
    {
        return Err(EmbeddingDiagnosticRecordError::Invalid(
            "compiled binding does not match resident model evidence",
        ));
    }
    Ok(())
}

fn validate_embedding_model_evidence(
    body: &PersistedEmbeddingDiagnosticBody,
) -> Result<(), EmbeddingDiagnosticRecordError> {
    let resident_native = body.resident_model.as_native();
    let execution_native = body.execution_model.as_native();
    if model_fingerprint_id(&resident_native) != body.resident_model_fingerprint
        || model_fingerprint_id(&execution_native) != body.execution_model_fingerprint
        || body.output_model_id != body.resident_model.model_id
    {
        return Err(EmbeddingDiagnosticRecordError::Invalid(
            "semantic model fingerprint identity mismatch",
        ));
    }
    if body.resident_model.model_sha256 != body.execution_model.model_sha256
        || body.resident_model.model_size != body.execution_model.model_size
        || body.resident_model.tokenizer_sha256 != body.execution_model.tokenizer_sha256
        || body.resident_model.multimodal_projector_sha256
            != body.execution_model.multimodal_projector_sha256
    {
        return Err(EmbeddingDiagnosticRecordError::Invalid(
            "resident and embedding execution artifacts differ",
        ));
    }
    if !valid_embedding_model_fingerprint(&body.resident_model)
        || !valid_embedding_model_fingerprint(&body.execution_model)
    {
        return Err(EmbeddingDiagnosticRecordError::Invalid(
            "model fingerprint fields are malformed",
        ));
    }
    Ok(())
}

fn validate_embedding_output_header(
    body: &PersistedEmbeddingDiagnosticBody,
) -> Result<(), EmbeddingDiagnosticRecordError> {
    if body.output_model_id.is_empty()
        || body.output_model_id.len() > 1_024
        || body.output_model_id.chars().any(char::is_control)
        || body.dimensions == 0
        || body.dimensions > MAX_EMBEDDING_DIMENSIONS
        || body.outputs.is_empty()
        || body.outputs.len() > MAX_EMBEDDING_BATCH_INPUTS
    {
        return Err(EmbeddingDiagnosticRecordError::Invalid(
            "embedding output header is malformed",
        ));
    }
    Ok(())
}

fn validate_embedding_request_identity(
    body: &PersistedEmbeddingDiagnosticBody,
) -> Result<(), EmbeddingDiagnosticRecordError> {
    let expected_request_id = derive_persisted_embedding_request_id(body);
    if body.request_id != expected_request_id {
        return Err(EmbeddingDiagnosticRecordError::Invalid(
            "request ID does not bind exact ordered inputs",
        ));
    }
    if native_embedding_request_fingerprint(body) != body.request_fingerprint {
        return Err(EmbeddingDiagnosticRecordError::Invalid(
            "native request fingerprint mismatch",
        ));
    }
    Ok(())
}

fn validate_embedding_vectors(
    body: &PersistedEmbeddingDiagnosticBody,
) -> Result<Vec<CheckedPersistedEmbeddingVector>, EmbeddingDiagnosticRecordError> {
    let mut ids = HashSet::with_capacity(body.outputs.len());
    let mut total_tokens = 0_usize;
    let mut total_values = 0_usize;
    let mut checked = Vec::with_capacity(body.outputs.len());
    for (expected_index, output) in body.outputs.iter().enumerate() {
        total_tokens = total_tokens
            .checked_add(output.exact_token_ids.len())
            .ok_or(EmbeddingDiagnosticRecordError::Invalid(
                "embedding token count overflow",
            ))?;
        total_values = total_values.checked_add(output.value_bits.len()).ok_or(
            EmbeddingDiagnosticRecordError::Invalid("embedding value count overflow"),
        )?;
        let expected_rows = if body.pooling == EmbeddingPoolingMode::None {
            output.exact_token_ids.len()
        } else {
            1
        };
        let expected_values = expected_rows.checked_mul(body.dimensions as usize).ok_or(
            EmbeddingDiagnosticRecordError::Invalid("embedding output geometry overflow"),
        )?;
        let id_valid = !output.input_id.is_empty()
            && output.input_id.len() <= MAX_EMBEDDING_INPUT_ID_BYTES
            && !output.input_id.chars().any(char::is_control)
            && ids.insert(output.input_id.as_str());
        if output.input_index != expected_index
            || !id_valid
            || output.exact_token_ids.is_empty()
            || output.exact_token_ids.len() > MAX_EMBEDDING_INPUT_TOKENS
            || output
                .exact_token_ids
                .iter()
                .any(|token| *token > i32::MAX as u32)
            || output.row_count as usize != expected_rows
            || output.value_bits.len() != expected_values
        {
            return Err(EmbeddingDiagnosticRecordError::Invalid(
                "embedding vector identity or geometry mismatch",
            ));
        }
        let values = output
            .value_bits
            .iter()
            .map(|bits| f32::from_bits(*bits))
            .collect::<Vec<_>>();
        if values.iter().any(|value| !value.is_finite()) {
            return Err(EmbeddingDiagnosticRecordError::Invalid(
                "embedding vector contains a non-finite value",
            ));
        }
        checked.push(CheckedPersistedEmbeddingVector {
            input_id: output.input_id.clone(),
            input_index: output.input_index,
            exact_token_ids: output.exact_token_ids.clone(),
            row_count: output.row_count,
            values,
        });
    }
    if total_tokens > MAX_EMBEDDING_BATCH_TOKENS || total_values > MAX_EMBEDDING_BATCH_VALUES {
        return Err(EmbeddingDiagnosticRecordError::Invalid(
            "embedding batch exceeds the exact token or value bound",
        ));
    }
    Ok(checked)
}

fn validate_embedding_native_receipts(
    body: &PersistedEmbeddingDiagnosticBody,
) -> Result<(), EmbeddingDiagnosticRecordError> {
    if native_embedding_output_fingerprint(body) != body.output_bits_fingerprint {
        return Err(EmbeddingDiagnosticRecordError::Invalid(
            "native output-bits fingerprint mismatch",
        ));
    }
    if native_embedding_ledger_fingerprint(body) != body.ledger_fingerprint {
        return Err(EmbeddingDiagnosticRecordError::Invalid(
            "native owner-ledger fingerprint mismatch",
        ));
    }
    Ok(())
}

fn derive_persisted_embedding_request_id(body: &PersistedEmbeddingDiagnosticBody) -> String {
    let mut digest = Sha256::new();
    digest.update(EMBEDDING_REQUEST_DOMAIN.as_bytes());
    digest.update(body.binding.binding_fingerprint.as_bytes());
    digest.update(body.resident_model_fingerprint.as_bytes());
    digest.update([embedding_pooling_tag(body.pooling)]);
    digest.update([embedding_normalization_tag(body.normalization)]);
    for input in &body.outputs {
        digest.update((input.input_id.len() as u64).to_be_bytes());
        digest.update(input.input_id.as_bytes());
        digest.update((input.exact_token_ids.len() as u64).to_be_bytes());
        for token in &input.exact_token_ids {
            digest.update(token.to_be_bytes());
        }
    }
    format!("loom-embedding-v1-{:x}", digest.finalize())
}

fn embedding_record_fingerprint(body_bytes: &[u8]) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(EMBEDDING_DIAGNOSTIC_RECORD_DOMAIN.as_bytes());
    digest.update((body_bytes.len() as u64).to_be_bytes());
    digest.update(body_bytes);
    BlobId::from_bytes(digest.finalize().into())
}

fn valid_embedding_model_fingerprint(value: &PersistedEmbeddingModelFingerprint) -> bool {
    let bounded_text = [
        value.model_id.as_str(),
        value.binding_version.as_str(),
        value.build_id.as_str(),
        value.backend.as_str(),
    ]
    .into_iter()
    .all(|text| !text.is_empty() && text.len() <= 1_024 && !text.chars().any(char::is_control));
    let canonical_digests = [
        value.model_sha256.as_str(),
        value.tokenizer_sha256.as_str(),
        value.chat_template_sha256.as_str(),
        value.rope_config_sha256.as_str(),
        value.kv_layout_sha256.as_str(),
    ]
    .into_iter()
    .chain(value.multimodal_projector_sha256.as_deref())
    .all(is_lower_sha256);
    bounded_text
        && canonical_digests
        && value.model_size > 0
        && value.context_tokens > 0
        && value.batch_tokens > 0
        && value.max_sequences > 0
}

fn is_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn native_embedding_request_fingerprint(body: &PersistedEmbeddingDiagnosticBody) -> BlobId {
    let mut digest = NativeEmbeddingEvidenceDigest::new("native-embedding-request-v1");
    digest.text(&body.request_id);
    digest.text(&body.resident_model.model_id);
    digest.pooling(body.pooling);
    digest.normalization(body.normalization);
    digest.usize(body.outputs.len());
    for output in &body.outputs {
        digest.text(&output.input_id);
        digest.usize(output.exact_token_ids.len());
        for token in &output.exact_token_ids {
            // Vector validation rejects values above `i32::MAX` before any
            // replay fingerprint is accepted.
            digest.i32((*token).cast_signed());
        }
    }
    digest.finish()
}

fn native_embedding_output_fingerprint(body: &PersistedEmbeddingDiagnosticBody) -> BlobId {
    let mut digest = NativeEmbeddingEvidenceDigest::new("native-embedding-output-bits-v1");
    digest.text(&body.request_id);
    digest.text(&body.output_model_id);
    digest.pooling(body.pooling);
    digest.normalization(body.normalization);
    digest.u32(body.dimensions);
    digest.model_fingerprint(&body.execution_model);
    digest.byte(0); // NativeTransport::InProcess
    digest.byte(1); // real_engine_invoked
    digest.byte(0); // fake_fixture
    digest.usize(body.outputs.len());
    for output in &body.outputs {
        digest.text(&output.input_id);
        digest.usize(output.input_index);
        digest.usize(output.exact_token_ids.len());
        for token in &output.exact_token_ids {
            // Vector validation rejects values above `i32::MAX` before any
            // replay fingerprint is accepted.
            digest.i32((*token).cast_signed());
        }
        digest.u32(output.row_count);
        digest.usize(output.value_bits.len());
        for bits in &output.value_bits {
            digest.u32(*bits);
        }
    }
    digest.finish()
}

fn native_embedding_ledger_fingerprint(body: &PersistedEmbeddingDiagnosticBody) -> BlobId {
    let mut digest = NativeEmbeddingEvidenceDigest::new("native-embedding-owner-ledger-v1");
    digest.text(&body.request_fingerprint.to_hex());
    digest.model_fingerprint(&body.resident_model);
    digest.model_fingerprint(&body.execution_model);
    digest.pooling(body.pooling);
    digest.normalization(body.normalization);
    digest.u32(body.dimensions);
    digest.text(&body.output_bits_fingerprint.to_hex());
    digest.u64(body.owner_call_sequence);
    digest.byte(0); // NativeTransport::InProcess
    digest.text(LLAMA_NATIVE_BUILD_MANIFEST_SHA256);
    digest.text("completed");
    digest.finish()
}

struct NativeEmbeddingEvidenceDigest(Sha256);

impl NativeEmbeddingEvidenceDigest {
    fn new(domain: &str) -> Self {
        let mut value = Self(Sha256::new());
        value.text(domain);
        value
    }

    fn text(&mut self, value: &str) {
        self.u64(value.len() as u64);
        self.0.update(value.as_bytes());
    }

    fn byte(&mut self, value: u8) {
        self.0.update([value]);
    }

    fn i32(&mut self, value: i32) {
        self.0.update(value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.0.update(value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.0.update(value.to_le_bytes());
    }

    fn usize(&mut self, value: usize) {
        self.u64(value as u64);
    }

    fn pooling(&mut self, value: EmbeddingPoolingMode) {
        self.byte(embedding_pooling_tag(value));
    }

    fn normalization(&mut self, value: EmbeddingNormalizationMode) {
        self.byte(embedding_normalization_tag(value));
    }

    fn model_fingerprint(&mut self, value: &PersistedEmbeddingModelFingerprint) {
        self.text(&value.model_id);
        self.u64(value.model_size);
        self.text(&value.model_sha256);
        self.text(&value.tokenizer_sha256);
        self.text(&value.chat_template_sha256);
        if let Some(projector) = &value.multimodal_projector_sha256 {
            self.byte(1);
            self.text(projector);
        } else {
            self.byte(0);
        }
        self.text(&value.binding_version);
        self.text(&value.build_id);
        self.text(&value.backend);
        self.u32(value.context_tokens);
        self.u32(value.batch_tokens);
        self.u32(value.max_sequences);
        self.text(&value.rope_config_sha256);
        self.text(&value.kv_layout_sha256);
    }

    fn finish(self) -> BlobId {
        BlobId::from_bytes(self.0.finalize().into())
    }
}

/// Verified diagnostic data plus optional final-lifecycle lineage.
pub struct VerifiedEmbeddingDiagnostic {
    data: EmbeddingDiagnosticData,
    lineage: EmbeddingWorkerLineage,
}

impl fmt::Debug for VerifiedEmbeddingDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedEmbeddingDiagnostic")
            .field("data", &self.data)
            .field("owner_call_sequence", &self.lineage.owner_call_sequence())
            .finish_non_exhaustive()
    }
}

impl std::ops::Deref for VerifiedEmbeddingDiagnostic {
    type Target = EmbeddingDiagnosticData;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl VerifiedEmbeddingDiagnostic {
    pub fn into_data(self) -> EmbeddingDiagnosticData {
        self.data
    }

    pub fn into_parts(self) -> (EmbeddingDiagnosticData, EmbeddingWorkerLineage) {
        (self.data, self.lineage)
    }

    pub fn bind_joined(
        self,
        joined: &JoinedNativeModel,
    ) -> Result<JoinedEmbeddingDiagnostic, ControlledInferenceError> {
        let proof = self.lineage.bind_joined(joined)?;
        Ok(JoinedEmbeddingDiagnostic {
            data: self.data,
            proof,
        })
    }
}

pub struct JoinedEmbeddingDiagnostic {
    data: EmbeddingDiagnosticData,
    proof: JoinedEmbeddingLineage,
}

impl fmt::Debug for JoinedEmbeddingDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JoinedEmbeddingDiagnostic")
            .field("data", &self.data)
            .field("proof", &self.proof)
            .finish()
    }
}

impl std::ops::Deref for JoinedEmbeddingDiagnostic {
    type Target = EmbeddingDiagnosticData;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl JoinedEmbeddingDiagnostic {
    pub const fn lineage(&self) -> &JoinedEmbeddingLineage {
        &self.proof
    }

    pub fn into_data(self) -> EmbeddingDiagnosticData {
        self.data
    }
}

impl ExactEmbeddingBackend for NativeLlamaWriter {
    type Request = ExactEmbeddingRequest;
    type Ticket = VerifiedEmbeddingTicket;
    type VerifiedDiagnostic = VerifiedEmbeddingDiagnostic;
    type Error = ControlledInferenceError;

    fn start_embeddings(&self, request: Self::Request) -> Result<Self::Ticket, Self::Error> {
        start_embeddings(self, request)
    }

    fn wait_embeddings(
        &self,
        ticket: Self::Ticket,
    ) -> Result<Self::VerifiedDiagnostic, Self::Error> {
        ticket.wait()
    }
}

fn start_embeddings(
    writer: &NativeLlamaWriter,
    request: ExactEmbeddingRequest,
) -> Result<VerifiedEmbeddingTicket, ControlledInferenceError> {
    let status = writer.handle.status();
    let fingerprint = status
        .fingerprint
        .ok_or(InferenceError::ResidentModelHasNoFingerprint)?;
    request
        .binding
        .verify_native_model(&fingerprint)
        .map_err(InferenceError::from)?;
    let mut digest = Sha256::new();
    digest.update(EMBEDDING_REQUEST_DOMAIN.as_bytes());
    digest.update(request.binding.fingerprint().as_bytes());
    digest.update(model_fingerprint_id(&fingerprint).as_bytes());
    digest.update([embedding_pooling_tag(request.pooling)]);
    digest.update([embedding_normalization_tag(request.normalization)]);
    let native_inputs = request
        .inputs
        .iter()
        .map(|input| {
            digest.update((input.input_id.len() as u64).to_be_bytes());
            digest.update(input.input_id.as_bytes());
            digest.update((input.token_ids.len() as u64).to_be_bytes());
            for token in &input.token_ids {
                digest.update(token.to_be_bytes());
            }
            EmbeddingInput::new(input.input_id.clone(), to_i32_tokens(&input.token_ids)?)
                .map_err(Into::into)
        })
        .collect::<Result<Vec<_>, ControlledInferenceError>>()?;
    let request_id = format!("loom-embedding-v1-{:x}", digest.finalize());
    let native_request = EmbeddingBatchRequest::new(
        request_id,
        fingerprint.model_id.clone(),
        native_inputs,
        request.pooling.into(),
        request.normalization.into(),
    )?;
    let native_ticket = writer.handle.embed_batch(native_request.clone())?;
    Ok(VerifiedEmbeddingTicket {
        native_ticket: Some(native_ticket),
        pending: Some(PendingEmbedding {
            binding: request.binding,
            request: native_request,
            resident_model_fingerprint: fingerprint,
            pooling: request.pooling,
            normalization: request.normalization,
        }),
        handle: Some(writer.handle.clone()),
    })
}

fn mint_embedding_diagnostic(
    pending: PendingEmbedding,
    seal: &VerifiedEmbeddingBatch,
) -> Result<EmbeddingDiagnosticData, ControlledInferenceError> {
    if seal.terminal() != VerifiedEmbeddingTerminal::Completed
        || seal.request() != &pending.request
        || seal.resident_model_fingerprint() != &pending.resident_model_fingerprint
        || seal.requested_pooling() != pending.request.pooling()
        || seal.requested_normalization() != pending.request.normalization()
        || seal.transport() != NativeTransport::InProcess
        || seal.output().evidence().fake_fixture()
        || !seal.output().evidence().real_engine_invoked()
        || seal.output().outputs().len() != pending.request.inputs().len()
    {
        return Err(ControlledInferenceError::EmbeddingSealMismatch);
    }
    pending
        .binding
        .verify_native_model(seal.resident_model_fingerprint())
        .map_err(InferenceError::from)?;
    let dimensions = seal.resolved_config().dimensions();
    let outputs = seal
        .output()
        .outputs()
        .iter()
        .zip(pending.request.inputs())
        .enumerate()
        .map(|(index, (output, input))| {
            if output.input_index() != index
                || output.input_id() != input.input_id()
                || output.token_ids() != input.token_ids()
                || output.values().iter().any(|value| !value.is_finite())
            {
                return Err(ControlledInferenceError::EmbeddingSealMismatch);
            }
            Ok(VerifiedEmbeddingVector {
                input_id: output.input_id().to_string(),
                input_index: index,
                exact_token_ids: output
                    .token_ids()
                    .iter()
                    .map(|token| {
                        u32::try_from(*token)
                            .map_err(|_| ControlledInferenceError::EmbeddingSealMismatch)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                row_count: output.row_count(),
                values: output.values().to_vec(),
            })
        })
        .collect::<Result<Vec<_>, ControlledInferenceError>>()?;
    Ok(EmbeddingDiagnosticData {
        request_id: pending.request.request_id().to_string(),
        output_model_id: seal.output().model_id().to_string(),
        binding: pending.binding,
        resident_model: PersistedEmbeddingModelFingerprint::from(seal.resident_model_fingerprint()),
        execution_model: PersistedEmbeddingModelFingerprint::from(seal.execution_fingerprint()),
        resident_model_fingerprint: model_fingerprint_id(seal.resident_model_fingerprint()),
        execution_model_fingerprint: model_fingerprint_id(seal.execution_fingerprint()),
        tokenizer_fingerprint: parse_sha256(&seal.resident_model_fingerprint().tokenizer_sha256)?,
        pooling: pending.pooling,
        normalization: pending.normalization,
        dimensions,
        outputs,
        request_fingerprint: parse_sha256(seal.request_sha256())?,
        output_bits_fingerprint: parse_sha256(seal.output_bits_sha256())?,
        ledger_fingerprint: parse_sha256(seal.ledger_sha256())?,
        owner_call_sequence: seal.owner_call_sequence(),
    })
}

const fn embedding_pooling_tag(value: EmbeddingPoolingMode) -> u8 {
    match value {
        EmbeddingPoolingMode::None => 0,
        EmbeddingPoolingMode::Mean => 1,
        EmbeddingPoolingMode::Cls => 2,
        EmbeddingPoolingMode::Last => 3,
        EmbeddingPoolingMode::Rank => 4,
    }
}

const fn embedding_normalization_tag(value: EmbeddingNormalizationMode) -> u8 {
    match value {
        EmbeddingNormalizationMode::None => 0,
        EmbeddingNormalizationMode::L2 => 1,
    }
}

fn parse_sha256(value: &str) -> Result<BlobId, ControlledInferenceError> {
    BlobId::from_str(value).map_err(|_| ControlledInferenceError::ControlledSealMismatch)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diagnostic_model(kv_label: &[u8]) -> PersistedEmbeddingModelFingerprint {
        PersistedEmbeddingModelFingerprint {
            model_id: "diagnostic-model".to_owned(),
            model_size: 4096,
            model_sha256: BlobId::digest(b"model").to_hex(),
            tokenizer_sha256: BlobId::digest(b"tokenizer").to_hex(),
            chat_template_sha256: BlobId::digest(b"template").to_hex(),
            multimodal_projector_sha256: None,
            binding_version: "diagnostic-binding-v1".to_owned(),
            build_id: "diagnostic-build-v1".to_owned(),
            backend: "cpu".to_owned(),
            context_tokens: 128,
            batch_tokens: 32,
            max_sequences: 1,
            rope_config_sha256: BlobId::digest(b"rope").to_hex(),
            kv_layout_sha256: BlobId::digest(kv_label).to_hex(),
        }
    }

    fn diagnostic_body() -> PersistedEmbeddingDiagnosticBody {
        let resident_model = diagnostic_model(b"resident-kv");
        let execution_model = diagnostic_model(b"embedding-kv");
        let resident_model_fingerprint = model_fingerprint_id(&resident_model.as_native());
        let execution_model_fingerprint = model_fingerprint_id(&execution_model.as_native());
        let tokenizer =
            BlobId::from_str(&resident_model.tokenizer_sha256).expect("fixture tokenizer digest");
        let mut body = PersistedEmbeddingDiagnosticBody {
            request_id: String::new(),
            output_model_id: resident_model.model_id.clone(),
            binding: PersistedEmbeddingBinding {
                binding_id: "writer".to_owned(),
                binding_fingerprint: BlobId::digest(b"binding"),
                manifest_fingerprint: BlobId::digest(b"manifest"),
                model_sha256: BlobId::from_str(&resident_model.model_sha256)
                    .expect("fixture model digest"),
                model_byte_len: resident_model.model_size,
                tokenizer_sha256: tokenizer,
                multimodal_projector_sha256: None,
                architecture: "diagnostic".to_owned(),
                context_tokens: 64,
            },
            resident_model,
            execution_model,
            resident_model_fingerprint,
            execution_model_fingerprint,
            tokenizer_fingerprint: tokenizer,
            pooling: EmbeddingPoolingMode::None,
            normalization: EmbeddingNormalizationMode::None,
            dimensions: 2,
            outputs: vec![PersistedEmbeddingVector {
                input_id: "tail".to_owned(),
                input_index: 0,
                exact_token_ids: vec![7, 11],
                row_count: 2,
                value_bits: [0.25_f32, -0.5, 0.75, 1.0]
                    .into_iter()
                    .map(f32::to_bits)
                    .collect(),
            }],
            request_fingerprint: BlobId::digest(b"pending-request"),
            output_bits_fingerprint: BlobId::digest(b"pending-output"),
            ledger_fingerprint: BlobId::digest(b"pending-ledger"),
            owner_call_sequence: 3,
        };
        body.request_id = derive_persisted_embedding_request_id(&body);
        body.request_fingerprint = native_embedding_request_fingerprint(&body);
        body.output_bits_fingerprint = native_embedding_output_fingerprint(&body);
        body.ledger_fingerprint = native_embedding_ledger_fingerprint(&body);
        body
    }

    fn diagnostic_record(body: PersistedEmbeddingDiagnosticBody) -> Vec<u8> {
        let body_bytes = serde_json::to_vec(&body).expect("fixture body JSON");
        serde_json::to_vec(&PersistedEmbeddingDiagnosticRecord {
            format: EMBEDDING_DIAGNOSTIC_RECORD_FORMAT.to_owned(),
            record_fingerprint: embedding_record_fingerprint(&body_bytes),
            body,
        })
        .expect("fixture record JSON")
    }

    #[test]
    fn exact_embedding_input_rejects_empty_and_out_of_range_tokens() {
        assert!(ExactEmbeddingInput::new("empty", Vec::new()).is_err());
        assert!(ExactEmbeddingInput::new("negative-native", vec![i32::MAX as u32 + 1]).is_err());
        assert!(ExactEmbeddingInput::new("", vec![1]).is_err());
        assert!(ExactEmbeddingInput::new("line\nbreak", vec![1]).is_err());
    }

    #[test]
    fn hidden_interventions_are_explicitly_unavailable() {
        assert_eq!(
            format!(
                "{}",
                ControlledInferenceError::HiddenInterventionUnavailable(
                    UnavailableHiddenIntervention::JSpaceProjection
                )
            ),
            "JSpaceProjection is unavailable because upstream exposes no required operation"
        );
    }

    #[test]
    fn canonical_embedding_diagnostic_replay_is_exact_but_never_authority() {
        let bytes = diagnostic_record(diagnostic_body());
        let checked = verify_persisted_embedding_diagnostic(&bytes)
            .expect("internally consistent diagnostic record replays");
        assert_eq!(checked.outputs().len(), 1);
        assert_eq!(checked.outputs()[0].exact_token_ids(), [7, 11]);
        assert_eq!(checked.outputs()[0].values(), [0.25, -0.5, 0.75, 1.0]);
        assert_eq!(checked.owner_call_sequence(), 3);

        let mut noncanonical = bytes.clone();
        noncanonical.push(b'\n');
        assert!(matches!(
            verify_persisted_embedding_diagnostic(&noncanonical),
            Err(EmbeddingDiagnosticRecordError::NonCanonical)
        ));

        let mut changed_token = diagnostic_body();
        changed_token.outputs[0].exact_token_ids[0] = 8;
        assert!(matches!(
            verify_persisted_embedding_diagnostic(&diagnostic_record(changed_token)),
            Err(EmbeddingDiagnosticRecordError::Invalid(
                "request ID does not bind exact ordered inputs"
            ))
        ));

        let mut changed_value = diagnostic_body();
        changed_value.outputs[0].value_bits[0] = 0.5_f32.to_bits();
        assert!(matches!(
            verify_persisted_embedding_diagnostic(&diagnostic_record(changed_value)),
            Err(EmbeddingDiagnosticRecordError::Invalid(
                "native output-bits fingerprint mismatch"
            ))
        ));

        let mut changed_sequence = diagnostic_body();
        changed_sequence.owner_call_sequence += 1;
        assert!(matches!(
            verify_persisted_embedding_diagnostic(&diagnostic_record(changed_sequence)),
            Err(EmbeddingDiagnosticRecordError::Invalid(
                "native owner-ledger fingerprint mismatch"
            ))
        ));
    }

    #[test]
    fn zero_is_valid_in_each_typed_owner_sequence_namespace() {
        let mut zero_embedding = diagnostic_body();
        let nonzero_ledger = zero_embedding.ledger_fingerprint;
        zero_embedding.owner_call_sequence = 0;
        zero_embedding.ledger_fingerprint = native_embedding_ledger_fingerprint(&zero_embedding);
        assert_ne!(zero_embedding.ledger_fingerprint, nonzero_ledger);

        let checked = verify_persisted_embedding_diagnostic(&diagnostic_record(zero_embedding))
            .expect("the first embedding call has native owner sequence zero");
        assert_eq!(checked.owner_call_sequence(), 0);

        let controlled = JoinedControlledLineage {
            owner_call_sequence: 0,
        };
        let embedding = JoinedEmbeddingLineage {
            owner_call_sequence: 0,
        };
        assert_eq!(controlled.owner_call_sequence(), 0);
        assert_eq!(embedding.owner_call_sequence(), 0);
    }
}
