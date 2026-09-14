//! A peer can spend an explicit model grant, never the active manuscript's
//! authority. Local model work preempts this single, separately owned job.
use super::*;
use loom_cabal::compute::{ComputeExecutor, ComputeFailure, ComputeModel, HostComputeJob};
use std::{future::Future, pin::Pin};
use tokio_util::sync::CancellationToken;

const IDLE_DELAY: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub(super) struct IdleComputeOwner {
    state: Mutex<IdleState>,
    drained: Condvar,
    delay: Duration,
}

#[derive(Debug)]
struct IdleState {
    closed: bool,
    foreground: usize,
    idle_since: Instant,
    active: Option<CancellationToken>,
}

impl Default for IdleComputeOwner {
    fn default() -> Self {
        Self::new(IDLE_DELAY)
    }
}

impl IdleComputeOwner {
    fn new(delay: Duration) -> Self {
        Self {
            state: Mutex::new(IdleState {
                closed: false,
                foreground: 0,
                idle_since: Instant::now(),
                active: None,
            }),
            drained: Condvar::new(),
            delay,
        }
    }

    pub fn idle(&self) -> bool {
        self.state.lock().is_ok_and(|state| {
            !state.closed
                && state.foreground == 0
                && state.active.is_none()
                && state.idle_since.elapsed() >= self.delay
        })
    }

    fn reserve(self: &Arc<Self>, cancel: &CancellationToken) -> Result<IdleJob, ComputeFailure> {
        let mut state = self.state.lock().map_err(|_| ComputeFailure::HostBusy)?;
        if state.closed
            || state.foreground != 0
            || state.active.is_some()
            || state.idle_since.elapsed() < self.delay
            || cancel.is_cancelled()
        {
            return Err(ComputeFailure::HostBusy);
        }
        let cancel = cancel.child_token();
        state.active = Some(cancel.clone());
        Ok(IdleJob {
            owner: self.clone(),
            cancel,
        })
    }

    /// Reserve foreground priority before waiting. A new peer job cannot slip
    /// into the gap between the previous worker's release and model admission.
    pub fn foreground(self: &Arc<Self>) -> ForegroundModel {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.foreground += 1;
        if let Some(cancel) = &state.active {
            cancel.cancel();
        }
        while state.active.is_some() {
            state = self
                .drained
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        ForegroundModel(self.clone())
    }

    pub fn close_and_drain(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        if let Some(cancel) = &state.active {
            cancel.cancel();
        }
        while state.active.is_some() {
            state = self
                .drained
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

#[derive(Debug)]
pub(super) struct ForegroundModel(Arc<IdleComputeOwner>);

impl Drop for ForegroundModel {
    fn drop(&mut self) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.foreground -= 1;
        state.idle_since = Instant::now();
    }
}

#[derive(Debug)]
struct IdleJob {
    owner: Arc<IdleComputeOwner>,
    cancel: CancellationToken,
}

impl Drop for IdleJob {
    fn drop(&mut self) {
        self.owner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active = None;
        self.owner.drained.notify_all();
    }
}

/// Own only shared native state, with no AppHandle/PluginState reference cycle.
#[derive(Clone, Debug)]
pub(super) struct NativeExecutor {
    backend: Arc<LlamaBackend>,
    close_requested: Arc<AtomicBool>,
    application: Arc<Mutex<ApplicationPhase>>,
    model: Arc<Mutex<ModelRegistry>>,
    model_lifecycle: Arc<Mutex<()>>,
    generations: Arc<GenerationRegistry>,
    owner: Arc<IdleComputeOwner>,
    root: Option<PathBuf>,
}

impl NativeExecutor {
    pub fn from_state(state: &PluginState) -> Self {
        Self {
            backend: state.backend.clone(),
            close_requested: state.close_requested.clone(),
            application: state.application.clone(),
            model: state.model.clone(),
            model_lifecycle: state.model_lifecycle.clone(),
            generations: state.generations.clone(),
            owner: state.peer_compute.clone(),
            root: state
                .app_local_data_root
                .as_ref()
                .map(|root| root.join("peer-compute-writing")),
        }
    }

    fn selected(&self) -> Result<LoadedModel, ComputeFailure> {
        let model = self
            .model
            .try_lock()
            .map_err(|_| ComputeFailure::HostBusy)?;
        match &*model {
            ModelRegistry::Loaded(model)
                if model.descriptor.capabilities.completion_text.is_supported()
                    && model
                        .descriptor
                        .capabilities
                        .per_case_cancellation
                        .is_supported() =>
            {
                Ok((**model).clone())
            }
            _ => Err(ComputeFailure::ModelUnavailable),
        }
    }
}

impl ComputeExecutor for NativeExecutor {
    fn available(&self, model: &ComputeModel) -> bool {
        cfg!(unix)
            && self.root.is_some()
            && !self.close_requested.load(Ordering::Acquire)
            && self.owner.idle()
            && self
                .generations
                .active_local_branch_count()
                .is_ok_and(|count| count == 0)
            && self
                .selected()
                .and_then(|loaded| model_claim(&loaded))
                .is_ok_and(|claim| claim == *model)
    }

    fn execute(
        &self,
        job: HostComputeJob,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<String, ComputeFailure>> + Send>> {
        let executor = self.clone();
        Box::pin(async move {
            let lease = executor.owner.reserve(&cancel)?;
            // The protocol owner awaits this task even after cancellation. The
            // idle lease also keeps local model transitions waiting until every
            // native handle below has joined, including during unwinding.
            tokio::task::spawn_blocking(move || executor.run(&job, &lease))
                .await
                .unwrap_or(Err(ComputeFailure::WorkerPanicked))
        })
    }
}

pub(super) fn model_claim(model: &LoadedModel) -> Result<ComputeModel, ComputeFailure> {
    if !model.descriptor.capabilities.completion_text.is_supported()
        || !model
            .descriptor
            .capabilities
            .per_case_cancellation
            .is_supported()
    {
        return Err(ComputeFailure::ModelUnavailable);
    }
    let bytes = serde_json::to_vec(&("loom_peer_model_v1", &model.descriptor, &model.profile))
        .map_err(|_| ComputeFailure::ModelUnavailable)?;
    let mut name: String = model
        .descriptor
        .display_name
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    while name.len() > 160 {
        name.pop();
    }
    if name.is_empty() {
        name = "Local model".into();
    }
    Ok(ComputeModel {
        fingerprint: BlobId::digest(&bytes).to_string(),
        name,
    })
}

impl NativeExecutor {
    fn run(&self, job: &HostComputeJob, lease: &IdleJob) -> Result<String, ComputeFailure> {
        let root = self.root.as_ref().ok_or(ComputeFailure::ModelUnavailable)?;
        // Never block on application admission while foreground code is
        // draining this lease under that same admission guard.
        let admission = self
            .application
            .try_lock()
            .map_err(|_| ComputeFailure::HostBusy)?;
        if *admission != ApplicationPhase::Running
            || self.close_requested.load(Ordering::Acquire)
            || lease.cancel.is_cancelled()
            || self
                .generations
                .active_local_branch_count()
                .map_err(failed)?
                != 0
        {
            return Err(ComputeFailure::HostBusy);
        }
        let lifecycle = self
            .model_lifecycle
            .try_lock()
            .map_err(|_| ComputeFailure::HostBusy)?;
        let model = self.selected()?;
        if model_claim(&model)? != job.grant.model {
            return Err(ComputeFailure::ModelUnavailable);
        }
        let mut store = private_store(root)?;
        let prepared = prepare(&mut store, &model, job)?;
        if lease.cancel.is_cancelled() {
            return Err(ComputeFailure::HostBusy);
        }
        let owner = self
            .backend
            .start_exact_continuation(prepared.request)
            .map_err(|_| ComputeFailure::ExecutionFailed)?;
        drop(lifecycle);
        drop(admission);
        let control = owner.control();
        let result = loop {
            if lease.cancel.is_cancelled() {
                control.cancel_all();
            }
            match control.receive_result_timeout(Duration::from_millis(50)) {
                Err(LlamaBackendError::ResultTimeout) => (),
                result => break result,
            }
        };
        let joined = owner.shutdown_joined();
        if joined.worker_panicked() {
            return Err(ComputeFailure::WorkerPanicked);
        }
        let result = result.map_err(|_| ComputeFailure::ExecutionFailed)?;
        let candidate = result
            .candidates
            .first()
            .ok_or(ComputeFailure::ExecutionFailed)?;
        validate_candidate_receipt_binding(
            candidate,
            &prepared.request_id,
            prepared.prompt_blob,
            PromptMode::RawCompletion,
            &result.context_binding,
            &model.descriptor,
            0,
        )
        .map_err(|_| ComputeFailure::ExecutionFailed)?;
        let evidence = store
            .store_provenance_blob(&serde_json::to_vec(&result).map_err(failed)?)
            .map_err(failed)?;
        // Cancellation is checked after joining, before retaining a successful
        // output. A peer never receives prose from an interrupted local run.
        if lease.cancel.is_cancelled() {
            return Err(ComputeFailure::HostBusy);
        }
        if candidate.terminal.status != GenerationTerminalStatus::Completed {
            return Err(ComputeFailure::ExecutionFailed);
        }
        if candidate.output_text.len() > loom_cabal::compute::MAX_COMPUTE_TEXT_BYTES
            || candidate.token_trace.generated_token_ids.len()
                > job.input.max_output_tokens as usize
        {
            return Err(ComputeFailure::InvalidOutput);
        }
        store
            .create_generated_document_if_absent(
                format!("Results/{}/{}.md", job.peer, job.id),
                DocumentContent::Prose(candidate.output_text.clone()),
                "Peer completion",
                evidence,
            )
            .map_err(failed)?;
        Ok(candidate.output_text.clone())
    }
}

fn private_store(root: &Path) -> Result<ProjectStore, ComputeFailure> {
    if !cfg!(unix) {
        return Err(ComputeFailure::InputUnsupported);
    }
    if root
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(ComputeFailure::ExecutionFailed);
    }
    std::fs::create_dir_all(root).map_err(|_| ComputeFailure::ExecutionFailed)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| ComputeFailure::ExecutionFailed)?;
    }
    if root.join(".loom").exists() {
        ProjectStore::open(root)
    } else {
        ProjectStore::initialize(root, "Peer compute").map(|(store, _)| store)
    }
    .map_err(|_| ComputeFailure::ExecutionFailed)
}

struct Prepared {
    request: ExactContinuationRequest,
    request_id: String,
    prompt_blob: BlobId,
}

fn prepare(
    store: &mut ProjectStore,
    model: &LoadedModel,
    job: &HostComputeJob,
) -> Result<Prepared, ComputeFailure> {
    let path = format!("Requests/{}/{}.md", job.peer, job.id);
    if store.document_path_is_reserved(&path).map_err(failed)? || store.root().join(&path).exists()
    {
        return Err(ComputeFailure::ExecutionFailed);
    }
    let request_bytes =
        serde_json::to_vec(&(&job.id, &job.peer, &job.grant, &job.input)).map_err(failed)?;
    let request_evidence = store
        .store_provenance_blob(&request_bytes)
        .map_err(failed)?;
    store
        .create_derived_document_if_absent(
            &path,
            DocumentContent::Prose(job.input.prompt.clone()),
            "Received peer prompt",
            request_evidence,
        )
        .map_err(failed)?;
    let source = store.read_document(path).map_err(failed)?;
    let environment = model_environment_from_verified(&model.descriptor).map_err(failed)?;
    let environment = store
        .record_model_environment(&environment)
        .map_err(failed)?;
    let prompt_blob = store
        .store_provenance_blob(job.input.prompt.as_bytes())
        .map_err(failed)?;
    let recipe = PromptRecipe {
        mode: PromptMode::RawCompletion,
        exact_prompt_blob_id: prompt_blob,
        exact_prompt_token_ids: None,
        ordered_input_artifact_ids: vec![source.artifact_id],
        prompt_token_count: None,
    };
    let prompt = store.record_prompt_recipe(&recipe).map_err(failed)?;
    let context = store
        .record_context_recipe(&ContextRecipe {
            source_revision_id: source.revision_id,
            ordered_source_artifact_ids: vec![source.artifact_id],
            token_budget: u64::from(model.profile.context_tokens),
            retrieval_evidence_blob_id: Some(request_evidence),
        })
        .map_err(failed)?;
    let authority = store
        .record_authority_policy(&AuthorityPolicy {
            policy_version: 1,
            writer_environment_artifact_ids: vec![environment.artifact_id],
            critic_environment_artifact_ids: Vec::new(),
        })
        .map_err(failed)?;
    let mut sampling = sampling_for_weave_case(
        CommandId::new(),
        0,
        job.input.max_output_tokens,
        0.8,
        WeavePreset::ManualV2,
    );
    sampling.seed = job.input.seed;
    let generation = GenerationStart {
        run_id: GenerationRunId::new(),
        branch_id: BranchId::new(),
        document_id: source.document_id,
        source_revision_id: source.revision_id,
        target_range: ByteRange::new(0, 0).expect("empty range"),
        model_environment_artifact_id: environment.artifact_id,
        prompt_recipe_artifact_id: prompt.artifact_id,
        context_recipe_artifact_id: context.artifact_id,
        authority_policy_artifact_id: authority.artifact_id,
        seed: u64::from(sampling.seed),
        sampling: serde_json::Value::Null,
    };
    let case = ContinuationCase::bind_sampling(generation, sampling).map_err(failed)?;
    let request_id = format!(
        "peer-{}",
        BlobId::digest(&serde_json::to_vec(&(job.peer, job.id)).map_err(failed)?)
    );
    Ok(Prepared {
        request: ExactContinuationRequest {
            request_id: request_id.clone(),
            model: model.profile.clone(),
            exact_manuscript_prefix: job.input.prompt.clone(),
            context_preamble: String::new(),
            media: Vec::new(),
            prompt_recipe: recipe,
            cases: vec![case],
        },
        request_id,
        prompt_blob,
    })
}

fn failed(_error: impl std::fmt::Display) -> ComputeFailure {
    ComputeFailure::ExecutionFailed
}

#[cfg(test)]
#[path = "peer_compute_tests.rs"]
mod tests;
