//! A bounded evaluator over ordinary documents. The worker owns every native
//! call; references never execute, and results never overwrite source writing.

use super::*;
use crate::material_context::{self, Value};
use loom_document::{
    NeuralCommand, NeuralExpression, document_references, parse_neural_command,
    render_base_function_prompt,
};
use std::sync::atomic::AtomicUsize;

#[path = "terminal_scope.rs"]
mod scope;
use scope::{DocumentRunRequest, PreparedInput, RunRequest, RunScope, WorkspacePaneRequest};

const MAX_CALLS: usize = 8;
const MAX_PROMPT_BYTES: usize = 65_536;
const MAX_HISTORY: usize = 64;
const TERMINAL_GENERATION_TOKENS: u32 = 512;
const MAX_PRESENTATION_EVENTS: usize = 16;

fn remaining_context_bytes(context_tokens: u32, used_bytes: usize) -> usize {
    usize::try_from(
        context_tokens
            .saturating_sub(TERMINAL_GENERATION_TOKENS)
            .saturating_sub(1_024),
    )
    .unwrap_or(usize::MAX)
    .saturating_sub(used_bytes)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct TerminalRun {
    run_id: String,
    status: String,
    expression: String,
    #[serde(default)]
    source_document_id: String,
    #[serde(default)]
    presentation: Option<TerminalPresentation>,
    #[serde(default)]
    turn_boundary: Option<TerminalTurnBoundary>,
    title: String,
    output_document_id: Option<String>,
    output_relative_path: Option<String>,
    preview: String,
    error: Option<String>,
    created_at_ms: i64,
    /// Derived only at the IPC boundary; retained receipts have no event log.
    #[serde(default, skip_deserializing, skip_serializing_if = "Vec::is_empty")]
    events: Vec<TerminalEvent>,
}

#[derive(Clone, Debug, Serialize)]
struct TerminalEvent {
    kind: TerminalEventKind,
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum TerminalEventKind {
    Search,
    Context,
}

fn event_text(text: &str, limit: usize) -> String {
    let mut characters = text.chars();
    let mut bounded: String = characters
        .by_ref()
        .take(limit)
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    if characters.next().is_some() {
        bounded.push('…');
    }
    bounded
}

/// Receipts establish what happened. Expressions and model-written prose never
/// manufacture tool events, and presentation never rewrites retained evidence.
fn projected_run(receipt: &RunReceipt) -> TerminalRun {
    let mut run = receipt.run.clone();
    run.events.clear();
    let retained_sources = receipt
        .bindings
        .values()
        .filter_map(|value| match value.unscoped() {
            Value::Material { material } if material.text.is_some() => {
                Some(material.material.id.as_str())
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let sources = receipt.sources.len().saturating_add(retained_sources.len());
    if sources > 0 {
        run.events.push(TerminalEvent {
            kind: TerminalEventKind::Context,
            label: format!(
                "Read {sources} referenced {}",
                if sources == 1 { "source" } else { "sources" }
            ),
            detail: None,
        });
    }
    let available = MAX_PRESENTATION_EVENTS - run.events.len();
    let displayed = if receipt.searches.len() > available {
        available - 1
    } else {
        receipt.searches.len()
    };
    for search in receipt.searches.iter().take(displayed) {
        let count = search.hits.len();
        let outcome = if count == 0 && !search.complete {
            "No passages retained".to_owned()
        } else if count == 0 {
            "No matching passages".to_owned()
        } else {
            format!(
                "{count} {} retained",
                if count == 1 { "passage" } else { "passages" }
            )
        };
        let partial = if search.complete {
            ""
        } else {
            " · partial results"
        };
        run.events.push(TerminalEvent {
            kind: TerminalEventKind::Search,
            label: event_text(&format!("Searched {}", search.material.name), 96),
            detail: Some(event_text(
                &format!("“{}” · {outcome}{partial}", event_text(&search.query, 160)),
                256,
            )),
        });
    }
    let remaining = receipt.searches.len().saturating_sub(displayed);
    if remaining > 0 {
        run.events.push(TerminalEvent {
            kind: TerminalEventKind::Search,
            label: format!("{remaining} more searches"),
            detail: None,
        });
    }
    run
}

/// Display metadata is retained with the exact native input, never substituted
/// for it. Pane identity groups history without granting execution authority.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TerminalPresentation {
    pane_id: String,
    input: String,
}

/// The caller selects an output grammar independently of pane identity. A chat
/// continuation ends before the next speaker label; the prompt remains raw.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum TerminalTurnBoundary {
    Chat,
}

fn terminal_sampling(
    command_id: CommandId,
    step: u32,
    boundary: Option<TerminalTurnBoundary>,
) -> SamplingConfig {
    let mut sampling = sampling_for_weave_case(
        command_id,
        step,
        TERMINAL_GENERATION_TOKENS,
        0.8,
        WeavePreset::ManualV2,
    );
    if let Some(TerminalTurnBoundary::Chat) = boundary {
        sampling.stop = vec!["\nUser:".into(), "\nAssistant:".into()];
    }
    sampling
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SourceUse {
    origin: material_context::SourceOrigin,
    evidence_ids: Vec<String>,
    search_index: Option<usize>,
}

impl SourceUse {
    fn evidence_value(
        &self,
        evidence: &[crate::materials::MaterialEvidence],
    ) -> Result<Value, IpcFailure> {
        let evidence = self
            .evidence_ids
            .iter()
            .map(|id| {
                evidence
                    .iter()
                    .find(|hit| &hit.id == id)
                    .cloned()
                    .ok_or_else(|| failure("A retained source use is missing its evidence."))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Value::Scoped {
            origin: self.origin.clone(),
            value: Box::new(Value::Evidence {
                evidence,
                retrieval: None,
            }),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RunReceipt {
    #[serde(default)]
    scope: RunScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workspace_configuration: Option<Value>,
    run: TerminalRun,
    request_fingerprint: BlobId,
    source_document_id: DocumentId,
    source_revision_id: RevisionId,
    input_blob_id: BlobId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    function_recipe: Option<crate::workspace_template::FunctionRecipe>,
    #[serde(default)]
    context_references: Option<Vec<String>>,
    #[serde(default)]
    media: Vec<crate::terminal_media::RetainedMedia>,
    model: Option<VerifiedModelDescriptor>,
    bindings: BTreeMap<String, Value>,
    #[serde(default)]
    function_contexts: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    evidence: Vec<crate::materials::MaterialEvidence>,
    #[serde(default)]
    searches: Vec<crate::materials::MaterialSearch>,
    /// Typed full-scope aggregates, each retaining its explicit source origin.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    counts: Vec<Value>,
    /// Source attribution indexes the exact snapshots above without duplicating
    /// their text. Empty searches retain an origin through their search index.
    #[serde(default)]
    source_uses: Vec<SourceUse>,
    #[serde(default)]
    omitted_evidence: BTreeSet<String>,
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
        NeuralExpression::Count { source } => expression_names(source, names, calls),
        NeuralExpression::Find { source, query } => {
            expression_names(source, names, calls);
            expression_names(query, names, calls);
        }
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
    match expression {
        NeuralExpression::Count { source } => function_names(source, names),
        NeuralExpression::Call {
            function,
            arguments,
        } => {
            names.insert(function.clone());
            for argument in arguments {
                function_names(argument, names);
            }
        }
        NeuralExpression::Find { source, query } => {
            function_names(source, names);
            function_names(query, names);
        }
        _ => {}
    }
}

fn validate_explicit_references(references: &[String]) -> Result<(), IpcFailure> {
    if references.len() > 64
        || references.iter().any(|name| {
            name.trim().is_empty() || name.len() > 1024 || name.chars().any(char::is_control)
        })
    {
        return Err(failure(
            "Explicit context supports at most 64 document names of up to 1024 bytes each.",
        ));
    }
    Ok(())
}

fn prompt_reference_names(
    text: &str,
    references: Option<&[String]>,
    current_input: Option<&str>,
) -> Result<BTreeSet<String>, IpcFailure> {
    if let Some(references) = references {
        validate_explicit_references(references)?;
        let mut names = references.iter().cloned().collect::<BTreeSet<_>>();
        if let Some(input) = current_input {
            names.extend(
                document_references(input)
                    .map_err(io_failure)?
                    .into_iter()
                    .map(|reference| reference.name),
            );
        }
        Ok(names)
    } else {
        Ok(document_references(text)
            .map_err(io_failure)?
            .into_iter()
            .map(|reference| reference.name)
            .collect())
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
    let mut receipt: RunReceipt = serde_json::from_slice(&bytes).map_err(io_failure)?;
    if receipt.run.run_id != id {
        return Err(failure("Function receipt identity differs from its name"));
    }
    // The source has always been part of the immutable receipt. Expose it in
    // the presentation DTO without rewriting any retained history.
    receipt.run.source_document_id = receipt.source_document_id.to_string();
    Ok(Some(receipt))
}

fn settle_interrupted(
    run: &mut TerminalRun,
    scope: RunScope,
    state: &PluginState,
) -> Result<(), IpcFailure> {
    if run.status == "running"
        && state
            .generation_lifecycle
            .current_lease(&scope.request_id(&run.run_id))
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
    presentation: Option<TerminalPresentation>,
    context_references: Option<Vec<String>>,
    turn_boundary: Option<TerminalTurnBoundary>,
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<TerminalRun, IpcFailure> {
    let mut may_have_started = false;
    start_run(
        &RunRequest::Document(DocumentRunRequest {
            project_id,
            session_id,
            command_id,
            document_id,
            source_revision_id,
            expected_visible_blob_id,
            source_start_byte,
            source_end_byte,
            expression,
            presentation,
            context_references,
            turn_boundary,
        }),
        &app,
        &state,
        &mut may_have_started,
    )
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum WorkspacePaneSubmission {
    Accepted { run: Box<TerminalRun> },
    Rejected { error: IpcFailure },
}

#[tauri::command]
pub(super) async fn workspace_pane_run<R: Runtime>(
    request: WorkspacePaneRequest,
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<WorkspacePaneSubmission, IpcFailure> {
    let mut may_have_started = false;
    match start_run(
        &RunRequest::Workspace(request),
        &app,
        &state,
        &mut may_have_started,
    ) {
        Ok(run) => Ok(WorkspacePaneSubmission::Accepted { run: Box::new(run) }),
        Err(error) if !may_have_started => Ok(WorkspacePaneSubmission::Rejected { error }),
        Err(error) => Err(error),
    }
}

#[allow(clippy::too_many_lines)]
fn start_run<R: Runtime>(
    request: &RunRequest,
    app: &AppHandle<R>,
    state: &PluginState,
    may_have_started: &mut bool,
) -> Result<TerminalRun, IpcFailure> {
    let scope = request.scope();
    let (project_id, session_id, command_id) = request.identity();
    let project_id = project_id.to_owned();
    let session_id = session_id.to_owned();
    let command_id = parse_command_id(command_id)?;
    let mut fingerprint_bytes = request.fingerprint()?;
    let admission = lock_application_admission(state, "an experiment")?;
    let model_guard = lock_model_lifecycle(state)?;
    let mut session = lock_session(state)?;
    if scope == RunScope::Document {
        session
            .agency
            .admit_manual_generation()
            .map_err(io_failure)?;
    }
    let store = scope.store(&mut session, &project_id, &session_id)?;
    let root = store.root().to_owned();
    let existing = read_receipt(&root, &command_id.to_string(), false).inspect_err(|_| {
        // An unreadable receipt may belong to an earlier admitted submission.
        *may_have_started = true;
    })?;
    if let Some(receipt) = existing {
        // A replay uses the admitted configuration, even when the live dotfile
        // changed afterwards. A new command captures the new revision instead.
        if let Some(recipe) = &receipt.function_recipe {
            fingerprint_bytes.extend(serde_json::to_vec(recipe).map_err(io_failure)?);
        }
        let fingerprint = BlobId::digest(&fingerprint_bytes);
        if receipt.scope != scope || receipt.request_fingerprint != fingerprint {
            return Err(failure(
                "This run identifier belongs to a different experiment",
            ));
        }
        *may_have_started = true;
        let mut result = read_receipt(&root, &command_id.to_string(), true)?.unwrap_or(receipt);
        settle_interrupted(&mut result.run, scope, state)?;
        return Ok(projected_run(&result));
    }
    let PreparedInput {
        source,
        input,
        entry,
        presentation,
        context_references,
        turn_boundary,
        configuration: workspace_configuration,
        capture_request,
    } = request.prepare(&mut session)?;
    let document_id = source.document_id;
    if entry.trim().is_empty() {
        return Err(failure("Write or select an idea to try."));
    }
    let command = parse_neural_command(&entry).map_err(io_failure)?;
    let mut names = BTreeSet::new();
    let mut calls = 0;
    match &command {
        NeuralCommand::Prompt(text) => {
            names = prompt_reference_names(
                text,
                context_references.as_deref(),
                presentation
                    .as_ref()
                    .map(|presentation| presentation.input.as_str()),
            )?;
            calls = 1;
        }
        NeuralCommand::Expression(expression) => {
            if turn_boundary.is_some() {
                return Err(failure("A chat turn boundary requires a plain prompt."));
            }
            if context_references.is_some() {
                return Err(failure(
                    "Explicit context belongs to a plain prompt, not a function expression.",
                ));
            }
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
        Some(loaded_model(state)?)
    };
    let names = names.into_iter().collect::<Vec<_>>();
    // Only called functions contribute direct context. Reference values remain
    // literal documents; their links never trigger recursive resolution.
    let mut functions = BTreeSet::new();
    if let NeuralCommand::Expression(expression) = &command {
        function_names(expression, &mut functions);
    }
    let workspace_chat =
        scope == RunScope::Workspace && matches!(turn_boundary, Some(TerminalTurnBoundary::Chat));
    let function_recipe = if functions.is_empty() && !workspace_chat {
        None
    } else if scope == RunScope::Workspace {
        Some(crate::workspace_template::function_recipe_from_snapshot(
            crate::workspace_owner::store(&session)?,
            &source,
        )?)
    } else {
        Some(crate::workspace_template::function_recipe(
            crate::workspace_owner::store_mut(&mut session)?,
        )?)
    };
    let owner = session
        .workspace
        .as_ref()
        .ok_or_else(|| failure("The workspace closed before this experiment began."))?;
    let owner_project_id = owner.project_id;
    let owner_session_id = owner.session_id;
    if !names.is_empty() || !functions.is_empty() {
        crate::material_commands::restore_grants(
            state,
            crate::workspace_owner::require_store_mut(
                &mut session,
                &owner_project_id.to_string(),
                &owner_session_id.to_string(),
            )?,
        )?;
    }
    let mut capture = None;
    let mut capture_media = Vec::new();
    if scope == RunScope::Workspace && names.iter().any(|name| name == "document") {
        let (value, media) = scope::capture_document(&mut session, capture_request.as_ref())?;
        capture = Some(value);
        capture_media = media;
    }
    if let Some(recipe) = &function_recipe {
        fingerprint_bytes.extend(serde_json::to_vec(recipe).map_err(io_failure)?);
    }
    let fingerprint = BlobId::digest(&fingerprint_bytes);
    let mut all_names = names.into_iter().collect::<BTreeSet<_>>();
    let mut mounted = crate::workspace_references::Snapshots::default();
    mounted.admit(state, &session, all_names.iter().map(String::as_str))?;
    let mut function_contexts = BTreeMap::new();
    {
        let read_context = scope
            .read_context(
                &mut session,
                &project_id,
                &session_id,
                &owner_project_id.to_string(),
                &owner_session_id.to_string(),
            )?
            .with_mounted(&mounted);
        for function in functions {
            if function.ends_with('/') {
                return Err(failure("A function must name one document."));
            }
            let function_value = if scope == RunScope::Workspace && function == "document" {
                capture
                    .clone()
                    .ok_or_else(|| failure("This run has no captured active document."))?
            } else {
                read_context.resolve(&function)?
            };
            let function_text = material_context::exact(&function_value)?;
            let mut direct = BTreeMap::new();
            for reference in document_references(&function_text).map_err(io_failure)? {
                let bound = crate::workspace_references::relative(&function, &reference.name)?;
                all_names.insert(bound.clone());
                direct.insert(reference.name, bound);
            }
            function_contexts.insert(function, direct);
        }
    }
    mounted.admit(state, &session, all_names.iter().map(String::as_str))?;
    let mut bindings = BTreeMap::new();
    let mut sources = Vec::new();
    let mut seen_documents = BTreeSet::new();
    let mut folder_budget = material_context::FolderAdmissionBudget::default();
    let uses_capture = scope == RunScope::Workspace && all_names.contains("document");
    if uses_capture && capture.is_none() {
        let (value, media) = scope::capture_document(&mut session, capture_request.as_ref())?;
        capture = Some(value);
        capture_media = media;
    }
    let read_context = scope
        .read_context(
            &mut session,
            &project_id,
            &session_id,
            &owner_project_id.to_string(),
            &owner_session_id.to_string(),
        )?
        .with_mounted(&mounted);
    for name in all_names {
        let value = if scope == RunScope::Workspace && name == "document" {
            capture
                .clone()
                .ok_or_else(|| failure("This run has no captured active document."))?
        } else {
            read_context.resolve(&name)?
        };
        folder_budget.admit(&value)?;
        if let Value::Documents { documents } = value.unscoped()
            && !(scope == RunScope::Workspace && name == "document")
        {
            for document in documents {
                if crate::material_context::local_artifact_ids(read_context.documents, [&value])?
                    .contains(&document.artifact_id)
                    && seen_documents.insert(document.document_id)
                {
                    sources.push(document.clone());
                }
            }
        }
        bindings.insert(name, value);
    }
    let media = if let Some(model) = &model {
        let media = if scope == RunScope::Document {
            crate::terminal_media::resolve(
                read_context.documents,
                &source,
                &sources,
                resident_context_tokens(model),
            )?
        } else if uses_capture {
            capture_media
        } else {
            Vec::new()
        };
        let media = crate::terminal_media::merge(
            media,
            read_context.native_media(
                bindings
                    .iter()
                    .filter(|(name, _)| {
                        !(scope == RunScope::Workspace && name.as_str() == "document")
                    })
                    .map(|(_, value)| value),
            )?,
        )?;
        validate_media_against_resident_model(&media, &model.descriptor)?;
        media
    } else {
        // A reference-only expression still retains the mounted document's
        // exact media independently of the source root's later lifetime.
        read_context.native_media(
            bindings
                .iter()
                .filter(|(name, _)| name.contains("::"))
                .map(|(_, value)| value),
        )?
    };
    let store = scope.store(&mut session, &project_id, &session_id)?;
    let media_evidence = crate::terminal_media::retain(store, &media)?;
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
        scope,
        workspace_configuration,
        run: TerminalRun {
            run_id: command_id.to_string(),
            status: "running".into(),
            title,
            expression: entry,
            source_document_id: document_id.to_string(),
            presentation,
            turn_boundary,
            output_document_id: None,
            output_relative_path: None,
            preview: String::new(),
            error: None,
            created_at_ms: now_unix_ms(),
            events: Vec::new(),
        },
        request_fingerprint: fingerprint,
        source_document_id: document_id,
        source_revision_id: source.revision_id,
        input_blob_id,
        function_recipe,
        context_references,
        media: media_evidence,
        model: model.as_ref().map(|model| model.descriptor.clone()),
        bindings,
        function_contexts,
        evidence: Vec::new(),
        searches: Vec::new(),
        counts: Vec::new(),
        source_uses: Vec::new(),
        omitted_evidence: BTreeSet::new(),
        sources,
        steps: Vec::new(),
    };
    let identity = GenerationFamilyIdentity {
        request_id: scope.request_id(&command_id.to_string()),
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
        // Publication can fail after creating the receipt. From this point the
        // caller must reconcile the same command, never assume non-admission.
        *may_have_started = true;
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
    let response = projected_run(&receipt);
    let worker = std::thread::Builder::new()
        .name("loom-experiment".into())
        .spawn(move || {
            worker_start.wait();
            let state = worker_app.state::<PluginState>();
            let mut evaluator = Evaluator {
                state: &state,
                identity: &worker_identity,
                owner_project_id,
                owner_session_id,
                model: model.as_ref(),
                control: &worker_control,
                source: &source,
                input,
                receipt,
                media,
                step: 0,
                folder_scan_budget: material_context::FolderScanBudget::default(),
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
    owner_project_id: ProjectId,
    owner_session_id: CommandId,
    model: Option<&'a LoadedModel>,
    control: &'a TerminalControl,
    source: &'a LoadedDocument,
    input: String,
    receipt: RunReceipt,
    media: Vec<llama_native_types::MediaInput>,
    step: u32,
    folder_scan_budget: material_context::FolderScanBudget,
}

impl Evaluator<'_> {
    fn with_read_context<T>(
        &self,
        operation: impl FnOnce(material_context::ReadContext<'_>) -> Result<T, IpcFailure>,
    ) -> Result<T, IpcFailure> {
        let mut session = lock_session_internal(self.state)?;
        let context = match self.receipt.scope {
            RunScope::Document => crate::workspace_owner::read_context(
                &session,
                &self.identity.project_id.to_string(),
                &self.identity.session_id.to_string(),
                &self.owner_project_id.to_string(),
                &self.owner_session_id.to_string(),
            )?,
            RunScope::Workspace => {
                material_context::ReadContext::from(&*crate::workspace_owner::require_store_mut(
                    &mut session,
                    &self.owner_project_id.to_string(),
                    &self.owner_session_id.to_string(),
                )?)
            }
        };
        operation(context)
    }

    fn with_store<T>(
        &self,
        operation: impl FnOnce(&mut ProjectStore) -> Result<T, IpcFailure>,
    ) -> Result<T, IpcFailure> {
        let mut session = lock_session_internal(self.state)?;
        operation(self.receipt.scope.store(
            &mut session,
            &self.identity.project_id.to_string(),
            &self.identity.session_id.to_string(),
        )?)
    }

    fn evaluate_command(&mut self, command: &NeuralCommand) -> Result<Value, IpcFailure> {
        match command {
            NeuralCommand::Prompt(text) => {
                let mut prefix = String::new();
                let query = self
                    .receipt
                    .run
                    .presentation
                    .as_ref()
                    .map_or(text.as_str(), |presentation| presentation.input.as_str());
                let query = query.to_owned();
                for (name, value) in self.receipt.bindings.clone() {
                    self.append_context(&mut prefix, &name, &value, &query, text.len())?;
                }
                prefix.push_str(text);
                self.complete(bounded(prefix)?, self.function_prompt_mode())
                    .map(Value::Text)
            }
            NeuralCommand::Expression(expression) => self.evaluate(expression),
        }
    }

    fn evaluate(&mut self, expression: &NeuralExpression) -> Result<Value, IpcFailure> {
        if self.control.cancelled.load(Ordering::Acquire) {
            return Err(failure("Cancelled"));
        }
        match expression {
            NeuralExpression::Reference { name } => self.binding(name),
            NeuralExpression::Literal { text } => Ok(Value::Text(text.clone())),
            NeuralExpression::Count { source } => {
                let source = self.evaluate(source)?;
                let value = self.with_read_context(|context| context.count_documents(&source))?;
                self.record_evidence(&value)?;
                Ok(value)
            }
            NeuralExpression::Find { source, query } => {
                let source = self.evaluate(source)?;
                let query = material_context::exact(&self.evaluate(query)?)?;
                let value = self.with_read_context(|context| {
                    context.search_with_cancel(&source, &query, &self.folder_scan_budget, &|| {
                        self.control.cancelled.load(Ordering::Acquire)
                    })
                })?;
                self.record_evidence(&value)?;
                Ok(value)
            }
            NeuralExpression::Call {
                function,
                arguments,
            } => {
                let direct = self
                    .receipt
                    .function_contexts
                    .get(function)
                    .cloned()
                    .unwrap_or_default();
                let function = material_context::exact(&self.binding(function)?)?;
                let mut inputs = Vec::new();
                for argument in arguments {
                    let value = self.evaluate(argument)?;
                    self.record_evidence(&value)?;
                    inputs.push(material_context::exact(&value)?);
                }
                if inputs.is_empty() {
                    inputs.push(self.input.clone());
                }
                let query = bounded(inputs.join("\n\n"))?;
                let input_refs = inputs.iter().map(String::as_str).collect::<Vec<_>>();
                let base_prompt_bytes = render_base_function_prompt(&function, &input_refs)
                    .map_err(io_failure)?
                    .len();
                let mut contextual_function = String::new();
                let mut seen_context = BTreeSet::new();
                for reference in document_references(&function).map_err(io_failure)? {
                    if !seen_context.insert(reference.name.clone()) {
                        continue;
                    }
                    let value =
                        self.binding(direct.get(&reference.name).unwrap_or(&reference.name))?;
                    self.append_context(
                        &mut contextual_function,
                        &reference.name,
                        &value,
                        &query,
                        base_prompt_bytes,
                    )?;
                }
                contextual_function.push_str(&function);
                let prompt = render_base_function_prompt(&contextual_function, &input_refs)
                    .map_err(io_failure)?;
                let mode = self.function_prompt_mode();
                self.complete(bounded(prompt)?, mode).map(Value::Text)
            }
        }
    }

    fn function_prompt_mode(&self) -> PromptMode {
        match self
            .receipt
            .function_recipe
            .as_ref()
            .map(|recipe| recipe.format)
        {
            Some(crate::workspace_template::FunctionFormat::Model) => PromptMode::Function,
            _ => PromptMode::RawCompletion,
        }
    }

    fn record_evidence(&mut self, value: &Value) -> Result<(), IpcFailure> {
        if let Value::Count { .. } = value.unscoped() {
            if !matches!(value, Value::Scoped { .. }) {
                return Err(failure("A count is missing its source origin."));
            }
            let snapshot = serde_json::to_value(value).map_err(io_failure)?;
            if !self
                .receipt
                .counts
                .iter()
                .any(|prior| serde_json::to_value(prior).is_ok_and(|prior| prior == snapshot))
            {
                self.receipt.counts.push(value.clone());
            }
        }
        if let Value::Evidence {
            evidence,
            retrieval,
        } = value.unscoped()
        {
            let Value::Scoped { origin, .. } = value else {
                return Err(failure("Retrieved evidence is missing its source origin."));
            };
            let search_index = if let Some(search) = retrieval {
                let snapshot = serde_json::to_vec(search.as_ref()).map_err(io_failure)?;
                let mut existing = None;
                for (index, prior) in self.receipt.searches.iter().enumerate() {
                    if serde_json::to_vec(prior).map_err(io_failure)? == snapshot {
                        existing = Some(index);
                        break;
                    }
                }
                Some(existing.unwrap_or_else(|| {
                    let index = self.receipt.searches.len();
                    self.receipt.searches.push(search.as_ref().clone());
                    index
                }))
            } else {
                None
            };
            for hit in evidence {
                if !self.receipt.evidence.iter().any(|prior| prior.id == hit.id) {
                    self.receipt.evidence.push(hit.clone());
                }
            }
            let source_use = SourceUse {
                origin: origin.clone(),
                evidence_ids: evidence.iter().map(|hit| hit.id.clone()).collect(),
                search_index,
            };
            if !self.receipt.source_uses.iter().any(|prior| {
                prior.origin == source_use.origin
                    && prior.evidence_ids == source_use.evidence_ids
                    && prior.search_index == source_use.search_index
            }) {
                self.receipt.source_uses.push(source_use);
            }
        }
        Ok(())
    }

    fn append_context(
        &mut self,
        prefix: &mut String,
        name: &str,
        value: &Value,
        query: &str,
        base_bytes: usize,
    ) -> Result<(), IpcFailure> {
        let model = self
            .model
            .ok_or_else(|| failure("Choose a local model before trying this idea."))?;
        let label = format!("# {name:?}\n\n");
        let used = base_bytes
            .saturating_add(prefix.len())
            .saturating_add(label.len())
            .saturating_add(2);
        let budget = remaining_context_bytes(resident_context_tokens(model), used);
        let consulted = self.consult(value, query, budget)?;
        prefix.push_str(&label);
        prefix.push_str(&material_context::exact(&consulted)?);
        prefix.push_str("\n\n");
        Ok(())
    }

    fn consult(&mut self, value: &Value, query: &str, budget: usize) -> Result<Value, IpcFailure> {
        let (consulted, omitted) = self.with_read_context(|context| {
            // A captured document is exact admitted data. Reading its frozen
            // text neither requires nor renews access to its former root.
            if self.receipt.scope == RunScope::Workspace
                && let Value::Scoped { origin, value } = value
                && matches!(
                    value.as_ref(),
                    Value::Documents { .. } | Value::Evidence { .. }
                )
            {
                let (result, omitted) = material_context::consult_with_budget_and_cancel(
                    context.documents,
                    value,
                    query,
                    budget,
                    &self.folder_scan_budget,
                    &|| self.control.cancelled.load(Ordering::Acquire),
                )?;
                return Ok((
                    Value::Scoped {
                        origin: origin.clone(),
                        value: Box::new(result),
                    },
                    omitted,
                ));
            }
            context.consult_with_budget_and_cancel(
                value,
                query,
                budget,
                &self.folder_scan_budget,
                &|| self.control.cancelled.load(Ordering::Acquire),
            )
        })?;
        self.receipt.omitted_evidence.extend(omitted);
        self.record_evidence(&consulted)?;
        Ok(consulted)
    }

    fn binding(&self, name: &str) -> Result<Value, IpcFailure> {
        self.receipt
            .bindings
            .get(name)
            .cloned()
            .ok_or_else(|| failure(format!("Unresolved reference @{name}")))
    }

    #[allow(clippy::too_many_lines)]
    fn complete(&mut self, prompt: String, mode: PromptMode) -> Result<String, IpcFailure> {
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
            let evidence_values = self
                .receipt
                .source_uses
                .iter()
                .map(|source| source.evidence_value(&self.receipt.evidence))
                .collect::<Result<Vec<_>, _>>()?;
            inputs.extend(material_context::local_artifact_ids(
                store,
                self.receipt.bindings.values().chain(&evidence_values),
            )?);
            if let Some(artifact_id) = self
                .receipt
                .function_recipe
                .as_ref()
                .and_then(|recipe| recipe.local_configuration_artifact(store))
            {
                inputs.push(artifact_id);
            }
            inputs.sort();
            inputs.dedup();
            let recipe = PromptRecipe {
                mode,
                exact_prompt_blob_id: prompt_blob,
                exact_prompt_token_ids: None,
                ordered_input_artifact_ids: inputs.clone(),
                prompt_token_count: None,
            };
            let context_evidence = store
                .store_provenance_blob(
                    &serde_json::to_vec(&(
                        &self.receipt.sources,
                        &self.receipt.media,
                        &self.receipt.evidence,
                        &self.receipt.searches,
                        &self.receipt.bindings,
                        &self.receipt.omitted_evidence,
                        // Foreign configuration is retained with its source
                        // identity and exact bytes, never as a local artifact ID.
                        &self.receipt.function_recipe,
                        &self.receipt.source_uses,
                        &self.receipt.workspace_configuration,
                        &self.receipt.counts,
                    ))
                    .map_err(io_failure)?,
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
            let sampling = terminal_sampling(
                parse_command_id(&self.receipt.run.run_id)?,
                self.step,
                self.receipt.run.turn_boundary,
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
                media: self.media.clone(),
                first_word_choices: None,
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
            mode,
            &result.context_binding,
            &model.descriptor,
            // Terminal submits one continuation case, always at input index 0.
            0,
            None,
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
            self.retain(
                &candidate.output_text,
                evidence,
                true,
                &self.step.to_string(),
            )?;
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

    fn retain(
        &mut self,
        text: &str,
        evidence: BlobId,
        generated: bool,
        slot: &str,
    ) -> Result<(), IpcFailure> {
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
        let path = format!("Runs/{}/{}/{title}.md", self.receipt.run.run_id, slot);
        let id = self.with_store(|store| {
            if generated {
                store
                    .create_generated_document_if_absent(
                        &path,
                        DocumentContent::Prose(text.to_owned()),
                        "retained experiment",
                        evidence,
                    )
                    .map_err(IpcFailure::store)?;
            } else {
                store
                    .create_derived_document_if_absent(
                        &path,
                        DocumentContent::Prose(text.to_owned()),
                        "retained expression",
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

    fn finish(&mut self, outcome: Result<Value, IpcFailure>) -> Result<(), IpcFailure> {
        let outcome =
            outcome.and_then(|value| material_context::exact(&value).map(|text| (value, text)));
        match outcome {
            Ok((value, text)) => {
                self.record_evidence(&value)?;
                if self.receipt.run.output_document_id.is_none() || !matches!(value, Value::Text(_))
                {
                    let evidence = self.with_store(|store| {
                        store
                            .store_provenance_blob(
                                &serde_json::to_vec(&self.receipt).map_err(io_failure)?,
                            )
                            .map_err(IpcFailure::store)
                    })?;
                    self.retain(&text, evidence, false, "result")?;
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
    list_runs(&state, &project_id, &session_id, RunScope::Document)
}

#[tauri::command]
pub(super) async fn workspace_pane_list(
    project_id: String,
    session_id: String,
    state: State<'_, PluginState>,
) -> Result<Vec<TerminalRun>, IpcFailure> {
    list_runs(&state, &project_id, &session_id, RunScope::Workspace)
}

fn list_runs(
    state: &PluginState,
    project_id: &str,
    session_id: &str,
    scope: RunScope,
) -> Result<Vec<TerminalRun>, IpcFailure> {
    let mut session = lock_session(state)?;
    let store = scope.store(&mut session, project_id, session_id)?;
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
    for id in ids.into_iter().rev() {
        if let Some(receipt) = read_receipt(store.root(), &id, true)? {
            if visible_history(&receipt, scope) {
                runs.push(projected_run(&receipt));
            }
        } else if let Some(mut receipt) = read_receipt(store.root(), &id, false)?
            && visible_history(&receipt, scope)
        {
            settle_interrupted(&mut receipt.run, receipt.scope, state)?;
            runs.push(projected_run(&receipt));
        }
        if runs.len() == MAX_HISTORY {
            break;
        }
    }
    Ok(runs)
}

fn visible_history(receipt: &RunReceipt, scope: RunScope) -> bool {
    receipt.scope == scope || (scope == RunScope::Workspace && receipt.run.presentation.is_some())
}

#[tauri::command]
pub(super) async fn terminal_cancel(
    project_id: String,
    session_id: String,
    run_id: String,
    state: State<'_, PluginState>,
) -> Result<(), IpcFailure> {
    cancel_run(
        &state,
        &project_id,
        &session_id,
        &run_id,
        RunScope::Document,
    )
}

#[tauri::command]
pub(super) async fn workspace_pane_cancel(
    project_id: String,
    session_id: String,
    run_id: String,
    state: State<'_, PluginState>,
) -> Result<(), IpcFailure> {
    cancel_run(
        &state,
        &project_id,
        &session_id,
        &run_id,
        RunScope::Workspace,
    )
}

fn cancel_run(
    state: &PluginState,
    project_id: &str,
    session_id: &str,
    run_id: &str,
    scope: RunScope,
) -> Result<(), IpcFailure> {
    let request_id = scope.request_id(&parse_command_id(run_id)?.to_string());
    let routes = state
        .generations
        .active_routes_for_request(
            project_id.parse::<ProjectId>().map_err(io_failure)?,
            parse_command_id(session_id)?,
            &request_id,
        )
        .map_err(|error| IpcFailure::generation_registry(&error))?;
    if !routes.is_empty() {
        // The admitted route owns cancellation authority independently of the
        // store. A retrieval worker may currently hold the session lock.
        for route in routes {
            match state.generations.cancel_run(
                route.identity.project_id,
                route.identity.session_id,
                route.run_id,
            ) {
                Ok(_) | Err(loom_host::GenerationRegistryError::RunNotActive(_)) => {}
                Err(error) => return Err(IpcFailure::generation_registry(&error)),
            }
        }
        return Ok(());
    }
    // Preserve inactive receipt validation and the admission race: a family
    // may become live while we wait for an in-progress admission's store lock.
    let mut session = lock_session(state)?;
    let store = scope.store(&mut session, project_id, session_id)?;
    let receipt = read_receipt(store.root(), run_id, false)?
        .filter(|receipt| receipt.scope == scope)
        .ok_or_else(|| failure("This experiment does not exist in the requested scope."))?;
    if let Some(route) = state
        .generations
        .active_routes_for_document(
            store.manifest().project_id,
            parse_command_id(session_id)?,
            receipt.source_document_id,
        )
        .map_err(|error| IpcFailure::generation_registry(&error))?
        .into_iter()
        .find(|route| route.identity.request_id == request_id)
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

pub(super) fn output_document(
    store: &ProjectStore,
    run_id: &str,
) -> Result<Option<DocumentId>, IpcFailure> {
    let canonical = parse_command_id(run_id)?.to_string();
    if canonical != run_id {
        return Err(failure("The run identity is not canonical."));
    }
    let receipt = read_receipt(store.root(), run_id, true)?
        .or(read_receipt(store.root(), run_id, false)?)
        .filter(|receipt| visible_history(receipt, RunScope::Workspace))
        .ok_or_else(|| failure("This workspace pane run does not exist."))?;
    receipt
        .run
        .output_document_id
        .map(|id| id.parse().map_err(io_failure))
        .transpose()
}

/// Application close holds its admission barrier while this drain runs. Root
/// switches never invoke it: their document-session drain has a different token.
pub(super) fn drain_workspace_runs(state: &PluginState, wait: Duration) -> Result<(), IpcFailure> {
    let owner = {
        let session = lock_session_internal(state)?;
        session
            .workspace
            .as_ref()
            .map(|owner| (owner.project_id, owner.session_id))
    };
    let Some((project, session)) = owner else {
        return Ok(());
    };
    state
        .generations
        .cancel_session(project, session)
        .map_err(|error| IpcFailure::generation_registry(&error))?;
    if !state
        .generations
        .wait_for_session_idle(project, session, wait)
        .map_err(|error| IpcFailure::generation_registry(&error))?
    {
        let failures = state
            .generations
            .terminal_persistence_failures(project, session)
            .map_err(|error| IpcFailure::generation_registry(&error))?;
        if let Some(failure) = failures.first() {
            return Err(IpcFailure::new(
                "workspace_run_persistence_failed",
                failure.error.clone(),
                true,
            ));
        }
        return Err(IpcFailure::new(
            "workspace_run_cancellation_in_progress",
            "Workspace pane runs are preserving their final results. Retry closing shortly.",
            true,
        ));
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

#[cfg(all(test, unix))]
#[path = "terminal_tests.rs"]
mod integration_tests;
