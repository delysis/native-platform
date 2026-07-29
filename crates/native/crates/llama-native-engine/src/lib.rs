mod state_buffer;

use crossbeam_channel::{Receiver, Sender, bounded};
use encoding_rs::UTF_8;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::mtmd::{
    MtmdBitmap, MtmdContext, MtmdContextParams, MtmdInputText, mtmd_default_marker,
};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;
use llama_native_types::{
    BranchRequest, ChatMessage, ChatRole, GenerationEvent, GenerationEventKind, GenerationMetrics,
    GenerationOutput, GenerationRequest, GenerationState, MAX_PARALLEL_SEQUENCES, MediaInput,
    ModelFingerprint, ModelRuntimeState, NativeDevice, NativeError, NativeErrorCode,
    NativeModelConfig, NativeTransport, ResidentModelStatus, SamplerKind, SamplingConfig,
    SequenceStateBlob, SharedPrefixBatchRequest,
};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const LLAMA_CPP_BINDING_VERSION: &str = "0.1.150";
const COMMAND_CAPACITY: usize = 32;
const EVENT_CAPACITY: usize = 256;

static BACKEND: OnceLock<Result<LlamaBackend, String>> = OnceLock::new();

type NativeResult<T> = Result<T, NativeError>;
type CancelKey = (String, String);
type CancelRegistry = Arc<Mutex<HashMap<CancelKey, Arc<AtomicBool>>>>;

#[derive(Debug)]
pub struct GenerationTicket {
    pub request_id: String,
    pub events: Receiver<GenerationEvent>,
    result: Receiver<NativeResult<Vec<GenerationOutput>>>,
    cancellations: CancelRegistry,
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
        result
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
    Generate {
        request: SharedPrefixBatchRequest,
        event_tx: Sender<GenerationEvent>,
        result_tx: Sender<NativeResult<Vec<GenerationOutput>>>,
        cancellations: Vec<Arc<AtomicBool>>,
    },
    GenerateMultimodal {
        request: GenerationRequest,
        event_tx: Sender<GenerationEvent>,
        result_tx: Sender<NativeResult<Vec<GenerationOutput>>>,
        cancellation: Arc<AtomicBool>,
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
    Shutdown,
}

impl NativeModelHandle {
    pub fn load(config: NativeModelConfig) -> NativeResult<Self> {
        validate_config(&config)?;
        let (command_tx, command_rx) = bounded(COMMAND_CAPACITY);
        let (ready_tx, ready_rx) = bounded(1);
        let cancellations = Arc::new(Mutex::new(HashMap::new()));
        let status = Arc::new(RwLock::new(ResidentModelStatus {
            model_id: config.model_id.clone(),
            state: ModelRuntimeState::Loading,
            fingerprint: None,
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
                active_sequences: 0,
                max_sequences: 0,
            })
    }

    pub fn generate(&self, request: GenerationRequest) -> NativeResult<GenerationTicket> {
        if !request.media.is_empty() {
            return self.generate_multimodal(request);
        }
        let batch = SharedPrefixBatchRequest {
            request_id: request.request_id,
            model_id: request.model_id,
            common_messages: request.messages,
            branches: vec![BranchRequest {
                branch_id: "assistant".to_string(),
                label: "Assistant".to_string(),
                instruction: String::new(),
                sampling: request.sampling,
            }],
            cached_prefix: request.cached_prefix,
        };
        self.generate_shared_prefix(batch)
    }

    fn generate_multimodal(&self, request: GenerationRequest) -> NativeResult<GenerationTicket> {
        validate_generation_request(&request, &self.status())?;
        let cancellation = Arc::new(AtomicBool::new(false));
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
            })
        {
            cleanup_request_in_registry(&self.inner.cancellations, &request_id);
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
        })
    }

    pub fn generate_shared_prefix(
        &self,
        request: SharedPrefixBatchRequest,
    ) -> NativeResult<GenerationTicket> {
        validate_batch_request(&request, &self.status())?;
        let mut flags = Vec::with_capacity(request.branches.len());
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
        let (event_tx, event_rx) = bounded(EVENT_CAPACITY);
        let (result_tx, result_rx) = bounded(1);
        let request_id = request.request_id.clone();
        if let Err(error) = self.inner.command_tx.send(WorkerCommand::Generate {
            request,
            event_tx,
            result_tx,
            cancellations: flags,
        }) {
            cancel_request_in_registry(&self.inner.cancellations, &request_id);
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
    let model_params = LlamaModelParams::default().with_n_gpu_layers(gpu_layers);
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
            match MtmdContext::init_from_file(path, &model, &MtmdContextParams::default()) {
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
        .with_kv_unified(true)
        .with_no_perf(false);
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
    let fingerprint = match fingerprint_model(&config, backend, &model) {
        Ok(fingerprint) => fingerprint,
        Err(error) => {
            set_status_state(&status, ModelRuntimeState::Failed, 0);
            let _ = ready_tx.send(Err(error));
            return;
        }
    };
    if let Ok(mut current) = status.write() {
        current.state = ModelRuntimeState::Ready;
        current.fingerprint = Some(fingerprint);
    }
    let _ = ready_tx.send(Ok(()));
    let mut sequence_token_counts = HashMap::<i32, usize>::new();
    let mut sequence_token_ids = HashMap::<i32, Vec<i32>>::new();
    while let Ok(command) = command_rx.recv() {
        match command {
            WorkerCommand::Generate {
                request,
                event_tx,
                result_tx,
                cancellations,
            } => {
                set_status_state(&status, ModelRuntimeState::Ready, request.branches.len());
                let result = generate_batch(
                    &model,
                    &mut context,
                    &request,
                    &event_tx,
                    &cancellations,
                    &mut sequence_token_counts,
                    &mut sequence_token_ids,
                );
                set_status_state(&status, ModelRuntimeState::Ready, 0);
                let _ = result_tx.send(result);
            }
            WorkerCommand::GenerateMultimodal {
                request,
                event_tx,
                result_tx,
                cancellation,
            } => {
                set_status_state(&status, ModelRuntimeState::Ready, 1);
                let result = generate_multimodal(
                    &model,
                    &mut context,
                    multimodal.as_ref(),
                    &request,
                    &event_tx,
                    &cancellation,
                    MultimodalSequenceTracking {
                        token_counts: &mut sequence_token_counts,
                        token_ids: &mut sequence_token_ids,
                    },
                );
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
            WorkerCommand::Shutdown => break,
        }
    }
    set_status_state(&status, ModelRuntimeState::Stopped, 0);
}

fn generate_batch(
    model: &LlamaModel,
    context: &mut LlamaContext<'_>,
    request: &SharedPrefixBatchRequest,
    event_tx: &Sender<GenerationEvent>,
    cancellations: &[Arc<AtomicBool>],
    sequence_token_counts: &mut HashMap<i32, usize>,
    sequence_token_ids: &mut HashMap<i32, Vec<i32>>,
) -> NativeResult<Vec<GenerationOutput>> {
    context.clear_kv_cache();
    sequence_token_counts.clear();
    sequence_token_ids.clear();
    let started = Instant::now();
    let prompts = render_branch_prompts(model, request)?;
    let token_sets = prompts
        .iter()
        .map(|prompt| {
            model.str_to_token(prompt, AddBos::Always).map_err(|error| {
                NativeError::new(
                    NativeErrorCode::ModelInvalid,
                    format!("failed to tokenize prompt: {error}"),
                )
            })
        })
        .collect::<NativeResult<Vec<_>>>()?;
    let minimum_tokens = token_sets.iter().map(Vec::len).min().unwrap_or_default();
    if minimum_tokens == 0 {
        return Err(NativeError::new(
            NativeErrorCode::ModelInvalid,
            "the model chat template produced an empty prompt",
        ));
    }
    let mut restored_prefix = false;
    let shared_prefix = if let Some(state) = request.cached_prefix.as_ref() {
        if request.branches.len() != 1
            || state.token_count == 0
            || state.token_count != state.token_ids.len()
            || state.token_count >= token_sets[0].len()
            || !token_sets[0]
                .iter()
                .take(state.token_count)
                .map(|token| token.0)
                .eq(state.token_ids.iter().copied())
        {
            return Err(NativeError::new(
                NativeErrorCode::CacheIncompatible,
                "saved KV prefix does not match the rendered native prompt",
            ));
        }
        state_buffer::import_sequence(context, state, 0)?;
        restored_prefix = true;
        state.token_count
    } else if request.branches.len() > 1 {
        longest_common_prefix(&token_sets).min(minimum_tokens.saturating_sub(1))
    } else {
        0
    };
    let required_tokens = shared_prefix
        + token_sets
            .iter()
            .enumerate()
            .map(|(index, tokens)| {
                tokens.len().saturating_sub(shared_prefix)
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
            event_tx,
            request,
            branch,
            index,
            0,
            GenerationState::Prefilling,
        );
    }
    if shared_prefix > 0 && !restored_prefix {
        decode_tokens_chunked(context, &token_sets[0][..shared_prefix], 0, 0, false)?;
        for sequence_id in 1..request.branches.len() {
            context
                .copy_kv_cache_seq(0, sequence_id as i32, Some(0), Some(shared_prefix as u32))
                .map_err(|error| {
                    NativeError::new(
                        NativeErrorCode::DecodeFailed,
                        format!("failed to copy shared KV prefix: {error}"),
                    )
                })?;
        }
    }
    for (sequence_id, tokens) in token_sets.iter().enumerate() {
        let suffix = &tokens[shared_prefix..tokens.len() - 1];
        if !suffix.is_empty() {
            decode_tokens_chunked(
                context,
                suffix,
                sequence_id as i32,
                shared_prefix as i32,
                false,
            )?;
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
        sequence_token_counts.insert(sequence_id as i32, tokens.len());
        sequence_token_ids.insert(
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
                generated: 0,
                next_position: token_sets[index].len() as i32,
                logit_index: index as i32,
                state: GenerationState::Generating,
                finish_reason: String::new(),
                event_index: 1,
                first_token_ms: None,
            }
        })
        .collect::<Vec<_>>();
    for branch in &mut branches {
        emit_state(
            event_tx,
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
            if cancellations[index].load(Ordering::Acquire) {
                branch.state = GenerationState::Cancelled;
                branch.finish_reason = "cancelled".to_string();
                let _ = context.clear_kv_cache_seq(Some(index as u32), None, None);
                emit_state(
                    event_tx,
                    request,
                    branch.request,
                    index,
                    branch.event_index,
                    GenerationState::Cancelled,
                );
                branch.event_index += 1;
                continue;
            }
            let token = branch.sampler.sample(context, branch.logit_index);
            if model.is_eog_token(token) {
                branch.state = GenerationState::Completed;
                branch.finish_reason = "end_of_generation".to_string();
                continue;
            }
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
                let _ = event_tx.try_send(GenerationEvent {
                    request_id: request.request_id.clone(),
                    branch_id: branch.request.branch_id.clone(),
                    sequence_id: branch.sequence_id,
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
            sequence_token_counts.insert(branch.sequence_id, branch.next_position as usize);
            sequence_token_ids
                .entry(branch.sequence_id)
                .or_default()
                .push(token.0);
        }
        context
            .decode(&mut batch)
            .map_err(|error| native_decode_error("failed to decode generation batch", error))?;
    }
    for branch in &mut branches {
        if branch.state == GenerationState::Completed {
            emit_state(
                event_tx,
                request,
                branch.request,
                branch.sequence_id as usize,
                branch.event_index,
                GenerationState::Completed,
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
                model_id: request.model_id.clone(),
                text: branch.text,
                state: branch.state,
                finish_reason: branch.finish_reason,
                metrics: GenerationMetrics {
                    prompt_tokens: token_sets[branch.sequence_id as usize].len(),
                    completion_tokens,
                    shared_prefix_tokens: shared_prefix,
                    duration_ms,
                    first_token_ms: branch.first_token_ms,
                    tokens_per_second,
                },
                real_engine_invoked: true,
                fake_fixture: false,
                transport: NativeTransport::InProcess,
            }
        })
        .collect();
    Ok(outputs)
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
    let prompts = render_branch_prompts(model, request)?;
    let token_sets = prompts
        .iter()
        .map(|prompt| {
            model.str_to_token(prompt, AddBos::Always).map_err(|error| {
                NativeError::new(
                    NativeErrorCode::ModelInvalid,
                    format!("failed to tokenize cache probe: {error}"),
                )
            })
        })
        .collect::<NativeResult<Vec<_>>>()?;
    let minimum = token_sets.iter().map(Vec::len).min().unwrap_or_default();
    let prefix_tokens = longest_common_prefix(&token_sets).min(minimum.saturating_sub(1));
    if prefix_tokens == 0 {
        return Err(NativeError::new(
            NativeErrorCode::CacheIncompatible,
            "cache probes produced no reusable rendered prompt prefix",
        ));
    }
    decode_tokens_chunked(context, &token_sets[0][..prefix_tokens], 0, 0, false)?;
    let token_ids = token_sets[0][..prefix_tokens]
        .iter()
        .map(|token| token.0)
        .collect::<Vec<_>>();
    sequence_token_counts.insert(0, prefix_tokens);
    sequence_token_ids.insert(0, token_ids.clone());
    state_buffer::export_sequence(context, 0, prefix_tokens, token_ids)
}

fn generate_multimodal(
    model: &LlamaModel,
    context: &mut LlamaContext<'_>,
    multimodal: Option<&MtmdContext>,
    request: &GenerationRequest,
    event_tx: &Sender<GenerationEvent>,
    cancellation: &Arc<AtomicBool>,
    tracking: MultimodalSequenceTracking<'_>,
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
    let bitmaps = request
        .media
        .iter()
        .map(|media| media_bitmap(multimodal, media))
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
    emit_generation_state(event_tx, request, 0, GenerationState::Prefilling);
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
    emit_generation_state(event_tx, request, 1, GenerationState::Generating);
    let mut sampler = build_sampler(model, &request.sampling);
    let mut decoder = UTF_8.new_decoder();
    let mut text = String::new();
    let mut generated = 0_usize;
    let mut event_index = 2_u64;
    let mut first_token_ms = None;
    let finish_reason = loop {
        if cancellation.load(Ordering::Acquire) {
            let _ = context.clear_kv_cache_seq(Some(0), None, None);
            emit_generation_state(event_tx, request, event_index, GenerationState::Cancelled);
            break "cancelled".to_string();
        }
        let token = sampler.sample(context, -1);
        if model.is_eog_token(token) {
            break "end_of_generation".to_string();
        }
        sampler.accept(token);
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
            let _ = event_tx.try_send(GenerationEvent {
                request_id: request.request_id.clone(),
                branch_id: "assistant".to_string(),
                sequence_id: 0,
                event_index,
                event: GenerationEventKind::Delta { text: piece },
            });
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
        emit_generation_state(event_tx, request, event_index, GenerationState::Completed);
        GenerationState::Completed
    };
    let duration_ms = started.elapsed().as_millis();
    Ok(GenerationOutput {
        request_id: request.request_id.clone(),
        branch_id: "assistant".to_string(),
        model_id: request.model_id.clone(),
        text,
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
        },
        real_engine_invoked: true,
        fake_fixture: false,
        transport: NativeTransport::InProcess,
    })
}

struct MultimodalSequenceTracking<'a> {
    token_counts: &'a mut HashMap<i32, usize>,
    token_ids: &'a mut HashMap<i32, Vec<i32>>,
}

fn media_bitmap(context: &MtmdContext, media: &MediaInput) -> NativeResult<MtmdBitmap> {
    let bitmap = MtmdBitmap::from_buffer(context, &media.bytes, false).map_err(|error| {
        NativeError::new(
            NativeErrorCode::ModelInvalid,
            format!("failed to decode media {}: {error}", media.id),
        )
    })?;
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
    let mut messages = request.messages.clone();
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
    render_messages_prompt(model, messages)
}

fn emit_generation_state(
    event_tx: &Sender<GenerationEvent>,
    request: &GenerationRequest,
    event_index: u64,
    state: GenerationState,
) {
    let _ = event_tx.try_send(GenerationEvent {
        request_id: request.request_id.clone(),
        branch_id: "assistant".to_string(),
        sequence_id: 0,
        event_index,
        event: GenerationEventKind::State { state },
    });
}

struct ActiveBranch<'a> {
    sequence_id: i32,
    request: &'a BranchRequest,
    sampler: LlamaSampler,
    decoder: encoding_rs::Decoder,
    text: String,
    generated: usize,
    next_position: i32,
    logit_index: i32,
    state: GenerationState,
    finish_reason: String,
    event_index: u64,
    first_token_ms: Option<u128>,
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
    let template = model.chat_template(None).map_err(|error| {
        NativeError::new(
            NativeErrorCode::ModelInvalid,
            format!("model has no usable chat template: {error}"),
        )
    })?;
    request
        .branches
        .iter()
        .map(|branch| {
            let mut messages = request.common_messages.clone();
            if !branch.instruction.trim().is_empty() {
                messages.push(ChatMessage {
                    role: ChatRole::User,
                    content: format!(
                        "For this response, use this reasoning perspective:\n{}",
                        branch.instruction.trim()
                    ),
                });
            }
            let native_messages = native_chat_messages(messages)?;
            model
                .apply_chat_template(&template, &native_messages, true)
                .map_err(|error| {
                    NativeError::new(
                        NativeErrorCode::ModelInvalid,
                        format!("failed to apply model chat template: {error}"),
                    )
                })
        })
        .collect()
}

fn render_messages_prompt(model: &LlamaModel, messages: Vec<ChatMessage>) -> NativeResult<String> {
    let template = model.chat_template(None).map_err(|error| {
        NativeError::new(
            NativeErrorCode::ModelInvalid,
            format!("model has no usable chat template: {error}"),
        )
    })?;
    let native_messages = native_chat_messages(messages)?;
    model
        .apply_chat_template(&template, &native_messages, true)
        .map_err(|error| {
            NativeError::new(
                NativeErrorCode::ModelInvalid,
                format!("failed to apply model chat template: {error}"),
            )
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
    if request.common_messages.is_empty() {
        return Err(NativeError::new(
            NativeErrorCode::InvalidConfig,
            "generation requires at least one chat message",
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
    if request.messages.is_empty() || request.sampling.max_tokens == 0 {
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

fn fingerprint_model(
    config: &NativeModelConfig,
    backend: &LlamaBackend,
    model: &LlamaModel,
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
    let backend_name = if backend.supports_gpu_offload() {
        "metal"
    } else {
        "cpu"
    };
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
        build_id: format!("llama-cpp-2-{LLAMA_CPP_BINDING_VERSION}-{backend_name}"),
        backend: backend_name.to_string(),
    })
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
    let _ = event_tx.try_send(GenerationEvent {
        request_id: request.request_id.clone(),
        branch_id: branch.branch_id.clone(),
        sequence_id: sequence_id as i32,
        event_index,
        event: GenerationEventKind::State { state },
    });
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
    fn native_parallelism_is_bounded() {
        let mut config = NativeModelConfig::local(PathBuf::from("model.gguf"));
        config.max_sequences = MAX_PARALLEL_SEQUENCES + 1;
        let error = validate_config(&config).expect_err("excess sequences must fail");
        assert_eq!(error.code, NativeErrorCode::InvalidConfig);
    }

    #[test]
    fn longest_prefix_is_token_exact() {
        let first = vec![LlamaToken::new(1), LlamaToken::new(2), LlamaToken::new(3)];
        let second = vec![LlamaToken::new(1), LlamaToken::new(2), LlamaToken::new(4)];
        assert_eq!(longest_common_prefix(&[first, second]), 2);
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
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: "Reply with the single word ready.".to_string(),
            }],
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
}
