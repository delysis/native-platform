//! Owner-thread execution for the additive controlled-generation contract.
//!
//! The public request remains content-addressed and serializable. Resolved
//! constraint text is an ephemeral submission attachment: its exact length and
//! digest are checked before queue admission, then the body is consumed only by
//! the resident model owner thread.

use super::*;
use crate::control_math::{
    PowerTemperature, ValidatedLogits, VocabularyIdentity, classifier_free_guidance,
    power_temperature_transform,
};
use llama_cpp_2::json_schema_to_grammar;
use llama_cpp_2::token::data::LlamaTokenData;
use llama_cpp_2::token::data_array::LlamaTokenDataArray;
use llama_cpp_2::token_type::LlamaTokenAttr;
use llama_native_types::{
    AppliedControlKind, AppliedControlReport, BackendParticipantReport, ControlApplicationStage,
    ControlledGenerationBatchOutput, ControlledGenerationBatchRequest,
    ControlledGenerationCapabilities, ControlledGenerationCaseOutput, ControlledModelIdentity,
    DistributionObservationPolicy, DistributionTokenValue, DistributionValueKind,
    ExpectedControlApplication, ExtendedSampler, GenerationEvent, GenerationEventKind,
    GuidanceControl, MAX_CONSTRAINT_ARTIFACT_BYTES, MAX_DISTRIBUTION_OBSERVATION_TOP_K,
    MAX_TOKEN_PIECE_BYTES, ProbabilityStage, RankedDistributionCandidate,
    StageDistributionObservation, StructuredConstraint, TerminalSelector, TokenContractIdentity,
    TokenDistributionObservation, UnverifiedBackendControlDeclaration,
};

/// A frozen controlled request plus the one optional immutable constraint body
/// resolved by the embedding product.
///
/// This type is intentionally not serializable. Persistence uses the request
/// and its content-addressed reference; accepting arbitrary stored bytes as a
/// live grammar would erase the artifact boundary.
#[derive(Debug)]
pub struct ControlledGenerationSubmission {
    request: ControlledGenerationBatchRequest,
    constraint_body: Option<String>,
}

impl ControlledGenerationSubmission {
    /// Bind an exact constraint body to the request's immutable reference.
    /// Requests without a constraint must pass `None`.
    pub fn new(
        request: ControlledGenerationBatchRequest,
        constraint_body: Option<String>,
    ) -> NativeResult<Self> {
        validate_constraint_attachment(&request, constraint_body.as_deref())?;
        Ok(Self {
            request,
            constraint_body,
        })
    }

    #[must_use]
    pub const fn request(&self) -> &ControlledGenerationBatchRequest {
        &self.request
    }

    #[must_use]
    pub fn constraint_body(&self) -> Option<&str> {
        self.constraint_body.as_deref()
    }
}

/// Non-forgeable owner-worker authority for one controlled batch.
///
/// The seal has no public constructor and implements neither `Clone`,
/// `Default`, nor Serde. Its persistable [`ControlledGenerationBatchOutput`]
/// remains explicitly unverified after separation from this value.
///
/// ```compile_fail
/// use llama_native_engine::VerifiedControlledGenerationBatch;
/// fn clone_seal(value: &VerifiedControlledGenerationBatch) -> VerifiedControlledGenerationBatch {
///     value.clone()
/// }
/// ```
///
/// ```compile_fail
/// use llama_native_engine::VerifiedControlledGenerationBatch;
/// fn require_serialize<T: serde::Serialize>() {}
/// require_serialize::<VerifiedControlledGenerationBatch>();
/// ```
///
/// ```compile_fail
/// use llama_native_engine::VerifiedControlledGenerationBatch;
/// fn require_deserialize<T: for<'de> serde::Deserialize<'de>>() {}
/// require_deserialize::<VerifiedControlledGenerationBatch>();
/// ```
pub struct VerifiedControlledGenerationBatch {
    output: ControlledGenerationBatchOutput,
    model_fingerprint: ModelFingerprint,
    request_sha256: String,
    output_sha256: String,
    event_stream_sha256: String,
    runtime_operation_ledger_sha256: String,
    ledger_sha256: String,
    owner_call_sequence: u64,
    terminal: VerifiedControlledGenerationTerminal,
    runtime_cost: ControlledRuntimeCostEvidence,
    terminal_sampled_token_ids: Vec<Option<i32>>,
    events: Vec<GenerationEvent>,
    worker_identity: Arc<WorkerIdentity>,
}

/// The only successful terminal carried by a verified controlled batch.
/// Cancelled and failed batches remain inspectable diagnostics, never seals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifiedControlledGenerationTerminal {
    Completed,
}

/// Exact physical execution shape observed by the owner worker.
///
/// Frozen request cost remains the conservative logical charge. This evidence
/// separately records token-exact KV sharing and the physical context reserve
/// used by the live batched execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlledRuntimeCostEvidence {
    conditional_shared_prefix_tokens: usize,
    unconditional_shared_prefix_tokens: usize,
    physical_prompt_evaluations: u64,
    reserved_physical_context_cells: u64,
    sequence_slots: u32,
}

impl ControlledRuntimeCostEvidence {
    #[must_use]
    pub const fn conditional_shared_prefix_tokens(self) -> usize {
        self.conditional_shared_prefix_tokens
    }

    #[must_use]
    pub const fn unconditional_shared_prefix_tokens(self) -> usize {
        self.unconditional_shared_prefix_tokens
    }

    #[must_use]
    pub const fn physical_prompt_evaluations(self) -> u64 {
        self.physical_prompt_evaluations
    }

    #[must_use]
    pub const fn reserved_physical_context_cells(self) -> u64 {
        self.reserved_physical_context_cells
    }

    #[must_use]
    pub const fn sequence_slots(self) -> u32 {
        self.sequence_slots
    }
}

impl std::fmt::Debug for VerifiedControlledGenerationBatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VerifiedControlledGenerationBatch")
            .field("request_id", &self.output.request().request_id())
            .field("model_id", &self.model_fingerprint.model_id)
            .field("model_sha256", &self.model_fingerprint.model_sha256)
            .field("case_count", &self.output.cases().len())
            .field("event_count", &self.events.len())
            .field("request_sha256", &self.request_sha256)
            .field("output_sha256", &self.output_sha256)
            .field(
                "runtime_operation_ledger_sha256",
                &self.runtime_operation_ledger_sha256,
            )
            .field("ledger_sha256", &self.ledger_sha256)
            .field("owner_call_sequence", &self.owner_call_sequence)
            .field("terminal", &self.terminal)
            .field("runtime_cost", &self.runtime_cost)
            .finish()
    }
}

impl VerifiedControlledGenerationBatch {
    #[must_use]
    pub const fn output(&self) -> &ControlledGenerationBatchOutput {
        &self.output
    }

    #[must_use]
    pub const fn model_fingerprint(&self) -> &ModelFingerprint {
        &self.model_fingerprint
    }

    #[must_use]
    pub fn request_sha256(&self) -> &str {
        &self.request_sha256
    }

    #[must_use]
    pub fn output_sha256(&self) -> &str {
        &self.output_sha256
    }

    #[must_use]
    pub fn event_stream_sha256(&self) -> &str {
        &self.event_stream_sha256
    }

    #[must_use]
    pub fn runtime_operation_ledger_sha256(&self) -> &str {
        &self.runtime_operation_ledger_sha256
    }

    #[must_use]
    pub fn ledger_sha256(&self) -> &str {
        &self.ledger_sha256
    }

    #[must_use]
    pub const fn owner_call_sequence(&self) -> u64 {
        self.owner_call_sequence
    }

    #[must_use]
    pub const fn terminal(&self) -> VerifiedControlledGenerationTerminal {
        self.terminal
    }

    #[must_use]
    pub const fn runtime_cost(&self) -> ControlledRuntimeCostEvidence {
        self.runtime_cost
    }

    #[must_use]
    pub fn terminal_sampled_token_ids(&self) -> &[Option<i32>] {
        &self.terminal_sampled_token_ids
    }

    #[must_use]
    pub fn events(&self) -> &[GenerationEvent] {
        &self.events
    }

    /// Bind this completion to the exact model owner thread that later joined.
    #[must_use]
    pub fn belongs_to_joined_model(&self, joined: &JoinedNativeModel) -> bool {
        Arc::ptr_eq(&self.worker_identity, &joined.worker_identity)
    }

    /// Discard live authority and retain only the persistable, explicitly
    /// unverified receipt envelope.
    #[must_use]
    pub fn into_unverified_output(self) -> ControlledGenerationBatchOutput {
        self.output
    }
}

#[derive(Debug)]
pub(crate) struct ControlledGenerationEvidence {
    model_fingerprint: ModelFingerprint,
    request_sha256: String,
    output_sha256: String,
    event_stream_sha256: String,
    runtime_operation_ledger_sha256: String,
    ledger_sha256: String,
    owner_call_sequence: u64,
    runtime_cost: ControlledRuntimeCostEvidence,
    terminal_sampled_token_ids: Vec<Option<i32>>,
    events: Vec<GenerationEvent>,
    worker_identity: Arc<WorkerIdentity>,
}

#[derive(Debug)]
pub(crate) struct ControlledGenerationCompletion {
    output: ControlledGenerationBatchOutput,
    authority: NativeResult<Box<ControlledGenerationEvidence>>,
}

impl ControlledGenerationCompletion {
    fn authority_rejected(output: ControlledGenerationBatchOutput, error: NativeError) -> Self {
        Self {
            output,
            authority: Err(error),
        }
    }

    fn verified(
        output: ControlledGenerationBatchOutput,
        evidence: ControlledGenerationEvidence,
    ) -> Self {
        Self {
            output,
            authority: Ok(Box::new(evidence)),
        }
    }

    fn into_output(self) -> ControlledGenerationBatchOutput {
        self.output
    }

    fn into_verified(self) -> NativeResult<VerifiedControlledGenerationBatch> {
        let Self { output, authority } = self;
        let evidence = *authority?;
        Ok(VerifiedControlledGenerationBatch {
            output,
            model_fingerprint: evidence.model_fingerprint,
            request_sha256: evidence.request_sha256,
            output_sha256: evidence.output_sha256,
            event_stream_sha256: evidence.event_stream_sha256,
            runtime_operation_ledger_sha256: evidence.runtime_operation_ledger_sha256,
            ledger_sha256: evidence.ledger_sha256,
            owner_call_sequence: evidence.owner_call_sequence,
            terminal: VerifiedControlledGenerationTerminal::Completed,
            runtime_cost: evidence.runtime_cost,
            terminal_sampled_token_ids: evidence.terminal_sampled_token_ids,
            events: evidence.events,
            worker_identity: evidence.worker_identity,
        })
    }
}

/// One bounded, independently cancellable controlled-generation submission.
#[derive(Debug)]
pub struct ControlledGenerationTicket {
    pub request_id: String,
    pub events: Receiver<GenerationEvent>,
    result: Receiver<NativeResult<ControlledGenerationCompletion>>,
    control: Arc<ActiveRequest>,
}

impl ControlledGenerationTicket {
    pub fn cancel_case(&self, case_id: &str) -> bool {
        self.control.cancel_named(case_id)
    }

    pub fn cancel_all(&self) -> usize {
        self.control.cancel_all()
    }

    pub fn wait(self) -> NativeResult<ControlledGenerationBatchOutput> {
        let result = self.result.recv().map_err(|error| {
            NativeError::new(
                NativeErrorCode::WorkerStopped,
                format!("native worker stopped before returning controlled generation: {error}"),
            )
        })?;
        result.map(ControlledGenerationCompletion::into_output)
    }

    pub fn wait_timeout(
        self,
        timeout: Duration,
    ) -> NativeResult<WaitOutcome<Self, ControlledGenerationBatchOutput>> {
        match self.result.recv_timeout(timeout) {
            Ok(result) => result
                .map(ControlledGenerationCompletion::into_output)
                .map(WaitOutcome::Ready),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Ok(WaitOutcome::TimedOut(self)),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => Err(NativeError::new(
                NativeErrorCode::WorkerStopped,
                "native worker stopped before returning controlled generation",
            )),
        }
    }

    pub fn wait_verified(self) -> NativeResult<VerifiedControlledGenerationBatch> {
        let result = self.result.recv().map_err(|error| {
            NativeError::new(
                NativeErrorCode::WorkerStopped,
                format!(
                    "native worker stopped before returning verified controlled generation: {error}"
                ),
            )
        })?;
        result?.into_verified()
    }

    pub fn wait_verified_timeout(
        self,
        timeout: Duration,
    ) -> NativeResult<WaitOutcome<Self, VerifiedControlledGenerationBatch>> {
        match self.result.recv_timeout(timeout) {
            Ok(result) => result?.into_verified().map(WaitOutcome::Ready),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Ok(WaitOutcome::TimedOut(self)),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => Err(NativeError::new(
                NativeErrorCode::WorkerStopped,
                "native worker stopped before returning verified controlled generation",
            )),
        }
    }
}

impl Drop for ControlledGenerationTicket {
    fn drop(&mut self) {
        self.control.cancel_all();
    }
}

impl NativeModelHandle {
    /// Exact control features implemented by the single-resident owner worker.
    /// Multi-model arithmetic and static profiles remain explicitly disabled.
    #[must_use]
    pub fn controlled_generation_capabilities(&self) -> ControlledGenerationCapabilities {
        ControlledGenerationCapabilities::inspected(true, false, true, false, false)
    }

    /// Obtain the live writer identity required to compile a fingerprint-bound
    /// controlled request. Vocabulary, special-token, and token-byte digests
    /// are computed on the model owner thread, not accepted from the caller.
    pub fn controlled_model_identity(
        &self,
        participant_id: impl Into<String>,
    ) -> NativeResult<ControlledModelIdentity> {
        self.inner.ensure_accepting()?;
        let participant_id = participant_id.into();
        validate_public_id("controlled participant_id", &participant_id)?;
        let (response_tx, response_rx) = bounded(1);
        self.inner.send_command(
            WorkerCommand::InspectControlledIdentity {
                participant_id,
                response: response_tx,
            },
            "inspecting the controlled-generation model identity",
        )?;
        response_rx.recv().map_err(|error| {
            NativeError::new(
                NativeErrorCode::WorkerStopped,
                format!("native worker stopped during controlled identity inspection: {error}"),
            )
        })?
    }

    /// Admit one frozen exact-token controlled batch to the resident worker.
    pub fn generate_controlled(
        &self,
        submission: ControlledGenerationSubmission,
    ) -> NativeResult<ControlledGenerationTicket> {
        preflight_submission(&submission, &self.status())?;

        let request_id = submission.request().request_id().to_string();
        let mut flags = Vec::with_capacity(submission.request().cases().len());
        for _ in submission.request().cases() {
            flags.push(Arc::new(AtomicBool::new(false)));
        }

        let (event_tx, event_rx) = bounded(EVENT_CAPACITY);
        let (result_tx, result_rx) = bounded(1);
        let admitted_request_sha256 = submission.request().fingerprint_sha256();
        let owned_cancellations = submission
            .request()
            .cases()
            .iter()
            .zip(&flags)
            .map(|(case, flag)| (case.case_id().to_string(), Arc::clone(flag)))
            .collect::<Vec<_>>();
        let control = self.inner.admit_command(
            request_id.clone(),
            RequestClass::ControlledGeneration,
            RequestControls::ControlledGeneration {
                cancellations: owned_cancellations,
            },
            |request_lease| WorkerCommand::ControlledGenerate {
                submission: Box::new(submission),
                admitted_request_sha256,
                event_tx,
                result_tx,
                cancellations: flags,
                request_lease,
            },
            "submitting controlled generation",
        )?;
        Ok(ControlledGenerationTicket {
            request_id,
            events: event_rx,
            result: result_rx,
            control,
        })
    }
}

fn validate_public_id(field: &str, value: &str) -> NativeResult<()> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(NativeError::new(
            NativeErrorCode::InvalidConfig,
            format!("{field} must contain 1 to 256 UTF-8 bytes and no control characters"),
        ));
    }
    Ok(())
}

fn validate_constraint_attachment(
    request: &ControlledGenerationBatchRequest,
    body: Option<&str>,
) -> NativeResult<()> {
    match (request.control().constraint(), body) {
        (None, None) => Ok(()),
        (None, Some(_)) => Err(NativeError::new(
            NativeErrorCode::InvalidConfig,
            "a controlled request without a constraint cannot carry constraint bytes",
        )),
        (Some(_), None) => Err(NativeError::new(
            NativeErrorCode::InvalidConfig,
            "a controlled structured constraint requires its exact resolved artifact body",
        )),
        (Some(constraint), Some(body)) => {
            let reference = constraint.reference();
            if body.is_empty() || body.len() > MAX_CONSTRAINT_ARTIFACT_BYTES as usize {
                return Err(NativeError::new(
                    NativeErrorCode::InvalidConfig,
                    "resolved constraint body is empty or exceeds the public artifact bound",
                ));
            }
            if u32::try_from(body.len()).ok() != Some(reference.byte_len())
                || format!("{:x}", Sha256::digest(body.as_bytes())) != reference.sha256()
            {
                return Err(NativeError::new(
                    NativeErrorCode::InvalidConfig,
                    "resolved constraint bytes do not match their exact length and SHA-256 reference",
                ));
            }
            Ok(())
        }
    }
}

fn preflight_submission(
    submission: &ControlledGenerationSubmission,
    status: &ResidentModelStatus,
) -> NativeResult<()> {
    validate_constraint_attachment(submission.request(), submission.constraint_body())?;
    if status.state != ModelRuntimeState::Ready {
        return Err(NativeError::new(
            NativeErrorCode::ModelNotLoaded,
            "controlled generation requires a ready resident model",
        ));
    }
    let live = status.fingerprint.as_ref().ok_or_else(|| {
        NativeError::new(
            NativeErrorCode::ModelNotLoaded,
            "ready resident model has no fingerprint",
        )
    })?;
    let request = submission.request();
    if request.control().writer().fingerprint() != live {
        return Err(NativeError::new(
            NativeErrorCode::InvalidConfig,
            "controlled writer fingerprint does not match the exact resident model",
        ));
    }
    if !request.control().auxiliary_models().is_empty() {
        return Err(NativeError::new(
            NativeErrorCode::UnsupportedParameter,
            "this single-resident worker does not support auxiliary-model logit arithmetic",
        ));
    }
    if !request.control().static_profiles().is_empty() {
        return Err(NativeError::new(
            NativeErrorCode::UnsupportedParameter,
            "static adapter and activation-vector profiles are unavailable on this controlled path",
        ));
    }
    if request.control().guidance().iter().any(|control| {
        !matches!(
            control,
            GuidanceControl::SameModelCfg { .. } | GuidanceControl::PowerSampling { .. }
        )
    }) {
        return Err(NativeError::new(
            NativeErrorCode::UnsupportedParameter,
            "multi-model controlled guidance is unavailable on the single-resident worker",
        ));
    }
    if request
        .cases()
        .iter()
        .any(|case| case.sampling().seed == u32::MAX)
    {
        return Err(NativeError::new(
            NativeErrorCode::UnsupportedParameter,
            "controlled generation rejects the random default-seed sentinel",
        ));
    }
    if matches!(
        request.control().terminal_selector(),
        TerminalSelector::Distribution
    ) && request
        .cases()
        .iter()
        .any(|case| case.sampling().temperature <= 0.0)
    {
        return Err(NativeError::new(
            NativeErrorCode::InvalidConfig,
            "the ordinary distribution selector requires positive temperature; use the explicit greedy selector otherwise",
        ));
    }
    let _writer_cost = request
        .cost()
        .participants()
        .first()
        .filter(|cost| cost.participant_id() == request.control().writer().participant_id())
        .ok_or_else(|| {
            NativeError::new(
                NativeErrorCode::InvalidConfig,
                "controlled cost accounting has no exact writer entry",
            )
        })?;
    if request.cost().participants().len() != 1 || request.cost().total_model_contexts() != 1 {
        return Err(NativeError::new(
            NativeErrorCode::UnsupportedParameter,
            "single-resident controlled generation requires exactly one model context",
        ));
    }
    if request.cost().total_sequence_slots() > live.max_sequences {
        return Err(NativeError::new(
            NativeErrorCode::MemoryBudgetExceeded,
            "controlled conditional and unconditional sequences exceed resident capacity",
        ));
    }
    let physical_context_cells = controlled_physical_context_cells(request)?;
    if physical_context_cells > u64::from(live.context_tokens) {
        return Err(NativeError::new(
            NativeErrorCode::MemoryBudgetExceeded,
            format!(
                "controlled execution requires {physical_context_cells} physical context positions after exact prefix sharing but the resident context has {}",
                live.context_tokens
            ),
        ));
    }
    Ok(())
}

fn controlled_physical_context_cells(
    request: &ControlledGenerationBatchRequest,
) -> NativeResult<u64> {
    Ok(controlled_runtime_cost(request)?.reserved_physical_context_cells)
}

fn controlled_runtime_cost(
    request: &ControlledGenerationBatchRequest,
) -> NativeResult<ControlledRuntimeCostEvidence> {
    let conditional_shared = token_id_shared_prefix(
        request
            .cases()
            .iter()
            .map(|case| case.conditional_prompt().token_ids()),
    );
    let unconditional_shared = token_id_shared_prefix(
        request
            .cases()
            .iter()
            .filter_map(|case| case.unconditional_prompt().map(|prompt| prompt.token_ids())),
    );
    let mut prompt_evaluations = u64::try_from(conditional_shared)
        .ok()
        .and_then(|value| value.checked_add(unconditional_shared as u64));
    let mut cells = prompt_evaluations;
    for case in request.cases() {
        let generated = u64::from(case.sampling().max_tokens);
        let conditional_suffix = case
            .conditional_prompt()
            .token_ids()
            .len()
            .saturating_sub(conditional_shared) as u64;
        prompt_evaluations =
            prompt_evaluations.and_then(|value| value.checked_add(conditional_suffix));
        cells = cells
            .and_then(|value| value.checked_add(conditional_suffix))
            .and_then(|value| value.checked_add(generated));
        if let Some(unconditional) = case.unconditional_prompt() {
            let unconditional_suffix = unconditional
                .token_ids()
                .len()
                .saturating_sub(unconditional_shared) as u64;
            prompt_evaluations =
                prompt_evaluations.and_then(|value| value.checked_add(unconditional_suffix));
            cells = cells
                .and_then(|value| value.checked_add(unconditional_suffix))
                .and_then(|value| value.checked_add(generated));
        }
    }
    let physical_prompt_evaluations = prompt_evaluations.ok_or_else(|| {
        NativeError::new(
            NativeErrorCode::MemoryBudgetExceeded,
            "controlled physical prompt-evaluation accounting overflowed",
        )
    })?;
    let reserved_physical_context_cells = cells.ok_or_else(|| {
        NativeError::new(
            NativeErrorCode::MemoryBudgetExceeded,
            "controlled physical context-cell accounting overflowed",
        )
    })?;
    Ok(ControlledRuntimeCostEvidence {
        conditional_shared_prefix_tokens: conditional_shared,
        unconditional_shared_prefix_tokens: unconditional_shared,
        physical_prompt_evaluations,
        reserved_physical_context_cells,
        sequence_slots: request.cost().total_sequence_slots(),
    })
}

fn token_id_shared_prefix<'a>(mut prompts: impl Iterator<Item = &'a [i32]>) -> usize {
    let Some(first) = prompts.next() else {
        return 0;
    };
    let rest = prompts.collect::<Vec<_>>();
    if rest.is_empty() {
        return 0;
    }
    (0..first.len())
        .take_while(|index| {
            rest.iter()
                .all(|tokens| tokens.get(*index) == first.get(*index))
        })
        .count()
        .min(
            rest.iter()
                .map(|tokens| tokens.len())
                .chain(std::iter::once(first.len()))
                .min()
                .unwrap_or_default()
                .saturating_sub(1),
        )
}

/// Derive the exact tokenizer contract once on the owner thread. Token bytes
/// are hashed with length framing, so concatenation ambiguity cannot alias a
/// different vocabulary.
pub(crate) fn derive_live_token_contract(
    model: &LlamaModel,
    fingerprint: &ModelFingerprint,
) -> NativeResult<TokenContractIdentity> {
    let vocabulary_size = model.n_vocab();
    if vocabulary_size <= 0 {
        return Err(NativeError::new(
            NativeErrorCode::ModelInvalid,
            "controlled generation requires a non-empty vocabulary",
        ));
    }
    let mut vocabulary = StableEvidenceDigest::new("live-vocabulary-contract-v1");
    let mut token_bytes = StableEvidenceDigest::new("live-token-bytes-contract-v1");
    let mut special_tokens = StableEvidenceDigest::new("live-special-token-contract-v1");
    vocabulary.i32(vocabulary_size);
    vocabulary.text(&format!("{:?}", model.vocab_type()));
    special_tokens.i32(model.token_bos().0);
    special_tokens.i32(model.token_eos().0);
    special_tokens.i32(model.token_nl().0);
    special_tokens.i32(model.token_sep().0);
    special_tokens.i32(model.decode_start_token().0);
    for token_id in 0..vocabulary_size {
        let token = LlamaToken::new(token_id);
        let attributes = model.token_attr(token).bits();
        let piece = exact_token_contract_piece(model, token, attributes).map_err(|error| {
            NativeError::new(
                NativeErrorCode::ModelInvalid,
                format!("failed to inspect controlled token {token_id}: {error}"),
            )
        })?;
        vocabulary.i32(token_id);
        vocabulary.u32(attributes);
        token_bytes.i32(token_id);
        match piece {
            ExactTokenContractPiece::Rendered(bytes) => {
                token_bytes.text("rendered");
                token_bytes.bytes(&bytes);
            }
            ExactTokenContractPiece::Undefined => token_bytes.text("undefined"),
            ExactTokenContractPiece::Unused => token_bytes.text("unused"),
        }
        special_tokens.i32(token_id);
        special_tokens.bool(model.is_eog_token(token));
    }
    TokenContractIdentity::new(
        fingerprint.tokenizer_sha256.clone(),
        vocabulary.finish(),
        special_tokens.finish(),
        token_bytes.finish(),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExactTokenContractPiece {
    Rendered(Vec<u8>),
    Undefined,
    Unused,
}

fn exact_token_contract_piece(
    model: &LlamaModel,
    token: LlamaToken,
    attributes: u32,
) -> Result<ExactTokenContractPiece, llama_cpp_2::TokenToStringError> {
    if let Some(sparse) = sparse_token_contract_piece(attributes) {
        return Ok(sparse);
    }
    match model.token_to_piece_bytes(token, 64, true, None) {
        Err(llama_cpp_2::TokenToStringError::InsufficientBufferSpace(required)) if required < 0 => {
            let required = usize::try_from(required.unsigned_abs())
                .map_err(|_| llama_cpp_2::TokenToStringError::InsufficientBufferSpace(required))?;
            if required > MAX_TOKEN_PIECE_BYTES {
                return Err(llama_cpp_2::TokenToStringError::InsufficientBufferSpace(
                    -(MAX_TOKEN_PIECE_BYTES as i32),
                ));
            }
            model
                .token_to_piece_bytes(token, required, true, None)
                .map(ExactTokenContractPiece::Rendered)
        }
        Ok(bytes) => Ok(ExactTokenContractPiece::Rendered(bytes)),
        Err(error) => Err(error),
    }
}

fn sparse_token_contract_piece(attributes: u32) -> Option<ExactTokenContractPiece> {
    if attributes == 0 {
        Some(ExactTokenContractPiece::Undefined)
    } else if attributes & LlamaTokenAttr::Unused as u32 != 0 {
        Some(ExactTokenContractPiece::Unused)
    } else {
        None
    }
}

pub(crate) fn controlled_identity(
    participant_id: String,
    fingerprint: &ModelFingerprint,
    token_contract: &TokenContractIdentity,
) -> NativeResult<ControlledModelIdentity> {
    ControlledModelIdentity::new(participant_id, fingerprint.clone(), token_contract.clone())
}

#[derive(Debug)]
pub(crate) struct ControlledExecution {
    outputs: Vec<ControlledGenerationCaseOutput>,
    terminal_sampled_token_ids: Vec<Option<i32>>,
    runtime_ledger: RuntimeControlLedger,
    runtime_cost: ControlledRuntimeCostEvidence,
}

#[derive(Debug)]
struct RuntimeOperationEvidence {
    expected: ExpectedControlApplication,
    invocations_by_case: Vec<u64>,
    effective_invocations_by_case: Vec<u64>,
    effective_invocations: u64,
    digest: StableEvidenceDigest,
}

#[derive(Debug)]
struct RuntimeControlLedger {
    operations: Vec<RuntimeOperationEvidence>,
    decisions_by_case: Vec<u64>,
    next_operation_by_case: Vec<u16>,
    next_runtime_ordinal: u64,
}

#[derive(Debug)]
struct FinalizedRuntimeControlEvidence {
    reports: Vec<AppliedControlReport>,
    ledger_sha256: String,
    every_requested_operation_effective: bool,
    ineffective_operations: Vec<ExpectedControlApplication>,
}

impl RuntimeControlLedger {
    fn new(request: &ControlledGenerationBatchRequest) -> Self {
        let case_count = request.cases().len();
        let operations = request
            .control()
            .expected_application_plan()
            .into_iter()
            .map(|expected| {
                let mut digest = StableEvidenceDigest::new("controlled-runtime-operation-v1");
                digest.u32(u32::from(expected.operation_index()));
                digest.text(&format!("{:?}", expected.kind()));
                digest.text(&format!("{:?}", expected.stage()));
                RuntimeOperationEvidence {
                    expected,
                    invocations_by_case: vec![0; case_count],
                    effective_invocations_by_case: vec![0; case_count],
                    effective_invocations: 0,
                    digest,
                }
            })
            .collect();
        Self {
            operations,
            decisions_by_case: vec![0; case_count],
            next_operation_by_case: vec![0; case_count],
            next_runtime_ordinal: 0,
        }
    }

    fn begin_decision(&mut self, case_index: usize, generated_index: usize) -> NativeResult<()> {
        let operation_count = u16::try_from(self.operations.len()).map_err(|_| {
            generation_verification_error("controlled operation count does not fit u16")
        })?;
        let next_operation = self
            .next_operation_by_case
            .get_mut(case_index)
            .ok_or_else(|| generation_verification_error("runtime ledger case index overflow"))?;
        if *next_operation != 0 && *next_operation != operation_count {
            return Err(generation_verification_error(
                "a controlled decision began before its prior operation trace completed",
            ));
        }
        *next_operation = 0;
        let decision_count = self
            .decisions_by_case
            .get_mut(case_index)
            .ok_or_else(|| generation_verification_error("runtime ledger case index overflow"))?;
        if *decision_count != generated_index as u64 {
            return Err(generation_verification_error(
                "controlled runtime decision index is not contiguous",
            ));
        }
        *decision_count = decision_count.checked_add(1).ok_or_else(|| {
            generation_verification_error("controlled runtime decision count overflow")
        })?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn record(
        &mut self,
        case_index: usize,
        generated_index: usize,
        kind: AppliedControlKind,
        stage: ControlApplicationStage,
        effective: bool,
        runtime_evidence_sha256: &str,
    ) -> NativeResult<()> {
        let expected_index = *self
            .next_operation_by_case
            .get(case_index)
            .ok_or_else(|| generation_verification_error("runtime ledger case index overflow"))?;
        let operation = self
            .operations
            .get_mut(usize::from(expected_index))
            .ok_or_else(|| {
                generation_verification_error(
                    "runtime emitted more operations than the frozen control plan",
                )
            })?;
        if operation.expected.kind() != kind || operation.expected.stage() != stage {
            return Err(generation_verification_error(format!(
                "runtime operation {kind:?}/{stage:?} disagrees with frozen operation {} ({:?}/{:?})",
                operation.expected.operation_index(),
                operation.expected.kind(),
                operation.expected.stage(),
            )));
        }
        let invocation_count = operation
            .invocations_by_case
            .get_mut(case_index)
            .ok_or_else(|| generation_verification_error("runtime ledger case index overflow"))?;
        if *invocation_count != generated_index as u64 {
            return Err(generation_verification_error(
                "controlled operation invocation is missing, duplicated, or reordered",
            ));
        }
        *invocation_count = invocation_count.checked_add(1).ok_or_else(|| {
            generation_verification_error("controlled operation invocation count overflow")
        })?;
        if effective {
            let effective_count = operation
                .effective_invocations_by_case
                .get_mut(case_index)
                .ok_or_else(|| {
                    generation_verification_error("runtime ledger case index overflow")
                })?;
            *effective_count = effective_count.checked_add(1).ok_or_else(|| {
                generation_verification_error(
                    "controlled per-case effective-operation count overflow",
                )
            })?;
            operation.effective_invocations = operation
                .effective_invocations
                .checked_add(1)
                .ok_or_else(|| {
                    generation_verification_error("controlled effective-operation count overflow")
                })?;
        }
        operation.digest.u64(self.next_runtime_ordinal);
        operation.digest.u64(case_index as u64);
        operation.digest.u64(generated_index as u64);
        operation.digest.bool(effective);
        operation.digest.text(runtime_evidence_sha256);
        self.next_runtime_ordinal = self.next_runtime_ordinal.checked_add(1).ok_or_else(|| {
            generation_verification_error("controlled runtime operation ordinal overflow")
        })?;
        self.next_operation_by_case[case_index] =
            expected_index.checked_add(1).ok_or_else(|| {
                generation_verification_error("controlled runtime operation index overflow")
            })?;
        Ok(())
    }

    fn finish_decision(&self, case_index: usize) -> NativeResult<()> {
        if usize::from(
            *self.next_operation_by_case.get(case_index).ok_or_else(|| {
                generation_verification_error("runtime ledger case index overflow")
            })?,
        ) != self.operations.len()
        {
            return Err(generation_verification_error(
                "controlled runtime skipped a requested operation during a token decision",
            ));
        }
        Ok(())
    }

    fn finalize(
        self,
        request: &ControlledGenerationBatchRequest,
        runtime_cost: ControlledRuntimeCostEvidence,
    ) -> NativeResult<FinalizedRuntimeControlEvidence> {
        if self.operations.is_empty()
            || self.decisions_by_case.contains(&0)
            || self
                .next_operation_by_case
                .iter()
                .any(|index| usize::from(*index) != self.operations.len())
        {
            return Err(generation_verification_error(
                "controlled runtime did not execute a complete operation plan for every case",
            ));
        }
        let mut ledger = StableEvidenceDigest::new("controlled-runtime-ledger-v1");
        ledger.text(&request.fingerprint_sha256());
        ledger.u64(self.next_runtime_ordinal);
        ledger.u64(self.decisions_by_case.len() as u64);
        for count in &self.decisions_by_case {
            ledger.u64(*count);
        }
        ledger.u64(runtime_cost.conditional_shared_prefix_tokens as u64);
        ledger.u64(runtime_cost.unconditional_shared_prefix_tokens as u64);
        ledger.u64(runtime_cost.physical_prompt_evaluations);
        ledger.u64(runtime_cost.reserved_physical_context_cells);
        ledger.u32(runtime_cost.sequence_slots);
        let mut reports = Vec::with_capacity(self.operations.len());
        let mut every_requested_operation_effective = true;
        let mut ineffective_operations = Vec::new();
        for operation in self.operations {
            if operation.invocations_by_case != self.decisions_by_case {
                return Err(generation_verification_error(
                    "controlled runtime operation coverage disagrees with token decisions",
                ));
            }
            let effective_for_every_case = operation
                .effective_invocations_by_case
                .iter()
                .all(|count| *count > 0);
            every_requested_operation_effective &= effective_for_every_case;
            if !effective_for_every_case {
                ineffective_operations.push(operation.expected);
            }
            let operation_sha256 = operation.digest.finish();
            ledger.u32(u32::from(operation.expected.operation_index()));
            ledger.text(&format!("{:?}", operation.expected.kind()));
            ledger.text(&format!("{:?}", operation.expected.stage()));
            ledger.u64(operation.effective_invocations);
            for count in &operation.effective_invocations_by_case {
                ledger.u64(*count);
            }
            ledger.text(&operation_sha256);
            reports.push(AppliedControlReport::new(
                operation.expected.operation_index(),
                operation.expected.kind(),
                operation.expected.stage(),
                operation_sha256,
            )?);
        }
        Ok(FinalizedRuntimeControlEvidence {
            reports,
            ledger_sha256: ledger.finish(),
            every_requested_operation_effective,
            ineffective_operations,
        })
    }
}

pub(crate) fn execute_controlled_generation(
    model: &LlamaModel,
    context: &mut LlamaContext<'_>,
    submission: &ControlledGenerationSubmission,
    event_tx: &Sender<GenerationEvent>,
    retained_events: &mut Vec<GenerationEvent>,
    cancellations: &[Arc<AtomicBool>],
    tracking: SequenceTracking<'_>,
) -> NativeResult<ControlledExecution> {
    validate_live_request(model, submission)?;
    if is_disabled_control_baseline(submission.request()) {
        return execute_disabled_baseline(
            model,
            context,
            submission.request(),
            event_tx,
            retained_events,
            cancellations,
            tracking,
        );
    }
    execute_active_controls(
        model,
        context,
        submission,
        event_tx,
        retained_events,
        cancellations,
        tracking,
    )
}

fn validate_live_request(
    model: &LlamaModel,
    submission: &ControlledGenerationSubmission,
) -> NativeResult<()> {
    let n_vocab = model.n_vocab();
    for (case_index, case) in submission.request().cases().iter().enumerate() {
        for (prompt_name, prompt) in [
            ("conditional", Some(case.conditional_prompt())),
            ("unconditional", case.unconditional_prompt()),
        ] {
            let Some(prompt) = prompt else {
                continue;
            };
            if let Some(token_id) = prompt
                .token_ids()
                .iter()
                .find(|token_id| **token_id < 0 || **token_id >= n_vocab)
            {
                return Err(NativeError::new(
                    NativeErrorCode::InvalidConfig,
                    format!(
                        "controlled case {case_index} {prompt_name} prompt token {token_id} is outside vocabulary 0..{n_vocab}"
                    ),
                ));
            }
        }
        for sampler in submission
            .request()
            .control()
            .extended_samplers()
            .as_slice()
        {
            if let ExtendedSampler::SparseLogitBias { biases } = sampler
                && let Some(entry) = biases
                    .as_slice()
                    .iter()
                    .find(|entry| entry.token_id >= n_vocab)
            {
                return Err(NativeError::new(
                    NativeErrorCode::InvalidConfig,
                    format!(
                        "controlled sparse bias token {} is outside vocabulary 0..{n_vocab}",
                        entry.token_id
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn is_disabled_control_baseline(request: &ControlledGenerationBatchRequest) -> bool {
    request.control().constraint().is_none()
        && request.control().guidance().is_empty()
        && request.control().extended_samplers().is_empty()
        && request.control().observations().is_disabled()
        && request.control().static_profiles().is_empty()
        && request.control().terminal_selector() == TerminalSelector::Distribution
}

fn as_exact_legacy_request(request: &ControlledGenerationBatchRequest) -> GenerationBatchRequest {
    GenerationBatchRequest {
        request_id: request.request_id().to_string(),
        model_id: request.control().writer().fingerprint().model_id.clone(),
        cases: request
            .cases()
            .iter()
            .map(|case| GenerationCase {
                case_id: case.case_id().to_string(),
                input: GenerationInput::Completion {
                    prompts: vec![CompletionPrompt::Tokens {
                        token_ids: case.conditional_prompt().token_ids().to_vec(),
                    }],
                },
                sampling: case.sampling().clone(),
                cached_prefix: None,
            })
            .collect(),
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_disabled_baseline(
    model: &LlamaModel,
    context: &mut LlamaContext<'_>,
    request: &ControlledGenerationBatchRequest,
    event_tx: &Sender<GenerationEvent>,
    retained_events: &mut Vec<GenerationEvent>,
    cancellations: &[Arc<AtomicBool>],
    tracking: SequenceTracking<'_>,
) -> NativeResult<ControlledExecution> {
    let legacy = as_exact_legacy_request(request);
    let exact_budget = exact_token_batch_cell_budget(&legacy).map_err(|error| {
        NativeError::new(
            NativeErrorCode::InvalidConfig,
            format!("controlled baseline exact-cell budget failed: {error}"),
        )
    })?;
    let (normalized, tokens) = prepare_generation_batch(model, &legacy)?;
    let reasoning = legacy
        .cases
        .iter()
        .map(|_| Arc::new(AtomicBool::new(false)))
        .collect::<Vec<_>>();
    let mut runtime_ledger = RuntimeControlLedger::new(request);
    let mut runtime_sample_trace = Vec::new();
    let execution = generate_batch(
        model,
        context,
        &normalized,
        Some(tokens),
        Some(&exact_budget),
        BatchSupervision {
            event_tx,
            retained_events: Some(retained_events),
            retain_token_piece_traces: false,
            unrecorded_control_used: None,
            runtime_sample_trace: Some(&mut runtime_sample_trace),
            cancellations,
            reasoning_forces: &reasoning,
        },
        tracking,
    )?;
    join_baseline_runtime_trace(&execution, &runtime_sample_trace, &mut runtime_ledger)?;
    let outputs = request
        .cases()
        .iter()
        .zip(execution.outputs)
        .map(|(case, output)| {
            ControlledGenerationCaseOutput::new(case.case_id().to_string(), output, Vec::new())
        })
        .collect::<NativeResult<Vec<_>>>()?;
    Ok(ControlledExecution {
        outputs,
        terminal_sampled_token_ids: execution.terminal_sampled_token_ids,
        runtime_ledger,
        runtime_cost: controlled_runtime_cost(request)?,
    })
}

fn join_baseline_runtime_trace(
    execution: &GeneratedBatchExecution,
    trace: &[RuntimeSampleSelection],
    runtime_ledger: &mut RuntimeControlLedger,
) -> NativeResult<()> {
    if execution.outputs.len() != execution.terminal_sampled_token_ids.len() {
        return Err(generation_verification_error(
            "legacy baseline output and terminal cardinality disagree",
        ));
    }
    let mut generated_by_case = vec![0_usize; execution.outputs.len()];
    let mut terminal_by_case = vec![false; execution.outputs.len()];
    for sample in trace {
        let output = execution.outputs.get(sample.case_index).ok_or_else(|| {
            generation_verification_error("legacy sampler trace case index exceeds its batch")
        })?;
        if terminal_by_case[sample.case_index]
            || sample.generated_index != generated_by_case[sample.case_index]
        {
            return Err(generation_verification_error(
                "legacy sampler trace is missing, duplicated, reordered, or continues after terminal",
            ));
        }
        if sample.terminal {
            if execution.terminal_sampled_token_ids[sample.case_index] != Some(sample.token_id)
                || sample.generated_index != output.generated_token_ids.len()
            {
                return Err(generation_verification_error(
                    "legacy sampler terminal trace disagrees with owner-worker output evidence",
                ));
            }
            terminal_by_case[sample.case_index] = true;
        } else {
            if output
                .generated_token_ids
                .get(sample.generated_index)
                .copied()
                != Some(sample.token_id)
            {
                return Err(generation_verification_error(
                    "legacy sampler trace token disagrees with owner-worker output evidence",
                ));
            }
            generated_by_case[sample.case_index] = generated_by_case[sample.case_index]
                .checked_add(1)
                .ok_or_else(|| {
                    generation_verification_error("legacy sampler trace count overflowed")
                })?;
        }
        runtime_ledger.begin_decision(sample.case_index, sample.generated_index)?;
        runtime_ledger.record(
            sample.case_index,
            sample.generated_index,
            AppliedControlKind::OrdinaryDistributionSelector,
            ControlApplicationStage::Sampler,
            true,
            &terminal_selection_digest(sample.token_id, sample.generated_index, sample.terminal),
        )?;
        runtime_ledger.finish_decision(sample.case_index)?;
    }
    for (case_index, output) in execution.outputs.iter().enumerate() {
        if generated_by_case[case_index] != output.generated_token_ids.len()
            || terminal_by_case[case_index]
                != execution.terminal_sampled_token_ids[case_index].is_some()
        {
            return Err(generation_verification_error(
                "legacy sampler trace is absent or incomplete for a baseline case",
            ));
        }
    }
    Ok(())
}

struct ControlledCaseSamplers {
    grammar: Option<LlamaSampler>,
    ordinary: Option<LlamaSampler>,
    terminal: LlamaSampler,
}

struct ActiveControlledCase {
    conditional_sequence: i32,
    unconditional_sequence: Option<i32>,
    samplers: ControlledCaseSamplers,
    decoder: encoding_rs::Decoder,
    text: String,
    generated_token_ids: Vec<i32>,
    observations: Vec<TokenDistributionObservation>,
    terminal_sampled_token_id: Option<i32>,
    conditional_position: i32,
    unconditional_position: Option<i32>,
    conditional_logit_index: i32,
    unconditional_logit_index: Option<i32>,
    state: GenerationState,
    finish_reason: String,
    event_index: u64,
    first_token_ms: Option<u128>,
}

struct ControlledPrefillLayout {
    cfg: bool,
    conditional_shared_prefix: usize,
    conditional_logit_indexes: Vec<i32>,
    unconditional_logit_indexes: Vec<Option<i32>>,
}

#[derive(Clone)]
struct DistributionSnapshot {
    logits: Vec<f32>,
    allowed: Vec<bool>,
}

impl DistributionSnapshot {
    fn all(logits: Vec<f32>) -> Self {
        let allowed = vec![true; logits.len()];
        Self { logits, allowed }
    }

    fn from_candidates(vocabulary_size: usize, candidates: &LlamaTokenDataArray) -> Self {
        let mut logits = vec![0.0; vocabulary_size];
        let mut allowed = vec![false; vocabulary_size];
        for candidate in &candidates.data {
            let token_id = candidate.id().0;
            let Ok(index) = usize::try_from(token_id) else {
                continue;
            };
            if index < vocabulary_size && candidate.logit().is_finite() {
                logits[index] = candidate.logit();
                allowed[index] = true;
            }
        }
        Self { logits, allowed }
    }

    fn retained(&self) -> usize {
        self.allowed.iter().filter(|allowed| **allowed).count()
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_active_controls(
    model: &LlamaModel,
    context: &mut LlamaContext<'_>,
    submission: &ControlledGenerationSubmission,
    event_tx: &Sender<GenerationEvent>,
    retained_events: &mut Vec<GenerationEvent>,
    cancellations: &[Arc<AtomicBool>],
    mut tracking: SequenceTracking<'_>,
) -> NativeResult<ControlledExecution> {
    let request = submission.request();
    let compiled_grammar = compile_constraint(submission)?;
    let vocabulary_size = usize::try_from(model.n_vocab()).map_err(|_| {
        NativeError::new(
            NativeErrorCode::ModelInvalid,
            "model vocabulary size does not fit the controlled runtime",
        )
    })?;
    let token_contract = request.control().writer().token_contract();
    let vocabulary = VocabularyIdentity::new(
        token_contract.tokenizer_sha256(),
        token_contract.vocabulary_sha256(),
        vocabulary_size,
    )
    .map_err(control_math_error)?;
    let mut runtime_ledger = RuntimeControlLedger::new(request);
    let conditional_tokens = request
        .cases()
        .iter()
        .map(|case| {
            case.conditional_prompt()
                .token_ids()
                .iter()
                .copied()
                .map(LlamaToken::new)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let unconditional_tokens = request
        .cases()
        .iter()
        .map(|case| {
            case.unconditional_prompt().map(|prompt| {
                prompt
                    .token_ids()
                    .iter()
                    .copied()
                    .map(LlamaToken::new)
                    .collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    let started = Instant::now();
    for case_index in 0..request.cases().len() {
        emit_controlled_event(
            event_tx,
            retained_events,
            request,
            case_index,
            0,
            GenerationEventKind::State {
                state: GenerationState::Prefilling,
            },
        );
    }
    let layout = prefill_controlled_batch(
        context,
        request,
        &conditional_tokens,
        &unconditional_tokens,
        &mut tracking,
    )?;
    let mut cases = request
        .cases()
        .iter()
        .enumerate()
        .map(|(case_index, case)| {
            let conditional_sequence = controlled_conditional_sequence(case_index, layout.cfg)?;
            let unconditional_sequence = layout
                .cfg
                .then(|| controlled_unconditional_sequence(case_index))
                .transpose()?;
            Ok(ActiveControlledCase {
                conditional_sequence,
                unconditional_sequence,
                samplers: build_case_samplers(
                    model,
                    case.sampling(),
                    request,
                    &conditional_tokens[case_index],
                    compiled_grammar.as_deref(),
                )?,
                decoder: UTF_8.new_decoder(),
                text: String::new(),
                generated_token_ids: Vec::with_capacity(case.sampling().max_tokens as usize),
                observations: Vec::with_capacity(case.sampling().max_tokens as usize),
                terminal_sampled_token_id: None,
                conditional_position: i32::try_from(conditional_tokens[case_index].len()).map_err(
                    |_| {
                        NativeError::new(
                            NativeErrorCode::InvalidConfig,
                            "conditional position overflow",
                        )
                    },
                )?,
                unconditional_position: unconditional_tokens[case_index]
                    .as_ref()
                    .map(|tokens| i32::try_from(tokens.len()))
                    .transpose()
                    .map_err(|_| {
                        NativeError::new(
                            NativeErrorCode::InvalidConfig,
                            "unconditional position overflow",
                        )
                    })?,
                conditional_logit_index: layout.conditional_logit_indexes[case_index],
                unconditional_logit_index: layout.unconditional_logit_indexes[case_index],
                state: GenerationState::Generating,
                finish_reason: String::new(),
                event_index: 1,
                first_token_ms: None,
            })
        })
        .collect::<NativeResult<Vec<_>>>()?;
    for (case_index, active) in cases.iter_mut().enumerate() {
        emit_controlled_event(
            event_tx,
            retained_events,
            request,
            case_index,
            active.event_index,
            GenerationEventKind::State {
                state: GenerationState::Generating,
            },
        );
        active.event_index += 1;
    }

    loop {
        let mut next_tokens = Vec::new();
        for (case_index, active) in cases.iter_mut().enumerate() {
            if active.state != GenerationState::Generating {
                continue;
            }
            if cancellations[case_index].load(Ordering::Acquire) {
                active.state = GenerationState::Cancelled;
                active.finish_reason = "cancelled".to_string();
                let _ = context.clear_kv_cache_seq(
                    Some(active.conditional_sequence as u32),
                    None,
                    None,
                );
                if let Some(sequence) = active.unconditional_sequence {
                    let _ = context.clear_kv_cache_seq(Some(sequence as u32), None, None);
                }
                continue;
            }
            let conditional_logits = context
                .get_logits_ith(active.conditional_logit_index)
                .to_vec();
            let unconditional_logits = active
                .unconditional_logit_index
                .map(|index| context.get_logits_ith(index).to_vec());
            let (token, observation) = sample_controlled_token(
                model,
                request,
                &request.cases()[case_index],
                &vocabulary,
                &conditional_logits,
                unconditional_logits.as_deref(),
                &mut active.samplers,
                active.generated_token_ids.len(),
                case_index,
                &mut runtime_ledger,
            )?;
            if model.is_eog_token(token) {
                active.state = GenerationState::Completed;
                active.finish_reason = "end_of_generation".to_string();
                active.terminal_sampled_token_id = Some(token.0);
                continue;
            }
            active.generated_token_ids.push(token.0);
            if let Some(observation) = observation {
                active.observations.push(observation);
            }
            let bytes = model
                .token_to_piece_bytes(token, 512, false, None)
                .map_err(|error| {
                    NativeError::new(
                        NativeErrorCode::DecodeFailed,
                        format!("failed to decode controlled token: {error}"),
                    )
                })?;
            let mut piece = String::with_capacity(bytes.len());
            let _ = active.decoder.decode_to_string(&bytes, &mut piece, false);
            if active.first_token_ms.is_none() {
                active.first_token_ms = Some(started.elapsed().as_millis());
            }
            append_generated_utf8_piece(&mut active.text, &piece)?;
            if !piece.is_empty() {
                emit_controlled_event(
                    event_tx,
                    retained_events,
                    request,
                    case_index,
                    active.event_index,
                    GenerationEventKind::Delta { text: piece },
                );
                active.event_index += 1;
            }
            let case = &request.cases()[case_index];
            if let Some(stop) = case
                .sampling()
                .stop
                .iter()
                .find(|stop| !stop.is_empty() && active.text.ends_with(stop.as_str()))
            {
                let keep = active.text.len().saturating_sub(stop.len());
                active.text.truncate(keep);
                active.state = GenerationState::Completed;
                active.finish_reason = "stop_sequence".to_string();
            } else if active.generated_token_ids.len() >= case.sampling().max_tokens as usize {
                active.state = GenerationState::Completed;
                active.finish_reason = "max_tokens".to_string();
            } else {
                next_tokens.push((case_index, token));
            }
        }
        if next_tokens.is_empty() {
            break;
        }
        decode_controlled_batch_next(context, &next_tokens, &mut cases, &mut tracking)?;
    }

    let duration_ms = started.elapsed().as_millis();
    let mut outputs = Vec::with_capacity(cases.len());
    let mut terminal_sampled_token_ids = Vec::with_capacity(cases.len());
    for (case_index, active) in cases.into_iter().enumerate() {
        let case = &request.cases()[case_index];
        debug_assert!(matches!(
            active.state,
            GenerationState::Completed | GenerationState::Cancelled
        ));
        emit_controlled_event(
            event_tx,
            retained_events,
            request,
            case_index,
            active.event_index,
            GenerationEventKind::State {
                state: active.state,
            },
        );
        let completion_tokens = active.generated_token_ids.len();
        let tokens_per_second = if duration_ms == 0 {
            0.0
        } else {
            completion_tokens as f64 / (duration_ms as f64 / 1000.0)
        };
        let output = GenerationOutput {
            request_id: request.request_id().to_string(),
            branch_id: case.case_id().to_string(),
            input_index: case_index,
            model_id: request.control().writer().fingerprint().model_id.clone(),
            text: active.text,
            generated_token_ids: active.generated_token_ids,
            token_observations: None,
            state: active.state,
            finish_reason: active.finish_reason,
            metrics: GenerationMetrics {
                prompt_tokens: conditional_tokens[case_index].len(),
                completion_tokens,
                shared_prefix_tokens: layout.conditional_shared_prefix,
                duration_ms,
                first_token_ms: active.first_token_ms,
                tokens_per_second,
                cache: GenerationCacheMetrics {
                    supplied_prefix_tokens: 0,
                    restored_prefix_tokens: 0,
                    batch_shared_prefix_tokens: layout.conditional_shared_prefix,
                },
            },
            real_engine_invoked: true,
            fake_fixture: false,
            transport: NativeTransport::InProcess,
        };
        outputs.push(ControlledGenerationCaseOutput::new(
            case.case_id().to_string(),
            output,
            active.observations,
        )?);
        terminal_sampled_token_ids.push(active.terminal_sampled_token_id);
    }
    Ok(ControlledExecution {
        outputs,
        terminal_sampled_token_ids,
        runtime_ledger,
        runtime_cost: controlled_runtime_cost(request)?,
    })
}

fn compile_constraint(submission: &ControlledGenerationSubmission) -> NativeResult<Option<String>> {
    let Some(constraint) = submission.request().control().constraint() else {
        return Ok(None);
    };
    let body = submission.constraint_body().ok_or_else(|| {
        NativeError::new(
            NativeErrorCode::InvalidConfig,
            "controlled constraint body disappeared after admission",
        )
    })?;
    match constraint {
        StructuredConstraint::Gbnf { .. } => Ok(Some(body.to_string())),
        StructuredConstraint::JsonSchema { .. } => {
            json_schema_to_grammar(body).map(Some).map_err(|error| {
                NativeError::new(
                    NativeErrorCode::InvalidConfig,
                    format!("failed to compile JSON schema constraint: {error}"),
                )
            })
        }
    }
}

fn build_case_samplers(
    model: &LlamaModel,
    sampling: &SamplingConfig,
    request: &ControlledGenerationBatchRequest,
    prompt: &[LlamaToken],
    grammar: Option<&str>,
) -> NativeResult<ControlledCaseSamplers> {
    let mut ordinary = build_transform_sampler(model, sampling);
    if let Some(sampler) = ordinary.as_mut() {
        sampler.accept_many(prompt);
    }
    let grammar = grammar
        .map(|grammar| {
            LlamaSampler::grammar(model, grammar, "root").map_err(|error| {
                NativeError::new(
                    NativeErrorCode::InvalidConfig,
                    format!("failed to compile GBNF constraint: {error}"),
                )
            })
        })
        .transpose()?;
    let terminal = match request.control().terminal_selector() {
        TerminalSelector::Distribution => LlamaSampler::dist(sampling.seed),
        TerminalSelector::Greedy => LlamaSampler::greedy(),
        TerminalSelector::MirostatV1 => {
            let config = request
                .control()
                .extended_samplers()
                .as_slice()
                .iter()
                .find_map(|sampler| match sampler {
                    ExtendedSampler::MirostatV1 { config } => Some(*config),
                    _ => None,
                })
                .ok_or_else(|| {
                    NativeError::new(
                        NativeErrorCode::InvalidConfig,
                        "Mirostat v1 selector has no exact configuration",
                    )
                })?;
            LlamaSampler::mirostat(
                model.n_vocab(),
                sampling.seed,
                config.tau(),
                config.eta(),
                config.m(),
            )
        }
        TerminalSelector::MirostatV2 => {
            let config = request
                .control()
                .extended_samplers()
                .as_slice()
                .iter()
                .find_map(|sampler| match sampler {
                    ExtendedSampler::MirostatV2 { config } => Some(*config),
                    _ => None,
                })
                .ok_or_else(|| {
                    NativeError::new(
                        NativeErrorCode::InvalidConfig,
                        "Mirostat v2 selector has no exact configuration",
                    )
                })?;
            LlamaSampler::mirostat_v2(sampling.seed, config.tau(), config.eta())
        }
    };
    Ok(ControlledCaseSamplers {
        grammar,
        ordinary,
        terminal,
    })
}

fn build_transform_sampler(model: &LlamaModel, config: &SamplingConfig) -> Option<LlamaSampler> {
    let mut samplers = Vec::new();
    for kind in &config.sampler_order {
        match kind {
            SamplerKind::Penalties
                if config.repeat_penalty != 1.0
                    || config.frequency_penalty != 0.0
                    || config.presence_penalty != 0.0 =>
            {
                samplers.push(LlamaSampler::penalties(
                    config.repeat_last_n,
                    config.repeat_penalty,
                    config.frequency_penalty,
                    config.presence_penalty,
                ));
            }
            SamplerKind::Dry if config.dry_multiplier > 0.0 => {
                samplers.push(LlamaSampler::dry(
                    model,
                    config.dry_multiplier,
                    config.dry_base,
                    config.dry_allowed_length,
                    config.dry_penalty_last_n,
                    ["\n", ":", "\"", "*"],
                ));
            }
            SamplerKind::TopK if config.top_k > 0 => {
                samplers.push(LlamaSampler::top_k(config.top_k));
            }
            SamplerKind::TypicalP if config.typical_p < 1.0 => {
                samplers.push(LlamaSampler::typical(config.typical_p, 1));
            }
            SamplerKind::TopP if config.top_p < 1.0 => {
                samplers.push(LlamaSampler::top_p(config.top_p, 1));
            }
            SamplerKind::MinP if config.min_p > 0.0 => {
                samplers.push(LlamaSampler::min_p(config.min_p, 1));
            }
            SamplerKind::Xtc if config.xtc_probability > 0.0 => {
                samplers.push(LlamaSampler::xtc(
                    config.xtc_probability,
                    config.xtc_threshold,
                    1,
                    config.seed,
                ));
            }
            SamplerKind::Temperature => {
                if config.dynamic_temperature_range > 0.0 {
                    samplers.push(LlamaSampler::temp_ext(
                        config.temperature,
                        config.dynamic_temperature_range,
                        config.dynamic_temperature_exponent,
                    ));
                } else if config.temperature > 0.0 {
                    samplers.push(LlamaSampler::temp(config.temperature));
                }
            }
            _ => {}
        }
    }
    (!samplers.is_empty()).then(|| LlamaSampler::chain_simple(samplers))
}

fn prefill_controlled_batch(
    context: &mut LlamaContext<'_>,
    request: &ControlledGenerationBatchRequest,
    conditional: &[Vec<LlamaToken>],
    unconditional: &[Option<Vec<LlamaToken>>],
    tracking: &mut SequenceTracking<'_>,
) -> NativeResult<ControlledPrefillLayout> {
    if conditional.len() != request.cases().len()
        || unconditional.len() != request.cases().len()
        || conditional.iter().any(Vec::is_empty)
    {
        return Err(generation_verification_error(
            "controlled prefill lost exact case cardinality or a non-empty prompt",
        ));
    }
    let cfg = request.control().uses_same_model_cfg();
    if unconditional.iter().any(Option::is_some) != cfg
        || cfg && unconditional.iter().any(Option::is_none)
    {
        return Err(generation_verification_error(
            "controlled prefill CFG layout disagrees with exact prompts",
        ));
    }
    let conditional_sequences = (0..conditional.len())
        .map(|index| controlled_conditional_sequence(index, cfg))
        .collect::<NativeResult<Vec<_>>>()?;
    let conditional_shared_prefix = controlled_shared_prefix(conditional);
    let unconditional_sets = unconditional
        .iter()
        .filter_map(Option::as_ref)
        .cloned()
        .collect::<Vec<_>>();
    let unconditional_sequences = (0..unconditional_sets.len())
        .map(controlled_unconditional_sequence)
        .collect::<NativeResult<Vec<_>>>()?;
    let unconditional_shared_prefix = controlled_shared_prefix(&unconditional_sets);
    let runtime_cost = controlled_runtime_cost(request)?;
    if runtime_cost.conditional_shared_prefix_tokens != conditional_shared_prefix
        || runtime_cost.unconditional_shared_prefix_tokens != unconditional_shared_prefix
    {
        return Err(generation_verification_error(
            "controlled token and request prefix calculations disagree",
        ));
    }
    if runtime_cost.reserved_physical_context_cells > u64::from(context.n_ctx()) {
        return Err(NativeError::new(
            NativeErrorCode::PromptTooLarge,
            format!(
                "controlled batch requires {} physical context cells after exact prefix sharing but the context has {}",
                runtime_cost.reserved_physical_context_cells,
                context.n_ctx()
            ),
        ));
    }

    context.clear_kv_cache();
    tracking.token_counts.clear();
    tracking.token_ids.clear();
    prefill_controlled_group(
        context,
        conditional,
        &conditional_sequences,
        conditional_shared_prefix,
        "conditional",
    )?;
    if cfg {
        prefill_controlled_group(
            context,
            &unconditional_sets,
            &unconditional_sequences,
            unconditional_shared_prefix,
            "unconditional",
        )?;
    }

    let sequence_count = conditional.len() + unconditional_sets.len();
    let mut final_batch = LlamaBatch::new(sequence_count, 1);
    let mut conditional_logit_indexes = Vec::with_capacity(conditional.len());
    let mut unconditional_logit_indexes = vec![None; conditional.len()];
    let mut logit_index = 0_i32;
    for (case_index, tokens) in conditional.iter().enumerate() {
        let sequence = conditional_sequences[case_index];
        final_batch
            .add(
                tokens[tokens.len() - 1],
                (tokens.len() - 1) as i32,
                &[sequence],
                true,
            )
            .map_err(|error| {
                native_decode_error("failed to build controlled conditional prompt", error)
            })?;
        conditional_logit_indexes.push(logit_index);
        logit_index = logit_index.checked_add(1).ok_or_else(|| {
            generation_verification_error("controlled prompt logit index overflow")
        })?;
        tracking.token_counts.insert(sequence, tokens.len());
        tracking
            .token_ids
            .insert(sequence, tokens.iter().map(|token| token.0).collect());
    }
    for (case_index, tokens) in unconditional_sets.iter().enumerate() {
        let sequence = unconditional_sequences[case_index];
        final_batch
            .add(
                tokens[tokens.len() - 1],
                (tokens.len() - 1) as i32,
                &[sequence],
                true,
            )
            .map_err(|error| {
                native_decode_error("failed to build controlled unconditional prompt", error)
            })?;
        unconditional_logit_indexes[case_index] = Some(logit_index);
        logit_index = logit_index.checked_add(1).ok_or_else(|| {
            generation_verification_error("controlled prompt logit index overflow")
        })?;
        tracking.token_counts.insert(sequence, tokens.len());
        tracking
            .token_ids
            .insert(sequence, tokens.iter().map(|token| token.0).collect());
    }
    context
        .decode(&mut final_batch)
        .map_err(|error| native_decode_error("failed to decode controlled prompt batch", error))?;
    Ok(ControlledPrefillLayout {
        cfg,
        conditional_shared_prefix,
        conditional_logit_indexes,
        unconditional_logit_indexes,
    })
}

fn prefill_controlled_group(
    context: &mut LlamaContext<'_>,
    token_sets: &[Vec<LlamaToken>],
    sequences: &[i32],
    shared_prefix: usize,
    label: &str,
) -> NativeResult<()> {
    if token_sets.len() != sequences.len() || token_sets.iter().any(Vec::is_empty) {
        return Err(generation_verification_error(format!(
            "controlled {label} prefix group is dimensionally invalid",
        )));
    }
    if token_sets.is_empty() {
        return Ok(());
    }
    if shared_prefix > 0 {
        decode_tokens_chunked(
            context,
            &token_sets[0][..shared_prefix],
            sequences[0],
            0,
            false,
        )?;
        let shared_prefix = u32::try_from(shared_prefix).map_err(|_| {
            generation_verification_error("controlled shared prefix does not fit u32")
        })?;
        for destination in sequences.iter().copied().skip(1) {
            context
                .copy_kv_cache_seq(sequences[0], destination, Some(0), Some(shared_prefix))
                .map_err(|error| {
                    NativeError::new(
                        NativeErrorCode::DecodeFailed,
                        format!("failed to copy controlled {label} KV prefix: {error}"),
                    )
                })?;
        }
    }
    for (tokens, sequence) in token_sets.iter().zip(sequences) {
        let suffix = &tokens[shared_prefix..tokens.len() - 1];
        if !suffix.is_empty() {
            decode_tokens_chunked(context, suffix, *sequence, shared_prefix as i32, false)?;
        }
    }
    Ok(())
}

fn controlled_shared_prefix(token_sets: &[Vec<LlamaToken>]) -> usize {
    if token_sets.len() < 2 {
        return 0;
    }
    let minimum = token_sets.iter().map(Vec::len).min().unwrap_or_default();
    longest_common_prefix(token_sets).min(minimum.saturating_sub(1))
}

fn controlled_conditional_sequence(case_index: usize, cfg: bool) -> NativeResult<i32> {
    let multiplier = 1 + usize::from(cfg);
    i32::try_from(case_index.checked_mul(multiplier).ok_or_else(|| {
        generation_verification_error("controlled conditional sequence index overflow")
    })?)
    .map_err(|_| generation_verification_error("controlled sequence index does not fit i32"))
}

fn controlled_unconditional_sequence(case_index: usize) -> NativeResult<i32> {
    i32::try_from(
        case_index
            .checked_mul(2)
            .and_then(|index| index.checked_add(1))
            .ok_or_else(|| {
                generation_verification_error("controlled unconditional sequence index overflow")
            })?,
    )
    .map_err(|_| generation_verification_error("controlled sequence index does not fit i32"))
}

fn decode_controlled_batch_next(
    context: &mut LlamaContext<'_>,
    next_tokens: &[(usize, LlamaToken)],
    cases: &mut [ActiveControlledCase],
    tracking: &mut SequenceTracking<'_>,
) -> NativeResult<()> {
    let batch_tokens = next_tokens
        .iter()
        .try_fold(0_usize, |count, (case_index, _)| {
            let active = cases.get(*case_index)?;
            count.checked_add(1 + usize::from(active.unconditional_sequence.is_some()))
        })
        .ok_or_else(|| {
            generation_verification_error("controlled continuation batch size overflow")
        })?;
    let mut batch = LlamaBatch::new(batch_tokens, 1);
    let mut logit_index = 0_i32;
    for (case_index, token) in next_tokens {
        let active = cases.get_mut(*case_index).ok_or_else(|| {
            generation_verification_error("controlled continuation case index overflow")
        })?;
        batch
            .add(
                *token,
                active.conditional_position,
                &[active.conditional_sequence],
                true,
            )
            .map_err(|error| {
                native_decode_error("failed to build controlled continuation batch", error)
            })?;
        active.conditional_logit_index = logit_index;
        logit_index = logit_index.checked_add(1).ok_or_else(|| {
            generation_verification_error("controlled continuation logit index overflow")
        })?;
        active.conditional_position += 1;
        tracking.token_counts.insert(
            active.conditional_sequence,
            active.conditional_position as usize,
        );
        tracking
            .token_ids
            .entry(active.conditional_sequence)
            .or_default()
            .push(token.0);
        if let (Some(sequence), Some(position)) = (
            active.unconditional_sequence,
            active.unconditional_position.as_mut(),
        ) {
            batch
                .add(*token, *position, &[sequence], true)
                .map_err(|error| {
                    native_decode_error("failed to build controlled CFG continuation batch", error)
                })?;
            active.unconditional_logit_index = Some(logit_index);
            logit_index = logit_index.checked_add(1).ok_or_else(|| {
                generation_verification_error("controlled CFG logit index overflow")
            })?;
            *position += 1;
            tracking.token_counts.insert(sequence, *position as usize);
            tracking
                .token_ids
                .entry(sequence)
                .or_default()
                .push(token.0);
        }
    }
    context.decode(&mut batch).map_err(|error| {
        native_decode_error("failed to decode controlled continuation batch", error)
    })
}

fn emit_controlled_event(
    event_tx: &Sender<GenerationEvent>,
    retained_events: &mut Vec<GenerationEvent>,
    request: &ControlledGenerationBatchRequest,
    case_index: usize,
    event_index: u64,
    event: GenerationEventKind,
) {
    let case = &request.cases()[case_index];
    let emitted = GenerationEvent {
        request_id: request.request_id().to_string(),
        branch_id: case.case_id().to_string(),
        sequence_id: case_index as i32,
        input_index: case_index,
        event_index,
        event,
    };
    retained_events.push(emitted.clone());
    if matches!(
        emitted.event,
        GenerationEventKind::State { state } if is_terminal_state(state)
    ) {
        try_emit_terminal(event_tx, emitted);
    } else {
        try_emit_nonterminal(event_tx, emitted);
    }
}

#[allow(clippy::too_many_arguments)]
fn sample_controlled_token(
    model: &LlamaModel,
    request: &ControlledGenerationBatchRequest,
    case: &llama_native_types::ControlledGenerationCase,
    vocabulary: &VocabularyIdentity,
    conditional_logits: &[f32],
    unconditional_logits: Option<&[f32]>,
    samplers: &mut ControlledCaseSamplers,
    generated_index: usize,
    case_index: usize,
    runtime_ledger: &mut RuntimeControlLedger,
) -> NativeResult<(LlamaToken, Option<TokenDistributionObservation>)> {
    runtime_ledger.begin_decision(case_index, generated_index)?;
    let _ = ValidatedLogits::new(conditional_logits, vocabulary).map_err(control_math_error)?;
    let raw = DistributionSnapshot::all(conditional_logits.to_vec());
    let constraint_mask = constraint_mask(conditional_logits, samplers.grammar.as_ref())?;
    if request.control().constraint().is_some() {
        let effective = constraint_mask.iter().any(|allowed| !*allowed);
        runtime_ledger.record(
            case_index,
            generated_index,
            AppliedControlKind::StructuredConstraint,
            ControlApplicationStage::Constraint,
            effective,
            &constraint_evidence_digest(conditional_logits, &constraint_mask),
        )?;
    }
    let post_constraint = DistributionSnapshot {
        logits: conditional_logits.to_vec(),
        allowed: constraint_mask.clone(),
    };

    let mut guided = conditional_logits.to_vec();
    for control in request.control().guidance() {
        let before = guided.clone();
        match control {
            GuidanceControl::SameModelCfg { scale, rescale } => {
                let unconditional_logits = unconditional_logits.ok_or_else(|| {
                    NativeError::new(
                        NativeErrorCode::InvalidConfig,
                        "same-model CFG lost its unconditional logits",
                    )
                })?;
                let conditional =
                    ValidatedLogits::new(&guided, vocabulary).map_err(control_math_error)?;
                let unconditional = ValidatedLogits::new(unconditional_logits, vocabulary)
                    .map_err(control_math_error)?;
                guided = classifier_free_guidance(
                    conditional,
                    unconditional,
                    scale.get(),
                    rescale.map(|value| value.get()),
                )
                .map_err(control_math_error)?;
                let (effective, evidence) =
                    distribution_transition_evidence(&before, &guided, &constraint_mask)?;
                runtime_ledger.record(
                    case_index,
                    generated_index,
                    AppliedControlKind::SameModelCfg,
                    ControlApplicationStage::Guidance,
                    effective,
                    &evidence,
                )?;
            }
            GuidanceControl::PowerSampling { exponent } => {
                guided = power_temperature_transform(
                    ValidatedLogits::new(&guided, vocabulary).map_err(control_math_error)?,
                    PowerTemperature::Power(exponent.get()),
                )
                .map_err(control_math_error)?;
                let (effective, evidence) =
                    distribution_transition_evidence(&before, &guided, &constraint_mask)?;
                runtime_ledger.record(
                    case_index,
                    generated_index,
                    AppliedControlKind::PowerSampling,
                    ControlApplicationStage::Guidance,
                    effective,
                    &evidence,
                )?;
            }
            GuidanceControl::ContrastiveExpertAmateur { .. }
            | GuidanceControl::DExperts { .. }
            | GuidanceControl::GenArm { .. } => {
                return Err(NativeError::new(
                    NativeErrorCode::UnsupportedParameter,
                    "multi-model guidance crossed single-resident admission",
                ));
            }
        }
    }
    let post_guidance = DistributionSnapshot {
        logits: guided.clone(),
        allowed: constraint_mask.clone(),
    };
    let extended = request.control().extended_samplers().as_slice();
    let mut first_extended = 0;
    if let Some(ExtendedSampler::SparseLogitBias { biases }) = extended.first() {
        let before = guided.clone();
        apply_sparse_bias_logits(&mut guided, &constraint_mask, biases.as_slice())?;
        let (effective, evidence) =
            distribution_transition_evidence(&before, &guided, &constraint_mask)?;
        runtime_ledger.record(
            case_index,
            generated_index,
            AppliedControlKind::SparseLogitBias,
            ControlApplicationStage::Sampler,
            effective,
            &evidence,
        )?;
        first_extended = 1;
    }
    let mut candidates = candidate_array(&DistributionSnapshot {
        logits: guided,
        allowed: constraint_mask,
    })?;
    for extended in &extended[first_extended..] {
        match extended {
            ExtendedSampler::SparseLogitBias { .. } => {
                return Err(NativeError::new(
                    NativeErrorCode::InvalidConfig,
                    "sparse logit bias must precede every truncation sampler",
                ));
            }
            ExtendedSampler::EtaCutoff { cutoff } => {
                let before = candidate_evidence_digest(&candidates);
                let before_len = candidates.data.len();
                apply_eta_cutoff(&mut candidates, cutoff.get())?;
                runtime_ledger.record(
                    case_index,
                    generated_index,
                    AppliedControlKind::EtaCutoff,
                    ControlApplicationStage::Sampler,
                    candidates.data.len() != before_len,
                    &candidate_transition_evidence(&before, &candidates),
                )?;
            }
            ExtendedSampler::TopNSigma { sigma } => {
                let before = candidate_evidence_digest(&candidates);
                let before_len = candidates.data.len();
                apply_top_n_sigma(&mut candidates, sigma.get())?;
                runtime_ledger.record(
                    case_index,
                    generated_index,
                    AppliedControlKind::TopNSigma,
                    ControlApplicationStage::Sampler,
                    candidates.data.len() != before_len,
                    &candidate_transition_evidence(&before, &candidates),
                )?;
            }
            ExtendedSampler::MirostatV1 { .. } | ExtendedSampler::MirostatV2 { .. } => {}
        }
    }
    if let Some(ordinary) = samplers.ordinary.as_ref() {
        ordinary.apply(&mut candidates);
    }
    retain_finite_candidates(&mut candidates)?;
    if candidates.data.is_empty() {
        return Err(NativeError::new(
            NativeErrorCode::DecodeFailed,
            "controlled sampler removed every candidate",
        ));
    }
    samplers.terminal.apply(&mut candidates);
    let token = candidates.selected_token().ok_or_else(|| {
        NativeError::new(
            NativeErrorCode::DecodeFailed,
            "controlled terminal selector returned no token",
        )
    })?;
    runtime_ledger.record(
        case_index,
        generated_index,
        terminal_control_kind(request.control().terminal_selector()),
        ControlApplicationStage::Sampler,
        true,
        &terminal_candidate_evidence(&candidates, token, generated_index),
    )?;
    let post_sampler =
        DistributionSnapshot::from_candidates(vocabulary.vocabulary_size(), &candidates);
    if !post_sampler
        .allowed
        .get(token.0 as usize)
        .copied()
        .unwrap_or(false)
    {
        return Err(NativeError::new(
            NativeErrorCode::DecodeFailed,
            "controlled terminal token is absent from its final distribution",
        ));
    }
    let observation = if request.control().observations().is_disabled() {
        None
    } else {
        Some(build_distribution_observation(
            model,
            request.control().observations(),
            generated_index,
            token,
            &raw,
            &post_constraint,
            &post_guidance,
            &post_sampler,
        )?)
    };
    if let Some(observation) = &observation {
        runtime_ledger.record(
            case_index,
            generated_index,
            AppliedControlKind::DistributionObservation,
            ControlApplicationStage::EvidenceCapture,
            true,
            &distribution_observation_evidence(observation),
        )?;
    }
    runtime_ledger.finish_decision(case_index)?;
    if let Some(grammar) = samplers.grammar.as_mut() {
        grammar.accept(token);
    }
    if let Some(ordinary) = samplers.ordinary.as_mut() {
        ordinary.accept(token);
    }
    samplers.terminal.accept(token);
    let _ = case;
    Ok((token, observation))
}

fn constraint_mask(logits: &[f32], grammar: Option<&LlamaSampler>) -> NativeResult<Vec<bool>> {
    let Some(grammar) = grammar else {
        return Ok(vec![true; logits.len()]);
    };
    let mut candidates = LlamaTokenDataArray::from_iter(
        logits.iter().enumerate().map(|(token_id, logit)| {
            LlamaTokenData::new(LlamaToken::new(token_id as i32), *logit, 0.0)
        }),
        false,
    );
    grammar.apply(&mut candidates);
    let mut allowed = vec![false; logits.len()];
    for candidate in candidates.data {
        let token_id = candidate.id().0;
        if let Ok(index) = usize::try_from(token_id)
            && index < allowed.len()
            && candidate.logit().is_finite()
        {
            allowed[index] = true;
        }
    }
    if !allowed.iter().any(|allowed| *allowed) {
        return Err(NativeError::new(
            NativeErrorCode::DecodeFailed,
            "structured constraint rejected the complete vocabulary",
        ));
    }
    Ok(allowed)
}

fn candidate_array(snapshot: &DistributionSnapshot) -> NativeResult<LlamaTokenDataArray> {
    if snapshot.logits.len() != snapshot.allowed.len() || snapshot.retained() == 0 {
        return Err(NativeError::new(
            NativeErrorCode::DecodeFailed,
            "controlled distribution mask is empty or dimensionally invalid",
        ));
    }
    Ok(LlamaTokenDataArray::from_iter(
        snapshot
            .logits
            .iter()
            .zip(&snapshot.allowed)
            .enumerate()
            .filter(|(_, (_, allowed))| **allowed)
            .map(|(token_id, (logit, _))| {
                LlamaTokenData::new(LlamaToken::new(token_id as i32), *logit, 0.0)
            }),
        false,
    ))
}

fn retain_finite_candidates(candidates: &mut LlamaTokenDataArray) -> NativeResult<()> {
    candidates
        .data
        .retain(|candidate| candidate.logit().is_finite());
    candidates.selected = None;
    candidates.sorted = false;
    if candidates.data.is_empty() {
        return Err(NativeError::new(
            NativeErrorCode::DecodeFailed,
            "sampler produced no finite candidate logits",
        ));
    }
    Ok(())
}

fn apply_sparse_bias_logits(
    logits: &mut [f32],
    allowed: &[bool],
    biases: &[llama_native_types::TokenLogitBias],
) -> NativeResult<()> {
    if logits.len() != allowed.len() {
        return Err(generation_verification_error(
            "sparse logit bias received a dimensionally invalid constraint mask",
        ));
    }
    for bias in biases {
        let index = usize::try_from(bias.token_id).map_err(|_| {
            NativeError::new(
                NativeErrorCode::InvalidConfig,
                "sparse logit bias token ID does not fit the resident vocabulary",
            )
        })?;
        if !allowed.get(index).copied().unwrap_or(false) {
            continue;
        }
        let value = logits[index] + bias.bias;
        if !value.is_finite() {
            return Err(NativeError::new(
                NativeErrorCode::DecodeFailed,
                "sparse logit bias produced a non-finite value",
            ));
        }
        logits[index] = value;
    }
    Ok(())
}

fn terminal_control_kind(selector: TerminalSelector) -> AppliedControlKind {
    match selector {
        TerminalSelector::Distribution => AppliedControlKind::OrdinaryDistributionSelector,
        TerminalSelector::Greedy => AppliedControlKind::GreedySelector,
        TerminalSelector::MirostatV1 => AppliedControlKind::MirostatV1,
        TerminalSelector::MirostatV2 => AppliedControlKind::MirostatV2,
    }
}

fn terminal_selection_digest(token_id: i32, generated_index: usize, terminal: bool) -> String {
    let mut digest = StableEvidenceDigest::new("controlled-terminal-selection-v1");
    digest.i32(token_id);
    digest.u64(generated_index as u64);
    digest.bool(terminal);
    digest.finish()
}

fn constraint_evidence_digest(logits: &[f32], allowed: &[bool]) -> String {
    let mut digest = StableEvidenceDigest::new("controlled-constraint-runtime-v1");
    digest.u64(logits.len() as u64);
    for (logit, allowed) in logits.iter().zip(allowed) {
        digest.u32(logit.to_bits());
        digest.bool(*allowed);
    }
    digest.finish()
}

fn distribution_transition_evidence(
    before: &[f32],
    after: &[f32],
    allowed: &[bool],
) -> NativeResult<(bool, String)> {
    if before.len() != after.len() || before.len() != allowed.len() || before.is_empty() {
        return Err(generation_verification_error(
            "controlled distribution transition changed dimensions or was empty",
        ));
    }
    let before_max = before
        .iter()
        .zip(allowed)
        .filter_map(|(logit, allowed)| allowed.then_some(*logit))
        .fold(f32::NEG_INFINITY, f32::max);
    let after_max = after
        .iter()
        .zip(allowed)
        .filter_map(|(logit, allowed)| allowed.then_some(*logit))
        .fold(f32::NEG_INFINITY, f32::max);
    if !before_max.is_finite() || !after_max.is_finite() {
        return Err(NativeError::new(
            NativeErrorCode::DecodeFailed,
            "controlled distribution transition has no finite allowed support",
        ));
    }
    let mut relative_change = false;
    let mut digest = StableEvidenceDigest::new("controlled-distribution-transition-v1");
    digest.u64(before.len() as u64);
    for ((before, after), allowed) in before.iter().zip(after).zip(allowed) {
        if !before.is_finite() || !after.is_finite() {
            return Err(NativeError::new(
                NativeErrorCode::DecodeFailed,
                "controlled distribution transition contains a non-finite value",
            ));
        }
        if *allowed {
            let before_centered = f64::from(*before) - f64::from(before_max);
            let after_centered = f64::from(*after) - f64::from(after_max);
            relative_change |= before_centered.to_bits() != after_centered.to_bits();
            digest.u64(before_centered.to_bits());
            digest.u64(after_centered.to_bits());
        }
        digest.bool(*allowed);
        digest.u32(before.to_bits());
        digest.u32(after.to_bits());
    }
    Ok((relative_change, digest.finish()))
}

fn candidate_evidence_digest(candidates: &LlamaTokenDataArray) -> String {
    let mut digest = StableEvidenceDigest::new("controlled-candidate-array-v1");
    digest.u64(candidates.data.len() as u64);
    for candidate in &candidates.data {
        digest.i32(candidate.id().0);
        digest.u32(candidate.logit().to_bits());
        digest.u32(candidate.p().to_bits());
    }
    digest.finish()
}

fn candidate_transition_evidence(before_sha256: &str, after: &LlamaTokenDataArray) -> String {
    let mut digest = StableEvidenceDigest::new("controlled-candidate-transition-v1");
    digest.text(before_sha256);
    digest.text(&candidate_evidence_digest(after));
    digest.finish()
}

fn terminal_candidate_evidence(
    candidates: &LlamaTokenDataArray,
    selected: LlamaToken,
    generated_index: usize,
) -> String {
    let mut digest = StableEvidenceDigest::new("controlled-terminal-candidate-v1");
    digest.text(&candidate_evidence_digest(candidates));
    digest.i32(selected.0);
    digest.u64(generated_index as u64);
    digest.finish()
}

fn distribution_observation_evidence(observation: &TokenDistributionObservation) -> String {
    let mut digest = StableEvidenceDigest::new("controlled-distribution-observation-runtime-v1");
    digest.u64(observation.generated_index() as u64);
    digest.i32(observation.token_id());
    digest.u64(observation.observations().len() as u64);
    for stage in observation.observations() {
        digest.text(&format!("{:?}", stage.stage()));
        digest.text(&format!("{:?}", stage.value_kind()));
        digest.i32(stage.selected().token_id());
        digest.u32(stage.selected().value().to_bits());
        digest.u64(stage.ranked_candidates().len() as u64);
        for candidate in stage.ranked_candidates() {
            digest.u32(u32::from(candidate.rank()));
            digest.i32(candidate.token().token_id());
            digest.u32(candidate.token().value().to_bits());
        }
    }
    digest.finish()
}

fn apply_eta_cutoff(candidates: &mut LlamaTokenDataArray, eta: f32) -> NativeResult<()> {
    if candidates.data.is_empty() {
        return Err(NativeError::new(
            NativeErrorCode::DecodeFailed,
            "eta cutoff received no candidates",
        ));
    }
    let max = candidates
        .data
        .iter()
        .map(LlamaTokenData::logit)
        .fold(f32::NEG_INFINITY, f32::max);
    let weights = candidates
        .data
        .iter()
        .map(|candidate| (f64::from(candidate.logit()) - f64::from(max)).exp())
        .collect::<Vec<_>>();
    let sum = weights.iter().sum::<f64>();
    if !sum.is_finite() || sum <= 0.0 {
        return Err(NativeError::new(
            NativeErrorCode::DecodeFailed,
            "eta cutoff could not normalize the controlled distribution",
        ));
    }
    let probabilities = weights
        .iter()
        .map(|weight| weight / sum)
        .collect::<Vec<_>>();
    let entropy = probabilities
        .iter()
        .filter(|probability| **probability > 0.0)
        .map(|probability| -probability * probability.ln())
        .sum::<f64>();
    let eta = f64::from(eta);
    let threshold = eta.min(eta.sqrt() * (-entropy).exp());
    retain_by_mask(
        candidates,
        probabilities
            .iter()
            .map(|probability| *probability >= threshold)
            .collect(),
    )
}

fn apply_top_n_sigma(candidates: &mut LlamaTokenDataArray, n: f32) -> NativeResult<()> {
    if candidates.data.is_empty() {
        return Err(NativeError::new(
            NativeErrorCode::DecodeFailed,
            "top-n-sigma received no candidates",
        ));
    }
    let count = candidates.data.len() as f64;
    let mean = candidates
        .data
        .iter()
        .map(|candidate| f64::from(candidate.logit()))
        .sum::<f64>()
        / count;
    let deviation = (candidates
        .data
        .iter()
        .map(|candidate| {
            let centered = f64::from(candidate.logit()) - mean;
            centered * centered
        })
        .sum::<f64>()
        / count)
        .sqrt();
    let max = candidates
        .data
        .iter()
        .map(LlamaTokenData::logit)
        .fold(f32::NEG_INFINITY, f32::max);
    let threshold = f64::from(max) - f64::from(n) * deviation;
    let mask = candidates
        .data
        .iter()
        .map(|candidate| f64::from(candidate.logit()) >= threshold)
        .collect();
    retain_by_mask(candidates, mask)
}

fn retain_by_mask(candidates: &mut LlamaTokenDataArray, mut mask: Vec<bool>) -> NativeResult<()> {
    if mask.len() != candidates.data.len() {
        return Err(NativeError::new(
            NativeErrorCode::Internal,
            "controlled candidate mask dimension mismatch",
        ));
    }
    if !mask.iter().any(|allowed| *allowed)
        && let Some((index, _)) = candidates
            .data
            .iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| left.logit().total_cmp(&right.logit()))
    {
        mask[index] = true;
    }
    let mut index = 0_usize;
    candidates.data.retain(|_| {
        let retain = mask[index];
        index += 1;
        retain
    });
    candidates.selected = None;
    candidates.sorted = false;
    retain_finite_candidates(candidates)
}

#[allow(clippy::too_many_arguments)]
fn build_distribution_observation(
    model: &LlamaModel,
    policy: &DistributionObservationPolicy,
    generated_index: usize,
    token: LlamaToken,
    raw: &DistributionSnapshot,
    post_constraint: &DistributionSnapshot,
    post_guidance: &DistributionSnapshot,
    post_sampler: &DistributionSnapshot,
) -> NativeResult<TokenDistributionObservation> {
    let mut observations = Vec::with_capacity(policy.stages().len() * policy.value_kinds().len());
    for stage in policy.stages() {
        let snapshot = match stage {
            ProbabilityStage::RawModel => raw,
            ProbabilityStage::PostConstraint => post_constraint,
            ProbabilityStage::PostGuidance => post_guidance,
            ProbabilityStage::PostSampler => post_sampler,
        };
        for value_kind in policy.value_kinds() {
            observations.push(build_stage_observation(
                model,
                *stage,
                *value_kind,
                token,
                snapshot,
                policy.include_selected_token(),
                policy.top_k(),
            )?);
        }
    }
    TokenDistributionObservation::new(generated_index, token.0, observations)
}

fn build_stage_observation(
    model: &LlamaModel,
    stage: ProbabilityStage,
    value_kind: DistributionValueKind,
    selected: LlamaToken,
    snapshot: &DistributionSnapshot,
    include_selected_bytes: bool,
    top_k: u16,
) -> NativeResult<StageDistributionObservation> {
    let selected_index = usize::try_from(selected.0).map_err(|_| {
        NativeError::new(
            NativeErrorCode::DecodeFailed,
            "controlled sampler selected a negative token",
        )
    })?;
    if !snapshot
        .allowed
        .get(selected_index)
        .copied()
        .unwrap_or(false)
    {
        return Err(NativeError::new(
            NativeErrorCode::DecodeFailed,
            "selected token is outside an observed causal distribution",
        ));
    }
    let normalization = log_normalization(snapshot)?;
    let value = distribution_value(snapshot.logits[selected_index], value_kind, normalization);
    let selected_value = DistributionTokenValue::new(
        selected.0,
        include_selected_bytes
            .then(|| token_bytes(model, selected))
            .transpose()?,
        value,
    )?;
    let mut ranked = snapshot
        .allowed
        .iter()
        .enumerate()
        .filter_map(|(token_id, allowed)| allowed.then_some(token_id))
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        snapshot.logits[*right]
            .total_cmp(&snapshot.logits[*left])
            .then_with(|| left.cmp(right))
    });
    let requested = usize::from(top_k.min(MAX_DISTRIBUTION_OBSERVATION_TOP_K));
    ranked.truncate(requested.min(ranked.len()));
    let ranked = ranked
        .into_iter()
        .enumerate()
        .map(|(rank, token_id)| {
            let token = LlamaToken::new(token_id as i32);
            let token = DistributionTokenValue::new(
                token.0,
                Some(token_bytes(model, token)?),
                distribution_value(snapshot.logits[token_id], value_kind, normalization),
            )?;
            RankedDistributionCandidate::new((rank + 1) as u16, token)
        })
        .collect::<NativeResult<Vec<_>>>()?;
    StageDistributionObservation::new(stage, value_kind, selected_value, ranked)
}

fn log_normalization(snapshot: &DistributionSnapshot) -> NativeResult<f64> {
    let max = snapshot
        .logits
        .iter()
        .zip(&snapshot.allowed)
        .filter_map(|(logit, allowed)| allowed.then_some(*logit))
        .fold(f32::NEG_INFINITY, f32::max);
    if !max.is_finite() {
        return Err(NativeError::new(
            NativeErrorCode::DecodeFailed,
            "observed distribution has no finite support",
        ));
    }
    let sum = snapshot
        .logits
        .iter()
        .zip(&snapshot.allowed)
        .filter_map(|(logit, allowed)| allowed.then_some(*logit))
        .map(|logit| (f64::from(logit) - f64::from(max)).exp())
        .sum::<f64>();
    if !sum.is_finite() || sum <= 0.0 {
        return Err(NativeError::new(
            NativeErrorCode::DecodeFailed,
            "observed distribution normalization failed",
        ));
    }
    Ok(f64::from(max) + sum.ln())
}

fn distribution_value(logit: f32, kind: DistributionValueKind, log_z: f64) -> f32 {
    match kind {
        DistributionValueKind::Logit => logit,
        DistributionValueKind::Probability => (f64::from(logit) - log_z).exp() as f32,
        DistributionValueKind::LogProbability => ((f64::from(logit) - log_z) as f32).min(0.0),
    }
}

fn token_bytes(model: &LlamaModel, token: LlamaToken) -> NativeResult<Vec<u8>> {
    model
        .token_to_piece_bytes(token, MAX_TOKEN_PIECE_BYTES, true, None)
        .map_err(|error| {
            NativeError::new(
                NativeErrorCode::DecodeFailed,
                format!("failed to capture controlled token-byte evidence: {error}"),
            )
        })
}

fn control_math_error(error: impl std::fmt::Display) -> NativeError {
    NativeError::new(
        NativeErrorCode::InvalidConfig,
        format!("controlled logit arithmetic failed: {error}"),
    )
}

pub(crate) fn validate_submission_identity(
    submission: &ControlledGenerationSubmission,
    fingerprint: &ModelFingerprint,
    live_token_contract: &TokenContractIdentity,
) -> NativeResult<()> {
    let writer = submission.request().control().writer();
    if writer.fingerprint() != fingerprint || writer.token_contract() != live_token_contract {
        return Err(NativeError::new(
            NativeErrorCode::InvalidConfig,
            "controlled writer identity does not match the live model and tokenizer contract",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn finalize_controlled_completion(
    model: &LlamaModel,
    submission: ControlledGenerationSubmission,
    execution: ControlledExecution,
    events: Vec<GenerationEvent>,
    fingerprint: ModelFingerprint,
    live_token_contract: &TokenContractIdentity,
    artifacts: &ModelArtifactGuards,
    strict_precheck: NativeResult<()>,
    admitted_request_sha256: &str,
    owner_call_sequence: u64,
    worker_identity: Arc<WorkerIdentity>,
) -> NativeResult<ControlledGenerationCompletion> {
    validate_submission_identity(&submission, &fingerprint, live_token_contract)?;
    let ControlledGenerationSubmission {
        request,
        constraint_body: _,
    } = submission;
    let ControlledExecution {
        outputs,
        terminal_sampled_token_ids,
        runtime_ledger,
        runtime_cost,
    } = execution;
    let runtime_evidence = runtime_ledger.finalize(&request, runtime_cost)?;
    let event_sha256 = event_stream_digest(&events);
    let participant_reports = participant_reports(&request, &fingerprint)?;
    let declaration = UnverifiedBackendControlDeclaration::new(
        NativeTransport::InProcess,
        true,
        false,
        request.control().fingerprint_sha256(),
        event_sha256.clone(),
        participant_reports,
        runtime_evidence.reports.clone(),
    )?;
    let output = ControlledGenerationBatchOutput::new(request, outputs, declaration)?;
    let authority = strict_precheck.and_then(|()| {
        verify_controlled_authority(
            model,
            &output,
            ControlledAuthorityVerification {
                fingerprint: &fingerprint,
                terminal_sampled_token_ids: &terminal_sampled_token_ids,
                events: &events,
                runtime_evidence: &runtime_evidence,
                runtime_cost,
                admitted_request_sha256,
                artifacts,
            },
        )
    });
    Ok(match authority {
        Ok(()) => {
            let output_sha256 = output.receipt().output_sha256().to_string();
            let ledger_sha256 = controlled_authority_ledger_sha256(
                admitted_request_sha256,
                &output_sha256,
                &event_sha256,
                &runtime_evidence.ledger_sha256,
                &fingerprint,
                owner_call_sequence,
            );
            ControlledGenerationCompletion::verified(
                output,
                ControlledGenerationEvidence {
                    model_fingerprint: fingerprint,
                    request_sha256: admitted_request_sha256.to_string(),
                    output_sha256,
                    event_stream_sha256: event_sha256,
                    runtime_operation_ledger_sha256: runtime_evidence.ledger_sha256,
                    ledger_sha256,
                    owner_call_sequence,
                    runtime_cost,
                    terminal_sampled_token_ids,
                    events,
                    worker_identity,
                },
            )
        }
        Err(error) => ControlledGenerationCompletion::authority_rejected(output, error),
    })
}

fn participant_reports(
    request: &ControlledGenerationBatchRequest,
    fingerprint: &ModelFingerprint,
) -> NativeResult<Vec<BackendParticipantReport>> {
    let writer = request.control().writer();
    let mut evidence = StableEvidenceDigest::new("controlled-participant-load-v1");
    evidence.text(&writer.identity_sha256());
    evidence.text(&fingerprint.model_sha256);
    evidence.text(&fingerprint.tokenizer_sha256);
    evidence.text(&fingerprint.binding_version);
    evidence.text(&fingerprint.build_id);
    evidence.text(LLAMA_NATIVE_BUILD_MANIFEST_SHA256);
    Ok(vec![BackendParticipantReport::new(
        writer.participant_id().to_string(),
        writer.identity_sha256(),
        evidence.finish(),
    )?])
}

fn event_stream_digest(events: &[GenerationEvent]) -> String {
    let mut digest = StableEvidenceDigest::new("controlled-event-stream-v1");
    for event in events {
        digest.text(&event.request_id);
        digest.text(&event.branch_id);
        digest.i32(event.sequence_id);
        digest.u64(event.input_index as u64);
        digest.u64(event.event_index);
        match &event.event {
            GenerationEventKind::State { state } => {
                digest.text("state");
                digest.text(&format!("{state:?}"));
            }
            GenerationEventKind::Delta { text } => {
                digest.text("delta");
                digest.text(text);
            }
            GenerationEventKind::Warning { code, message } => {
                digest.text("warning");
                digest.text(code);
                digest.text(message);
            }
        }
    }
    digest.finish()
}

struct ControlledAuthorityVerification<'a> {
    fingerprint: &'a ModelFingerprint,
    terminal_sampled_token_ids: &'a [Option<i32>],
    events: &'a [GenerationEvent],
    runtime_evidence: &'a FinalizedRuntimeControlEvidence,
    runtime_cost: ControlledRuntimeCostEvidence,
    admitted_request_sha256: &'a str,
    artifacts: &'a ModelArtifactGuards,
}

fn verify_controlled_authority(
    model: &LlamaModel,
    output: &ControlledGenerationBatchOutput,
    verification: ControlledAuthorityVerification<'_>,
) -> NativeResult<()> {
    let ControlledAuthorityVerification {
        fingerprint,
        terminal_sampled_token_ids,
        events,
        runtime_evidence,
        runtime_cost,
        admitted_request_sha256,
        artifacts,
    } = verification;
    let request = output.request();
    if runtime_cost != controlled_runtime_cost(request)?
        || admitted_request_sha256 != request.fingerprint_sha256()
        || output.receipt().request_sha256() != admitted_request_sha256
        || request.control().writer().fingerprint() != fingerprint
        || output.cases().len() != request.cases().len()
        || terminal_sampled_token_ids.len() != request.cases().len()
    {
        return Err(generation_verification_error(
            "controlled authority identity or ordered output count disagrees",
        ));
    }
    let expected_participants = participant_reports(request, fingerprint)?;
    let declaration = output.receipt().backend_declaration();
    if declaration.transport() != NativeTransport::InProcess
        || !declaration.real_engine_invoked()
        || declaration.fake_fixture()
    {
        return Err(generation_verification_error(
            "controlled backend declaration lacks real in-process transport evidence",
        ));
    }
    if declaration.backend_event_stream_sha256() != event_stream_digest(events) {
        return Err(generation_verification_error(
            "controlled backend event digest disagrees with the retained live stream",
        ));
    }
    if declaration.participant_reports() != expected_participants {
        return Err(generation_verification_error(
            "controlled backend participant report disagrees with the resident writer",
        ));
    }
    if declaration.applied_operations() != runtime_evidence.reports {
        return Err(generation_verification_error(
            "controlled backend operation reports disagree with the private runtime ledger",
        ));
    }
    if !runtime_evidence.every_requested_operation_effective {
        let ineffective = runtime_evidence
            .ineffective_operations
            .iter()
            .map(|operation| format!("{:?}", operation.kind()))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(generation_verification_error(format!(
            "requested controlled operations were invoked but had no observable effect: {ineffective}"
        )));
    }

    let mut next_event_indexes = vec![0_u64; request.cases().len()];
    let mut terminals = vec![None; request.cases().len()];
    let mut deltas = vec![String::new(); request.cases().len()];
    for event in events {
        let Some(case) = request.cases().get(event.input_index) else {
            return Err(generation_verification_error(
                "controlled event input index exceeds the submitted batch",
            ));
        };
        if event.request_id != request.request_id()
            || event.branch_id != case.case_id()
            || event.sequence_id != event.input_index as i32
            || event.event_index != next_event_indexes[event.input_index]
            || terminals[event.input_index].is_some()
        {
            return Err(generation_verification_error(
                "controlled event identity, ordering, or terminality disagrees",
            ));
        }
        match &event.event {
            GenerationEventKind::State {
                state: GenerationState::Prefilling,
            } if event.event_index == 0 => {}
            GenerationEventKind::State {
                state: GenerationState::Generating,
            } if event.event_index == 1 => {}
            GenerationEventKind::State { state }
                if event.event_index >= 2
                    && matches!(
                        state,
                        GenerationState::Completed | GenerationState::Cancelled
                    ) =>
            {
                terminals[event.input_index] = Some(*state);
            }
            GenerationEventKind::Delta { text } if event.event_index >= 2 && !text.is_empty() => {
                deltas[event.input_index].push_str(text);
            }
            GenerationEventKind::Warning { .. }
            | GenerationEventKind::State { .. }
            | GenerationEventKind::Delta { .. } => {
                return Err(generation_verification_error(
                    "controlled authority ledger contains an impossible event transition",
                ));
            }
        }
        next_event_indexes[event.input_index] += 1;
    }

    for (index, ((case, case_output), terminal_token)) in request
        .cases()
        .iter()
        .zip(output.cases())
        .zip(terminal_sampled_token_ids)
        .enumerate()
    {
        let generation = case_output.generation();
        let decoded = decode_verified_token_text(model, &generation.generated_token_ids)?;
        if generation.request_id != request.request_id()
            || generation.branch_id != case.case_id()
            || generation.input_index != index
            || generation.model_id != fingerprint.model_id
            || !generation.real_engine_invoked
            || generation.fake_fixture
            || generation.transport != NativeTransport::InProcess
            || terminals[index] != Some(generation.state)
            || deltas[index] != decoded
            || generation.metrics.prompt_tokens != case.conditional_prompt().token_ids().len()
            || generation.metrics.completion_tokens != generation.generated_token_ids.len()
            || generation.state != GenerationState::Completed
            || generation.metrics.shared_prefix_tokens
                != runtime_cost.conditional_shared_prefix_tokens
            || generation.metrics.cache.supplied_prefix_tokens != 0
            || generation.metrics.cache.restored_prefix_tokens != 0
            || generation.metrics.cache.batch_shared_prefix_tokens
                != runtime_cost.conditional_shared_prefix_tokens
        {
            return Err(generation_verification_error(
                "controlled output is not causally joined to its exact prompt and event ledger",
            ));
        }
        match (generation.state, generation.finish_reason.as_str()) {
            (GenerationState::Cancelled, "cancelled")
                if terminal_token.is_none() && generation.text == decoded => {}
            (GenerationState::Completed, "end_of_generation")
                if terminal_token
                    .is_some_and(|token| model.is_eog_token(LlamaToken::new(token)))
                    && generation.text == decoded => {}
            (GenerationState::Completed, "max_tokens")
                if terminal_token.is_none()
                    && generation.text == decoded
                    && generation.generated_token_ids.len()
                        == case.sampling().max_tokens as usize => {}
            (GenerationState::Completed, "stop_sequence")
                if terminal_token.is_none()
                    && case.sampling().stop.iter().any(|stop| {
                        !stop.is_empty() && decoded == format!("{}{stop}", generation.text)
                    }) => {}
            _ => {
                return Err(generation_verification_error(
                    "controlled terminal token, text projection, and finish reason disagree",
                ));
            }
        }
    }
    artifacts.verify_strict_unchanged(fingerprint)
}

fn controlled_authority_ledger_sha256(
    request_sha256: &str,
    output_sha256: &str,
    event_stream_sha256: &str,
    runtime_operation_ledger_sha256: &str,
    fingerprint: &ModelFingerprint,
    owner_call_sequence: u64,
) -> String {
    let mut digest = StableEvidenceDigest::new("verified-controlled-generation-v1");
    digest.text(request_sha256);
    digest.text(output_sha256);
    digest.text(event_stream_sha256);
    digest.text(runtime_operation_ledger_sha256);
    digest.text(&fingerprint.model_id);
    digest.text(&fingerprint.model_sha256);
    digest.text(&fingerprint.tokenizer_sha256);
    digest.text(&fingerprint.binding_version);
    digest.text(&fingerprint.build_id);
    digest.u64(owner_call_sequence);
    digest.finish()
}

pub(crate) fn emit_missing_failed_terminals(
    event_tx: &Sender<GenerationEvent>,
    request: &ControlledGenerationBatchRequest,
    retained_events: &mut Vec<GenerationEvent>,
) {
    for (case_index, _) in request.cases().iter().enumerate() {
        let terminal = retained_events.iter().any(|event| {
            event.input_index == case_index
                && matches!(
                    event.event,
                    GenerationEventKind::State { state }
                        if matches!(state, GenerationState::Completed | GenerationState::Cancelled | GenerationState::Failed)
                )
        });
        if terminal {
            continue;
        }
        let next_index = retained_events
            .iter()
            .filter(|event| event.input_index == case_index)
            .count() as u64;
        emit_controlled_event(
            event_tx,
            retained_events,
            request,
            case_index,
            next_index,
            GenerationEventKind::State {
                state: GenerationState::Failed,
            },
        );
    }
}

pub(crate) fn reject_queued_controlled(
    submission: ControlledGenerationSubmission,
    event_tx: Sender<GenerationEvent>,
    result_tx: Sender<NativeResult<ControlledGenerationCompletion>>,
    cancellations: Vec<Arc<AtomicBool>>,
) {
    for flag in cancellations {
        flag.store(true, Ordering::Release);
    }
    let request = submission.request();
    for (case_index, case) in request.cases().iter().enumerate() {
        try_emit_terminal(
            &event_tx,
            GenerationEvent {
                request_id: request.request_id().to_string(),
                branch_id: case.case_id().to_string(),
                sequence_id: case_index as i32,
                input_index: case_index,
                event_index: 0,
                event: GenerationEventKind::State {
                    state: GenerationState::Cancelled,
                },
            },
        );
    }
    let _ = result_tx.send(Err(NativeError::new(
        NativeErrorCode::Cancelled,
        "native model shutdown cancelled an admitted queued controlled request",
    )));
}

#[derive(Debug)]
struct StableEvidenceDigest(Sha256);

impl StableEvidenceDigest {
    fn new(domain: &str) -> Self {
        let mut value = Self(Sha256::new());
        value.text(domain);
        value
    }

    fn bytes(&mut self, value: &[u8]) {
        self.u64(value.len() as u64);
        self.0.update(value);
    }

    fn text(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    fn bool(&mut self, value: bool) {
        self.0.update([u8::from(value)]);
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

    fn finish(self) -> String {
        format!("{:x}", self.0.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llama_native_types::{
        ConstraintArtifactReference, ControlProgram, DistributionValueKindSet, EtaCutoff, ExactF32,
        ExactTokenPrompt, ExtendedSamplerProgram, MirostatV1Config, MirostatV2Config,
        ProbabilityStageSet, SparseLogitBias, StaticAdapter, StaticControlArtifact,
        StaticControlProfile, TokenLogitBias, TopNSigma,
    };

    static CONTROL_REAL_MODEL_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn fingerprint(model_id: &str, model_hash: char) -> ModelFingerprint {
        ModelFingerprint {
            model_id: model_id.to_string(),
            model_size: 1024,
            model_sha256: model_hash.to_string().repeat(64),
            tokenizer_sha256: "b".repeat(64),
            chat_template_sha256: "c".repeat(64),
            multimodal_projector_sha256: None,
            binding_version: LLAMA_CPP_BINDING_VERSION.to_string(),
            build_id: "controlled-test-build".to_string(),
            backend: "cpu".to_string(),
            context_tokens: 256,
            batch_tokens: 64,
            max_sequences: 4,
            rope_config_sha256: "d".repeat(64),
            kv_layout_sha256: "e".repeat(64),
        }
    }

    fn token_contract() -> TokenContractIdentity {
        TokenContractIdentity::new(
            "b".repeat(64),
            "f".repeat(64),
            "1".repeat(64),
            "2".repeat(64),
        )
        .expect("test token contract")
    }

    fn writer() -> ControlledModelIdentity {
        ControlledModelIdentity::new(
            "writer".to_string(),
            fingerprint("model", 'a'),
            token_contract(),
        )
        .expect("test writer")
    }

    fn case(id: &str, unconditional: bool) -> llama_native_types::ControlledGenerationCase {
        llama_native_types::ControlledGenerationCase::new(
            id.to_string(),
            ExactTokenPrompt::new(vec![1, 2, 3]).expect("conditional prompt"),
            unconditional.then(|| ExactTokenPrompt::new(vec![1, 4]).expect("unconditional prompt")),
            SamplingConfig {
                seed: 17,
                temperature: 0.8,
                max_tokens: 4,
                ..SamplingConfig::default()
            },
        )
        .expect("test case")
    }

    fn request(
        guidance: Vec<GuidanceControl>,
        static_profiles: Vec<StaticControlProfile>,
        auxiliary: Vec<ControlledModelIdentity>,
    ) -> ControlledGenerationBatchRequest {
        let cfg = guidance
            .iter()
            .any(|control| matches!(control, GuidanceControl::SameModelCfg { .. }));
        let program = ControlProgram::new(
            writer(),
            auxiliary,
            None,
            guidance,
            ExtendedSamplerProgram::default(),
            TerminalSelector::Distribution,
            DistributionObservationPolicy::default(),
            static_profiles,
        )
        .expect("test control program");
        ControlledGenerationBatchRequest::new(
            "controlled-request".to_string(),
            vec![case("case-0", cfg)],
            program,
        )
        .expect("test request")
    }

    fn ready_status() -> ResidentModelStatus {
        ResidentModelStatus {
            model_id: "model".to_string(),
            model_path: std::path::PathBuf::new(),
            state: ModelRuntimeState::Ready,
            fingerprint: Some(fingerprint("model", 'a')),
            descriptor: None,
            active_sequences: 0,
            max_sequences: 4,
        }
    }

    #[test]
    fn disabled_control_request_maps_byte_for_byte_to_exact_legacy_input() {
        let request = request(Vec::new(), Vec::new(), Vec::new());
        assert!(is_disabled_control_baseline(&request));
        let legacy = as_exact_legacy_request(&request);
        assert_eq!(legacy.request_id, request.request_id());
        assert_eq!(legacy.model_id, "model");
        assert_eq!(legacy.cases.len(), 1);
        assert_eq!(legacy.cases[0].sampling, *request.cases()[0].sampling());
        assert_eq!(
            legacy.cases[0].input,
            GenerationInput::Completion {
                prompts: vec![CompletionPrompt::Tokens {
                    token_ids: vec![1, 2, 3]
                }]
            }
        );
    }

    #[test]
    fn sparse_vocabulary_slots_have_explicit_nonrendered_contract_states() {
        assert_eq!(
            sparse_token_contract_piece(0),
            Some(ExactTokenContractPiece::Undefined)
        );
        assert_eq!(
            sparse_token_contract_piece(LlamaTokenAttr::Unused as u32),
            Some(ExactTokenContractPiece::Unused)
        );
        assert_eq!(
            sparse_token_contract_piece(LlamaTokenAttr::Normal as u32),
            None,
            "renderable slots still require exact upstream bytes"
        );
        assert_ne!(
            {
                let mut digest = StableEvidenceDigest::new("token-piece-test-v1");
                digest.text("undefined");
                digest.finish()
            },
            {
                let mut digest = StableEvidenceDigest::new("token-piece-test-v1");
                digest.text("unused");
                digest.finish()
            },
            "undefined and unused slots cannot alias in the token-byte contract"
        );
    }

    #[test]
    fn disabled_baseline_authority_requires_exact_sampler_site_trace() {
        let request = request(Vec::new(), Vec::new(), Vec::new());
        let output = GenerationOutput {
            request_id: request.request_id().to_string(),
            branch_id: "case-0".to_string(),
            input_index: 0,
            model_id: "model".to_string(),
            text: "x".to_string(),
            generated_token_ids: vec![7],
            token_observations: None,
            state: GenerationState::Completed,
            finish_reason: "max_tokens".to_string(),
            metrics: GenerationMetrics {
                prompt_tokens: 3,
                completion_tokens: 1,
                shared_prefix_tokens: 0,
                duration_ms: 1,
                first_token_ms: Some(1),
                tokens_per_second: 1.0,
                cache: GenerationCacheMetrics::default(),
            },
            real_engine_invoked: true,
            fake_fixture: false,
            transport: NativeTransport::InProcess,
        };
        let execution = GeneratedBatchExecution {
            outputs: vec![output],
            terminal_sampled_token_ids: vec![None],
            token_piece_traces: vec![TokenPieceTrace {
                raw_piece_bytes: vec![b'x'],
                cumulative_boundaries: vec![0, 1],
            }],
        };
        assert!(
            join_baseline_runtime_trace(&execution, &[], &mut RuntimeControlLedger::new(&request))
                .is_err(),
            "terminal output cannot synthesize an absent sampler-site trace"
        );
        assert!(
            join_baseline_runtime_trace(
                &execution,
                &[RuntimeSampleSelection {
                    case_index: 0,
                    generated_index: 0,
                    token_id: 8,
                    terminal: false,
                }],
                &mut RuntimeControlLedger::new(&request),
            )
            .is_err(),
            "a trace for a different sampled token cannot be joined"
        );

        let mut ledger = RuntimeControlLedger::new(&request);
        join_baseline_runtime_trace(
            &execution,
            &[RuntimeSampleSelection {
                case_index: 0,
                generated_index: 0,
                token_id: 7,
                terminal: false,
            }],
            &mut ledger,
        )
        .expect("exact sampler-site trace joins");
        assert!(
            ledger
                .finalize(&request, controlled_runtime_cost(&request).expect("cost"))
                .expect("complete runtime ledger")
                .every_requested_operation_effective
        );
    }

    #[test]
    fn cfg_cost_is_explicit_and_single_resident_capabilities_stay_false_elsewhere() {
        let request = request(
            vec![GuidanceControl::SameModelCfg {
                scale: ExactF32::new(1.5).expect("scale"),
                rescale: Some(ExactF32::new(0.25).expect("rescale")),
            }],
            Vec::new(),
            Vec::new(),
        );
        let writer_cost = &request.cost().participants()[0];
        assert_eq!(writer_cost.conditional_sequences(), 1);
        assert_eq!(writer_cost.unconditional_sequences(), 1);
        assert_eq!(request.cost().total_sequence_slots(), 2);
        assert_eq!(writer_cost.maximum_generated_evaluations(), 8);
        let capabilities =
            ControlledGenerationCapabilities::inspected(true, false, true, false, false);
        assert!(capabilities.same_model_cfg());
        assert!(capabilities.power_sampling());
        assert!(!capabilities.multi_model_logit_arithmetic());
        assert!(!capabilities.static_adapter_profiles());
        assert!(!capabilities.static_activation_vectors());
    }

    #[test]
    fn static_profiles_and_auxiliary_models_fail_before_queue_admission() {
        let artifact =
            StaticControlArtifact::new("adapter".to_string(), "3".repeat(64), 16, "4".repeat(64))
                .expect("artifact");
        let static_request = request(
            Vec::new(),
            vec![StaticControlProfile::AdapterStack {
                profile_id: "profile".to_string(),
                participant_id: "writer".to_string(),
                adapters: vec![StaticAdapter {
                    artifact,
                    scale: ExactF32::new(1.0).expect("scale"),
                }],
            }],
            Vec::new(),
        );
        let static_submission =
            ControlledGenerationSubmission::new(static_request, None).expect("submission");
        assert_eq!(
            preflight_submission(&static_submission, &ready_status())
                .expect_err("static profile must fail")
                .code,
            NativeErrorCode::UnsupportedParameter
        );

        let auxiliary = ControlledModelIdentity::new(
            "amateur".to_string(),
            fingerprint("amateur", '9'),
            token_contract(),
        )
        .expect("auxiliary");
        let auxiliary_request = request(
            vec![GuidanceControl::ContrastiveExpertAmateur {
                amateur_participant_id: "amateur".to_string(),
                primary_coefficient: ExactF32::new(1.0).expect("primary"),
                amateur_coefficient: ExactF32::new(-1.0).expect("amateur"),
            }],
            Vec::new(),
            vec![auxiliary],
        );
        let auxiliary_submission =
            ControlledGenerationSubmission::new(auxiliary_request, None).expect("submission");
        assert_eq!(
            preflight_submission(&auxiliary_submission, &ready_status())
                .expect_err("auxiliary model must fail")
                .code,
            NativeErrorCode::UnsupportedParameter
        );
    }

    #[test]
    fn constraint_body_must_match_exact_reference_before_admission() {
        let body = "root ::= \"ok\"";
        let reference = ConstraintArtifactReference::new(
            "grammar".to_string(),
            format!("{:x}", Sha256::digest(body.as_bytes())),
            body.len() as u32,
        )
        .expect("reference");
        let program = ControlProgram::new(
            writer(),
            Vec::new(),
            Some(StructuredConstraint::Gbnf { reference }),
            Vec::new(),
            ExtendedSamplerProgram::default(),
            TerminalSelector::Distribution,
            DistributionObservationPolicy::default(),
            Vec::new(),
        )
        .expect("program");
        let request = ControlledGenerationBatchRequest::new(
            "constraint-request".to_string(),
            vec![case("case-0", false)],
            program,
        )
        .expect("request");
        assert!(
            ControlledGenerationSubmission::new(request.clone(), Some(body.to_string())).is_ok()
        );
        assert!(
            ControlledGenerationSubmission::new(
                request.clone(),
                Some("root ::= \"no\"".to_string())
            )
            .is_err()
        );
        assert!(ControlledGenerationSubmission::new(request, None).is_err());
    }

    #[test]
    fn queued_shutdown_emits_exactly_one_cancelled_terminal_per_case() {
        let request = request(Vec::new(), Vec::new(), Vec::new());
        let submission =
            ControlledGenerationSubmission::new(request.clone(), None).expect("submission");
        let (event_tx, event_rx) = bounded(EVENT_CAPACITY);
        let (result_tx, result_rx) = bounded(1);
        let flags = request
            .cases()
            .iter()
            .map(|_| Arc::new(AtomicBool::new(false)))
            .collect::<Vec<_>>();
        reject_queued_controlled(submission, event_tx, result_tx, flags.clone());
        assert!(flags.iter().all(|flag| flag.load(Ordering::Acquire)));
        assert_eq!(
            result_rx
                .recv()
                .expect("result")
                .expect_err("queued request is cancelled")
                .code,
            NativeErrorCode::Cancelled
        );
        let events = event_rx.try_iter().collect::<Vec<_>>();
        assert_eq!(events.len(), request.cases().len());
        assert!(events.iter().all(|event| matches!(
            event.event,
            GenerationEventKind::State {
                state: GenerationState::Cancelled
            }
        )));
    }

    #[test]
    fn stale_controlled_ticket_cannot_remove_reused_request_identity() {
        let old = Arc::new(AtomicBool::new(false));
        let old_registry = Arc::new(RequestRegistry::new());
        let (old_control, old_lease) = old_registry
            .reserve(
                "request",
                RequestClass::ControlledGeneration,
                RequestControls::ControlledGeneration {
                    cancellations: vec![("case".to_owned(), Arc::clone(&old))],
                },
            )
            .expect("old request reserves");
        drop(old_lease);
        let (_result_tx, result_rx) = bounded(1);
        let (_event_tx, event_rx) = bounded(1);
        let ticket = ControlledGenerationTicket {
            request_id: "request".to_string(),
            events: event_rx,
            result: result_rx,
            control: old_control,
        };
        let replacement = Arc::new(AtomicBool::new(false));
        let replacement_registry = Arc::new(RequestRegistry::new());
        let (_replacement_control, _replacement_lease) = replacement_registry
            .reserve(
                "request",
                RequestClass::ControlledGeneration,
                RequestControls::ControlledGeneration {
                    cancellations: vec![("case".to_owned(), Arc::clone(&replacement))],
                },
            )
            .expect("replacement request reserves");
        drop(ticket);
        assert!(!replacement.load(Ordering::Acquire));
        assert_eq!(replacement_registry.active_count(), 1);
    }

    #[test]
    fn controlled_submission_rejects_a_generation_namespace_duplicate_before_queueing() {
        let (command_tx, command_rx) = bounded(COMMAND_CAPACITY);
        let (shutdown_tx, _shutdown_rx) = bounded(1);
        let requests = Arc::new(RequestRegistry::new());
        let (_control, _lease) = requests
            .reserve(
                "controlled-request",
                RequestClass::Generation,
                RequestControls::Generation {
                    cancellations: Vec::new(),
                    reasoning_forces: Vec::new(),
                },
            )
            .expect("existing request reserves");
        let handle = NativeModelHandle {
            inner: Arc::new(NativeModelInner {
                worker_identity: Arc::new(WorkerIdentity),
                command_tx,
                shutdown_tx,
                closing: AtomicBool::new(false),
                admission: Mutex::new(()),
                requests,
                status: Arc::new(RwLock::new(ready_status())),
            }),
        };
        let error = handle
            .generate_controlled(
                ControlledGenerationSubmission::new(
                    request(Vec::new(), Vec::new(), Vec::new()),
                    None,
                )
                .expect("submission"),
            )
            .expect_err("duplicate request IDs fail before queue admission");
        assert_eq!(error.code, NativeErrorCode::DuplicateActiveRequest);
        assert!(command_rx.try_recv().is_err());
    }

    #[test]
    fn dropped_controlled_ticket_keeps_request_id_reserved_until_executor_terminal() {
        let (command_tx, command_rx) = bounded(COMMAND_CAPACITY);
        let (shutdown_tx, _shutdown_rx) = bounded(1);
        let handle = NativeModelHandle {
            inner: Arc::new(NativeModelInner {
                worker_identity: Arc::new(WorkerIdentity),
                command_tx,
                shutdown_tx,
                closing: AtomicBool::new(false),
                admission: Mutex::new(()),
                requests: Arc::new(RequestRegistry::new()),
                status: Arc::new(RwLock::new(ready_status())),
            }),
        };
        let submission = || {
            ControlledGenerationSubmission::new(request(Vec::new(), Vec::new(), Vec::new()), None)
                .expect("submission")
        };

        let ticket = handle
            .generate_controlled(submission())
            .expect("first controlled request is admitted");
        drop(ticket);
        assert_eq!(handle.inner.requests.active_count(), 1);
        assert_eq!(
            handle
                .generate_controlled(submission())
                .expect_err("ticket drop cannot release executor identity")
                .code,
            NativeErrorCode::DuplicateActiveRequest
        );

        reject_queued_command(command_rx.recv().expect("executor owns controlled command"));
        assert_eq!(handle.inner.requests.active_count(), 0);

        let retry = handle
            .generate_controlled(submission())
            .expect("controlled identity is reusable after terminal");
        reject_queued_command(command_rx.recv().expect("retry controlled command"));
        drop(retry);
        assert_eq!(handle.inner.requests.active_count(), 0);
    }

    #[test]
    fn masked_eta_and_top_sigma_are_deterministic_and_never_empty() {
        let source = [0.0, -0.5, -2.0, -4.0];
        let mut first = LlamaTokenDataArray::from_iter(
            source
                .iter()
                .enumerate()
                .map(|(id, logit)| LlamaTokenData::new(LlamaToken::new(id as i32), *logit, 0.0)),
            false,
        );
        let mut second = first.clone();
        apply_eta_cutoff(&mut first, 0.1).expect("eta");
        apply_eta_cutoff(&mut second, 0.1).expect("eta");
        assert_eq!(first, second);
        apply_top_n_sigma(&mut first, 0.01).expect("top sigma");
        assert!(!first.data.is_empty());
        assert_eq!(first.data[0].id(), LlamaToken::new(0));
    }

    #[test]
    fn runtime_effect_requires_a_relative_change_on_allowed_support() {
        let before = [1.0, 2.0, 3.0];
        let uniformly_shifted = [6.0, 7.0, 8.0];
        let (effective, _) =
            distribution_transition_evidence(&before, &uniformly_shifted, &[true, true, true])
                .expect("uniform transition");
        assert!(!effective, "a uniform logit shift cannot affect sampling");

        let masked_only = [1.0, 2.0, 30.0];
        let (effective, _) =
            distribution_transition_evidence(&before, &masked_only, &[true, true, false])
                .expect("masked transition");
        assert!(!effective, "changes outside allowed support are not causal");

        let relative = [1.0, 2.5, 3.0];
        let (effective, _) =
            distribution_transition_evidence(&before, &relative, &[true, true, true])
                .expect("relative transition");
        assert!(effective);
    }

    #[test]
    fn json_schema_uses_upstream_safe_converter() {
        let grammar = json_schema_to_grammar(
            r#"{"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}"#,
        )
        .expect("valid JSON schema compiles");
        assert!(grammar.contains("root"));
    }

    #[test]
    fn runtime_ledger_rejects_skipped_operations_and_marks_noops_ineligible() {
        let request = request(Vec::new(), Vec::new(), Vec::new());
        let mut skipped = RuntimeControlLedger::new(&request);
        skipped.begin_decision(0, 0).expect("begin decision");
        assert!(
            skipped
                .finalize(&request, controlled_runtime_cost(&request).expect("cost"))
                .is_err(),
            "a request-shaped plan without a runtime invocation must fail closed"
        );

        let mut no_op = RuntimeControlLedger::new(&request);
        no_op.begin_decision(0, 0).expect("begin decision");
        no_op
            .record(
                0,
                0,
                AppliedControlKind::OrdinaryDistributionSelector,
                ControlApplicationStage::Sampler,
                false,
                &"a".repeat(64),
            )
            .expect("record runtime invocation");
        no_op.finish_decision(0).expect("finish decision");
        let finalized = no_op
            .finalize(&request, controlled_runtime_cost(&request).expect("cost"))
            .expect("complete invocation trace remains diagnostic");
        assert_eq!(finalized.reports.len(), 1);
        assert!(!finalized.every_requested_operation_effective);
    }

    #[test]
    fn runtime_ledger_rejects_requested_operation_reordering() {
        let request = request(
            vec![
                GuidanceControl::SameModelCfg {
                    scale: ExactF32::new(1.5).expect("scale"),
                    rescale: None,
                },
                GuidanceControl::PowerSampling {
                    exponent: ExactF32::new(1.2).expect("power"),
                },
            ],
            Vec::new(),
            Vec::new(),
        );
        let mut ledger = RuntimeControlLedger::new(&request);
        ledger.begin_decision(0, 0).expect("begin decision");
        let error = ledger
            .record(
                0,
                0,
                AppliedControlKind::PowerSampling,
                ControlApplicationStage::Guidance,
                true,
                &"b".repeat(64),
            )
            .expect_err("power cannot be reported before CFG actually runs");
        assert_eq!(error.code, NativeErrorCode::Internal);
    }

    #[test]
    fn runtime_ledger_requires_each_operation_to_affect_every_case() {
        let program = ControlProgram::new(
            writer(),
            Vec::new(),
            None,
            Vec::new(),
            ExtendedSamplerProgram::default(),
            TerminalSelector::Distribution,
            DistributionObservationPolicy::default(),
            Vec::new(),
        )
        .expect("program");
        let sampling = SamplingConfig {
            max_tokens: 1,
            ..SamplingConfig::default()
        };
        let cases = [vec![1, 2, 3], vec![1, 2, 4]]
            .into_iter()
            .enumerate()
            .map(|(index, tokens)| {
                llama_native_types::ControlledGenerationCase::new(
                    format!("case-{index}"),
                    ExactTokenPrompt::new(tokens).expect("prompt"),
                    None,
                    sampling.clone(),
                )
                .expect("case")
            })
            .collect();
        let request =
            ControlledGenerationBatchRequest::new("per-case-effect".to_string(), cases, program)
                .expect("request");
        let mut ledger = RuntimeControlLedger::new(&request);
        for (case_index, effective) in [true, false].into_iter().enumerate() {
            ledger
                .begin_decision(case_index, 0)
                .expect("begin decision");
            ledger
                .record(
                    case_index,
                    0,
                    AppliedControlKind::OrdinaryDistributionSelector,
                    ControlApplicationStage::Sampler,
                    effective,
                    &format!("{case_index:x}").repeat(64),
                )
                .expect("runtime selection");
            ledger.finish_decision(case_index).expect("finish decision");
        }
        let finalized = ledger
            .finalize(&request, controlled_runtime_cost(&request).expect("cost"))
            .expect("complete ledger remains diagnostic");
        assert!(!finalized.every_requested_operation_effective);
        assert_eq!(finalized.ineffective_operations.len(), 1);
    }

    #[test]
    fn exact_prefix_cost_distinguishes_logical_charge_from_physical_reuse() {
        let program = ControlProgram::new(
            writer(),
            Vec::new(),
            None,
            Vec::new(),
            ExtendedSamplerProgram::default(),
            TerminalSelector::Distribution,
            DistributionObservationPolicy::default(),
            Vec::new(),
        )
        .expect("program");
        let sampling = SamplingConfig {
            max_tokens: 4,
            ..SamplingConfig::default()
        };
        let cases = [vec![1, 2, 3], vec![1, 2, 4]]
            .into_iter()
            .enumerate()
            .map(|(index, tokens)| {
                llama_native_types::ControlledGenerationCase::new(
                    format!("case-{index}"),
                    ExactTokenPrompt::new(tokens).expect("prompt"),
                    None,
                    sampling.clone(),
                )
                .expect("case")
            })
            .collect();
        let request =
            ControlledGenerationBatchRequest::new("shared-prefix-cost".to_string(), cases, program)
                .expect("request");
        let cost = controlled_runtime_cost(&request).expect("runtime cost");
        assert_eq!(request.cost().exact_prompt_tokens(), 6);
        assert_eq!(cost.conditional_shared_prefix_tokens(), 2);
        assert_eq!(cost.unconditional_shared_prefix_tokens(), 0);
        assert_eq!(cost.physical_prompt_evaluations(), 4);
        assert_eq!(cost.reserved_physical_context_cells(), 12);
        assert_eq!(cost.sequence_slots(), 2);
    }

    #[test]
    fn sparse_bias_program_cannot_hide_behind_truncation_or_encode_zero_work() {
        let zero = SparseLogitBias::new(vec![TokenLogitBias {
            token_id: 1,
            bias: 0.0,
        }])
        .expect("bounded sparse map");
        assert!(
            ExtendedSamplerProgram::new(vec![ExtendedSampler::SparseLogitBias { biases: zero }])
                .is_err()
        );
        let bias = SparseLogitBias::new(vec![TokenLogitBias {
            token_id: 1,
            bias: 2.0,
        }])
        .expect("bias");
        assert!(
            ExtendedSamplerProgram::new(vec![
                ExtendedSampler::EtaCutoff {
                    cutoff: EtaCutoff::new(0.1).expect("eta"),
                },
                ExtendedSampler::SparseLogitBias { biases: bias },
            ])
            .is_err(),
            "bias after a support filter would permit a silently skipped token"
        );
    }

    #[test]
    fn controlled_joined_worker_binding_is_exact_instance_identity() {
        let request = request(Vec::new(), Vec::new(), Vec::new());
        let mut ledger = RuntimeControlLedger::new(&request);
        ledger.begin_decision(0, 0).expect("begin decision");
        ledger
            .record(
                0,
                0,
                AppliedControlKind::OrdinaryDistributionSelector,
                ControlApplicationStage::Sampler,
                true,
                &"a".repeat(64),
            )
            .expect("runtime selection");
        ledger.finish_decision(0).expect("finish decision");
        let runtime_cost = controlled_runtime_cost(&request).expect("cost");
        let runtime = ledger
            .finalize(&request, runtime_cost)
            .expect("runtime ledger");
        let fingerprint = request.control().writer().fingerprint().clone();
        let declaration = UnverifiedBackendControlDeclaration::new(
            NativeTransport::InProcess,
            true,
            false,
            request.control().fingerprint_sha256(),
            "b".repeat(64),
            participant_reports(&request, &fingerprint).expect("participants"),
            runtime.reports,
        )
        .expect("declaration");
        let generation = GenerationOutput {
            request_id: request.request_id().to_string(),
            branch_id: request.cases()[0].case_id().to_string(),
            input_index: 0,
            model_id: fingerprint.model_id.clone(),
            text: "x".to_string(),
            generated_token_ids: vec![7],
            token_observations: None,
            state: GenerationState::Completed,
            finish_reason: "max_tokens".to_string(),
            metrics: GenerationMetrics {
                prompt_tokens: 3,
                completion_tokens: 1,
                shared_prefix_tokens: 0,
                duration_ms: 1,
                first_token_ms: Some(1),
                tokens_per_second: 1.0,
                cache: GenerationCacheMetrics::default(),
            },
            real_engine_invoked: true,
            fake_fixture: false,
            transport: NativeTransport::InProcess,
        };
        let output = ControlledGenerationBatchOutput::new(
            request,
            vec![
                ControlledGenerationCaseOutput::new("case-0".to_string(), generation, Vec::new())
                    .expect("case output"),
            ],
            declaration,
        )
        .expect("batch output");
        let worker_identity = Arc::new(WorkerIdentity);
        let verified = ControlledGenerationCompletion::verified(
            output,
            ControlledGenerationEvidence {
                model_fingerprint: fingerprint,
                request_sha256: "c".repeat(64),
                output_sha256: "d".repeat(64),
                event_stream_sha256: "e".repeat(64),
                runtime_operation_ledger_sha256: runtime.ledger_sha256,
                ledger_sha256: "f".repeat(64),
                owner_call_sequence: 3,
                runtime_cost,
                terminal_sampled_token_ids: vec![None],
                events: Vec::new(),
                worker_identity: Arc::clone(&worker_identity),
            },
        )
        .into_verified()
        .expect("private evidence mints authority");
        let joined = JoinedNativeModel {
            model_id: "model".to_string(),
            worker_identity,
        };
        let other = JoinedNativeModel {
            model_id: "model".to_string(),
            worker_identity: Arc::new(WorkerIdentity),
        };
        assert!(verified.belongs_to_joined_model(&joined));
        assert!(!verified.belongs_to_joined_model(&other));
        assert_eq!(verified.owner_call_sequence(), 3);
        assert_eq!(verified.runtime_cost(), runtime_cost);
    }

    #[test]
    #[ignore = "requires MOM_LLAMA_MODEL_PATH and a real small local GGUF"]
    fn real_small_gguf_controlled_generation_proves_baseline_cfg_constraints_and_samplers()
    -> Result<(), Box<dyn std::error::Error>> {
        let _guard = CONTROL_REAL_MODEL_TEST_LOCK
            .lock()
            .expect("real-model lock");
        let model_path = std::env::var("MOM_LLAMA_MODEL_PATH")?;
        let mut config = NativeModelConfig::local(std::path::PathBuf::from(model_path));
        config.device = NativeDevice::Auto;
        config.context_tokens = 512;
        config.batch_tokens = 128;
        config.max_sequences = 4;
        let owner = NativeModelOwner::load(config)?;
        let handle = owner.handle();
        let identity = handle.controlled_model_identity("writer")?;
        let prepared = handle.prepare_input(GenerationInput::Completion {
            prompts: vec![CompletionPrompt::Text {
                text: "The rain stopped. Mara listened".to_string(),
                special_tokens: SpecialTokenPolicy::AddBosParseSpecial,
            }],
        })?;
        let prompt = prepared[0].token_ids.clone();
        let unconditional = handle.prepare_input(GenerationInput::Completion {
            prompts: vec![CompletionPrompt::Text {
                text: "The story continued".to_string(),
                special_tokens: SpecialTokenPolicy::AddBosParseSpecial,
            }],
        })?[0]
            .token_ids
            .clone();

        let sampling = SamplingConfig {
            seed: 41,
            temperature: 0.8,
            max_tokens: 4,
            ..SamplingConfig::default()
        };
        let baseline_program = ControlProgram::new(
            identity.clone(),
            Vec::new(),
            None,
            Vec::new(),
            ExtendedSamplerProgram::default(),
            TerminalSelector::Distribution,
            DistributionObservationPolicy::default(),
            Vec::new(),
        )?;
        let baseline_request = ControlledGenerationBatchRequest::new(
            "real-controlled-baseline".to_string(),
            vec![llama_native_types::ControlledGenerationCase::new(
                "case-0".to_string(),
                ExactTokenPrompt::new(prompt.clone())?,
                None,
                sampling.clone(),
            )?],
            baseline_program,
        )?;
        let verified = handle
            .generate_controlled(ControlledGenerationSubmission::new(baseline_request, None)?)?
            .wait_verified()?;
        assert!(
            verified
                .output()
                .cases()
                .iter()
                .all(|output| output.generation().real_engine_invoked)
        );
        let persisted = serde_json::to_string(verified.output())?;
        let restored: ControlledGenerationBatchOutput = match serde_json::from_str(&persisted) {
            Ok(restored) => restored,
            Err(error) => {
                let value: serde_json::Value = serde_json::from_str(&persisted)?;
                let request: ControlledGenerationBatchRequest =
                    serde_json::from_value(value["request"].clone())?;
                let cases: Vec<ControlledGenerationCaseOutput> =
                    serde_json::from_value(value["cases"].clone())?;
                let cases_equal = cases == verified.output().cases();
                let stored: llama_native_types::UnverifiedAppliedControlReceipt =
                    serde_json::from_value(value["receipt"].clone())?;
                let rebuilt = ControlledGenerationBatchOutput::new(
                    request,
                    cases,
                    stored.backend_declaration().clone(),
                )?;
                panic!(
                    "controlled JSON round-trip failed ({error}); request={} control={} floats={} output={} backend={} cases_equal={cases_equal}",
                    rebuilt.receipt().request_sha256() == stored.request_sha256(),
                    rebuilt.receipt().requested_control_sha256()
                        == stored.requested_control_sha256(),
                    rebuilt.receipt().exact_float_bits_sha256() == stored.exact_float_bits_sha256(),
                    rebuilt.receipt().output_sha256() == stored.output_sha256(),
                    rebuilt.receipt().backend_declaration_sha256()
                        == stored.backend_declaration_sha256(),
                );
            }
        };
        assert_eq!(restored, *verified.output());

        let legacy = handle
            .generate_batch(GenerationBatchRequest {
                request_id: "real-legacy-baseline".to_string(),
                model_id: identity.fingerprint().model_id.clone(),
                cases: vec![GenerationCase {
                    case_id: "case-0".to_string(),
                    input: GenerationInput::Completion {
                        prompts: vec![CompletionPrompt::Tokens {
                            token_ids: prompt.clone(),
                        }],
                    },
                    sampling: sampling.clone(),
                    cached_prefix: None,
                }],
            })?
            .wait()?;
        assert_eq!(
            legacy[0].generated_token_ids,
            verified.output().cases()[0]
                .generation()
                .generated_token_ids
        );
        assert_eq!(
            legacy[0].text,
            verified.output().cases()[0].generation().text
        );

        let observations = DistributionObservationPolicy::new(
            ProbabilityStageSet::new(vec![
                ProbabilityStage::RawModel,
                ProbabilityStage::PostConstraint,
                ProbabilityStage::PostGuidance,
                ProbabilityStage::PostSampler,
            ])?,
            DistributionValueKindSet::new(vec![
                DistributionValueKind::Probability,
                DistributionValueKind::LogProbability,
            ])?,
            true,
            4,
        )?;
        let extended = ExtendedSamplerProgram::new(vec![
            ExtendedSampler::SparseLogitBias {
                biases: SparseLogitBias::new(vec![TokenLogitBias {
                    token_id: prompt[0],
                    bias: 0.25,
                }])?,
            },
            ExtendedSampler::EtaCutoff {
                cutoff: EtaCutoff::new(0.001)?,
            },
            ExtendedSampler::TopNSigma {
                sigma: TopNSigma::new(0.1)?,
            },
        ])?;
        let active_program = ControlProgram::new(
            identity.clone(),
            Vec::new(),
            None,
            vec![
                GuidanceControl::SameModelCfg {
                    scale: ExactF32::new(1.25)?,
                    rescale: Some(ExactF32::new(0.2)?),
                },
                GuidanceControl::PowerSampling {
                    exponent: ExactF32::new(1.1)?,
                },
            ],
            extended,
            TerminalSelector::Distribution,
            observations,
            Vec::new(),
        )?;
        let mut second_prompt = prompt.clone();
        second_prompt.push(*prompt.last().expect("prepared prompt is non-empty"));
        let mut second_unconditional = unconditional.clone();
        second_unconditional.push(
            *unconditional
                .last()
                .expect("prepared unconditional prompt is non-empty"),
        );
        let active = ControlledGenerationBatchRequest::new(
            "real-controlled-active".to_string(),
            vec![
                llama_native_types::ControlledGenerationCase::new(
                    "case-0".to_string(),
                    ExactTokenPrompt::new(prompt.clone())?,
                    Some(ExactTokenPrompt::new(unconditional.clone())?),
                    SamplingConfig {
                        max_tokens: 2,
                        ..sampling.clone()
                    },
                )?,
                llama_native_types::ControlledGenerationCase::new(
                    "case-1".to_string(),
                    ExactTokenPrompt::new(second_prompt)?,
                    Some(ExactTokenPrompt::new(second_unconditional)?),
                    SamplingConfig {
                        seed: sampling.seed.wrapping_add(1),
                        max_tokens: 2,
                        ..sampling.clone()
                    },
                )?,
            ],
            active_program,
        )?;
        let active = handle
            .generate_controlled(ControlledGenerationSubmission::new(active, None)?)?
            .wait_verified()?;
        assert_eq!(active.output().cases().len(), 2);
        assert!(
            active
                .output()
                .cases()
                .iter()
                .all(|case| !case.distribution_observations().is_empty())
        );
        assert_eq!(active.output().request().cost().total_sequence_slots(), 4);
        assert!(active.runtime_cost().conditional_shared_prefix_tokens() > 0);
        assert!(active.runtime_cost().unconditional_shared_prefix_tokens() > 0);
        assert!(
            active.runtime_cost().physical_prompt_evaluations()
                < active.output().request().cost().exact_prompt_tokens()
        );
        assert!(active.output().cases().iter().all(|case| {
            case.generation().metrics.shared_prefix_tokens
                == active.runtime_cost().conditional_shared_prefix_tokens()
        }));

        let schema = r#"{"type":"string"}"#;
        let reference = ConstraintArtifactReference::new(
            "real-schema".to_string(),
            format!("{:x}", Sha256::digest(schema.as_bytes())),
            schema.len() as u32,
        )?;
        let constrained_program = ControlProgram::new(
            identity.clone(),
            Vec::new(),
            Some(StructuredConstraint::JsonSchema { reference }),
            Vec::new(),
            ExtendedSamplerProgram::default(),
            TerminalSelector::Greedy,
            DistributionObservationPolicy::default(),
            Vec::new(),
        )?;
        let constrained = ControlledGenerationBatchRequest::new(
            "real-controlled-json".to_string(),
            vec![llama_native_types::ControlledGenerationCase::new(
                "case-0".to_string(),
                ExactTokenPrompt::new(prompt.clone())?,
                None,
                SamplingConfig {
                    max_tokens: 2,
                    ..sampling.clone()
                },
            )?],
            constrained_program,
        )?;
        let constrained = handle
            .generate_controlled(ControlledGenerationSubmission::new(
                constrained,
                Some(schema.to_string()),
            )?)?
            .wait_verified()?;
        assert!(
            constrained.output().cases()[0]
                .generation()
                .real_engine_invoked
        );

        let gbnf = r#"root ::= "A""#;
        let reference = ConstraintArtifactReference::new(
            "real-gbnf".to_string(),
            format!("{:x}", Sha256::digest(gbnf.as_bytes())),
            gbnf.len() as u32,
        )?;
        let gbnf_program = ControlProgram::new(
            identity.clone(),
            Vec::new(),
            Some(StructuredConstraint::Gbnf { reference }),
            Vec::new(),
            ExtendedSamplerProgram::default(),
            TerminalSelector::Greedy,
            DistributionObservationPolicy::default(),
            Vec::new(),
        )?;
        let gbnf_request = ControlledGenerationBatchRequest::new(
            "real-controlled-gbnf".to_string(),
            vec![llama_native_types::ControlledGenerationCase::new(
                "case-0".to_string(),
                ExactTokenPrompt::new(prompt.clone())?,
                None,
                SamplingConfig {
                    max_tokens: 2,
                    ..sampling.clone()
                },
            )?],
            gbnf_program,
        )?;
        let gbnf_output = handle
            .generate_controlled(ControlledGenerationSubmission::new(
                gbnf_request,
                Some(gbnf.to_string()),
            )?)?
            .wait_verified()?;
        assert_eq!(gbnf_output.output().cases()[0].generation().text, "A");

        let cancelled_program = ControlProgram::new(
            identity.clone(),
            Vec::new(),
            None,
            Vec::new(),
            ExtendedSamplerProgram::default(),
            TerminalSelector::Distribution,
            DistributionObservationPolicy::default(),
            Vec::new(),
        )?;
        let cancelled_request = ControlledGenerationBatchRequest::new(
            "real-controlled-cancelled".to_string(),
            vec![llama_native_types::ControlledGenerationCase::new(
                "case-0".to_string(),
                ExactTokenPrompt::new(prompt.clone())?,
                None,
                sampling.clone(),
            )?],
            cancelled_program,
        )?;
        let cancelled_ticket = handle.generate_controlled(ControlledGenerationSubmission::new(
            cancelled_request,
            None,
        )?)?;
        assert_eq!(cancelled_ticket.cancel_all(), 1);
        assert!(
            cancelled_ticket.wait_verified().is_err(),
            "cancelled controlled work cannot mint strict authority"
        );

        for (request_id, selector, sampler) in [
            (
                "real-mirostat-v1",
                TerminalSelector::MirostatV1,
                ExtendedSampler::MirostatV1 {
                    config: MirostatV1Config::new(5.0, 0.1, 100)?,
                },
            ),
            (
                "real-mirostat-v2",
                TerminalSelector::MirostatV2,
                ExtendedSampler::MirostatV2 {
                    config: MirostatV2Config::new(5.0, 0.1)?,
                },
            ),
        ] {
            let program = ControlProgram::new(
                identity.clone(),
                Vec::new(),
                None,
                Vec::new(),
                ExtendedSamplerProgram::new(vec![sampler])?,
                selector,
                DistributionObservationPolicy::default(),
                Vec::new(),
            )?;
            let request = ControlledGenerationBatchRequest::new(
                request_id.to_string(),
                vec![llama_native_types::ControlledGenerationCase::new(
                    "case-0".to_string(),
                    ExactTokenPrompt::new(prompt.clone())?,
                    None,
                    SamplingConfig {
                        max_tokens: 1,
                        ..sampling.clone()
                    },
                )?],
                program,
            )?;
            let output = handle
                .generate_controlled(ControlledGenerationSubmission::new(request, None)?)?
                .wait_verified()?;
            assert!(output.output().cases()[0].generation().real_engine_invoked);
        }
        let joined = owner.shutdown_joined()?;
        assert!(verified.belongs_to_joined_model(&joined));
        Ok(())
    }
}
