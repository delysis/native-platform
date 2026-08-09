use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded};
use llama_native_types::{
    CompletionPrompt, GenerationBatchRequest, GenerationCase, GenerationEvent as NativeEvent,
    GenerationEventKind as NativeEventKind, GenerationOutput, GenerationState, NativeError,
    NativeTransport, SamplingConfig, SpecialTokenPolicy,
};
use loom_types::{
    ArtifactId, BlobId, BranchCandidate, BranchId, ByteRange, CandidateId, GeneratedSpan,
    GenerationEvent, GenerationEventKind, GenerationMetrics, GenerationProvenance, GenerationRunId,
    GenerationStart, GenerationTerminalEvent, GenerationTerminalStatus, InferenceEvidenceKind,
    LoomEvent, ModelEnvironment, PromptMode, PromptRecipe, TokenTrace, now_unix_ms,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::{
    LocalModelProfile, ModelInspectionError, VerifiedModelDescriptor, verify_model_inspection,
};
use crate::runtime::{
    BatchExecution, BatchRuntime, HostShutdownReceipt, ModelRelease, NativeHostRuntime,
    RuntimeEvidenceClass,
};

pub const DEFAULT_EVENT_CAPACITY: usize = 256;
pub const MAX_EVENT_CAPACITY: usize = 65_536;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ContinuationCase {
    pub generation: GenerationStart,
    pub sampling: SamplingConfig,
}

impl ContinuationCase {
    pub fn bind_sampling(
        mut generation: GenerationStart,
        sampling: SamplingConfig,
    ) -> Result<Self, serde_json::Error> {
        generation.seed = u64::from(sampling.seed);
        generation.sampling = serde_json::to_value(&sampling)?;
        Ok(Self {
            generation,
            sampling,
        })
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExactContinuationRequest {
    pub request_id: String,
    pub model: LocalModelProfile,
    /// The exact UTF-8 bytes submitted as the completion prompt. The adapter
    /// appends no system, chat-template, instruction, or control text.
    pub exact_manuscript_prefix: String,
    pub prompt_recipe: PromptRecipe,
    pub cases: Vec<ContinuationCase>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CandidateProvenanceRecord {
    pub generation: GenerationStart,
    pub candidate: BranchCandidate,
    pub generated_span: GeneratedSpan,
    pub token_trace: TokenTrace,
    pub terminal: GenerationTerminalEvent,
    pub output_text: String,
    pub finish_reason: String,
    /// Exact serialized native events whose digest is stored in `token_trace`.
    pub raw_event_stream_bytes: Vec<u8>,
    /// Exact serialized native output receipt whose digest is in provenance.
    pub backend_receipt_bytes: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExactContinuationResult {
    pub request_id: String,
    pub exact_prompt_blob_id: BlobId,
    pub exact_manuscript_prefix: String,
    pub model_environment: ModelEnvironment,
    pub model: VerifiedModelDescriptor,
    pub candidates: Vec<CandidateProvenanceRecord>,
}

#[derive(Debug, Error)]
pub enum LlamaBackendError {
    #[error("invalid exact-continuation request: {0}")]
    InvalidRequest(String),
    #[error(transparent)]
    ModelInspection(#[from] ModelInspectionError),
    #[error("native generation failed: {0}")]
    Native(#[from] NativeError),
    #[error("native output violated the ordered batch contract: {0}")]
    OutputContract(String),
    #[error("failed to serialize provenance: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("invalid native fingerprint: {0}")]
    Fingerprint(#[from] loom_types::HashIdParseError),
    #[error("failed to start the Loom event forwarder: {0}")]
    WorkerSpawn(#[source] std::io::Error),
    #[error("the Loom event forwarder panicked before it could be joined")]
    WorkerPanicked,
    #[error("generation result channel disconnected")]
    ResultDisconnected,
    #[error("generation did not finish before the requested timeout")]
    ResultTimeout,
}

#[derive(Debug)]
pub struct LlamaBackend {
    runtime: Arc<dyn BatchRuntime>,
    event_capacity: usize,
}

impl Default for LlamaBackend {
    fn default() -> Self {
        Self::with_runtime(
            Arc::new(NativeHostRuntime::default()),
            DEFAULT_EVENT_CAPACITY,
        )
        .expect("the built-in event capacity is valid")
    }
}

impl LlamaBackend {
    pub fn with_runtime(
        runtime: Arc<dyn BatchRuntime>,
        event_capacity: usize,
    ) -> Result<Self, LlamaBackendError> {
        if event_capacity == 0 || event_capacity > MAX_EVENT_CAPACITY {
            return Err(LlamaBackendError::InvalidRequest(format!(
                "event capacity must be in 1..={MAX_EVENT_CAPACITY}"
            )));
        }
        Ok(Self {
            runtime,
            event_capacity,
        })
    }

    pub fn inspect_model(
        &self,
        profile: &LocalModelProfile,
    ) -> Result<VerifiedModelDescriptor, LlamaBackendError> {
        let inspection = self.runtime.inspect_model(profile)?;
        verify_model_inspection(profile, inspection).map_err(Into::into)
    }

    /// Releases native resident state for a model that is no longer selected.
    /// Callers must ensure no active generation still references the profile.
    pub fn release_model(
        &self,
        profile: &LocalModelProfile,
    ) -> Result<ModelRelease, LlamaBackendError> {
        self.runtime.release_model(profile).map_err(Into::into)
    }

    /// Releases every runtime-owned model and returns only after the native
    /// host has proved that no resident slot remains.
    pub fn shutdown_and_verify_empty(&self) -> Result<HostShutdownReceipt, LlamaBackendError> {
        self.runtime.shutdown_and_verify_empty().map_err(Into::into)
    }

    pub fn start_exact_continuation(
        &self,
        request: ExactContinuationRequest,
    ) -> Result<LlamaGenerationHandle, LlamaBackendError> {
        let model = self.inspect_model(&request.model)?;
        validate_request(&request, &model, self.event_capacity)?;
        let exact_prompt_blob_id = BlobId::digest(request.exact_manuscript_prefix.as_bytes());
        let model_environment = model_environment_from_verified(&model)?;
        let native_request = build_native_request(&request);
        let execution = self.runtime.start_batch(&request.model, native_request)?;
        let identities = request
            .cases
            .iter()
            .enumerate()
            .map(|(input_index, case)| CaseIdentity {
                case_id: case.generation.branch_id.to_string(),
                run_id: case.generation.run_id,
                branch_id: case.generation.branch_id,
                input_index,
            })
            .collect::<Vec<_>>();
        let (event_tx, event_rx) = bounded(self.event_capacity);
        let (result_tx, result_rx) = bounded(1);
        let events = Arc::new(EventStream::new(event_tx, self.event_capacity, &identities));
        for identity in &identities {
            events.emit_generation(identity, GenerationEventKind::Queued);
        }

        let worker_execution = Arc::clone(&execution);
        let worker_events = Arc::clone(&events);
        let worker_identities = identities.clone();
        let runtime_evidence = self.runtime.evidence_class();
        let worker_request = request.clone();
        let worker_model = model.clone();
        let worker_environment = model_environment.clone();
        let worker = std::thread::Builder::new()
            .name("loom-llama-event-forwarder".to_string())
            .spawn(move || {
                run_generation_worker(
                    worker_execution.as_ref(),
                    worker_events.as_ref(),
                    &worker_identities,
                    runtime_evidence,
                    worker_request,
                    exact_prompt_blob_id,
                    worker_model,
                    worker_environment,
                    &result_tx,
                );
            })
            .map_err(|error| {
                for identity in &identities {
                    let _ = execution.cancel_case(&identity.case_id);
                }
                LlamaBackendError::WorkerSpawn(error)
            })?;

        Ok(LlamaGenerationHandle {
            request_id: request.request_id,
            identities,
            execution,
            events,
            event_rx,
            result_rx,
            worker: Mutex::new(Some(worker)),
        })
    }
}

#[derive(Debug)]
pub struct LlamaGenerationHandle {
    request_id: String,
    identities: Vec<CaseIdentity>,
    execution: Arc<dyn BatchExecution>,
    events: Arc<EventStream>,
    event_rx: Receiver<LoomEvent>,
    result_rx: Receiver<Result<ExactContinuationResult, LlamaBackendError>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl LlamaGenerationHandle {
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    pub fn cancel_branch(&self, branch_id: BranchId) -> bool {
        let Some(identity) = self
            .identities
            .iter()
            .find(|identity| identity.branch_id == branch_id)
        else {
            return false;
        };
        if !self.execution.cancel_case(&identity.case_id) {
            return false;
        }
        self.events
            .emit_generation(identity, GenerationEventKind::CancellationRequested);
        true
    }

    pub fn cancel_run(&self, run_id: GenerationRunId) -> bool {
        let Some(identity) = self
            .identities
            .iter()
            .find(|identity| identity.run_id == run_id)
        else {
            return false;
        };
        self.cancel_branch(identity.branch_id)
    }

    pub fn receive_event_timeout(
        &self,
        timeout: Duration,
    ) -> Result<Option<LoomEvent>, LlamaBackendError> {
        match self.event_rx.recv_timeout(timeout) {
            Ok(event) => Ok(Some(event)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(LlamaBackendError::ResultDisconnected),
        }
    }

    pub fn wait(self) -> Result<ExactContinuationResult, LlamaBackendError> {
        let result = self
            .result_rx
            .recv()
            .map_err(|_| LlamaBackendError::ResultDisconnected);
        self.join_worker()?;
        result?
    }

    pub fn wait_timeout(
        &self,
        timeout: Duration,
    ) -> Result<ExactContinuationResult, LlamaBackendError> {
        match self.result_rx.recv_timeout(timeout) {
            Ok(result) => {
                self.join_worker()?;
                result
            }
            Err(RecvTimeoutError::Timeout) => Err(LlamaBackendError::ResultTimeout),
            Err(RecvTimeoutError::Disconnected) => {
                self.join_worker()?;
                Err(LlamaBackendError::ResultDisconnected)
            }
        }
    }

    fn join_worker(&self) -> Result<(), LlamaBackendError> {
        let worker = self
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        worker.map_or(Ok(()), |worker| {
            worker.join().map_err(|_| LlamaBackendError::WorkerPanicked)
        })
    }
}

impl Drop for LlamaGenerationHandle {
    fn drop(&mut self) {
        let worker_is_live = self
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some();
        if worker_is_live {
            for identity in &self.identities {
                let _ = self.execution.cancel_case(&identity.case_id);
            }
        }
        let _ = self.join_worker();
    }
}

#[derive(Clone, Debug)]
struct CaseIdentity {
    case_id: String,
    run_id: GenerationRunId,
    branch_id: BranchId,
    input_index: usize,
}

#[derive(Debug)]
struct EventStream {
    sender: Sender<LoomEvent>,
    capacity: usize,
    terminal_reserve: usize,
    emission: Mutex<()>,
    sequences: Mutex<BTreeMap<BranchId, u64>>,
    terminals: Mutex<BTreeMap<BranchId, GenerationTerminalEvent>>,
}

impl EventStream {
    fn new(sender: Sender<LoomEvent>, capacity: usize, identities: &[CaseIdentity]) -> Self {
        Self {
            sender,
            capacity,
            terminal_reserve: identities.len(),
            emission: Mutex::new(()),
            sequences: Mutex::new(
                identities
                    .iter()
                    .map(|identity| (identity.branch_id, 0))
                    .collect(),
            ),
            terminals: Mutex::new(BTreeMap::new()),
        }
    }

    fn emit_generation(&self, identity: &CaseIdentity, kind: GenerationEventKind) {
        let _emission = self
            .emission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.is_terminated(identity.branch_id)
            || self.sender.len() >= self.capacity.saturating_sub(self.terminal_reserve)
        {
            return;
        }
        let sequence = self.next_sequence(identity.branch_id);
        let _ = self.sender.try_send(LoomEvent::Generation(GenerationEvent {
            event_id: loom_types::GenerationEventId::new(),
            run_id: identity.run_id,
            branch_id: identity.branch_id,
            sequence,
            kind,
            occurred_at_ms: now_unix_ms(),
        }));
    }

    fn emit_terminal(
        &self,
        identity: &CaseIdentity,
        status: GenerationTerminalStatus,
        candidate_id: Option<CandidateId>,
        error: Option<String>,
    ) -> GenerationTerminalEvent {
        let _emission = self
            .emission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut terminals = self
            .terminals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(terminal) = terminals.get(&identity.branch_id) {
            return terminal.clone();
        }
        let terminal = GenerationTerminalEvent {
            event_id: loom_types::GenerationEventId::new(),
            run_id: identity.run_id,
            branch_id: identity.branch_id,
            sequence: self.next_sequence(identity.branch_id),
            status,
            candidate_id,
            error,
            occurred_at_ms: now_unix_ms(),
        };
        terminals.insert(identity.branch_id, terminal.clone());
        let _ = self
            .sender
            .try_send(LoomEvent::GenerationTerminal(terminal.clone()));
        terminal
    }

    fn is_terminated(&self, branch_id: BranchId) -> bool {
        self.terminals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&branch_id)
    }

    fn next_sequence(&self, branch_id: BranchId) -> u64 {
        let mut sequences = self
            .sequences
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let sequence = sequences.entry(branch_id).or_default();
        let current = *sequence;
        *sequence = sequence.saturating_add(1);
        current
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn run_generation_worker(
    execution: &dyn BatchExecution,
    events: &EventStream,
    identities: &[CaseIdentity],
    runtime_evidence: RuntimeEvidenceClass,
    request: ExactContinuationRequest,
    exact_prompt_blob_id: BlobId,
    model: VerifiedModelDescriptor,
    model_environment: ModelEnvironment,
    result_tx: &Sender<Result<ExactContinuationResult, LlamaBackendError>>,
) {
    let identity_by_case = identities
        .iter()
        .map(|identity| (identity.case_id.clone(), identity.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut raw_events = identities
        .iter()
        .map(|identity| (identity.case_id.clone(), Vec::<NativeEvent>::new()))
        .collect::<BTreeMap<_, _>>();

    let result = 'worker: loop {
        match execution.receive_event_timeout(Duration::from_millis(5)) {
            Ok(Some(event)) => {
                if let Err(error) = record_and_forward_native_event(
                    events,
                    &request.request_id,
                    &identity_by_case,
                    &mut raw_events,
                    event,
                ) {
                    break Err(error);
                }
            }
            Ok(None) => {}
            Err(error) => break Err(LlamaBackendError::Native(error)),
        }
        match execution.try_result() {
            Ok(Some(outputs)) => {
                loop {
                    match execution.receive_event_timeout(Duration::ZERO) {
                        Ok(Some(event)) => {
                            if let Err(error) = record_and_forward_native_event(
                                events,
                                &request.request_id,
                                &identity_by_case,
                                &mut raw_events,
                                event,
                            ) {
                                break 'worker Err(error);
                            }
                        }
                        Ok(None) => break,
                        Err(error) => break 'worker Err(LlamaBackendError::Native(error)),
                    }
                }
                break build_result(
                    &request,
                    identities,
                    runtime_evidence,
                    exact_prompt_blob_id,
                    &model,
                    outputs,
                    raw_events,
                );
            }
            Ok(None) => {}
            Err(error) => break Err(LlamaBackendError::Native(error)),
        }
    };

    match result {
        Ok(materials) => {
            let mut candidates = Vec::with_capacity(materials.len());
            for (identity, material) in identities.iter().zip(materials) {
                events.emit_generation(
                    identity,
                    GenerationEventKind::CandidateReady {
                        candidate_id: material.candidate.candidate_id,
                        generated_span_artifact_id: material.candidate.generated_span_artifact_id,
                    },
                );
                let terminal = events.emit_terminal(
                    identity,
                    material.status,
                    Some(material.candidate.candidate_id),
                    None,
                );
                candidates.push(CandidateProvenanceRecord {
                    generation: material.generation,
                    candidate: material.candidate,
                    generated_span: material.generated_span,
                    token_trace: material.token_trace,
                    terminal,
                    output_text: material.output_text,
                    finish_reason: material.finish_reason,
                    raw_event_stream_bytes: material.raw_event_stream_bytes,
                    backend_receipt_bytes: material.backend_receipt_bytes,
                });
            }
            let _ = result_tx.send(Ok(ExactContinuationResult {
                request_id: request.request_id,
                exact_prompt_blob_id,
                exact_manuscript_prefix: request.exact_manuscript_prefix,
                model_environment,
                model,
                candidates,
            }));
        }
        Err(error) => {
            let message = error.to_string();
            for identity in identities {
                events.emit_terminal(
                    identity,
                    GenerationTerminalStatus::Failed,
                    None,
                    Some(message.clone()),
                );
            }
            let _ = result_tx.send(Err(error));
        }
    }
}

fn record_and_forward_native_event(
    events: &EventStream,
    request_id: &str,
    identities: &BTreeMap<String, CaseIdentity>,
    raw_events: &mut BTreeMap<String, Vec<NativeEvent>>,
    event: NativeEvent,
) -> Result<(), LlamaBackendError> {
    let identity = identities.get(&event.branch_id).ok_or_else(|| {
        LlamaBackendError::OutputContract(format!(
            "received event for unknown case `{}`",
            event.branch_id
        ))
    })?;
    if event.request_id != request_id || event.input_index != identity.input_index {
        return Err(LlamaBackendError::OutputContract(format!(
            "case `{}` event identity/order did not match the native request",
            identity.case_id
        )));
    }
    let case_events = raw_events.entry(event.branch_id.clone()).or_default();
    if case_events
        .last()
        .is_some_and(|previous| previous.event_index >= event.event_index)
    {
        return Err(LlamaBackendError::OutputContract(format!(
            "case `{}` event indices were not strictly increasing",
            identity.case_id
        )));
    }
    case_events.push(event.clone());
    match event.event {
        NativeEventKind::State {
            state:
                GenerationState::Queued
                | GenerationState::Completed
                | GenerationState::Cancelled
                | GenerationState::Failed,
        } => {}
        NativeEventKind::State {
            state: GenerationState::Prefilling,
        } => events.emit_generation(identity, GenerationEventKind::Prefilling),
        NativeEventKind::State {
            state: GenerationState::Generating,
        } => events.emit_generation(identity, GenerationEventKind::Generating),
        NativeEventKind::Delta { text } => {
            events.emit_generation(identity, GenerationEventKind::TextDelta { text });
        }
        NativeEventKind::Warning { code, message } => {
            events.emit_generation(identity, GenerationEventKind::Warning { code, message });
        }
    }
    Ok(())
}

#[derive(Debug)]
struct CandidateMaterial {
    generation: GenerationStart,
    candidate: BranchCandidate,
    generated_span: GeneratedSpan,
    token_trace: TokenTrace,
    status: GenerationTerminalStatus,
    output_text: String,
    finish_reason: String,
    raw_event_stream_bytes: Vec<u8>,
    backend_receipt_bytes: Vec<u8>,
}

#[derive(Serialize)]
struct BackendReceipt<'a> {
    exact_prompt_blob_id: BlobId,
    model_environment_id: loom_types::ModelEnvironmentId,
    output: &'a GenerationOutput,
}

#[derive(Deserialize)]
struct OwnedBackendReceipt {
    exact_prompt_blob_id: BlobId,
    model_environment_id: loom_types::ModelEnvironmentId,
    output: GenerationOutput,
}

/// Verifies that preserved native receipt bytes describe this exact Loom run.
///
/// The receipt is deliberately checked again at the product boundary. A
/// digest proves that bytes were preserved; these comparisons prove that the
/// preserved bytes bind the prompt, model environment, branch, output, and
/// token evidence that the store is about to attribute.
pub fn validate_candidate_receipt_binding(
    record: &CandidateProvenanceRecord,
    expected_request_id: &str,
    expected_prompt_blob_id: BlobId,
    expected_model_environment_id: loom_types::ModelEnvironmentId,
    expected_local_model_id: &str,
    expected_input_index: usize,
) -> Result<(), LlamaBackendError> {
    let receipt: OwnedBackendReceipt = serde_json::from_slice(&record.backend_receipt_bytes)?;
    let output = &receipt.output;
    let output_token_ids = output
        .generated_token_ids
        .iter()
        .copied()
        .map(|token_id| {
            u32::try_from(token_id).map_err(|_| {
                LlamaBackendError::OutputContract(format!(
                    "preserved receipt returned negative token ID {token_id}"
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let receipt_status = match output.state {
        GenerationState::Completed => GenerationTerminalStatus::Completed,
        GenerationState::Cancelled => GenerationTerminalStatus::Cancelled,
        GenerationState::Failed => GenerationTerminalStatus::Failed,
        state @ (GenerationState::Queued
        | GenerationState::Prefilling
        | GenerationState::Generating) => {
            return Err(LlamaBackendError::OutputContract(format!(
                "preserved receipt contains nonterminal output state {state:?}"
            )));
        }
    };
    let output_blob_id = BlobId::digest(output.text.as_bytes());
    let identities_match = receipt.exact_prompt_blob_id == expected_prompt_blob_id
        && receipt.model_environment_id == expected_model_environment_id
        && output.request_id == expected_request_id
        && output.branch_id == record.generation.branch_id.to_string()
        && output.input_index == expected_input_index
        && output.model_id == expected_local_model_id
        && output.text == record.output_text
        && output.finish_reason == record.finish_reason
        && output_token_ids == record.token_trace.generated_token_ids
        && receipt_status == record.terminal.status
        && record.candidate.run_id == record.generation.run_id
        && record.candidate.branch_id == record.generation.branch_id
        && record.candidate.output_blob_id == output_blob_id
        && record.generated_span.candidate_id == record.candidate.candidate_id
        && record.generated_span.run_id == record.generation.run_id
        && record.generated_span.branch_id == record.generation.branch_id
        && record.generated_span.output_blob_id == output_blob_id
        && record.terminal.run_id == record.generation.run_id
        && record.terminal.branch_id == record.generation.branch_id;
    if !identities_match {
        return Err(LlamaBackendError::OutputContract(
            "preserved backend receipt is not bound to the expected prompt, model, run, and output"
                .to_string(),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn build_result(
    request: &ExactContinuationRequest,
    identities: &[CaseIdentity],
    runtime_evidence: RuntimeEvidenceClass,
    exact_prompt_blob_id: BlobId,
    model: &VerifiedModelDescriptor,
    outputs: Vec<GenerationOutput>,
    mut raw_events: BTreeMap<String, Vec<NativeEvent>>,
) -> Result<Vec<CandidateMaterial>, LlamaBackendError> {
    if outputs.len() != identities.len() {
        return Err(LlamaBackendError::OutputContract(format!(
            "received {} outputs for {} cases",
            outputs.len(),
            identities.len()
        )));
    }
    let evidence_kind = match runtime_evidence {
        RuntimeEvidenceClass::RealNative => InferenceEvidenceKind::LiveInference,
        RuntimeEvidenceClass::TestFixture => InferenceEvidenceKind::Fixture,
    };
    outputs
        .into_iter()
        .zip(identities)
        .map(|(output, identity)| {
            let case_events = raw_events.remove(&identity.case_id).unwrap_or_default();
            build_candidate_material(
                request,
                identity,
                runtime_evidence,
                evidence_kind,
                exact_prompt_blob_id,
                model,
                output,
                &case_events,
            )
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn build_candidate_material(
    request: &ExactContinuationRequest,
    identity: &CaseIdentity,
    runtime_evidence: RuntimeEvidenceClass,
    evidence_kind: InferenceEvidenceKind,
    exact_prompt_blob_id: BlobId,
    model: &VerifiedModelDescriptor,
    output: GenerationOutput,
    raw_events: &[NativeEvent],
) -> Result<CandidateMaterial, LlamaBackendError> {
    validate_output(
        &output,
        identity,
        &request.request_id,
        &request.model.model_id,
        runtime_evidence,
    )?;
    if output.token_observations.is_some() {
        return Err(LlamaBackendError::OutputContract(
            "native probability observations cannot be relabeled as Loom logprobs".to_string(),
        ));
    }
    let generated_token_ids = checked_token_ids(&output, identity)?;
    let raw_event_stream_bytes = serde_json::to_vec(&raw_events)?;
    let raw_event_stream_blob_id = BlobId::digest(&raw_event_stream_bytes);
    let backend_receipt_bytes = serde_json::to_vec(&BackendReceipt {
        exact_prompt_blob_id,
        model_environment_id: model.model_environment_id,
        output: &output,
    })?;
    let backend_receipt_blob_id = BlobId::digest(&backend_receipt_bytes);
    let output_blob_id = BlobId::digest(output.text.as_bytes());
    let output_end = u64::try_from(output.text.len()).map_err(|_| {
        LlamaBackendError::OutputContract("output byte length exceeds u64".to_string())
    })?;
    let token_trace_artifact_id = ArtifactId::new();
    let generated_span_artifact_id = ArtifactId::new();
    let candidate_id = CandidateId::new();
    let token_trace = TokenTrace {
        generated_token_ids,
        observations: Vec::new(),
        raw_event_stream_blob_id,
        provenance: Some(GenerationProvenance {
            evidence_kind,
            metrics: generation_metrics(&output)?,
            backend_receipt_blob_id: Some(backend_receipt_blob_id),
            sequence_state_blob_id: None,
        }),
    };
    let generated_span = GeneratedSpan {
        candidate_id,
        run_id: identity.run_id,
        branch_id: identity.branch_id,
        output_blob_id,
        output_byte_range: ByteRange::new(0, output_end).ok_or_else(|| {
            LlamaBackendError::OutputContract("invalid output byte range".to_string())
        })?,
        token_trace_artifact_id,
    };
    let candidate = BranchCandidate {
        candidate_id,
        run_id: identity.run_id,
        branch_id: identity.branch_id,
        generated_span_artifact_id,
        token_trace_artifact_id,
        output_blob_id,
    };
    let status = terminal_status(output.state, identity)?;
    Ok(CandidateMaterial {
        generation: request.cases[identity.input_index].generation.clone(),
        candidate,
        generated_span,
        token_trace,
        status,
        output_text: output.text,
        finish_reason: output.finish_reason,
        raw_event_stream_bytes,
        backend_receipt_bytes,
    })
}

fn checked_token_ids(
    output: &GenerationOutput,
    identity: &CaseIdentity,
) -> Result<Vec<u32>, LlamaBackendError> {
    output
        .generated_token_ids
        .iter()
        .copied()
        .map(|token_id| {
            u32::try_from(token_id).map_err(|_| {
                LlamaBackendError::OutputContract(format!(
                    "case `{}` returned negative token ID {token_id}",
                    identity.case_id
                ))
            })
        })
        .collect()
}

fn terminal_status(
    state: GenerationState,
    identity: &CaseIdentity,
) -> Result<GenerationTerminalStatus, LlamaBackendError> {
    match state {
        GenerationState::Completed => Ok(GenerationTerminalStatus::Completed),
        GenerationState::Cancelled => Ok(GenerationTerminalStatus::Cancelled),
        GenerationState::Failed => Ok(GenerationTerminalStatus::Failed),
        GenerationState::Queued | GenerationState::Prefilling | GenerationState::Generating => {
            Err(LlamaBackendError::OutputContract(format!(
                "case `{}` returned nonterminal output state {state:?}",
                identity.case_id
            )))
        }
    }
}

fn validate_request(
    request: &ExactContinuationRequest,
    model: &VerifiedModelDescriptor,
    event_capacity: usize,
) -> Result<(), LlamaBackendError> {
    if request.request_id.trim().is_empty() {
        return Err(LlamaBackendError::InvalidRequest(
            "request ID cannot be empty".to_string(),
        ));
    }
    if request.exact_manuscript_prefix.is_empty() {
        return Err(LlamaBackendError::InvalidRequest(
            "exact manuscript prefix cannot be empty".to_string(),
        ));
    }
    let prompt_blob_id = BlobId::digest(request.exact_manuscript_prefix.as_bytes());
    if request.prompt_recipe.mode != PromptMode::Completion {
        return Err(LlamaBackendError::InvalidRequest(
            "raw continuation requires PromptMode::Completion".to_string(),
        ));
    }
    if request.prompt_recipe.exact_prompt_blob_id != prompt_blob_id {
        return Err(LlamaBackendError::InvalidRequest(
            "prompt recipe hash does not match the exact manuscript prefix".to_string(),
        ));
    }
    if request.prompt_recipe.exact_prompt_token_ids.is_some() {
        return Err(LlamaBackendError::InvalidRequest(
            "text completion cannot accept an unverified predeclared token prompt".to_string(),
        ));
    }
    if request.cases.is_empty() || request.cases.len() > model.capabilities.max_cases as usize {
        return Err(LlamaBackendError::InvalidRequest(format!(
            "case count must be in 1..={} for this loaded model",
            model.capabilities.max_cases
        )));
    }
    if event_capacity < request.cases.len() {
        return Err(LlamaBackendError::InvalidRequest(format!(
            "event capacity {event_capacity} cannot reserve {} branch terminals",
            request.cases.len()
        )));
    }
    let mut branch_ids = BTreeSet::new();
    let mut run_ids = BTreeSet::new();
    let first = &request.cases[0].generation;
    for case in &request.cases {
        if !branch_ids.insert(case.generation.branch_id) {
            return Err(LlamaBackendError::InvalidRequest(
                "branch IDs must be unique within a batch".to_string(),
            ));
        }
        if !run_ids.insert(case.generation.run_id) {
            return Err(LlamaBackendError::InvalidRequest(
                "generation run IDs must be unique within a batch".to_string(),
            ));
        }
        if case.generation.seed != u64::from(case.sampling.seed) {
            return Err(LlamaBackendError::InvalidRequest(format!(
                "generation seed {} does not match native sampler seed {}",
                case.generation.seed, case.sampling.seed
            )));
        }
        let sampling = serde_json::to_value(&case.sampling)?;
        if case.generation.sampling != sampling {
            return Err(LlamaBackendError::InvalidRequest(
                "GenerationStart sampling must exactly match the native sampler contract"
                    .to_string(),
            ));
        }
        if case.generation.document_id != first.document_id
            || case.generation.source_revision_id != first.source_revision_id
            || case.generation.target_range != first.target_range
            || case.generation.model_environment_artifact_id != first.model_environment_artifact_id
            || case.generation.prompt_recipe_artifact_id != first.prompt_recipe_artifact_id
            || case.generation.context_recipe_artifact_id != first.context_recipe_artifact_id
            || case.generation.authority_policy_artifact_id != first.authority_policy_artifact_id
        {
            return Err(LlamaBackendError::InvalidRequest(
                "all cases in one raw branch family must bind the same source, target, model, prompt, context, and authority artifacts"
                    .to_string(),
            ));
        }
    }
    Ok(())
}

fn build_native_request(request: &ExactContinuationRequest) -> GenerationBatchRequest {
    GenerationBatchRequest {
        request_id: request.request_id.clone(),
        model_id: request.model.model_id.clone(),
        cases: request
            .cases
            .iter()
            .map(|case| GenerationCase {
                case_id: case.generation.branch_id.to_string(),
                input: llama_native_types::GenerationInput::Completion {
                    prompts: vec![CompletionPrompt::Text {
                        text: request.exact_manuscript_prefix.clone(),
                        special_tokens: SpecialTokenPolicy::NoBosParseSpecial,
                    }],
                },
                sampling: case.sampling.clone(),
                cached_prefix: None,
            })
            .collect(),
    }
}

fn validate_output(
    output: &GenerationOutput,
    identity: &CaseIdentity,
    request_id: &str,
    model_id: &str,
    runtime_evidence: RuntimeEvidenceClass,
) -> Result<(), LlamaBackendError> {
    if output.request_id != request_id
        || output.model_id != model_id
        || output.branch_id != identity.case_id
        || output.input_index != identity.input_index
    {
        return Err(LlamaBackendError::OutputContract(format!(
            "case `{}` identity/order did not match native output",
            identity.case_id
        )));
    }
    if output.generated_token_ids.len() != output.metrics.completion_tokens {
        return Err(LlamaBackendError::OutputContract(format!(
            "case `{}` token evidence length did not match completion metrics",
            identity.case_id
        )));
    }
    match runtime_evidence {
        RuntimeEvidenceClass::RealNative
            if output.real_engine_invoked
                && !output.fake_fixture
                && output.transport == NativeTransport::InProcess =>
        {
            Ok(())
        }
        RuntimeEvidenceClass::RealNative => Err(LlamaBackendError::OutputContract(format!(
            "case `{}` did not carry live in-process inference evidence",
            identity.case_id
        ))),
        RuntimeEvidenceClass::TestFixture
            if output.fake_fixture || output.transport == NativeTransport::FakeFixture =>
        {
            Ok(())
        }
        RuntimeEvidenceClass::TestFixture => Err(LlamaBackendError::OutputContract(format!(
            "fixture case `{}` was not explicitly labeled as fixture output",
            identity.case_id
        ))),
    }
}

fn generation_metrics(output: &GenerationOutput) -> Result<GenerationMetrics, LlamaBackendError> {
    let duration_ms = u64::try_from(output.metrics.duration_ms).map_err(|_| {
        LlamaBackendError::OutputContract("duration exceeds u64 milliseconds".to_string())
    })?;
    let first_token_ms = output
        .metrics
        .first_token_ms
        .map(u64::try_from)
        .transpose()
        .map_err(|_| {
            LlamaBackendError::OutputContract(
                "first-token duration exceeds u64 milliseconds".to_string(),
            )
        })?;
    let decode_tokens_per_second = (output.metrics.tokens_per_second.is_finite()
        && output.metrics.tokens_per_second >= 0.0)
        .then_some(output.metrics.tokens_per_second);
    Ok(GenerationMetrics {
        prompt_tokens: Some(to_u64(output.metrics.prompt_tokens)?),
        completion_tokens: Some(to_u64(output.metrics.completion_tokens)?),
        shared_prefix_tokens: Some(to_u64(output.metrics.cache.batch_shared_prefix_tokens)?),
        restored_cache_tokens: Some(to_u64(output.metrics.cache.restored_prefix_tokens)?),
        saved_cache_tokens: None,
        duration_ms: Some(duration_ms),
        first_token_ms,
        decode_tokens_per_second,
    })
}

fn to_u64(value: usize) -> Result<u64, LlamaBackendError> {
    u64::try_from(value).map_err(|_| {
        LlamaBackendError::OutputContract("native token count exceeds u64".to_string())
    })
}

/// Canonical Loom environment artifact payload for a verified native model.
///
/// Callers persist this exact value before starting a branch family; the
/// backend returns the same value with the result so the coordinator can fail
/// closed if model identity changes between inspection and decoding.
pub fn model_environment_from_verified(
    model: &VerifiedModelDescriptor,
) -> Result<ModelEnvironment, LlamaBackendError> {
    Ok(ModelEnvironment {
        environment_id: model.model_environment_id,
        model_identifier: model.stable_model_id.clone(),
        model_fingerprint: BlobId::from_str(&model.model_sha256)?,
        tokenizer_fingerprint: BlobId::from_str(&model.tokenizer_sha256)?,
        backend_identifier: model.build_id.clone(),
        capabilities: serde_json::to_value(&model.capabilities)?,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use crossbeam_channel::RecvTimeoutError;
    use llama_native_types::{
        CacheOperationCapabilities, CapabilityDeclarationStatus, ExactModelCapabilities,
        GenerationBatchCapabilities, GenerationCacheMetrics, GenerationMetrics as NativeMetrics,
        GenerationOutputCapabilities, ModelCapabilities, ModelFingerprint, NativeModelDescriptor,
        PromptForm, PromptInputCapabilities, SamplingParameter,
    };

    use super::*;
    use crate::model::RuntimeModelInspection;
    use crate::runtime::CompleteModelRelease;

    #[derive(Debug)]
    struct FakeExecution {
        event_rx: Receiver<NativeEvent>,
        result: Mutex<Option<Vec<GenerationOutput>>>,
        ready: AtomicBool,
        complete_on_cancel: AtomicBool,
        panic_on_receive: AtomicBool,
        cancelled: Mutex<Vec<String>>,
    }

    impl FakeExecution {
        fn set_ready(&self) {
            self.ready.store(true, Ordering::Release);
        }

        fn cancelled_cases(&self) -> Vec<String> {
            self.cancelled
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    impl BatchExecution for FakeExecution {
        fn cancel_case(&self, case_id: &str) -> bool {
            self.cancelled
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(case_id.to_string());
            if self.complete_on_cancel.load(Ordering::Acquire) {
                self.set_ready();
            }
            true
        }

        fn receive_event_timeout(
            &self,
            timeout: Duration,
        ) -> Result<Option<NativeEvent>, NativeError> {
            assert!(
                !self.panic_on_receive.load(Ordering::Acquire),
                "fixture event worker panic"
            );
            match self.event_rx.recv_timeout(timeout) {
                Ok(event) => Ok(Some(event)),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => Ok(None),
            }
        }

        fn try_result(&self) -> Result<Option<Vec<GenerationOutput>>, NativeError> {
            if !self.ready.load(Ordering::Acquire) {
                return Ok(None);
            }
            Ok(self
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take())
        }
    }

    #[derive(Debug)]
    struct FakeRuntime {
        class: RuntimeEvidenceClass,
        inspection: RuntimeModelInspection,
        execution: Arc<FakeExecution>,
        captured: Mutex<Option<GenerationBatchRequest>>,
        released: AtomicBool,
    }

    impl BatchRuntime for FakeRuntime {
        fn evidence_class(&self) -> RuntimeEvidenceClass {
            self.class
        }

        fn inspect_model(
            &self,
            _profile: &LocalModelProfile,
        ) -> Result<RuntimeModelInspection, NativeError> {
            Ok(self.inspection.clone())
        }

        fn start_batch(
            &self,
            _profile: &LocalModelProfile,
            request: GenerationBatchRequest,
        ) -> Result<Arc<dyn BatchExecution>, NativeError> {
            *self
                .captured
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(request);
            Ok(self.execution.clone())
        }

        fn release_model(&self, _profile: &LocalModelProfile) -> Result<ModelRelease, NativeError> {
            Ok(if self.released.swap(true, Ordering::AcqRel) {
                ModelRelease::AlreadyAbsent
            } else {
                ModelRelease::Released {
                    proof: CompleteModelRelease::from_complete_count(std::num::NonZeroUsize::MIN),
                }
            })
        }
    }

    fn model_profile() -> LocalModelProfile {
        let mut profile = LocalModelProfile::for_gguf("fixture.gguf");
        profile.model_id = "fixture-model".to_string();
        profile.max_parallel_cases = 4;
        profile
    }

    fn model_inspection(profile: &LocalModelProfile) -> RuntimeModelInspection {
        let model_sha256 = "11".repeat(32);
        let tokenizer_sha256 = "22".repeat(32);
        let fingerprint = ModelFingerprint {
            model_id: profile.model_id.clone(),
            model_path: profile.model_path.clone(),
            model_size: 1_000,
            model_sha256: model_sha256.clone(),
            tokenizer_sha256,
            chat_template_sha256: "33".repeat(32),
            multimodal_projector_sha256: None,
            binding_version: "fixture-binding".to_string(),
            build_id: "fixture-build".to_string(),
            backend: "cpu".to_string(),
            context_tokens: 8_192,
            batch_tokens: 512,
            max_sequences: 4,
            rope_config_sha256: "44".repeat(32),
            kv_layout_sha256: "55".repeat(32),
        };
        let exact = ExactModelCapabilities {
            declaration: CapabilityDeclarationStatus::Inspected,
            prompts: PromptInputCapabilities {
                chat: false,
                completion_text: true,
                completion_token_ids: true,
                fill_in_middle: None,
            },
            outputs: GenerationOutputCapabilities {
                generated_token_ids: true,
                token_observations: false,
                probability_stages: Vec::new(),
                log_probability_stages: Vec::new(),
            },
            batches: GenerationBatchCapabilities {
                max_cases: 4,
                ordered_outputs: true,
                per_case_sampling: true,
                per_case_cancellation: true,
            },
            cache: CacheOperationCapabilities {
                sequence_snapshot: true,
                sequence_restore: true,
                per_case_restore: true,
                token_exact_shared_prefix: true,
            },
            media: Vec::new(),
        };
        RuntimeModelInspection {
            descriptor: NativeModelDescriptor {
                stable_model_id: format!("sha256:{model_sha256}"),
                model_id: profile.model_id.clone(),
                display_name: "Fixture Model".to_string(),
                architecture: "fixture".to_string(),
                parameter_count: 10,
                model_size: 1_000,
                context_tokens: 8_192,
                max_sequences: 4,
                backend: "cpu".to_string(),
                capabilities: ModelCapabilities {
                    prompt_forms: vec![PromptForm::Completion],
                    chat_template_available: false,
                    multimodal: false,
                    media_kinds: Vec::new(),
                    streaming: true,
                    cancellation: true,
                    max_batch_inputs: 4,
                    sampling_parameters: vec![SamplingParameter::Seed],
                    exact,
                },
            },
            fingerprint,
        }
    }

    fn base_generation(branch_id: BranchId, run_id: GenerationRunId) -> GenerationStart {
        GenerationStart {
            run_id,
            branch_id,
            document_id: loom_types::DocumentId::new(),
            source_revision_id: loom_types::RevisionId::new(),
            target_range: ByteRange::new(7, 7).expect("cursor range"),
            model_environment_artifact_id: ArtifactId::new(),
            prompt_recipe_artifact_id: ArtifactId::new(),
            context_recipe_artifact_id: ArtifactId::new(),
            authority_policy_artifact_id: ArtifactId::new(),
            seed: 0,
            sampling: serde_json::Value::Null,
        }
    }

    fn request_with_two_cases() -> ExactContinuationRequest {
        let prefix = "The rain began at the window".to_string();
        let first_generation = base_generation(BranchId::new(), GenerationRunId::new());
        let mut second_generation = first_generation.clone();
        second_generation.branch_id = BranchId::new();
        second_generation.run_id = GenerationRunId::new();
        let first = ContinuationCase::bind_sampling(
            first_generation,
            SamplingConfig {
                seed: 41,
                max_tokens: 2,
                ..SamplingConfig::default()
            },
        )
        .expect("bind sampling");
        let second = ContinuationCase::bind_sampling(
            second_generation,
            SamplingConfig {
                seed: 42,
                max_tokens: 2,
                ..SamplingConfig::default()
            },
        )
        .expect("bind sampling");
        ExactContinuationRequest {
            request_id: "fixture-request".to_string(),
            model: model_profile(),
            exact_manuscript_prefix: prefix.clone(),
            prompt_recipe: PromptRecipe {
                mode: PromptMode::Completion,
                exact_prompt_blob_id: BlobId::digest(prefix.as_bytes()),
                exact_prompt_token_ids: None,
                ordered_input_artifact_ids: Vec::new(),
                prompt_token_count: None,
            },
            cases: vec![first, second],
        }
    }

    fn native_output(
        request: &ExactContinuationRequest,
        input_index: usize,
        state: GenerationState,
        fixture: bool,
    ) -> GenerationOutput {
        GenerationOutput {
            request_id: request.request_id.clone(),
            branch_id: request.cases[input_index].generation.branch_id.to_string(),
            input_index,
            model_id: request.model.model_id.clone(),
            text: " and did not stop.".to_string(),
            generated_token_ids: vec![101, 102],
            token_observations: None,
            state,
            finish_reason: if state == GenerationState::Cancelled {
                "cancelled".to_string()
            } else {
                "max_tokens".to_string()
            },
            metrics: NativeMetrics {
                prompt_tokens: 7,
                completion_tokens: 2,
                shared_prefix_tokens: 6,
                duration_ms: 20,
                first_token_ms: Some(5),
                tokens_per_second: 100.0,
                cache: GenerationCacheMetrics {
                    supplied_prefix_tokens: 0,
                    restored_prefix_tokens: 0,
                    batch_shared_prefix_tokens: 6,
                },
            },
            real_engine_invoked: !fixture,
            fake_fixture: fixture,
            transport: if fixture {
                NativeTransport::FakeFixture
            } else {
                NativeTransport::InProcess
            },
        }
    }

    fn native_events(request: &ExactContinuationRequest) -> Vec<NativeEvent> {
        request
            .cases
            .iter()
            .enumerate()
            .flat_map(|(index, case)| {
                let branch_id = case.generation.branch_id.to_string();
                let sequence_id = i32::try_from(index).expect("fixture sequence fits i32");
                [
                    NativeEvent {
                        request_id: request.request_id.clone(),
                        branch_id: branch_id.clone(),
                        sequence_id,
                        input_index: index,
                        event_index: 0,
                        event: NativeEventKind::State {
                            state: GenerationState::Prefilling,
                        },
                    },
                    NativeEvent {
                        request_id: request.request_id.clone(),
                        branch_id: branch_id.clone(),
                        sequence_id,
                        input_index: index,
                        event_index: 1,
                        event: NativeEventKind::State {
                            state: GenerationState::Generating,
                        },
                    },
                    NativeEvent {
                        request_id: request.request_id.clone(),
                        branch_id: branch_id.clone(),
                        sequence_id,
                        input_index: index,
                        event_index: 2,
                        event: NativeEventKind::Delta {
                            text: " and".to_string(),
                        },
                    },
                    NativeEvent {
                        request_id: request.request_id.clone(),
                        branch_id,
                        sequence_id,
                        input_index: index,
                        event_index: 3,
                        event: NativeEventKind::State {
                            state: GenerationState::Completed,
                        },
                    },
                ]
            })
            .collect()
    }

    fn fake_runtime(
        request: &ExactContinuationRequest,
        outputs: Vec<GenerationOutput>,
        events: Vec<NativeEvent>,
        ready: bool,
        class: RuntimeEvidenceClass,
    ) -> Arc<FakeRuntime> {
        let (event_tx, event_rx) = bounded(events.len().max(1));
        for event in events {
            event_tx.send(event).expect("queue fake event");
        }
        drop(event_tx);
        Arc::new(FakeRuntime {
            class,
            inspection: model_inspection(&request.model),
            execution: Arc::new(FakeExecution {
                event_rx,
                result: Mutex::new(Some(outputs)),
                ready: AtomicBool::new(ready),
                complete_on_cancel: AtomicBool::new(false),
                panic_on_receive: AtomicBool::new(false),
                cancelled: Mutex::new(Vec::new()),
            }),
            captured: Mutex::new(None),
            released: AtomicBool::new(false),
        })
    }

    fn drain_events(handle: &LlamaGenerationHandle) -> Vec<LoomEvent> {
        let mut events = Vec::new();
        while let Some(event) = handle
            .receive_event_timeout(Duration::from_millis(10))
            .expect("receive event")
        {
            events.push(event);
        }
        events
    }

    fn assert_exact_native_request(
        captured: &GenerationBatchRequest,
        request: &ExactContinuationRequest,
    ) {
        assert_eq!(captured.cases.len(), 2);
        for (index, case) in captured.cases.iter().enumerate() {
            assert_eq!(
                case.sampling.seed,
                41 + u32::try_from(index).expect("fixture index fits u32")
            );
            match &case.input {
                llama_native_types::GenerationInput::Completion { prompts } => {
                    assert_eq!(prompts.len(), 1);
                    assert_eq!(
                        prompts[0],
                        CompletionPrompt::Text {
                            text: request.exact_manuscript_prefix.clone(),
                            special_tokens: SpecialTokenPolicy::NoBosParseSpecial,
                        }
                    );
                }
                other => panic!("unexpected hidden prompt mode: {other:?}"),
            }
        }
    }

    fn assert_fixture_candidate_provenance(result: &ExactContinuationResult) {
        assert_eq!(result.candidates.len(), 2);
        assert_ne!(
            result.candidates[0].candidate.candidate_id,
            result.candidates[1].candidate.candidate_id
        );
        assert_eq!(
            result.candidates[0].candidate.output_blob_id,
            result.candidates[1].candidate.output_blob_id
        );
        for (input_index, record) in result.candidates.iter().enumerate() {
            let provenance = record
                .token_trace
                .provenance
                .as_ref()
                .expect("generation provenance");
            assert_eq!(provenance.evidence_kind, InferenceEvidenceKind::Fixture);
            assert_ne!(
                provenance.evidence_kind,
                InferenceEvidenceKind::HistoricalReceipt
            );
            assert_ne!(
                provenance.evidence_kind,
                InferenceEvidenceKind::LiveInference
            );
            assert_eq!(record.token_trace.generated_token_ids, vec![101, 102]);
            assert!(record.token_trace.observations.is_empty());
            assert_eq!(
                provenance.metrics.shared_prefix_tokens,
                Some(6),
                "cache reuse must be mapped, not inferred"
            );
            assert_eq!(
                record.token_trace.raw_event_stream_blob_id,
                BlobId::digest(&record.raw_event_stream_bytes)
            );
            assert_eq!(
                provenance.backend_receipt_blob_id,
                Some(BlobId::digest(&record.backend_receipt_bytes))
            );
            validate_candidate_receipt_binding(
                record,
                &result.request_id,
                result.exact_prompt_blob_id,
                result.model_environment.environment_id,
                &result.model.local_model_id,
                input_index,
            )
            .expect("receipt must bind the exact fixture result");
        }
    }

    fn assert_stream_contract(events: &[LoomEvent], request: &ExactContinuationRequest) {
        assert!(events.iter().any(|event| matches!(
            event,
            LoomEvent::Generation(GenerationEvent {
                kind: GenerationEventKind::TextDelta { text },
                ..
            }) if text == " and"
        )));
        for case in &request.cases {
            let terminals = events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        LoomEvent::GenerationTerminal(terminal)
                            if terminal.branch_id == case.generation.branch_id
                    )
                })
                .count();
            assert_eq!(terminals, 1);
        }
    }

    #[test]
    fn exact_prefix_batch_is_completion_only_and_fixture_evidence_stays_fixture() {
        let request = request_with_two_cases();
        let outputs = (0..request.cases.len())
            .map(|index| native_output(&request, index, GenerationState::Completed, true))
            .collect();
        let runtime = fake_runtime(
            &request,
            outputs,
            native_events(&request),
            true,
            RuntimeEvidenceClass::TestFixture,
        );
        let backend = LlamaBackend::with_runtime(runtime.clone(), 64).expect("backend");
        let handle = backend
            .start_exact_continuation(request.clone())
            .expect("start generation");
        let result = handle
            .wait_timeout(Duration::from_secs(2))
            .expect("generation result");
        let loom_events = drain_events(&handle);

        let captured = runtime
            .captured
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .expect("captured request");
        assert_exact_native_request(&captured, &request);
        assert_fixture_candidate_provenance(&result);
        assert_stream_contract(&loom_events, &request);
        drop(handle);
        assert!(runtime.execution.cancelled_cases().is_empty());
        assert_eq!(
            backend
                .release_model(&request.model)
                .expect("release fixture model"),
            ModelRelease::Released {
                proof: CompleteModelRelease::from_complete_count(std::num::NonZeroUsize::MIN,),
            }
        );
        assert_eq!(
            backend
                .release_model(&request.model)
                .expect("second release is idempotent"),
            ModelRelease::AlreadyAbsent
        );
    }

    #[test]
    fn timeout_retains_worker_until_later_completion_is_joined() {
        let request = request_with_two_cases();
        let outputs = (0..request.cases.len())
            .map(|index| native_output(&request, index, GenerationState::Completed, true))
            .collect();
        let runtime = fake_runtime(
            &request,
            outputs,
            Vec::new(),
            false,
            RuntimeEvidenceClass::TestFixture,
        );
        let backend = LlamaBackend::with_runtime(runtime.clone(), 32).expect("backend");
        let handle = backend
            .start_exact_continuation(request)
            .expect("start generation");

        assert!(matches!(
            handle.wait_timeout(Duration::ZERO),
            Err(LlamaBackendError::ResultTimeout)
        ));
        runtime.execution.set_ready();
        handle
            .wait_timeout(Duration::from_secs(2))
            .expect("later generation result");
        drop(handle);
        assert!(runtime.execution.cancelled_cases().is_empty());
    }

    #[test]
    fn early_drop_cancels_every_case_and_joins_the_worker() {
        let request = request_with_two_cases();
        let expected_cases = request
            .cases
            .iter()
            .map(|case| case.generation.branch_id.to_string())
            .collect::<Vec<_>>();
        let outputs = (0..request.cases.len())
            .map(|index| native_output(&request, index, GenerationState::Cancelled, true))
            .collect();
        let runtime = fake_runtime(
            &request,
            outputs,
            Vec::new(),
            false,
            RuntimeEvidenceClass::TestFixture,
        );
        runtime
            .execution
            .complete_on_cancel
            .store(true, Ordering::Release);
        let backend = LlamaBackend::with_runtime(runtime.clone(), 32).expect("backend");
        let handle = backend
            .start_exact_continuation(request)
            .expect("start generation");

        drop(handle);

        assert_eq!(runtime.execution.cancelled_cases(), expected_cases);
    }

    #[test]
    fn worker_panic_is_reported_by_the_join_boundary() {
        let request = request_with_two_cases();
        let outputs = (0..request.cases.len())
            .map(|index| native_output(&request, index, GenerationState::Completed, true))
            .collect();
        let runtime = fake_runtime(
            &request,
            outputs,
            Vec::new(),
            false,
            RuntimeEvidenceClass::TestFixture,
        );
        runtime
            .execution
            .panic_on_receive
            .store(true, Ordering::Release);
        let backend = LlamaBackend::with_runtime(runtime.clone(), 32).expect("backend");
        let handle = backend
            .start_exact_continuation(request)
            .expect("start generation");

        assert!(matches!(
            handle.wait_timeout(Duration::from_secs(2)),
            Err(LlamaBackendError::WorkerPanicked)
        ));
        drop(handle);
        assert!(runtime.execution.cancelled_cases().is_empty());
    }

    #[test]
    fn cancellation_targets_one_branch_and_remains_recoverable() {
        let request = request_with_two_cases();
        let outputs = vec![
            native_output(&request, 0, GenerationState::Cancelled, true),
            native_output(&request, 1, GenerationState::Completed, true),
        ];
        let runtime = fake_runtime(
            &request,
            outputs,
            Vec::new(),
            false,
            RuntimeEvidenceClass::TestFixture,
        );
        let backend = LlamaBackend::with_runtime(runtime.clone(), 32).expect("backend");
        let handle = backend
            .start_exact_continuation(request.clone())
            .expect("start generation");
        let cancelled_branch = request.cases[0].generation.branch_id;
        assert!(handle.cancel_branch(cancelled_branch));
        assert_eq!(
            runtime.execution.cancelled_cases(),
            vec![cancelled_branch.to_string()]
        );
        runtime.execution.set_ready();
        let result = handle
            .wait_timeout(Duration::from_secs(2))
            .expect("generation result");
        assert_eq!(
            result.candidates[0].terminal.status,
            GenerationTerminalStatus::Cancelled
        );
        assert!(!result.candidates[0].output_text.is_empty());
        let events = drain_events(&handle);
        assert!(events.iter().any(|event| matches!(
            event,
            LoomEvent::Generation(GenerationEvent {
                branch_id,
                kind: GenerationEventKind::CancellationRequested,
                ..
            }) if *branch_id == cancelled_branch
        )));
    }

    #[test]
    fn fixture_runtime_cannot_label_a_result_as_live_inference() {
        let request = request_with_two_cases();
        let outputs = (0..request.cases.len())
            .map(|index| native_output(&request, index, GenerationState::Completed, false))
            .collect();
        let runtime = fake_runtime(
            &request,
            outputs,
            Vec::new(),
            true,
            RuntimeEvidenceClass::TestFixture,
        );
        let backend = LlamaBackend::with_runtime(runtime, 32).expect("backend");
        let handle = backend
            .start_exact_continuation(request)
            .expect("start generation");
        let error = handle
            .wait_timeout(Duration::from_secs(2))
            .expect_err("dishonest fixture output must fail closed");
        assert!(matches!(error, LlamaBackendError::OutputContract(_)));
        let events = drain_events(&handle);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    LoomEvent::GenerationTerminal(GenerationTerminalEvent {
                        status: GenerationTerminalStatus::Failed,
                        ..
                    })
                ))
                .count(),
            2
        );
    }

    #[test]
    #[ignore = "requires LOOM_GGUF_MODEL_PATH and a real local GGUF"]
    fn real_gguf_raw_family_acceptance() -> Result<(), Box<dyn std::error::Error>> {
        let model_path = std::env::var("LOOM_GGUF_MODEL_PATH")?;
        let result = run_real_raw_family(&model_path)?;
        assert_eq!(result.candidates.len(), 2);
        assert!(result.candidates.iter().all(|candidate| {
            candidate
                .token_trace
                .provenance
                .as_ref()
                .is_some_and(|provenance| {
                    provenance.evidence_kind == InferenceEvidenceKind::LiveInference
                })
        }));
        Ok(())
    }

    #[test]
    #[ignore = "requires LOOM_GGUF_MODEL_PATH and a real local GGUF"]
    fn real_gguf_cpu_cancellation_and_complete_release() -> Result<(), Box<dyn std::error::Error>> {
        let model_path = std::env::var("LOOM_GGUF_MODEL_PATH")?;
        let mut request = request_with_two_cases();
        request.model = LocalModelProfile::for_gguf(&model_path);
        request.model.device = crate::LocalDevicePreference::Cpu;
        request.model.max_parallel_cases = 2;
        request.request_id = "real-cancel-release".to_string();
        request.exact_manuscript_prefix = "The lamp made a small island of light".to_string();
        request.prompt_recipe.exact_prompt_blob_id =
            BlobId::digest(request.exact_manuscript_prefix.as_bytes());
        for case in &mut request.cases {
            case.sampling.max_tokens = 2_048;
            case.generation.sampling = serde_json::to_value(&case.sampling)?;
        }
        let profile = request.model.clone();
        let branches = request
            .cases
            .iter()
            .map(|case| case.generation.branch_id)
            .collect::<Vec<_>>();
        let backend = LlamaBackend::default();
        let handle = backend.start_exact_continuation(request)?;
        for branch_id in branches {
            assert!(handle.cancel_branch(branch_id));
        }
        let result = handle.wait_timeout(Duration::from_secs(300))?;
        assert!(
            result.candidates.iter().all(|candidate| {
                candidate.terminal.status == GenerationTerminalStatus::Cancelled
            })
        );
        drop(handle);
        let ModelRelease::Released { proof } = backend.release_model(&profile)? else {
            return Err("loaded CPU model was already absent during release".into());
        };
        assert_eq!(proof.matched_slots(), proof.released_slots());
        assert_eq!(proof.released_slots().get(), 1);
        let shutdown = backend.shutdown_and_verify_empty()?;
        assert_eq!(shutdown.matched_slots(), 0);
        assert_eq!(shutdown.released_slots(), 0);
        Ok(())
    }

    #[test]
    #[ignore = "requires LOOM_GEMMA4_E2B_BASE_PATH and the pinned Gemma 4 E2B base Q8 GGUF"]
    fn real_gemma4_e2b_base_raw_family_acceptance() -> Result<(), Box<dyn std::error::Error>> {
        const EXPECTED_SHA256: &str =
            "aa0a9a03993440f45176f19f8189a2e84c210ff8628ec13dc6edf42d017f7670";
        let model_path = std::env::var("LOOM_GEMMA4_E2B_BASE_PATH")?;
        let result = run_real_raw_family(&model_path)?;

        assert_eq!(result.model.architecture.as_deref(), Some("gemma4"));
        assert_eq!(result.model.model_sha256, EXPECTED_SHA256);
        assert_eq!(
            result.model.capabilities.chat,
            crate::CapabilitySupport::Unsupported
        );
        assert!(result.model.capabilities.completion_text.is_supported());
        assert_eq!(result.candidates.len(), 2);
        assert_eq!(result.candidates[0].generation.seed, 41);
        assert_eq!(result.candidates[1].generation.seed, 42);
        assert_ne!(
            result.candidates[0].generation.branch_id,
            result.candidates[1].generation.branch_id
        );
        assert!(result.candidates.iter().all(|candidate| {
            !candidate.token_trace.generated_token_ids.is_empty()
                && candidate
                    .token_trace
                    .provenance
                    .as_ref()
                    .is_some_and(|provenance| {
                        provenance.evidence_kind == InferenceEvidenceKind::LiveInference
                            && provenance.metrics.shared_prefix_tokens.unwrap_or_default() > 0
                    })
        }));
        assert_eq!(
            result.exact_prompt_blob_id,
            BlobId::digest(result.exact_manuscript_prefix.as_bytes())
        );
        Ok(())
    }

    fn run_real_raw_family(
        model_path: &str,
    ) -> Result<ExactContinuationResult, Box<dyn std::error::Error>> {
        let mut request = request_with_two_cases();
        request.model = LocalModelProfile::for_gguf(model_path);
        request.model.device = crate::LocalDevicePreference::Cpu;
        request.model.max_parallel_cases = 2;
        request.request_id = "real-raw-family".to_string();
        request.exact_manuscript_prefix = "The lamp made a small island of light".to_string();
        request.prompt_recipe.exact_prompt_blob_id =
            BlobId::digest(request.exact_manuscript_prefix.as_bytes());
        let backend = LlamaBackend::default();
        let handle = backend.start_exact_continuation(request)?;
        Ok(handle.wait_timeout(Duration::from_secs(300))?)
    }
}
