pub mod control_math;
mod state_buffer;

use crossbeam_channel::{Receiver, Sender, bounded};
use encoding_rs::UTF_8;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::params::LlamaContextParams;
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
    ChatTemplateChoice, CompletionPrompt, ExactModelCapabilities, GenerationBatchCapabilities,
    GenerationBatchRequest, GenerationCacheMetrics, GenerationCase, GenerationEvent,
    GenerationEventKind, GenerationInput, GenerationMetrics, GenerationOutput,
    GenerationOutputCapabilities, GenerationRequest, GenerationState, MAX_PARALLEL_SEQUENCES,
    MediaInput, MediaInputCapability, MediaKind, ModelCapabilities, ModelFingerprint,
    ModelRuntimeState, NativeDevice, NativeError, NativeErrorCode, NativeEvidenceCapabilities,
    NativeModelConfig, NativeModelDescriptor, NativeTransport, PreparedPrompt,
    ProjectorRequirement, PromptForm, PromptInputCapabilities, PromptTokenPolicy,
    ResidentModelStatus, SamplerKind, SamplingConfig, SamplingParameter, SequenceStateBlob,
    SharedPrefixBatchRequest, SpecialTokenPolicy, TokenizedPrompt,
};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::Read;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const LLAMA_CPP_BINDING_VERSION: &str = "0.1.153";
const COMMAND_CAPACITY: usize = 32;
const EVENT_CAPACITY: usize = 256;

static BACKEND: OnceLock<Result<LlamaBackend, String>> = OnceLock::new();

type NativeResult<T> = Result<T, NativeError>;
type CancelKey = (String, String);
type CancelRegistry = Arc<Mutex<HashMap<CancelKey, Arc<AtomicBool>>>>;
type ReasoningForceRegistry = Arc<Mutex<HashMap<CancelKey, Arc<AtomicBool>>>>;

#[derive(Debug)]
pub struct GenerationTicket {
    pub request_id: String,
    pub events: Receiver<GenerationEvent>,
    result: Receiver<NativeResult<Vec<GenerationOutput>>>,
    cancellations: CancelRegistry,
    reasoning_forces: ReasoningForceRegistry,
}

impl GenerationTicket {
    pub fn cancel_branch(&self, branch_id: &str) -> bool {
        cancel_in_registry(&self.cancellations, &self.request_id, branch_id)
    }

    pub fn cancel_all(&self) -> usize {
        cancel_request_in_registry(&self.cancellations, &self.request_id)
    }

    pub fn wait(self) -> NativeResult<Vec<GenerationOutput>> {
        let result = self.result.recv().map_err(|error| {
            NativeError::new(
                NativeErrorCode::WorkerStopped,
                format!("native worker stopped before returning a result: {error}"),
            )
        })?;
        cleanup_request_in_registry(&self.cancellations, &self.request_id);
        cleanup_request_in_registry(&self.reasoning_forces, &self.request_id);
        result
    }

    pub fn wait_timeout(&self, timeout: Duration) -> NativeResult<Vec<GenerationOutput>> {
        let result = self.result.recv_timeout(timeout).map_err(|error| {
            NativeError::new(
                NativeErrorCode::WorkerStopped,
                format!("native generation did not finish before the timeout: {error}"),
            )
        })?;
        cleanup_request_in_registry(&self.cancellations, &self.request_id);
        cleanup_request_in_registry(&self.reasoning_forces, &self.request_id);
        result
    }

    pub fn try_wait(&self) -> NativeResult<Option<Vec<GenerationOutput>>> {
        match self.result.try_recv() {
            Ok(result) => {
                cleanup_request_in_registry(&self.cancellations, &self.request_id);
                cleanup_request_in_registry(&self.reasoning_forces, &self.request_id);
                result.map(Some)
            }
            Err(crossbeam_channel::TryRecvError::Empty) => Ok(None),
            Err(crossbeam_channel::TryRecvError::Disconnected) => Err(NativeError::new(
                NativeErrorCode::WorkerStopped,
                "native worker stopped before returning a result",
            )),
        }
    }
}

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

#[derive(Debug)]
struct NativeModelInner {
    command_tx: Sender<WorkerCommand>,
    cancellations: CancelRegistry,
    reasoning_forces: ReasoningForceRegistry,
    status: Arc<RwLock<ResidentModelStatus>>,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for NativeModelInner {
    fn drop(&mut self) {
        if let Ok(registry) = self.cancellations.lock() {
            for flag in registry.values() {
                flag.store(true, Ordering::Release);
            }
        }
        if let Ok(registry) = self.reasoning_forces.lock() {
            for flag in registry.values() {
                flag.store(true, Ordering::Release);
            }
        }
        let _ = self.command_tx.try_send(WorkerCommand::Shutdown);
        if let Ok(join) = self.join.get_mut()
            && let Some(handle) = join.take()
        {
            let _ = handle.join();
        }
    }
}

#[derive(Debug)]
enum WorkerCommand {
    GenerateBatch {
        request: GenerationBatchRequest,
        event_tx: Sender<GenerationEvent>,
        result_tx: Sender<NativeResult<Vec<GenerationOutput>>>,
        cancellations: Vec<Arc<AtomicBool>>,
        reasoning_forces: Vec<Arc<AtomicBool>>,
    },
    Generate {
        request: SharedPrefixBatchRequest,
        event_tx: Sender<GenerationEvent>,
        result_tx: Sender<NativeResult<Vec<GenerationOutput>>>,
        cancellations: Vec<Arc<AtomicBool>>,
        reasoning_forces: Vec<Arc<AtomicBool>>,
    },
    GenerateMultimodal {
        request: GenerationRequest,
        event_tx: Sender<GenerationEvent>,
        result_tx: Sender<NativeResult<Vec<GenerationOutput>>>,
        cancellation: Arc<AtomicBool>,
        reasoning_force: Arc<AtomicBool>,
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
    Shutdown,
}

impl NativeModelHandle {
    pub fn load(config: NativeModelConfig) -> NativeResult<Self> {
        validate_config(&config)?;
        let (command_tx, command_rx) = bounded(COMMAND_CAPACITY);
        let (ready_tx, ready_rx) = bounded(1);
        let cancellations = Arc::new(Mutex::new(HashMap::new()));
        let reasoning_forces = Arc::new(Mutex::new(HashMap::new()));
        let status = Arc::new(RwLock::new(ResidentModelStatus {
            model_id: config.model_id.clone(),
            state: ModelRuntimeState::Loading,
            fingerprint: None,
            descriptor: None,
            active_sequences: 0,
            max_sequences: config.max_sequences,
        }));
        let worker_status = Arc::clone(&status);
        let worker = thread::Builder::new()
            .name(format!("llama-model-{}", config.model_id))
            .spawn(move || run_worker(config, command_rx, ready_tx, worker_status))
            .map_err(|error| {
                NativeError::new(
                    NativeErrorCode::Internal,
                    format!("failed to start native model worker: {error}"),
                )
            })?;
        ready_rx
            .recv_timeout(Duration::from_secs(300))
            .map_err(|error| {
                NativeError::new(
                    NativeErrorCode::ModelLoadFailed,
                    format!("native model worker did not become ready: {error}"),
                )
            })??;
        Ok(Self {
            inner: Arc::new(NativeModelInner {
                command_tx,
                cancellations,
                reasoning_forces,
                status,
                join: Mutex::new(Some(worker)),
            }),
        })
    }

    pub fn status(&self) -> ResidentModelStatus {
        self.inner
            .status
            .read()
            .map(|status| status.clone())
            .unwrap_or_else(|_| ResidentModelStatus {
                model_id: "unknown".to_string(),
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
        self.generate_batch(GenerationBatchRequest {
            request_id,
            model_id,
            cases,
        })
    }

    /// Submit an ordered family of independently sampled raw generation cases.
    pub fn generate_batch(
        &self,
        request: GenerationBatchRequest,
    ) -> NativeResult<GenerationTicket> {
        validate_generation_batch_request(&request, &self.status())?;
        let mut cancellations = Vec::with_capacity(request.cases.len());
        let mut reasoning_forces = Vec::with_capacity(request.cases.len());
        {
            let mut registry = self.inner.cancellations.lock().map_err(|_| {
                NativeError::new(
                    NativeErrorCode::Internal,
                    "cancellation registry is poisoned",
                )
            })?;
            let mut reasoning_registry = self.inner.reasoning_forces.lock().map_err(|_| {
                NativeError::new(NativeErrorCode::Internal, "reasoning registry is poisoned")
            })?;
            for case in &request.cases {
                let cancellation = Arc::new(AtomicBool::new(false));
                let reasoning = Arc::new(AtomicBool::new(false));
                registry.insert(
                    (request.request_id.clone(), case.case_id.clone()),
                    Arc::clone(&cancellation),
                );
                reasoning_registry.insert(
                    (request.request_id.clone(), case.case_id.clone()),
                    Arc::clone(&reasoning),
                );
                cancellations.push(cancellation);
                reasoning_forces.push(reasoning);
            }
        }
        let (event_tx, event_rx) = bounded(EVENT_CAPACITY);
        let (result_tx, result_rx) = bounded(1);
        let request_id = request.request_id.clone();
        if let Err(error) = self.inner.command_tx.send(WorkerCommand::GenerateBatch {
            request,
            event_tx,
            result_tx,
            cancellations,
            reasoning_forces,
        }) {
            cleanup_request_in_registry(&self.inner.cancellations, &request_id);
            cleanup_request_in_registry(&self.inner.reasoning_forces, &request_id);
            return Err(NativeError::new(
                NativeErrorCode::WorkerStopped,
                format!("native model worker is not accepting batch requests: {error}"),
            ));
        }
        Ok(GenerationTicket {
            request_id,
            events: event_rx,
            result: result_rx,
            cancellations: Arc::clone(&self.inner.cancellations),
            reasoning_forces: Arc::clone(&self.inner.reasoning_forces),
        })
    }

    fn generate_multimodal(&self, request: GenerationRequest) -> NativeResult<GenerationTicket> {
        validate_generation_request(&request, &self.status())?;
        let cancellation = Arc::new(AtomicBool::new(false));
        let reasoning_force = Arc::new(AtomicBool::new(false));
        {
            let mut registry = self.inner.cancellations.lock().map_err(|_| {
                NativeError::new(
                    NativeErrorCode::Internal,
                    "cancellation registry is poisoned",
                )
            })?;
            registry.insert(
                (request.request_id.clone(), "assistant".to_string()),
                Arc::clone(&cancellation),
            );
        }
        {
            let mut registry = self.inner.reasoning_forces.lock().map_err(|_| {
                NativeError::new(
                    NativeErrorCode::Internal,
                    "reasoning force registry is poisoned",
                )
            })?;
            registry.insert(
                (request.request_id.clone(), "assistant".to_string()),
                Arc::clone(&reasoning_force),
            );
        }
        let (event_tx, event_rx) = bounded(EVENT_CAPACITY);
        let (result_tx, result_rx) = bounded(1);
        let request_id = request.request_id.clone();
        if let Err(error) = self
            .inner
            .command_tx
            .send(WorkerCommand::GenerateMultimodal {
                request,
                event_tx,
                result_tx,
                cancellation,
                reasoning_force,
            })
        {
            cleanup_request_in_registry(&self.inner.cancellations, &request_id);
            cleanup_request_in_registry(&self.inner.reasoning_forces, &request_id);
            return Err(NativeError::new(
                NativeErrorCode::WorkerStopped,
                format!("native model worker is not accepting requests: {error}"),
            ));
        }
        Ok(GenerationTicket {
            request_id,
            events: event_rx,
            result: result_rx,
            cancellations: Arc::clone(&self.inner.cancellations),
            reasoning_forces: Arc::clone(&self.inner.reasoning_forces),
        })
    }

    pub fn generate_shared_prefix(
        &self,
        request: SharedPrefixBatchRequest,
    ) -> NativeResult<GenerationTicket> {
        validate_batch_request(&request, &self.status())?;
        let mut flags = Vec::with_capacity(request.branches.len());
        let mut reasoning_flags = Vec::with_capacity(request.branches.len());
        {
            let mut registry = self.inner.cancellations.lock().map_err(|_| {
                NativeError::new(
                    NativeErrorCode::Internal,
                    "cancellation registry is poisoned",
                )
            })?;
            for branch in &request.branches {
                let flag = Arc::new(AtomicBool::new(false));
                registry.insert(
                    (request.request_id.clone(), branch.branch_id.clone()),
                    Arc::clone(&flag),
                );
                flags.push(flag);
            }
        }
        {
            let mut registry = self.inner.reasoning_forces.lock().map_err(|_| {
                NativeError::new(
                    NativeErrorCode::Internal,
                    "reasoning force registry is poisoned",
                )
            })?;
            for branch in &request.branches {
                let flag = Arc::new(AtomicBool::new(false));
                registry.insert(
                    (request.request_id.clone(), branch.branch_id.clone()),
                    Arc::clone(&flag),
                );
                reasoning_flags.push(flag);
            }
        }
        let (event_tx, event_rx) = bounded(EVENT_CAPACITY);
        let (result_tx, result_rx) = bounded(1);
        let request_id = request.request_id.clone();
        if let Err(error) = self.inner.command_tx.send(WorkerCommand::Generate {
            request,
            event_tx,
            result_tx,
            cancellations: flags,
            reasoning_forces: reasoning_flags,
        }) {
            cancel_request_in_registry(&self.inner.cancellations, &request_id);
            cleanup_request_in_registry(&self.inner.reasoning_forces, &request_id);
            return Err(NativeError::new(
                NativeErrorCode::WorkerStopped,
                format!("native model worker is not accepting requests: {error}"),
            ));
        }
        Ok(GenerationTicket {
            request_id,
            events: event_rx,
            result: result_rx,
            cancellations: Arc::clone(&self.inner.cancellations),
            reasoning_forces: Arc::clone(&self.inner.reasoning_forces),
        })
    }

    pub fn cancel(&self, request_id: &str, branch_id: Option<&str>) -> usize {
        match branch_id {
            Some(branch_id) => usize::from(cancel_in_registry(
                &self.inner.cancellations,
                request_id,
                branch_id,
            )),
            None => cancel_request_in_registry(&self.inner.cancellations, request_id),
        }
    }

    pub fn skip_reasoning(&self, request_id: &str, branch_id: Option<&str>) -> usize {
        match branch_id {
            Some(branch_id) => usize::from(set_flag_in_registry(
                &self.inner.reasoning_forces,
                request_id,
                branch_id,
            )),
            None => set_request_flags_in_registry(&self.inner.reasoning_forces, request_id),
        }
    }

    pub fn snapshot_sequence(&self, sequence_id: i32) -> NativeResult<SequenceStateBlob> {
        let (response_tx, response_rx) = bounded(1);
        self.inner
            .command_tx
            .send(WorkerCommand::Snapshot {
                sequence_id,
                response: response_tx,
            })
            .map_err(|error| {
                NativeError::new(
                    NativeErrorCode::WorkerStopped,
                    format!("native model worker is unavailable: {error}"),
                )
            })?;
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
        let (response_tx, response_rx) = bounded(1);
        self.inner
            .command_tx
            .send(WorkerCommand::Restore {
                state,
                destination_sequence_id,
                response: response_tx,
            })
            .map_err(|error| {
                NativeError::new(
                    NativeErrorCode::WorkerStopped,
                    format!("native model worker is unavailable: {error}"),
                )
            })?;
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
        validate_batch_request(&request, &self.status())?;
        if request.branches.len() < 2 {
            return Err(NativeError::new(
                NativeErrorCode::InvalidConfig,
                "prefix prefill requires two probe branches",
            ));
        }
        let (response_tx, response_rx) = bounded(1);
        self.inner
            .command_tx
            .send(WorkerCommand::PrefillPrefix {
                request,
                response: response_tx,
            })
            .map_err(|error| {
                NativeError::new(
                    NativeErrorCode::WorkerStopped,
                    format!("native model worker is unavailable: {error}"),
                )
            })?;
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
        let (response_tx, response_rx) = bounded(1);
        self.inner
            .command_tx
            .send(WorkerCommand::Tokenize {
                messages,
                chat_template,
                response: response_tx,
            })
            .map_err(|error| {
                NativeError::new(
                    NativeErrorCode::WorkerStopped,
                    format!("native model worker is not accepting tokenization: {error}"),
                )
            })?;
        response_rx.recv().map_err(|error| {
            NativeError::new(
                NativeErrorCode::WorkerStopped,
                format!("native model worker stopped during tokenization: {error}"),
            )
        })?
    }

    pub fn prepare_input(&self, input: GenerationInput) -> NativeResult<Vec<PreparedPrompt>> {
        let (response_tx, response_rx) = bounded(1);
        self.inner
            .command_tx
            .send(WorkerCommand::PrepareInput {
                input,
                response: response_tx,
            })
            .map_err(|error| {
                NativeError::new(
                    NativeErrorCode::WorkerStopped,
                    format!("native model worker is not accepting prompt preparation: {error}"),
                )
            })?;
        response_rx.recv().map_err(|error| {
            NativeError::new(
                NativeErrorCode::WorkerStopped,
                format!("native model worker stopped during prompt preparation: {error}"),
            )
        })?
    }
}

fn run_worker(
    config: NativeModelConfig,
    command_rx: Receiver<WorkerCommand>,
    ready_tx: Sender<NativeResult<()>>,
    status: Arc<RwLock<ResidentModelStatus>>,
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
    let model = match LlamaModel::load_from_file(backend, &config.model_path, &model_params) {
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
    let multimodal = match config.mmproj_path.as_ref() {
        Some(path) => {
            let Some(path) = path.to_str() else {
                let error = NativeError::new(
                    NativeErrorCode::InvalidConfig,
                    "multimodal projector path is not valid UTF-8",
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
            match MtmdContext::init_from_file(path, &model, &params) {
                Ok(context) => Some(context),
                Err(error) => {
                    let error = NativeError::new(
                        NativeErrorCode::ModelLoadFailed,
                        format!("failed to load multimodal projector {path}: {error}"),
                    );
                    set_status_state(&status, ModelRuntimeState::Failed, 0);
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            }
        }
        None => None,
    };
    let context_tokens = config.context_tokens.min(model.n_ctx_train()).max(512);
    let context_params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(context_tokens))
        .with_n_batch(config.batch_tokens)
        .with_n_ubatch(config.batch_tokens.min(512))
        .with_n_seq_max(config.max_sequences)
        .with_n_threads(native_thread_count())
        .with_n_threads_batch(native_thread_count())
        .with_kv_unified(true)
        .with_no_perf(false);
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
        backend,
        &model,
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
        current.fingerprint = Some(fingerprint);
    }
    let _ = ready_tx.send(Ok(()));
    let mut sequence_token_counts = HashMap::<i32, usize>::new();
    let mut sequence_token_ids = HashMap::<i32, Vec<i32>>::new();
    while let Ok(command) = command_rx.recv() {
        match command {
            WorkerCommand::GenerateBatch {
                request,
                event_tx,
                result_tx,
                cancellations,
                reasoning_forces,
            } => {
                set_status_state(&status, ModelRuntimeState::Ready, request.cases.len());
                let result = prepare_generation_batch(&model, &request).and_then(
                    |(normalized, token_sets)| {
                        generate_batch(
                            &model,
                            &mut context,
                            &normalized,
                            Some(token_sets),
                            BatchSupervision {
                                event_tx: &event_tx,
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
                if result.is_err() {
                    emit_failed_case_events(&event_tx, &request);
                }
                set_status_state(&status, ModelRuntimeState::Ready, 0);
                let _ = result_tx.send(result);
            }
            WorkerCommand::Generate {
                request,
                event_tx,
                result_tx,
                cancellations,
                reasoning_forces,
            } => {
                set_status_state(&status, ModelRuntimeState::Ready, request.branches.len());
                let result = generate_batch(
                    &model,
                    &mut context,
                    &request,
                    None,
                    BatchSupervision {
                        event_tx: &event_tx,
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
                let _ = result_tx.send(result);
            }
            WorkerCommand::GenerateMultimodal {
                request,
                event_tx,
                result_tx,
                cancellation,
                reasoning_force,
            } => {
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
                let _ = result_tx.send(result.map(|output| vec![output]));
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
            WorkerCommand::Shutdown => break,
        }
    }
    set_status_state(&status, ModelRuntimeState::Stopped, 0);
}

fn generate_batch(
    model: &LlamaModel,
    context: &mut LlamaContext<'_>,
    request: &SharedPrefixBatchRequest,
    prepared_token_sets: Option<Vec<Vec<LlamaToken>>>,
    supervision: BatchSupervision<'_>,
    tracking: SequenceTracking<'_>,
) -> NativeResult<Vec<GenerationOutput>> {
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
        .sum::<usize>();
    let required_tokens = cached_cells
        + shared_uncached_prefix
        + token_sets
            .iter()
            .enumerate()
            .map(|(index, tokens)| {
                tokens.len().saturating_sub(prefix_lengths[index])
                    + request.branches[index].sampling.max_tokens as usize
            })
            .sum::<usize>();
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
        emit_state(
            supervision.event_tx,
            request,
            branch,
            index,
            0,
            GenerationState::Prefilling,
        );
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
        emit_state(
            supervision.event_tx,
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
            if supervision.reasoning_forces[index].load(Ordering::Acquire)
                && branch.forced_tokens.is_empty()
                && let Some(end_marker) = active_reasoning_end_marker(&branch.text)
            {
                let tokens = model
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
            let token = if let Some(token) = branch.forced_tokens.pop_front() {
                branch.sampler.accept(token);
                token
            } else {
                branch.sampler.sample(context, branch.logit_index)
            };
            if model.is_eog_token(token) {
                branch.state = GenerationState::Completed;
                branch.finish_reason = "end_of_generation".to_string();
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
            let mut piece = String::with_capacity(bytes.len());
            let _ = branch
                .decoder
                .decode_to_string(bytes.as_slice(), &mut piece, false);
            if branch.first_token_ms.is_none() {
                branch.first_token_ms = Some(started.elapsed().as_millis());
            }
            branch.text.push_str(&piece);
            branch.generated += 1;
            if !piece.is_empty() {
                try_emit_nonterminal(
                    supervision.event_tx,
                    GenerationEvent {
                        request_id: request.request_id.clone(),
                        branch_id: branch.request.branch_id.clone(),
                        sequence_id: branch.sequence_id,
                        input_index: index,
                        event_index: branch.event_index,
                        event: GenerationEventKind::Delta { text: piece },
                    },
                );
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
        if matches!(
            branch.state,
            GenerationState::Completed | GenerationState::Cancelled
        ) {
            emit_state(
                supervision.event_tx,
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
    let outputs = branches
        .into_iter()
        .map(|branch| {
            let completion_tokens = branch.generated;
            let tokens_per_second = if duration_ms == 0 {
                0.0
            } else {
                completion_tokens as f64 / (duration_ms as f64 / 1000.0)
            };
            GenerationOutput {
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
            }
        })
        .collect();
    Ok(outputs)
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
            emit_generation_state(
                supervision.event_tx,
                request,
                event_index,
                GenerationState::Cancelled,
            );
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
        let mut piece = String::with_capacity(bytes.len());
        let _ = decoder.decode_to_string(bytes.as_slice(), &mut piece, false);
        if first_token_ms.is_none() {
            first_token_ms = Some(started.elapsed().as_millis());
        }
        text.push_str(&piece);
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
    let state = if finish_reason == "cancelled" {
        GenerationState::Cancelled
    } else {
        emit_generation_state(
            supervision.event_tx,
            request,
            event_index,
            GenerationState::Completed,
        );
        GenerationState::Completed
    };
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
    cancellations: &'a [Arc<AtomicBool>],
    reasoning_forces: &'a [Arc<AtomicBool>],
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
    if config
        .model_path
        .extension()
        .and_then(|value| value.to_str())
        != Some("gguf")
    {
        return Err(NativeError::new(
            NativeErrorCode::ModelInvalid,
            "native models must use the .gguf format",
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
    _backend: &LlamaBackend,
    model: &LlamaModel,
    execution_backend: &str,
    context_tokens: u32,
    rope_config_sha256: String,
    kv_layout_sha256: String,
) -> NativeResult<ModelFingerprint> {
    let mut file = File::open(&config.model_path).map_err(|error| {
        NativeError::new(
            NativeErrorCode::ModelMissing,
            format!("failed to open model for fingerprinting: {error}"),
        )
    })?;
    let metadata = file.metadata().map_err(|error| {
        NativeError::new(
            NativeErrorCode::ModelInvalid,
            format!("failed to inspect model: {error}"),
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            NativeError::new(
                NativeErrorCode::ModelInvalid,
                format!("failed while hashing model: {error}"),
            )
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let model_sha256 = format!("{:x}", hasher.finalize());
    let chat_template = model
        .chat_template(None)
        .ok()
        .and_then(|template| template.to_string().ok())
        .unwrap_or_default();
    let chat_template_sha256 = format!("{:x}", Sha256::digest(chat_template.as_bytes()));
    let multimodal_projector_sha256 = config.mmproj_path.as_deref().map(hash_file).transpose()?;
    Ok(ModelFingerprint {
        model_id: config.model_id.clone(),
        model_path: config.model_path.clone(),
        model_size: metadata.len(),
        model_sha256: model_sha256.clone(),
        // The tokenizer is embedded in GGUF. Using the complete GGUF hash is
        // conservative: any tokenizer or model tensor change invalidates state.
        tokenizer_sha256: model_sha256,
        chat_template_sha256,
        multimodal_projector_sha256,
        binding_version: LLAMA_CPP_BINDING_VERSION.to_string(),
        build_id: format!("llama-cpp-2-{LLAMA_CPP_BINDING_VERSION}-{execution_backend}"),
        backend: execution_backend.to_string(),
        context_tokens,
        batch_tokens: config.batch_tokens,
        max_sequences: config.max_sequences,
        rope_config_sha256,
        kv_layout_sha256,
    })
}

fn describe_model(
    config: &NativeModelConfig,
    model: &LlamaModel,
    fingerprint: &ModelFingerprint,
    media_kinds: Vec<MediaKind>,
) -> NativeModelDescriptor {
    let display_name = model
        .meta_val_str("general.name")
        .ok()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| config.model_id.clone());
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

fn inspected_capabilities(
    chat_template_available: bool,
    max_sequences: u32,
    media_kinds: &[MediaKind],
) -> ExactModelCapabilities {
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
        evidence: NativeEvidenceCapabilities::default(),
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

fn hash_file(path: &std::path::Path) -> NativeResult<String> {
    let mut file = File::open(path).map_err(|error| {
        NativeError::new(
            NativeErrorCode::ModelMissing,
            format!(
                "failed to open {} for fingerprinting: {error}",
                path.display()
            ),
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            NativeError::new(
                NativeErrorCode::ModelInvalid,
                format!("failed while hashing {}: {error}", path.display()),
            )
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn emit_state(
    event_tx: &Sender<GenerationEvent>,
    request: &SharedPrefixBatchRequest,
    branch: &BranchRequest,
    sequence_id: usize,
    event_index: u64,
    state: GenerationState,
) {
    let event = GenerationEvent {
        request_id: request.request_id.clone(),
        branch_id: branch.branch_id.clone(),
        sequence_id: sequence_id as i32,
        input_index: sequence_id,
        event_index,
        event: GenerationEventKind::State { state },
    };
    if is_terminal_state(state) {
        try_emit_terminal(event_tx, event);
    } else {
        try_emit_nonterminal(event_tx, event);
    }
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

fn cancel_in_registry(registry: &CancelRegistry, request_id: &str, branch_id: &str) -> bool {
    registry
        .lock()
        .ok()
        .and_then(|entries| {
            entries
                .get(&(request_id.to_string(), branch_id.to_string()))
                .cloned()
        })
        .map(|flag| {
            flag.store(true, Ordering::Release);
            true
        })
        .unwrap_or(false)
}

fn cancel_request_in_registry(registry: &CancelRegistry, request_id: &str) -> usize {
    registry
        .lock()
        .map(|entries| {
            entries
                .iter()
                .filter(|((candidate, _), _)| candidate == request_id)
                .map(|(_, flag)| {
                    flag.store(true, Ordering::Release);
                    1_usize
                })
                .sum()
        })
        .unwrap_or_default()
}

fn set_flag_in_registry(
    registry: &ReasoningForceRegistry,
    request_id: &str,
    branch_id: &str,
) -> bool {
    cancel_in_registry(registry, request_id, branch_id)
}

fn set_request_flags_in_registry(registry: &ReasoningForceRegistry, request_id: &str) -> usize {
    cancel_request_in_registry(registry, request_id)
}

fn cleanup_request_in_registry(registry: &CancelRegistry, request_id: &str) {
    if let Ok(mut entries) = registry.lock() {
        entries.retain(|(candidate, _), _| candidate != request_id);
    }
}

fn native_decode_error(context: &str, error: impl std::fmt::Display) -> NativeError {
    NativeError::new(NativeErrorCode::DecodeFailed, format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reported_binding_version_matches_the_exact_manifest_pin() {
        let manifest = include_str!("../Cargo.toml");
        assert!(manifest.contains(&format!(
            "llama-cpp-2 = {{ version = \"={LLAMA_CPP_BINDING_VERSION}\""
        )));
        assert!(manifest.contains(&format!(
            "llama-cpp-sys-2 = {{ version = \"={LLAMA_CPP_BINDING_VERSION}\""
        )));
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
        let error = NativeModelHandle::load(NativeModelConfig::local(PathBuf::from(
            "/missing/model.gguf",
        )))
        .expect_err("missing models must fail");
        assert_eq!(error.code, NativeErrorCode::ModelMissing);
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
        let registry: ReasoningForceRegistry = Arc::new(Mutex::new(HashMap::new()));
        let first = Arc::new(AtomicBool::new(false));
        let second = Arc::new(AtomicBool::new(false));
        registry.lock().expect("lock registry").insert(
            ("request".to_string(), "first".to_string()),
            Arc::clone(&first),
        );
        registry.lock().expect("lock registry").insert(
            ("request".to_string(), "second".to_string()),
            Arc::clone(&second),
        );
        assert!(set_flag_in_registry(&registry, "request", "first"));
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
        let model_path = std::env::var("MOM_LLAMA_MODEL_PATH")?;
        let mut config = NativeModelConfig::local(PathBuf::from(model_path));
        // CI and sandboxed smoke tests cannot assume access to a Metal command queue.
        config.device = NativeDevice::Cpu;
        let handle = NativeModelHandle::load(config)?;
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
        let model_path = std::env::var("MOM_LLAMA_MODEL_PATH")?;
        let mut config = NativeModelConfig::local(PathBuf::from(model_path));
        config.device = NativeDevice::Cpu;
        config.max_sequences = 2;
        let handle = NativeModelHandle::load(config)?;
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
        let outputs = ticket.wait_timeout(Duration::from_secs(120))?;
        let events = ticket.events.try_iter().collect::<Vec<_>>();
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

        let family_ticket = handle.generate_batch(GenerationBatchRequest {
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
        })?;
        let family_outputs = family_ticket.wait_timeout(Duration::from_secs(120))?;
        let family_events = family_ticket.events.try_iter().collect::<Vec<_>>();
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
        let cancelled_outputs = cancel_ticket.wait_timeout(Duration::from_secs(120))?;
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
}
