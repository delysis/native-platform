//! A bounded evaluator over ordinary documents. The worker owns every native
//! call; references never execute, and results never overwrite source writing.

use super::*;
use loom_document::{
    NeuralCommand, NeuralExpression, document_references, parse_neural_command,
    render_base_function_prompt,
};
use std::fmt::Write as _;
use std::sync::atomic::AtomicUsize;

const MAX_CALLS: usize = 8;
const MAX_PROMPT_BYTES: usize = 65_536;
const MAX_HISTORY: usize = 64;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct TerminalRun {
    run_id: String,
    status: String,
    expression: String,
    title: String,
    output_document_id: Option<String>,
    output_relative_path: Option<String>,
    preview: String,
    error: Option<String>,
    created_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RunReceipt {
    run: TerminalRun,
    request_fingerprint: BlobId,
    source_document_id: DocumentId,
    source_revision_id: RevisionId,
    input_blob_id: BlobId,
    model: Option<VerifiedModelDescriptor>,
    bindings: BTreeMap<String, String>,
    sources: Vec<crate::document_bindings::ResolvedDocument>,
    steps: Vec<BlobId>,
}

#[derive(Debug, Default)]
pub(super) struct TerminalControl {
    cancelled: AtomicBool,
    current: Mutex<Option<Arc<LlamaGenerationControl>>>,
    joined: AtomicUsize,
    panicked: AtomicBool,
}

impl TerminalControl {
    pub(super) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(current) = self
            .current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            current.cancel_all();
        }
    }

    pub(super) fn joined_count(&self) -> usize {
        self.joined.load(Ordering::Acquire)
    }
    pub(super) fn panicked(&self) -> bool {
        self.panicked.load(Ordering::Acquire)
    }

    fn attach(&self, current: Arc<LlamaGenerationControl>) {
        let mut slot = self
            .current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.cancelled.load(Ordering::Acquire) {
            current.cancel_all();
        }
        *slot = Some(current);
    }
}

impl BranchCancellation for TerminalControl {
    fn cancel_branch(&self, _branch: BranchId) -> bool {
        self.cancel();
        true
    }
}

fn failure(message: impl Into<String>) -> IpcFailure {
    IpcFailure::new("terminal_failed", message, false)
}

fn io_failure(error: impl std::fmt::Display) -> IpcFailure {
    failure(error.to_string())
}

/// A caret selects its paragraph; explicit ranges retain their exact bytes.
fn input_range(text: &str, start: u64, end: u64) -> Result<&str, IpcFailure> {
    let (start, end) = (
        usize::try_from(start).map_err(io_failure)?,
        usize::try_from(end).map_err(io_failure)?,
    );
    if start > end
        || end > text.len()
        || !text.is_char_boundary(start)
        || !text.is_char_boundary(end)
    {
        return Err(failure("The selection changed. Select the passage again."));
    }
    if start != end {
        return Ok(&text[start..end]);
    }
    let left = ["\n\n", "\r\n\r\n"]
        .iter()
        .filter_map(|separator| {
            text[..start]
                .rfind(separator)
                .map(|at| at + separator.len())
        })
        .max()
        .unwrap_or(0);
    let right = ["\n\n", "\r\n\r\n"]
        .iter()
        .filter_map(|separator| text[start..].find(separator).map(|at| start + at))
        .min()
        .unwrap_or(text.len());
    Ok(&text[left..right])
}

fn expression_names(
    expression: &NeuralExpression,
    names: &mut BTreeSet<String>,
    calls: &mut usize,
) {
    match expression {
        NeuralExpression::Reference { name } => {
            names.insert(name.clone());
        }
        NeuralExpression::Literal { .. } => {}
        NeuralExpression::Call {
            function,
            arguments,
        } => {
            names.insert(function.clone());
            *calls += 1;
            for argument in arguments {
                expression_names(argument, names, calls);
            }
        }
    }
}

fn function_names(expression: &NeuralExpression, names: &mut BTreeSet<String>) {
    if let NeuralExpression::Call {
        function,
        arguments,
    } = expression
    {
        names.insert(function.clone());
        for argument in arguments {
            function_names(argument, names);
        }
    }
}

fn bounded(value: String) -> Result<String, IpcFailure> {
    if value.len() > MAX_PROMPT_BYTES {
        Err(failure(
            "This experiment exceeds the context budget. Reference a smaller passage.",
        ))
    } else {
        Ok(value)
    }
}

// Receipts are immutable, private sidecar evidence. Generated writing itself
// is always an ordinary registered Markdown document, never a receipt file.
fn receipt_directory(root: &Path) -> Result<PathBuf, IpcFailure> {
    crate::terminal_receipts::directory(root)
}

fn write_receipt(root: &Path, receipt: &RunReceipt, finished: bool) -> Result<(), IpcFailure> {
    crate::terminal_receipts::write(
        root,
        &receipt.run.run_id,
        finished,
        &serde_json::to_vec(receipt).map_err(io_failure)?,
    )
}

fn read_receipt(root: &Path, id: &str, finished: bool) -> Result<Option<RunReceipt>, IpcFailure> {
    let Some(bytes) = crate::terminal_receipts::read(root, id, finished)? else {
        return Ok(None);
    };
    let receipt: RunReceipt = serde_json::from_slice(&bytes).map_err(io_failure)?;
    if receipt.run.run_id != id {
        return Err(failure("Function receipt identity differs from its name"));
    }
    Ok(Some(receipt))
}

fn settle_interrupted(run: &mut TerminalRun, state: &PluginState) -> Result<(), IpcFailure> {
    if run.status == "running"
        && state
            .generation_lifecycle
            .current_lease(&format!("terminal-{}", run.run_id))
            .map_err(io_failure)?
            .is_none()
    {
        run.status = "failed".into();
        run.error = Some("Interrupted before completion; retained results are in Runs.".into());
    }
    Ok(())
}

#[tauri::command]
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) async fn terminal_run<R: Runtime>(
    project_id: String,
    session_id: String,
    command_id: String,
    document_id: String,
    source_revision_id: String,
    expected_visible_blob_id: String,
    source_start_byte: u64,
    source_end_byte: u64,
    expression: String,
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<TerminalRun, IpcFailure> {
    let command_id = parse_command_id(&command_id)?;
    let fingerprint = BlobId::digest(
        &serde_json::to_vec(&(
            &project_id,
            &document_id,
            &source_revision_id,
            &expected_visible_blob_id,
            source_start_byte,
            source_end_byte,
            &expression,
        ))
        .map_err(io_failure)?,
    );
    let admission = lock_application_admission(&state, "an experiment")?;
    let model_guard = lock_model_lifecycle(&state)?;
    let mut session = lock_session(&state)?;
    session
        .agency
        .admit_manual_generation()
        .map_err(io_failure)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    let root = store.root().to_owned();
    if let Some(receipt) = read_receipt(&root, &command_id.to_string(), false)? {
        if receipt.request_fingerprint != fingerprint {
            return Err(failure(
                "This run identifier belongs to a different experiment",
            ));
        }
        let mut result = read_receipt(&root, &command_id.to_string(), true)?.unwrap_or(receipt);
        settle_interrupted(&mut result.run, &state)?;
        return Ok(result.run);
    }
    let document_id = document_id.parse::<DocumentId>().map_err(io_failure)?;
    let summary = store
        .registered_document(document_id)
        .map_err(IpcFailure::store)?
        .ok_or_else(|| failure("The source document is no longer available"))?;
    let source = store
        .read_document(&summary.relative_path)
        .map_err(IpcFailure::store)?;
    if source.revision_id.to_string() != source_revision_id
        || source.blob_id.to_string() != expected_visible_blob_id
    {
        return Err(failure(
            "The source changed before the experiment began. Try again.",
        ));
    }
    let input = input_range(&source.text, source_start_byte, source_end_byte)?.to_owned();
    let entry = if expression.is_empty() {
        input.clone()
    } else {
        expression.clone()
    };
    if entry.trim().is_empty() {
        return Err(failure("Write or select an idea to try."));
    }
    let command = parse_neural_command(&entry).map_err(io_failure)?;
    let mut names = BTreeSet::new();
    let mut calls = 0;
    match &command {
        NeuralCommand::Prompt(text) => {
            for reference in document_references(text).map_err(io_failure)? {
                names.insert(reference.name);
            }
            calls = 1;
        }
        NeuralCommand::Expression(expression) => {
            expression_names(expression, &mut names, &mut calls);
        }
    }
    if calls > MAX_CALLS {
        return Err(failure(
            "An experiment can contain at most eight model calls.",
        ));
    }
    let model = if calls == 0 {
        None
    } else {
        Some(loaded_model(&state)?)
    };
    let names = names.into_iter().collect::<Vec<_>>();
    // Only called functions contribute direct context. Reference values remain
    // literal documents; their links never trigger recursive resolution.
    let mut functions = BTreeSet::new();
    if let NeuralCommand::Expression(expression) = &command {
        function_names(expression, &mut functions);
    }
    let mut all_names = names.into_iter().collect::<BTreeSet<_>>();
    for function in functions {
        if function.ends_with('/') {
            return Err(failure("A function must name one document."));
        }
        let documents =
            crate::document_bindings::resolve_references(store, std::slice::from_ref(&function))?;
        for document in documents {
            for reference in document_references(&document.text).map_err(io_failure)? {
                all_names.insert(reference.name);
            }
        }
    }
    let all_names = all_names.into_iter().collect::<Vec<_>>();
    let sources = crate::document_bindings::resolve_references(store, &all_names)?;
    let mut bindings = BTreeMap::new();
    let mut binding_bytes = 0;
    for name in all_names.into_iter().collect::<BTreeSet<_>>() {
        let documents =
            crate::document_bindings::resolve_references(store, std::slice::from_ref(&name))?;
        let mut text = String::new();
        for document in documents {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&document.text);
        }
        binding_bytes += text.len();
        if binding_bytes > MAX_PROMPT_BYTES {
            return Err(failure(
                "The combined document references exceed the context budget.",
            ));
        }
        bindings.insert(name, bounded(text)?);
    }
    let input_blob_id = store
        .store_provenance_blob(input.as_bytes())
        .map_err(IpcFailure::store)?;
    let title = match &command {
        NeuralCommand::Expression(NeuralExpression::Call { function, .. }) => function.clone(),
        NeuralCommand::Expression(NeuralExpression::Reference { name }) => format!("From {name}"),
        _ => entry
            .lines()
            .next()
            .unwrap_or("Take")
            .trim_start_matches(['#', ' '])
            .chars()
            .take(60)
            .collect(),
    };
    let receipt = RunReceipt {
        run: TerminalRun {
            run_id: command_id.to_string(),
            status: "running".into(),
            title,
            expression: entry,
            output_document_id: None,
            output_relative_path: None,
            preview: String::new(),
            error: None,
            created_at_ms: now_unix_ms(),
        },
        request_fingerprint: fingerprint,
        source_document_id: document_id,
        source_revision_id: source.revision_id,
        input_blob_id,
        model: model.as_ref().map(|model| model.descriptor.clone()),
        bindings,
        sources,
        steps: Vec::new(),
    };
    let identity = GenerationFamilyIdentity {
        request_id: format!("terminal-{command_id}"),
        project_id: store.manifest().project_id,
        session_id: parse_command_id(&session_id)?,
        document_id,
    };
    let run_id = GenerationRunId::new();
    let branch_id = BranchId::new();
    let control = Arc::new(TerminalControl::default());
    state
        .generations
        .reserve(identity.clone(), vec![(run_id, branch_id)])
        .map_err(|error| IpcFailure::generation_registry(&error))?;
    if let Err(error) = state
        .generations
        .attach_cancellation(&identity.request_id, control.clone())
    {
        let _ = state.generations.complete_family(&identity.request_id);
        return Err(IpcFailure::generation_registry(&error));
    }
    let (ticket, lease) = match state
        .generation_lifecycle
        .reserve(identity.request_id.clone())
    {
        Ok(value) => value,
        Err(error) => {
            let _ = state.generations.complete_family(&identity.request_id);
            return Err(io_failure(error));
        }
    };
    let setup = (|| {
        state
            .generation_lifecycle
            .queue(&lease)
            .map_err(io_failure)?;
        state
            .generation_lifecycle
            .start(&lease)
            .map_err(io_failure)?;
        write_receipt(&root, &receipt, false)
    })();
    if let Err(error) = setup {
        let _ = state.generation_lifecycle.fail_and_release(&lease);
        let _ = state.generations.complete_family(&identity.request_id);
        return Err(error);
    }
    drop(session);
    let reservation = match state
        .generation_workers
        .reserve(&identity.request_id, &admission)
    {
        Ok(value) => value,
        Err(error) => {
            let _ = state.generation_lifecycle.fail_and_release(&lease);
            let _ = state.generations.complete_family(&identity.request_id);
            return Err(error);
        }
    };
    let start = Arc::new(GenerationWorkerStartGate::default());
    let worker_start = Arc::clone(&start);
    let worker_control = Arc::clone(&control);
    let worker_app = app.clone();
    let worker_identity = identity.clone();
    let response = receipt.run.clone();
    let worker = std::thread::Builder::new()
        .name("loom-experiment".into())
        .spawn(move || {
            worker_start.wait();
            let state = worker_app.state::<PluginState>();
            let mut evaluator = Evaluator {
                state: &state,
                identity: &worker_identity,
                model: model.as_ref(),
                control: &worker_control,
                source: &source,
                input,
                receipt,
                step: 0,
            };
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                evaluator.evaluate_command(&command)
            }));
            let outcome = outcome.unwrap_or_else(|_| {
                worker_control.panicked.store(true, Ordering::Release);
                Err(failure("The experiment stopped unexpectedly."))
            });
            let saved = evaluator.finish(outcome);
            let terminal = if saved.is_err() {
                GenerationTerminalClass::Failed
            } else {
                match evaluator.receipt.run.status.as_str() {
                    "cancelled" => GenerationTerminalClass::Cancelled,
                    "completed" => GenerationTerminalClass::Completed,
                    _ => GenerationTerminalClass::Failed,
                }
            };
            if let Err(error) = saved {
                let _ = state
                    .generations
                    .mark_terminal_persistence_failure(&worker_identity.request_id, error.message);
            } else {
                let _ = state
                    .generations
                    .complete_family(&worker_identity.request_id);
            }
            let _ = state
                .generation_lifecycle
                .terminal_and_release(&lease, terminal);
        });
    let worker = match worker {
        Ok(worker) => worker,
        Err(error) => {
            if let Ok(Some(lease)) = state
                .generation_lifecycle
                .current_lease(&identity.request_id)
            {
                let _ = state.generation_lifecycle.fail_and_release(&lease);
            }
            let _ = state.generations.complete_family(&identity.request_id);
            return Err(io_failure(error));
        }
    };
    if let Err(error) = reservation.attach(worker, GenerationWorkerOwner::Terminal(control)) {
        error.owner.cancel_all();
        start.release();
        let _ = error.worker.join();
        error.owner.shutdown_joined();
        return Err(error.failure);
    }
    start.release();
    ticket.detach();
    drop(model_guard);
    Ok(response)
}

struct Evaluator<'a> {
    state: &'a PluginState,
    identity: &'a GenerationFamilyIdentity,
    model: Option<&'a LoadedModel>,
    control: &'a TerminalControl,
    source: &'a LoadedDocument,
    input: String,
    receipt: RunReceipt,
    step: u32,
}

impl Evaluator<'_> {
    fn with_store<T>(
        &self,
        operation: impl FnOnce(&mut ProjectStore) -> Result<T, IpcFailure>,
    ) -> Result<T, IpcFailure> {
        let mut session = lock_session_internal(self.state)?;
        operation(require_bound_store(
            &mut session,
            &self.identity.project_id.to_string(),
            &self.identity.session_id.to_string(),
        )?)
    }

    fn evaluate_command(&mut self, command: &NeuralCommand) -> Result<String, IpcFailure> {
        match command {
            NeuralCommand::Prompt(text) => {
                let mut prefix = String::new();
                for (name, value) in &self.receipt.bindings {
                    let _ = write!(prefix, "# {name}\n\n{value}\n\n");
                }
                prefix.push_str(text);
                self.complete(bounded(prefix)?)
            }
            NeuralCommand::Expression(expression) => self.evaluate(expression),
        }
    }

    fn evaluate(&mut self, expression: &NeuralExpression) -> Result<String, IpcFailure> {
        if self.control.cancelled.load(Ordering::Acquire) {
            return Err(failure("Cancelled"));
        }
        match expression {
            NeuralExpression::Reference { name } => self.binding(name),
            NeuralExpression::Literal { text } => Ok(text.clone()),
            NeuralExpression::Call {
                function,
                arguments,
            } => {
                let function = self.binding(function)?;
                let mut inputs = Vec::new();
                for argument in arguments {
                    inputs.push(self.evaluate(argument)?);
                }
                if inputs.is_empty() {
                    inputs.push(self.input.clone());
                }
                let mut contextual_function = String::new();
                let mut seen_context = BTreeSet::new();
                for reference in document_references(&function).map_err(io_failure)? {
                    if !seen_context.insert(reference.name.clone()) {
                        continue;
                    }
                    let context = self.binding(&reference.name)?;
                    let _ = write!(contextual_function, "# {}\n\n{context}\n\n", reference.name);
                }
                contextual_function.push_str(&function);
                let prompt = render_base_function_prompt(
                    &contextual_function,
                    &inputs.iter().map(String::as_str).collect::<Vec<_>>(),
                )
                .map_err(io_failure)?;
                self.complete(bounded(prompt)?)
            }
        }
    }

    fn binding(&self, name: &str) -> Result<String, IpcFailure> {
        self.receipt
            .bindings
            .get(name)
            .cloned()
            .ok_or_else(|| failure(format!("Unresolved document @{name}")))
    }

    #[allow(clippy::too_many_lines)]
    fn complete(&mut self, prompt: String) -> Result<String, IpcFailure> {
        if self.control.cancelled.load(Ordering::Acquire) {
            return Err(failure("Cancelled"));
        }
        let model = self
            .model
            .ok_or_else(|| failure("Choose a local model before trying this idea."))?;
        self.step += 1;
        let request_id = format!("{}-{}", self.identity.request_id, self.step);
        let environment = model_environment_from_verified(&model.descriptor)
            .map_err(|error| IpcFailure::backend(&error))?;
        let (recipe, case) = self.with_store(|store| {
            let prompt_blob = store
                .store_provenance_blob(prompt.as_bytes())
                .map_err(IpcFailure::store)?;
            let environment_artifact = store
                .record_model_environment(&environment)
                .map_err(IpcFailure::store)?;
            let mut inputs = vec![self.source.artifact_id];
            inputs.extend(self.receipt.sources.iter().map(|source| source.artifact_id));
            inputs.sort();
            inputs.dedup();
            let recipe = PromptRecipe {
                mode: PromptMode::RawCompletion,
                exact_prompt_blob_id: prompt_blob,
                exact_prompt_token_ids: None,
                ordered_input_artifact_ids: inputs.clone(),
                prompt_token_count: None,
            };
            let context_evidence = store
                .store_provenance_blob(
                    &serde_json::to_vec(&self.receipt.sources).map_err(io_failure)?,
                )
                .map_err(IpcFailure::store)?;
            let prompt_artifact = store
                .record_prompt_recipe(&recipe)
                .map_err(IpcFailure::store)?;
            let context = store
                .record_context_recipe(&ContextRecipe {
                    source_revision_id: self.source.revision_id,
                    ordered_source_artifact_ids: inputs,
                    token_budget: u64::from(model.profile.context_tokens),
                    retrieval_evidence_blob_id: Some(context_evidence),
                })
                .map_err(IpcFailure::store)?;
            let authority = store
                .record_authority_policy(&AuthorityPolicy {
                    policy_version: 1,
                    writer_environment_artifact_ids: vec![environment_artifact.artifact_id],
                    critic_environment_artifact_ids: Vec::new(),
                })
                .map_err(IpcFailure::store)?;
            let sampling = sampling_for_weave_case(
                parse_command_id(&self.receipt.run.run_id)?,
                self.step,
                512,
                0.8,
                WeavePreset::ManualV2,
            );
            let generation = GenerationStart {
                run_id: GenerationRunId::new(),
                branch_id: BranchId::new(),
                document_id: self.source.document_id,
                source_revision_id: self.source.revision_id,
                target_range: ByteRange::new(0, 0).expect("empty range"),
                model_environment_artifact_id: environment_artifact.artifact_id,
                prompt_recipe_artifact_id: prompt_artifact.artifact_id,
                context_recipe_artifact_id: context.artifact_id,
                authority_policy_artifact_id: authority.artifact_id,
                seed: u64::from(sampling.seed),
                sampling: serde_json::Value::Null,
            };
            Ok((
                recipe,
                ContinuationCase::bind_sampling(generation, sampling).map_err(io_failure)?,
            ))
        })?;
        let owner = self
            .state
            .backend
            .start_exact_continuation(ExactContinuationRequest {
                request_id: request_id.clone(),
                model: model.profile.clone(),
                exact_manuscript_prefix: prompt,
                context_preamble: String::new(),
                media: Vec::new(),
                prompt_recipe: recipe.clone(),
                cases: vec![case],
            })
            .map_err(|error| IpcFailure::backend(&error))?;
        let control = owner.control();
        self.control.attach(control.clone());
        let result = loop {
            match control.receive_result_timeout(Duration::from_millis(100)) {
                Err(LlamaBackendError::ResultTimeout) => {
                    if self.control.cancelled.load(Ordering::Acquire) {
                        control.cancel_all();
                    }
                }
                result => break result,
            }
        };
        let joined = owner.shutdown_joined();
        self.control
            .joined
            .fetch_add(joined.joined_worker_count(), Ordering::AcqRel);
        self.control
            .panicked
            .fetch_or(joined.worker_panicked(), Ordering::AcqRel);
        self.control
            .current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let result = result.map_err(|error| IpcFailure::backend(&error))?;
        let candidate = result
            .candidates
            .first()
            .ok_or_else(|| failure("The model returned no result"))?;
        validate_candidate_receipt_binding(
            candidate,
            &request_id,
            recipe.exact_prompt_blob_id,
            PromptMode::RawCompletion,
            &result.context_binding,
            &model.descriptor,
            0,
        )
        .map_err(|error| IpcFailure::backend(&error))?;
        let evidence = self.with_store(|store| {
            store
                .store_provenance_blob(&serde_json::to_vec(&result).map_err(io_failure)?)
                .map_err(IpcFailure::store)
        })?;
        self.receipt.steps.push(evidence);
        if candidate.terminal.status == GenerationTerminalStatus::Completed
            || !candidate.output_text.is_empty()
        {
            self.retain(&candidate.output_text, evidence)?;
        }
        if candidate.terminal.status != GenerationTerminalStatus::Completed {
            return Err(failure(if self.control.cancelled.load(Ordering::Acquire) {
                "Cancelled"
            } else {
                "The model stopped before completing this experiment"
            }));
        }
        Ok(candidate.output_text.clone())
    }

    fn retain(&mut self, text: &str, evidence: BlobId) -> Result<(), IpcFailure> {
        let title = self
            .receipt
            .run
            .title
            .chars()
            .filter(|character| character.is_alphanumeric() || matches!(character, ' ' | '-' | '_'))
            .take(48)
            .collect::<String>();
        let title = if title.trim().is_empty() {
            "Take"
        } else {
            title.trim()
        };
        let path = format!(
            "Runs/{}/{} - {title}.md",
            self.receipt.run.run_id, self.step
        );
        let id = self.with_store(|store| {
            if self.step == 0 {
                store
                    .create_derived_document_if_absent(
                        &path,
                        DocumentContent::Prose(text.to_owned()),
                        "retained expression",
                        evidence,
                    )
                    .map_err(IpcFailure::store)?;
            } else {
                store
                    .create_generated_document_if_absent(
                        &path,
                        DocumentContent::Prose(text.to_owned()),
                        "retained experiment",
                        evidence,
                    )
                    .map_err(IpcFailure::store)?;
            }
            Ok(store
                .read_document(&path)
                .map_err(IpcFailure::store)?
                .document_id)
        })?;
        self.receipt.run.output_document_id = Some(id.to_string());
        self.receipt.run.output_relative_path = Some(path);
        self.receipt.run.preview = text.chars().take(2000).collect();
        Ok(())
    }

    fn finish(&mut self, outcome: Result<String, IpcFailure>) -> Result<(), IpcFailure> {
        match outcome {
            Ok(text) => {
                if self.receipt.run.output_document_id.is_none() {
                    let evidence = self.with_store(|store| {
                        store
                            .store_provenance_blob(
                                &serde_json::to_vec(&self.receipt).map_err(io_failure)?,
                            )
                            .map_err(IpcFailure::store)
                    })?;
                    self.retain(&text, evidence)?;
                }
                self.receipt.run.status = "completed".into();
            }
            Err(error) => {
                self.receipt.run.status = if self.control.cancelled.load(Ordering::Acquire) {
                    "cancelled"
                } else {
                    "failed"
                }
                .into();
                self.receipt.run.error = Some(error.message);
            }
        }
        self.with_store(|store| write_receipt(store.root(), &self.receipt, true))
    }
}

#[tauri::command]
pub(super) async fn terminal_list(
    project_id: String,
    session_id: String,
    state: State<'_, PluginState>,
) -> Result<Vec<TerminalRun>, IpcFailure> {
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    let directory = receipt_directory(store.root())?;
    let mut ids = BTreeSet::new();
    for (index, entry) in std::fs::read_dir(directory)
        .map_err(io_failure)?
        .take(20_001)
        .enumerate()
    {
        if index == 20_000 {
            return Err(failure("The experiment history exceeds its read budget."));
        }
        let entry = entry.map_err(io_failure)?;
        let name = entry.file_name();
        if let Some(id) = name
            .to_str()
            .and_then(|name| name.strip_suffix(".started.json"))
            && id.parse::<CommandId>().is_ok()
        {
            ids.insert(id.to_owned());
        }
    }
    let mut runs = Vec::new();
    for id in ids.into_iter().rev().take(MAX_HISTORY) {
        if let Some(receipt) = read_receipt(store.root(), &id, true)? {
            runs.push(receipt.run);
        } else if let Some(mut receipt) = read_receipt(store.root(), &id, false)? {
            settle_interrupted(&mut receipt.run, &state)?;
            runs.push(receipt.run);
        }
    }
    Ok(runs)
}

#[tauri::command]
pub(super) async fn terminal_cancel(
    project_id: String,
    session_id: String,
    run_id: String,
    state: State<'_, PluginState>,
) -> Result<(), IpcFailure> {
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    if read_receipt(store.root(), &run_id, false)?.is_none() {
        return Err(failure("This experiment does not exist"));
    }
    if let Some(route) = state
        .generations
        .active_routes_for_document(
            store.manifest().project_id,
            parse_command_id(&session_id)?,
            read_receipt(store.root(), &run_id, false)?
                .expect("checked receipt")
                .source_document_id,
        )
        .map_err(|error| IpcFailure::generation_registry(&error))?
        .into_iter()
        .find(|route| route.identity.request_id == format!("terminal-{run_id}"))
    {
        state
            .generations
            .cancel_run(
                route.identity.project_id,
                route.identity.session_id,
                route.run_id,
            )
            .map_err(|error| IpcFailure::generation_registry(&error))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_is_exact_and_caret_uses_only_its_paragraph() {
        let text = "α idea\ncontinues\n\nβ next";
        assert_eq!(input_range(text, 2, 2).unwrap(), "α idea\ncontinues");
        assert_eq!(input_range(text, 0, 2).unwrap(), "α");
        assert!(input_range(text, 1, 2).is_err());
        assert!(input_range(text, 3, 2).is_err());
        let windows = "α idea\r\ncontinues\r\n\r\nβ next";
        assert_eq!(input_range(windows, 2, 2).unwrap(), "α idea\r\ncontinues");
        let end = windows.len() as u64;
        assert_eq!(input_range(windows, end, end).unwrap(), "β next");
    }

    #[test]
    fn preflight_counts_nested_calls_without_executing_references() {
        let NeuralCommand::Expression(expression) =
            parse_neural_command("=@Draft |> @Shorten |> @Polish").unwrap()
        else {
            panic!("expression")
        };
        let (mut names, mut calls) = (BTreeSet::new(), 0);
        expression_names(&expression, &mut names, &mut calls);
        assert_eq!(calls, 2);
        assert_eq!(
            names.into_iter().collect::<Vec<_>>(),
            ["Draft", "Polish", "Shorten"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn receipt_directory_refuses_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".loom")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join(".loom/function-runs"))
            .unwrap();
        assert!(receipt_directory(root.path()).is_err());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}

#[cfg(test)]
#[path = "terminal_tests.rs"]
mod integration_tests;
