#[cfg(test)]
mod build_identity;
pub mod control_math;
mod controlled_runtime;
mod embedding_runtime;
mod operation_registry;
mod state_buffer;

pub use controlled_runtime::{
    ControlledGenerationSubmission, ControlledGenerationTicket, ControlledRuntimeCostEvidence,
    VerifiedControlledGenerationBatch, VerifiedControlledGenerationTerminal,
};
pub use embedding_runtime::{VerifiedEmbeddingBatch, VerifiedEmbeddingTerminal};

use operation_registry::{
    ActiveRequest, RequestClass, RequestControls, RequestLease, RequestRegistry,
};

use crossbeam_channel::{Receiver, Sender, bounded};
use encoding_rs::{CoderResult, UTF_8};
use fs4::FileExt as Fs4FileExt;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::params::{LlamaContextParams, LlamaPoolingType};
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaChatTemplate, LlamaModel};
use llama_cpp_2::mtmd::{
    MtmdBitmap, MtmdContext, MtmdContextParams, MtmdInputText, mtmd_default_marker,
};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;
use llama_native_types::{
    BranchRequest, CacheOperationCapabilities, CapabilityDeclarationStatus, ChatMessage, ChatRole,
    ChatTemplateChoice, CompletionPrompt, EmbeddingBatchOutput, EmbeddingBatchRequest,
    EmbeddingCapabilities, EmbeddingNormalization, EmbeddingNormalizationSupport,
    EmbeddingOutputConfig, EmbeddingPooling, EmbeddingPoolingSupport, EmbeddingTransportEvidence,
    EmbeddingVectorOutput, ExactModelCapabilities, ExactTokenBatchBudgetError,
    ExactTokenBatchCellBudget, GenerationBatchCapabilities, GenerationBatchRequest,
    GenerationCacheMetrics, GenerationCase, GenerationEvent, GenerationEventKind, GenerationInput,
    GenerationMetrics, GenerationOutput, GenerationOutputCapabilities, GenerationRequest,
    GenerationState, MAX_EMBEDDING_BATCH_INPUTS, MAX_EMBEDDING_BATCH_VALUES,
    MAX_EMBEDDING_DIMENSIONS, MAX_EMBEDDING_INPUT_TOKENS, MAX_EMBEDDING_VALUES_PER_OUTPUT,
    MAX_GENERATED_OUTPUT_BYTES, MAX_PARALLEL_SEQUENCES, MediaInput, MediaInputCapability,
    MediaKind, ModelCapabilities, ModelFingerprint, ModelRuntimeState, NativeDevice, NativeError,
    NativeErrorCode, NativeEvidenceCapabilities, NativeModelConfig, NativeModelDescriptor,
    NativeTransport, PreparedPrompt, ProjectorRequirement, PromptForm, PromptInputCapabilities,
    PromptTokenPolicy, ResidentModelStatus, SamplerKind, SamplingConfig, SamplingParameter,
    SequenceStateBlob, SharedPrefixBatchRequest, SpecialTokenPolicy, TokenizedPrompt,
    exact_token_batch_cell_budget,
};
use sha2::{Digest, Sha256};

use std::collections::{HashMap, VecDeque};
use std::fs::{File, Metadata};
use std::io::Read as _;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const LLAMA_CPP_BINDING_VERSION: &str = "0.1.154";
pub const LLAMA_CPP_BINDING_REV: &str = "a3cf95eb1d4fa748480eb780e6fcbfc1a5c1c391";
pub const LLAMA_CPP_REV: &str = "5f55650a78f92aff4d48d671423e888fac0469ff";
/// SHA-256 of a private, domain-separated build-input accumulator. The raw
/// inputs are deliberately neither compiled into this crate nor exposed.
///
/// ```compile_fail
/// let _ = llama_native_engine::LLAMA_NATIVE_BUILD_MANIFEST;
/// ```
pub const LLAMA_NATIVE_BUILD_MANIFEST_SHA256: &str = env!("LLAMA_NATIVE_BUILD_MANIFEST_SHA256");
const COMMAND_CAPACITY: usize = 32;
const EVENT_CAPACITY: usize = 256;

static BACKEND: OnceLock<Result<LlamaBackend, String>> = OnceLock::new();

type NativeResult<T> = Result<T, NativeError>;

/// A bounded wait observes a running operation without discarding its only
/// completion handle. Timeout is not an operation terminal.
#[derive(Debug)]
#[must_use]
pub enum WaitOutcome<Ticket, Output> {
    Ready(Output),
    TimedOut(Ticket),
}

/// A nonblocking wait consumes the ticket without discarding the caller's only
/// completion handle when the operation is still pending.
#[derive(Debug)]
#[must_use]
pub enum TryWaitOutcome<Ticket, Output> {
    Ready(Output),
    Pending(Ticket),
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ArtifactFileState {
    device: u64,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ArtifactFileState {
    length: u64,
    created: u64,
    modified: u64,
    attributes: u32,
}

#[cfg(not(any(unix, windows)))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct ArtifactFileState {
    length: u64,
    modified: Option<std::time::SystemTime>,
    readonly: bool,
}

impl ArtifactFileState {
    const fn length(&self) -> u64 {
        self.length
    }
}

#[derive(Debug)]
struct ModelArtifactGuard {
    label: &'static str,
    original_path: std::path::PathBuf,
    load_path: std::path::PathBuf,
    file: File,
    initial_state: ArtifactFileState,
    expected_sha256: String,
    #[cfg(all(test, unix))]
    payload_bytes_read: std::sync::atomic::AtomicU64,
    strict_binding_error: Option<String>,
}

#[derive(Debug)]
struct ModelArtifactGuards {
    model: ModelArtifactGuard,
    projector: Option<ModelArtifactGuard>,
}

/// Owner-worker authority proving one exact-token generation batch completed
/// through the live in-process engine.
///
/// The seal deliberately has no public constructor and implements neither
/// `Clone`, `Default`, nor Serde traits. Callers may inspect its evidence but
/// cannot reconstruct authority from caller-authored JSON or copied outputs.
/// On supported Unix hosts, model artifacts are reopened through a pinned file
/// descriptor, the held bytes are hashed once before the load attempt, and
/// identity plus high-resolution mutation metadata is checked before and after
/// strict generation. Unix file locks are advisory: the checks revoke authority
/// after detected mutation, but do not claim that llama.cpp could never parse
/// raced bytes. This is not a defense against an OS-compromised process or an
/// exotic filesystem that can hide and restore mutation metadata entirely
/// between checks. Strict artifact authority is currently unavailable on
/// Windows and other hosts without a verified handle-derived reopen path.
///
/// ```compile_fail
/// use llama_native_engine::VerifiedGenerationBatch;
/// fn clone_authority(seal: &VerifiedGenerationBatch) -> VerifiedGenerationBatch {
///     seal.clone()
/// }
/// ```
///
/// ```compile_fail
/// use llama_native_engine::VerifiedGenerationBatch;
/// fn require_default<T: Default>() {}
/// require_default::<VerifiedGenerationBatch>();
/// ```
///
/// ```compile_fail
/// use llama_native_engine::VerifiedGenerationBatch;
/// fn require_serialize<T: serde::Serialize>() {}
/// require_serialize::<VerifiedGenerationBatch>();
/// ```
///
/// ```compile_fail
/// use llama_native_engine::VerifiedGenerationBatch;
/// fn require_deserialize<T: for<'de> serde::Deserialize<'de>>() {}
/// require_deserialize::<VerifiedGenerationBatch>();
/// ```
pub struct VerifiedGenerationBatch {
    request: GenerationBatchRequest,
    model_fingerprint: ModelFingerprint,
    outputs: Vec<GenerationOutput>,
    terminal_sampled_token_ids: Vec<Option<i32>>,
    events: Vec<GenerationEvent>,
    token_piece_traces: Vec<TokenPieceTrace>,
}

/// Exact token-piece bytes captured at the live sampled-token decode site.
///
/// Boundaries are cumulative byte offsets. They always begin at zero, contain
/// one entry beyond the generated token count, are nondecreasing, and end at
/// `raw_piece_bytes().len()`. Equal adjacent boundaries represent a zero-byte
/// token piece. Individual pieces are raw bytes and need not be valid UTF-8.
///
/// This type has no public constructor and deliberately implements neither
/// `Clone` nor Serde traits. It is inspectable evidence held by a live
/// [`VerifiedGenerationBatch`], not independently reconstructable authority.
///
/// ```compile_fail
/// use llama_native_engine::TokenPieceTrace;
/// fn clone_trace(trace: &TokenPieceTrace) -> TokenPieceTrace { trace.clone() }
/// ```
///
/// ```compile_fail
/// use llama_native_engine::TokenPieceTrace;
/// fn require_serialize<T: serde::Serialize>() {}
/// require_serialize::<TokenPieceTrace>();
/// ```
///
/// ```compile_fail
/// use llama_native_engine::TokenPieceTrace;
/// let _ = TokenPieceTrace {
///     raw_piece_bytes: Vec::new(),
///     cumulative_boundaries: vec![0],
/// };
/// ```
#[derive(Debug)]
pub struct TokenPieceTrace {
    raw_piece_bytes: Vec<u8>,
    cumulative_boundaries: Vec<u64>,
}

impl TokenPieceTrace {
    fn with_token_capacity(token_capacity: usize) -> Self {
        let mut cumulative_boundaries = Vec::with_capacity(token_capacity.saturating_add(1));
        cumulative_boundaries.push(0);
        Self {
            raw_piece_bytes: Vec::new(),
            cumulative_boundaries,
        }
    }

    fn push_piece(&mut self, piece: &[u8]) -> NativeResult<()> {
        let next_len = checked_generated_output_len(self.raw_piece_bytes.len(), piece.len())?;
        let boundary = u64::try_from(next_len).map_err(|_| {
            generation_verification_error("token-piece byte boundary does not fit u64")
        })?;
        self.raw_piece_bytes.extend_from_slice(piece);
        self.cumulative_boundaries.push(boundary);
        Ok(())
    }

    fn validate(&self, token_count: usize) -> NativeResult<()> {
        let expected_boundary_count = token_count.checked_add(1).ok_or_else(|| {
            generation_verification_error("token-piece boundary count overflowed")
        })?;
        if self.cumulative_boundaries.len() != expected_boundary_count
            || self.cumulative_boundaries.first() != Some(&0)
            || !self
                .cumulative_boundaries
                .windows(2)
                .all(|pair| pair[0] <= pair[1])
            || self.cumulative_boundaries.last().copied()
                != u64::try_from(self.raw_piece_bytes.len()).ok()
        {
            return Err(generation_verification_error(
                "token-piece bytes and cumulative boundaries are inconsistent",
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn raw_piece_bytes(&self) -> &[u8] {
        &self.raw_piece_bytes
    }

    #[must_use]
    pub fn cumulative_boundaries(&self) -> &[u64] {
        &self.cumulative_boundaries
    }
}

impl std::fmt::Debug for VerifiedGenerationBatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VerifiedGenerationBatch")
            .field("request_id", &self.request.request_id)
            .field("model_id", &self.model_fingerprint.model_id)
            .field("model_sha256", &self.model_fingerprint.model_sha256)
            .field("case_count", &self.request.cases.len())
            .field("output_count", &self.outputs.len())
            .field("terminal_count", &self.terminal_sampled_token_ids.len())
            .field("event_count", &self.events.len())
            .field("token_piece_trace_count", &self.token_piece_traces.len())
            .finish()
    }
}

impl VerifiedGenerationBatch {
    #[must_use]
    pub const fn request(&self) -> &GenerationBatchRequest {
        &self.request
    }

    #[must_use]
    pub const fn model_fingerprint(&self) -> &ModelFingerprint {
        &self.model_fingerprint
    }

    #[must_use]
    pub fn outputs(&self) -> &[GenerationOutput] {
        &self.outputs
    }

    /// The sampled terminal token for each ordered output. This is `Some` only
    /// when the live model recognized an end-of-generation token; cancellation,
    /// maximum-token, and stop-sequence terminals carry `None`.
    #[must_use]
    pub fn terminal_sampled_token_ids(&self) -> &[Option<i32>] {
        &self.terminal_sampled_token_ids
    }

    #[must_use]
    pub fn events(&self) -> &[GenerationEvent] {
        &self.events
    }

    #[must_use]
    pub fn token_piece_traces(&self) -> &[TokenPieceTrace] {
        &self.token_piece_traces
    }
}

#[derive(Debug)]
struct VerifiedGenerationEvidence {
    request: GenerationBatchRequest,
    model_fingerprint: ModelFingerprint,
    terminal_sampled_token_ids: Vec<Option<i32>>,
    events: Vec<GenerationEvent>,
    token_piece_traces: Vec<TokenPieceTrace>,
}

#[derive(Debug)]
struct GenerationAuthorityCapture {
    terminal_sampled_token_ids: Vec<Option<i32>>,
    events: Vec<GenerationEvent>,
    token_piece_traces: Vec<TokenPieceTrace>,
}

#[derive(Debug)]
struct GenerationCompletion {
    outputs: Vec<GenerationOutput>,
    authority: NativeResult<Box<VerifiedGenerationEvidence>>,
}

#[derive(Debug)]
struct GeneratedBatchExecution {
    outputs: Vec<GenerationOutput>,
    terminal_sampled_token_ids: Vec<Option<i32>>,
    token_piece_traces: Vec<TokenPieceTrace>,
}

/// Private bounded trace emitted at the exact legacy sampler call site.
///
/// Controlled baseline authority may retain this trace, but callers cannot
/// construct or inject it through any public request or receipt surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RuntimeSampleSelection {
    case_index: usize,
    generated_index: usize,
    token_id: i32,
    terminal: bool,
}

impl GenerationCompletion {
    fn unverified(outputs: Vec<GenerationOutput>) -> Self {
        Self::authority_rejected(
            outputs,
            NativeError::new(
                NativeErrorCode::UnsupportedParameter,
                "this generation path does not carry exact-token owner-worker authority",
            ),
        )
    }

    fn authority_rejected(outputs: Vec<GenerationOutput>, error: NativeError) -> Self {
        Self {
            outputs,
            authority: Err(error),
        }
    }

    fn verified(outputs: Vec<GenerationOutput>, evidence: VerifiedGenerationEvidence) -> Self {
        Self {
            outputs,
            authority: Ok(Box::new(evidence)),
        }
    }

    fn into_outputs(self) -> Vec<GenerationOutput> {
        self.outputs
    }

    fn into_verified(self) -> NativeResult<VerifiedGenerationBatch> {
        let Self { outputs, authority } = self;
        let evidence = *authority?;
        Ok(VerifiedGenerationBatch {
            request: evidence.request,
            model_fingerprint: evidence.model_fingerprint,
            outputs,
            terminal_sampled_token_ids: evidence.terminal_sampled_token_ids,
            events: evidence.events,
            token_piece_traces: evidence.token_piece_traces,
        })
    }
}

#[derive(Debug)]
pub struct GenerationTicket {
    pub request_id: String,
    pub events: Receiver<GenerationEvent>,
    result: Receiver<NativeResult<GenerationCompletion>>,
    control: Arc<ActiveRequest>,
}

impl GenerationTicket {
    pub fn cancel_branch(&self, branch_id: &str) -> bool {
        self.control.cancel_named(branch_id)
    }

    pub fn cancel_all(&self) -> usize {
        self.control.cancel_all()
    }

    pub fn wait(self) -> NativeResult<Vec<GenerationOutput>> {
        let result = self.result.recv().map_err(|error| {
            NativeError::new(
                NativeErrorCode::WorkerStopped,
                format!("native worker stopped before returning a result: {error}"),
            )
        })?;
        result.map(GenerationCompletion::into_outputs)
    }

    pub fn wait_timeout(
        self,
        timeout: Duration,
    ) -> NativeResult<WaitOutcome<Self, Vec<GenerationOutput>>> {
        match self.result.recv_timeout(timeout) {
            Ok(result) => result
                .map(GenerationCompletion::into_outputs)
                .map(WaitOutcome::Ready),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Ok(WaitOutcome::TimedOut(self)),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => Err(NativeError::new(
                NativeErrorCode::WorkerStopped,
                "native worker stopped before returning a generation result",
            )),
        }
    }

    pub fn try_wait(self) -> NativeResult<TryWaitOutcome<Self, Vec<GenerationOutput>>> {
        match self.result.try_recv() {
            Ok(result) => result
                .map(GenerationCompletion::into_outputs)
                .map(TryWaitOutcome::Ready),
            Err(crossbeam_channel::TryRecvError::Empty) => Ok(TryWaitOutcome::Pending(self)),
            Err(crossbeam_channel::TryRecvError::Disconnected) => Err(NativeError::new(
                NativeErrorCode::WorkerStopped,
                "native worker stopped before returning a result",
            )),
        }
    }

    /// Wait for an opaque owner-worker seal. This succeeds only for the exact
    /// token `GenerateBatch` path while its platform artifact binding remains
    /// valid; compatibility, shared-prefix, multimodal, and currently Windows
    /// generation remain intentionally unverified.
    pub fn wait_verified(self) -> NativeResult<VerifiedGenerationBatch> {
        let result = self.result.recv().map_err(|error| {
            NativeError::new(
                NativeErrorCode::WorkerStopped,
                format!("native worker stopped before returning a verified result: {error}"),
            )
        })?;
        result?.into_verified()
    }

    pub fn wait_verified_timeout(
        self,
        timeout: Duration,
    ) -> NativeResult<WaitOutcome<Self, VerifiedGenerationBatch>> {
        match self.result.recv_timeout(timeout) {
            Ok(result) => result?.into_verified().map(WaitOutcome::Ready),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Ok(WaitOutcome::TimedOut(self)),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => Err(NativeError::new(
                NativeErrorCode::WorkerStopped,
                "native worker stopped before returning a verified generation result",
            )),
        }
    }

    pub fn try_wait_verified(self) -> NativeResult<TryWaitOutcome<Self, VerifiedGenerationBatch>> {
        match self.result.try_recv() {
            Ok(result) => result?.into_verified().map(TryWaitOutcome::Ready),
            Err(crossbeam_channel::TryRecvError::Empty) => Ok(TryWaitOutcome::Pending(self)),
            Err(crossbeam_channel::TryRecvError::Disconnected) => Err(NativeError::new(
                NativeErrorCode::WorkerStopped,
                "native worker stopped before returning a verified result",
            )),
        }
    }
}

impl Drop for GenerationTicket {
    fn drop(&mut self) {
        self.control.cancel_all();
    }
}

/// One cancellable owner-worker embedding request.
#[derive(Debug)]
pub struct EmbeddingTicket {
    pub request_id: String,
    result: Receiver<embedding_runtime::EmbeddingCompletion>,
    control: Arc<ActiveRequest>,
}

impl EmbeddingTicket {
    /// Request cooperative cancellation. A decode already inside llama.cpp is
    /// allowed to finish, but its values are discarded before publication.
    pub fn cancel(&self) {
        self.control.cancel_all();
    }

    pub fn wait(self) -> NativeResult<EmbeddingBatchOutput> {
        let completion = self.result.recv().map_err(|error| {
            NativeError::new(
                NativeErrorCode::WorkerStopped,
                format!("native worker stopped before returning embeddings: {error}"),
            )
        })?;
        completion.into_output()
    }

    pub fn wait_timeout(
        self,
        timeout: Duration,
    ) -> NativeResult<WaitOutcome<Self, EmbeddingBatchOutput>> {
        match self.result.recv_timeout(timeout) {
            Ok(completion) => completion.into_output().map(WaitOutcome::Ready),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Ok(WaitOutcome::TimedOut(self)),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => Err(NativeError::new(
                NativeErrorCode::WorkerStopped,
                "native worker stopped before returning embeddings",
            )),
        }
    }

    pub fn try_wait(self) -> NativeResult<TryWaitOutcome<Self, EmbeddingBatchOutput>> {
        match self.result.try_recv() {
            Ok(completion) => completion.into_output().map(TryWaitOutcome::Ready),
            Err(crossbeam_channel::TryRecvError::Empty) => Ok(TryWaitOutcome::Pending(self)),
            Err(crossbeam_channel::TryRecvError::Disconnected) => Err(NativeError::new(
                NativeErrorCode::WorkerStopped,
                "native worker stopped before returning embeddings",
            )),
        }
    }

    /// Consume this ticket into an opaque owner-worker embedding seal.
    /// Serialized outputs, fixtures, cancelled work, replayed claims, and
    /// artifact identity failures cannot satisfy this path.
    pub fn wait_verified(self) -> NativeResult<VerifiedEmbeddingBatch> {
        let completion = self.result.recv().map_err(|error| {
            NativeError::new(
                NativeErrorCode::WorkerStopped,
                format!("native worker stopped before returning verified embeddings: {error}"),
            )
        })?;
        completion.into_verified()
    }

    pub fn wait_verified_timeout(
        self,
        timeout: Duration,
    ) -> NativeResult<WaitOutcome<Self, VerifiedEmbeddingBatch>> {
        match self.result.recv_timeout(timeout) {
            Ok(completion) => completion.into_verified().map(WaitOutcome::Ready),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Ok(WaitOutcome::TimedOut(self)),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => Err(NativeError::new(
                NativeErrorCode::WorkerStopped,
                "native worker stopped before returning verified embeddings",
            )),
        }
    }

    pub fn try_wait_verified(self) -> NativeResult<TryWaitOutcome<Self, VerifiedEmbeddingBatch>> {
        match self.result.try_recv() {
            Ok(completion) => completion.into_verified().map(TryWaitOutcome::Ready),
            Err(crossbeam_channel::TryRecvError::Empty) => Ok(TryWaitOutcome::Pending(self)),
            Err(crossbeam_channel::TryRecvError::Disconnected) => Err(NativeError::new(
                NativeErrorCode::WorkerStopped,
                "native worker stopped before returning verified embeddings",
            )),
        }
    }
}

impl Drop for EmbeddingTicket {
    fn drop(&mut self) {
        self.control.cancel_all();
    }
}

/// Revocable command client for a worker owned by [`NativeModelOwner`].
///
/// Worker creation is deliberately unavailable on this cloneable type:
///
/// ```compile_fail
/// use llama_native_engine::NativeModelHandle;
/// use llama_native_types::NativeModelConfig;
/// let _ = NativeModelHandle::load(NativeModelConfig::local("model.gguf".into()));
/// ```
#[derive(Debug)]
pub struct NativeModelHandle {
    inner: Arc<NativeModelInner>,
}

impl Clone for NativeModelHandle {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

/// Linear evidence that one native model owner thread returned and was joined.
///
/// This token has no public constructor and is deliberately neither cloneable
/// nor serializable. Dropping a handle, observing an empty host slot, or seeing
/// a `Stopped` status cannot manufacture it.
#[derive(Debug)]
pub struct JoinedNativeModel {
    model_id: String,
    worker_identity: Arc<WorkerIdentity>,
    expected_workers: usize,
    joined_workers: usize,
    expected_worker_ids: Vec<String>,
    joined_worker_ids: Vec<String>,
}

impl JoinedNativeModel {
    #[must_use]
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    #[must_use]
    pub fn belongs_to(&self, handle: &NativeModelHandle) -> bool {
        Arc::ptr_eq(&self.worker_identity, &handle.inner.worker_identity)
    }

    #[must_use]
    pub fn expected_worker_count(&self) -> usize {
        self.expected_workers
    }

    #[must_use]
    pub fn joined_worker_count(&self) -> usize {
        self.joined_workers
    }

    #[must_use]
    pub fn expected_worker_ids(&self) -> &[String] {
        &self.expected_worker_ids
    }

    #[must_use]
    pub fn joined_worker_ids(&self) -> &[String] {
        &self.joined_worker_ids
    }
}

/// Unique owner authority for one native model worker.
///
/// Cloneable [`NativeModelHandle`] values are command clients only. They do
/// not own the worker thread and cannot keep it alive after this owner begins
/// shutdown. Dropping the owner synchronously cancels work, closes admission,
/// signals the priority shutdown channel, and joins the worker.
#[derive(Debug)]
pub struct NativeModelOwner {
    inner: Arc<NativeModelInner>,
    join: Option<JoinHandle<()>>,
}

impl NativeModelOwner {
    pub fn load(config: NativeModelConfig) -> NativeResult<Self> {
        let (inner, join) = NativeModelHandle::load_inner(config)?;
        Ok(Self {
            inner,
            join: Some(join),
        })
    }

    #[must_use]
    pub fn handle(&self) -> NativeModelHandle {
        NativeModelHandle {
            inner: Arc::clone(&self.inner),
        }
    }

    #[must_use]
    pub fn status(&self) -> ResidentModelStatus {
        self.handle().status()
    }

    /// Atomically closes command admission, cancels every registered request,
    /// and signals the worker's priority shutdown lane without waiting for its
    /// current native call to return.
    pub fn begin_shutdown(&self) {
        self.inner.begin_shutdown();
    }

    pub fn shutdown_joined(mut self) -> NativeResult<JoinedNativeModel> {
        let model_id = self.status().model_id;
        let worker_identity = Arc::clone(&self.inner.worker_identity);
        self.inner.begin_shutdown();
        self.join_worker()?;
        let lifecycle = self.inner.requests.shutdown();
        Ok(JoinedNativeModel {
            model_id,
            worker_identity,
            expected_workers: lifecycle.expected_workers,
            joined_workers: lifecycle.joined_workers,
            expected_worker_ids: lifecycle.expected_worker_ids,
            joined_worker_ids: lifecycle.joined_worker_ids,
        })
    }

    fn join_worker(&mut self) -> NativeResult<()> {
        let Some(join) = self.join.take() else {
            return self.inner.requests.mark_closed();
        };
        let join_result = join.join().map_err(|_| {
            NativeError::new(
                NativeErrorCode::WorkerStopped,
                "native model owner worker panicked before it could be joined",
            )
        });
        self.inner
            .requests
            .record_external_worker_joined(&self.inner.worker_id);
        let registry_result = self.inner.requests.mark_closed();
        join_result.and(registry_result)
    }
}

impl Drop for NativeModelOwner {
    fn drop(&mut self) {
        self.inner.begin_shutdown();
        let _ = self.join_worker();
    }
}

#[derive(Debug)]
struct WorkerBootstrapGuard {
    command_tx: Option<Sender<WorkerCommand>>,
    shutdown_tx: Option<Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl WorkerBootstrapGuard {
    fn new(
        command_tx: Sender<WorkerCommand>,
        shutdown_tx: Sender<()>,
        join: JoinHandle<()>,
    ) -> Self {
        Self {
            command_tx: Some(command_tx),
            shutdown_tx: Some(shutdown_tx),
            join: Some(join),
        }
    }

    fn into_parts(mut self) -> NativeResult<(Sender<WorkerCommand>, Sender<()>, JoinHandle<()>)> {
        let command_tx = self.command_tx.take().ok_or_else(|| {
            NativeError::new(
                NativeErrorCode::Internal,
                "native model bootstrap lost its command sender",
            )
        })?;
        let shutdown_tx = self.shutdown_tx.take().ok_or_else(|| {
            NativeError::new(
                NativeErrorCode::Internal,
                "native model bootstrap lost its shutdown sender",
            )
        })?;
        let join = self.join.take().ok_or_else(|| {
            NativeError::new(
                NativeErrorCode::Internal,
                "native model bootstrap lost its worker join handle",
            )
        })?;
        Ok((command_tx, shutdown_tx, join))
    }

    fn shutdown_and_join(&mut self) -> NativeResult<()> {
        if let Some(shutdown_tx) = &self.shutdown_tx {
            let _ = shutdown_tx.try_send(());
        }
        self.command_tx.take();
        self.shutdown_tx.take();
        let Some(join) = self.join.take() else {
            return Ok(());
        };
        join.join().map_err(|_| {
            NativeError::new(
                NativeErrorCode::WorkerStopped,
                "native model bootstrap worker panicked before it could be joined",
            )
        })
    }
}

impl Drop for WorkerBootstrapGuard {
    fn drop(&mut self) {
        let _ = self.shutdown_and_join();
    }
}

#[derive(Debug)]
struct NativeModelInner {
    worker_identity: Arc<WorkerIdentity>,
    worker_id: String,
    command_tx: Sender<WorkerCommand>,
    shutdown_tx: Sender<()>,
    closing: AtomicBool,
    admission: Mutex<()>,
    requests: Arc<RequestRegistry>,
    status: Arc<RwLock<ResidentModelStatus>>,
}

#[derive(Debug)]
struct WorkerIdentity;

impl NativeModelInner {
    fn ensure_accepting(&self) -> NativeResult<()> {
        if self.closing.load(Ordering::Acquire) {
            return Err(NativeError::new(
                NativeErrorCode::WorkerStopped,
                "native model worker admission is closed",
            ));
        }
        Ok(())
    }

    fn send_command(&self, command: WorkerCommand, context: &str) -> NativeResult<()> {
        let _admission = self.admission.lock().map_err(|_| {
            NativeError::new(
                NativeErrorCode::Internal,
                "native model admission lock is poisoned",
            )
        })?;
        self.ensure_accepting()?;
        self.command_tx
            .try_send(command)
            .map_err(|error| match error {
                crossbeam_channel::TrySendError::Full(_) => NativeError::new(
                    NativeErrorCode::QueueFull,
                    format!("native model command queue is full while {context}"),
                ),
                crossbeam_channel::TrySendError::Disconnected(_) => NativeError::new(
                    NativeErrorCode::WorkerStopped,
                    format!("native model worker stopped while {context}"),
                ),
            })
    }

    fn admit_command(
        &self,
        request_id: String,
        class: RequestClass,
        controls: RequestControls,
        build: impl FnOnce(RequestLease) -> WorkerCommand,
        context: &str,
    ) -> NativeResult<Arc<ActiveRequest>> {
        let _admission = self.admission.lock().map_err(|_| {
            NativeError::new(
                NativeErrorCode::Internal,
                "native model admission lock is poisoned",
            )
        })?;
        self.ensure_accepting()?;
        let (control, lease) = self.requests.reserve(request_id, class, controls)?;
        lease.queued()?;
        match self.command_tx.try_send(build(lease)) {
            Ok(()) => Ok(control),
            Err(error) => {
                let (code, message, command) = match error {
                    crossbeam_channel::TrySendError::Full(command) => (
                        NativeErrorCode::QueueFull,
                        format!("native model command queue is full while {context}"),
                        command,
                    ),
                    crossbeam_channel::TrySendError::Disconnected(command) => (
                        NativeErrorCode::WorkerStopped,
                        format!("native model worker stopped while {context}"),
                        command,
                    ),
                };
                drop(command);
                Err(NativeError::new(code, message))
            }
        }
    }

    fn begin_shutdown(&self) {
        let _admission = self
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.closing.store(true, Ordering::Release);
        self.requests.begin_quiesce_and_cancel_all();
        // This independent one-slot channel is the shutdown priority lane. A
        // full command queue cannot delay or lose the signal; an already-full
        // shutdown lane means the signal is already pending.
        let _ = self.shutdown_tx.try_send(());
    }
}

#[derive(Debug)]
enum WorkerCommand {
    EmbedBatch {
        request: EmbeddingBatchRequest,
        admitted_request_sha256: String,
        result: Sender<embedding_runtime::EmbeddingCompletion>,
        cancellation: Arc<AtomicBool>,
        request_lease: RequestLease,
    },
    GenerateBatch {
        request: GenerationBatchRequest,
        exact_cell_budget: Option<ExactTokenBatchCellBudget>,
        admission: GenerationBatchAdmission,
        event_tx: Sender<GenerationEvent>,
        result_tx: Sender<NativeResult<GenerationCompletion>>,
        cancellations: Vec<Arc<AtomicBool>>,
        reasoning_forces: Vec<Arc<AtomicBool>>,
        request_lease: RequestLease,
    },
    Generate {
        request: SharedPrefixBatchRequest,
        event_tx: Sender<GenerationEvent>,
        result_tx: Sender<NativeResult<GenerationCompletion>>,
        cancellations: Vec<Arc<AtomicBool>>,
        reasoning_forces: Vec<Arc<AtomicBool>>,
        request_lease: RequestLease,
    },
    GenerateMultimodal {
        request: GenerationRequest,
        event_tx: Sender<GenerationEvent>,
        result_tx: Sender<NativeResult<GenerationCompletion>>,
        cancellation: Arc<AtomicBool>,
        reasoning_force: Arc<AtomicBool>,
        request_lease: RequestLease,
    },
    ControlledGenerate {
        submission: Box<ControlledGenerationSubmission>,
        admitted_request_sha256: String,
        event_tx: Sender<GenerationEvent>,
        result_tx: Sender<NativeResult<controlled_runtime::ControlledGenerationCompletion>>,
        cancellations: Vec<Arc<AtomicBool>>,
        request_lease: RequestLease,
    },
    InspectControlledIdentity {
        participant_id: String,
        response: Sender<NativeResult<llama_native_types::ControlledModelIdentity>>,
    },
    Snapshot {
        sequence_id: i32,
        response: Sender<NativeResult<SequenceStateBlob>>,
    },
    Restore {
        state: SequenceStateBlob,
        destination_sequence_id: i32,
        response: Sender<NativeResult<()>>,
    },
    PrefillPrefix {
        request: SharedPrefixBatchRequest,
        response: Sender<NativeResult<SequenceStateBlob>>,
    },
    Tokenize {
        messages: Vec<ChatMessage>,
        chat_template: ChatTemplateChoice,
        response: Sender<NativeResult<TokenizedPrompt>>,
    },
    PrepareInput {
        input: GenerationInput,
        response: Sender<NativeResult<Vec<PreparedPrompt>>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GenerationBatchAdmission {
    Compatibility,
    ExactBatch,
}

impl NativeModelHandle {
    fn load_inner(
        config: NativeModelConfig,
    ) -> NativeResult<(Arc<NativeModelInner>, JoinHandle<()>)> {
        validate_config(&config)?;
        let (command_tx, command_rx) = bounded(COMMAND_CAPACITY);
        let (shutdown_tx, shutdown_rx) = bounded(1);
        let (ready_tx, ready_rx) = bounded(1);
        let status = Arc::new(RwLock::new(ResidentModelStatus {
            model_id: config.model_id.clone(),
            model_path: config.model_path.clone(),
            state: ModelRuntimeState::Loading,
            fingerprint: None,
            descriptor: None,
            active_sequences: 0,
            max_sequences: config.max_sequences,
        }));
        let worker_status = Arc::clone(&status);
        let worker_identity = Arc::new(WorkerIdentity);
        let owner_worker_identity = Arc::clone(&worker_identity);
        let worker_id = format!("llama-model-{}", config.model_id);
        let requests = Arc::new(RequestRegistry::with_external_worker(worker_id.clone()));
        let worker = thread::Builder::new()
            .name(worker_id.clone())
            .spawn(move || {
                run_worker(
                    config,
                    command_rx,
                    shutdown_rx,
                    ready_tx,
                    worker_status,
                    owner_worker_identity,
                );
            })
            .map_err(|error| {
                NativeError::new(
                    NativeErrorCode::Internal,
                    format!("failed to start native model worker: {error}"),
                )
            })?;
        let mut bootstrap = WorkerBootstrapGuard::new(command_tx, shutdown_tx, worker);
        let readiness = ready_rx.recv_timeout(Duration::from_mins(5));
        match readiness {
            Ok(Ok(())) => {}
            Ok(Err(load_error)) => {
                if let Err(join_error) = bootstrap.shutdown_and_join() {
                    return Err(NativeError::new(
                        NativeErrorCode::WorkerStopped,
                        format!(
                            "native model load failed ({load_error}); its bootstrap worker also failed to join ({join_error})"
                        ),
                    ));
                }
                return Err(load_error);
            }
            Err(readiness_error) => {
                let load_error = NativeError::new(
                    NativeErrorCode::ModelLoadFailed,
                    format!("native model worker did not become ready: {readiness_error}"),
                );
                if let Err(join_error) = bootstrap.shutdown_and_join() {
                    return Err(NativeError::new(
                        NativeErrorCode::WorkerStopped,
                        format!(
                            "{load_error}; its bootstrap worker also failed to join ({join_error})"
                        ),
                    ));
                }
                return Err(load_error);
            }
        }
        let (command_tx, shutdown_tx, worker) = bootstrap.into_parts()?;
        Ok((
            Arc::new(NativeModelInner {
                worker_identity,
                worker_id,
                command_tx,
                shutdown_tx,
                closing: AtomicBool::new(false),
                admission: Mutex::new(()),
                requests,
                status,
            }),
            worker,
        ))
    }

    pub fn status(&self) -> ResidentModelStatus {
        self.inner
            .status
            .read()
            .map(|status| status.clone())
            .unwrap_or_else(|_| ResidentModelStatus {
                model_id: "unknown".to_string(),
                model_path: std::path::PathBuf::new(),
                state: ModelRuntimeState::Failed,
                fingerprint: None,
                descriptor: None,
                active_sequences: 0,
                max_sequences: 0,
            })
    }

    /// Returns true when both handles address the same resident owner-thread worker.
    ///
    /// This is intentionally narrower than comparing model IDs: two separately loaded
    /// workers may legitimately expose the same model fingerprint, while repeated
    /// requests for one resident profile must reuse the same worker allocation.
    pub fn is_same_worker(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// Submit exact token IDs for in-process embedding on this model's owner
    /// worker. No text tokenization or generation-context mutation occurs.
    pub fn embed_batch(&self, request: EmbeddingBatchRequest) -> NativeResult<EmbeddingTicket> {
        validate_embedding_batch_request(&request, &self.status())?;
        let cancellation = Arc::new(AtomicBool::new(false));
        let (result_tx, result_rx) = bounded(1);
        let request_id = request.request_id().to_string();
        let admitted_request_sha256 = embedding_runtime::embedding_request_sha256(&request);
        let control = self.inner.admit_command(
            request_id.clone(),
            RequestClass::Embedding,
            RequestControls::Embedding {
                cancellation: Arc::clone(&cancellation),
            },
            |request_lease| WorkerCommand::EmbedBatch {
                request,
                admitted_request_sha256,
                result: result_tx,
                cancellation: Arc::clone(&cancellation),
                request_lease,
            },
            "submitting embeddings",
        )?;
        Ok(EmbeddingTicket {
            request_id,
            result: result_rx,
            control,
        })
    }

    pub fn generate(&self, request: GenerationRequest) -> NativeResult<GenerationTicket> {
        if matches!(&request.input, GenerationInput::Chat { .. }) && !request.media.is_empty() {
            return self.generate_multimodal(request);
        }
        if !request.media.is_empty() {
            return Err(NativeError::new(
                NativeErrorCode::UnsupportedMedia,
                "media inputs require a chat generation request",
            ));
        }
        let GenerationRequest {
            request_id,
            model_id,
            input,
            sampling,
            media: _,
            cached_prefix,
        } = request;
        let cases = match input {
            GenerationInput::Chat { messages, template } => vec![GenerationCase {
                case_id: "assistant".to_string(),
                input: GenerationInput::Chat { messages, template },
                sampling,
                cached_prefix,
            }],
            GenerationInput::Completion { prompts } => {
                if cached_prefix.is_some() && prompts.len() != 1 {
                    return Err(NativeError::new(
                        NativeErrorCode::InvalidConfig,
                        "a request-level cached prefix can only be used with one completion prompt",
                    ));
                }
                prompts
                    .into_iter()
                    .enumerate()
                    .map(|(index, prompt)| GenerationCase {
                        case_id: format!("completion-{index}"),
                        input: GenerationInput::Completion {
                            prompts: vec![prompt],
                        },
                        sampling: sampling.clone(),
                        cached_prefix: cached_prefix.clone(),
                    })
                    .collect()
            }
            GenerationInput::FillInMiddle { .. } => {
                return Err(NativeError::new(
                    NativeErrorCode::UnsupportedPromptForm,
                    "fill-in-middle generation is unavailable because this model has no verified FIM token contract",
                ));
            }
        };
        self.submit_generation_batch(
            GenerationBatchRequest {
                request_id,
                model_id,
                cases,
            },
            GenerationBatchAdmission::Compatibility,
        )
    }

    /// Submit an ordered family of independently sampled raw generation cases.
    pub fn generate_batch(
        &self,
        request: GenerationBatchRequest,
    ) -> NativeResult<GenerationTicket> {
        self.submit_generation_batch(request, GenerationBatchAdmission::ExactBatch)
    }

    fn submit_generation_batch(
        &self,
        request: GenerationBatchRequest,
        admission: GenerationBatchAdmission,
    ) -> NativeResult<GenerationTicket> {
        let status = self.status();
        validate_generation_batch_request(&request, &status)?;
        let exact_cell_budget = exact_token_budget_for_submission(&request, &status)?;
        let mut cancellations = Vec::with_capacity(request.cases.len());
        let mut reasoning_forces = Vec::with_capacity(request.cases.len());
        for _ in &request.cases {
            cancellations.push(Arc::new(AtomicBool::new(false)));
            reasoning_forces.push(Arc::new(AtomicBool::new(false)));
        }
        let (event_tx, event_rx) = bounded(EVENT_CAPACITY);
        let (result_tx, result_rx) = bounded(1);
        let request_id = request.request_id.clone();
        let owned_cancellations = request
            .cases
            .iter()
            .zip(&cancellations)
            .map(|(case, flag)| (case.case_id.clone(), Arc::clone(flag)))
            .collect();
        let owned_reasoning_forces = request
            .cases
            .iter()
            .zip(&reasoning_forces)
            .map(|(case, flag)| (case.case_id.clone(), Arc::clone(flag)))
            .collect();
        let control = self.inner.admit_command(
            request_id.clone(),
            RequestClass::Generation,
            RequestControls::Generation {
                cancellations: owned_cancellations,
                reasoning_forces: owned_reasoning_forces,
            },
            |request_lease| WorkerCommand::GenerateBatch {
                request,
                exact_cell_budget,
                admission,
                event_tx,
                result_tx,
                cancellations,
                reasoning_forces,
                request_lease,
            },
            "submitting an exact generation batch",
        )?;
        Ok(GenerationTicket {
            request_id,
            events: event_rx,
            result: result_rx,
            control,
        })
    }

    fn generate_multimodal(&self, request: GenerationRequest) -> NativeResult<GenerationTicket> {
        validate_generation_request(&request, &self.status())?;
        let cancellation = Arc::new(AtomicBool::new(false));
        let reasoning_force = Arc::new(AtomicBool::new(false));
        let (event_tx, event_rx) = bounded(EVENT_CAPACITY);
        let (result_tx, result_rx) = bounded(1);
        let request_id = request.request_id.clone();
        let owned_cancellations = vec![("assistant".to_owned(), Arc::clone(&cancellation))];
        let owned_reasoning_forces = vec![("assistant".to_owned(), Arc::clone(&reasoning_force))];
        let control = self.inner.admit_command(
            request_id.clone(),
            RequestClass::Generation,
            RequestControls::Generation {
                cancellations: owned_cancellations,
                reasoning_forces: owned_reasoning_forces,
            },
            |request_lease| WorkerCommand::GenerateMultimodal {
                request,
                event_tx,
                result_tx,
                cancellation,
                reasoning_force,
                request_lease,
            },
            "submitting multimodal generation",
        )?;
        Ok(GenerationTicket {
            request_id,
            events: event_rx,
            result: result_rx,
            control,
        })
    }

    pub fn generate_shared_prefix(
        &self,
        request: SharedPrefixBatchRequest,
    ) -> NativeResult<GenerationTicket> {
        validate_batch_request(&request, &self.status())?;
        let mut flags = Vec::with_capacity(request.branches.len());
        let mut reasoning_flags = Vec::with_capacity(request.branches.len());
        for _ in &request.branches {
            flags.push(Arc::new(AtomicBool::new(false)));
            reasoning_flags.push(Arc::new(AtomicBool::new(false)));
        }
        let (event_tx, event_rx) = bounded(EVENT_CAPACITY);
        let (result_tx, result_rx) = bounded(1);
        let request_id = request.request_id.clone();
        let owned_cancellations = request
            .branches
            .iter()
            .zip(&flags)
            .map(|(branch, flag)| (branch.branch_id.clone(), Arc::clone(flag)))
            .collect();
        let owned_reasoning_forces = request
            .branches
            .iter()
            .zip(&reasoning_flags)
            .map(|(branch, flag)| (branch.branch_id.clone(), Arc::clone(flag)))
            .collect();
        let control = self.inner.admit_command(
            request_id.clone(),
            RequestClass::Generation,
            RequestControls::Generation {
                cancellations: owned_cancellations,
                reasoning_forces: owned_reasoning_forces,
            },
            |request_lease| WorkerCommand::Generate {
                request,
                event_tx,
                result_tx,
                cancellations: flags,
                reasoning_forces: reasoning_flags,
                request_lease,
            },
            "submitting shared-prefix generation",
        )?;
        Ok(GenerationTicket {
            request_id,
            events: event_rx,
            result: result_rx,
            control,
        })
    }

    pub fn cancel(&self, request_id: &str, branch_id: Option<&str>) -> usize {
        let Some(active) = self.inner.requests.active(request_id) else {
            return 0;
        };
        match (active.class(), branch_id) {
            (RequestClass::Generation | RequestClass::ControlledGeneration, Some(branch_id)) => {
                usize::from(active.cancel_named(branch_id))
            }
            (RequestClass::Generation | RequestClass::ControlledGeneration, None) => {
                active.cancel_all()
            }
            (RequestClass::Embedding, None) => active.cancel_all(),
            (RequestClass::Embedding, Some(_)) => 0,
        }
    }

    pub fn skip_reasoning(&self, request_id: &str, branch_id: Option<&str>) -> usize {
        let Some(active) = self.inner.requests.active(request_id) else {
            return 0;
        };
        if active.class() != RequestClass::Generation {
            return 0;
        }
        match branch_id {
            Some(branch_id) => usize::from(active.force_reasoning_exit(branch_id)),
            None => active.force_all_reasoning_exits(),
        }
    }

    pub fn snapshot_sequence(&self, sequence_id: i32) -> NativeResult<SequenceStateBlob> {
        self.inner.ensure_accepting()?;
        let (response_tx, response_rx) = bounded(1);
        self.inner.send_command(
            WorkerCommand::Snapshot {
                sequence_id,
                response: response_tx,
            },
            "submitting a sequence snapshot",
        )?;
        response_rx.recv().map_err(|error| {
            NativeError::new(
                NativeErrorCode::WorkerStopped,
                format!("native model worker dropped the snapshot response: {error}"),
            )
        })?
    }

    pub fn restore_sequence(
        &self,
        state: SequenceStateBlob,
        destination_sequence_id: i32,
    ) -> NativeResult<()> {
        self.inner.ensure_accepting()?;
        let (response_tx, response_rx) = bounded(1);
        self.inner.send_command(
            WorkerCommand::Restore {
                state,
                destination_sequence_id,
                response: response_tx,
            },
            "submitting a sequence restore",
        )?;
        response_rx.recv().map_err(|error| {
            NativeError::new(
                NativeErrorCode::WorkerStopped,
                format!("native model worker dropped the restore response: {error}"),
            )
        })?
    }

    pub fn prefill_shared_prefix(
        &self,
        request: SharedPrefixBatchRequest,
    ) -> NativeResult<SequenceStateBlob> {
        self.inner.ensure_accepting()?;
        validate_batch_request(&request, &self.status())?;
        if request.branches.len() < 2 {
            return Err(NativeError::new(
                NativeErrorCode::InvalidConfig,
                "prefix prefill requires two probe branches",
            ));
        }
        let (response_tx, response_rx) = bounded(1);
        self.inner.send_command(
            WorkerCommand::PrefillPrefix {
                request,
                response: response_tx,
            },
            "submitting prefix prefill",
        )?;
        response_rx.recv().map_err(|error| {
            NativeError::new(
                NativeErrorCode::WorkerStopped,
                format!("native model worker dropped the prefill response: {error}"),
            )
        })?
    }

    pub fn tokenize_messages(&self, messages: Vec<ChatMessage>) -> NativeResult<TokenizedPrompt> {
        self.tokenize_messages_with_template(messages, ChatTemplateChoice::ModelDefault)
    }

    pub fn tokenize_messages_with_template(
        &self,
        messages: Vec<ChatMessage>,
        chat_template: ChatTemplateChoice,
    ) -> NativeResult<TokenizedPrompt> {
        self.inner.ensure_accepting()?;
        let (response_tx, response_rx) = bounded(1);
        self.inner.send_command(
            WorkerCommand::Tokenize {
                messages,
                chat_template,
                response: response_tx,
            },
            "submitting tokenization",
        )?;
        response_rx.recv().map_err(|error| {
            NativeError::new(
                NativeErrorCode::WorkerStopped,
                format!("native model worker stopped during tokenization: {error}"),
            )
        })?
    }

    pub fn prepare_input(&self, input: GenerationInput) -> NativeResult<Vec<PreparedPrompt>> {
        self.inner.ensure_accepting()?;
        let (response_tx, response_rx) = bounded(1);
        self.inner.send_command(
            WorkerCommand::PrepareInput {
                input,
                response: response_tx,
            },
            "submitting prompt preparation",
        )?;
        response_rx.recv().map_err(|error| {
            NativeError::new(
                NativeErrorCode::WorkerStopped,
                format!("native model worker stopped during prompt preparation: {error}"),
            )
        })?
    }
}

impl ModelArtifactGuard {
    fn open(
        path: &std::path::Path,
        label: &'static str,
        caller_expected_sha256: Option<&str>,
    ) -> NativeResult<Self> {
        let file = open_guarded_artifact(path).map_err(|error| {
            NativeError::new(
                NativeErrorCode::ModelMissing,
                format!("failed to open {label} {}: {error}", path.display()),
            )
        })?;
        let metadata = file.metadata().map_err(|error| {
            NativeError::new(
                NativeErrorCode::ModelInvalid,
                format!("failed to inspect {label} {}: {error}", path.display()),
            )
        })?;
        if !metadata.is_file() {
            return Err(NativeError::new(
                NativeErrorCode::ModelInvalid,
                format!("{label} is not a regular file: {}", path.display()),
            ));
        }
        let initial_state = artifact_file_state(&metadata);
        let path_state = artifact_path_state(path, label)?;
        if path_state != initial_state {
            return Err(artifact_changed_error(
                label,
                path,
                "path identity changed while opening the artifact",
            ));
        }

        #[cfg(windows)]
        let lock_error = {
            // The deny-write/delete share mode is mandatory and stronger than
            // an advisory range lock. Take the latter when available, but do
            // not make strict authority depend on duplicate lock semantics.
            let _ = Fs4FileExt::try_lock_shared(&file);
            None
        };
        #[cfg(not(windows))]
        let lock_error = Fs4FileExt::try_lock_shared(&file)
            .err()
            .map(|error| format!("cooperative shared lock unavailable: {error}"));
        #[cfg(all(test, unix))]
        let payload_bytes_read = std::sync::atomic::AtomicU64::new(0);
        #[cfg(all(test, unix))]
        let expected_sha256 = hash_open_artifact(&file, label, path, &payload_bytes_read)?;
        #[cfg(not(all(test, unix)))]
        let expected_sha256 = hash_open_artifact(&file, label, path)?;
        let state_after_hash = file
            .metadata()
            .map(|value| artifact_file_state(&value))
            .map_err(|error| {
                NativeError::new(
                    NativeErrorCode::ModelInvalid,
                    format!("failed to re-inspect {label} {}: {error}", path.display()),
                )
            })?;
        if state_after_hash != initial_state || artifact_path_state(path, label)? != initial_state {
            return Err(artifact_changed_error(
                label,
                path,
                "identity or metadata changed while computing the initial SHA-256",
            ));
        }
        if let Some(expected) = caller_expected_sha256
            && expected != expected_sha256
        {
            return Err(NativeError::new(
                NativeErrorCode::ModelInvalid,
                format!(
                    "{label} {} has SHA-256 {expected_sha256}, expected {expected}",
                    path.display()
                ),
            ));
        }

        let pinned_load_path = pinned_artifact_load_path(&file, &initial_state, path);
        let mut strict_errors = Vec::with_capacity(2);
        if let Some(error) = lock_error {
            strict_errors.push(error);
        }
        if pinned_load_path.is_none() {
            strict_errors.push(
                "this platform cannot make llama.cpp reopen the held artifact handle".to_string(),
            );
        }
        let strict_binding_error = (!strict_errors.is_empty()).then(|| strict_errors.join("; "));

        Ok(Self {
            label,
            original_path: path.to_path_buf(),
            load_path: pinned_load_path.unwrap_or_else(|| path.to_path_buf()),
            file,
            initial_state,
            expected_sha256,
            #[cfg(all(test, unix))]
            payload_bytes_read,
            strict_binding_error,
        })
    }

    fn verify_identity(&self) -> NativeResult<()> {
        let held_state = self.file.metadata().map_err(|error| {
            NativeError::new(
                NativeErrorCode::ModelInvalid,
                format!(
                    "failed to re-inspect held {} {}: {error}",
                    self.label,
                    self.original_path.display()
                ),
            )
        })?;
        if artifact_file_state(&held_state) != self.initial_state {
            return Err(artifact_changed_error(
                self.label,
                &self.original_path,
                "held file identity or metadata changed",
            ));
        }
        if artifact_path_state(&self.original_path, self.label)? != self.initial_state {
            return Err(artifact_changed_error(
                self.label,
                &self.original_path,
                "configured path no longer names the held file",
            ));
        }
        Ok(())
    }

    fn verify_strict_unchanged(&self) -> NativeResult<()> {
        if let Some(error) = &self.strict_binding_error {
            return Err(NativeError::new(
                NativeErrorCode::ModelInvalid,
                format!(
                    "{} {} cannot support strict generation authority: {error}",
                    self.label,
                    self.original_path.display()
                ),
            ));
        }
        self.verify_identity()
    }

    #[cfg(all(test, unix))]
    fn payload_bytes_read(&self) -> u64 {
        self.payload_bytes_read.load(Ordering::Relaxed)
    }
}

impl ModelArtifactGuards {
    fn open(config: &NativeModelConfig) -> NativeResult<Self> {
        let model = ModelArtifactGuard::open(
            &config.model_path,
            "model",
            config.expected_model_sha256.as_deref(),
        )?;
        let projector = config
            .mmproj_path
            .as_deref()
            .map(|path| {
                ModelArtifactGuard::open(
                    path,
                    "multimodal projector",
                    config.expected_mmproj_sha256.as_deref(),
                )
            })
            .transpose()?;
        Ok(Self { model, projector })
    }

    fn verify_loaded_identities(&self) -> NativeResult<()> {
        self.model.verify_identity()?;
        if let Some(projector) = &self.projector {
            projector.verify_identity()?;
        }
        Ok(())
    }

    fn verify_strict_unchanged(&self, fingerprint: &ModelFingerprint) -> NativeResult<()> {
        if fingerprint.model_sha256 != self.model.expected_sha256 {
            return Err(generation_verification_error(
                "resident fingerprint no longer matches the held model artifact",
            ));
        }
        match (&self.projector, &fingerprint.multimodal_projector_sha256) {
            (Some(projector), Some(fingerprint_sha256))
                if fingerprint_sha256 == &projector.expected_sha256 => {}
            (None, None) => {}
            _ => {
                return Err(generation_verification_error(
                    "resident fingerprint no longer matches the held projector artifact",
                ));
            }
        }
        self.model.verify_strict_unchanged()?;
        if let Some(projector) = &self.projector {
            projector.verify_strict_unchanged()?;
        }
        // Recheck the first artifact after checking the second, narrowing the
        // cross-artifact mutation window without pretending filesystem atomicity.
        self.model.verify_identity()?;
        Ok(())
    }
}

#[cfg(unix)]
fn artifact_file_state(metadata: &Metadata) -> ArtifactFileState {
    use std::os::unix::fs::MetadataExt as _;
    ArtifactFileState {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    }
}

#[cfg(windows)]
fn artifact_file_state(metadata: &Metadata) -> ArtifactFileState {
    use std::os::windows::fs::MetadataExt as _;
    ArtifactFileState {
        length: metadata.file_size(),
        created: metadata.creation_time(),
        modified: metadata.last_write_time(),
        attributes: metadata.file_attributes(),
    }
}

#[cfg(not(any(unix, windows)))]
fn artifact_file_state(metadata: &Metadata) -> ArtifactFileState {
    ArtifactFileState {
        length: metadata.len(),
        modified: metadata.modified().ok(),
        readonly: metadata.permissions().readonly(),
    }
}

fn artifact_path_state(path: &std::path::Path, label: &str) -> NativeResult<ArtifactFileState> {
    std::fs::metadata(path)
        .map(|metadata| artifact_file_state(&metadata))
        .map_err(|error| {
            NativeError::new(
                NativeErrorCode::ModelInvalid,
                format!("failed to inspect {label} path {}: {error}", path.display()),
            )
        })
}

#[cfg(windows)]
fn open_guarded_artifact(path: &std::path::Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
}

#[cfg(not(windows))]
fn open_guarded_artifact(path: &std::path::Path) -> std::io::Result<File> {
    File::open(path)
}

#[cfg(unix)]
fn pinned_artifact_load_path(
    file: &File,
    expected_state: &ArtifactFileState,
    _original_path: &std::path::Path,
) -> Option<std::path::PathBuf> {
    use std::os::fd::AsRawFd as _;
    ["/dev/fd", "/proc/self/fd"].into_iter().find_map(|root| {
        let candidate = std::path::PathBuf::from(root).join(file.as_raw_fd().to_string());
        let probe = File::open(&candidate).ok()?;
        (artifact_file_state(&probe.metadata().ok()?) == *expected_state).then_some(candidate)
    })
}

#[cfg(windows)]
fn pinned_artifact_load_path(
    _file: &File,
    _expected_state: &ArtifactFileState,
    _original_path: &std::path::Path,
) -> Option<std::path::PathBuf> {
    // Deny-write/delete sharing improves legacy load stability, but a caller
    // path is not a handle-derived reopen path and the metadata above lacks a
    // volume/file identity. Keep legacy loading available while refusing to
    // mint strict authority until both properties are implemented and tested.
    None
}

#[cfg(not(any(unix, windows)))]
fn pinned_artifact_load_path(
    _file: &File,
    _expected_state: &ArtifactFileState,
    _original_path: &std::path::Path,
) -> Option<std::path::PathBuf> {
    None
}

#[cfg(unix)]
fn read_artifact_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<usize> {
    use std::os::unix::fs::FileExt as _;
    file.read_at(buffer, offset)
}

#[cfg(windows)]
fn read_artifact_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<usize> {
    use std::os::windows::fs::FileExt as _;
    file.seek_read(buffer, offset)
}

#[cfg(not(any(unix, windows)))]
fn read_artifact_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<usize> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut reader = file.try_clone()?;
    reader.seek(SeekFrom::Start(offset))?;
    reader.read(buffer)
}

#[cfg(not(all(test, unix)))]
fn hash_open_artifact(file: &File, label: &str, path: &std::path::Path) -> NativeResult<String> {
    hash_open_artifact_with_read_observer(file, label, path, |_| {})
}

#[cfg(all(test, unix))]
fn hash_open_artifact(
    file: &File,
    label: &str,
    path: &std::path::Path,
    payload_bytes_read: &std::sync::atomic::AtomicU64,
) -> NativeResult<String> {
    hash_open_artifact_with_read_observer(file, label, path, |read| {
        payload_bytes_read.fetch_add(read as u64, Ordering::Relaxed);
    })
}

fn hash_open_artifact_with_read_observer(
    file: &File,
    label: &str,
    path: &std::path::Path,
    mut observe_read: impl FnMut(usize),
) -> NativeResult<String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut offset = 0_u64;
    loop {
        let read = read_artifact_at(file, &mut buffer, offset).map_err(|error| {
            NativeError::new(
                NativeErrorCode::ModelInvalid,
                format!("failed while hashing {label} {}: {error}", path.display()),
            )
        })?;
        if read == 0 {
            break;
        }
        observe_read(read);
        hasher.update(&buffer[..read]);
        offset = offset.checked_add(read as u64).ok_or_else(|| {
            NativeError::new(
                NativeErrorCode::ModelInvalid,
                format!("{label} {} is too large to hash safely", path.display()),
            )
        })?;
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn artifact_changed_error(label: &str, path: &std::path::Path, detail: &str) -> NativeError {
    NativeError::new(
        NativeErrorCode::ModelInvalid,
        format!(
            "{label} {} changed across the trusted load/generation boundary: {detail}",
            path.display()
        ),
    )
}

fn run_worker(
    config: NativeModelConfig,
    command_rx: Receiver<WorkerCommand>,
    shutdown_rx: Receiver<()>,
    ready_tx: Sender<NativeResult<()>>,
    status: Arc<RwLock<ResidentModelStatus>>,
    worker_identity: Arc<WorkerIdentity>,
) {
    let backend = match backend() {
        Ok(backend) => backend,
        Err(error) => {
            set_status_state(&status, ModelRuntimeState::Failed, 0);
            let _ = ready_tx.send(Err(error));
            return;
        }
    };
    let configured_gpu_layers = if config.gpu_layers < 0 {
        u32::MAX
    } else {
        config.gpu_layers as u32
    };
    let gpu_layers = match config.device {
        NativeDevice::Cpu => 0,
        NativeDevice::Auto | NativeDevice::Metal if backend.supports_gpu_offload() => {
            configured_gpu_layers
        }
        NativeDevice::Metal => {
            let error = NativeError::new(
                NativeErrorCode::InvalidConfig,
                "Metal was requested but this build has no GPU offload backend",
            );
            set_status_state(&status, ModelRuntimeState::Failed, 0);
            let _ = ready_tx.send(Err(error));
            return;
        }
        NativeDevice::Auto => 0,
    };
    let execution_backend = if gpu_layers == 0 {
        "cpu"
    } else if backend.supports_gpu_offload() {
        "metal"
    } else {
        "cpu"
    };
    let mut model_params = LlamaModelParams::default().with_n_gpu_layers(gpu_layers);
    if config.device == NativeDevice::Cpu {
        model_params = match model_params.with_devices(&[]) {
            Ok(model_params) => model_params,
            Err(error) => {
                let error = NativeError::new(
                    NativeErrorCode::InvalidConfig,
                    format!("failed to configure the CPU-only native backend: {error}"),
                );
                set_status_state(&status, ModelRuntimeState::Failed, 0);
                let _ = ready_tx.send(Err(error));
                return;
            }
        };
    }
    let artifacts = match ModelArtifactGuards::open(&config) {
        Ok(artifacts) => artifacts,
        Err(error) => {
            set_status_state(&status, ModelRuntimeState::Failed, 0);
            let _ = ready_tx.send(Err(error));
            return;
        }
    };
    let model = match LlamaModel::load_from_file(backend, &artifacts.model.load_path, &model_params)
    {
        Ok(model) => model,
        Err(error) => {
            let error = NativeError::new(
                NativeErrorCode::ModelLoadFailed,
                format!("failed to load {}: {error}", config.model_path.display()),
            );
            set_status_state(&status, ModelRuntimeState::Failed, 0);
            let _ = ready_tx.send(Err(error));
            return;
        }
    };
    let multimodal = match artifacts.projector.as_ref() {
        Some(artifact) => {
            let Some(load_path) = artifact.load_path.to_str() else {
                let error = NativeError::new(
                    NativeErrorCode::InvalidConfig,
                    "pinned multimodal projector path is not valid UTF-8",
                );
                set_status_state(&status, ModelRuntimeState::Failed, 0);
                let _ = ready_tx.send(Err(error));
                return;
            };
            let params = MtmdContextParams {
                use_gpu: config.device != NativeDevice::Cpu,
                n_threads: native_thread_count(),
                ..MtmdContextParams::default()
            };
            match MtmdContext::init_from_file(load_path, &model, &params) {
                Ok(context) => Some(context),
                Err(error) => {
                    let error = NativeError::new(
                        NativeErrorCode::ModelLoadFailed,
                        format!(
                            "failed to load multimodal projector {}: {error}",
                            artifact.original_path.display()
                        ),
                    );
                    set_status_state(&status, ModelRuntimeState::Failed, 0);
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            }
        }
        None => None,
    };
    if let Err(error) = artifacts.verify_loaded_identities() {
        set_status_state(&status, ModelRuntimeState::Failed, 0);
        let _ = ready_tx.send(Err(error));
        return;
    }
    let context_tokens = config.context_tokens.min(model.n_ctx_train()).max(512);
    let context_params = generation_context_params(&config, context_tokens);
    let (rope_config_sha256, kv_layout_sha256) = context_fingerprints(&context_params);
    let mut context = match model.new_context(backend, context_params) {
        Ok(context) => context,
        Err(error) => {
            let error = NativeError::new(
                NativeErrorCode::ContextCreateFailed,
                format!("failed to create llama.cpp context: {error}"),
            );
            set_status_state(&status, ModelRuntimeState::Failed, 0);
            let _ = ready_tx.send(Err(error));
            return;
        }
    };
    let fingerprint = match fingerprint_model(
        &config,
        &model,
        &artifacts,
        execution_backend,
        context_tokens,
        rope_config_sha256,
        kv_layout_sha256,
    ) {
        Ok(fingerprint) => fingerprint,
        Err(error) => {
            set_status_state(&status, ModelRuntimeState::Failed, 0);
            let _ = ready_tx.send(Err(error));
            return;
        }
    };
    if let Ok(mut current) = status.write() {
        let media_kinds = multimodal
            .as_ref()
            .map_or_else(Vec::new, media_kinds_for_context);
        current.state = ModelRuntimeState::Ready;
        current.descriptor = Some(describe_model(&config, &model, &fingerprint, media_kinds));
        current.fingerprint = Some(fingerprint.clone());
    }
    let _ = ready_tx.send(Ok(()));
    let mut sequence_token_counts = HashMap::<i32, usize>::new();
    let mut sequence_token_ids = HashMap::<i32, Vec<i32>>::new();
    let mut controlled_token_contract = None;
    let mut embedding_call_sequence = 0_u64;
    let mut controlled_call_sequence = 0_u64;
    loop {
        let command = crossbeam_channel::select_biased! {
            recv(shutdown_rx) -> _ => {
                reject_queued_commands(&command_rx);
                break;
            },
            recv(command_rx) -> command => match command {
                Ok(command) => command,
                Err(_) => break,
            },
        };
        match command {
            WorkerCommand::EmbedBatch {
                request,
                admitted_request_sha256,
                result,
                cancellation,
                request_lease,
            } => {
                if let Err(error) = request_lease.running() {
                    let _ = result.send(embedding_runtime::EmbeddingCompletion::failed(error));
                    continue;
                }
                let _ = request_lease.progress(0);
                set_status_state(&status, ModelRuntimeState::Ready, 1);
                let owner_call_sequence = embedding_call_sequence;
                let Some(next_call_sequence) = embedding_call_sequence.checked_add(1) else {
                    let _ = result.send(embedding_runtime::EmbeddingCompletion::failed(
                        NativeError::new(
                            NativeErrorCode::Internal,
                            "native embedding owner call sequence overflowed",
                        ),
                    ));
                    set_status_state(&status, ModelRuntimeState::Ready, 0);
                    let _ = request_lease.completed_or_failed(false);
                    continue;
                };
                embedding_call_sequence = next_call_sequence;
                let strict_precheck = artifacts.verify_strict_unchanged(&fingerprint);
                let embedding_result = execute_embedding_batch(
                    &config,
                    backend,
                    &model,
                    &fingerprint,
                    context_tokens,
                    &request,
                    &cancellation,
                );
                let completion_succeeded = embedding_result.is_ok();
                let completion = match embedding_result {
                    Ok(output) => {
                        let captured_output_bits_sha256 =
                            embedding_runtime::embedding_output_bits_sha256(&output);
                        let authority = strict_precheck.and_then(|()| {
                            let maximum_input_tokens = request
                                .inputs()
                                .iter()
                                .map(|input| input.token_ids().len())
                                .max()
                                .unwrap_or_default();
                            let maximum_input_tokens = u32::try_from(maximum_input_tokens)
                                .map_err(|_| {
                                    NativeError::new(
                                        NativeErrorCode::Internal,
                                        "verified embedding token count does not fit u32",
                                    )
                                })?;
                            let params = embedding_context_params(
                                &config,
                                context_tokens,
                                maximum_input_tokens,
                                request.pooling(),
                            );
                            let expected_execution_fingerprint =
                                embedding_execution_fingerprint(&fingerprint, &params);
                            let expected_dimensions =
                                u32::try_from(embedding_output_width(&model, request.pooling())?)
                                    .map_err(|_| {
                                    NativeError::new(
                                        NativeErrorCode::ModelInvalid,
                                        "verified embedding dimensions do not fit u32",
                                    )
                                })?;
                            embedding_runtime::verify_embedding_batch_authority(
                                request,
                                &admitted_request_sha256,
                                fingerprint.clone(),
                                expected_execution_fingerprint,
                                expected_dimensions,
                                &output,
                                &captured_output_bits_sha256,
                                owner_call_sequence,
                                &[embedding_runtime::EmbeddingCompletionTerminal::Completed],
                                &cancellation,
                                Arc::clone(&worker_identity),
                                &artifacts,
                            )
                        });
                        if authority.is_ok() {
                            record_embedding_capability(&status, &output, context_tokens);
                        }
                        embedding_runtime::EmbeddingCompletion::completed(output, authority)
                    }
                    Err(error) => embedding_runtime::EmbeddingCompletion::failed(error),
                };
                set_status_state(&status, ModelRuntimeState::Ready, 0);
                let _ = request_lease.completed_or_failed(completion_succeeded);
                let _ = result.send(completion);
            }
            WorkerCommand::GenerateBatch {
                request,
                exact_cell_budget,
                admission,
                event_tx,
                result_tx,
                cancellations,
                reasoning_forces,
                request_lease,
            } => {
                if let Err(error) = request_lease.running() {
                    let _ = result_tx.send(Err(error));
                    continue;
                }
                let _ = request_lease.progress(0);
                set_status_state(&status, ModelRuntimeState::Ready, request.cases.len());
                let statically_sealable =
                    is_statically_sealable_generation_batch(&request, admission);
                let strict_precheck = if statically_sealable {
                    artifacts.verify_strict_unchanged(&fingerprint)
                } else {
                    Ok(())
                };
                let retain_authority_evidence =
                    should_retain_authority_evidence(statically_sealable, &strict_precheck);
                let mut retained_events = retain_authority_evidence.then(Vec::new);
                let mut unrecorded_control_used = false;
                // Authority admission is deliberately orthogonal to legacy
                // generation. A failed strict precheck must not suppress valid
                // model output requested through `generate_batch(...).wait()`.
                let generated = prepare_generation_batch(&model, &request).and_then(
                    |(normalized, token_sets)| {
                        generate_batch(
                            &model,
                            &mut context,
                            &normalized,
                            Some(token_sets),
                            exact_cell_budget.as_ref(),
                            BatchSupervision {
                                event_tx: &event_tx,
                                retained_events: retained_events.as_mut(),
                                retain_token_piece_traces: retain_authority_evidence,
                                unrecorded_control_used: retain_authority_evidence
                                    .then_some(&mut unrecorded_control_used),
                                runtime_sample_trace: None,
                                cancellations: &cancellations,
                                reasoning_forces: &reasoning_forces,
                            },
                            SequenceTracking {
                                token_counts: &mut sequence_token_counts,
                                token_ids: &mut sequence_token_ids,
                            },
                        )
                    },
                );
                if generated.is_err() {
                    emit_failed_case_events(&event_tx, &request);
                }
                if retain_authority_evidence {
                    unrecorded_control_used |= reasoning_forces
                        .iter()
                        .any(|flag| flag.load(Ordering::Acquire));
                }
                let sealable_batch = retain_authority_evidence
                    && is_sealable_generation_batch(&request, admission, unrecorded_control_used);
                let result = generated.map(|execution| {
                    let GeneratedBatchExecution {
                        outputs,
                        terminal_sampled_token_ids,
                        token_piece_traces,
                    } = execution;
                    let authority = strict_precheck.and_then(|()| {
                        if !sealable_batch {
                            return Err(NativeError::new(
                                NativeErrorCode::UnsupportedParameter,
                                "this generation path does not carry exact-token owner-worker authority",
                            ));
                        }
                        let events = retained_events.take().ok_or_else(|| {
                            generation_verification_error(
                                "statically sealable generation did not retain its event ledger",
                            )
                        })?;
                        // Validate while the outputs are borrowed, then move
                        // them once into the completion envelope. Legacy waits
                        // never pay for a defensive output clone.
                        verify_generation_batch_authority(
                            &model,
                            request,
                            fingerprint.clone(),
                            &outputs,
                            GenerationAuthorityCapture {
                                terminal_sampled_token_ids,
                                events,
                                token_piece_traces,
                            },
                            &artifacts,
                        )
                    });
                    match authority {
                        Ok(evidence) => GenerationCompletion::verified(outputs, evidence),
                        Err(error) => GenerationCompletion::authority_rejected(outputs, error),
                    }
                });
                set_status_state(&status, ModelRuntimeState::Ready, 0);
                let _ = request_lease.completed_or_failed(result.is_ok());
                let _ = result_tx.send(result);
            }
            WorkerCommand::Generate {
                request,
                event_tx,
                result_tx,
                cancellations,
                reasoning_forces,
                request_lease,
            } => {
                if let Err(error) = request_lease.running() {
                    let _ = result_tx.send(Err(error));
                    continue;
                }
                let _ = request_lease.progress(0);
                set_status_state(&status, ModelRuntimeState::Ready, request.branches.len());
                let result = generate_batch(
                    &model,
                    &mut context,
                    &request,
                    None,
                    None,
                    BatchSupervision {
                        event_tx: &event_tx,
                        retained_events: None,
                        retain_token_piece_traces: false,
                        unrecorded_control_used: None,
                        runtime_sample_trace: None,
                        cancellations: &cancellations,
                        reasoning_forces: &reasoning_forces,
                    },
                    SequenceTracking {
                        token_counts: &mut sequence_token_counts,
                        token_ids: &mut sequence_token_ids,
                    },
                );
                if result.is_err() {
                    emit_failed_branch_events(&event_tx, &request);
                }
                set_status_state(&status, ModelRuntimeState::Ready, 0);
                let _ = request_lease.completed_or_failed(result.is_ok());
                let _ = result_tx.send(
                    result.map(|execution| GenerationCompletion::unverified(execution.outputs)),
                );
            }
            WorkerCommand::GenerateMultimodal {
                request,
                event_tx,
                result_tx,
                cancellation,
                reasoning_force,
                request_lease,
            } => {
                if let Err(error) = request_lease.running() {
                    let _ = result_tx.send(Err(error));
                    continue;
                }
                let _ = request_lease.progress(0);
                set_status_state(&status, ModelRuntimeState::Ready, 1);
                let result = generate_multimodal(
                    &model,
                    &mut context,
                    multimodal.as_ref(),
                    &request,
                    SingleSequenceSupervision {
                        event_tx: &event_tx,
                        cancellation: &cancellation,
                        reasoning_force: &reasoning_force,
                    },
                    SequenceTracking {
                        token_counts: &mut sequence_token_counts,
                        token_ids: &mut sequence_token_ids,
                    },
                );
                if result.is_err() {
                    emit_generation_state(&event_tx, &request, u64::MAX, GenerationState::Failed);
                }
                set_status_state(&status, ModelRuntimeState::Ready, 0);
                let _ = request_lease.completed_or_failed(result.is_ok());
                let _ = result_tx
                    .send(result.map(|output| GenerationCompletion::unverified(vec![output])));
            }
            WorkerCommand::ControlledGenerate {
                submission,
                admitted_request_sha256,
                event_tx,
                result_tx,
                cancellations,
                request_lease,
            } => {
                if let Err(error) = request_lease.running() {
                    let _ = result_tx.send(Err(error));
                    continue;
                }
                let _ = request_lease.progress(0);
                let owner_call_sequence = controlled_call_sequence;
                let Some(next_call_sequence) = controlled_call_sequence.checked_add(1) else {
                    let _ = result_tx.send(Err(NativeError::new(
                        NativeErrorCode::Internal,
                        "native controlled-generation owner call sequence overflowed",
                    )));
                    let _ = request_lease.completed_or_failed(false);
                    continue;
                };
                controlled_call_sequence = next_call_sequence;
                set_status_state(
                    &status,
                    ModelRuntimeState::Ready,
                    submission.request().cost().total_sequence_slots() as usize,
                );
                let strict_precheck = artifacts.verify_strict_unchanged(&fingerprint);
                let mut retained_events = Vec::new();
                let live_contract = ensure_controlled_token_contract(
                    &mut controlled_token_contract,
                    &model,
                    &fingerprint,
                );
                let execution = live_contract
                    .and_then(|contract| {
                        controlled_runtime::validate_submission_identity(
                            &submission,
                            &fingerprint,
                            contract,
                        )
                    })
                    .and_then(|()| {
                        controlled_runtime::execute_controlled_generation(
                            &model,
                            &mut context,
                            &submission,
                            &event_tx,
                            &mut retained_events,
                            &cancellations,
                            SequenceTracking {
                                token_counts: &mut sequence_token_counts,
                                token_ids: &mut sequence_token_ids,
                            },
                        )
                    });
                if execution.is_err() {
                    controlled_runtime::emit_missing_failed_terminals(
                        &event_tx,
                        submission.request(),
                        &mut retained_events,
                    );
                }
                let result = execution.and_then(|execution| {
                    let contract = controlled_token_contract.as_ref().ok_or_else(|| {
                        NativeError::new(
                            NativeErrorCode::Internal,
                            "controlled token contract disappeared after execution",
                        )
                    })?;
                    controlled_runtime::finalize_controlled_completion(
                        &model,
                        *submission,
                        execution,
                        retained_events,
                        fingerprint.clone(),
                        contract,
                        &artifacts,
                        strict_precheck,
                        &admitted_request_sha256,
                        owner_call_sequence,
                        Arc::clone(&worker_identity),
                    )
                });
                set_status_state(&status, ModelRuntimeState::Ready, 0);
                let _ = request_lease.completed_or_failed(result.is_ok());
                let _ = result_tx.send(result);
            }
            WorkerCommand::InspectControlledIdentity {
                participant_id,
                response,
            } => {
                let result = ensure_controlled_token_contract(
                    &mut controlled_token_contract,
                    &model,
                    &fingerprint,
                )
                .and_then(|contract| {
                    controlled_runtime::controlled_identity(participant_id, &fingerprint, contract)
                });
                let _ = response.send(result);
            }
            WorkerCommand::Snapshot {
                sequence_id,
                response,
            } => {
                let token_count = sequence_token_counts
                    .get(&sequence_id)
                    .copied()
                    .unwrap_or_default();
                let _ = response.send(state_buffer::export_sequence(
                    &context,
                    sequence_id,
                    token_count,
                    sequence_token_ids
                        .get(&sequence_id)
                        .cloned()
                        .unwrap_or_default(),
                ));
            }
            WorkerCommand::Restore {
                state,
                destination_sequence_id,
                response,
            } => {
                let token_count = state.token_count;
                let result =
                    state_buffer::import_sequence(&mut context, &state, destination_sequence_id);
                if result.is_ok() {
                    sequence_token_counts.insert(destination_sequence_id, token_count);
                    sequence_token_ids.insert(destination_sequence_id, state.token_ids);
                }
                let _ = response.send(result);
            }
            WorkerCommand::PrefillPrefix { request, response } => {
                let result = prefill_shared_prefix(
                    &model,
                    &mut context,
                    &request,
                    &mut sequence_token_counts,
                    &mut sequence_token_ids,
                );
                let _ = response.send(result);
            }
            WorkerCommand::Tokenize {
                messages,
                chat_template,
                response,
            } => {
                let _ = response.send(tokenize_messages(&model, messages, &chat_template));
            }
            WorkerCommand::PrepareInput { input, response } => {
                let _ = response.send(prepare_input(&model, input));
            }
        }
    }
    set_status_state(&status, ModelRuntimeState::Stopped, 0);
}

fn reject_queued_commands(command_rx: &Receiver<WorkerCommand>) {
    while let Ok(command) = command_rx.try_recv() {
        reject_queued_command(command);
    }
}

fn reject_queued_command(command: WorkerCommand) {
    let cancelled = || {
        NativeError::new(
            NativeErrorCode::Cancelled,
            "native model shutdown cancelled an admitted queued command",
        )
    };
    match command {
        WorkerCommand::EmbedBatch {
            result,
            cancellation,
            request_lease,
            ..
        } => {
            cancellation.store(true, Ordering::Release);
            let _ = request_lease.cancel_queued();
            let _ = result.send(embedding_runtime::EmbeddingCompletion::failed(cancelled()));
        }
        WorkerCommand::GenerateBatch {
            request,
            event_tx,
            result_tx,
            cancellations,
            reasoning_forces: _,
            request_lease,
            ..
        } => {
            for flag in cancellations {
                flag.store(true, Ordering::Release);
            }
            emit_cancelled_case_events(&event_tx, &request);
            let _ = request_lease.cancel_queued();
            let _ = result_tx.send(Err(cancelled()));
        }
        WorkerCommand::Generate {
            request,
            event_tx,
            result_tx,
            cancellations,
            reasoning_forces: _,
            request_lease,
        } => {
            for flag in cancellations {
                flag.store(true, Ordering::Release);
            }
            emit_cancelled_branch_events(&event_tx, &request);
            let _ = request_lease.cancel_queued();
            let _ = result_tx.send(Err(cancelled()));
        }
        WorkerCommand::GenerateMultimodal {
            request,
            event_tx,
            result_tx,
            cancellation,
            reasoning_force: _,
            request_lease,
        } => {
            cancellation.store(true, Ordering::Release);
            emit_generation_state(&event_tx, &request, u64::MAX, GenerationState::Cancelled);
            let _ = request_lease.cancel_queued();
            let _ = result_tx.send(Err(cancelled()));
        }
        WorkerCommand::ControlledGenerate {
            submission,
            admitted_request_sha256: _,
            event_tx,
            result_tx,
            cancellations,
            request_lease,
        } => {
            let _ = request_lease.cancel_queued();
            controlled_runtime::reject_queued_controlled(
                *submission,
                event_tx,
                result_tx,
                cancellations,
            );
        }
        WorkerCommand::InspectControlledIdentity { response, .. } => {
            let _ = response.send(Err(cancelled()));
        }
        WorkerCommand::Snapshot { response, .. } => {
            let _ = response.send(Err(cancelled()));
        }
        WorkerCommand::Restore { response, .. } => {
            let _ = response.send(Err(cancelled()));
        }
        WorkerCommand::PrefillPrefix { response, .. } => {
            let _ = response.send(Err(cancelled()));
        }
        WorkerCommand::Tokenize { response, .. } => {
            let _ = response.send(Err(cancelled()));
        }
        WorkerCommand::PrepareInput { response, .. } => {
            let _ = response.send(Err(cancelled()));
        }
    }
}

fn ensure_controlled_token_contract<'a>(
    slot: &'a mut Option<llama_native_types::TokenContractIdentity>,
    model: &LlamaModel,
    fingerprint: &ModelFingerprint,
) -> NativeResult<&'a llama_native_types::TokenContractIdentity> {
    if slot.is_none() {
        *slot = Some(controlled_runtime::derive_live_token_contract(
            model,
            fingerprint,
        )?);
    }
    slot.as_ref().ok_or_else(|| {
        NativeError::new(
            NativeErrorCode::Internal,
            "controlled token contract initialization returned no value",
        )
    })
}

fn generation_context_params(
    config: &NativeModelConfig,
    context_tokens: u32,
) -> LlamaContextParams {
    LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(context_tokens))
        .with_n_batch(config.batch_tokens)
        .with_n_ubatch(config.batch_tokens.min(512))
        .with_n_seq_max(config.max_sequences)
        .with_n_threads(native_thread_count())
        .with_n_threads_batch(native_thread_count())
        .with_kv_unified(true)
        .with_no_perf(false)
}

fn embedding_context_params(
    config: &NativeModelConfig,
    context_tokens: u32,
    maximum_input_tokens: u32,
    pooling: EmbeddingPooling,
) -> LlamaContextParams {
    let physical_batch = maximum_input_tokens.min(config.batch_tokens).clamp(1, 512);
    LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(context_tokens))
        .with_n_batch(maximum_input_tokens.max(1))
        .with_n_ubatch(physical_batch)
        .with_n_seq_max(1)
        .with_n_threads(native_thread_count())
        .with_n_threads_batch(native_thread_count())
        .with_kv_unified(true)
        .with_no_perf(false)
        .with_embeddings(true)
        .with_pooling_type(llama_pooling(pooling))
}

const fn llama_pooling(pooling: EmbeddingPooling) -> LlamaPoolingType {
    match pooling {
        EmbeddingPooling::None => LlamaPoolingType::None,
        EmbeddingPooling::Mean => LlamaPoolingType::Mean,
        EmbeddingPooling::Cls => LlamaPoolingType::Cls,
        EmbeddingPooling::Last => LlamaPoolingType::Last,
        EmbeddingPooling::Rank => LlamaPoolingType::Rank,
    }
}

fn resolved_embedding_pooling(pooling: LlamaPoolingType) -> NativeResult<EmbeddingPooling> {
    match pooling {
        LlamaPoolingType::None => Ok(EmbeddingPooling::None),
        LlamaPoolingType::Mean => Ok(EmbeddingPooling::Mean),
        LlamaPoolingType::Cls => Ok(EmbeddingPooling::Cls),
        LlamaPoolingType::Last => Ok(EmbeddingPooling::Last),
        LlamaPoolingType::Rank => Ok(EmbeddingPooling::Rank),
        LlamaPoolingType::Unspecified => Err(NativeError::new(
            NativeErrorCode::UnsupportedParameter,
            "llama.cpp did not resolve a concrete embedding pooling mode",
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_embedding_batch(
    config: &NativeModelConfig,
    backend: &LlamaBackend,
    model: &LlamaModel,
    fingerprint: &ModelFingerprint,
    context_tokens: u32,
    request: &EmbeddingBatchRequest,
    cancellation: &AtomicBool,
) -> NativeResult<EmbeddingBatchOutput> {
    check_embedding_cancellation(cancellation)?;
    let maximum_input_tokens = request
        .inputs()
        .iter()
        .map(|input| input.token_ids().len())
        .max()
        .unwrap_or_default();
    if maximum_input_tokens > context_tokens as usize {
        return Err(NativeError::new(
            NativeErrorCode::PromptTooLarge,
            format!(
                "embedding input requires {maximum_input_tokens} context cells but the model context has {context_tokens}"
            ),
        ));
    }
    let maximum_input_tokens = u32::try_from(maximum_input_tokens).map_err(|_| {
        NativeError::new(
            NativeErrorCode::InvalidConfig,
            "embedding input token count does not fit the native context contract",
        )
    })?;
    validate_embedding_token_ids(model, request)?;
    let expected_width = embedding_output_width(model, request.pooling())?;
    validate_embedding_output_budget(
        request.inputs().iter().map(|input| input.token_ids().len()),
        request.pooling(),
        expected_width,
    )?;
    let params = embedding_context_params(
        config,
        context_tokens,
        maximum_input_tokens,
        request.pooling(),
    );
    let execution_fingerprint = embedding_execution_fingerprint(fingerprint, &params);
    let mut context = model.new_context(backend, params).map_err(|error| {
        NativeError::new(
            NativeErrorCode::ContextCreateFailed,
            format!("failed to create the temporary embedding context: {error}"),
        )
    })?;
    let live_pooling = resolved_embedding_pooling(context.pooling_type())?;
    if live_pooling != request.pooling() {
        return Err(NativeError::new(
            NativeErrorCode::UnsupportedParameter,
            format!(
                "requested {:?} embedding pooling but llama.cpp resolved {:?}",
                request.pooling(),
                live_pooling
            ),
        ));
    }

    let mut dimensions = None;
    let mut total_values = 0usize;
    let mut outputs = Vec::with_capacity(request.inputs().len());
    for (input_index, input) in request.inputs().iter().enumerate() {
        check_embedding_cancellation(cancellation)?;
        context.clear_kv_cache();
        let tokens = input
            .token_ids()
            .iter()
            .copied()
            .map(LlamaToken::new)
            .collect::<Vec<_>>();
        let mut batch = LlamaBatch::new(tokens.len(), 1);
        batch
            .add_sequence(&tokens, 0, live_pooling == EmbeddingPooling::None)
            .map_err(|error| native_decode_error("failed to build embedding batch", error))?;
        context
            .decode(&mut batch)
            .map_err(|error| native_decode_error("failed to decode embedding batch", error))?;
        check_embedding_cancellation(cancellation)?;
        let (row_count, width, mut values) =
            read_embedding_values(&context, live_pooling, tokens.len())?;
        if width != expected_width {
            return Err(NativeError::new(
                NativeErrorCode::ModelInvalid,
                format!("model returned embedding width {width} after reporting {expected_width}"),
            ));
        }
        if dimensions.is_some_and(|known| known != width) {
            return Err(NativeError::new(
                NativeErrorCode::ModelInvalid,
                "model changed embedding width inside one request",
            ));
        }
        dimensions = Some(width);
        apply_embedding_normalization(&mut values, row_count, width, request.normalization())?;
        total_values = total_values.checked_add(values.len()).ok_or_else(|| {
            NativeError::new(
                NativeErrorCode::InvalidConfig,
                "embedding batch value count overflow",
            )
        })?;
        if total_values > MAX_EMBEDDING_BATCH_VALUES {
            return Err(NativeError::new(
                NativeErrorCode::InvalidConfig,
                format!(
                    "embedding batch cannot materialize more than {MAX_EMBEDDING_BATCH_VALUES} values"
                ),
            ));
        }
        outputs.push(EmbeddingVectorOutput::new(
            input.input_id().to_string(),
            input_index,
            input.token_ids().to_vec(),
            u32::try_from(row_count).map_err(|_| {
                NativeError::new(
                    NativeErrorCode::InvalidConfig,
                    "embedding row count does not fit the public output contract",
                )
            })?,
            values,
        )?);
    }
    let dimensions = u32::try_from(dimensions.unwrap_or_default()).map_err(|_| {
        NativeError::new(
            NativeErrorCode::ModelInvalid,
            "embedding width does not fit the public output contract",
        )
    })?;
    check_embedding_cancellation(cancellation)?;
    let output_config =
        EmbeddingOutputConfig::new(live_pooling, request.normalization(), dimensions)?;
    EmbeddingBatchOutput::new(
        request.request_id().to_string(),
        request.model_id().to_string(),
        output_config,
        outputs,
        execution_fingerprint,
        EmbeddingTransportEvidence::new(NativeTransport::InProcess, true, false)?,
    )
}

fn embedding_output_width(model: &LlamaModel, pooling: EmbeddingPooling) -> NativeResult<usize> {
    let width = if pooling == EmbeddingPooling::Rank {
        usize::try_from(model.n_cls_out()).map_err(|_| {
            NativeError::new(
                NativeErrorCode::ModelInvalid,
                "model classification width does not fit this host",
            )
        })?
    } else {
        usize::try_from(model.n_embd_out()).map_err(|_| {
            NativeError::new(
                NativeErrorCode::ModelInvalid,
                "model embedding width is negative or does not fit this host",
            )
        })?
    };
    if width == 0 || width > MAX_EMBEDDING_DIMENSIONS as usize {
        return Err(NativeError::new(
            NativeErrorCode::ModelInvalid,
            format!(
                "model reports embedding width {width}; expected 1..={MAX_EMBEDDING_DIMENSIONS}"
            ),
        ));
    }
    Ok(width)
}

fn validate_embedding_output_budget(
    input_token_counts: impl IntoIterator<Item = usize>,
    pooling: EmbeddingPooling,
    width: usize,
) -> NativeResult<()> {
    if width == 0 || width > MAX_EMBEDDING_DIMENSIONS as usize {
        return Err(NativeError::new(
            NativeErrorCode::ModelInvalid,
            format!(
                "model reports embedding width {width}; expected 1..={MAX_EMBEDDING_DIMENSIONS}"
            ),
        ));
    }
    let mut total_values = 0usize;
    for token_count in input_token_counts {
        let row_count = if pooling == EmbeddingPooling::None {
            token_count
        } else {
            1
        };
        let value_count = row_count.checked_mul(width).ok_or_else(|| {
            NativeError::new(
                NativeErrorCode::InvalidConfig,
                "embedding output shape overflows this host",
            )
        })?;
        if value_count > MAX_EMBEDDING_VALUES_PER_OUTPUT {
            return Err(NativeError::new(
                NativeErrorCode::InvalidConfig,
                format!(
                    "one embedding output would exceed {MAX_EMBEDDING_VALUES_PER_OUTPUT} values"
                ),
            ));
        }
        total_values = total_values.checked_add(value_count).ok_or_else(|| {
            NativeError::new(
                NativeErrorCode::InvalidConfig,
                "embedding batch value count overflows this host",
            )
        })?;
        if total_values > MAX_EMBEDDING_BATCH_VALUES {
            return Err(NativeError::new(
                NativeErrorCode::InvalidConfig,
                format!("embedding batch would exceed {MAX_EMBEDDING_BATCH_VALUES} total values"),
            ));
        }
    }
    Ok(())
}

fn embedding_execution_fingerprint(
    resident_fingerprint: &ModelFingerprint,
    params: &LlamaContextParams,
) -> ModelFingerprint {
    let (rope_config_sha256, base_kv_layout_sha256) = context_fingerprints(params);
    let embedding_layout = format!(
        "base={base_kv_layout_sha256};embeddings={};pooling={:?}",
        params.embeddings(),
        params.pooling_type(),
    );
    let mut fingerprint = resident_fingerprint.clone();
    fingerprint.context_tokens = params.n_ctx().map_or(0, NonZeroU32::get);
    fingerprint.batch_tokens = params.n_batch();
    fingerprint.max_sequences = params.n_seq_max();
    fingerprint.rope_config_sha256 = rope_config_sha256;
    fingerprint.kv_layout_sha256 = format!("{:x}", Sha256::digest(embedding_layout.as_bytes()));
    fingerprint
}

fn validate_embedding_token_ids(
    model: &LlamaModel,
    request: &EmbeddingBatchRequest,
) -> NativeResult<()> {
    validate_embedding_token_ids_in_vocab(model.n_vocab(), request)
}

fn validate_embedding_token_ids_in_vocab(
    vocabulary_size: i32,
    request: &EmbeddingBatchRequest,
) -> NativeResult<()> {
    if vocabulary_size <= 0 {
        return Err(NativeError::new(
            NativeErrorCode::ModelInvalid,
            "embedding model reported an empty vocabulary",
        ));
    }
    for input in request.inputs() {
        if let Some(invalid) = input
            .token_ids()
            .iter()
            .find(|token_id| **token_id < 0 || **token_id >= vocabulary_size)
        {
            return Err(NativeError::new(
                NativeErrorCode::InvalidConfig,
                format!(
                    "embedding input {} contains token ID {invalid} outside vocabulary 0..{vocabulary_size}",
                    input.input_id()
                ),
            ));
        }
    }
    Ok(())
}

fn read_embedding_values(
    context: &LlamaContext<'_>,
    pooling: EmbeddingPooling,
    token_count: usize,
) -> NativeResult<(usize, usize, Vec<f32>)> {
    if pooling == EmbeddingPooling::None {
        let first = context
            .embeddings_ith(0)
            .map_err(|error| embedding_read_error(pooling, error))?;
        let width = first.len();
        let value_count = token_count.checked_mul(width).ok_or_else(|| {
            NativeError::new(
                NativeErrorCode::InvalidConfig,
                "per-token embedding value count overflow",
            )
        })?;
        if value_count > MAX_EMBEDDING_VALUES_PER_OUTPUT {
            return Err(NativeError::new(
                NativeErrorCode::InvalidConfig,
                format!(
                    "one unpooled embedding cannot materialize more than {MAX_EMBEDDING_VALUES_PER_OUTPUT} values"
                ),
            ));
        }
        let mut values = Vec::with_capacity(value_count);
        values.extend_from_slice(first);
        for row_index in 1..token_count {
            let row_index = i32::try_from(row_index).map_err(|_| {
                NativeError::new(
                    NativeErrorCode::InvalidConfig,
                    "embedding token index does not fit llama.cpp",
                )
            })?;
            let row = context
                .embeddings_ith(row_index)
                .map_err(|error| embedding_read_error(pooling, error))?;
            if row.len() != width {
                return Err(NativeError::new(
                    NativeErrorCode::ModelInvalid,
                    "model changed per-token embedding width inside one input",
                ));
            }
            values.extend_from_slice(row);
        }
        Ok((token_count, width, values))
    } else {
        let embedding = context
            .embeddings_seq_ith(0)
            .map_err(|error| embedding_read_error(pooling, error))?;
        if embedding.len() > MAX_EMBEDDING_VALUES_PER_OUTPUT {
            return Err(NativeError::new(
                NativeErrorCode::InvalidConfig,
                format!(
                    "one pooled embedding cannot materialize more than {MAX_EMBEDDING_VALUES_PER_OUTPUT} values"
                ),
            ));
        }
        Ok((1, embedding.len(), embedding.to_vec()))
    }
}

fn embedding_read_error(pooling: EmbeddingPooling, error: impl std::fmt::Display) -> NativeError {
    NativeError::new(
        NativeErrorCode::UnsupportedParameter,
        format!("model cannot return {pooling:?} embeddings: {error}"),
    )
}

fn apply_embedding_normalization(
    values: &mut [f32],
    row_count: usize,
    width: usize,
    normalization: EmbeddingNormalization,
) -> NativeResult<()> {
    let expected = row_count.checked_mul(width).ok_or_else(|| {
        NativeError::new(
            NativeErrorCode::InvalidConfig,
            "embedding output shape overflow",
        )
    })?;
    if width == 0 || values.len() != expected || values.iter().any(|value| !value.is_finite()) {
        return Err(NativeError::new(
            NativeErrorCode::ModelInvalid,
            "embedding rows must have a positive stable width and finite values",
        ));
    }
    if normalization == EmbeddingNormalization::None {
        return Ok(());
    }
    for row in values.chunks_exact_mut(width) {
        let norm = row
            .iter()
            .map(|value| f64::from(*value).powi(2))
            .sum::<f64>()
            .sqrt();
        if norm == 0.0 || !norm.is_finite() {
            return Err(NativeError::new(
                NativeErrorCode::ModelInvalid,
                "cannot L2-normalize a zero-norm or non-finite embedding row",
            ));
        }
        for value in row {
            *value = (f64::from(*value) / norm) as f32;
        }
    }
    if values.iter().any(|value| !value.is_finite()) {
        return Err(NativeError::new(
            NativeErrorCode::ModelInvalid,
            "embedding normalization produced a non-finite value",
        ));
    }
    Ok(())
}

fn check_embedding_cancellation(cancellation: &AtomicBool) -> NativeResult<()> {
    if cancellation.load(Ordering::Acquire) {
        Err(NativeError::new(
            NativeErrorCode::Cancelled,
            "embedding request was cancelled",
        ))
    } else {
        Ok(())
    }
}

fn record_embedding_capability(
    status: &Arc<RwLock<ResidentModelStatus>>,
    output: &EmbeddingBatchOutput,
    context_tokens: u32,
) {
    let Ok(mut status) = status.write() else {
        return;
    };
    let Some(descriptor) = status.descriptor.as_mut() else {
        return;
    };
    let previous = descriptor.capabilities.exact.evidence.embeddings;
    let mut pooling = if previous.declaration() == CapabilityDeclarationStatus::Inspected {
        previous.pooling()
    } else {
        EmbeddingPoolingSupport::default()
    };
    match output.config().pooling() {
        EmbeddingPooling::None => pooling.none = true,
        EmbeddingPooling::Mean => pooling.mean = true,
        EmbeddingPooling::Cls => pooling.cls = true,
        EmbeddingPooling::Last => pooling.last = true,
        EmbeddingPooling::Rank => pooling.rank = true,
    }
    let dimensions = if previous.declaration() == CapabilityDeclarationStatus::Inspected
        && previous.dimensions() != Some(output.config().dimensions())
    {
        None
    } else {
        Some(output.config().dimensions())
    };
    if let Ok(capabilities) = EmbeddingCapabilities::new(
        CapabilityDeclarationStatus::Inspected,
        pooling,
        EmbeddingNormalizationSupport {
            none: true,
            l2: true,
        },
        u16::try_from(MAX_EMBEDDING_BATCH_INPUTS).ok(),
        Some(context_tokens.min(MAX_EMBEDDING_INPUT_TOKENS as u32)),
        dimensions,
    ) {
        descriptor.capabilities.exact.evidence.embeddings = capabilities;
    }
}

fn generate_batch(
    model: &LlamaModel,
    context: &mut LlamaContext<'_>,
    request: &SharedPrefixBatchRequest,
    prepared_token_sets: Option<Vec<Vec<LlamaToken>>>,
    exact_cell_budget: Option<&ExactTokenBatchCellBudget>,
    mut supervision: BatchSupervision<'_>,
    tracking: SequenceTracking<'_>,
) -> NativeResult<GeneratedBatchExecution> {
    let retain_token_piece_traces = supervision.retain_token_piece_traces;
    context.clear_kv_cache();
    tracking.token_counts.clear();
    tracking.token_ids.clear();
    let started = Instant::now();
    let token_sets = if let Some(token_sets) = prepared_token_sets {
        token_sets
    } else {
        let prompts = render_branch_prompts(model, request)?;
        prompts
            .iter()
            .map(|prompt| {
                model.str_to_token(prompt, AddBos::Always).map_err(|error| {
                    NativeError::new(
                        NativeErrorCode::ModelInvalid,
                        format!("failed to tokenize prompt: {error}"),
                    )
                })
            })
            .collect::<NativeResult<Vec<_>>>()?
    };
    let minimum_tokens = token_sets.iter().map(Vec::len).min().unwrap_or_default();
    if minimum_tokens == 0 {
        return Err(NativeError::new(
            NativeErrorCode::ModelInvalid,
            "generation input produced an empty token sequence",
        ));
    }
    if request.cached_prefix.is_some()
        && request
            .branches
            .iter()
            .any(|branch| branch.cached_prefix.is_some())
    {
        return Err(NativeError::new(
            NativeErrorCode::InvalidConfig,
            "a request-level cache and branch-level caches cannot be combined",
        ));
    }
    let cached_states = request
        .branches
        .iter()
        .enumerate()
        .map(|(index, branch)| {
            if index == 0 && request.branches.len() == 1 {
                request
                    .cached_prefix
                    .as_ref()
                    .or(branch.cached_prefix.as_ref())
            } else {
                branch.cached_prefix.as_ref()
            }
        })
        .collect::<Vec<_>>();
    let mut prefix_lengths = vec![0_usize; request.branches.len()];
    let uncached_indices = cached_states
        .iter()
        .enumerate()
        .filter_map(|(index, state)| state.is_none().then_some(index))
        .collect::<Vec<_>>();
    for (index, state) in cached_states.iter().enumerate() {
        let Some(state) = state else {
            continue;
        };
        if state.token_count == 0
            || state.token_count != state.token_ids.len()
            || state.token_count >= token_sets[index].len()
            || !token_sets[index]
                .iter()
                .take(state.token_count)
                .map(|token| token.0)
                .eq(state.token_ids.iter().copied())
        {
            return Err(NativeError::new(
                NativeErrorCode::CacheIncompatible,
                format!(
                    "saved KV prefix does not match rendered branch `{}`",
                    request.branches[index].branch_id
                ),
            ));
        }
        state_buffer::import_sequence(context, state, index as i32)?;
        prefix_lengths[index] = state.token_count;
    }
    let (shared_uncached_prefix, required_tokens) = if let Some(budget) = exact_cell_budget {
        exact_budget_execution_values(
            budget,
            request,
            &token_sets,
            &cached_states,
            &mut prefix_lengths,
        )?
    } else {
        let uncached_token_sets = uncached_indices
            .iter()
            .map(|index| token_sets[*index].clone())
            .collect::<Vec<_>>();
        let shared_uncached_prefix = if uncached_token_sets.len() > 1 {
            let uncached_minimum = uncached_token_sets
                .iter()
                .map(Vec::len)
                .min()
                .unwrap_or_default();
            longest_common_prefix(&uncached_token_sets).min(uncached_minimum.saturating_sub(1))
        } else {
            0
        };
        for index in &uncached_indices {
            prefix_lengths[*index] = shared_uncached_prefix;
        }
        let cached_cells = cached_states
            .iter()
            .filter_map(|state| state.map(|state| state.token_count))
            .try_fold(0_usize, |total, count| total.checked_add(count))
            .ok_or_else(|| batch_cell_budget_error("cached-prefix cell count overflowed"))?;
        let branch_cells = token_sets
            .iter()
            .enumerate()
            .try_fold(0_usize, |total, (index, tokens)| {
                let prompt_suffix = tokens.len().checked_sub(prefix_lengths[index])?;
                let case_cells = prompt_suffix
                    .checked_add(request.branches[index].sampling.max_tokens as usize)?;
                total.checked_add(case_cells)
            })
            .ok_or_else(|| batch_cell_budget_error("generation batch cell count overflowed"))?;
        let required_tokens = cached_cells
            .checked_add(shared_uncached_prefix)
            .and_then(|total| total.checked_add(branch_cells))
            .ok_or_else(|| batch_cell_budget_error("generation batch cell count overflowed"))?;
        (shared_uncached_prefix, required_tokens)
    };
    if required_tokens > context.n_ctx() as usize {
        return Err(NativeError::new(
            NativeErrorCode::PromptTooLarge,
            format!(
                "consult requires {required_tokens} context cells but this model context has {}",
                context.n_ctx()
            ),
        ));
    }
    for (index, branch) in request.branches.iter().enumerate() {
        supervision.emit_state(request, branch, index, 0, GenerationState::Prefilling);
    }
    if shared_uncached_prefix > 0 {
        let source = uncached_indices[0];
        decode_tokens_chunked(
            context,
            &token_sets[source][..shared_uncached_prefix],
            source as i32,
            0,
            false,
        )?;
        for destination in uncached_indices.iter().copied().skip(1) {
            context
                .copy_kv_cache_seq(
                    source as i32,
                    destination as i32,
                    Some(0),
                    Some(shared_uncached_prefix as u32),
                )
                .map_err(|error| {
                    NativeError::new(
                        NativeErrorCode::DecodeFailed,
                        format!("failed to copy shared KV prefix: {error}"),
                    )
                })?;
        }
    }
    for (sequence_id, tokens) in token_sets.iter().enumerate() {
        let prefix = prefix_lengths[sequence_id];
        let suffix = &tokens[prefix..tokens.len() - 1];
        if !suffix.is_empty() {
            decode_tokens_chunked(context, suffix, sequence_id as i32, prefix as i32, false)?;
        }
    }
    let mut final_prompt_batch = LlamaBatch::new(request.branches.len(), 1);
    for (sequence_id, tokens) in token_sets.iter().enumerate() {
        final_prompt_batch
            .add(
                tokens[tokens.len() - 1],
                (tokens.len() - 1) as i32,
                &[sequence_id as i32],
                true,
            )
            .map_err(|error| native_decode_error("failed to build final prompt batch", error))?;
        tracking
            .token_counts
            .insert(sequence_id as i32, tokens.len());
        tracking.token_ids.insert(
            sequence_id as i32,
            tokens.iter().map(|token| token.0).collect(),
        );
    }
    context
        .decode(&mut final_prompt_batch)
        .map_err(|error| native_decode_error("failed to decode final prompt tokens", error))?;
    let mut branches = request
        .branches
        .iter()
        .enumerate()
        .map(|(index, branch)| {
            let mut sampler = build_sampler(model, &branch.sampling);
            sampler.accept_many(token_sets[index].iter());
            ActiveBranch {
                sequence_id: index as i32,
                request: branch,
                sampler,
                decoder: UTF_8.new_decoder(),
                text: String::new(),
                generated_token_ids: Vec::with_capacity(branch.sampling.max_tokens as usize),
                token_piece_trace: retain_token_piece_traces.then(|| {
                    TokenPieceTrace::with_token_capacity(branch.sampling.max_tokens as usize)
                }),
                terminal_sampled_token_id: None,
                generated: 0,
                next_position: token_sets[index].len() as i32,
                logit_index: index as i32,
                state: GenerationState::Generating,
                finish_reason: String::new(),
                event_index: 1,
                first_token_ms: None,
                forced_tokens: VecDeque::new(),
            }
        })
        .collect::<Vec<_>>();
    for branch in &mut branches {
        supervision.emit_state(
            request,
            branch.request,
            branch.sequence_id as usize,
            branch.event_index,
            GenerationState::Generating,
        );
        branch.event_index += 1;
    }
    loop {
        let mut next_tokens = Vec::<(usize, LlamaToken)>::new();
        for (index, branch) in branches.iter_mut().enumerate() {
            if branch.state != GenerationState::Generating {
                continue;
            }
            if supervision.cancellations[index].load(Ordering::Acquire) {
                branch.state = GenerationState::Cancelled;
                branch.finish_reason = "cancelled".to_string();
                let _ = context.clear_kv_cache_seq(Some(index as u32), None, None);
                continue;
            }
            if supervision.reasoning_forces[index].load(Ordering::Acquire) {
                supervision.mark_unrecorded_control();
                if branch.forced_tokens.is_empty()
                    && let Some(end_marker) = active_reasoning_end_marker(&branch.text)
                {
                    let tokens =
                        model
                            .str_to_token(end_marker, AddBos::Never)
                            .map_err(|error| {
                                NativeError::new(
                                    NativeErrorCode::ModelInvalid,
                                    format!("failed to tokenize the reasoning end marker: {error}"),
                                )
                            })?;
                    branch.forced_tokens.extend(tokens);
                    supervision.reasoning_forces[index].store(false, Ordering::Release);
                }
            }
            let token = if let Some(token) = branch.forced_tokens.pop_front() {
                branch.sampler.accept(token);
                token
            } else {
                branch.sampler.sample(context, branch.logit_index)
            };
            let terminal = model.is_eog_token(token);
            supervision.record_runtime_sample(
                index,
                branch.generated_token_ids.len(),
                token.0,
                terminal,
            );
            if terminal {
                branch.state = GenerationState::Completed;
                branch.finish_reason = "end_of_generation".to_string();
                branch.terminal_sampled_token_id = Some(token.0);
                continue;
            }
            branch.generated_token_ids.push(token.0);
            let bytes = model
                .token_to_piece_bytes(token, 512, false, None)
                .map_err(|error| {
                    NativeError::new(
                        NativeErrorCode::DecodeFailed,
                        format!("failed to decode generated token: {error}"),
                    )
                })?;
            if let Some(trace) = &mut branch.token_piece_trace {
                trace.push_piece(&bytes)?;
            }
            let piece = decode_generated_utf8_piece(&mut branch.decoder, &bytes, false)?;
            if branch.first_token_ms.is_none() {
                branch.first_token_ms = Some(started.elapsed().as_millis());
            }
            append_generated_utf8_piece(&mut branch.text, &piece)?;
            branch.generated += 1;
            if !piece.is_empty() {
                supervision.emit(GenerationEvent {
                    request_id: request.request_id.clone(),
                    branch_id: branch.request.branch_id.clone(),
                    sequence_id: branch.sequence_id,
                    input_index: index,
                    event_index: branch.event_index,
                    event: GenerationEventKind::Delta { text: piece },
                });
                branch.event_index += 1;
            }
            if let Some(stop) = branch
                .request
                .sampling
                .stop
                .iter()
                .find(|stop| !stop.is_empty() && branch.text.ends_with(stop.as_str()))
            {
                let keep = branch.text.len().saturating_sub(stop.len());
                branch.text.truncate(keep);
                branch.state = GenerationState::Completed;
                branch.finish_reason = "stop_sequence".to_string();
            } else if branch.generated >= branch.request.sampling.max_tokens as usize {
                branch.state = GenerationState::Completed;
                branch.finish_reason = "max_tokens".to_string();
            } else {
                next_tokens.push((index, token));
            }
        }
        if next_tokens.is_empty() {
            break;
        }
        let mut batch = LlamaBatch::new(next_tokens.len(), 1);
        for (logit_index, (branch_index, token)) in next_tokens.iter().enumerate() {
            let branch = &mut branches[*branch_index];
            batch
                .add(*token, branch.next_position, &[branch.sequence_id], true)
                .map_err(|error| native_decode_error("failed to build generation batch", error))?;
            branch.logit_index = logit_index as i32;
            branch.next_position += 1;
            tracking
                .token_counts
                .insert(branch.sequence_id, branch.next_position as usize);
            tracking
                .token_ids
                .entry(branch.sequence_id)
                .or_default()
                .push(token.0);
        }
        context
            .decode(&mut batch)
            .map_err(|error| native_decode_error("failed to decode generation batch", error))?;
    }
    for branch in &mut branches {
        let piece = decode_generated_utf8_piece(&mut branch.decoder, &[], true)?;
        append_generated_utf8_piece(&mut branch.text, &piece)?;
        if !piece.is_empty() {
            supervision.emit(GenerationEvent {
                request_id: request.request_id.clone(),
                branch_id: branch.request.branch_id.clone(),
                sequence_id: branch.sequence_id,
                input_index: branch.sequence_id as usize,
                event_index: branch.event_index,
                event: GenerationEventKind::Delta { text: piece },
            });
            branch.event_index += 1;
        }
        if matches!(
            branch.state,
            GenerationState::Completed | GenerationState::Cancelled
        ) {
            supervision.emit_state(
                request,
                branch.request,
                branch.sequence_id as usize,
                branch.event_index,
                branch.state,
            );
            branch.event_index += 1;
        }
    }
    let duration_ms = started.elapsed().as_millis();
    let mut outputs = Vec::with_capacity(branches.len());
    let mut terminal_sampled_token_ids = Vec::with_capacity(branches.len());
    let mut token_piece_traces = Vec::with_capacity(branches.len());
    for branch in branches {
        let completion_tokens = branch.generated;
        let tokens_per_second = if duration_ms == 0 {
            0.0
        } else {
            completion_tokens as f64 / (duration_ms as f64 / 1000.0)
        };
        terminal_sampled_token_ids.push(branch.terminal_sampled_token_id);
        if let Some(trace) = branch.token_piece_trace {
            token_piece_traces.push(trace);
        }
        outputs.push(GenerationOutput {
            request_id: request.request_id.clone(),
            branch_id: branch.request.branch_id.clone(),
            input_index: branch.sequence_id as usize,
            model_id: request.model_id.clone(),
            text: branch.text,
            generated_token_ids: branch.generated_token_ids,
            token_observations: None,
            state: branch.state,
            finish_reason: branch.finish_reason,
            metrics: GenerationMetrics {
                prompt_tokens: token_sets[branch.sequence_id as usize].len(),
                completion_tokens,
                shared_prefix_tokens: prefix_lengths[branch.sequence_id as usize],
                duration_ms,
                first_token_ms: branch.first_token_ms,
                tokens_per_second,
                cache: if let Some(state) = cached_states[branch.sequence_id as usize] {
                    GenerationCacheMetrics {
                        supplied_prefix_tokens: state.token_count,
                        restored_prefix_tokens: state.token_count,
                        batch_shared_prefix_tokens: 0,
                    }
                } else {
                    GenerationCacheMetrics {
                        supplied_prefix_tokens: 0,
                        restored_prefix_tokens: 0,
                        batch_shared_prefix_tokens: shared_uncached_prefix,
                    }
                },
            },
            real_engine_invoked: true,
            fake_fixture: false,
            transport: NativeTransport::InProcess,
        });
    }
    Ok(GeneratedBatchExecution {
        outputs,
        terminal_sampled_token_ids,
        token_piece_traces,
    })
}

fn prepare_generation_batch(
    model: &LlamaModel,
    request: &GenerationBatchRequest,
) -> NativeResult<(SharedPrefixBatchRequest, Vec<Vec<LlamaToken>>)> {
    let token_sets = request
        .cases
        .iter()
        .enumerate()
        .map(|(index, case)| generation_case_tokens(model, case, index))
        .collect::<NativeResult<Vec<_>>>()?;
    let branches = request
        .cases
        .iter()
        .map(|case| BranchRequest {
            branch_id: case.case_id.clone(),
            label: case.case_id.clone(),
            instruction: String::new(),
            sampling: case.sampling.clone(),
            messages: Vec::new(),
            cached_prefix: case.cached_prefix.clone(),
        })
        .collect();
    Ok((
        SharedPrefixBatchRequest {
            request_id: request.request_id.clone(),
            model_id: request.model_id.clone(),
            common_messages: Vec::new(),
            chat_template: ChatTemplateChoice::ModelDefault,
            branches,
            cached_prefix: None,
        },
        token_sets,
    ))
}

fn exact_generation_case_token_ids(case: &GenerationCase) -> Option<&[i32]> {
    match &case.input {
        GenerationInput::Completion { prompts } => match prompts.as_slice() {
            [CompletionPrompt::Tokens { token_ids }] if !token_ids.is_empty() => Some(token_ids),
            _ => None,
        },
        GenerationInput::Chat { .. } | GenerationInput::FillInMiddle { .. } => None,
    }
}

fn is_exact_token_generation_batch(request: &GenerationBatchRequest) -> bool {
    !request.cases.is_empty()
        && request
            .cases
            .iter()
            .all(|case| exact_generation_case_token_ids(case).is_some())
}

fn exact_batch_shared_prefix_tokens(request: &GenerationBatchRequest) -> Option<usize> {
    usize::try_from(
        exact_token_batch_cell_budget(request)
            .ok()?
            .shared_uncached_prefix_tokens(),
    )
    .ok()
}

fn is_statically_sealable_generation_batch(
    request: &GenerationBatchRequest,
    admission: GenerationBatchAdmission,
) -> bool {
    admission == GenerationBatchAdmission::ExactBatch
        && is_exact_token_generation_batch(request)
        && request
            .cases
            .iter()
            .all(|case| case.cached_prefix.is_none() && case.sampling.seed != u32::MAX)
}

fn is_sealable_generation_batch(
    request: &GenerationBatchRequest,
    admission: GenerationBatchAdmission,
    unrecorded_control_used: bool,
) -> bool {
    is_statically_sealable_generation_batch(request, admission) && !unrecorded_control_used
}

fn should_retain_authority_evidence(
    statically_sealable: bool,
    strict_precheck: &NativeResult<()>,
) -> bool {
    statically_sealable && strict_precheck.is_ok()
}

fn verify_generation_batch_authority(
    model: &LlamaModel,
    request: GenerationBatchRequest,
    model_fingerprint: ModelFingerprint,
    outputs: &[GenerationOutput],
    capture: GenerationAuthorityCapture,
    artifacts: &ModelArtifactGuards,
) -> NativeResult<VerifiedGenerationEvidence> {
    let GenerationAuthorityCapture {
        terminal_sampled_token_ids,
        events,
        token_piece_traces,
    } = capture;
    if token_piece_traces.len() != outputs.len() {
        return Err(generation_verification_error(
            "token-piece trace count does not match the generated outputs",
        ));
    }
    let decoded_token_text = outputs
        .iter()
        .zip(&token_piece_traces)
        .map(|(output, trace)| {
            validate_live_token_piece_trace(model, output, trace)?;
            strict_verified_utf8_bytes(trace.raw_piece_bytes())
        })
        .collect::<NativeResult<Vec<_>>>()?;
    validate_verified_generation_batch(
        &request,
        &model_fingerprint,
        outputs,
        &terminal_sampled_token_ids,
        &events,
        &decoded_token_text,
        |token_id| model.is_eog_token(LlamaToken::new(token_id)),
    )?;
    // This is deliberately the last fallible check before producing the
    // private evidence that `wait_verified` can turn into authority, after all
    // tokenizer/EOG queries made during validation.
    artifacts.verify_strict_unchanged(&model_fingerprint)?;
    Ok(VerifiedGenerationEvidence {
        request,
        model_fingerprint,
        terminal_sampled_token_ids,
        events,
        token_piece_traces,
    })
}

fn validate_live_token_piece_trace(
    model: &LlamaModel,
    output: &GenerationOutput,
    trace: &TokenPieceTrace,
) -> NativeResult<()> {
    trace.validate(output.generated_token_ids.len())?;
    for (index, (token_id, boundaries)) in output
        .generated_token_ids
        .iter()
        .zip(trace.cumulative_boundaries.windows(2))
        .enumerate()
    {
        let start = usize::try_from(boundaries[0]).map_err(|_| {
            generation_verification_error("token-piece start boundary does not fit usize")
        })?;
        let end = usize::try_from(boundaries[1]).map_err(|_| {
            generation_verification_error("token-piece end boundary does not fit usize")
        })?;
        let captured = trace.raw_piece_bytes.get(start..end).ok_or_else(|| {
            generation_verification_error("token-piece boundary falls outside captured bytes")
        })?;
        let expected = model
            .token_to_piece_bytes(LlamaToken::new(*token_id), 512, false, None)
            .map_err(|error| {
                generation_verification_error(format!(
                    "failed to verify generated token piece {index}: {error}"
                ))
            })?;
        if captured != expected {
            return Err(generation_verification_error(
                "captured token-piece bytes disagree with the sampled token ID",
            ));
        }
    }
    Ok(())
}

fn validate_verified_generation_batch<F>(
    request: &GenerationBatchRequest,
    model_fingerprint: &ModelFingerprint,
    outputs: &[GenerationOutput],
    terminal_sampled_token_ids: &[Option<i32>],
    events: &[GenerationEvent],
    decoded_token_text: &[String],
    is_eog_token: F,
) -> NativeResult<()>
where
    F: Fn(i32) -> bool,
{
    if !is_exact_token_generation_batch(request) {
        return Err(generation_verification_error(
            "only exact-token completion batches can receive owner-worker authority",
        ));
    }
    if request
        .cases
        .iter()
        .any(|case| case.cached_prefix.is_some())
    {
        return Err(generation_verification_error(
            "caller-supplied cached state cannot receive owner-worker authority",
        ));
    }
    if request
        .cases
        .iter()
        .any(|case| case.sampling.seed == u32::MAX)
    {
        return Err(generation_verification_error(
            "the random default-seed sentinel cannot receive owner-worker authority",
        ));
    }
    if request.model_id != model_fingerprint.model_id {
        return Err(generation_verification_error(
            "request and live model fingerprint IDs disagree",
        ));
    }
    if outputs.len() != request.cases.len()
        || decoded_token_text.len() != outputs.len()
        || terminal_sampled_token_ids.len() != outputs.len()
    {
        return Err(generation_verification_error(
            "verified output count does not match the exact submitted batch",
        ));
    }
    let expected_shared_prefix = exact_batch_shared_prefix_tokens(request).ok_or_else(|| {
        generation_verification_error("failed to derive exact batch prefix metrics")
    })?;

    let mut next_event_indexes = vec![0_u64; request.cases.len()];
    let mut terminal_states = vec![None; request.cases.len()];
    let mut delta_text = vec![String::new(); request.cases.len()];
    for event in events {
        let Some(case) = request.cases.get(event.input_index) else {
            return Err(generation_verification_error(
                "retained event input index is outside the submitted batch",
            ));
        };
        let expected_sequence_id = i32::try_from(event.input_index).map_err(|_| {
            generation_verification_error("retained event input index does not fit sequence ID")
        })?;
        if event.request_id != request.request_id
            || event.branch_id != case.case_id
            || event.sequence_id != expected_sequence_id
        {
            return Err(generation_verification_error(
                "retained event request, case, or sequence identity disagrees",
            ));
        }
        let expected_event_index = next_event_indexes[event.input_index];
        if event.event_index != expected_event_index {
            return Err(generation_verification_error(
                "retained event indexes are not contiguous per case",
            ));
        }
        if terminal_states[event.input_index].is_some() {
            return Err(generation_verification_error(
                "retained ledger contains an event after a case terminal",
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
                terminal_states[event.input_index] = Some(*state);
            }
            GenerationEventKind::Delta { text } if event.event_index >= 2 && !text.is_empty() => {
                delta_text[event.input_index].push_str(text);
            }
            _ => {
                return Err(generation_verification_error(
                    "retained ledger contains an impossible generation event transition",
                ));
            }
        }
        next_event_indexes[event.input_index] = expected_event_index
            .checked_add(1)
            .ok_or_else(|| generation_verification_error("retained event index overflowed"))?;
    }

    for (index, (case, output)) in request.cases.iter().zip(outputs).enumerate() {
        let prompt_token_ids = exact_generation_case_token_ids(case).ok_or_else(|| {
            generation_verification_error("verified case lost its exact token prompt")
        })?;
        if output.request_id != request.request_id
            || output.model_id != request.model_id
            || output.branch_id != case.case_id
            || output.input_index != index
        {
            return Err(generation_verification_error(
                "ordered output identity disagrees with the submitted request",
            ));
        }
        if !output.real_engine_invoked
            || output.fake_fixture
            || output.transport != NativeTransport::InProcess
        {
            return Err(generation_verification_error(
                "verified output lacks real in-process engine evidence",
            ));
        }
        if output.generated_token_ids.len() != output.metrics.completion_tokens {
            return Err(generation_verification_error(
                "generated token evidence disagrees with completion metrics",
            ));
        }
        if output.metrics.prompt_tokens != prompt_token_ids.len()
            || output.generated_token_ids.len() > case.sampling.max_tokens as usize
            || output.metrics.shared_prefix_tokens != expected_shared_prefix
            || output.metrics.cache.supplied_prefix_tokens != 0
            || output.metrics.cache.restored_prefix_tokens != 0
            || output.metrics.cache.batch_shared_prefix_tokens != expected_shared_prefix
        {
            return Err(generation_verification_error(
                "output prompt, generation, or uncached-prefix metrics disagree",
            ));
        }
        if output
            .generated_token_ids
            .iter()
            .copied()
            .any(&is_eog_token)
        {
            return Err(generation_verification_error(
                "generated prose token evidence contains an unseparated EOG token",
            ));
        }
        if !matches!(
            output.state,
            GenerationState::Completed | GenerationState::Cancelled
        ) || terminal_states[index] != Some(output.state)
        {
            return Err(generation_verification_error(
                "output and retained terminal states disagree",
            ));
        }
        if delta_text[index] != decoded_token_text[index] {
            return Err(generation_verification_error(
                "retained delta text disagrees with the exact generated token IDs",
            ));
        }
        let terminal_sampled_token_id = terminal_sampled_token_ids[index];
        match (output.state, output.finish_reason.as_str()) {
            (GenerationState::Cancelled, "cancelled")
                if terminal_sampled_token_id.is_none()
                    && output.text == decoded_token_text[index] => {}
            (GenerationState::Completed, "end_of_generation")
                if terminal_sampled_token_id.is_some_and(&is_eog_token)
                    && output.text == decoded_token_text[index] => {}
            (GenerationState::Completed, "max_tokens")
                if terminal_sampled_token_id.is_none()
                    && output.text == decoded_token_text[index]
                    && output.generated_token_ids.len() == case.sampling.max_tokens as usize => {}
            (GenerationState::Completed, "stop_sequence")
                if terminal_sampled_token_id.is_none()
                    && case.sampling.stop.iter().any(|stop| {
                        !stop.is_empty()
                            && decoded_token_text[index] == format!("{}{stop}", output.text)
                    }) => {}
            _ => {
                return Err(generation_verification_error(
                    "output text, stop condition, state, and finish reason disagree",
                ));
            }
        }
    }
    Ok(())
}

fn decode_verified_token_text(model: &LlamaModel, token_ids: &[i32]) -> NativeResult<String> {
    let mut pieces = Vec::with_capacity(token_ids.len());
    for token_id in token_ids {
        let bytes = model
            .token_to_piece_bytes(LlamaToken::new(*token_id), 512, false, None)
            .map_err(|error| {
                generation_verification_error(format!(
                    "failed to re-decode generated token evidence: {error}"
                ))
            })?;
        pieces.push(bytes);
    }
    strict_verified_utf8(&pieces)
}

fn strict_verified_utf8(pieces: &[Vec<u8>]) -> NativeResult<String> {
    let byte_count = pieces.iter().try_fold(0_usize, |total, piece| {
        checked_generated_output_len(total, piece.len())
    })?;
    let mut bytes = Vec::with_capacity(byte_count);
    for piece in pieces {
        bytes.extend_from_slice(piece);
    }
    strict_verified_utf8_bytes(&bytes)
}

fn strict_verified_utf8_bytes(bytes: &[u8]) -> NativeResult<String> {
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|error| {
            generation_verification_error(format!(
                "generated token pieces are not complete canonical UTF-8: {error}"
            ))
        })
}

fn append_generated_utf8_piece(output: &mut String, piece: &str) -> NativeResult<()> {
    let _ = checked_generated_output_len(output.len(), piece.len())?;
    output.push_str(piece);
    Ok(())
}

fn decode_generated_utf8_piece(
    decoder: &mut encoding_rs::Decoder,
    bytes: &[u8],
    last: bool,
) -> NativeResult<String> {
    let capacity = decoder.max_utf8_buffer_length(bytes.len()).ok_or_else(|| {
        NativeError::new(
            NativeErrorCode::MemoryBudgetExceeded,
            "generated UTF-8 decoder capacity overflowed",
        )
    })?;
    let mut piece = String::new();
    piece.try_reserve_exact(capacity).map_err(|error| {
        NativeError::new(
            NativeErrorCode::MemoryBudgetExceeded,
            format!("generated UTF-8 decoder allocation failed: {error}"),
        )
    })?;
    let mut remaining = bytes;
    loop {
        let (result, read, _had_errors) = decoder.decode_to_string(remaining, &mut piece, last);
        remaining = remaining.get(read..).ok_or_else(|| {
            NativeError::new(
                NativeErrorCode::DecodeFailed,
                "generated UTF-8 decoder consumed beyond its input",
            )
        })?;
        match result {
            CoderResult::InputEmpty if remaining.is_empty() => return Ok(piece),
            CoderResult::InputEmpty => {
                return Err(NativeError::new(
                    NativeErrorCode::DecodeFailed,
                    "generated UTF-8 decoder left unconsumed input",
                ));
            }
            CoderResult::OutputFull => {
                let additional =
                    decoder
                        .max_utf8_buffer_length(remaining.len())
                        .ok_or_else(|| {
                            NativeError::new(
                                NativeErrorCode::MemoryBudgetExceeded,
                                "generated UTF-8 decoder retry capacity overflowed",
                            )
                        })?;
                piece.try_reserve(additional.max(1)).map_err(|error| {
                    NativeError::new(
                        NativeErrorCode::MemoryBudgetExceeded,
                        format!("generated UTF-8 decoder retry allocation failed: {error}"),
                    )
                })?;
            }
        }
    }
}

fn checked_generated_output_len(current: usize, additional: usize) -> NativeResult<usize> {
    let next = current.checked_add(additional).ok_or_else(|| {
        NativeError::new(
            NativeErrorCode::MemoryBudgetExceeded,
            "generated UTF-8 output byte count overflowed",
        )
    })?;
    if next > MAX_GENERATED_OUTPUT_BYTES {
        return Err(NativeError::new(
            NativeErrorCode::MemoryBudgetExceeded,
            format!(
                "generated UTF-8 output exceeds the {MAX_GENERATED_OUTPUT_BYTES}-byte per-case ceiling"
            ),
        ));
    }
    Ok(next)
}

fn generation_verification_error(message: impl Into<String>) -> NativeError {
    NativeError::new(NativeErrorCode::Internal, message)
}

fn generation_case_tokens(
    model: &LlamaModel,
    case: &GenerationCase,
    case_index: usize,
) -> NativeResult<Vec<LlamaToken>> {
    match &case.input {
        GenerationInput::Chat { messages, template } => {
            let rendered =
                render_messages_prompt_with_template(model, messages.clone(), true, template)?;
            model
                .str_to_token(&rendered, AddBos::Always)
                .map_err(|error| {
                    NativeError::new(
                        NativeErrorCode::ModelInvalid,
                        format!("failed to tokenize generation case {case_index}: {error}"),
                    )
                })
        }
        GenerationInput::Completion { prompts } => {
            let Some(prompt) = prompts.first() else {
                return Err(NativeError::new(
                    NativeErrorCode::InvalidConfig,
                    format!("generation case {case_index} has no completion prompt"),
                ));
            };
            completion_prompt_tokens(model, prompt, case_index)
        }
        GenerationInput::FillInMiddle { .. } => Err(NativeError::new(
            NativeErrorCode::UnsupportedPromptForm,
            format!(
                "generation case {case_index} requested fill-in-middle without a verified model-specific token contract"
            ),
        )),
    }
}

fn completion_prompt_tokens(
    model: &LlamaModel,
    prompt: &CompletionPrompt,
    index: usize,
) -> NativeResult<Vec<LlamaToken>> {
    match prompt {
        CompletionPrompt::Text {
            text,
            special_tokens,
        } => model
            .str_to_token(
                text,
                match special_tokens {
                    SpecialTokenPolicy::NoBosParseSpecial => AddBos::Never,
                    SpecialTokenPolicy::AddBosParseSpecial => AddBos::Always,
                },
            )
            .map_err(|error| {
                NativeError::new(
                    NativeErrorCode::ModelInvalid,
                    format!("failed to tokenize completion prompt {index}: {error}"),
                )
            }),
        CompletionPrompt::Tokens { token_ids } => {
            let vocabulary_size = model.n_vocab();
            if let Some(invalid) = token_ids
                .iter()
                .find(|token| **token < 0 || **token >= vocabulary_size)
            {
                return Err(NativeError::new(
                    NativeErrorCode::InvalidConfig,
                    format!(
                        "completion prompt {index} contains token ID {invalid} outside vocabulary 0..{vocabulary_size}"
                    ),
                ));
            }
            Ok(token_ids.iter().copied().map(LlamaToken::new).collect())
        }
    }
}

fn prepare_input(model: &LlamaModel, input: GenerationInput) -> NativeResult<Vec<PreparedPrompt>> {
    match input {
        GenerationInput::Chat { messages, template } => {
            let rendered = render_messages_prompt_with_template(model, messages, true, &template)?;
            let tokens = model
                .str_to_token(&rendered, AddBos::Always)
                .map_err(|error| {
                    NativeError::new(
                        NativeErrorCode::ModelInvalid,
                        format!("failed to tokenize rendered chat prompt: {error}"),
                    )
                })?;
            Ok(vec![PreparedPrompt {
                input_index: 0,
                prompt_form: PromptForm::Chat,
                token_policy: PromptTokenPolicy::ChatTemplate,
                source_sha256: format!("{:x}", Sha256::digest(rendered.as_bytes())),
                token_ids: tokens.into_iter().map(|token| token.0).collect(),
            }])
        }
        GenerationInput::Completion { prompts } => prompts
            .iter()
            .enumerate()
            .map(|(index, prompt)| {
                let tokens = completion_prompt_tokens(model, prompt, index)?;
                let (token_policy, source_sha256) = match prompt {
                    CompletionPrompt::Text {
                        text,
                        special_tokens,
                    } => (
                        match special_tokens {
                            SpecialTokenPolicy::NoBosParseSpecial => {
                                PromptTokenPolicy::NoBosParseSpecial
                            }
                            SpecialTokenPolicy::AddBosParseSpecial => {
                                PromptTokenPolicy::AddBosParseSpecial
                            }
                        },
                        format!("{:x}", Sha256::digest(text.as_bytes())),
                    ),
                    CompletionPrompt::Tokens { token_ids } => {
                        let mut hasher = Sha256::new();
                        for token in token_ids {
                            hasher.update(token.to_le_bytes());
                        }
                        (
                            PromptTokenPolicy::ExactTokenIds,
                            format!("{:x}", hasher.finalize()),
                        )
                    }
                };
                Ok(PreparedPrompt {
                    input_index: index,
                    prompt_form: PromptForm::Completion,
                    token_policy,
                    source_sha256,
                    token_ids: tokens.into_iter().map(|token| token.0).collect(),
                })
            })
            .collect(),
        GenerationInput::FillInMiddle { .. } => Err(NativeError::new(
            NativeErrorCode::UnsupportedPromptForm,
            "fill-in-middle prompt preparation requires a verified model-specific FIM token contract",
        )),
    }
}

fn prefill_shared_prefix(
    model: &LlamaModel,
    context: &mut LlamaContext<'_>,
    request: &SharedPrefixBatchRequest,
    sequence_token_counts: &mut HashMap<i32, usize>,
    sequence_token_ids: &mut HashMap<i32, Vec<i32>>,
) -> NativeResult<SequenceStateBlob> {
    context.clear_kv_cache();
    sequence_token_counts.clear();
    sequence_token_ids.clear();
    let prompt = render_messages_prompt_with_template(
        model,
        request.common_messages.clone(),
        false,
        &request.chat_template,
    )?;
    let tokens = model
        .str_to_token(&prompt, AddBos::Always)
        .map_err(|error| {
            NativeError::new(
                NativeErrorCode::ModelInvalid,
                format!("failed to tokenize the common cache prefix: {error}"),
            )
        })?;
    if tokens.is_empty() {
        return Err(NativeError::new(
            NativeErrorCode::CacheIncompatible,
            "the common messages produced no reusable rendered prompt prefix",
        ));
    }
    decode_tokens_chunked(context, &tokens, 0, 0, false)?;
    let token_ids = tokens.iter().map(|token| token.0).collect::<Vec<_>>();
    sequence_token_counts.insert(0, tokens.len());
    sequence_token_ids.insert(0, token_ids.clone());
    state_buffer::export_sequence(context, 0, tokens.len(), token_ids)
}

fn generate_multimodal(
    model: &LlamaModel,
    context: &mut LlamaContext<'_>,
    multimodal: Option<&MtmdContext>,
    request: &GenerationRequest,
    supervision: SingleSequenceSupervision<'_>,
    tracking: SequenceTracking<'_>,
) -> NativeResult<GenerationOutput> {
    let Some(multimodal) = multimodal else {
        return Err(NativeError::new(
            NativeErrorCode::InvalidConfig,
            "multimodal generation requires a loaded mmproj GGUF",
        ));
    };
    context.clear_kv_cache();
    tracking.token_counts.clear();
    tracking.token_ids.clear();
    let started = Instant::now();
    let prompt = render_multimodal_prompt(model, request)?;
    let supported_media_kinds = media_kinds_for_context(multimodal);
    let bitmaps = request
        .media
        .iter()
        .map(|media| media_bitmap(multimodal, &supported_media_kinds, media))
        .collect::<NativeResult<Vec<_>>>()?;
    let bitmap_refs = bitmaps.iter().collect::<Vec<_>>();
    let chunks = multimodal
        .tokenize(
            MtmdInputText {
                text: prompt,
                add_special: true,
                parse_special: true,
            },
            &bitmap_refs,
        )
        .map_err(|error| {
            NativeError::new(
                NativeErrorCode::ModelInvalid,
                format!("failed to tokenize multimodal prompt: {error}"),
            )
        })?;
    let prompt_tokens = chunks.total_tokens();
    if prompt_tokens.saturating_add(request.sampling.max_tokens as usize) > context.n_ctx() as usize
    {
        return Err(NativeError::new(
            NativeErrorCode::PromptTooLarge,
            "multimodal prompt and completion exceed the native context",
        ));
    }
    emit_generation_state(
        supervision.event_tx,
        request,
        0,
        GenerationState::Prefilling,
    );
    let mut next_position = chunks
        .eval_chunks(multimodal, context, 0, 0, context.n_batch() as i32, true)
        .map_err(|error| {
            NativeError::new(
                NativeErrorCode::DecodeFailed,
                format!("failed to evaluate multimodal prompt: {error}"),
            )
        })?;
    tracking
        .token_counts
        .insert(0, next_position.max(0) as usize);
    emit_generation_state(
        supervision.event_tx,
        request,
        1,
        GenerationState::Generating,
    );
    let mut sampler = build_sampler(model, &request.sampling);
    let mut decoder = UTF_8.new_decoder();
    let mut text = String::new();
    let mut generated_token_ids = Vec::with_capacity(request.sampling.max_tokens as usize);
    let mut generated = 0_usize;
    let mut event_index = 2_u64;
    let mut first_token_ms = None;
    let mut forced_tokens = VecDeque::new();
    let finish_reason = loop {
        if supervision.cancellation.load(Ordering::Acquire) {
            let _ = context.clear_kv_cache_seq(Some(0), None, None);
            break "cancelled".to_string();
        }
        if supervision.reasoning_force.load(Ordering::Acquire)
            && forced_tokens.is_empty()
            && let Some(end_marker) = active_reasoning_end_marker(&text)
        {
            let tokens = model
                .str_to_token(end_marker, AddBos::Never)
                .map_err(|error| {
                    NativeError::new(
                        NativeErrorCode::ModelInvalid,
                        format!("failed to tokenize the reasoning end marker: {error}"),
                    )
                })?;
            forced_tokens.extend(tokens);
            supervision.reasoning_force.store(false, Ordering::Release);
        }
        // `sample` samples and accepts. Only an externally forced token needs
        // an explicit accept here; accepting again corrupts stateful samplers.
        let token = if let Some(token) = forced_tokens.pop_front() {
            sampler.accept(token);
            token
        } else {
            sampler.sample(context, -1)
        };
        if model.is_eog_token(token) {
            break "end_of_generation".to_string();
        }
        generated_token_ids.push(token.0);
        let bytes = model
            .token_to_piece_bytes(token, 512, false, None)
            .map_err(|error| {
                NativeError::new(
                    NativeErrorCode::DecodeFailed,
                    format!("failed to decode generated token: {error}"),
                )
            })?;
        let piece = decode_generated_utf8_piece(&mut decoder, &bytes, false)?;
        if first_token_ms.is_none() {
            first_token_ms = Some(started.elapsed().as_millis());
        }
        append_generated_utf8_piece(&mut text, &piece)?;
        generated += 1;
        if !piece.is_empty() {
            try_emit_nonterminal(
                supervision.event_tx,
                GenerationEvent {
                    request_id: request.request_id.clone(),
                    branch_id: "assistant".to_string(),
                    sequence_id: 0,
                    input_index: 0,
                    event_index,
                    event: GenerationEventKind::Delta { text: piece },
                },
            );
            event_index += 1;
        }
        if let Some(stop) = request
            .sampling
            .stop
            .iter()
            .find(|stop| !stop.is_empty() && text.ends_with(stop.as_str()))
        {
            text.truncate(text.len().saturating_sub(stop.len()));
            break "stop_sequence".to_string();
        }
        if generated >= request.sampling.max_tokens as usize {
            break "max_tokens".to_string();
        }
        let mut batch = LlamaBatch::new(1, 1);
        batch
            .add(token, next_position, &[0], true)
            .map_err(|error| {
                native_decode_error("failed to build multimodal decode batch", error)
            })?;
        context
            .decode(&mut batch)
            .map_err(|error| native_decode_error("failed to continue multimodal decode", error))?;
        next_position += 1;
        tracking.token_counts.insert(0, next_position as usize);
    };
    let final_piece = decode_generated_utf8_piece(&mut decoder, &[], true)?;
    append_generated_utf8_piece(&mut text, &final_piece)?;
    if !final_piece.is_empty() {
        try_emit_nonterminal(
            supervision.event_tx,
            GenerationEvent {
                request_id: request.request_id.clone(),
                branch_id: "assistant".to_string(),
                sequence_id: 0,
                input_index: 0,
                event_index,
                event: GenerationEventKind::Delta { text: final_piece },
            },
        );
        event_index += 1;
    }
    let state = if finish_reason == "cancelled" {
        GenerationState::Cancelled
    } else {
        GenerationState::Completed
    };
    emit_generation_state(supervision.event_tx, request, event_index, state);
    let duration_ms = started.elapsed().as_millis();
    Ok(GenerationOutput {
        request_id: request.request_id.clone(),
        branch_id: "assistant".to_string(),
        input_index: 0,
        model_id: request.model_id.clone(),
        text,
        generated_token_ids,
        token_observations: None,
        state,
        finish_reason,
        metrics: GenerationMetrics {
            prompt_tokens,
            completion_tokens: generated,
            shared_prefix_tokens: 0,
            duration_ms,
            first_token_ms,
            tokens_per_second: if duration_ms == 0 {
                0.0
            } else {
                generated as f64 / (duration_ms as f64 / 1000.0)
            },
            cache: GenerationCacheMetrics::default(),
        },
        real_engine_invoked: true,
        fake_fixture: false,
        transport: NativeTransport::InProcess,
    })
}

struct SequenceTracking<'a> {
    token_counts: &'a mut HashMap<i32, usize>,
    token_ids: &'a mut HashMap<i32, Vec<i32>>,
}

struct BatchSupervision<'a> {
    event_tx: &'a Sender<GenerationEvent>,
    retained_events: Option<&'a mut Vec<GenerationEvent>>,
    retain_token_piece_traces: bool,
    unrecorded_control_used: Option<&'a mut bool>,
    runtime_sample_trace: Option<&'a mut Vec<RuntimeSampleSelection>>,
    cancellations: &'a [Arc<AtomicBool>],
    reasoning_forces: &'a [Arc<AtomicBool>],
}

impl BatchSupervision<'_> {
    fn mark_unrecorded_control(&mut self) {
        if let Some(used) = self.unrecorded_control_used.as_deref_mut() {
            *used = true;
        }
    }

    fn record_runtime_sample(
        &mut self,
        case_index: usize,
        generated_index: usize,
        token_id: i32,
        terminal: bool,
    ) {
        if let Some(trace) = self.runtime_sample_trace.as_deref_mut() {
            trace.push(RuntimeSampleSelection {
                case_index,
                generated_index,
                token_id,
                terminal,
            });
        }
    }

    fn emit(&mut self, event: GenerationEvent) {
        if let Some(retained_events) = self.retained_events.as_deref_mut() {
            retained_events.push(event.clone());
        }
        if matches!(
            &event.event,
            GenerationEventKind::State { state } if is_terminal_state(*state)
        ) {
            try_emit_terminal(self.event_tx, event);
        } else {
            try_emit_nonterminal(self.event_tx, event);
        }
    }

    fn emit_state(
        &mut self,
        request: &SharedPrefixBatchRequest,
        branch: &BranchRequest,
        sequence_id: usize,
        event_index: u64,
        state: GenerationState,
    ) {
        self.emit(GenerationEvent {
            request_id: request.request_id.clone(),
            branch_id: branch.branch_id.clone(),
            sequence_id: sequence_id as i32,
            input_index: sequence_id,
            event_index,
            event: GenerationEventKind::State { state },
        });
    }
}

struct SingleSequenceSupervision<'a> {
    event_tx: &'a Sender<GenerationEvent>,
    cancellation: &'a Arc<AtomicBool>,
    reasoning_force: &'a Arc<AtomicBool>,
}

fn media_kinds_for_context(context: &MtmdContext) -> Vec<MediaKind> {
    media_kinds_from_support(context.support_vision(), context.support_audio())
}

fn media_kinds_from_support(support_vision: bool, support_audio: bool) -> Vec<MediaKind> {
    let mut media_kinds = Vec::with_capacity(2);
    if support_vision {
        media_kinds.push(MediaKind::Image);
    }
    if support_audio {
        media_kinds.push(MediaKind::Audio);
    }
    media_kinds
}

fn validate_declared_media_kind(
    media_id: &str,
    declared_kind: MediaKind,
    supported_media_kinds: &[MediaKind],
) -> NativeResult<()> {
    if supported_media_kinds.contains(&declared_kind) {
        return Ok(());
    }
    Err(NativeError::new(
        NativeErrorCode::UnsupportedMedia,
        format!(
            "media {media_id} declares {} input, which the loaded multimodal projector does not support",
            media_kind_name(declared_kind)
        ),
    ))
}

fn validate_decoded_media_kind(
    media_id: &str,
    declared_kind: MediaKind,
    decoded_is_audio: bool,
) -> NativeResult<()> {
    let decoded_kind = if decoded_is_audio {
        MediaKind::Audio
    } else {
        MediaKind::Image
    };
    if decoded_kind == declared_kind {
        return Ok(());
    }
    Err(NativeError::new(
        NativeErrorCode::UnsupportedMedia,
        format!(
            "media {media_id} declares {} input but decoded as {}",
            media_kind_name(declared_kind),
            media_kind_name(decoded_kind)
        ),
    ))
}

fn media_kind_name(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Image => "image",
        MediaKind::Audio => "audio",
    }
}

fn media_bitmap(
    context: &MtmdContext,
    supported_media_kinds: &[MediaKind],
    media: &MediaInput,
) -> NativeResult<MtmdBitmap> {
    validate_declared_media_kind(&media.id, media.kind, supported_media_kinds)?;
    let bitmap = MtmdBitmap::from_buffer(context, &media.bytes, false).map_err(|error| {
        NativeError::new(
            NativeErrorCode::ModelInvalid,
            format!("failed to decode media {}: {error}", media.id),
        )
    })?;
    validate_decoded_media_kind(&media.id, media.kind, bitmap.is_audio())?;
    bitmap.set_id(&media.sha256).map_err(|error| {
        NativeError::new(
            NativeErrorCode::InvalidConfig,
            format!("invalid media identity {}: {error}", media.id),
        )
    })?;
    Ok(bitmap)
}

fn render_multimodal_prompt(
    model: &LlamaModel,
    request: &GenerationRequest,
) -> NativeResult<String> {
    let GenerationInput::Chat { messages, template } = &request.input else {
        return Err(NativeError::new(
            NativeErrorCode::UnsupportedPromptForm,
            "multimodal generation requires chat input",
        ));
    };
    let mut messages = messages.clone();
    let markers = request
        .media
        .iter()
        .map(|media| format!("{}\n{}", media.id, mtmd_default_marker()))
        .collect::<Vec<_>>()
        .join("\n\n");
    if let Some(message) = messages
        .iter_mut()
        .rev()
        .find(|message| message.role == ChatRole::User)
    {
        message.content = format!("{markers}\n\n{}", message.content);
    } else {
        messages.push(ChatMessage {
            role: ChatRole::User,
            content: markers,
        });
    }
    render_messages_prompt_with_template(model, messages, true, template)
}

fn emit_generation_state(
    event_tx: &Sender<GenerationEvent>,
    request: &GenerationRequest,
    event_index: u64,
    state: GenerationState,
) {
    let event = GenerationEvent {
        request_id: request.request_id.clone(),
        branch_id: "assistant".to_string(),
        sequence_id: 0,
        input_index: 0,
        event_index,
        event: GenerationEventKind::State { state },
    };
    if is_terminal_state(state) {
        try_emit_terminal(event_tx, event);
    } else {
        try_emit_nonterminal(event_tx, event);
    }
}

struct ActiveBranch<'a> {
    sequence_id: i32,
    request: &'a BranchRequest,
    sampler: LlamaSampler,
    decoder: encoding_rs::Decoder,
    text: String,
    generated_token_ids: Vec<i32>,
    token_piece_trace: Option<TokenPieceTrace>,
    terminal_sampled_token_id: Option<i32>,
    generated: usize,
    next_position: i32,
    logit_index: i32,
    state: GenerationState,
    finish_reason: String,
    event_index: u64,
    first_token_ms: Option<u128>,
    forced_tokens: VecDeque<LlamaToken>,
}

fn active_reasoning_end_marker(text: &str) -> Option<&'static str> {
    [
        ("<think>", "</think>"),
        (
            "<<<reasoning_content_start>>>",
            "<<<reasoning_content_end>>>",
        ),
        ("[Start thinking]", "[End thinking]"),
    ]
    .into_iter()
    .find_map(|(start, end)| {
        let latest_start = text.rfind(start)?;
        let latest_end = text.rfind(end);
        (latest_end.is_none_or(|end_index| latest_start > end_index)).then_some(end)
    })
}

fn decode_tokens_chunked(
    context: &mut LlamaContext<'_>,
    tokens: &[LlamaToken],
    sequence_id: i32,
    start_position: i32,
    final_logits: bool,
) -> NativeResult<()> {
    let chunk_size = context.n_batch().max(1) as usize;
    for (chunk_index, chunk) in tokens.chunks(chunk_size).enumerate() {
        let mut batch = LlamaBatch::new(chunk.len(), 1);
        for (offset, token) in chunk.iter().enumerate() {
            let absolute_offset = chunk_index * chunk_size + offset;
            batch
                .add(
                    *token,
                    start_position + absolute_offset as i32,
                    &[sequence_id],
                    final_logits && absolute_offset + 1 == tokens.len(),
                )
                .map_err(|error| native_decode_error("failed to build prompt batch", error))?;
        }
        context
            .decode(&mut batch)
            .map_err(|error| native_decode_error("failed to decode prompt batch", error))?;
    }
    Ok(())
}

fn render_branch_prompts(
    model: &LlamaModel,
    request: &SharedPrefixBatchRequest,
) -> NativeResult<Vec<String>> {
    request
        .branches
        .iter()
        .map(|branch| {
            let mut messages = if branch.messages.is_empty() {
                request.common_messages.clone()
            } else {
                branch.messages.clone()
            };
            if !branch.instruction.trim().is_empty() {
                messages.push(ChatMessage {
                    role: ChatRole::User,
                    content: format!(
                        "For this response, use this reasoning perspective:\n{}",
                        branch.instruction.trim()
                    ),
                });
            }
            apply_model_chat_template(model, messages, true, &request.chat_template)
        })
        .collect()
}

fn render_messages_prompt_with_template(
    model: &LlamaModel,
    messages: Vec<ChatMessage>,
    add_assistant: bool,
    choice: &ChatTemplateChoice,
) -> NativeResult<String> {
    apply_model_chat_template(model, messages, add_assistant, choice)
}

fn apply_model_chat_template(
    model: &LlamaModel,
    messages: Vec<ChatMessage>,
    add_assistant: bool,
    choice: &ChatTemplateChoice,
) -> NativeResult<String> {
    let template = match choice {
        ChatTemplateChoice::ModelDefault => model.chat_template(None).map_err(|error| {
            NativeError::new(
                NativeErrorCode::ModelInvalid,
                format!("model has no usable chat template: {error}"),
            )
        })?,
        ChatTemplateChoice::Override(template) => {
            LlamaChatTemplate::new(template).map_err(|error| {
                NativeError::new(
                    NativeErrorCode::InvalidConfig,
                    format!("invalid frozen chat template: {error}"),
                )
            })?
        }
    };
    let native_messages = native_chat_messages(messages)?;
    match model.apply_chat_template(&template, &native_messages, add_assistant) {
        Ok(rendered) => Ok(rendered),
        Err(primary_error) => {
            if matches!(choice, ChatTemplateChoice::Override(_)) {
                return Err(NativeError::new(
                    NativeErrorCode::ModelInvalid,
                    format!("failed to apply frozen chat template: {primary_error}"),
                ));
            }
            let embedded_template = template.to_str().unwrap_or_default();
            let architecture = model
                .meta_val_str("general.architecture")
                .unwrap_or_default();
            let Some(fallback_name) = fallback_chat_template_name(&architecture, embedded_template)
            else {
                return Err(NativeError::new(
                    NativeErrorCode::ModelInvalid,
                    format!("failed to apply model chat template: {primary_error}"),
                ));
            };
            let fallback = LlamaChatTemplate::new(fallback_name).map_err(|error| {
                NativeError::new(
                    NativeErrorCode::ModelInvalid,
                    format!(
                        "failed to construct `{fallback_name}` chat-template fallback: {error}"
                    ),
                )
            })?;
            model
                .apply_chat_template(&fallback, &native_messages, add_assistant)
                .map_err(|fallback_error| {
                    NativeError::new(
                        NativeErrorCode::ModelInvalid,
                        format!(
                            "failed to apply embedded chat template ({primary_error}) and model-compatible `{fallback_name}` fallback ({fallback_error})"
                        ),
                    )
                })
        }
    }
}

fn fallback_chat_template_name<'a>(
    architecture: &str,
    embedded_template: &'a str,
) -> Option<&'a str> {
    if architecture.starts_with("gemma") || embedded_template.contains("<start_of_turn>") {
        Some("gemma")
    } else {
        None
    }
}

fn tokenize_messages(
    model: &LlamaModel,
    messages: Vec<ChatMessage>,
    chat_template: &ChatTemplateChoice,
) -> NativeResult<TokenizedPrompt> {
    let rendered = render_messages_prompt_with_template(model, messages, true, chat_template)?;
    let tokens = model
        .str_to_token(&rendered, AddBos::Always)
        .map_err(|error| {
            NativeError::new(
                NativeErrorCode::ModelInvalid,
                format!("failed to tokenize rendered chat prompt: {error}"),
            )
        })?;
    Ok(TokenizedPrompt {
        rendered_sha256: format!("{:x}", Sha256::digest(rendered.as_bytes())),
        token_ids: tokens.into_iter().map(|token| token.0).collect(),
    })
}

fn native_chat_messages(messages: Vec<ChatMessage>) -> NativeResult<Vec<LlamaChatMessage>> {
    messages
        .into_iter()
        .map(|message| {
            LlamaChatMessage::new(role_name(message.role).to_string(), message.content).map_err(
                |error| {
                    NativeError::new(
                        NativeErrorCode::InvalidConfig,
                        format!("chat message contains invalid data: {error}"),
                    )
                },
            )
        })
        .collect()
}

fn role_name(role: ChatRole) -> &'static str {
    match role {
        ChatRole::System => "system",
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
        ChatRole::Tool => "tool",
    }
}

fn build_sampler(model: &LlamaModel, config: &SamplingConfig) -> LlamaSampler {
    if config.temperature <= 0.0 {
        return LlamaSampler::greedy();
    }
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
                } else {
                    samplers.push(LlamaSampler::temp(config.temperature));
                }
            }
            _ => {}
        }
    }
    samplers.push(LlamaSampler::dist(config.seed));
    LlamaSampler::chain_simple(samplers)
}

fn longest_common_prefix(token_sets: &[Vec<LlamaToken>]) -> usize {
    let Some(first) = token_sets.first() else {
        return 0;
    };
    (0..first.len())
        .take_while(|index| {
            token_sets
                .iter()
                .skip(1)
                .all(|tokens| tokens.get(*index) == first.get(*index))
        })
        .count()
}

fn exact_budget_execution_values(
    budget: &ExactTokenBatchCellBudget,
    request: &SharedPrefixBatchRequest,
    token_sets: &[Vec<LlamaToken>],
    cached_states: &[Option<&SequenceStateBlob>],
    prefix_lengths: &mut [usize],
) -> NativeResult<(usize, usize)> {
    if budget.cases().len() != request.branches.len()
        || token_sets.len() != request.branches.len()
        || cached_states.len() != request.branches.len()
        || prefix_lengths.len() != request.branches.len()
    {
        return Err(generation_verification_error(
            "preflight exact-token cell budget lost batch cardinality",
        ));
    }
    let shared_uncached_prefix = usize::try_from(budget.shared_uncached_prefix_tokens())
        .map_err(|_| batch_cell_budget_error("shared prefix does not fit this host"))?;
    let required_cells = usize::try_from(budget.required_cells())
        .map_err(|_| batch_cell_budget_error("required cells do not fit this host"))?;

    let mut observed_cached_prefix_tokens = 0_u64;
    for (index, case_budget) in budget.cases().iter().enumerate() {
        let actual_cached_prefix = cached_states[index].map_or(0, |state| state.token_count);
        let actual_cached_prefix = u64::try_from(actual_cached_prefix)
            .map_err(|_| batch_cell_budget_error("cached prefix does not fit u64"))?;
        let expected_reused_prefix = if cached_states[index].is_some() {
            actual_cached_prefix
        } else {
            budget.shared_uncached_prefix_tokens()
        };
        let actual_prompt_tokens = u64::try_from(token_sets[index].len())
            .map_err(|_| batch_cell_budget_error("prompt length does not fit u64"))?;
        if case_budget.input_index() != index
            || case_budget.prompt_tokens() != actual_prompt_tokens
            || case_budget.cached_prefix_tokens() != actual_cached_prefix
            || case_budget.reused_prefix_tokens() != expected_reused_prefix
            || case_budget.maximum_sampled_tokens()
                != u64::from(request.branches[index].sampling.max_tokens)
        {
            return Err(generation_verification_error(
                "preflight exact-token cell budget disagrees with prepared execution",
            ));
        }
        observed_cached_prefix_tokens = observed_cached_prefix_tokens
            .checked_add(actual_cached_prefix)
            .ok_or_else(|| batch_cell_budget_error("cached-prefix cell count overflowed"))?;
        prefix_lengths[index] = usize::try_from(case_budget.reused_prefix_tokens())
            .map_err(|_| batch_cell_budget_error("reused prefix does not fit this host"))?;
    }
    if observed_cached_prefix_tokens != budget.cached_prefix_tokens() {
        return Err(generation_verification_error(
            "preflight exact-token cached-prefix total disagrees with execution",
        ));
    }
    Ok((shared_uncached_prefix, required_cells))
}

fn batch_cell_budget_error(message: impl Into<String>) -> NativeError {
    NativeError::new(NativeErrorCode::MemoryBudgetExceeded, message)
}

fn validate_config(config: &NativeModelConfig) -> NativeResult<()> {
    if config.max_sequences == 0 || config.max_sequences > MAX_PARALLEL_SEQUENCES {
        return Err(NativeError::new(
            NativeErrorCode::InvalidConfig,
            format!("max_sequences must be between 1 and {MAX_PARALLEL_SEQUENCES}"),
        ));
    }
    if config.batch_tokens == 0 || config.context_tokens < 512 {
        return Err(NativeError::new(
            NativeErrorCode::InvalidConfig,
            "batch_tokens must be positive and context_tokens must be at least 512",
        ));
    }
    if !config.model_path.is_file() {
        return Err(NativeError::new(
            NativeErrorCode::ModelMissing,
            format!("model file does not exist: {}", config.model_path.display()),
        ));
    }
    validate_expected_sha256(
        "expected_model_sha256",
        config.expected_model_sha256.as_deref(),
    )?;
    validate_expected_sha256(
        "expected_mmproj_sha256",
        config.expected_mmproj_sha256.as_deref(),
    )?;
    if config.expected_mmproj_sha256.is_some() && config.mmproj_path.is_none() {
        return Err(NativeError::new(
            NativeErrorCode::InvalidConfig,
            "expected_mmproj_sha256 requires a multimodal projector path",
        ));
    }
    validate_gguf_header(&config.model_path)
}

fn validate_gguf_header(path: &Path) -> NativeResult<()> {
    let mut header = [0_u8; 4];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .map_err(|error| {
            NativeError::new(
                NativeErrorCode::ModelInvalid,
                format!("failed to inspect local model header: {error}"),
            )
        })?;
    if &header != b"GGUF" {
        return Err(NativeError::new(
            NativeErrorCode::ModelInvalid,
            "native model does not have a GGUF header",
        ));
    }
    Ok(())
}

fn validate_expected_sha256(field: &str, value: Option<&str>) -> NativeResult<()> {
    if let Some(value) = value
        && (value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    {
        return Err(NativeError::new(
            NativeErrorCode::InvalidConfig,
            format!("{field} must be exactly 64 lowercase hexadecimal characters"),
        ));
    }
    Ok(())
}

fn validate_batch_request(
    request: &SharedPrefixBatchRequest,
    status: &ResidentModelStatus,
) -> NativeResult<()> {
    if request.model_id != status.model_id {
        return Err(NativeError::new(
            NativeErrorCode::ModelNotLoaded,
            format!("model {} is not resident", request.model_id),
        ));
    }
    if request.branches.is_empty() || request.branches.len() > status.max_sequences as usize {
        return Err(NativeError::new(
            NativeErrorCode::InvalidConfig,
            format!("generation requires 1 to {} branches", status.max_sequences),
        ));
    }
    if request.common_messages.is_empty()
        && request
            .branches
            .iter()
            .any(|branch| branch.messages.is_empty())
    {
        return Err(NativeError::new(
            NativeErrorCode::InvalidConfig,
            "generation requires common messages or complete per-branch messages",
        ));
    }
    let mut ids = std::collections::HashSet::new();
    for branch in &request.branches {
        if branch.branch_id.trim().is_empty() || !ids.insert(branch.branch_id.as_str()) {
            return Err(NativeError::new(
                NativeErrorCode::InvalidConfig,
                "branch IDs must be non-empty and unique",
            ));
        }
        if branch.sampling.max_tokens == 0 {
            return Err(NativeError::new(
                NativeErrorCode::InvalidConfig,
                "max_tokens must be positive",
            ));
        }
    }
    Ok(())
}

fn validate_generation_request(
    request: &GenerationRequest,
    status: &ResidentModelStatus,
) -> NativeResult<()> {
    if request.model_id != status.model_id {
        return Err(NativeError::new(
            NativeErrorCode::ModelNotLoaded,
            format!("model {} is not resident", request.model_id),
        ));
    }
    let message_count = match &request.input {
        GenerationInput::Chat { messages, .. } => messages.len(),
        _ => 0,
    };
    if message_count == 0 || request.sampling.max_tokens == 0 {
        return Err(NativeError::new(
            NativeErrorCode::InvalidConfig,
            "multimodal generation requires messages and a positive token limit",
        ));
    }
    if request.media.is_empty() {
        return Err(NativeError::new(
            NativeErrorCode::InvalidConfig,
            "multimodal generation requires at least one media item",
        ));
    }
    let supported_media_kinds = status
        .descriptor
        .as_ref()
        .map(|descriptor| descriptor.capabilities.media_kinds.as_slice())
        .unwrap_or_default();
    for media in &request.media {
        validate_declared_media_kind(&media.id, media.kind, supported_media_kinds)?;
    }
    Ok(())
}

fn validate_generation_batch_request(
    request: &GenerationBatchRequest,
    status: &ResidentModelStatus,
) -> NativeResult<()> {
    if request.model_id != status.model_id {
        return Err(NativeError::new(
            NativeErrorCode::ModelNotLoaded,
            format!("model {} is not resident", request.model_id),
        ));
    }
    if request.cases.is_empty() || request.cases.len() > status.max_sequences as usize {
        return Err(NativeError::new(
            NativeErrorCode::InvalidConfig,
            format!(
                "batch generation requires 1 to {} cases",
                status.max_sequences
            ),
        ));
    }
    let mut case_ids = std::collections::HashSet::with_capacity(request.cases.len());
    for (index, case) in request.cases.iter().enumerate() {
        if case.case_id.trim().is_empty() || !case_ids.insert(case.case_id.as_str()) {
            return Err(NativeError::new(
                NativeErrorCode::InvalidConfig,
                "generation case IDs must be non-empty and unique",
            ));
        }
        if case.sampling.max_tokens == 0 {
            return Err(NativeError::new(
                NativeErrorCode::InvalidConfig,
                format!("generation case {index} has a zero token limit"),
            ));
        }
        match &case.input {
            GenerationInput::Chat { messages, .. } if messages.is_empty() => {
                return Err(NativeError::new(
                    NativeErrorCode::InvalidConfig,
                    format!("generation case {index} has no chat messages"),
                ));
            }
            GenerationInput::Completion { prompts } if prompts.len() != 1 => {
                return Err(NativeError::new(
                    NativeErrorCode::InvalidConfig,
                    format!("generation case {index} must contain exactly one completion prompt"),
                ));
            }
            GenerationInput::Completion { prompts } => {
                let empty = match &prompts[0] {
                    CompletionPrompt::Text { text, .. } => text.is_empty(),
                    CompletionPrompt::Tokens { token_ids } => token_ids.is_empty(),
                };
                if empty {
                    return Err(NativeError::new(
                        NativeErrorCode::InvalidConfig,
                        format!("generation case {index} has an empty completion prompt"),
                    ));
                }
            }
            GenerationInput::FillInMiddle { .. } => {
                return Err(NativeError::new(
                    NativeErrorCode::UnsupportedPromptForm,
                    format!(
                        "generation case {index} requested fill-in-middle without a verified model-specific token contract"
                    ),
                ));
            }
            GenerationInput::Chat { .. } => {}
        }
    }
    Ok(())
}

fn exact_token_budget_for_submission(
    request: &GenerationBatchRequest,
    status: &ResidentModelStatus,
) -> NativeResult<Option<ExactTokenBatchCellBudget>> {
    let budget = match exact_token_batch_cell_budget(request) {
        Ok(budget) => budget,
        Err(ExactTokenBatchBudgetError::NonExactTokenCase { .. }) => return Ok(None),
        Err(ExactTokenBatchBudgetError::InvalidCachedPrefix { index }) => {
            return Err(NativeError::new(
                NativeErrorCode::CacheIncompatible,
                format!("generation case {index} has an invalid exact-token cached prefix"),
            ));
        }
        Err(
            error @ (ExactTokenBatchBudgetError::EmptyBatch
            | ExactTokenBatchBudgetError::EmptyPrompt { .. }
            | ExactTokenBatchBudgetError::NegativeTokenId { .. }
            | ExactTokenBatchBudgetError::ZeroCompletionBudget { .. }
            | ExactTokenBatchBudgetError::ArithmeticOverflow),
        ) => {
            return Err(NativeError::new(
                NativeErrorCode::InvalidConfig,
                error.to_string(),
            ));
        }
    };
    let context_tokens = status
        .fingerprint
        .as_ref()
        .map(|fingerprint| fingerprint.context_tokens)
        .ok_or_else(|| {
            NativeError::new(
                NativeErrorCode::ModelNotLoaded,
                "resident model has no exact context fingerprint",
            )
        })?;
    if !budget.fits(u64::from(context_tokens)) {
        return Err(NativeError::new(
            NativeErrorCode::PromptTooLarge,
            format!(
                "exact-token batch requires {} KV cells but the resident context has {context_tokens}",
                budget.required_cells()
            ),
        ));
    }
    Ok(Some(budget))
}

fn validate_embedding_batch_request(
    request: &EmbeddingBatchRequest,
    status: &ResidentModelStatus,
) -> NativeResult<()> {
    if status.state != ModelRuntimeState::Ready || status.fingerprint.is_none() {
        return Err(NativeError::new(
            NativeErrorCode::ModelNotLoaded,
            format!("model {} is not ready for embeddings", status.model_id),
        ));
    }
    if request.model_id() != status.model_id {
        return Err(NativeError::new(
            NativeErrorCode::ModelNotLoaded,
            format!("model {} is not resident", request.model_id()),
        ));
    }
    Ok(())
}

fn backend() -> NativeResult<&'static LlamaBackend> {
    match BACKEND.get_or_init(|| LlamaBackend::init().map_err(|error| error.to_string())) {
        Ok(backend) => Ok(backend),
        Err(message) => Err(NativeError::new(
            NativeErrorCode::Internal,
            format!("failed to initialize llama.cpp backend: {message}"),
        )),
    }
}

fn native_thread_count() -> i32 {
    std::thread::available_parallelism()
        .map(|parallelism| parallelism.get().min(16) as i32)
        .unwrap_or(4)
}

fn fingerprint_model(
    config: &NativeModelConfig,
    model: &LlamaModel,
    artifacts: &ModelArtifactGuards,
    execution_backend: &str,
    context_tokens: u32,
    rope_config_sha256: String,
    kv_layout_sha256: String,
) -> NativeResult<ModelFingerprint> {
    artifacts.verify_loaded_identities()?;
    let model_sha256 = artifacts.model.expected_sha256.clone();
    let chat_template = model
        .chat_template(None)
        .ok()
        .and_then(|template| template.to_string().ok())
        .unwrap_or_default();
    let chat_template_sha256 = format!("{:x}", Sha256::digest(chat_template.as_bytes()));
    let multimodal_projector_sha256 = artifacts
        .projector
        .as_ref()
        .map(|projector| projector.expected_sha256.clone());
    // This binds the private compiler recipe, reviewed static dependency
    // artifacts, and selected backend. It deliberately does not claim to hash
    // a loaded process image or a pathname-resolved host executable.
    let build_id = native_build_id(execution_backend);
    Ok(ModelFingerprint {
        model_id: config.model_id.clone(),
        model_size: artifacts.model.initial_state.length(),
        model_sha256: model_sha256.clone(),
        // The tokenizer is embedded in GGUF. Using the complete GGUF hash is
        // conservative: any tokenizer or model tensor change invalidates state.
        tokenizer_sha256: model_sha256,
        chat_template_sha256,
        multimodal_projector_sha256,
        binding_version: LLAMA_CPP_BINDING_VERSION.to_string(),
        build_id,
        backend: execution_backend.to_string(),
        context_tokens,
        batch_tokens: config.batch_tokens,
        max_sequences: config.max_sequences,
        rope_config_sha256,
        kv_layout_sha256,
    })
}

fn native_build_id(execution_backend: &str) -> String {
    native_build_id_from_private_digest(LLAMA_NATIVE_BUILD_MANIFEST_SHA256, execution_backend)
}

fn native_build_id_from_private_digest(
    private_build_digest: &str,
    execution_backend: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"llama-native-build-recipe-v3\0");
    hasher.update((private_build_digest.len() as u64).to_le_bytes());
    hasher.update(private_build_digest.as_bytes());
    hasher.update((execution_backend.len() as u64).to_le_bytes());
    hasher.update(execution_backend.as_bytes());
    format!("llama-native-build-v3-{:x}", hasher.finalize())
}

fn describe_model(
    config: &NativeModelConfig,
    model: &LlamaModel,
    fingerprint: &ModelFingerprint,
    media_kinds: Vec<MediaKind>,
) -> NativeModelDescriptor {
    let declared_name = model
        .meta_val_str("general.name")
        .ok()
        .filter(|name| !name.trim().is_empty());
    let base_model_name = model
        .meta_val_str("general.base_model.0.name")
        .ok()
        .filter(|name| !name.trim().is_empty());
    let display_name = inspected_display_name(
        declared_name.as_deref(),
        base_model_name.as_deref(),
        &config.model_id,
    );
    let architecture = model
        .meta_val_str("general.architecture")
        .unwrap_or_else(|_| "unknown".to_string());
    let chat_template_available = model
        .chat_template(None)
        .ok()
        .and_then(|template| template.to_string().ok())
        .is_some_and(|template| !template.trim().is_empty());
    let mut prompt_forms = vec![PromptForm::Completion];
    if chat_template_available {
        prompt_forms.insert(0, PromptForm::Chat);
    }
    let multimodal = !media_kinds.is_empty();
    let exact = inspected_capabilities(
        chat_template_available,
        fingerprint.max_sequences,
        &media_kinds,
    );
    NativeModelDescriptor {
        stable_model_id: format!("sha256:{}", fingerprint.model_sha256),
        model_id: config.model_id.clone(),
        display_name,
        architecture,
        parameter_count: model.n_params(),
        model_size: fingerprint.model_size,
        context_tokens: fingerprint.context_tokens,
        max_sequences: fingerprint.max_sequences,
        backend: fingerprint.backend.clone(),
        capabilities: ModelCapabilities {
            prompt_forms,
            chat_template_available,
            multimodal,
            media_kinds,
            streaming: true,
            cancellation: true,
            max_batch_inputs: fingerprint.max_sequences,
            sampling_parameters: vec![
                SamplingParameter::Seed,
                SamplingParameter::Temperature,
                SamplingParameter::DynamicTemperature,
                SamplingParameter::TopK,
                SamplingParameter::TopP,
                SamplingParameter::MinP,
                SamplingParameter::TypicalP,
                SamplingParameter::Xtc,
                SamplingParameter::RepeatPenalty,
                SamplingParameter::FrequencyPenalty,
                SamplingParameter::PresencePenalty,
                SamplingParameter::Dry,
                SamplingParameter::SamplerOrder,
                SamplingParameter::MaxTokens,
                SamplingParameter::Stop,
            ],
            exact,
        },
    }
}

fn inspected_display_name(
    declared_name: Option<&str>,
    base_model_name: Option<&str>,
    configured_model_id: &str,
) -> String {
    let declared_name = declared_name.map(str::trim).filter(|name| !name.is_empty());
    let declared_is_placeholder = declared_name
        .is_some_and(|name| matches!(name.to_ascii_lowercase().as_str(), "hf" | "gguf" | "model"));
    if !declared_is_placeholder && let Some(name) = declared_name {
        return name.to_string();
    }
    base_model_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .or(declared_name)
        .unwrap_or(configured_model_id)
        .to_string()
}

fn inspected_capabilities(
    chat_template_available: bool,
    max_sequences: u32,
    media_kinds: &[MediaKind],
) -> ExactModelCapabilities {
    let structured_constraints = llama_native_types::StructuredConstraintCapabilities::new(
        CapabilityDeclarationStatus::Inspected,
        true,
        true,
        Some(llama_native_types::MAX_CONSTRAINT_ARTIFACT_BYTES),
    )
    .unwrap_or_else(|error| panic!("static structured capabilities are invalid: {error}"));
    let distribution_observations = llama_native_types::DistributionObservationCapabilities::new(
        CapabilityDeclarationStatus::Inspected,
        llama_native_types::ProbabilityStageSupport {
            raw_model: true,
            post_constraint: true,
            post_guidance: true,
            post_sampler: true,
        },
        llama_native_types::DistributionValueKindSupport {
            logits: true,
            probabilities: true,
            log_probabilities: true,
        },
        true,
        Some(llama_native_types::MAX_DISTRIBUTION_OBSERVATION_TOP_K),
    )
    .unwrap_or_else(|error| panic!("static observation capabilities are invalid: {error}"));
    let extended_sampling = llama_native_types::ExtendedSamplingCapabilities::new(
        CapabilityDeclarationStatus::Inspected,
        llama_native_types::ExtendedSamplerSupport {
            mirostat_v1: true,
            mirostat_v2: true,
            eta_cutoff: true,
            sparse_logit_bias: true,
            top_n_sigma: true,
        },
        u16::try_from(llama_native_types::MAX_SPARSE_LOGIT_BIAS_ENTRIES).ok(),
    )
    .unwrap_or_else(|error| panic!("static sampler capabilities are invalid: {error}"));
    ExactModelCapabilities {
        declaration: CapabilityDeclarationStatus::Inspected,
        prompts: PromptInputCapabilities {
            chat: chat_template_available,
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
            max_cases: max_sequences,
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
        evidence: NativeEvidenceCapabilities {
            embeddings: EmbeddingCapabilities::default(),
            structured_constraints,
            distribution_observations,
            extended_sampling,
        },
        media: media_kinds
            .iter()
            .copied()
            .map(|kind| MediaInputCapability {
                kind,
                projector: ProjectorRequirement::Required,
                accepted_mime_types: None,
                max_objects_per_request: None,
                max_bytes_per_object: None,
                max_total_bytes_per_request: None,
            })
            .collect(),
    }
}

fn context_fingerprints(params: &LlamaContextParams) -> (String, String) {
    let rope = format!(
        "scaling={:?};base={:08x};scale={:08x}",
        params.rope_scaling_type(),
        params.rope_freq_base().to_bits(),
        params.rope_freq_scale().to_bits(),
    );
    let kv = format!(
        "ctx={};batch={};ubatch={};seq={};type_k={:?};type_v={:?};flash={:?};swa_full={};unified={}",
        params.n_ctx().map_or(0, NonZeroU32::get),
        params.n_batch(),
        params.n_ubatch(),
        params.n_seq_max(),
        params.type_k(),
        params.type_v(),
        params.flash_attention_policy(),
        params.swa_full(),
        params.kv_unified(),
    );
    (
        format!("{:x}", Sha256::digest(rope.as_bytes())),
        format!("{:x}", Sha256::digest(kv.as_bytes())),
    )
}

fn emit_failed_case_events(event_tx: &Sender<GenerationEvent>, request: &GenerationBatchRequest) {
    for (index, case) in request.cases.iter().enumerate() {
        try_emit_terminal(
            event_tx,
            GenerationEvent {
                request_id: request.request_id.clone(),
                branch_id: case.case_id.clone(),
                sequence_id: index as i32,
                input_index: index,
                event_index: u64::MAX,
                event: GenerationEventKind::State {
                    state: GenerationState::Failed,
                },
            },
        );
    }
}

fn emit_cancelled_case_events(
    event_tx: &Sender<GenerationEvent>,
    request: &GenerationBatchRequest,
) {
    for (index, case) in request.cases.iter().enumerate() {
        try_emit_terminal(
            event_tx,
            GenerationEvent {
                request_id: request.request_id.clone(),
                branch_id: case.case_id.clone(),
                sequence_id: index as i32,
                input_index: index,
                event_index: u64::MAX,
                event: GenerationEventKind::State {
                    state: GenerationState::Cancelled,
                },
            },
        );
    }
}

fn emit_failed_branch_events(
    event_tx: &Sender<GenerationEvent>,
    request: &SharedPrefixBatchRequest,
) {
    for (index, branch) in request.branches.iter().enumerate() {
        try_emit_terminal(
            event_tx,
            GenerationEvent {
                request_id: request.request_id.clone(),
                branch_id: branch.branch_id.clone(),
                sequence_id: index as i32,
                input_index: index,
                event_index: u64::MAX,
                event: GenerationEventKind::State {
                    state: GenerationState::Failed,
                },
            },
        );
    }
}

fn emit_cancelled_branch_events(
    event_tx: &Sender<GenerationEvent>,
    request: &SharedPrefixBatchRequest,
) {
    for (index, branch) in request.branches.iter().enumerate() {
        try_emit_terminal(
            event_tx,
            GenerationEvent {
                request_id: request.request_id.clone(),
                branch_id: branch.branch_id.clone(),
                sequence_id: index as i32,
                input_index: index,
                event_index: u64::MAX,
                event: GenerationEventKind::State {
                    state: GenerationState::Cancelled,
                },
            },
        );
    }
}

const fn is_terminal_state(state: GenerationState) -> bool {
    matches!(
        state,
        GenerationState::Completed | GenerationState::Cancelled | GenerationState::Failed
    )
}

fn try_emit_nonterminal(event_tx: &Sender<GenerationEvent>, event: GenerationEvent) {
    let terminal_reserve = MAX_PARALLEL_SEQUENCES as usize;
    if event_tx.len() < EVENT_CAPACITY.saturating_sub(terminal_reserve) {
        let _ = event_tx.try_send(event);
    }
}

fn try_emit_terminal(event_tx: &Sender<GenerationEvent>, event: GenerationEvent) {
    // Non-terminal producers always preserve one slot for every possible
    // sequence, so one owner-thread producer can publish every case terminal
    // without turning this bounded stream into a generation deadlock.
    let _ = event_tx.try_send(event);
}

fn set_status_state(
    status: &Arc<RwLock<ResidentModelStatus>>,
    state: ModelRuntimeState,
    active_sequences: usize,
) {
    if let Ok(mut status) = status.write() {
        status.state = state;
        status.active_sequences = active_sequences;
    }
}

fn native_decode_error(context: &str, error: impl std::fmt::Display) -> NativeError {
    NativeError::new(NativeErrorCode::DecodeFailed, format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use llama_native_types::EmbeddingInput;
    use std::sync::{Mutex, atomic::AtomicU64};

    type TestSealFixture = (
        GenerationBatchRequest,
        ModelFingerprint,
        Vec<GenerationOutput>,
        Vec<Option<i32>>,
        Vec<GenerationEvent>,
        Vec<String>,
    );

    static TEST_ARTIFACT_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);
    static REAL_MODEL_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn inspected_name_replaces_generic_hf_label_with_declared_base_model() {
        assert_eq!(
            inspected_display_name(
                Some("Hf"),
                Some("Gemma 4 12B It"),
                "content-addressed-model"
            ),
            "Gemma 4 12B It"
        );
        assert_eq!(
            inspected_display_name(
                Some("Writer fine-tune"),
                Some("Gemma 4 12B It"),
                "content-addressed-model"
            ),
            "Writer fine-tune"
        );
    }

    fn test_generation_reservation(
        request_id: &str,
        branches: &[&str],
    ) -> (Arc<ActiveRequest>, RequestLease) {
        let registry = Arc::new(RequestRegistry::new());
        let cancellations = branches
            .iter()
            .map(|branch| ((*branch).to_owned(), Arc::new(AtomicBool::new(false))))
            .collect();
        let reasoning_forces = branches
            .iter()
            .map(|branch| ((*branch).to_owned(), Arc::new(AtomicBool::new(false))))
            .collect();
        registry
            .reserve(
                request_id,
                RequestClass::Generation,
                RequestControls::Generation {
                    cancellations,
                    reasoning_forces,
                },
            )
            .expect("test request reserves")
    }

    fn test_embedding_reservation(
        request_id: &str,
        cancellation: Arc<AtomicBool>,
    ) -> (Arc<ActiveRequest>, RequestLease) {
        Arc::new(RequestRegistry::new())
            .reserve(
                request_id,
                RequestClass::Embedding,
                RequestControls::Embedding { cancellation },
            )
            .expect("test embedding request reserves")
    }

    fn require_ready<T, Output>(outcome: WaitOutcome<T, Output>) -> NativeResult<Output> {
        match outcome {
            WaitOutcome::Ready(output) => Ok(output),
            WaitOutcome::TimedOut(_) => Err(NativeError::new(
                NativeErrorCode::Internal,
                "test operation exceeded its explicit evidence timeout",
            )),
        }
    }

    fn test_admission_handle(
        status: ResidentModelStatus,
        queue_capacity: usize,
    ) -> (NativeModelHandle, Receiver<WorkerCommand>) {
        let (command_tx, command_rx) = bounded(queue_capacity);
        let (shutdown_tx, _shutdown_rx) = bounded(1);
        (
            NativeModelHandle {
                inner: Arc::new(NativeModelInner {
                    worker_identity: Arc::new(WorkerIdentity),
                    worker_id: "admission-test-worker".to_owned(),
                    command_tx,
                    shutdown_tx,
                    closing: AtomicBool::new(false),
                    admission: Mutex::new(()),
                    requests: Arc::new(RequestRegistry::new()),
                    status: Arc::new(RwLock::new(status)),
                }),
            },
            command_rx,
        )
    }

    fn test_worker_owner(
        model_id: &str,
        stopped: Arc<AtomicBool>,
    ) -> (NativeModelOwner, NativeModelHandle) {
        let (command_tx, command_rx) = bounded(COMMAND_CAPACITY);
        let (shutdown_tx, shutdown_rx) = bounded(1);
        let worker_id = format!("test-worker-{model_id}");
        let join = std::thread::spawn(move || {
            crossbeam_channel::select_biased! {
                recv(shutdown_rx) -> _ => {},
                recv(command_rx) -> _ => {},
            }
            stopped.store(true, Ordering::Release);
        });
        let inner = Arc::new(NativeModelInner {
            worker_identity: Arc::new(WorkerIdentity),
            worker_id: worker_id.clone(),
            command_tx,
            shutdown_tx,
            closing: AtomicBool::new(false),
            admission: Mutex::new(()),
            requests: Arc::new(RequestRegistry::with_external_worker(worker_id)),
            status: Arc::new(RwLock::new(ResidentModelStatus {
                model_id: model_id.to_string(),
                model_path: std::path::PathBuf::new(),
                state: ModelRuntimeState::Ready,
                fingerprint: None,
                descriptor: None,
                active_sequences: 0,
                max_sequences: 1,
            })),
        });
        let handle = NativeModelHandle {
            inner: Arc::clone(&inner),
        };
        (
            NativeModelOwner {
                inner,
                join: Some(join),
            },
            handle,
        )
    }

    #[test]
    fn unique_owner_revokes_live_clients_and_joins_without_arc_inference() {
        let stopped = Arc::new(AtomicBool::new(false));
        let (owner, client) = test_worker_owner("owned-shutdown-test", Arc::clone(&stopped));

        let joined = owner
            .shutdown_joined()
            .expect("unique owner joins despite live command client");
        assert!(joined.belongs_to(&client));
        assert_eq!(joined.expected_worker_count(), 1);
        assert_eq!(joined.joined_worker_count(), 1);
        assert_eq!(joined.expected_worker_ids(), joined.joined_worker_ids());
        assert!(stopped.load(Ordering::Acquire));
        assert_eq!(
            client
                .snapshot_sequence(0)
                .expect_err("live client is revoked after owner shutdown")
                .code,
            NativeErrorCode::WorkerStopped
        );
    }

    #[test]
    fn queued_generation_rejection_emits_one_cancelled_terminal_per_case() {
        let (request, _, _, _, _, _) = seal_fixture();
        let (event_tx, event_rx) = bounded(EVENT_CAPACITY);
        let (result_tx, result_rx) = bounded(1);
        let cancellations = request
            .cases
            .iter()
            .map(|_| Arc::new(AtomicBool::new(false)))
            .collect::<Vec<_>>();
        let case_ids = request
            .cases
            .iter()
            .map(|case| case.case_id.as_str())
            .collect::<Vec<_>>();
        let (_, request_lease) = test_generation_reservation(&request.request_id, &case_ids);
        reject_queued_command(WorkerCommand::GenerateBatch {
            request: request.clone(),
            exact_cell_budget: None,
            admission: GenerationBatchAdmission::Compatibility,
            event_tx,
            result_tx,
            cancellations: cancellations.clone(),
            reasoning_forces: request
                .cases
                .iter()
                .map(|_| Arc::new(AtomicBool::new(false)))
                .collect(),
            request_lease,
        });

        assert!(
            cancellations
                .iter()
                .all(|flag| flag.load(Ordering::Acquire))
        );
        assert_eq!(
            result_rx
                .recv()
                .expect("queued result is resolved")
                .expect_err("queued generation is cancelled")
                .code,
            NativeErrorCode::Cancelled
        );
        let terminals = event_rx.try_iter().collect::<Vec<_>>();
        assert_eq!(terminals.len(), request.cases.len());
        for (index, event) in terminals.iter().enumerate() {
            assert_eq!(event.input_index, index);
            assert_eq!(event.branch_id, request.cases[index].case_id);
            assert!(matches!(
                event.event,
                GenerationEventKind::State {
                    state: GenerationState::Cancelled
                }
            ));
        }
    }

    #[test]
    fn queued_rejection_publishes_terminal_before_releasing_executor_identity() {
        let (request, _, _, _, _, _) = seal_fixture();
        let registry = Arc::new(RequestRegistry::new());
        let (_, request_lease) = registry
            .reserve(
                request.request_id.clone(),
                RequestClass::Generation,
                RequestControls::Generation {
                    cancellations: Vec::new(),
                    reasoning_forces: Vec::new(),
                },
            )
            .expect("request reserves");
        let (event_tx, event_rx) = bounded(EVENT_CAPACITY);
        let (result_tx, result_rx) = bounded(0);
        let cancellations = request
            .cases
            .iter()
            .map(|_| Arc::new(AtomicBool::new(false)))
            .collect::<Vec<_>>();
        let reasoning_forces = request
            .cases
            .iter()
            .map(|_| Arc::new(AtomicBool::new(false)))
            .collect::<Vec<_>>();
        let rejector = std::thread::spawn(move || {
            reject_queued_command(WorkerCommand::GenerateBatch {
                request,
                exact_cell_budget: None,
                admission: GenerationBatchAdmission::Compatibility,
                event_tx,
                result_tx,
                cancellations,
                reasoning_forces,
                request_lease,
            });
        });

        event_rx
            .recv()
            .expect("queued terminal event precedes final result publication");
        assert_eq!(
            registry.active_count(),
            1,
            "executor identity remains reserved while final publication is blocked"
        );
        assert_eq!(
            result_rx
                .recv()
                .expect("queued final is published")
                .expect_err("queued request is cancelled")
                .code,
            NativeErrorCode::Cancelled
        );
        rejector.join().expect("queued rejector joins");
        assert_eq!(registry.active_count(), 0);
    }

    #[test]
    fn dropped_generation_ticket_keeps_request_id_reserved_until_executor_terminal() {
        let (request, fingerprint, _, _, _, _) = seal_fixture();
        let status = ResidentModelStatus {
            model_id: request.model_id.clone(),
            model_path: PathBuf::new(),
            state: ModelRuntimeState::Ready,
            fingerprint: Some(fingerprint),
            descriptor: None,
            active_sequences: 0,
            max_sequences: 4,
        };
        let (handle, command_rx) = test_admission_handle(status, COMMAND_CAPACITY);

        let ticket = handle
            .generate_batch(request.clone())
            .expect("first request is admitted");
        drop(ticket);
        assert_eq!(handle.inner.requests.active_count(), 1);
        assert_eq!(
            handle
                .generate_batch(request.clone())
                .expect_err("ticket drop cannot release executor identity")
                .code,
            NativeErrorCode::DuplicateActiveRequest
        );

        reject_queued_command(command_rx.recv().expect("executor owns queued command"));
        assert_eq!(handle.inner.requests.active_count(), 0);

        let retry = handle
            .generate_batch(request)
            .expect("identity is reusable after executor terminal");
        reject_queued_command(command_rx.recv().expect("retry command"));
        drop(retry);
        assert_eq!(handle.inner.requests.active_count(), 0);
    }

    #[test]
    fn waiter_timeout_returns_live_ticket_without_releasing_executor_identity() {
        let registry = Arc::new(RequestRegistry::new());
        let (control, lease) = registry
            .reserve(
                "timeout-request",
                RequestClass::Generation,
                RequestControls::Generation {
                    cancellations: Vec::new(),
                    reasoning_forces: Vec::new(),
                },
            )
            .expect("request reserves");
        let (result_tx, result_rx) = bounded(1);
        let (_event_tx, event_rx) = bounded(EVENT_CAPACITY);
        let ticket = GenerationTicket {
            request_id: "timeout-request".to_owned(),
            events: event_rx,
            result: result_rx,
            control,
        };

        let ticket = match ticket
            .wait_timeout(Duration::ZERO)
            .expect("timeout is an observation, not an error")
        {
            WaitOutcome::TimedOut(ticket) => ticket,
            WaitOutcome::Ready(_) => panic!("no executor result was published"),
        };
        assert_eq!(registry.active_count(), 1);
        result_tx
            .send(Ok(GenerationCompletion::unverified(Vec::new())))
            .expect("executor terminal publication");
        drop(lease);
        assert!(
            ticket
                .wait()
                .expect("later wait receives terminal")
                .is_empty()
        );
        assert_eq!(registry.active_count(), 0);
    }

    #[test]
    fn bootstrap_guard_never_detaches_its_worker() {
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stopped = Arc::clone(&stopped);
        let (command_tx, command_rx) = bounded(COMMAND_CAPACITY);
        let (shutdown_tx, shutdown_rx) = bounded(1);
        let join = std::thread::spawn(move || {
            crossbeam_channel::select_biased! {
                recv(shutdown_rx) -> _ => {},
                recv(command_rx) -> _ => {},
            }
            worker_stopped.store(true, Ordering::Release);
        });

        drop(WorkerBootstrapGuard::new(command_tx, shutdown_tx, join));
        assert!(stopped.load(Ordering::Acquire));
    }

    #[test]
    fn worker_panic_cannot_mint_joined_shutdown_authority() {
        let (command_tx, _command_rx) = bounded(COMMAND_CAPACITY);
        let (shutdown_tx, shutdown_rx) = bounded(1);
        let join = std::thread::spawn(move || {
            let _ = shutdown_rx.recv();
            panic!("intentional owner-worker panic");
        });
        let inner = Arc::new(NativeModelInner {
            worker_identity: Arc::new(WorkerIdentity),
            worker_id: "panicking-owner-test-worker".to_owned(),
            command_tx,
            shutdown_tx,
            closing: AtomicBool::new(false),
            admission: Mutex::new(()),
            requests: Arc::new(RequestRegistry::new()),
            status: Arc::new(RwLock::new(ResidentModelStatus {
                model_id: "panicking-worker".to_string(),
                model_path: std::path::PathBuf::new(),
                state: ModelRuntimeState::Ready,
                fingerprint: None,
                descriptor: None,
                active_sequences: 0,
                max_sequences: 1,
            })),
        });
        let owner = NativeModelOwner {
            inner,
            join: Some(join),
        };
        let error = owner
            .shutdown_joined()
            .expect_err("a panicked worker must not yield joined evidence");
        assert_eq!(error.code, NativeErrorCode::WorkerStopped);
    }

    struct TestArtifactDirectory {
        path: std::path::PathBuf,
    }

    impl TestArtifactDirectory {
        fn new(label: &str) -> Self {
            loop {
                let sequence = TEST_ARTIFACT_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
                let path =
                    std::env::temp_dir().join(format!("llama-native-engine-{label}-{sequence}"));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Self { path },
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("unique artifact test directory is created: {error}"),
                }
            }
        }

        fn join(&self, name: &str) -> std::path::PathBuf {
            self.path.join(name)
        }
    }

    impl Drop for TestArtifactDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn test_model_fingerprint(model_id: &str) -> ModelFingerprint {
        ModelFingerprint {
            model_id: model_id.to_string(),
            model_size: 1,
            model_sha256: "a".repeat(64),
            tokenizer_sha256: "b".repeat(64),
            chat_template_sha256: "c".repeat(64),
            multimodal_projector_sha256: None,
            binding_version: LLAMA_CPP_BINDING_VERSION.to_string(),
            build_id: "test-build".to_string(),
            backend: "cpu".to_string(),
            context_tokens: 2_048,
            batch_tokens: 64,
            max_sequences: 4,
            rope_config_sha256: "d".repeat(64),
            kv_layout_sha256: "e".repeat(64),
        }
    }

    fn embedding_request(model_id: &str, token_ids: Vec<i32>) -> EmbeddingBatchRequest {
        EmbeddingBatchRequest::new(
            "embedding-request".to_string(),
            model_id.to_string(),
            vec![EmbeddingInput::new("input-0".to_string(), token_ids).expect("valid input")],
            EmbeddingPooling::None,
            EmbeddingNormalization::None,
        )
        .expect("valid embedding request")
    }

    fn seal_fixture() -> TestSealFixture {
        let cases = [
            ("case-a", vec![1, 2, 3], 17, "alpha"),
            ("case-b", vec![1, 2, 4], 18, "beta"),
        ];
        let request = GenerationBatchRequest {
            request_id: "verified-request".to_string(),
            model_id: "model".to_string(),
            cases: cases
                .iter()
                .map(|(case_id, prompt_tokens, seed, _)| GenerationCase {
                    case_id: (*case_id).to_string(),
                    input: GenerationInput::Completion {
                        prompts: vec![CompletionPrompt::Tokens {
                            token_ids: prompt_tokens.clone(),
                        }],
                    },
                    sampling: SamplingConfig {
                        seed: *seed,
                        max_tokens: 4,
                        ..SamplingConfig::default()
                    },
                    cached_prefix: None,
                })
                .collect(),
        };
        let outputs = cases
            .iter()
            .enumerate()
            .map(
                |(index, (case_id, prompt_tokens, _, text))| GenerationOutput {
                    request_id: request.request_id.clone(),
                    branch_id: (*case_id).to_string(),
                    input_index: index,
                    model_id: request.model_id.clone(),
                    text: (*text).to_string(),
                    generated_token_ids: vec![100 + index as i32],
                    token_observations: None,
                    state: GenerationState::Completed,
                    finish_reason: "end_of_generation".to_string(),
                    metrics: GenerationMetrics {
                        prompt_tokens: prompt_tokens.len(),
                        completion_tokens: 1,
                        shared_prefix_tokens: 2,
                        duration_ms: 1,
                        first_token_ms: Some(1),
                        tokens_per_second: 1_000.0,
                        cache: GenerationCacheMetrics {
                            supplied_prefix_tokens: 0,
                            restored_prefix_tokens: 0,
                            batch_shared_prefix_tokens: 2,
                        },
                    },
                    real_engine_invoked: true,
                    fake_fixture: false,
                    transport: NativeTransport::InProcess,
                },
            )
            .collect::<Vec<_>>();
        let mut events = Vec::new();
        for (index, (case_id, ..)) in cases.iter().enumerate() {
            events.push(GenerationEvent {
                request_id: request.request_id.clone(),
                branch_id: (*case_id).to_string(),
                sequence_id: index as i32,
                input_index: index,
                event_index: 0,
                event: GenerationEventKind::State {
                    state: GenerationState::Prefilling,
                },
            });
        }
        for (index, (case_id, ..)) in cases.iter().enumerate() {
            events.push(GenerationEvent {
                request_id: request.request_id.clone(),
                branch_id: (*case_id).to_string(),
                sequence_id: index as i32,
                input_index: index,
                event_index: 1,
                event: GenerationEventKind::State {
                    state: GenerationState::Generating,
                },
            });
        }
        for (index, (case_id, _, _, text)) in cases.iter().enumerate() {
            events.push(GenerationEvent {
                request_id: request.request_id.clone(),
                branch_id: (*case_id).to_string(),
                sequence_id: index as i32,
                input_index: index,
                event_index: 2,
                event: GenerationEventKind::Delta {
                    text: (*text).to_string(),
                },
            });
        }
        for (index, (case_id, ..)) in cases.iter().enumerate() {
            events.push(GenerationEvent {
                request_id: request.request_id.clone(),
                branch_id: (*case_id).to_string(),
                sequence_id: index as i32,
                input_index: index,
                event_index: 3,
                event: GenerationEventKind::State {
                    state: GenerationState::Completed,
                },
            });
        }
        (
            request,
            test_model_fingerprint("model"),
            outputs,
            vec![Some(900), Some(901)],
            events,
            vec!["alpha".to_string(), "beta".to_string()],
        )
    }

    fn is_test_eog_token(token_id: i32) -> bool {
        matches!(token_id, 900 | 901)
    }

    fn test_token_piece_traces(decoded: &[String]) -> Vec<TokenPieceTrace> {
        decoded
            .iter()
            .map(|text| {
                let mut trace = TokenPieceTrace::with_token_capacity(1);
                trace
                    .push_piece(text.as_bytes())
                    .expect("test piece fits the bounded trace");
                trace
            })
            .collect()
    }

    fn test_verified_generation() -> VerifiedGenerationBatch {
        let (request, model_fingerprint, outputs, terminal_sampled_token_ids, events, decoded) =
            seal_fixture();
        validate_verified_generation_batch(
            &request,
            &model_fingerprint,
            &outputs,
            &terminal_sampled_token_ids,
            &events,
            &decoded,
            is_test_eog_token,
        )
        .expect("test authority fixture must satisfy live invariants");
        let token_piece_traces = test_token_piece_traces(&decoded);
        VerifiedGenerationBatch {
            request,
            model_fingerprint,
            outputs,
            terminal_sampled_token_ids,
            events,
            token_piece_traces,
        }
    }

    fn verified_completion(verified: VerifiedGenerationBatch) -> GenerationCompletion {
        let VerifiedGenerationBatch {
            request,
            model_fingerprint,
            outputs,
            terminal_sampled_token_ids,
            events,
            token_piece_traces,
        } = verified;
        GenerationCompletion::verified(
            outputs,
            VerifiedGenerationEvidence {
                request,
                model_fingerprint,
                terminal_sampled_token_ids,
                events,
                token_piece_traces,
            },
        )
    }

    fn completed_generation_ticket(completion: GenerationCompletion) -> GenerationTicket {
        let (result_tx, result_rx) = bounded(1);
        result_tx
            .send(Ok(completion))
            .expect("test result receiver remains live");
        let (_event_tx, event_rx) = bounded(EVENT_CAPACITY);
        let (control, lease) = test_generation_reservation("verified-request", &[]);
        drop(lease);
        GenerationTicket {
            request_id: "verified-request".to_string(),
            events: event_rx,
            result: result_rx,
            control,
        }
    }

    #[test]
    fn consuming_try_wait_pending_returns_ticket() {
        let (result_tx, result_rx) = bounded(1);
        let (_event_tx, event_rx) = bounded(EVENT_CAPACITY);
        let (control, lease) = test_generation_reservation("pending-request", &["case"]);
        let ticket = GenerationTicket {
            request_id: "pending-request".to_string(),
            events: event_rx,
            result: result_rx,
            control,
        };

        let ticket = match ticket.try_wait().expect("pending poll is valid") {
            TryWaitOutcome::Pending(ticket) => ticket,
            TryWaitOutcome::Ready(_) => panic!("empty result channel cannot be ready"),
        };
        result_tx
            .send(Ok(GenerationCompletion::unverified(Vec::new())))
            .expect("pending ticket still owns its receiver");
        drop(lease);
        assert!(
            ticket
                .wait()
                .expect("returned ticket remains usable")
                .is_empty()
        );
    }

    #[test]
    fn consuming_try_wait_ready_returns_output() {
        let ticket = completed_generation_ticket(GenerationCompletion::unverified(Vec::new()));
        match ticket.try_wait().expect("completed poll is valid") {
            TryWaitOutcome::Ready(outputs) => assert!(outputs.is_empty()),
            TryWaitOutcome::Pending(_) => panic!("published result cannot remain pending"),
        }
    }

    #[test]
    fn generic_cancel_cancels_embedding() {
        let (handle, _command_rx) = test_admission_handle(
            ResidentModelStatus {
                model_id: "model".to_string(),
                model_path: PathBuf::new(),
                state: ModelRuntimeState::Ready,
                fingerprint: Some(test_model_fingerprint("model")),
                descriptor: None,
                active_sequences: 0,
                max_sequences: 4,
            },
            COMMAND_CAPACITY,
        );
        let cancellation = Arc::new(AtomicBool::new(false));
        let (_control, lease) = handle
            .inner
            .requests
            .reserve(
                "embedding-request",
                RequestClass::Embedding,
                RequestControls::Embedding {
                    cancellation: Arc::clone(&cancellation),
                },
            )
            .expect("embedding request reserves");

        assert_eq!(handle.cancel("embedding-request", None), 1);
        assert!(cancellation.load(Ordering::Acquire));
        assert_eq!(handle.cancel("embedding-request", Some("not-a-branch")), 0);
        drop(lease);
    }

    #[test]
    fn completed_ticket_drop_cannot_cancel_a_reused_request_identity() {
        let request_id = "reused-request".to_owned();
        let branch_id = "branch".to_owned();
        let old_cancel = Arc::new(AtomicBool::new(false));
        let old_reasoning = Arc::new(AtomicBool::new(false));
        let old_registry = Arc::new(RequestRegistry::new());
        let (old_control, old_lease) = old_registry
            .reserve(
                request_id.clone(),
                RequestClass::Generation,
                RequestControls::Generation {
                    cancellations: vec![(branch_id.clone(), Arc::clone(&old_cancel))],
                    reasoning_forces: vec![(branch_id.clone(), Arc::clone(&old_reasoning))],
                },
            )
            .expect("old request reserves");
        drop(old_lease);
        let (result_tx, result_rx) = bounded(1);
        result_tx
            .send(Ok(GenerationCompletion::unverified(Vec::new())))
            .expect("result receiver remains live");
        let (_event_tx, event_rx) = bounded(EVENT_CAPACITY);
        let old_ticket = GenerationTicket {
            request_id: request_id.clone(),
            events: event_rx,
            result: result_rx,
            control: old_control,
        };

        match old_ticket.try_wait().expect("completed result") {
            TryWaitOutcome::Ready(outputs) => assert!(outputs.is_empty()),
            TryWaitOutcome::Pending(_) => panic!("completed result cannot remain pending"),
        }

        let new_cancel = Arc::new(AtomicBool::new(false));
        let new_reasoning = Arc::new(AtomicBool::new(false));
        let new_registry = Arc::new(RequestRegistry::new());
        let (_new_control, _new_lease) = new_registry
            .reserve(
                request_id,
                RequestClass::Generation,
                RequestControls::Generation {
                    cancellations: vec![(branch_id.clone(), Arc::clone(&new_cancel))],
                    reasoning_forces: vec![(branch_id, Arc::clone(&new_reasoning))],
                },
            )
            .expect("new request reserves");
        assert!(!new_cancel.load(Ordering::Acquire));
        assert!(!new_reasoning.load(Ordering::Acquire));
        assert_eq!(new_registry.active_count(), 1);
    }

    #[test]
    fn reported_binding_identity_matches_the_private_recipe_and_lock_pin() {
        assert_eq!(
            LLAMA_CPP_BINDING_REV,
            "a3cf95eb1d4fa748480eb780e6fcbfc1a5c1c391"
        );
        assert_eq!(LLAMA_CPP_REV, "5f55650a78f92aff4d48d671423e888fac0469ff");
        let manifest = include_str!("../Cargo.toml");
        let pinned_manifest_lines = |package: &str| {
            let prefix = format!("{package} = {{ version = \"={LLAMA_CPP_BINDING_VERSION}\"");
            manifest
                .lines()
                .filter(|line| line.starts_with(&prefix))
                .filter(|line| line.contains("git = "))
                .filter(|line| line.contains(&format!("rev = \"{LLAMA_CPP_BINDING_REV}\"")))
                .count()
        };
        assert_eq!(pinned_manifest_lines("llama-cpp-2"), 2);
        assert_eq!(pinned_manifest_lines("llama-cpp-sys-2"), 1);
        assert_eq!(
            manifest
                .lines()
                .filter(|line| line.starts_with("llama-cpp-2 = "))
                .filter(|line| line.contains("features = [\"common\", \"sampler\", \"mtmd\"]"))
                .count(),
            1
        );
        assert_eq!(
            manifest
                .lines()
                .filter(|line| line.starts_with("llama-cpp-2 = "))
                .filter(|line| {
                    line.contains("features = [\"common\", \"sampler\", \"mtmd\", \"metal\"]")
                })
                .count(),
            1
        );

        let lock = include_str!("../../../../../Cargo.lock");
        let locked_source_suffix =
            format!("?rev={LLAMA_CPP_BINDING_REV}#{LLAMA_CPP_BINDING_REV}\"");
        assert_eq!(lock.matches(&locked_source_suffix).count(), 2);
        for package in ["llama-cpp-2", "llama-cpp-sys-2"] {
            let package_name = format!("name = \"{package}\"");
            let mut matching_blocks = lock
                .split("[[package]]")
                .filter(|block| block.lines().any(|line| line.trim_end() == package_name));
            let block = matching_blocks
                .next()
                .expect("binding package must be locked");
            assert!(matching_blocks.next().is_none());
            assert!(block.contains("source = \"git+"));
            assert!(block.contains(&locked_source_suffix));
        }

        let private_build_digest = LLAMA_NATIVE_BUILD_MANIFEST_SHA256;
        assert_eq!(private_build_digest.len(), 64);
        assert!(
            private_build_digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );

        let cpu_build = native_build_id_from_private_digest(private_build_digest, "cpu");
        assert_eq!(
            cpu_build,
            native_build_id_from_private_digest(private_build_digest, "cpu")
        );
        assert_ne!(
            cpu_build,
            native_build_id_from_private_digest(private_build_digest, "metal")
        );
        assert_ne!(
            cpu_build,
            native_build_id_from_private_digest(&"c".repeat(64), "cpu")
        );
        assert!(cpu_build.starts_with("llama-native-build-v3-"));
        assert_eq!(cpu_build.len(), "llama-native-build-v3-".len() + 64);
        assert_eq!(native_build_id("cpu"), cpu_build);
    }

    fn reviewed_embedding_binding_surface(context: &LlamaContext<'_>) {
        let _ = context.pooling_type();
        let _ = context.embeddings_ith(0);
        let _ = context.embeddings_seq_ith(0);
    }

    #[test]
    fn reviewed_embedding_surface_resolves_through_the_single_pinned_wrapper() {
        let _surface: fn(&LlamaContext<'_>) = reviewed_embedding_binding_surface;
        assert_eq!(
            llama_pooling(EmbeddingPooling::None),
            LlamaPoolingType::None
        );
        assert_eq!(
            llama_pooling(EmbeddingPooling::Mean),
            LlamaPoolingType::Mean
        );
        assert_eq!(llama_pooling(EmbeddingPooling::Cls), LlamaPoolingType::Cls);
        assert_eq!(
            llama_pooling(EmbeddingPooling::Last),
            LlamaPoolingType::Last
        );
        assert_eq!(
            llama_pooling(EmbeddingPooling::Rank),
            LlamaPoolingType::Rank
        );
        assert_eq!(
            resolved_embedding_pooling(LlamaPoolingType::Unspecified)
                .expect_err("unresolved pooling must fail closed")
                .code,
            NativeErrorCode::UnsupportedParameter
        );
    }

    #[test]
    fn verified_batch_invariants_bind_request_model_outputs_tokens_text_and_events() {
        let (request, fingerprint, outputs, terminal_ids, events, decoded) = seal_fixture();
        validate_verified_generation_batch(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
            is_test_eog_token,
        )
        .expect("coherent owner-worker evidence verifies");

        let expected_outputs = outputs.clone();
        let token_piece_traces = test_token_piece_traces(&decoded);
        let verified = VerifiedGenerationBatch {
            request,
            model_fingerprint: fingerprint,
            outputs,
            terminal_sampled_token_ids: terminal_ids,
            events,
            token_piece_traces,
        };
        assert_eq!(verified.request().request_id, "verified-request");
        assert_eq!(verified.model_fingerprint().model_id, "model");
        assert_eq!(verified.outputs(), expected_outputs);
        assert_eq!(
            verified.terminal_sampled_token_ids(),
            [Some(900), Some(901)]
        );
        assert_eq!(verified.events().len(), 8);
        assert_eq!(
            verified_completion(verified).into_outputs(),
            expected_outputs,
            "legacy consumption must preserve the exact baseline outputs"
        );
    }

    #[test]
    fn verified_batch_debug_redacts_prompts_outputs_stops_tokens_and_events() {
        let mut verified = test_verified_generation();
        verified.request.request_id = "safe-request-id".to_string();
        verified.request.cases[0].sampling.stop = vec!["PRIVATE_STOP_SENTINEL".to_string()];
        verified.request.cases[0].input = GenerationInput::Completion {
            prompts: vec![CompletionPrompt::Tokens {
                token_ids: vec![2_147_483_001],
            }],
        };
        verified.outputs[0].text = "PRIVATE_PROSE_SENTINEL".to_string();
        verified.events[2].event = GenerationEventKind::Delta {
            text: "PRIVATE_EVENT_SENTINEL".to_string(),
        };

        let debug = format!("{verified:?}");
        assert!(debug.contains("safe-request-id"));
        assert!(debug.contains("model_sha256"));
        for secret in [
            "PRIVATE_STOP_SENTINEL",
            "2147483001",
            "PRIVATE_PROSE_SENTINEL",
            "PRIVATE_EVENT_SENTINEL",
            "alpha",
            "beta",
        ] {
            assert!(!debug.contains(secret), "Debug leaked {secret}");
        }
    }

    #[test]
    fn verified_batch_rejects_every_tampered_trust_dimension() {
        let assert_rejected = |request: &GenerationBatchRequest,
                               fingerprint: &ModelFingerprint,
                               outputs: &[GenerationOutput],
                               terminal_ids: &[Option<i32>],
                               events: &[GenerationEvent],
                               decoded: &[String]| {
            let error = validate_verified_generation_batch(
                request,
                fingerprint,
                outputs,
                terminal_ids,
                events,
                decoded,
                is_test_eog_token,
            )
            .expect_err("tampered evidence must not mint authority");
            assert_eq!(error.code, NativeErrorCode::Internal);
        };

        let (request, mut fingerprint, outputs, terminal_ids, events, decoded) = seal_fixture();
        fingerprint.model_id = "other-model".to_string();
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (mut request, fingerprint, outputs, terminal_ids, events, decoded) = seal_fixture();
        request.cases[0].input = GenerationInput::Completion {
            prompts: vec![CompletionPrompt::Text {
                text: "caller text".to_string(),
                special_tokens: SpecialTokenPolicy::NoBosParseSpecial,
            }],
        };
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (request, fingerprint, mut outputs, terminal_ids, events, decoded) = seal_fixture();
        outputs.swap(0, 1);
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (request, fingerprint, mut outputs, terminal_ids, events, decoded) = seal_fixture();
        outputs[0].real_engine_invoked = false;
        outputs[0].fake_fixture = true;
        outputs[0].transport = NativeTransport::FakeFixture;
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (request, fingerprint, mut outputs, terminal_ids, events, decoded) = seal_fixture();
        outputs[0].generated_token_ids.push(999);
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (request, fingerprint, outputs, terminal_ids, events, mut decoded) = seal_fixture();
        decoded[0] = "tampered-token-decoding".to_string();
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (request, fingerprint, mut outputs, terminal_ids, events, decoded) = seal_fixture();
        outputs[0].text = "different prose".to_string();
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (request, fingerprint, outputs, terminal_ids, mut events, decoded) = seal_fixture();
        events[4].request_id = "wrong-request".to_string();
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (request, fingerprint, outputs, terminal_ids, mut events, decoded) = seal_fixture();
        events[4].branch_id = "wrong-case".to_string();
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (request, fingerprint, outputs, terminal_ids, mut events, decoded) = seal_fixture();
        events[4].sequence_id = 1;
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (request, fingerprint, outputs, terminal_ids, mut events, decoded) = seal_fixture();
        events[4].input_index = 99;
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (request, fingerprint, outputs, terminal_ids, mut events, decoded) = seal_fixture();
        events[4].event_index = 9;
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (request, fingerprint, outputs, terminal_ids, mut events, decoded) = seal_fixture();
        events.pop();
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (request, fingerprint, outputs, terminal_ids, mut events, decoded) = seal_fixture();
        events.push(events[6].clone());
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (request, fingerprint, mut outputs, terminal_ids, mut events, decoded) = seal_fixture();
        outputs[0].state = GenerationState::Failed;
        events[6].event = GenerationEventKind::State {
            state: GenerationState::Failed,
        };
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (request, fingerprint, outputs, mut terminal_ids, events, decoded) = seal_fixture();
        terminal_ids[0] = None;
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (request, fingerprint, outputs, mut terminal_ids, events, decoded) = seal_fixture();
        terminal_ids[0] = Some(123);
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (request, fingerprint, outputs, mut terminal_ids, events, decoded) = seal_fixture();
        terminal_ids.pop();
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (request, fingerprint, mut outputs, terminal_ids, events, decoded) = seal_fixture();
        outputs[0].generated_token_ids[0] = 900;
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (request, fingerprint, mut outputs, terminal_ids, events, decoded) = seal_fixture();
        outputs[0].metrics.prompt_tokens += 1;
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (request, fingerprint, mut outputs, terminal_ids, events, decoded) = seal_fixture();
        outputs[0].metrics.shared_prefix_tokens += 1;
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (request, fingerprint, mut outputs, terminal_ids, events, decoded) = seal_fixture();
        outputs[0].metrics.cache.restored_prefix_tokens = 1;
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (request, fingerprint, mut outputs, terminal_ids, events, decoded) = seal_fixture();
        outputs[0].metrics.cache.supplied_prefix_tokens = 1;
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (request, fingerprint, mut outputs, terminal_ids, events, decoded) = seal_fixture();
        outputs[0].metrics.cache.batch_shared_prefix_tokens += 1;
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (mut request, fingerprint, outputs, terminal_ids, events, decoded) = seal_fixture();
        request.cases[0].sampling.max_tokens = 0;
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (mut request, fingerprint, outputs, terminal_ids, events, decoded) = seal_fixture();
        request.cases[0].sampling.seed = u32::MAX;
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );

        let (mut request, fingerprint, outputs, terminal_ids, events, decoded) = seal_fixture();
        request.cases[0].cached_prefix = Some(SequenceStateBlob {
            sequence_id: 0,
            token_count: 1,
            bytes: vec![1, 2, 3],
            token_ids: vec![1],
        });
        assert_rejected(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
        );
    }

    #[test]
    fn verified_batch_requires_exact_stop_suffix_and_published_projection() {
        let (mut request, fingerprint, mut outputs, mut terminal_ids, mut events, mut decoded) =
            seal_fixture();
        request.cases[0].sampling.stop = vec!["<stop>".to_string()];
        outputs[0].finish_reason = "stop_sequence".to_string();
        terminal_ids[0] = None;
        decoded[0] = "alpha<stop>".to_string();
        events[4].event = GenerationEventKind::Delta {
            text: decoded[0].clone(),
        };
        validate_verified_generation_batch(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
            is_test_eog_token,
        )
        .expect("an exact removed stop suffix is a coherent output projection");

        request.cases[0].sampling.stop = vec!["different".to_string()];
        let error = validate_verified_generation_batch(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
            is_test_eog_token,
        )
        .expect_err("a claimed stop projection without its configured suffix must fail");
        assert_eq!(error.code, NativeErrorCode::Internal);
    }

    #[test]
    fn verified_terminal_sample_evidence_matches_each_non_eog_terminal() {
        let (mut request, fingerprint, mut outputs, mut terminal_ids, events, decoded) =
            seal_fixture();
        request.cases[0].sampling.max_tokens = 1;
        outputs[0].finish_reason = "max_tokens".to_string();
        terminal_ids[0] = None;
        validate_verified_generation_batch(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
            is_test_eog_token,
        )
        .expect("max-token termination has no separately sampled EOG token");

        let (request, fingerprint, mut outputs, mut terminal_ids, mut events, decoded) =
            seal_fixture();
        outputs[0].state = GenerationState::Cancelled;
        outputs[0].finish_reason = "cancelled".to_string();
        terminal_ids[0] = None;
        events[6].event = GenerationEventKind::State {
            state: GenerationState::Cancelled,
        };
        validate_verified_generation_batch(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
            is_test_eog_token,
        )
        .expect("cancelled termination has no separately sampled EOG token");
    }

    #[test]
    fn strict_seal_utf8_accepts_split_codepoints_and_rejects_incomplete_or_malformed_bytes() {
        let split_euro = vec![vec![0xe2], vec![0x82], vec![0xac]];
        assert_eq!(
            strict_verified_utf8(&split_euro).expect("split token pieces form canonical UTF-8"),
            "€"
        );

        let incomplete = vec![vec![0xe2], vec![0x82]];
        assert_eq!(
            strict_verified_utf8(&incomplete)
                .expect_err("an incomplete final codepoint cannot receive authority")
                .code,
            NativeErrorCode::Internal
        );

        let malformed = vec![vec![b'a'], vec![0xff], vec![b'b']];
        assert_eq!(
            strict_verified_utf8(&malformed)
                .expect_err("malformed token bytes cannot receive authority")
                .code,
            NativeErrorCode::Internal
        );
    }

    #[test]
    fn token_piece_boundaries_match_token_count() {
        let mut trace = TokenPieceTrace::with_token_capacity(3);
        trace.push_piece(b"ab").expect("first piece");
        trace.push_piece(b"").expect("zero-byte piece");
        trace.push_piece(b"c").expect("third piece");

        trace
            .validate(3)
            .expect("three tokens have four boundaries");
        assert_eq!(trace.raw_piece_bytes(), b"abc");
        assert_eq!(trace.cumulative_boundaries(), [0, 2, 2, 3]);
        assert_eq!(
            trace.cumulative_boundaries().len(),
            4,
            "one terminal boundary follows every generated token"
        );
    }

    #[test]
    fn zero_byte_piece_is_representable() {
        let mut trace = TokenPieceTrace::with_token_capacity(1);
        trace.push_piece(&[]).expect("zero-byte piece is valid");

        trace
            .validate(1)
            .expect("equal adjacent boundaries are valid");
        assert!(trace.raw_piece_bytes().is_empty());
        assert_eq!(trace.cumulative_boundaries(), [0, 0]);
    }

    #[test]
    fn invalid_utf8_piece_is_preserved() {
        let mut trace = TokenPieceTrace::with_token_capacity(2);
        trace
            .push_piece(&[0xe2])
            .expect("incomplete leading byte is retained exactly");
        trace
            .push_piece(&[0x82, 0xac])
            .expect("continuation bytes are retained exactly");

        trace
            .validate(2)
            .expect("raw piece boundaries remain coherent");
        assert_eq!(trace.raw_piece_bytes(), [0xe2, 0x82, 0xac]);
        assert_eq!(trace.cumulative_boundaries(), [0, 1, 3]);
        assert_eq!(
            strict_verified_utf8_bytes(trace.raw_piece_bytes())
                .expect("the complete projection is canonical UTF-8"),
            "€"
        );
    }

    #[test]
    fn split_utf8_projection_reaches_verified_seal_validation() {
        let mut decoder = UTF_8.new_decoder();
        let first = decode_generated_utf8_piece(&mut decoder, &[0xe2], false)
            .expect("leading byte is buffered");
        let second = decode_generated_utf8_piece(&mut decoder, &[0x82, 0xac], false)
            .expect("continuation bytes complete the scalar");
        let final_piece = decode_generated_utf8_piece(&mut decoder, &[], true)
            .expect("complete decoder state finalizes cleanly");
        assert!(first.is_empty());
        assert_eq!(second, "€");
        assert!(final_piece.is_empty());

        let (request, fingerprint, mut outputs, terminal_ids, mut events, mut decoded) =
            seal_fixture();
        outputs[0].text = second.clone();
        outputs[0].generated_token_ids = vec![100, 101];
        outputs[0].metrics.completion_tokens = 2;
        events[4].event = GenerationEventKind::Delta {
            text: second.clone(),
        };
        decoded[0] = second;

        let mut trace = TokenPieceTrace::with_token_capacity(2);
        trace.push_piece(&[0xe2]).expect("first exact piece");
        trace.push_piece(&[0x82, 0xac]).expect("second exact piece");
        trace
            .validate(outputs[0].generated_token_ids.len())
            .expect("trace cardinality matches the sampled tokens");
        assert_eq!(
            strict_verified_utf8_bytes(trace.raw_piece_bytes()).expect("complete exact UTF-8"),
            decoded[0]
        );
        validate_verified_generation_batch(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &events,
            &decoded,
            is_test_eog_token,
        )
        .expect("live projection and exact trace agree at the seal validator");

        let mut traces = test_token_piece_traces(&decoded);
        traces[0] = trace;
        let verified = VerifiedGenerationBatch {
            request,
            model_fingerprint: fingerprint,
            outputs,
            terminal_sampled_token_ids: terminal_ids,
            events,
            token_piece_traces: traces,
        };
        assert_eq!(verified_completion(verified).into_outputs()[0].text, "€");
    }

    #[test]
    fn tampered_trace_cannot_verify() {
        let mut trace = TokenPieceTrace::with_token_capacity(2);
        trace.push_piece(b"a").expect("first piece");
        trace.push_piece(b"b").expect("second piece");
        trace.cumulative_boundaries[1] = 3;

        assert_eq!(
            trace
                .validate(2)
                .expect_err("an out-of-range, nonmonotonic boundary must fail")
                .code,
            NativeErrorCode::Internal
        );
    }

    #[test]
    fn generated_output_byte_accounting_accepts_the_ceiling_and_rejects_overflow() {
        assert_eq!(
            checked_generated_output_len(MAX_GENERATED_OUTPUT_BYTES - 3, 3)
                .expect("exact byte ceiling is admissible"),
            MAX_GENERATED_OUTPUT_BYTES
        );
        assert_eq!(
            checked_generated_output_len(MAX_GENERATED_OUTPUT_BYTES, 1)
                .expect_err("one byte over the ceiling must fail")
                .code,
            NativeErrorCode::MemoryBudgetExceeded
        );
        assert_eq!(
            checked_generated_output_len(usize::MAX, 1)
                .expect_err("host integer overflow must fail")
                .code,
            NativeErrorCode::MemoryBudgetExceeded
        );
    }

    #[test]
    fn only_exact_token_generate_batch_results_can_yield_a_seal() {
        let (request, _, outputs, _, _, _) = seal_fixture();
        assert!(is_exact_token_generation_batch(&request));
        assert!(is_sealable_generation_batch(
            &request,
            GenerationBatchAdmission::ExactBatch,
            false
        ));
        assert!(
            !is_sealable_generation_batch(&request, GenerationBatchAdmission::ExactBatch, true),
            "out-of-band reasoning intervention is not bound by the request and must remove authority"
        );
        assert!(
            !is_sealable_generation_batch(&request, GenerationBatchAdmission::Compatibility, false),
            "the compatibility wrapper must not mint exact batch authority"
        );

        let non_exact_inputs = [
            GenerationInput::Chat {
                messages: vec![ChatMessage {
                    role: ChatRole::User,
                    content: "not exact tokens".to_string(),
                }],
                template: ChatTemplateChoice::ModelDefault,
            },
            GenerationInput::Completion {
                prompts: vec![CompletionPrompt::Text {
                    text: "not exact tokens".to_string(),
                    special_tokens: SpecialTokenPolicy::NoBosParseSpecial,
                }],
            },
            GenerationInput::FillInMiddle {
                prefix: "prefix".to_string(),
                suffix: "suffix".to_string(),
            },
        ];
        for input in non_exact_inputs {
            let mut non_exact = request.clone();
            non_exact.cases[0].input = input;
            assert!(!is_exact_token_generation_batch(&non_exact));
        }

        let mut cached = request.clone();
        cached.cases[0].cached_prefix = Some(SequenceStateBlob {
            sequence_id: 77,
            token_count: 1,
            bytes: vec![0xde, 0xad, 0xbe, 0xef],
            token_ids: vec![1],
        });
        assert!(is_exact_token_generation_batch(&cached));
        assert!(
            !is_statically_sealable_generation_batch(&cached, GenerationBatchAdmission::ExactBatch),
            "publicly relabelable KV bytes must never enter strict authority"
        );

        let mut default_seed = request.clone();
        default_seed.cases[0].sampling.seed = u32::MAX;
        assert!(
            !is_statically_sealable_generation_batch(
                &default_seed,
                GenerationBatchAdmission::ExactBatch
            ),
            "llama.cpp's randomized default-seed sentinel is not an exact treatment"
        );

        let error = GenerationCompletion::unverified(outputs.clone())
            .into_verified()
            .expect_err("compatibility, shared-prefix, and multimodal results stay unverified");
        assert_eq!(error.code, NativeErrorCode::UnsupportedParameter);
        assert_eq!(
            GenerationCompletion::unverified(outputs.clone()).into_outputs(),
            outputs,
            "strict seal rejection must not change legacy cached-result delivery"
        );
    }

    #[test]
    fn statically_ineligible_legacy_batches_stream_without_internal_ledger_retention() {
        let (request, _, _, _, events, _) = seal_fixture();
        let statically_sealable = is_statically_sealable_generation_batch(
            &request,
            GenerationBatchAdmission::Compatibility,
        );
        assert!(!statically_sealable);
        let mut retained_events = statically_sealable.then(Vec::new);
        let (event_tx, event_rx) = bounded(EVENT_CAPACITY);
        let cancellations = [Arc::new(AtomicBool::new(false))];
        let reasoning_forces = [Arc::new(AtomicBool::new(false))];
        let mut unrecorded_control_used = false;
        let expected = events[0].clone();
        BatchSupervision {
            event_tx: &event_tx,
            retained_events: retained_events.as_mut(),
            retain_token_piece_traces: false,
            unrecorded_control_used: statically_sealable.then_some(&mut unrecorded_control_used),
            runtime_sample_trace: None,
            cancellations: &cancellations,
            reasoning_forces: &reasoning_forces,
        }
        .emit(expected.clone());

        assert!(retained_events.is_none());
        assert!(!unrecorded_control_used);
        assert_eq!(
            event_rx.try_recv().expect("legacy event remains visible"),
            expected
        );
    }

    #[test]
    fn failed_strict_precheck_does_not_allocate_an_authority_ledger() {
        let failure: NativeResult<()> = Err(NativeError::new(
            NativeErrorCode::ModelInvalid,
            "strict artifact binding unavailable",
        ));
        assert!(should_retain_authority_evidence(true, &Ok(())));
        assert!(!should_retain_authority_evidence(true, &failure));
        assert!(!should_retain_authority_evidence(false, &Ok(())));
        let retained_events =
            should_retain_authority_evidence(true, &failure).then(Vec::<GenerationEvent>::new);
        assert!(retained_events.is_none());
    }

    #[test]
    fn verified_wait_variants_return_authority_while_legacy_wait_discards_it() {
        let ticket = completed_generation_ticket(verified_completion(test_verified_generation()));
        let verified = ticket
            .wait_verified()
            .expect("blocking verified wait returns the opaque seal");
        assert_eq!(verified.request().request_id, "verified-request");

        let ticket = completed_generation_ticket(verified_completion(test_verified_generation()));
        let verified = require_ready(
            ticket
                .wait_verified_timeout(Duration::from_millis(1))
                .expect("timed verified wait returns the opaque seal"),
        )
        .expect("completed fixture cannot time out");
        assert_eq!(verified.outputs().len(), 2);

        let ticket = completed_generation_ticket(verified_completion(test_verified_generation()));
        let verified = match ticket
            .try_wait_verified()
            .expect("nonblocking verified wait is typed")
        {
            TryWaitOutcome::Ready(verified) => verified,
            TryWaitOutcome::Pending(_) => panic!("completed result cannot remain pending"),
        };
        assert_eq!(verified.events().len(), 8);

        let ticket = completed_generation_ticket(verified_completion(test_verified_generation()));
        let outputs = ticket
            .wait()
            .expect("legacy wait preserves outputs while consuming authority");
        assert_eq!(outputs.len(), 2);

        let (_, _, outputs, _, _, _) = seal_fixture();
        let ticket = completed_generation_ticket(GenerationCompletion::unverified(outputs));
        let error = ticket
            .wait_verified()
            .expect_err("unverified paths cannot satisfy a verified wait");
        assert_eq!(error.code, NativeErrorCode::UnsupportedParameter);
    }

    #[test]
    fn strict_authority_failures_never_poison_legacy_output_delivery() {
        let failures = [
            (
                NativeErrorCode::ModelInvalid,
                "strict artifact precheck failed",
            ),
            (NativeErrorCode::Internal, "strict UTF-8 validation failed"),
            (
                NativeErrorCode::ModelInvalid,
                "strict artifact postcheck failed",
            ),
            (
                NativeErrorCode::Internal,
                "strict authority mint validation failed",
            ),
        ];

        for (code, message) in failures {
            let (_, _, outputs, _, _, _) = seal_fixture();
            let expected = outputs.clone();
            let legacy = completed_generation_ticket(GenerationCompletion::authority_rejected(
                outputs,
                NativeError::new(code, message),
            ));
            assert_eq!(
                legacy
                    .wait()
                    .expect("authority rejection must not poison legacy output"),
                expected
            );

            let (_, _, outputs, _, _, _) = seal_fixture();
            let strict = completed_generation_ticket(GenerationCompletion::authority_rejected(
                outputs,
                NativeError::new(code, message),
            ));
            let error = strict
                .wait_verified()
                .expect_err("verified wait must preserve the exact authority failure");
            assert_eq!(error.code, code);
            assert_eq!(error.message, message);
        }
    }

    #[test]
    fn dropped_external_nonterminals_do_not_change_the_retained_seal_ledger() {
        let (request, fingerprint, outputs, terminal_ids, expected_events, decoded) =
            seal_fixture();
        let (event_tx, event_rx) = bounded(1);
        let cancellations = (0..request.cases.len())
            .map(|_| Arc::new(AtomicBool::new(false)))
            .collect::<Vec<_>>();
        let reasoning_forces = (0..request.cases.len())
            .map(|_| Arc::new(AtomicBool::new(false)))
            .collect::<Vec<_>>();
        let mut retained_events = Vec::new();
        {
            let mut supervision = BatchSupervision {
                event_tx: &event_tx,
                retained_events: Some(&mut retained_events),
                retain_token_piece_traces: true,
                unrecorded_control_used: None,
                runtime_sample_trace: None,
                cancellations: &cancellations,
                reasoning_forces: &reasoning_forces,
            };
            for event in expected_events.iter().cloned() {
                supervision.emit(event);
            }
        }
        assert_eq!(event_rx.try_iter().count(), 1);
        assert_eq!(retained_events, expected_events);
        validate_verified_generation_batch(
            &request,
            &fingerprint,
            &outputs,
            &terminal_ids,
            &retained_events,
            &decoded,
            is_test_eog_token,
        )
        .expect("external backpressure cannot weaken retained authority");
    }

    #[test]
    fn embedding_context_is_temporary_and_has_its_own_exact_fingerprint() {
        let mut config = NativeModelConfig::local(PathBuf::from("model.gguf"));
        config.batch_tokens = 64;
        config.max_sequences = 4;
        let generation_before = generation_context_params(&config, 2_048);
        let generation_before_fingerprints = context_fingerprints(&generation_before);
        assert!(!generation_before.embeddings());
        assert_eq!(
            generation_before.pooling_type(),
            LlamaPoolingType::Unspecified
        );

        let embedding = embedding_context_params(&config, 2_048, 7, EmbeddingPooling::Mean);
        assert!(embedding.embeddings());
        assert_eq!(embedding.pooling_type(), LlamaPoolingType::Mean);
        assert_eq!(embedding.n_batch(), 7);
        assert_eq!(embedding.n_seq_max(), 1);
        let execution =
            embedding_execution_fingerprint(&test_model_fingerprint("model"), &embedding);
        assert_eq!(execution.context_tokens, 2_048);
        assert_eq!(execution.batch_tokens, 7);
        assert_eq!(execution.max_sequences, 1);
        assert_ne!(execution.kv_layout_sha256, "e".repeat(64));

        let generation_after = generation_context_params(&config, 2_048);
        assert!(!generation_after.embeddings());
        assert_eq!(
            generation_after.pooling_type(),
            LlamaPoolingType::Unspecified
        );
        assert_eq!(
            context_fingerprints(&generation_after),
            generation_before_fingerprints
        );
    }

    #[test]
    fn embedding_normalization_is_row_local_and_fail_closed() {
        let mut values = vec![3.0, 4.0, 0.0, 0.0, 5.0, 12.0];
        apply_embedding_normalization(&mut values, 2, 3, EmbeddingNormalization::L2)
            .expect("nonzero finite rows normalize");
        assert!((values[0] - 0.6).abs() < 1.0e-6);
        assert!((values[1] - 0.8).abs() < 1.0e-6);
        assert!((values[4] - (5.0 / 13.0)).abs() < 1.0e-6);
        assert!((values[5] - (12.0 / 13.0)).abs() < 1.0e-6);
        for row in values.chunks_exact(3) {
            let norm = row.iter().map(|value| value * value).sum::<f32>().sqrt();
            assert!((norm - 1.0).abs() < 1.0e-6);
        }

        let mut unchanged = vec![1.0, 2.0];
        apply_embedding_normalization(&mut unchanged, 1, 2, EmbeddingNormalization::None)
            .expect("finite unnormalized rows remain valid");
        assert_eq!(unchanged, vec![1.0, 2.0]);

        let mut zero = vec![0.0, 0.0];
        let zero_error = apply_embedding_normalization(&mut zero, 1, 2, EmbeddingNormalization::L2)
            .expect_err("zero-norm rows must fail");
        assert_eq!(zero_error.code, NativeErrorCode::ModelInvalid);

        let mut non_finite = vec![f32::NAN];
        let finite_error =
            apply_embedding_normalization(&mut non_finite, 1, 1, EmbeddingNormalization::None)
                .expect_err("non-finite model outputs must fail");
        assert_eq!(finite_error.code, NativeErrorCode::ModelInvalid);
    }

    #[test]
    fn embedding_cancellation_is_typed_and_ticket_scoped() {
        let cancellation = Arc::new(AtomicBool::new(false));
        check_embedding_cancellation(&cancellation).expect("unset cancellation permits work");
        let (_result_tx, result_rx) = bounded::<embedding_runtime::EmbeddingCompletion>(1);
        let (control, lease) =
            test_embedding_reservation("embedding-request", Arc::clone(&cancellation));
        drop(lease);
        let ticket = EmbeddingTicket {
            request_id: "embedding-request".to_string(),
            result: result_rx,
            control,
        };
        ticket.cancel();
        assert!(cancellation.load(Ordering::Acquire));
        assert_eq!(
            check_embedding_cancellation(&cancellation)
                .expect_err("set cancellation must stop work")
                .code,
            NativeErrorCode::Cancelled
        );
    }

    #[test]
    fn queued_embedding_shutdown_has_one_cancelled_completion() {
        let request = embedding_request("model", vec![1, 2]);
        let admitted_request_sha256 = embedding_runtime::embedding_request_sha256(&request);
        let (result_tx, result_rx) = bounded(1);
        let cancellation = Arc::new(AtomicBool::new(false));
        let (_, request_lease) =
            test_embedding_reservation("embedding-request", Arc::clone(&cancellation));
        reject_queued_command(WorkerCommand::EmbedBatch {
            request,
            admitted_request_sha256,
            result: result_tx,
            cancellation: Arc::clone(&cancellation),
            request_lease,
        });
        assert!(cancellation.load(Ordering::Acquire));
        let completion = result_rx.recv().expect("one shutdown completion");
        assert_eq!(
            completion.terminal(),
            embedding_runtime::EmbeddingCompletionTerminal::Cancelled
        );
        assert_eq!(
            completion
                .into_output()
                .expect_err("queued shutdown cannot publish values")
                .code,
            NativeErrorCode::Cancelled
        );
        assert!(result_rx.try_recv().is_err());
    }

    #[test]
    fn duplicate_embedding_request_id_fails_before_queue_admission() {
        let (command_tx, command_rx) = bounded(COMMAND_CAPACITY);
        let (shutdown_tx, _shutdown_rx) = bounded(1);
        let requests = Arc::new(RequestRegistry::new());
        let (_control, _lease) = requests
            .reserve(
                "embedding-request",
                RequestClass::Embedding,
                RequestControls::Embedding {
                    cancellation: Arc::new(AtomicBool::new(false)),
                },
            )
            .expect("existing embedding request reserves");
        let handle = NativeModelHandle {
            inner: Arc::new(NativeModelInner {
                worker_identity: Arc::new(WorkerIdentity),
                worker_id: "embedding-duplicate-test-worker".to_owned(),
                command_tx,
                shutdown_tx,
                closing: AtomicBool::new(false),
                admission: Mutex::new(()),
                requests,
                status: Arc::new(RwLock::new(ResidentModelStatus {
                    model_id: "model".to_string(),
                    model_path: PathBuf::new(),
                    state: ModelRuntimeState::Ready,
                    fingerprint: Some(test_model_fingerprint("model")),
                    descriptor: None,
                    active_sequences: 0,
                    max_sequences: 4,
                })),
            }),
        };
        let error = handle
            .embed_batch(embedding_request("model", vec![1]))
            .expect_err("duplicate embedding IDs fail before queueing");
        assert_eq!(error.code, NativeErrorCode::DuplicateActiveRequest);
        assert!(command_rx.try_recv().is_err());
    }

    #[test]
    fn dropped_embedding_ticket_keeps_request_id_reserved_until_executor_terminal() {
        let fingerprint = test_model_fingerprint("model");
        let status = ResidentModelStatus {
            model_id: "model".to_owned(),
            model_path: PathBuf::new(),
            state: ModelRuntimeState::Ready,
            fingerprint: Some(fingerprint),
            descriptor: None,
            active_sequences: 0,
            max_sequences: 4,
        };
        let (handle, command_rx) = test_admission_handle(status, COMMAND_CAPACITY);
        let request = embedding_request("model", vec![1]);

        let ticket = handle
            .embed_batch(request.clone())
            .expect("first embedding request is admitted");
        drop(ticket);
        assert_eq!(handle.inner.requests.active_count(), 1);
        assert_eq!(
            handle
                .embed_batch(request.clone())
                .expect_err("ticket drop cannot release executor identity")
                .code,
            NativeErrorCode::DuplicateActiveRequest
        );

        reject_queued_command(command_rx.recv().expect("executor owns queued embedding"));
        assert_eq!(handle.inner.requests.active_count(), 0);

        let retry = handle
            .embed_batch(request)
            .expect("embedding identity is reusable after terminal");
        reject_queued_command(command_rx.recv().expect("retry embedding command"));
        drop(retry);
        assert_eq!(handle.inner.requests.active_count(), 0);
    }

    #[test]
    fn queue_full_releases_unstarted_request_reservation() {
        let status = ResidentModelStatus {
            model_id: "model".to_owned(),
            model_path: PathBuf::new(),
            state: ModelRuntimeState::Ready,
            fingerprint: Some(test_model_fingerprint("model")),
            descriptor: None,
            active_sequences: 0,
            max_sequences: 4,
        };
        let (handle, _command_rx) = test_admission_handle(status, 0);
        let error = handle
            .embed_batch(embedding_request("model", vec![1]))
            .expect_err("zero-capacity queue rejects without a waiting worker");
        assert_eq!(error.code, NativeErrorCode::QueueFull);
        assert_eq!(handle.inner.requests.active_count(), 0);
    }

    #[test]
    fn stale_embedding_ticket_cannot_remove_or_cancel_reused_identity() {
        let old = Arc::new(AtomicBool::new(false));
        let (old_control, old_lease) =
            test_embedding_reservation("embedding-request", Arc::clone(&old));
        drop(old_lease);
        let (_result_tx, result_rx) = bounded::<embedding_runtime::EmbeddingCompletion>(1);
        let ticket = EmbeddingTicket {
            request_id: "embedding-request".to_string(),
            result: result_rx,
            control: old_control,
        };
        let replacement = Arc::new(AtomicBool::new(false));
        let replacement_registry = Arc::new(RequestRegistry::new());
        let (_replacement_control, _replacement_lease) = replacement_registry
            .reserve(
                "embedding-request",
                RequestClass::Embedding,
                RequestControls::Embedding {
                    cancellation: Arc::clone(&replacement),
                },
            )
            .expect("replacement request reserves");
        drop(ticket);
        assert!(!replacement.load(Ordering::Acquire));
        assert_eq!(replacement_registry.active_count(), 1);
    }

    #[test]
    fn embedding_request_requires_the_ready_matching_resident_model() {
        let request = embedding_request("other-model", vec![1]);
        let ready = ResidentModelStatus {
            model_id: "model".to_string(),
            model_path: PathBuf::from("model.gguf"),
            state: ModelRuntimeState::Ready,
            fingerprint: Some(test_model_fingerprint("model")),
            descriptor: None,
            active_sequences: 0,
            max_sequences: 1,
        };
        let mismatch = validate_embedding_batch_request(&request, &ready)
            .expect_err("a different model ID must fail admission");
        assert_eq!(mismatch.code, NativeErrorCode::ModelNotLoaded);

        let mut not_ready = ready;
        not_ready.state = ModelRuntimeState::Stopped;
        let request = embedding_request("model", vec![1]);
        let state_error = validate_embedding_batch_request(&request, &not_ready)
            .expect_err("a stopped model must fail admission");
        assert_eq!(state_error.code, NativeErrorCode::ModelNotLoaded);
    }

    #[test]
    fn embedding_token_ids_must_fit_the_live_vocabulary() {
        let request = embedding_request("model", vec![0, 9]);
        validate_embedding_token_ids_in_vocab(10, &request)
            .expect("upper-exclusive vocabulary range accepts its last token");

        let request = embedding_request("model", vec![10]);
        let range_error = validate_embedding_token_ids_in_vocab(10, &request)
            .expect_err("a token equal to vocabulary size must fail");
        assert_eq!(range_error.code, NativeErrorCode::InvalidConfig);
        assert!(range_error.message.contains("token ID 10"));

        let empty_vocab_error = validate_embedding_token_ids_in_vocab(0, &request)
            .expect_err("an empty reported vocabulary must fail");
        assert_eq!(empty_vocab_error.code, NativeErrorCode::ModelInvalid);
    }

    #[test]
    fn embedding_value_budget_fails_before_context_creation_or_decode() {
        validate_embedding_output_budget([2, 3], EmbeddingPooling::None, 8)
            .expect("small unpooled outputs fit");
        validate_embedding_output_budget([usize::MAX], EmbeddingPooling::Mean, 8)
            .expect("pooled output size does not scale with token count");

        let oversized_rows = MAX_EMBEDDING_VALUES_PER_OUTPUT / 8 + 1;
        let output_error =
            validate_embedding_output_budget([oversized_rows], EmbeddingPooling::None, 8)
                .expect_err("an oversized per-token output must fail preflight");
        assert_eq!(output_error.code, NativeErrorCode::InvalidConfig);

        let rows_per_input = MAX_EMBEDDING_VALUES_PER_OUTPUT / 8;
        let batch_error =
            validate_embedding_output_budget([rows_per_input; 5], EmbeddingPooling::None, 8)
                .expect_err("an oversized aggregate batch must fail preflight");
        assert_eq!(batch_error.code, NativeErrorCode::InvalidConfig);

        let width_error = validate_embedding_output_budget(
            [1],
            EmbeddingPooling::Mean,
            MAX_EMBEDDING_DIMENSIONS as usize + 1,
        )
        .expect_err("an impossible model width must fail preflight");
        assert_eq!(width_error.code, NativeErrorCode::ModelInvalid);
    }

    #[test]
    fn reported_media_kinds_match_exact_mtmd_support() {
        assert_eq!(
            media_kinds_from_support(false, false),
            Vec::<MediaKind>::new()
        );
        assert_eq!(
            media_kinds_from_support(true, false),
            vec![MediaKind::Image]
        );
        assert_eq!(
            media_kinds_from_support(false, true),
            vec![MediaKind::Audio]
        );
        assert_eq!(
            media_kinds_from_support(true, true),
            vec![MediaKind::Image, MediaKind::Audio]
        );
    }

    #[test]
    fn declared_media_kind_must_be_supported() {
        assert!(
            validate_declared_media_kind("image-1", MediaKind::Image, &[MediaKind::Image]).is_ok()
        );
        let error = validate_declared_media_kind("audio-1", MediaKind::Audio, &[MediaKind::Image])
            .expect_err("unsupported declared media must fail before decoding");
        assert_eq!(error.code, NativeErrorCode::UnsupportedMedia);
        assert!(error.message.contains("audio-1"));
    }

    #[test]
    fn decoded_media_kind_must_match_the_declaration() {
        assert!(validate_decoded_media_kind("image-1", MediaKind::Image, false).is_ok());
        assert!(validate_decoded_media_kind("audio-1", MediaKind::Audio, true).is_ok());

        let image_error = validate_decoded_media_kind("image-2", MediaKind::Image, true)
            .expect_err("audio decoded from declared image must fail");
        assert_eq!(image_error.code, NativeErrorCode::UnsupportedMedia);

        let audio_error = validate_decoded_media_kind("audio-2", MediaKind::Audio, false)
            .expect_err("image decoded from declared audio must fail");
        assert_eq!(audio_error.code, NativeErrorCode::UnsupportedMedia);
    }

    use std::path::PathBuf;

    #[test]
    fn missing_model_is_typed_before_binding_initialization() {
        let error = NativeModelOwner::load(NativeModelConfig::local(PathBuf::from(
            "/missing/model.gguf",
        )))
        .expect_err("missing models must fail");
        assert_eq!(error.code, NativeErrorCode::ModelMissing);
    }

    #[cfg(windows)]
    #[test]
    fn windows_preserves_legacy_load_but_refuses_strict_artifact_authority() {
        let directory = TestArtifactDirectory::new("windows-legacy-only");
        let configured_path = directory.join("model.gguf");
        std::fs::write(&configured_path, b"model").expect("write model artifact");
        let guard = ModelArtifactGuard::open(&configured_path, "model", None)
            .expect("Windows deny-sharing still supports legacy model loading");
        assert_eq!(guard.load_path, configured_path);
        let error = guard
            .verify_strict_unchanged()
            .expect_err("Windows lacks a reviewed handle-derived reopen identity");
        assert_eq!(error.code, NativeErrorCode::ModelInvalid);
        assert!(
            error
                .message
                .contains("cannot support strict generation authority")
        );
    }

    #[cfg(unix)]
    #[test]
    fn pinned_artifact_handle_defeats_path_rename_swap_and_invalidates_strict_authority() {
        let directory = TestArtifactDirectory::new("path-swap");
        let configured_path = directory.join("model.gguf");
        let displaced_path = directory.join("original.gguf");
        let replacement_path = directory.join("replacement.gguf");
        std::fs::write(&configured_path, b"model-a").expect("write original artifact");
        std::fs::write(&replacement_path, b"model-b").expect("write replacement artifact");

        let guard = ModelArtifactGuard::open(&configured_path, "model", None)
            .expect("regular local files support a pinned guarded open");
        guard
            .verify_strict_unchanged()
            .expect("unchanged held artifact verifies");
        assert_ne!(guard.load_path, configured_path);

        std::fs::rename(&configured_path, &displaced_path).expect("rename held inode away");
        std::fs::rename(&replacement_path, &configured_path).expect("replace configured path");

        assert_eq!(
            std::fs::read(&guard.load_path).expect("pinned descriptor path remains readable"),
            b"model-a",
            "llama.cpp's pinned reopen path must still name the originally opened inode"
        );
        let error = guard
            .verify_strict_unchanged()
            .expect_err("a configured-path identity swap must revoke strict authority");
        assert_eq!(error.code, NativeErrorCode::ModelInvalid);
    }

    #[cfg(unix)]
    #[test]
    fn held_artifact_metadata_detects_same_inode_in_place_mutation() {
        use std::io::{Seek as _, SeekFrom, Write as _};

        let directory = TestArtifactDirectory::new("in-place-change");
        let configured_path = directory.join("model.gguf");
        std::fs::write(&configured_path, b"alpha").expect("write original artifact");
        let guard = ModelArtifactGuard::open(&configured_path, "model", None)
            .expect("regular local files support a pinned guarded open");
        let payload_bytes_read = guard.payload_bytes_read();

        // Unix flock is intentionally advisory. This writer does not cooperate,
        // demonstrating that immutable identity/mutation metadata, not the lock
        // alone, closes the ordinary same-user mutation path.
        let mut writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&configured_path)
            .expect("an uncooperative same-user writer can ignore advisory flock");
        writer
            .seek(SeekFrom::Start(0))
            .expect("seek mutable test file");
        writer
            .write_all(b"omega")
            .expect("mutate bytes without changing file length");
        writer.sync_all().expect("flush mutation");

        let error = guard
            .verify_strict_unchanged()
            .expect_err("same-inode content mutation must revoke strict authority");
        assert_eq!(error.code, NativeErrorCode::ModelInvalid);
        assert_eq!(guard.payload_bytes_read(), payload_bytes_read);
    }

    #[cfg(unix)]
    #[test]
    fn strict_pre_post_artifact_checks_read_no_payload_after_initial_hash() {
        let directory = TestArtifactDirectory::new("metadata-only-verification");
        let configured_path = directory.join("model.gguf");
        std::fs::write(&configured_path, b"immutable model bytes").expect("write artifact");
        let guard = ModelArtifactGuard::open(&configured_path, "model", None)
            .expect("regular local files support guarded verification");
        let after_initial_hash = guard.payload_bytes_read();
        assert_eq!(after_initial_hash, 21);

        guard
            .verify_strict_unchanged()
            .expect("strict precheck accepts unchanged metadata");
        guard
            .verify_strict_unchanged()
            .expect("strict postcheck accepts unchanged metadata");
        assert_eq!(
            guard.payload_bytes_read(),
            after_initial_hash,
            "strict generation-bound checks must not reread model payload bytes"
        );
    }

    #[test]
    fn caller_expected_artifact_hash_fails_before_model_load() {
        let directory = TestArtifactDirectory::new("expected-hash");
        let configured_path = directory.join("model.gguf");
        std::fs::write(&configured_path, b"artifact").expect("write artifact");
        let wrong_sha256 = "0".repeat(64);
        let error = ModelArtifactGuard::open(&configured_path, "model", Some(&wrong_sha256))
            .expect_err("trusted expected digest must reject different bytes");
        assert_eq!(error.code, NativeErrorCode::ModelInvalid);
        assert!(error.message.contains("expected"));
    }

    #[test]
    fn expected_artifact_hash_configuration_is_canonical_and_projector_scoped() {
        let directory = TestArtifactDirectory::new("hash-config");
        let configured_path = directory.join("model.gguf");
        std::fs::write(&configured_path, b"artifact").expect("write artifact");
        let mut config = NativeModelConfig::local(configured_path);
        config.expected_model_sha256 = Some("A".repeat(64));
        let error = validate_config(&config).expect_err("uppercase digest is noncanonical");
        assert_eq!(error.code, NativeErrorCode::InvalidConfig);

        config.expected_model_sha256 = Some("a".repeat(64));
        config.expected_mmproj_sha256 = Some("b".repeat(64));
        let error = validate_config(&config)
            .expect_err("a projector digest without a projector path is ambiguous");
        assert_eq!(error.code, NativeErrorCode::InvalidConfig);
    }

    #[test]
    fn native_config_recognizes_gguf_content_instead_of_trusting_the_file_name() {
        let directory = TestArtifactDirectory::new("gguf-content-validation");
        let extensionless = directory.join("content-addressed-model");
        std::fs::write(&extensionless, b"GGUF").expect("write extensionless GGUF fixture");
        validate_config(&NativeModelConfig::local(extensionless))
            .expect("extensionless GGUF content is valid");

        let uppercase = directory.join("model.GGUF");
        std::fs::write(&uppercase, b"GGUF").expect("write uppercase GGUF fixture");
        validate_config(&NativeModelConfig::local(uppercase))
            .expect("uppercase extension does not hide valid GGUF content");

        let false_name = directory.join("not-a-model.gguf");
        std::fs::write(&false_name, b"nope").expect("write false GGUF fixture");
        let error = validate_config(&NativeModelConfig::local(false_name))
            .expect_err("a .gguf suffix cannot replace the GGUF header");
        assert_eq!(error.code, NativeErrorCode::ModelInvalid);
    }

    #[test]
    fn reasoning_force_only_targets_an_unclosed_reasoning_block() {
        assert_eq!(active_reasoning_end_marker("plain answer"), None);
        assert_eq!(
            active_reasoning_end_marker("<think>working"),
            Some("</think>")
        );
        assert_eq!(
            active_reasoning_end_marker("<think>done</think>answer"),
            None
        );
        assert_eq!(
            active_reasoning_end_marker(
                "<<<reasoning_content_start>>>working<<<reasoning_content_end>>>answer \
                 <<<reasoning_content_start>>>more"
            ),
            Some("<<<reasoning_content_end>>>")
        );
    }

    #[test]
    fn reasoning_force_registry_is_separate_and_branch_scoped() {
        let first = Arc::new(AtomicBool::new(false));
        let second = Arc::new(AtomicBool::new(false));
        let registry = Arc::new(RequestRegistry::new());
        let (control, _lease) = registry
            .reserve(
                "request",
                RequestClass::Generation,
                RequestControls::Generation {
                    cancellations: Vec::new(),
                    reasoning_forces: vec![
                        ("first".to_owned(), Arc::clone(&first)),
                        ("second".to_owned(), Arc::clone(&second)),
                    ],
                },
            )
            .expect("request reserves");
        assert!(control.force_reasoning_exit("first"));
        assert!(first.load(Ordering::Acquire));
        assert!(!second.load(Ordering::Acquire));
    }

    #[test]
    fn native_parallelism_is_bounded() {
        let mut config = NativeModelConfig::local(PathBuf::from("model.gguf"));
        config.max_sequences = MAX_PARALLEL_SEQUENCES + 1;
        let error = validate_config(&config).expect_err("excess sequences must fail");
        assert_eq!(error.code, NativeErrorCode::InvalidConfig);
    }

    fn test_status(max_sequences: u32) -> ResidentModelStatus {
        ResidentModelStatus {
            model_id: "model".to_string(),
            model_path: PathBuf::from("model.gguf"),
            state: ModelRuntimeState::Ready,
            fingerprint: None,
            descriptor: None,
            active_sequences: 0,
            max_sequences,
        }
    }

    fn completion_case(case_id: &str, seed: u32) -> GenerationCase {
        GenerationCase {
            case_id: case_id.to_string(),
            input: GenerationInput::Completion {
                prompts: vec![CompletionPrompt::Tokens {
                    token_ids: vec![1, 2, 3],
                }],
            },
            sampling: SamplingConfig {
                seed,
                ..SamplingConfig::default()
            },
            cached_prefix: None,
        }
    }

    #[test]
    fn generation_cases_validate_independent_sampling_and_identity() {
        let request = GenerationBatchRequest {
            request_id: "request".to_string(),
            model_id: "model".to_string(),
            cases: vec![completion_case("first", 1), completion_case("second", 2)],
        };
        validate_generation_batch_request(&request, &test_status(2)).expect("valid ordered cases");
        assert_eq!(request.cases[0].sampling.seed, 1);
        assert_eq!(request.cases[1].sampling.seed, 2);

        let mut duplicate = request;
        duplicate.cases[1].case_id = "first".to_string();
        let error = validate_generation_batch_request(&duplicate, &test_status(2))
            .expect_err("duplicate case identities must fail");
        assert_eq!(error.code, NativeErrorCode::InvalidConfig);
    }

    #[test]
    fn exact_token_preflight_accounts_for_the_whole_batch_at_the_context_boundary() {
        let mut first = completion_case("first", 1);
        first.sampling.max_tokens = 6;
        let mut second = completion_case("second", 2);
        second.sampling.max_tokens = 6;
        let request = GenerationBatchRequest {
            request_id: "request".to_string(),
            model_id: "model".to_string(),
            cases: vec![first, second],
        };
        let budget = exact_token_batch_cell_budget(&request).expect("exact-token budget");
        assert_eq!(budget.required_cells(), 14);
        for case in &request.cases {
            let one_case = GenerationBatchRequest {
                request_id: "single".to_string(),
                model_id: request.model_id.clone(),
                cases: vec![case.clone()],
            };
            assert_eq!(
                exact_token_batch_cell_budget(&one_case)
                    .expect("one-case budget")
                    .required_cells(),
                8
            );
        }

        let mut fingerprint = test_model_fingerprint("model");
        fingerprint.context_tokens = 13;
        let mut status = test_status(2);
        status.fingerprint = Some(fingerprint.clone());
        let error = exact_token_budget_for_submission(&request, &status)
            .expect_err("aggregate batch must not pass on per-case admission");
        assert_eq!(error.code, NativeErrorCode::PromptTooLarge);
        assert!(error.message.contains("requires 14 KV cells"));

        fingerprint.context_tokens = 14;
        status.fingerprint = Some(fingerprint);
        assert_eq!(
            exact_token_budget_for_submission(&request, &status)
                .expect("exact boundary fits")
                .expect("exact-token request")
                .required_cells(),
            14
        );
    }

    #[test]
    fn public_exact_token_budget_matches_execution_prefix_and_cell_values() {
        let mut first = completion_case("first", 1);
        first.sampling.max_tokens = 1;
        let mut second = completion_case("second", 2);
        second.input = GenerationInput::Completion {
            prompts: vec![CompletionPrompt::Tokens {
                token_ids: vec![1, 2, 4, 5],
            }],
        };
        second.sampling.max_tokens = 3;
        let request = GenerationBatchRequest {
            request_id: "request".to_string(),
            model_id: "model".to_string(),
            cases: vec![first, second],
        };
        let budget = exact_token_batch_cell_budget(&request).expect("exact-token budget");
        let normalized = SharedPrefixBatchRequest {
            request_id: request.request_id.clone(),
            model_id: request.model_id.clone(),
            common_messages: Vec::new(),
            chat_template: ChatTemplateChoice::ModelDefault,
            branches: request
                .cases
                .iter()
                .map(|case| BranchRequest {
                    branch_id: case.case_id.clone(),
                    label: case.case_id.clone(),
                    instruction: String::new(),
                    sampling: case.sampling.clone(),
                    messages: Vec::new(),
                    cached_prefix: None,
                })
                .collect(),
            cached_prefix: None,
        };
        let token_sets = request
            .cases
            .iter()
            .map(|case| {
                exact_generation_case_token_ids(case)
                    .expect("exact prompt")
                    .iter()
                    .copied()
                    .map(LlamaToken::new)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let cached_states = vec![None, None];
        let mut prefix_lengths = vec![0, 0];
        let (shared_prefix, execution_cells) = exact_budget_execution_values(
            &budget,
            &normalized,
            &token_sets,
            &cached_states,
            &mut prefix_lengths,
        )
        .expect("execution accepts the public preflight budget");

        assert_eq!(shared_prefix, 2);
        assert_eq!(prefix_lengths, vec![2, 2]);
        assert_eq!(execution_cells as u64, budget.required_cells());
        assert_eq!(execution_cells, 7);
    }

    #[test]
    fn one_case_cannot_hide_multiple_completion_occurrences() {
        let mut case = completion_case("ambiguous", 1);
        case.input = GenerationInput::Completion {
            prompts: vec![
                CompletionPrompt::Tokens { token_ids: vec![1] },
                CompletionPrompt::Tokens { token_ids: vec![2] },
            ],
        };
        let request = GenerationBatchRequest {
            request_id: "request".to_string(),
            model_id: "model".to_string(),
            cases: vec![case],
        };
        let error = validate_generation_batch_request(&request, &test_status(1))
            .expect_err("one case must map to one output");
        assert_eq!(error.code, NativeErrorCode::InvalidConfig);
        assert!(error.message.contains("exactly one completion prompt"));
    }

    #[test]
    fn bounded_event_stream_reserves_one_terminal_for_every_case() {
        let request = GenerationBatchRequest {
            request_id: "request".to_string(),
            model_id: "model".to_string(),
            cases: (0..MAX_PARALLEL_SEQUENCES)
                .map(|index| completion_case(&format!("case-{index}"), index))
                .collect(),
        };
        let (event_tx, event_rx) = bounded(EVENT_CAPACITY);
        for index in 0..EVENT_CAPACITY * 2 {
            try_emit_nonterminal(
                &event_tx,
                GenerationEvent {
                    request_id: request.request_id.clone(),
                    branch_id: "case-0".to_string(),
                    sequence_id: 0,
                    input_index: 0,
                    event_index: index as u64,
                    event: GenerationEventKind::Delta {
                        text: "x".to_string(),
                    },
                },
            );
        }
        emit_failed_case_events(&event_tx, &request);

        let events = event_rx.try_iter().collect::<Vec<_>>();
        assert_eq!(events.len(), EVENT_CAPACITY);
        for case in &request.cases {
            let terminals = events
                .iter()
                .filter(|event| {
                    event.branch_id == case.case_id
                        && matches!(
                            event.event,
                            GenerationEventKind::State {
                                state: GenerationState::Failed
                            }
                        )
                })
                .count();
            assert_eq!(terminals, 1, "{} must have one terminal", case.case_id);
        }
    }

    #[test]
    fn inspected_capabilities_do_not_invent_probabilities_or_media_limits() {
        let capabilities = inspected_capabilities(true, 4, &[MediaKind::Image]);
        assert_eq!(
            capabilities.declaration,
            CapabilityDeclarationStatus::Inspected
        );
        assert!(capabilities.prompts.chat);
        assert!(capabilities.prompts.completion_token_ids);
        assert!(capabilities.prompts.fill_in_middle.is_none());
        assert!(capabilities.outputs.generated_token_ids);
        assert!(!capabilities.outputs.token_observations);
        assert!(capabilities.outputs.probability_stages.is_empty());
        assert!(capabilities.outputs.log_probability_stages.is_empty());
        assert!(capabilities.batches.per_case_sampling);
        assert!(capabilities.batches.per_case_cancellation);
        assert_eq!(capabilities.media.len(), 1);
        assert!(capabilities.media[0].accepted_mime_types.is_none());
        assert!(capabilities.media[0].max_bytes_per_object.is_none());
    }

    #[test]
    fn longest_prefix_is_token_exact() {
        let first = vec![LlamaToken::new(1), LlamaToken::new(2), LlamaToken::new(3)];
        let second = vec![LlamaToken::new(1), LlamaToken::new(2), LlamaToken::new(4)];
        assert_eq!(longest_common_prefix(&[first, second]), 2);
    }

    #[test]
    fn gemma_family_uses_the_supported_named_template_when_embedded_jinja_is_too_new() {
        assert_eq!(fallback_chat_template_name("gemma4", ""), Some("gemma"));
        assert_eq!(
            fallback_chat_template_name("unknown", "{{ '<start_of_turn>' }}"),
            Some("gemma")
        );
        assert_eq!(fallback_chat_template_name("qwen2", "chatml"), None);
    }

    #[test]
    #[ignore = "requires MOM_LLAMA_MODEL_PATH and a real local GGUF"]
    fn real_in_process_prompt_smoke() -> Result<(), Box<dyn std::error::Error>> {
        let _real_model_guard = REAL_MODEL_TEST_LOCK.lock().expect("real-model test lock");
        let model_path = std::env::var("MOM_LLAMA_MODEL_PATH")?;
        let mut config = NativeModelConfig::local(PathBuf::from(model_path));
        // CI and sandboxed smoke tests cannot assume access to a Metal command queue.
        config.device = NativeDevice::Cpu;
        let owner = NativeModelOwner::load(config)?;
        let handle = owner.handle();
        let model_id = handle.status().model_id;
        let ticket = handle.generate(GenerationRequest {
            request_id: "native-real-smoke".to_string(),
            model_id,
            input: GenerationInput::Chat {
                messages: vec![ChatMessage {
                    role: ChatRole::User,
                    content: "Reply with the single word ready.".to_string(),
                }],
                template: ChatTemplateChoice::ModelDefault,
            },
            sampling: SamplingConfig {
                seed: 1,
                temperature: 0.0,
                max_tokens: 16,
                ..SamplingConfig::default()
            },
            media: Vec::new(),
            cached_prefix: None,
        })?;
        let output = ticket.wait()?;
        assert_eq!(output.len(), 1);
        assert!(output[0].real_engine_invoked);
        assert!(!output[0].fake_fixture);
        assert_eq!(output[0].transport, NativeTransport::InProcess);
        assert!(!output[0].text.trim().is_empty());
        Ok(())
    }

    #[test]
    #[ignore = "requires MOM_LLAMA_MODEL_PATH and a real local GGUF"]
    fn real_completion_text_tokens_batch_and_capabilities() -> Result<(), Box<dyn std::error::Error>>
    {
        let _real_model_guard = REAL_MODEL_TEST_LOCK.lock().expect("real-model test lock");
        let model_path = std::env::var("MOM_LLAMA_MODEL_PATH")?;
        let mut config = NativeModelConfig::local(PathBuf::from(model_path));
        config.device = NativeDevice::Cpu;
        config.max_sequences = 2;
        let owner = NativeModelOwner::load(config)?;
        let handle = owner.handle();
        let status = handle.status();
        let descriptor = status
            .descriptor
            .as_ref()
            .expect("loaded models expose an inspected descriptor");
        assert!(descriptor.stable_model_id.starts_with("sha256:"));
        assert!(
            descriptor
                .capabilities
                .prompt_forms
                .contains(&PromptForm::Completion)
        );
        assert!(
            !descriptor
                .capabilities
                .prompt_forms
                .contains(&PromptForm::FillInMiddle)
        );
        assert_eq!(
            descriptor.capabilities.exact.declaration,
            CapabilityDeclarationStatus::Inspected
        );
        assert!(descriptor.capabilities.exact.outputs.generated_token_ids);
        assert!(
            descriptor
                .capabilities
                .exact
                .outputs
                .probability_stages
                .is_empty()
        );

        let exact_text = "  Once upon a time\n".to_string();
        let prepared_text = handle.prepare_input(GenerationInput::Completion {
            prompts: vec![CompletionPrompt::Text {
                text: exact_text.clone(),
                special_tokens: SpecialTokenPolicy::NoBosParseSpecial,
            }],
        })?;
        assert_eq!(
            prepared_text[0].source_sha256,
            format!("{:x}", Sha256::digest(exact_text.as_bytes()))
        );
        let exact_tokens = prepared_text[0].token_ids.clone();
        let prepared_tokens = handle.prepare_input(GenerationInput::Completion {
            prompts: vec![CompletionPrompt::Tokens {
                token_ids: exact_tokens.clone(),
            }],
        })?;
        assert_eq!(prepared_tokens[0].token_ids, exact_tokens);
        assert_eq!(
            prepared_tokens[0].token_policy,
            PromptTokenPolicy::ExactTokenIds
        );

        let ticket = handle.generate(GenerationRequest {
            request_id: "native-completion-batch".to_string(),
            model_id: status.model_id,
            input: GenerationInput::Completion {
                prompts: vec![
                    CompletionPrompt::Text {
                        text: exact_text,
                        special_tokens: SpecialTokenPolicy::NoBosParseSpecial,
                    },
                    CompletionPrompt::Tokens {
                        token_ids: exact_tokens.clone(),
                    },
                ],
            },
            sampling: SamplingConfig {
                seed: 7,
                temperature: 0.0,
                max_tokens: 4,
                ..SamplingConfig::default()
            },
            media: Vec::new(),
            cached_prefix: None,
        })?;
        let event_rx = ticket.events.clone();
        let outputs = require_ready(ticket.wait_timeout(Duration::from_mins(2))?)?;
        let events = event_rx.try_iter().collect::<Vec<_>>();
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].input_index, 0);
        assert_eq!(outputs[0].branch_id, "completion-0");
        assert_eq!(outputs[1].input_index, 1);
        assert_eq!(outputs[1].branch_id, "completion-1");
        assert!(outputs.iter().all(|output| output.real_engine_invoked));
        assert!(outputs.iter().all(|output| {
            output.generated_token_ids.len() == output.metrics.completion_tokens
                && output.token_observations.is_none()
        }));
        for input_index in 0..2 {
            let indexes = events
                .iter()
                .filter(|event| event.input_index == input_index)
                .map(|event| event.event_index)
                .collect::<Vec<_>>();
            assert!(!indexes.is_empty());
            assert!(indexes.windows(2).all(|pair| pair[0] < pair[1]));
        }

        let family_request = GenerationBatchRequest {
            request_id: "native-raw-family".to_string(),
            model_id: descriptor.model_id.clone(),
            cases: vec![
                GenerationCase {
                    case_id: "seed-41".to_string(),
                    input: GenerationInput::Completion {
                        prompts: vec![CompletionPrompt::Tokens {
                            token_ids: exact_tokens.clone(),
                        }],
                    },
                    sampling: SamplingConfig {
                        seed: 41,
                        max_tokens: 4,
                        ..SamplingConfig::default()
                    },
                    cached_prefix: None,
                },
                GenerationCase {
                    case_id: "seed-42".to_string(),
                    input: GenerationInput::Completion {
                        prompts: vec![CompletionPrompt::Tokens {
                            token_ids: exact_tokens.clone(),
                        }],
                    },
                    sampling: SamplingConfig {
                        seed: 42,
                        max_tokens: 4,
                        ..SamplingConfig::default()
                    },
                    cached_prefix: None,
                },
            ],
        };
        let family_ticket = handle.generate_batch(family_request.clone())?;
        let family_event_rx = family_ticket.events.clone();
        let verified = require_ready(family_ticket.wait_verified_timeout(Duration::from_mins(2))?)?;
        let family_events = family_event_rx.try_iter().collect::<Vec<_>>();
        let family_outputs = verified.outputs();
        assert_eq!(verified.request(), &family_request);
        assert_eq!(verified.model_fingerprint().model_id, descriptor.model_id);
        assert!(verified.events().len() >= family_events.len());
        assert_eq!(
            verified.terminal_sampled_token_ids().len(),
            family_outputs.len()
        );
        for (output, terminal_token_id) in family_outputs
            .iter()
            .zip(verified.terminal_sampled_token_ids())
        {
            assert_eq!(
                terminal_token_id.is_some(),
                output.finish_reason == "end_of_generation"
            );
        }
        assert_eq!(family_outputs[0].branch_id, "seed-41");
        assert_eq!(family_outputs[1].branch_id, "seed-42");
        assert!(family_outputs.iter().all(|output| {
            output.metrics.cache.batch_shared_prefix_tokens == exact_tokens.len() - 1
        }));
        for case_id in ["seed-41", "seed-42"] {
            let terminals = family_events
                .iter()
                .filter(|event| {
                    event.branch_id == case_id
                        && matches!(
                            event.event,
                            GenerationEventKind::State {
                                state: GenerationState::Completed
                                    | GenerationState::Cancelled
                                    | GenerationState::Failed
                            }
                        )
                })
                .count();
            assert_eq!(terminals, 1);
        }
        for case_id in ["seed-41", "seed-42"] {
            let retained = verified
                .events()
                .iter()
                .filter(|event| event.branch_id == case_id)
                .collect::<Vec<_>>();
            assert!(
                retained
                    .iter()
                    .enumerate()
                    .all(|(index, event)| event.event_index == index as u64)
            );
            assert_eq!(
                retained
                    .iter()
                    .filter(|event| {
                        matches!(
                            event.event,
                            GenerationEventKind::State {
                                state: GenerationState::Completed | GenerationState::Cancelled
                            }
                        )
                    })
                    .count(),
                1
            );
        }

        let mut legacy_request = family_request;
        legacy_request.request_id = "native-raw-family-legacy".to_string();
        let legacy_outputs = require_ready(
            handle
                .generate_batch(legacy_request)?
                .wait_timeout(Duration::from_mins(2))?,
        )?;
        for (sealed, legacy) in family_outputs.iter().zip(&legacy_outputs) {
            assert_eq!(sealed.branch_id, legacy.branch_id);
            assert_eq!(sealed.generated_token_ids, legacy.generated_token_ids);
            assert_eq!(sealed.text, legacy.text);
            assert_eq!(sealed.state, legacy.state);
            assert_eq!(sealed.finish_reason, legacy.finish_reason);
        }

        let default_seed_request = GenerationBatchRequest {
            request_id: "native-default-seed-strict".to_string(),
            model_id: descriptor.model_id.clone(),
            cases: vec![GenerationCase {
                case_id: "default-seed".to_string(),
                input: GenerationInput::Completion {
                    prompts: vec![CompletionPrompt::Tokens {
                        token_ids: exact_tokens.clone(),
                    }],
                },
                sampling: SamplingConfig {
                    max_tokens: 1,
                    ..SamplingConfig::default()
                },
                cached_prefix: None,
            }],
        };
        let default_seed_error = handle
            .generate_batch(default_seed_request.clone())?
            .wait_verified_timeout(Duration::from_mins(2))
            .expect_err("llama.cpp's randomized seed sentinel must stay unverified");
        assert_eq!(
            default_seed_error.code,
            NativeErrorCode::UnsupportedParameter
        );
        let mut legacy_default_seed_request = default_seed_request;
        legacy_default_seed_request.request_id = "native-default-seed-legacy".to_string();
        let legacy_default_seed_outputs = require_ready(
            handle
                .generate_batch(legacy_default_seed_request)?
                .wait_timeout(Duration::from_mins(2))?,
        )?;
        assert_eq!(legacy_default_seed_outputs.len(), 1);
        assert!(legacy_default_seed_outputs[0].real_engine_invoked);

        assert!(exact_tokens.len() >= 2);
        let alternate_tokens = handle.prepare_input(GenerationInput::Completion {
            prompts: vec![CompletionPrompt::Text {
                text: "Unrelated cache source".to_string(),
                special_tokens: SpecialTokenPolicy::NoBosParseSpecial,
            }],
        })?[0]
            .token_ids
            .clone();
        let source_token = alternate_tokens
            .into_iter()
            .find(|token_id| *token_id != exact_tokens[0])
            .expect("alternate text must contain a token distinct from the target prefix");
        let cache_source_request = GenerationBatchRequest {
            request_id: "native-forged-cache-source".to_string(),
            model_id: descriptor.model_id.clone(),
            cases: vec![GenerationCase {
                case_id: "source".to_string(),
                input: GenerationInput::Completion {
                    prompts: vec![CompletionPrompt::Tokens {
                        token_ids: vec![source_token],
                    }],
                },
                sampling: SamplingConfig {
                    seed: 73,
                    temperature: 0.0,
                    max_tokens: 1,
                    ..SamplingConfig::default()
                },
                cached_prefix: None,
            }],
        };
        require_ready(
            handle
                .generate_batch(cache_source_request)?
                .wait_timeout(Duration::from_mins(2))?,
        )?;
        let mut forged_cache = handle.snapshot_sequence(0)?;
        assert_eq!(forged_cache.token_count, 1);
        assert_eq!(forged_cache.token_ids, vec![source_token]);
        forged_cache.token_ids = vec![exact_tokens[0]];

        let forged_request = GenerationBatchRequest {
            request_id: "native-forged-cache-strict".to_string(),
            model_id: descriptor.model_id.clone(),
            cases: vec![GenerationCase {
                case_id: "forged".to_string(),
                input: GenerationInput::Completion {
                    prompts: vec![CompletionPrompt::Tokens {
                        token_ids: exact_tokens.clone(),
                    }],
                },
                sampling: SamplingConfig {
                    seed: 79,
                    temperature: 0.0,
                    max_tokens: 2,
                    ..SamplingConfig::default()
                },
                cached_prefix: Some(forged_cache.clone()),
            }],
        };
        let forged_error = handle
            .generate_batch(forged_request.clone())?
            .wait_verified_timeout(Duration::from_mins(2))
            .expect_err("relabelled public cache bytes must stay unverified");
        assert_eq!(forged_error.code, NativeErrorCode::UnsupportedParameter);

        let mut legacy_forged_request = forged_request;
        legacy_forged_request.request_id = "native-forged-cache-legacy".to_string();
        let legacy_forged_outputs = require_ready(
            handle
                .generate_batch(legacy_forged_request)?
                .wait_timeout(Duration::from_mins(2))?,
        )?;
        assert_eq!(legacy_forged_outputs.len(), 1);
        assert!(legacy_forged_outputs[0].real_engine_invoked);
        assert_eq!(
            legacy_forged_outputs[0]
                .metrics
                .cache
                .supplied_prefix_tokens,
            1
        );
        assert_eq!(
            legacy_forged_outputs[0]
                .metrics
                .cache
                .restored_prefix_tokens,
            1
        );

        let cancel_ticket = handle.generate(GenerationRequest {
            request_id: "native-completion-cancel".to_string(),
            model_id: descriptor.model_id.clone(),
            input: GenerationInput::Completion {
                prompts: vec![
                    CompletionPrompt::Text {
                        text: "Continue this sentence: The first result".to_string(),
                        special_tokens: SpecialTokenPolicy::NoBosParseSpecial,
                    },
                    CompletionPrompt::Text {
                        text: "Continue this sentence: The cancelled result".to_string(),
                        special_tokens: SpecialTokenPolicy::NoBosParseSpecial,
                    },
                ],
            },
            sampling: SamplingConfig {
                seed: 9,
                temperature: 0.0,
                max_tokens: 8,
                ..SamplingConfig::default()
            },
            media: Vec::new(),
            cached_prefix: None,
        })?;
        assert!(cancel_ticket.cancel_branch("completion-1"));
        let cancelled_outputs = require_ready(cancel_ticket.wait_timeout(Duration::from_mins(2))?)?;
        assert_eq!(cancelled_outputs[0].state, GenerationState::Completed);
        assert_eq!(cancelled_outputs[1].state, GenerationState::Cancelled);

        let error = handle
            .generate(GenerationRequest {
                request_id: "native-fim-blocker".to_string(),
                model_id: descriptor.model_id.clone(),
                input: GenerationInput::FillInMiddle {
                    prefix: "fn main() {".to_string(),
                    suffix: "}".to_string(),
                },
                sampling: SamplingConfig::default(),
                media: Vec::new(),
                cached_prefix: None,
            })
            .expect_err("unverified FIM must fail closed");
        assert_eq!(error.code, NativeErrorCode::UnsupportedPromptForm);
        Ok(())
    }

    #[test]
    #[ignore = "requires MOM_LLAMA_MODEL_PATH, MOM_LLAMA_MODEL_SHA256, and a real local GGUF"]
    fn real_strict_batch_retains_a_pre_cancelled_case_under_exact_model_binding()
    -> Result<(), Box<dyn std::error::Error>> {
        let _real_model_guard = REAL_MODEL_TEST_LOCK.lock().expect("real-model test lock");
        let model_path = std::env::var("MOM_LLAMA_MODEL_PATH")?;
        let model_sha256 = std::env::var("MOM_LLAMA_MODEL_SHA256")?;
        let mut config = NativeModelConfig::local(PathBuf::from(model_path));
        config.expected_model_sha256 = Some(model_sha256.clone());
        config.device = NativeDevice::Cpu;
        config.context_tokens = 512;
        config.batch_tokens = 64;
        config.max_sequences = 2;
        let owner = NativeModelOwner::load(config)?;
        let handle = owner.handle();
        let status = handle.status();
        let fingerprint = status
            .fingerprint
            .as_ref()
            .expect("real resident exposes a fingerprint");
        assert_eq!(fingerprint.model_sha256, model_sha256);

        let prepared = handle.prepare_input(GenerationInput::Completion {
            prompts: vec![CompletionPrompt::Text {
                text: "The rain stopped before Mara reached the house.".to_string(),
                special_tokens: SpecialTokenPolicy::NoBosParseSpecial,
            }],
        })?;
        let prompt_tokens = prepared
            .first()
            .expect("one prepared completion")
            .token_ids
            .clone();
        assert!(!prompt_tokens.is_empty());

        let request = GenerationBatchRequest {
            request_id: "native-real-strict-mixed-cancel".to_string(),
            model_id: status.model_id,
            cases: vec![
                GenerationCase {
                    case_id: "completed".to_string(),
                    input: GenerationInput::Completion {
                        prompts: vec![CompletionPrompt::Tokens {
                            token_ids: prompt_tokens.clone(),
                        }],
                    },
                    sampling: SamplingConfig {
                        seed: 101,
                        temperature: 0.0,
                        max_tokens: 8,
                        ..SamplingConfig::default()
                    },
                    cached_prefix: None,
                },
                GenerationCase {
                    case_id: "cancelled".to_string(),
                    input: GenerationInput::Completion {
                        prompts: vec![CompletionPrompt::Tokens {
                            token_ids: prompt_tokens,
                        }],
                    },
                    sampling: SamplingConfig {
                        seed: 102,
                        temperature: 0.0,
                        max_tokens: 128,
                        ..SamplingConfig::default()
                    },
                    cached_prefix: None,
                },
            ],
        };
        let budget = exact_token_batch_cell_budget(&request)?;
        assert!(budget.fits(512));
        let ticket = handle.generate_batch(request.clone())?;
        assert!(ticket.cancel_branch("cancelled"));
        let verified = require_ready(ticket.wait_verified_timeout(Duration::from_mins(2))?)?;

        assert_eq!(verified.request(), &request);
        assert_eq!(verified.outputs().len(), 2);
        assert_eq!(verified.outputs()[0].state, GenerationState::Completed);
        assert_eq!(verified.outputs()[1].state, GenerationState::Cancelled);
        assert_eq!(verified.outputs()[1].finish_reason, "cancelled");
        assert!(verified.terminal_sampled_token_ids()[1].is_none());
        assert_eq!(
            verified.token_piece_traces().len(),
            verified.outputs().len()
        );
        for (index, output) in verified.outputs().iter().enumerate() {
            let trace = &verified.token_piece_traces()[index];
            assert_eq!(
                trace.cumulative_boundaries().len(),
                output.generated_token_ids.len() + 1
            );
            assert_eq!(trace.cumulative_boundaries().first(), Some(&0));
            assert_eq!(
                trace.cumulative_boundaries().last().copied(),
                Some(u64::try_from(trace.raw_piece_bytes().len())?)
            );
            assert_eq!(
                strict_verified_utf8_bytes(trace.raw_piece_bytes())?,
                output.text
            );
            let terminal_count = verified
                .events()
                .iter()
                .filter(|event| {
                    event.input_index == index
                        && matches!(
                            event.event,
                            GenerationEventKind::State {
                                state: GenerationState::Completed | GenerationState::Cancelled
                            }
                        )
                })
                .count();
            assert_eq!(terminal_count, 1);
            assert!(output.real_engine_invoked);
            assert!(!output.fake_fixture);
            assert_eq!(output.transport, NativeTransport::InProcess);
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires MOM_LLAMA_MODEL_PATH and a real local GGUF"]
    fn real_per_token_embeddings_preserve_generation_context()
    -> Result<(), Box<dyn std::error::Error>> {
        let _real_model_guard = REAL_MODEL_TEST_LOCK.lock().expect("real-model test lock");
        let model_path = std::env::var("MOM_LLAMA_MODEL_PATH")?;
        let mut config = NativeModelConfig::local(PathBuf::from(model_path));
        config.device = NativeDevice::Auto;
        config.context_tokens = 512;
        config.batch_tokens = 64;
        config.max_sequences = 1;
        let owner = NativeModelOwner::load(config)?;
        let handle = owner.handle();
        let status_before = handle.status();
        let resident_fingerprint = status_before
            .fingerprint
            .clone()
            .expect("loaded model has a resident fingerprint");
        let model_id = status_before.model_id;

        let prepared = handle.prepare_input(GenerationInput::Completion {
            prompts: vec![CompletionPrompt::Text {
                text: "Once upon a time, the locked garden opened at midnight.".to_string(),
                special_tokens: SpecialTokenPolicy::NoBosParseSpecial,
            }],
        })?;
        let mut exact_tokens = prepared[0].token_ids.clone();
        exact_tokens.truncate(8);
        assert!(
            exact_tokens.len() >= 2,
            "the real tokenizer must produce at least two test tokens"
        );

        let generation_request = |request_id: &str| GenerationRequest {
            request_id: request_id.to_string(),
            model_id: model_id.clone(),
            input: GenerationInput::Completion {
                prompts: vec![CompletionPrompt::Tokens {
                    token_ids: exact_tokens.clone(),
                }],
            },
            sampling: SamplingConfig {
                seed: 91,
                temperature: 0.0,
                max_tokens: 2,
                ..SamplingConfig::default()
            },
            media: Vec::new(),
            cached_prefix: None,
        };
        let before = require_ready(
            handle
                .generate(generation_request("embedding-baseline-before"))?
                .wait_timeout(Duration::from_mins(2))?,
        )?;

        let input_tokens = [exact_tokens.clone(), exact_tokens[..2].to_vec()];
        let cancelled_ticket = handle.embed_batch(EmbeddingBatchRequest::new(
            "real-cancelled-embedding".to_string(),
            model_id.clone(),
            vec![EmbeddingInput::new(
                "cancelled".to_string(),
                input_tokens[0].clone(),
            )?],
            EmbeddingPooling::None,
            EmbeddingNormalization::None,
        )?)?;
        cancelled_ticket.cancel();
        assert_eq!(
            cancelled_ticket
                .wait_verified_timeout(Duration::from_mins(2))
                .expect_err("cancelled real embeddings cannot mint a seal")
                .code,
            NativeErrorCode::Cancelled
        );
        assert_eq!(
            handle
                .status()
                .descriptor
                .as_ref()
                .expect("loaded model descriptor remains present")
                .capabilities
                .exact
                .evidence
                .embeddings
                .declaration(),
            CapabilityDeclarationStatus::Unreported,
            "failed authority must not publish embedding capability status"
        );

        let verified_embedding = require_ready(
            handle
                .embed_batch(EmbeddingBatchRequest::new(
                    "real-per-token-embedding".to_string(),
                    model_id.clone(),
                    vec![
                        EmbeddingInput::new("full".to_string(), input_tokens[0].clone())?,
                        EmbeddingInput::new("prefix".to_string(), input_tokens[1].clone())?,
                    ],
                    EmbeddingPooling::None,
                    EmbeddingNormalization::None,
                )?)?
                .wait_verified_timeout(Duration::from_mins(2))?,
        )?;
        let embedding = verified_embedding.output();

        assert_eq!(embedding.request_id(), "real-per-token-embedding");
        assert_eq!(embedding.model_id(), model_id);
        assert_eq!(embedding.config().pooling(), EmbeddingPooling::None);
        assert!(embedding.config().dimensions() > 0);
        assert_eq!(embedding.outputs().len(), input_tokens.len());
        for (index, (output, expected_tokens)) in embedding
            .outputs()
            .iter()
            .zip(input_tokens.iter())
            .enumerate()
        {
            assert_eq!(output.input_index(), index);
            assert_eq!(output.token_ids(), expected_tokens);
            assert_eq!(output.row_count() as usize, expected_tokens.len());
            assert_eq!(
                output.values().len(),
                expected_tokens.len() * embedding.config().dimensions() as usize
            );
            assert!(output.values().iter().all(|value| value.is_finite()));
            assert_eq!(
                output
                    .values()
                    .chunks_exact(embedding.config().dimensions() as usize)
                    .count(),
                expected_tokens.len()
            );
        }
        assert_eq!(embedding.evidence().transport(), NativeTransport::InProcess);
        assert!(embedding.evidence().real_engine_invoked());
        assert!(!embedding.evidence().fake_fixture());
        assert_eq!(
            embedding.model_fingerprint().model_sha256,
            resident_fingerprint.model_sha256
        );
        assert_eq!(
            embedding.model_fingerprint().tokenizer_sha256,
            resident_fingerprint.tokenizer_sha256
        );
        assert_eq!(
            embedding.model_fingerprint().batch_tokens,
            exact_tokens.len() as u32
        );
        assert_eq!(embedding.model_fingerprint().max_sequences, 1);
        assert_eq!(
            verified_embedding.execution_fingerprint(),
            embedding.model_fingerprint()
        );
        assert_eq!(
            verified_embedding.resident_model_fingerprint(),
            &resident_fingerprint
        );
        assert_eq!(
            verified_embedding.terminal(),
            VerifiedEmbeddingTerminal::Completed
        );
        assert_eq!(
            verified_embedding.requested_pooling(),
            EmbeddingPooling::None
        );
        assert_eq!(
            verified_embedding.requested_normalization(),
            EmbeddingNormalization::None
        );
        assert_eq!(verified_embedding.resolved_config(), embedding.config());
        assert_eq!(verified_embedding.transport(), NativeTransport::InProcess);
        assert_eq!(verified_embedding.request_sha256().len(), 64);
        assert_eq!(verified_embedding.output_bits_sha256().len(), 64);
        assert_eq!(verified_embedding.ledger_sha256().len(), 64);
        assert_eq!(verified_embedding.owner_call_sequence(), 1);
        assert_ne!(
            embedding.model_fingerprint().kv_layout_sha256,
            resident_fingerprint.kv_layout_sha256
        );

        let status_after_embedding = handle.status();
        assert_eq!(
            status_after_embedding.fingerprint.as_ref(),
            Some(&resident_fingerprint),
            "a temporary embedding context must not replace resident generation identity"
        );
        let embedding_capabilities = status_after_embedding
            .descriptor
            .as_ref()
            .expect("loaded model descriptor remains present")
            .capabilities
            .exact
            .evidence
            .embeddings;
        assert_eq!(
            embedding_capabilities.declaration(),
            CapabilityDeclarationStatus::Inspected
        );
        assert!(embedding_capabilities.pooling().none);
        assert_eq!(
            embedding_capabilities.dimensions(),
            Some(embedding.config().dimensions())
        );

        let after = require_ready(
            handle
                .generate(generation_request("embedding-baseline-after"))?
                .wait_timeout(Duration::from_mins(2))?,
        )?;
        assert_eq!(before.len(), 1);
        assert_eq!(after.len(), 1);
        assert_eq!(before[0].generated_token_ids, after[0].generated_token_ids);
        assert_eq!(before[0].text, after[0].text);
        assert_eq!(before[0].state, after[0].state);
        let joined = owner.shutdown_joined()?;
        assert!(verified_embedding.belongs_to_joined_model(&joined));
        Ok(())
    }
}
