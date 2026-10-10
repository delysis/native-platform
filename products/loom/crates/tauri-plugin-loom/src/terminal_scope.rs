//! An execution destination is live authority; captured documents are frozen data.
use super::*;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum RunScope {
    #[default]
    Document,
    Workspace,
}

impl RunScope {
    pub(super) fn read_context<'a>(
        self,
        session: &'a mut Session,
        project: &str,
        token: &str,
        owner_project: &str,
        owner_token: &str,
    ) -> Result<material_context::ReadContext<'a>, IpcFailure> {
        match self {
            Self::Document => crate::workspace_owner::read_context(
                session,
                project,
                token,
                owner_project,
                owner_token,
            ),
            Self::Workspace => Ok(material_context::ReadContext::from(
                &*crate::workspace_owner::require_store_mut(session, owner_project, owner_token)?,
            )),
        }
    }
    pub(super) fn request_id(self, id: &str) -> String {
        let prefix = match self {
            Self::Document => "terminal",
            Self::Workspace => "pane",
        };
        format!("{prefix}-{id}")
    }
    pub(super) fn store<'a>(
        self,
        session: &'a mut Session,
        project: &str,
        token: &str,
    ) -> Result<&'a mut ProjectStore, IpcFailure> {
        match self {
            Self::Document => require_bound_store(session, project, token),
            Self::Workspace => crate::workspace_owner::require_store_mut(session, project, token),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_field_names)] // IPC identity fields match the document command contract.
pub(crate) struct CapturedDocument {
    pub project_id: String,
    pub session_id: String,
    pub document_id: String,
    pub revision_id: String,
    pub visible_blob_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkspacePaneRequest {
    pub workspace_id: String,
    pub workspace_session_id: String,
    pub command_id: String,
    pub pane_id: String,
    pub configuration_revision_id: String,
    pub expression: String,
    pub input: String,
    pub captured_document: Option<CapturedDocument>,
}

pub(super) struct DocumentRunRequest {
    pub project_id: String,
    pub session_id: String,
    pub command_id: String,
    pub document_id: String,
    pub source_revision_id: String,
    pub expected_visible_blob_id: String,
    pub source_start_byte: u64,
    pub source_end_byte: u64,
    pub expression: String,
    pub presentation: Option<TerminalPresentation>,
    pub context_references: Option<Vec<String>>,
    pub turn_boundary: Option<TerminalTurnBoundary>,
}

pub(super) enum RunRequest {
    Document(DocumentRunRequest),
    Workspace(WorkspacePaneRequest),
}

pub(super) struct PreparedInput {
    pub source: LoadedDocument,
    pub input: String,
    pub entry: String,
    pub presentation: Option<TerminalPresentation>,
    pub context_references: Option<Vec<String>>,
    pub turn_boundary: Option<TerminalTurnBoundary>,
    pub configuration: Option<Value>,
    pub capture_request: Option<CapturedDocument>,
}

impl RunRequest {
    pub(super) fn scope(&self) -> RunScope {
        match self {
            Self::Document(_) => RunScope::Document,
            Self::Workspace(_) => RunScope::Workspace,
        }
    }

    pub(super) fn identity(&self) -> (&str, &str, &str) {
        match self {
            Self::Document(request) => (
                &request.project_id,
                &request.session_id,
                &request.command_id,
            ),
            Self::Workspace(request) => (
                &request.workspace_id,
                &request.workspace_session_id,
                &request.command_id,
            ),
        }
    }

    pub(super) fn fingerprint(&self) -> Result<Vec<u8>, IpcFailure> {
        match self {
            Self::Workspace(request) => {
                if !crate::workspace_template::valid_pane_id(&request.pane_id)
                    || request.expression.len() > MAX_PROMPT_BYTES
                    || request.input.len() > MAX_PROMPT_BYTES
                {
                    return Err(failure(
                        "A pane run needs a valid pane name and input within 64 KiB.",
                    ));
                }
                serde_json::to_vec(&(RunScope::Workspace, request)).map_err(io_failure)
            }
            Self::Document(request) => {
                if let Some(presentation) = &request.presentation
                    && (!crate::workspace_template::valid_pane_id(&presentation.pane_id)
                        || presentation.input.len() > MAX_PROMPT_BYTES)
                {
                    return Err(failure(
                        "Run presentation needs a valid pane name and input within 64 KiB.",
                    ));
                }
                let mut bytes = serde_json::to_vec(&(
                    &request.project_id,
                    &request.document_id,
                    &request.source_revision_id,
                    &request.expected_visible_blob_id,
                    request.source_start_byte,
                    request.source_end_byte,
                    &request.expression,
                ))
                .map_err(io_failure)?;
                if let Some(presentation) = &request.presentation {
                    bytes.extend(serde_json::to_vec(presentation).map_err(io_failure)?);
                }
                if let Some(references) = &request.context_references {
                    validate_explicit_references(references)?;
                    bytes.extend(serde_json::to_vec(references).map_err(io_failure)?);
                }
                if let Some(boundary) = request.turn_boundary {
                    bytes.extend(serde_json::to_vec(&boundary).map_err(io_failure)?);
                }
                Ok(bytes)
            }
        }
    }

    pub(super) fn prepare(&self, session: &mut Session) -> Result<PreparedInput, IpcFailure> {
        match self {
            Self::Document(request) => {
                let store = require_bound_store(session, &request.project_id, &request.session_id)?;
                let source = read_source(
                    store,
                    &request.document_id,
                    &request.source_revision_id,
                    &request.expected_visible_blob_id,
                )?;
                let input = input_range(
                    &source.text,
                    request.source_start_byte,
                    request.source_end_byte,
                )?
                .to_owned();
                let entry = if request.expression.is_empty() {
                    input.clone()
                } else {
                    request.expression.clone()
                };
                Ok(PreparedInput {
                    source,
                    input,
                    entry,
                    presentation: request.presentation.clone(),
                    context_references: request.context_references.clone(),
                    turn_boundary: request.turn_boundary,
                    configuration: None,
                    capture_request: None,
                })
            }
            Self::Workspace(request) => prepare_workspace(session, request),
        }
    }
}

fn read_source(
    store: &ProjectStore,
    id: &str,
    revision: &str,
    blob: &str,
) -> Result<LoadedDocument, IpcFailure> {
    let id = id.parse::<DocumentId>().map_err(io_failure)?;
    let summary = store
        .registered_document(id)
        .map_err(IpcFailure::store)?
        .ok_or_else(|| failure("The source document is no longer available."))?;
    let source = store
        .read_document(&summary.relative_path)
        .map_err(IpcFailure::store)?;
    if source.revision_id.to_string() != revision || source.blob_id.to_string() != blob {
        return Err(failure(
            "The source changed before the experiment began. Try again.",
        ));
    }
    Ok(source)
}

fn prepare_workspace(
    session: &mut Session,
    request: &WorkspacePaneRequest,
) -> Result<PreparedInput, IpcFailure> {
    let store = crate::workspace_owner::require_store_mut(
        session,
        &request.workspace_id,
        &request.workspace_session_id,
    )?;
    let source = crate::workspace_template::load_template(store)?
        .ok_or_else(|| failure("Enable the workspace template before running a pane."))?;
    if source.revision_id.to_string() != request.configuration_revision_id {
        return Err(failure(
            "The pane configuration changed. Read the workspace again before running.",
        ));
    }
    let config = crate::workspace_template::parse_config(&source.text).map_err(failure)?;
    let pane = config
        .panes
        .get(&request.pane_id)
        .filter(|pane| {
            config.panes_enabled && pane.kind != crate::workspace_template::PaneKind::Editor
        })
        .ok_or_else(|| failure("This workspace has no executable pane with that name."))?;
    let mut references = Vec::new();
    for reference in pane.context.iter().chain(&pane.document) {
        let reference = document_references(reference).map_err(io_failure)?;
        let name = reference
            .first()
            .ok_or_else(|| failure("The pane context reference is empty."))?
            .name
            .clone();
        if name == "document" && request.captured_document.is_none() {
            return Err(failure("This pane requires a captured active document."));
        }
        references.push(name);
    }
    validate_explicit_references(&references)?;
    let is_chat = pane.kind == crate::workspace_template::PaneKind::Chat;
    let is_expression = matches!(
        parse_neural_command(&request.expression).map_err(io_failure)?,
        NeuralCommand::Expression(_)
    );
    if is_chat && is_expression {
        return Err(failure("A chat pane needs a plain prompt."));
    }
    let configuration = material_context::ReadContext::from(&*store)
        .resolve(crate::workspace_template::TEMPLATE_PATH)?;
    Ok(PreparedInput {
        source,
        input: request.input.clone(),
        entry: request.expression.clone(),
        presentation: Some(TerminalPresentation {
            pane_id: request.pane_id.clone(),
            input: request.input.clone(),
        }),
        context_references: (!is_expression).then_some(references),
        turn_boundary: is_chat.then_some(TerminalTurnBoundary::Chat),
        configuration: Some(configuration),
        capture_request: request.captured_document.clone(),
    })
}

pub(super) fn capture_document(
    session: &mut Session,
    capture: Option<&CapturedDocument>,
) -> Result<(Value, Vec<llama_native_types::MediaInput>), IpcFailure> {
    let capture =
        capture.ok_or_else(|| failure("This run requires a captured active document."))?;
    let store = require_bound_store(session, &capture.project_id, &capture.session_id)?;
    let source = read_source(
        store,
        &capture.document_id,
        &capture.revision_id,
        &capture.visible_blob_id,
    )?;
    let context = material_context::ReadContext::from(&*store);
    let value = context.resolve(&source.relative_path)?;
    let media = context.native_media([&value])?;
    Ok((value, media))
}
