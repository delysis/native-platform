#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::{Mutex, TryLockError};

use loom_backend_llama::{GgufHeaderStatus, ModelDiscoveryOptions, discover_gguf_models};
use loom_document::{DocumentContent, MergeError, MergeOutcome, three_way_merge};
use loom_host::AgencyGate;
use loom_store::{
    DocumentReconciliationSnapshot, ExternalReconciliationOutcome, ExternalReconciliationRequest,
    IdempotentSaveOutcome, LoadedDocument, ProjectStore, TransientDraft, VisibleProjectionState,
};
use loom_types::{BlobId, CommandId, CommandReceipt, DocumentKind, RevisionId, now_unix_ms};
use serde::Serialize;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::{AppHandle, Manager, Runtime, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;

const INITIAL_DOCUMENT: &str = "manuscript/001-opening.md";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum SessionPhase {
    #[default]
    Closed,
    Choosing,
    Open,
}

#[derive(Debug, Default)]
struct Session {
    phase: SessionPhase,
    store: Option<ProjectStore>,
    active_session_id: Option<CommandId>,
    agency: AgencyGate,
    last_close: Option<ProjectCloseReceipt>,
}

#[derive(Debug, Default)]
pub struct PluginState {
    session: Mutex<Session>,
}

#[derive(Debug, Default)]
pub struct Builder;

impl Builder {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub fn build<R: Runtime>(self) -> TauriPlugin<R> {
        PluginBuilder::new("loom")
            .invoke_handler(tauri::generate_handler![
                project_choose_create,
                project_choose_open,
                project_close,
                project_current,
                project_recover,
                document_open,
                document_checkpoint,
                document_draft_upsert,
                document_draft_clear,
                document_reconciliation_preview,
                document_reconcile_apply,
                model_list,
                focus_mode_set,
                application_close,
            ])
            .setup(|app, _api| {
                app.manage(PluginState::default());
                Ok(())
            })
            .build()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct IpcFailure {
    code: &'static str,
    message: String,
    retryable: bool,
}

impl IpcFailure {
    fn new(code: &'static str, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    fn store(error: loom_store::StoreError) -> Self {
        use loom_store::StoreError;

        let code = match &error {
            StoreError::Io(_) => "filesystem_error",
            StoreError::Sqlite(_) => "database_error",
            StoreError::Json(_) => "manifest_json_error",
            StoreError::Document(_) => "document_projection_error",
            StoreError::NonUtf8Path(_) => "non_utf8_path",
            StoreError::UnsafeRelativePath(_) => "unsafe_relative_path",
            StoreError::SymbolicLink(_) => "symbolic_link_refused",
            StoreError::NotDirectory(_) => "not_a_directory",
            StoreError::NotRegularFile(_) => "not_a_regular_file",
            StoreError::AlreadyInitialized(_) => "project_already_initialized",
            StoreError::NotAProject(_) => "not_a_loom_project",
            StoreError::UnsupportedFormat(_) => "unsupported_project_format",
            StoreError::UnsupportedSchema { .. } => "unsupported_project_schema",
            StoreError::InvalidProjectName { .. } => "invalid_project_name",
            StoreError::ReasonTooLong { .. } => "checkpoint_reason_too_long",
            StoreError::DocumentTooLarge { .. } => "document_too_large",
            StoreError::DocumentKindMismatch { .. } => "document_kind_mismatch",
            StoreError::VisibleFileConflict { .. } => "visible_file_conflict",
            StoreError::VisibleFileAlreadyExists(_) => "visible_file_already_exists",
            StoreError::DocumentAlreadyExists(_) => "document_already_exists",
            StoreError::UncheckpointedVisibleChange(_) => "external_file_change",
            StoreError::ExternalVisibleFileDeleted(_) => "external_file_deleted",
            StoreError::ExternalVisibleBlobMismatch { .. } => "external_file_conflict",
            StoreError::ExternalVisibleInvalidUtf8(_) => "external_file_invalid_utf8",
            StoreError::MissingBlob { .. } => "missing_content_blob",
            StoreError::UnregisteredBlob(_) => "unregistered_content_blob",
            StoreError::CorruptBlob { .. } => "corrupt_content_blob",
            StoreError::CorruptDatabase(_) => "corrupt_project_database",
            StoreError::NoActiveRevision(_) => "no_active_revision",
            StoreError::SourceRevisionMismatch { .. } => "source_revision_conflict",
            StoreError::SourceBlobMismatch { .. } => "source_blob_conflict",
            StoreError::ArtifactKindMismatch { .. } => "artifact_kind_mismatch",
            StoreError::GenerationRunNotFound(_) => "generation_run_not_found",
            StoreError::CandidateNotFound(_) => "candidate_not_found",
            StoreError::GenerationAlreadyTerminal(_) => "generation_already_terminal",
            StoreError::CompletedGenerationRequiresCandidate => {
                "generation_terminal_candidate_required"
            }
            StoreError::CandidateReadyRequiresTerminalCandidate => {
                "candidate_ready_requires_terminal"
            }
            StoreError::FailedGenerationRequiresError => "failed_generation_requires_error",
            StoreError::CriticCannotPromote => "critic_cannot_promote",
            StoreError::ModelRoleNotAssigned => "model_role_not_assigned",
            StoreError::InvalidAuthorityPolicy => "invalid_authority_policy",
            StoreError::InvalidGenerationRange => "invalid_generation_range",
            StoreError::NonCanonicalGeneratedText => "noncanonical_generated_text",
            StoreError::IdempotencyConflict { .. } => "idempotency_conflict",
            StoreError::ProvenancePayloadTooLarge { .. } => "provenance_payload_too_large",
            StoreError::EditDiffBudgetExceeded { .. } => "edit_diff_budget_exceeded",
            StoreError::RevisionSegmentLimitExceeded { .. } => "revision_segment_limit_exceeded",
            StoreError::TransientDraftVersionConflict { .. } => "transient_draft_version_conflict",
            StoreError::TransientDraftIdentityMismatch { .. } => {
                "transient_draft_identity_mismatch"
            }
        };
        Self::new(code, error.to_string(), false)
    }

    fn merge(error: &MergeError) -> Self {
        let code = match error {
            MergeError::HybridMetadataRequired => "hybrid_reconciliation_unsupported",
            MergeError::BudgetExceeded { .. } => "merge_budget_exceeded",
            MergeError::RangeTooLarge => "merge_range_too_large",
            MergeError::InvalidEditScript => "merge_invalid_edit_script",
        };
        Self::new(code, error.to_string(), false)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ProjectSnapshot {
    project_id: String,
    session_id: String,
    title: String,
    root: String,
    schema_version: u32,
    documents: Vec<DocumentSummary>,
    pending_recovery: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProjectCloseReceipt {
    command_id: String,
    project_id: String,
    session_id: String,
    closed_at_unix_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct DocumentSummary {
    document_id: String,
    relative_path: String,
    title: String,
    kind: DocumentKind,
    revision_id: Option<String>,
    active_blob_id: Option<String>,
    word_count: usize,
    externally_modified: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct OpenDocument {
    summary: DocumentSummary,
    visible_blob_id: String,
    text: String,
    transient_draft: Option<TransientDraftSnapshot>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TransientDraftSnapshot {
    document_id: String,
    source_revision_id: String,
    blob_id: String,
    version: String,
    kind: DocumentKind,
    text: String,
    updated_at_unix_ms: i64,
    replayed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct TransientDraftWriteReceipt {
    document_id: String,
    source_revision_id: String,
    blob_id: String,
    version: String,
    kind: DocumentKind,
    updated_at_unix_ms: i64,
    replayed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationAppSource {
    Caller,
    TransientDraft,
    Base,
}

/// Exact, untruncated inputs and immutable identities for one bounded merge
/// preview. `external_text` is the canonical merge input;
/// `external_visible_text` is the exact UTF-8 text currently on disk and is
/// the value bound by `external_visible_blob_id`.
#[derive(Clone, Debug, Serialize)]
pub struct ReconciliationPreview {
    project_id: String,
    session_id: String,
    document_id: String,
    relative_path: String,
    kind: DocumentKind,
    active_revision_id: String,
    active_artifact_id: String,
    base_blob_id: String,
    app_blob_id: String,
    external_blob_id: String,
    external_visible_blob_id: String,
    base_text: String,
    app_text: String,
    external_text: String,
    external_visible_text: String,
    app_source: ReconciliationAppSource,
    draft_version: Option<String>,
    outcome: MergeOutcome,
}

#[derive(Clone, Debug, Serialize)]
pub struct Receipt {
    command_id: String,
    command_kind: String,
    project_id: String,
    schema_version: u32,
    source_revision_id: Option<String>,
    result_revision_id: Option<String>,
    result_blob_id: Option<String>,
    request_fingerprint: Option<String>,
    replayed: bool,
    visible_projection: Option<VisibleProjectionState>,
    artifact_ids: Vec<String>,
    completed_at_unix_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct RecoveryReport {
    recovered: usize,
    conflicts: Vec<String>,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Serialize)]
pub struct ModelCapabilitySummary {
    model_id: String,
    display_name: String,
    local: bool,
    loaded: bool,
    completion: bool,
    fill_in_middle: bool,
    output_tokens: bool,
    logprobs: bool,
    model_path: String,
    file_bytes: u64,
    header_verified: bool,
}

#[tauri::command]
async fn project_choose_create<R: Runtime>(
    title: String,
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<ProjectSnapshot, IpcFailure> {
    reserve_project_choice(&state)?;
    let result = choose_project_folder(&app).and_then(|path| initialize_project(&path, title));
    finish_project_choice(&state, result)
}

fn reserve_project_choice(state: &State<'_, PluginState>) -> Result<(), IpcFailure> {
    let mut session = lock_session(state)?;
    if session.phase != SessionPhase::Closed {
        return Err(IpcFailure::new(
            "project_session_active",
            "close the current project or folder chooser before opening another",
            false,
        ));
    }
    session.phase = SessionPhase::Choosing;
    Ok(())
}

fn finish_project_choice(
    state: &State<'_, PluginState>,
    result: Result<ProjectStore, IpcFailure>,
) -> Result<ProjectSnapshot, IpcFailure> {
    let store = match result {
        Ok(store) => store,
        Err(error) => {
            release_project_choice(state)?;
            return Err(error);
        }
    };
    let session_id = CommandId::new();
    let snapshot = match snapshot_for(&store, session_id) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            release_project_choice(state)?;
            return Err(error);
        }
    };
    let mut session = lock_session_internal(state)?;
    if session.phase != SessionPhase::Choosing {
        return Err(IpcFailure::new(
            "project_choice_state_changed",
            "the project chooser lost its reserved session",
            false,
        ));
    }
    session.store = Some(store);
    session.active_session_id = Some(session_id);
    session.agency = AgencyGate::default();
    session.phase = SessionPhase::Open;
    Ok(snapshot)
}

fn release_project_choice(state: &State<'_, PluginState>) -> Result<(), IpcFailure> {
    let mut session = lock_session_internal(state)?;
    if session.phase == SessionPhase::Choosing {
        session.phase = SessionPhase::Closed;
    }
    Ok(())
}

fn choose_project_folder<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, IpcFailure> {
    let selected = app.dialog().file().blocking_pick_folder().ok_or_else(|| {
        IpcFailure::new(
            "folder_selection_cancelled",
            "no project folder was selected",
            false,
        )
    })?;
    selected.into_path().map_err(|error| {
        IpcFailure::new(
            "selected_folder_unavailable",
            format!("the selected folder is not a local filesystem path: {error}"),
            false,
        )
    })
}

fn initialize_project(path: &Path, title: String) -> Result<ProjectStore, IpcFailure> {
    let existing_initial = path.join(INITIAL_DOCUMENT);
    match std::fs::symlink_metadata(&existing_initial) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(IpcFailure::new(
                "initial_document_symlink",
                "the default manuscript path is a symbolic link; import it explicitly instead",
                false,
            ));
        }
        Ok(metadata) if metadata.is_file() => {
            return Err(IpcFailure::new(
                "existing_manuscript_requires_import",
                "the default manuscript file already exists; import the folder instead of creating an empty project",
                false,
            ));
        }
        Ok(_) => {
            return Err(IpcFailure::new(
                "initial_document_not_file",
                "the default manuscript path already exists and is not a regular file",
                false,
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(IpcFailure::new(
                "initial_document_inspection_failed",
                format!("could not inspect the default manuscript path: {error}"),
                false,
            ));
        }
    }
    let (mut store, _receipt) = ProjectStore::initialize(path, title).map_err(IpcFailure::store)?;
    store
        .create_document_if_absent(
            INITIAL_DOCUMENT,
            DocumentContent::Prose(String::new()),
            "initial manuscript",
        )
        .map_err(IpcFailure::store)?;
    Ok(store)
}

#[tauri::command]
async fn project_choose_open<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<ProjectSnapshot, IpcFailure> {
    reserve_project_choice(&state)?;
    let result = choose_project_folder(&app).and_then(|path| {
        let mut store = ProjectStore::open(path).map_err(IpcFailure::store)?;
        store.record_open().map_err(IpcFailure::store)?;
        Ok(store)
    });
    finish_project_choice(&state, result)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn project_close(
    project_id: String,
    session_id: String,
    command_id: String,
    state: State<'_, PluginState>,
) -> Result<ProjectCloseReceipt, IpcFailure> {
    let command_id = command_id.parse::<CommandId>().map_err(|_| {
        IpcFailure::new(
            "invalid_command_id",
            "close command ID is not a valid ULID",
            false,
        )
    })?;
    let mut session = lock_session(&state)?;
    if session.phase == SessionPhase::Closed {
        if let Some(receipt) = &session.last_close
            && receipt.command_id == command_id.to_string()
            && receipt.project_id == project_id
            && receipt.session_id == session_id
        {
            return Ok(receipt.clone());
        }
        return Err(IpcFailure::new(
            "project_not_open",
            "the requested project session is not open",
            false,
        ));
    }
    {
        require_bound_store(&mut session, &project_id, &session_id)?;
    }
    let receipt = ProjectCloseReceipt {
        command_id: command_id.to_string(),
        project_id,
        session_id,
        closed_at_unix_ms: now_unix_ms(),
    };
    session.store = None;
    session.active_session_id = None;
    session.agency = AgencyGate::default();
    session.phase = SessionPhase::Closed;
    session.last_close = Some(receipt.clone());
    Ok(receipt)
}

#[tauri::command]
async fn project_current(state: State<'_, PluginState>) -> Result<ProjectSnapshot, IpcFailure> {
    let session = lock_session(&state)?;
    if session.phase != SessionPhase::Open {
        return Err(IpcFailure::new(
            "project_not_open",
            "there is no live native project session to reattach",
            false,
        ));
    }
    let session_id = session.active_session_id.ok_or_else(|| {
        IpcFailure::new(
            "corrupt_project_session",
            "the live project session is missing its session ID",
            false,
        )
    })?;
    let store = session.store.as_ref().ok_or_else(|| {
        IpcFailure::new(
            "corrupt_project_session",
            "the live project session is missing its store",
            false,
        )
    })?;
    snapshot_for(store, session_id)
}

#[tauri::command]
async fn project_recover(
    project_id: String,
    session_id: String,
    state: State<'_, PluginState>,
) -> Result<RecoveryReport, IpcFailure> {
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    let report = store.recover().map_err(IpcFailure::store)?;
    Ok(RecoveryReport {
        recovered: report.applied + report.already_applied,
        conflicts: report
            .conflicts
            .into_iter()
            .map(|conflict| conflict.relative_path)
            .collect(),
    })
}

#[tauri::command]
async fn document_open(
    project_id: String,
    session_id: String,
    document_id: String,
    relative_path: String,
    state: State<'_, PluginState>,
) -> Result<OpenDocument, IpcFailure> {
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    let document = store
        .read_document(&relative_path)
        .map_err(IpcFailure::store)?;
    ensure_document_id(&document, &document_id)?;
    let mut draft = store
        .load_transient_draft(&relative_path)
        .map_err(IpcFailure::store)?;
    if let Some(existing) = &draft
        && existing.document_id == document.document_id
        && existing.kind == document.kind
        && existing.blob_id == document.blob_id
    {
        store
            .clear_transient_draft(&relative_path, existing.version)
            .map_err(IpcFailure::store)?;
        draft = None;
    }
    Ok(open_document_from(document, draft))
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn document_checkpoint(
    project_id: String,
    session_id: String,
    document_id: String,
    relative_path: String,
    text: String,
    kind: DocumentKind,
    expected_revision_id: Option<String>,
    expected_visible_blob_id: String,
    command_id: String,
    draft_version: Option<String>,
    state: State<'_, PluginState>,
) -> Result<Receipt, IpcFailure> {
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    let stored_document = store
        .list_documents()
        .map_err(IpcFailure::store)?
        .into_iter()
        .find(|candidate| candidate.relative_path == relative_path)
        .ok_or_else(|| {
            IpcFailure::new(
                "document_not_found",
                "the requested document is not registered in this project",
                false,
            )
        })?;
    ensure_document_identity(&stored_document.document_id.to_string(), &document_id)?;
    if kind == DocumentKind::Hybrid {
        return Err(IpcFailure::new(
            "hybrid_edit_unsupported",
            "hybrid editing is locked until block metadata can be preserved losslessly",
            false,
        ));
    }
    let expected_revision_id = expected_revision_id
        .ok_or_else(|| {
            IpcFailure::new(
                "revision_required",
                "checkpoint refused because the editor did not name its source revision",
                false,
            )
        })?
        .parse()
        .map_err(|_| {
            IpcFailure::new(
                "invalid_revision_id",
                "source revision ID is invalid",
                false,
            )
        })?;
    let expected_visible_blob_id = expected_visible_blob_id.parse::<BlobId>().map_err(|_| {
        IpcFailure::new(
            "invalid_blob_id",
            "source visible-blob ID is invalid",
            false,
        )
    })?;
    let command_id = command_id.parse::<CommandId>().map_err(|_| {
        IpcFailure::new(
            "invalid_command_id",
            "checkpoint command ID is not a valid ULID",
            false,
        )
    })?;
    let content = DocumentContent::from_visible(kind, text.into_bytes())
        .map_err(|error| IpcFailure::new("invalid_document", error.to_string(), false))?;
    let draft_version = parse_checkpoint_draft_version(draft_version)?;
    let outcome = if let Some(draft_version) = draft_version {
        store.save_document_if_source_idempotent_consuming_draft(
            command_id,
            relative_path,
            content,
            "editor idle checkpoint",
            expected_revision_id,
            expected_visible_blob_id,
            draft_version,
        )
    } else {
        store.save_document_if_source_idempotent(
            command_id,
            relative_path,
            content,
            "editor idle checkpoint",
            expected_revision_id,
            expected_visible_blob_id,
        )
    }
    .map_err(IpcFailure::store)?;
    Ok(Receipt::from(outcome))
}

fn parse_checkpoint_draft_version(
    draft_version: Option<String>,
) -> Result<Option<u64>, IpcFailure> {
    draft_version
        .map(|version| {
            version.parse::<u64>().map_err(|_| {
                IpcFailure::new(
                    "invalid_draft_version",
                    "draft version is not an unsigned decimal integer",
                    false,
                )
            })
        })
        .transpose()
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn document_draft_upsert(
    project_id: String,
    session_id: String,
    document_id: String,
    relative_path: String,
    text: String,
    kind: DocumentKind,
    source_revision_id: String,
    expected_version: String,
    state: State<'_, PluginState>,
) -> Result<TransientDraftWriteReceipt, IpcFailure> {
    if kind == DocumentKind::Hybrid {
        return Err(IpcFailure::new(
            "hybrid_edit_unsupported",
            "hybrid drafts are locked until block metadata can be preserved losslessly",
            false,
        ));
    }
    let source_revision_id = source_revision_id.parse::<RevisionId>().map_err(|_| {
        IpcFailure::new(
            "invalid_revision_id",
            "draft source revision ID is invalid",
            false,
        )
    })?;
    let expected_version = expected_version.parse::<u64>().map_err(|_| {
        IpcFailure::new(
            "invalid_draft_version",
            "draft version is not an unsigned decimal integer",
            false,
        )
    })?;
    let content = DocumentContent::from_visible(kind, text.into_bytes())
        .map_err(|error| IpcFailure::new("invalid_document", error.to_string(), false))?;
    let canonical_text = String::from_utf8(
        content
            .project_visible()
            .map_err(|error| IpcFailure::new("invalid_document", error.to_string(), false))?
            .bytes,
    )
    .map_err(|error| IpcFailure::new("invalid_document", error.to_string(), false))?;
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    ensure_registered_document(store, &relative_path, &document_id)?;
    match store.upsert_transient_draft(
        &relative_path,
        source_revision_id,
        expected_version,
        content,
    ) {
        Ok(outcome) => Ok(transient_draft_write_receipt(
            &outcome.draft,
            outcome.replayed,
        )),
        Err(loom_store::StoreError::TransientDraftVersionConflict { .. }) => {
            let existing = store
                .load_transient_draft(&relative_path)
                .map_err(IpcFailure::store)?;
            match existing {
                Some(draft)
                    if draft.document_id.to_string() == document_id
                        && draft.source_revision_id == source_revision_id
                        && draft.kind == kind
                        && draft.text == canonical_text =>
                {
                    Ok(transient_draft_write_receipt(&draft, true))
                }
                _ => Err(IpcFailure::new(
                    "transient_draft_version_conflict",
                    "a newer transient draft exists; reload it before writing",
                    false,
                )),
            }
        }
        Err(error) => Err(IpcFailure::store(error)),
    }
}

#[tauri::command]
async fn document_draft_clear(
    project_id: String,
    session_id: String,
    document_id: String,
    relative_path: String,
    expected_version: String,
    state: State<'_, PluginState>,
) -> Result<bool, IpcFailure> {
    let expected_version = expected_version.parse::<u64>().map_err(|_| {
        IpcFailure::new(
            "invalid_draft_version",
            "draft version is not an unsigned decimal integer",
            false,
        )
    })?;
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    ensure_registered_document(store, &relative_path, &document_id)?;
    match store.clear_transient_draft(&relative_path, expected_version) {
        Ok(cleared) => Ok(cleared),
        Err(loom_store::StoreError::TransientDraftVersionConflict { .. }) => {
            if store
                .load_transient_draft(&relative_path)
                .map_err(IpcFailure::store)?
                .is_none()
            {
                Ok(true)
            } else {
                Err(IpcFailure::new(
                    "transient_draft_version_conflict",
                    "a newer transient draft exists and was not cleared",
                    false,
                ))
            }
        }
        Err(error) => Err(IpcFailure::store(error)),
    }
}

#[derive(Debug)]
struct PreviewRequest {
    project_id: String,
    session_id: String,
    document_id: String,
    relative_path: String,
    expected_revision_id: RevisionId,
    expected_base_blob_id: BlobId,
    app_text: Option<String>,
}

#[derive(Debug)]
struct ApplyRequest {
    document_id: String,
    relative_path: String,
    expected_revision_id: RevisionId,
    expected_base_blob_id: BlobId,
    expected_visible_blob_id: BlobId,
    resolved_content: DocumentContent,
    reason: String,
    command_id: CommandId,
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn document_reconciliation_preview(
    project_id: String,
    session_id: String,
    document_id: String,
    relative_path: String,
    expected_revision_id: String,
    expected_base_blob_id: String,
    app_text: Option<String>,
    state: State<'_, PluginState>,
) -> Result<ReconciliationPreview, IpcFailure> {
    let expected_revision_id = expected_revision_id.parse::<RevisionId>().map_err(|_| {
        IpcFailure::new(
            "invalid_revision_id",
            "reconciliation source revision ID is invalid",
            false,
        )
    })?;
    let expected_base_blob_id = expected_base_blob_id.parse::<BlobId>().map_err(|_| {
        IpcFailure::new(
            "invalid_blob_id",
            "reconciliation base-blob ID is invalid",
            false,
        )
    })?;
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    reconciliation_preview_for_store(
        store,
        PreviewRequest {
            project_id,
            session_id,
            document_id,
            relative_path,
            expected_revision_id,
            expected_base_blob_id,
            app_text,
        },
    )
}

fn reconciliation_preview_for_store(
    store: &ProjectStore,
    request: PreviewRequest,
) -> Result<ReconciliationPreview, IpcFailure> {
    if store.manifest().project_id.to_string() != request.project_id {
        return Err(IpcFailure::new(
            "project_identity_mismatch",
            "this reconciliation preview does not belong to the open project",
            false,
        ));
    }
    let snapshot = store
        .reconciliation_snapshot(&request.relative_path)
        .map_err(IpcFailure::store)?;
    validate_preview_identity(&snapshot, &request)?;
    if snapshot.kind == DocumentKind::Hybrid {
        return Err(IpcFailure::new(
            "hybrid_reconciliation_unsupported",
            "hybrid reconciliation requires lossless block metadata and is not available yet",
            false,
        ));
    }
    let visible = snapshot.visible.as_ref().ok_or_else(|| {
        IpcFailure::new(
            "external_file_deleted",
            "the externally edited document was deleted; restore or import it before reconciling",
            false,
        )
    })?;
    if snapshot.visible_matches_active {
        return Err(IpcFailure::new(
            "external_file_unchanged",
            "the visible document still matches the active revision",
            false,
        ));
    }
    ensure_text_hash(
        &snapshot.base_text,
        snapshot.active_blob_id,
        "base_blob_identity_mismatch",
        "the immutable reconciliation base does not match its content identity",
    )?;
    ensure_text_hash(
        &visible.text,
        visible.blob_id,
        "external_file_conflict",
        "the external reconciliation snapshot changed while it was being read",
    )?;

    let draft = store
        .load_transient_draft(&request.relative_path)
        .map_err(IpcFailure::store)?;
    if let Some(draft) = &draft {
        validate_current_draft(draft, &snapshot)?;
    }
    let draft_version = draft.as_ref().map(|draft| draft.version.to_string());
    let (app_candidate, app_source) = match request.app_text {
        Some(text) => (text, ReconciliationAppSource::Caller),
        None => match &draft {
            Some(draft) => (draft.text.clone(), ReconciliationAppSource::TransientDraft),
            None => (snapshot.base_text.clone(), ReconciliationAppSource::Base),
        },
    };

    let base_text = canonical_visible_text(snapshot.kind, &snapshot.base_text)?;
    if base_text != snapshot.base_text {
        return Err(IpcFailure::new(
            "noncanonical_base_document",
            "the immutable base is not canonical for its registered document kind",
            false,
        ));
    }
    let app_text = canonical_visible_text(snapshot.kind, &app_candidate)?;
    let external_visible_text = visible.text.clone();
    let external_text = canonical_visible_text(snapshot.kind, &external_visible_text)?;
    let outcome = three_way_merge(snapshot.kind, &base_text, &app_text, &external_text)
        .map_err(|error| IpcFailure::merge(&error))?;

    Ok(ReconciliationPreview {
        project_id: request.project_id,
        session_id: request.session_id,
        document_id: snapshot.document_id.to_string(),
        relative_path: snapshot.relative_path,
        kind: snapshot.kind,
        active_revision_id: snapshot.active_revision_id.to_string(),
        active_artifact_id: snapshot.active_artifact_id.to_string(),
        base_blob_id: snapshot.active_blob_id.to_string(),
        app_blob_id: BlobId::digest(app_text.as_bytes()).to_string(),
        external_blob_id: BlobId::digest(external_text.as_bytes()).to_string(),
        external_visible_blob_id: visible.blob_id.to_string(),
        base_text,
        app_text,
        external_text,
        external_visible_text,
        app_source,
        draft_version,
        outcome,
    })
}

fn validate_preview_identity(
    snapshot: &DocumentReconciliationSnapshot,
    request: &PreviewRequest,
) -> Result<(), IpcFailure> {
    ensure_document_identity(&snapshot.document_id.to_string(), &request.document_id)?;
    if snapshot.relative_path != request.relative_path {
        return Err(IpcFailure::new(
            "document_path_identity_mismatch",
            "the requested path is not the registered document path",
            false,
        ));
    }
    if snapshot.active_revision_id != request.expected_revision_id {
        return Err(IpcFailure::new(
            "source_revision_conflict",
            "the active revision changed before reconciliation preview",
            false,
        ));
    }
    if snapshot.active_blob_id != request.expected_base_blob_id {
        return Err(IpcFailure::new(
            "source_blob_conflict",
            "the active base blob changed before reconciliation preview",
            false,
        ));
    }
    Ok(())
}

fn validate_current_draft(
    draft: &TransientDraft,
    snapshot: &DocumentReconciliationSnapshot,
) -> Result<(), IpcFailure> {
    if draft.document_id != snapshot.document_id
        || draft.source_revision_id != snapshot.active_revision_id
        || draft.kind != snapshot.kind
        || draft.blob_id != BlobId::digest(draft.text.as_bytes())
    {
        return Err(IpcFailure::new(
            "stale_transient_draft",
            "the recoverable draft is not based on the active reconciliation source",
            false,
        ));
    }
    Ok(())
}

fn ensure_text_hash(
    text: &str,
    expected: BlobId,
    code: &'static str,
    message: &'static str,
) -> Result<(), IpcFailure> {
    if BlobId::digest(text.as_bytes()) != expected {
        return Err(IpcFailure::new(code, message, false));
    }
    Ok(())
}

fn canonical_visible_text(kind: DocumentKind, text: &str) -> Result<String, IpcFailure> {
    if kind == DocumentKind::Hybrid {
        return Err(IpcFailure::new(
            "hybrid_reconciliation_unsupported",
            "hybrid reconciliation requires lossless block metadata and is not available yet",
            false,
        ));
    }
    let content = DocumentContent::from_visible(kind, text.as_bytes().to_vec())
        .map_err(|error| IpcFailure::new("invalid_document", error.to_string(), false))?;
    let bytes = content
        .project_visible()
        .map_err(|error| IpcFailure::new("invalid_document", error.to_string(), false))?
        .bytes;
    String::from_utf8(bytes)
        .map_err(|error| IpcFailure::new("invalid_document", error.to_string(), false))
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn document_reconcile_apply(
    project_id: String,
    session_id: String,
    document_id: String,
    relative_path: String,
    expected_revision_id: String,
    expected_base_blob_id: String,
    expected_external_visible_blob_id: String,
    resolved_text: String,
    kind: DocumentKind,
    reason: String,
    command_id: String,
    state: State<'_, PluginState>,
) -> Result<Receipt, IpcFailure> {
    if kind == DocumentKind::Hybrid {
        return Err(IpcFailure::new(
            "hybrid_reconciliation_unsupported",
            "hybrid reconciliation requires lossless block metadata and is not available yet",
            false,
        ));
    }
    let expected_revision_id = expected_revision_id.parse::<RevisionId>().map_err(|_| {
        IpcFailure::new(
            "invalid_revision_id",
            "reconciliation source revision ID is invalid",
            false,
        )
    })?;
    let expected_base_blob_id = expected_base_blob_id.parse::<BlobId>().map_err(|_| {
        IpcFailure::new(
            "invalid_blob_id",
            "reconciliation base-blob ID is invalid",
            false,
        )
    })?;
    let expected_visible_blob_id = expected_external_visible_blob_id
        .parse::<BlobId>()
        .map_err(|_| {
            IpcFailure::new(
                "invalid_blob_id",
                "external visible-blob ID is invalid",
                false,
            )
        })?;
    let command_id = command_id.parse::<CommandId>().map_err(|_| {
        IpcFailure::new(
            "invalid_command_id",
            "reconciliation command ID is not a valid ULID",
            false,
        )
    })?;
    let content = DocumentContent::from_visible(kind, resolved_text.into_bytes())
        .map_err(|error| IpcFailure::new("invalid_document", error.to_string(), false))?;

    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    reconcile_apply_for_store(
        store,
        ApplyRequest {
            document_id,
            relative_path,
            expected_revision_id,
            expected_base_blob_id,
            expected_visible_blob_id,
            resolved_content: content,
            reason,
            command_id,
        },
    )
}

fn reconcile_apply_for_store(
    store: &mut ProjectStore,
    request: ApplyRequest,
) -> Result<Receipt, IpcFailure> {
    if request.resolved_content.kind() == DocumentKind::Hybrid {
        return Err(IpcFailure::new(
            "hybrid_reconciliation_unsupported",
            "hybrid reconciliation requires lossless block metadata and is not available yet",
            false,
        ));
    }
    let registered_kind =
        registered_document_kind(store, &request.relative_path, &request.document_id)?;
    if registered_kind != request.resolved_content.kind() {
        return Err(IpcFailure::new(
            "document_kind_mismatch",
            "the reconciliation kind does not match the registered document",
            false,
        ));
    }
    let outcome = store
        .reconcile_external_idempotent(
            request.command_id,
            ExternalReconciliationRequest {
                relative_path: request.relative_path,
                expected_active_revision_id: request.expected_revision_id,
                expected_base_blob_id: request.expected_base_blob_id,
                expected_visible_blob_id: request.expected_visible_blob_id,
                resolved_content: request.resolved_content,
                reason: request.reason,
            },
        )
        .map_err(IpcFailure::store)?;
    Ok(Receipt::from(outcome))
}

#[tauri::command]
async fn model_list() -> Result<Vec<ModelCapabilitySummary>, IpcFailure> {
    let mut options = ModelDiscoveryOptions::default();
    options.max_entries = options.max_entries.min(20_000);
    options.max_depth = options.max_depth.min(12);
    let report = discover_gguf_models(&options)
        .map_err(|error| IpcFailure::new("model_discovery_error", error.to_string(), false))?;
    Ok(report
        .models
        .into_iter()
        .map(|model| {
            let model_path = model.resolved_path.to_string_lossy().into_owned();
            let display_name = model
                .selected_path
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or("Local GGUF")
                .to_owned();
            ModelCapabilitySummary {
                model_id: format!("discovered:{}", BlobId::digest(model_path.as_bytes())),
                display_name,
                local: true,
                loaded: false,
                // A GGUF header proves only the container. Completion and
                // token capabilities stay unavailable until native inspection.
                completion: false,
                fill_in_middle: false,
                output_tokens: false,
                logprobs: false,
                model_path,
                file_bytes: model.file_bytes,
                header_verified: matches!(model.header, GgufHeaderStatus::Verified),
            }
        })
        .collect())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn application_close<R: Runtime>(
    window: WebviewWindow<R>,
    state: State<'_, PluginState>,
) -> Result<(), IpcFailure> {
    let session = lock_session(&state)?;
    if session.phase != SessionPhase::Closed {
        return Err(IpcFailure::new(
            "project_must_close_first",
            "Loom refuses to close the window while a project session is active",
            false,
        ));
    }
    drop(session);
    window.destroy().map_err(|error| {
        IpcFailure::new(
            "window_close_failed",
            format!("the native Loom window could not close: {error}"),
            true,
        )
    })
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn focus_mode_set(
    project_id: String,
    session_id: String,
    enabled: bool,
    state: State<'_, PluginState>,
) -> Result<(), IpcFailure> {
    let mut session = lock_session(&state)?;
    require_bound_store(&mut session, &project_id, &session_id)?;
    session.agency.set_focus_mode(enabled);
    Ok(())
}

fn snapshot_for(
    store: &ProjectStore,
    session_id: CommandId,
) -> Result<ProjectSnapshot, IpcFailure> {
    let documents = store
        .list_documents()
        .map_err(IpcFailure::store)?
        .into_iter()
        .map(|summary| -> Result<DocumentSummary, IpcFailure> {
            let (active_blob_id, word_count, externally_modified) =
                if summary.active_revision_id.is_some() {
                    let reconciliation = store
                        .reconciliation_snapshot(&summary.relative_path)
                        .map_err(IpcFailure::store)?;
                    let word_count = reconciliation
                        .visible
                        .as_ref()
                        .map_or(0, |visible| count_words(&visible.text));
                    (
                        Some(reconciliation.active_blob_id.to_string()),
                        word_count,
                        !reconciliation.visible_matches_active,
                    )
                } else {
                    (None, 0, false)
                };
            Ok(DocumentSummary {
                document_id: summary.document_id.to_string(),
                title: title_for_path(&summary.relative_path),
                relative_path: summary.relative_path,
                kind: summary.kind,
                revision_id: summary.active_revision_id.map(|id| id.to_string()),
                active_blob_id,
                word_count,
                externally_modified,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let root = store
        .root()
        .to_str()
        .ok_or_else(|| {
            IpcFailure::new(
                "non_utf8_project_path",
                "project path is not valid UTF-8",
                false,
            )
        })?
        .to_owned();
    Ok(ProjectSnapshot {
        project_id: store.manifest().project_id.to_string(),
        session_id: session_id.to_string(),
        title: store.manifest().name.clone(),
        root,
        schema_version: store.manifest().schema_version,
        documents,
        pending_recovery: store.pending_outbox_count().map_err(IpcFailure::store)?,
    })
}

fn open_document_from(document: LoadedDocument, draft: Option<TransientDraft>) -> OpenDocument {
    let word_count = count_words(&document.text);
    OpenDocument {
        visible_blob_id: document.blob_id.to_string(),
        summary: DocumentSummary {
            document_id: document.document_id.to_string(),
            title: title_for_path(&document.relative_path),
            relative_path: document.relative_path,
            kind: document.kind,
            revision_id: Some(document.revision_id.to_string()),
            active_blob_id: Some(document.blob_id.to_string()),
            word_count,
            externally_modified: false,
        },
        text: document.text,
        transient_draft: draft.map(|draft| transient_draft_snapshot(draft, false)),
    }
}

fn transient_draft_snapshot(draft: TransientDraft, replayed: bool) -> TransientDraftSnapshot {
    TransientDraftSnapshot {
        document_id: draft.document_id.to_string(),
        source_revision_id: draft.source_revision_id.to_string(),
        blob_id: draft.blob_id.to_string(),
        version: draft.version.to_string(),
        kind: draft.kind,
        text: draft.text,
        updated_at_unix_ms: draft.updated_at_ms,
        replayed,
    }
}

fn transient_draft_write_receipt(
    draft: &TransientDraft,
    replayed: bool,
) -> TransientDraftWriteReceipt {
    TransientDraftWriteReceipt {
        document_id: draft.document_id.to_string(),
        source_revision_id: draft.source_revision_id.to_string(),
        blob_id: draft.blob_id.to_string(),
        version: draft.version.to_string(),
        kind: draft.kind,
        updated_at_unix_ms: draft.updated_at_ms,
        replayed,
    }
}

fn ensure_document_id(document: &LoadedDocument, expected: &str) -> Result<(), IpcFailure> {
    ensure_document_identity(&document.document_id.to_string(), expected)
}

fn ensure_document_identity(actual: &str, expected: &str) -> Result<(), IpcFailure> {
    if actual != expected {
        return Err(IpcFailure::new(
            "document_identity_mismatch",
            "the document identity does not match the authorized project entry",
            false,
        ));
    }
    Ok(())
}

fn ensure_registered_document(
    store: &ProjectStore,
    relative_path: &str,
    document_id: &str,
) -> Result<(), IpcFailure> {
    registered_document_kind(store, relative_path, document_id).map(|_| ())
}

fn registered_document_kind(
    store: &ProjectStore,
    relative_path: &str,
    document_id: &str,
) -> Result<DocumentKind, IpcFailure> {
    let stored_document = store
        .list_documents()
        .map_err(IpcFailure::store)?
        .into_iter()
        .find(|candidate| candidate.relative_path == relative_path)
        .ok_or_else(|| {
            IpcFailure::new(
                "document_not_found",
                "the requested document is not registered in this project",
                false,
            )
        })?;
    ensure_document_identity(&stored_document.document_id.to_string(), document_id)?;
    Ok(stored_document.kind)
}

fn lock_session<'a>(
    state: &'a State<'_, PluginState>,
) -> Result<std::sync::MutexGuard<'a, Session>, IpcFailure> {
    state.session.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => IpcFailure::new(
            "project_busy",
            "another bounded project operation is still running; retry shortly",
            true,
        ),
        TryLockError::Poisoned(_) => IpcFailure::new(
            "session_poisoned",
            "the project session entered an invalid state; restart Loom",
            false,
        ),
    })
}

fn lock_session_internal<'a>(
    state: &'a State<'_, PluginState>,
) -> Result<std::sync::MutexGuard<'a, Session>, IpcFailure> {
    state.session.lock().map_err(|_| {
        IpcFailure::new(
            "session_poisoned",
            "the project session entered an invalid state; restart Loom",
            false,
        )
    })
}

fn require_bound_store<'a>(
    session: &'a mut Session,
    project_id: &str,
    session_id: &str,
) -> Result<&'a mut ProjectStore, IpcFailure> {
    if session.phase != SessionPhase::Open {
        return Err(IpcFailure::new(
            "project_not_open",
            "open a Loom project first",
            false,
        ));
    }
    let active_session_id = session
        .active_session_id
        .ok_or_else(|| IpcFailure::new("project_not_open", "open a Loom project first", false))?;
    if active_session_id.to_string() != session_id {
        return Err(IpcFailure::new(
            "stale_project_session",
            "this command belongs to an expired project session",
            false,
        ));
    }
    let store = session
        .store
        .as_mut()
        .ok_or_else(|| IpcFailure::new("project_not_open", "open a Loom project first", false))?;
    if store.manifest().project_id.to_string() != project_id {
        return Err(IpcFailure::new(
            "project_identity_mismatch",
            "this command does not belong to the open project",
            false,
        ));
    }
    Ok(store)
}

fn title_for_path(path: &str) -> String {
    PathBuf::from(path)
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .map(|stem| stem.replace(['-', '_'], " "))
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| Path::new(path).to_string_lossy().into_owned())
}

fn count_words(text: &str) -> usize {
    text.split_whitespace().count()
}

impl From<CommandReceipt> for Receipt {
    fn from(receipt: CommandReceipt) -> Self {
        Self {
            command_id: receipt.command_id.to_string(),
            command_kind: receipt.command.as_str().to_owned(),
            project_id: receipt.project_id.to_string(),
            schema_version: receipt.project_schema_version,
            source_revision_id: receipt.source_revision_id.map(|id| id.to_string()),
            result_revision_id: receipt
                .resulting_revision_ids
                .last()
                .map(ToString::to_string),
            result_blob_id: None,
            request_fingerprint: None,
            replayed: false,
            visible_projection: None,
            artifact_ids: receipt
                .resulting_artifact_ids
                .into_iter()
                .map(|id| id.to_string())
                .collect(),
            completed_at_unix_ms: receipt.completed_at_ms,
        }
    }
}

impl From<IdempotentSaveOutcome> for Receipt {
    fn from(outcome: IdempotentSaveOutcome) -> Self {
        let result_blob_id = outcome.save.blob_id.to_string();
        let request_fingerprint = outcome.request_fingerprint.to_string();
        let replayed = outcome.replayed;
        let visible_projection = outcome.visible_projection;
        let mut receipt = Self::from(outcome.save.receipt);
        receipt.result_blob_id = Some(result_blob_id);
        receipt.request_fingerprint = Some(request_fingerprint);
        receipt.replayed = replayed;
        receipt.visible_projection = Some(visible_projection);
        receipt
    }
}

impl From<ExternalReconciliationOutcome> for Receipt {
    fn from(outcome: ExternalReconciliationOutcome) -> Self {
        let result_blob_id = outcome.save.blob_id.to_string();
        let request_fingerprint = outcome.request_fingerprint.to_string();
        let replayed = outcome.replayed;
        let visible_projection = outcome.visible_projection;
        let mut receipt = Self::from(outcome.save.receipt);
        receipt.result_blob_id = Some(result_blob_id);
        receipt.request_fingerprint = Some(request_fingerprint);
        receipt.replayed = replayed;
        receipt.visible_projection = Some(visible_projection);
        receipt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ReconciliationFixture {
        _temporary: tempfile::TempDir,
        store: ProjectStore,
        base: LoadedDocument,
        project_id: String,
        session_id: String,
    }

    impl ReconciliationFixture {
        fn new(base_text: &str) -> Self {
            let temporary = tempfile::tempdir().expect("temporary project parent");
            let root = temporary.path().join("Reconciliation Novel");
            let mut store =
                initialize_project(&root, "Reconciliation Novel".to_owned()).expect("initialize");
            let initial = store
                .read_document(INITIAL_DOCUMENT)
                .expect("read initial document");
            if !base_text.is_empty() {
                store
                    .save_document_if_source(
                        INITIAL_DOCUMENT,
                        DocumentContent::Prose(base_text.to_owned()),
                        "establish reconciliation base",
                        initial.revision_id,
                        initial.blob_id,
                    )
                    .expect("save reconciliation base");
            }
            let base = store
                .read_document(INITIAL_DOCUMENT)
                .expect("read reconciliation base");
            let project_id = store.manifest().project_id.to_string();
            Self {
                _temporary: temporary,
                store,
                base,
                project_id,
                session_id: CommandId::new().to_string(),
            }
        }

        fn set_external(&self, text: &str) -> BlobId {
            std::fs::write(self.store.root().join(INITIAL_DOCUMENT), text)
                .expect("write external document");
            BlobId::digest(text.as_bytes())
        }

        fn preview_request(&self, app_text: Option<&str>) -> PreviewRequest {
            PreviewRequest {
                project_id: self.project_id.clone(),
                session_id: self.session_id.clone(),
                document_id: self.base.document_id.to_string(),
                relative_path: INITIAL_DOCUMENT.to_owned(),
                expected_revision_id: self.base.revision_id,
                expected_base_blob_id: self.base.blob_id,
                app_text: app_text.map(str::to_owned),
            }
        }

        fn apply_request(
            &self,
            external_blob_id: BlobId,
            resolved_text: &str,
            command_id: CommandId,
        ) -> ApplyRequest {
            ApplyRequest {
                document_id: self.base.document_id.to_string(),
                relative_path: INITIAL_DOCUMENT.to_owned(),
                expected_revision_id: self.base.revision_id,
                expected_base_blob_id: self.base.blob_id,
                expected_visible_blob_id: external_blob_id,
                resolved_content: DocumentContent::Prose(resolved_text.to_owned()),
                reason: "author resolved external edit".to_owned(),
                command_id,
            }
        }
    }

    #[test]
    fn title_is_readable_without_changing_path_identity() {
        assert_eq!(title_for_path("manuscript/001-opening.md"), "001 opening");
    }

    #[test]
    fn word_count_handles_whitespace_without_normalizing_document() {
        assert_eq!(count_words("first\n  second\tthird"), 3);
    }

    #[test]
    fn project_creation_refuses_existing_default_manuscript_without_touching_it() {
        let temporary = tempfile::tempdir().expect("temporary parent");
        let root = temporary.path().join("Existing Novel");
        let manuscript = root.join(INITIAL_DOCUMENT);
        std::fs::create_dir_all(manuscript.parent().expect("manuscript parent"))
            .expect("create manuscript parent");
        let original = "Already written.\n\nStill here.\n";
        std::fs::write(&manuscript, original).expect("write existing manuscript");

        let error = initialize_project(&root, "Existing Novel".to_owned())
            .expect_err("creation must refuse an ambiguous existing manuscript");

        assert_eq!(error.code, "existing_manuscript_requires_import");
        assert_eq!(
            std::fs::read_to_string(&manuscript).expect("read visible manuscript"),
            original
        );
        assert!(!root.join(".loom").exists());
    }

    #[test]
    fn project_commands_reject_stale_session_and_cross_project_identity() {
        let temporary = tempfile::tempdir().expect("temporary parent");
        let root = temporary.path().join("Bound Novel");
        let store = initialize_project(&root, "Bound Novel".to_owned()).expect("initialize");
        let project_id = store.manifest().project_id.to_string();
        let session_id = CommandId::new();
        let mut session = Session {
            phase: SessionPhase::Open,
            store: Some(store),
            active_session_id: Some(session_id),
            agency: AgencyGate::default(),
            last_close: None,
        };

        assert!(require_bound_store(&mut session, &project_id, &session_id.to_string()).is_ok());
        let stale = require_bound_store(&mut session, &project_id, &CommandId::new().to_string())
            .expect_err("stale session must fail");
        assert_eq!(stale.code, "stale_project_session");
        let foreign = require_bound_store(
            &mut session,
            &loom_types::ProjectId::new().to_string(),
            &session_id.to_string(),
        )
        .expect_err("foreign project must fail");
        assert_eq!(foreign.code, "project_identity_mismatch");
    }

    #[test]
    fn checkpoint_draft_version_preserves_decimal_u64_and_allows_lost_first_reply() {
        let first = parse_checkpoint_draft_version(Some("0".to_owned()))
            .expect("parse lost first acknowledgement")
            .expect("draft version");
        assert_eq!(first, 0);

        let maximum = parse_checkpoint_draft_version(Some(u64::MAX.to_string()))
            .expect("parse maximum u64")
            .expect("maximum version");
        assert_eq!(maximum, u64::MAX);
        assert!(parse_checkpoint_draft_version(Some("-1".to_owned())).is_err());
        assert_eq!(
            parse_checkpoint_draft_version(None).expect("no draft claim"),
            None
        );
    }

    #[test]
    fn project_summary_keeps_active_identity_across_external_change_and_deletion() {
        let fixture = ReconciliationFixture::new("one two\n");
        fixture.set_external("one two three\n");

        let changed = snapshot_for(&fixture.store, CommandId::new()).expect("changed snapshot");
        let summary = &changed.documents[0];
        assert_eq!(
            summary.active_blob_id.as_deref(),
            Some(fixture.base.blob_id.to_string().as_str())
        );
        assert_eq!(
            summary.revision_id.as_deref(),
            Some(fixture.base.revision_id.to_string().as_str())
        );
        assert_eq!(summary.word_count, 3);
        assert!(summary.externally_modified);
        let serialized = serde_json::to_value(summary).expect("serialize document summary");
        assert_eq!(
            serialized
                .get("active_blob_id")
                .and_then(|value| value.as_str()),
            Some(fixture.base.blob_id.to_string().as_str())
        );
        assert!(serialized.get("activeBlobId").is_none());

        std::fs::remove_file(fixture.store.root().join(INITIAL_DOCUMENT))
            .expect("delete visible document");
        let deleted = snapshot_for(&fixture.store, CommandId::new()).expect("deleted snapshot");
        let summary = &deleted.documents[0];
        assert_eq!(
            summary.active_blob_id.as_deref(),
            Some(fixture.base.blob_id.to_string().as_str())
        );
        assert_eq!(summary.word_count, 0);
        assert!(summary.externally_modified);
    }

    #[test]
    fn reconciliation_preview_returns_exact_canonical_inputs_and_bound_hashes() {
        let fixture = ReconciliationFixture::new("alpha\nmiddle\nomega\n");
        let external_visible = "alpha\r\nmiddle\r\nOMEGA\r\n";
        let external_visible_blob_id = fixture.set_external(external_visible);

        let preview = reconciliation_preview_for_store(
            &fixture.store,
            fixture.preview_request(Some("ALPHA\nmiddle\nomega\n")),
        )
        .expect("preview non-overlapping edits");

        assert_eq!(preview.project_id, fixture.project_id);
        assert_eq!(preview.session_id, fixture.session_id);
        assert_eq!(preview.document_id, fixture.base.document_id.to_string());
        assert_eq!(
            preview.active_revision_id,
            fixture.base.revision_id.to_string()
        );
        assert_eq!(preview.base_blob_id, fixture.base.blob_id.to_string());
        assert_eq!(preview.base_text, "alpha\nmiddle\nomega\n");
        assert_eq!(preview.app_text, "ALPHA\nmiddle\nomega\n");
        assert_eq!(preview.external_visible_text, external_visible);
        assert_eq!(preview.external_text, "alpha\nmiddle\nOMEGA\n");
        assert_eq!(
            preview.external_visible_blob_id,
            external_visible_blob_id.to_string()
        );
        assert_eq!(
            preview.external_blob_id,
            BlobId::digest(preview.external_text.as_bytes()).to_string()
        );
        assert_eq!(preview.app_source, ReconciliationAppSource::Caller);
        assert_eq!(preview.draft_version, None);
        assert_eq!(
            preview.outcome,
            MergeOutcome::Merged {
                content: "ALPHA\nmiddle\nOMEGA\n".to_owned()
            }
        );
        let serialized = serde_json::to_value(&preview).expect("serialize preview contract");
        assert_eq!(
            serialized
                .get("project_id")
                .and_then(|value| value.as_str()),
            Some(fixture.project_id.as_str())
        );
        assert_eq!(
            serialized
                .get("app_source")
                .and_then(|value| value.as_str()),
            Some("caller")
        );
        assert!(serialized.get("projectId").is_none());
        assert!(serialized.get("appSource").is_none());
    }

    #[test]
    fn reconciliation_preview_rejects_deleted_hybrid_and_unbound_inputs() {
        let fixture = ReconciliationFixture::new("bound base\n");
        fixture.set_external("bound external\n");

        let mut wrong_revision = fixture.preview_request(None);
        wrong_revision.expected_revision_id = RevisionId::new();
        assert_eq!(
            reconciliation_preview_for_store(&fixture.store, wrong_revision)
                .expect_err("wrong revision must fail")
                .code,
            "source_revision_conflict"
        );

        let mut unsafe_path = fixture.preview_request(None);
        unsafe_path.relative_path = "../outside.md".to_owned();
        assert_eq!(
            reconciliation_preview_for_store(&fixture.store, unsafe_path)
                .expect_err("path authority must not expand")
                .code,
            "unsafe_relative_path"
        );

        std::fs::remove_file(fixture.store.root().join(INITIAL_DOCUMENT))
            .expect("delete external document");
        assert_eq!(
            reconciliation_preview_for_store(&fixture.store, fixture.preview_request(None))
                .expect_err("deleted external document must fail")
                .code,
            "external_file_deleted"
        );

        let temporary = tempfile::tempdir().expect("hybrid project parent");
        let root = temporary.path().join("Hybrid Novel");
        let mut store = initialize_project(&root, "Hybrid Novel".to_owned()).expect("initialize");
        store
            .create_document_if_absent(
                "manuscript/mixed.md",
                DocumentContent::Hybrid(vec![loom_document::HybridBlock {
                    kind: loom_document::HybridBlockKind::Prose,
                    text: "mixed base\n".to_owned(),
                }]),
                "create hybrid fixture",
            )
            .expect("create hybrid document");
        let hybrid = store
            .read_document("manuscript/mixed.md")
            .expect("read hybrid document");
        std::fs::write(root.join("manuscript/mixed.md"), "mixed external\n")
            .expect("write hybrid external edit");
        let project_id = store.manifest().project_id.to_string();
        let error = reconciliation_preview_for_store(
            &store,
            PreviewRequest {
                project_id,
                session_id: CommandId::new().to_string(),
                document_id: hybrid.document_id.to_string(),
                relative_path: "manuscript/mixed.md".to_owned(),
                expected_revision_id: hybrid.revision_id,
                expected_base_blob_id: hybrid.blob_id,
                app_text: None,
            },
        )
        .expect_err("hybrid reconciliation must fail closed");
        assert_eq!(error.code, "hybrid_reconciliation_unsupported");
    }

    #[test]
    fn preview_uses_current_draft_and_reports_conflicts_without_writing() {
        let mut fixture = ReconciliationFixture::new("dawn over water\n");
        let draft = fixture
            .store
            .upsert_transient_draft(
                INITIAL_DOCUMENT,
                fixture.base.revision_id,
                0,
                DocumentContent::Prose("winter over water\n".to_owned()),
            )
            .expect("write current draft")
            .draft;
        let external = "summer over water\n";
        fixture.set_external(external);

        let preview =
            reconciliation_preview_for_store(&fixture.store, fixture.preview_request(None))
                .expect("preview competing edits");

        assert_eq!(preview.app_source, ReconciliationAppSource::TransientDraft);
        assert_eq!(preview.draft_version, Some(draft.version.to_string()));
        assert!(matches!(
            preview.outcome,
            MergeOutcome::Conflict { ref conflicts } if !conflicts.is_empty()
        ));
        assert_eq!(
            std::fs::read_to_string(fixture.store.root().join(INITIAL_DOCUMENT))
                .expect("read untouched external file"),
            external
        );
        assert_eq!(
            fixture
                .store
                .reconciliation_snapshot(INITIAL_DOCUMENT)
                .expect("snapshot after preview")
                .active_revision_id,
            fixture.base.revision_id
        );
        assert_eq!(
            fixture
                .store
                .load_transient_draft(INITIAL_DOCUMENT)
                .expect("load draft after preview")
                .expect("draft remains")
                .version,
            draft.version
        );
    }

    #[test]
    fn preview_rejects_a_draft_from_an_old_active_revision() {
        let mut fixture = ReconciliationFixture::new("first base\n");
        fixture
            .store
            .upsert_transient_draft(
                INITIAL_DOCUMENT,
                fixture.base.revision_id,
                0,
                DocumentContent::Prose("old-source draft\n".to_owned()),
            )
            .expect("write draft");
        fixture
            .store
            .save_document_if_source(
                INITIAL_DOCUMENT,
                DocumentContent::Prose("new active base\n".to_owned()),
                "advance active revision",
                fixture.base.revision_id,
                fixture.base.blob_id,
            )
            .expect("advance revision without clearing draft");
        fixture.base = fixture
            .store
            .read_document(INITIAL_DOCUMENT)
            .expect("load new active revision");
        fixture.set_external("external against new base\n");

        let error = reconciliation_preview_for_store(&fixture.store, fixture.preview_request(None))
            .expect_err("stale draft must block reconciliation preview");

        assert_eq!(error.code, "stale_transient_draft");
    }

    #[test]
    fn reconciliation_apply_is_replay_safe_and_never_clears_the_draft() {
        let mut fixture = ReconciliationFixture::new("base manuscript\n");
        let draft = fixture
            .store
            .upsert_transient_draft(
                INITIAL_DOCUMENT,
                fixture.base.revision_id,
                0,
                DocumentContent::Prose("recoverable app draft\n".to_owned()),
            )
            .expect("write recoverable draft")
            .draft;
        let external_blob_id = fixture.set_external("external manuscript\n");
        let command_id = CommandId::new();
        let wrong_hash_request = fixture.apply_request(
            BlobId::digest(b"not the external document"),
            "author resolved manuscript\n",
            CommandId::new(),
        );
        let wrong_hash = reconcile_apply_for_store(&mut fixture.store, wrong_hash_request)
            .expect_err("apply must bind the exact external visible hash");
        assert_eq!(wrong_hash.code, "external_file_conflict");
        assert_eq!(
            std::fs::read_to_string(fixture.store.root().join(INITIAL_DOCUMENT))
                .expect("read external file after rejected apply"),
            "external manuscript\n"
        );

        let first_request =
            fixture.apply_request(external_blob_id, "author resolved manuscript\n", command_id);
        let first = reconcile_apply_for_store(&mut fixture.store, first_request)
            .expect("apply explicit resolution");

        assert_eq!(first.command_kind, "reconcile_external");
        assert!(!first.replayed);
        assert_eq!(
            first.visible_projection,
            Some(VisibleProjectionState::Applied)
        );
        assert_eq!(
            fixture
                .store
                .read_document(INITIAL_DOCUMENT)
                .expect("read reconciled document")
                .text,
            "author resolved manuscript\n"
        );
        assert_eq!(
            fixture
                .store
                .load_transient_draft(INITIAL_DOCUMENT)
                .expect("load preserved draft")
                .expect("draft must remain explicit")
                .version,
            draft.version
        );

        let replay_request =
            fixture.apply_request(external_blob_id, "author resolved manuscript\n", command_id);
        let replay = reconcile_apply_for_store(&mut fixture.store, replay_request)
            .expect("replay a committed apply after a lost reply");
        assert!(replay.replayed);
        assert_eq!(
            replay.visible_projection,
            Some(VisibleProjectionState::Applied)
        );
        assert_eq!(replay.result_revision_id, first.result_revision_id);
        assert_eq!(replay.result_blob_id, first.result_blob_id);
        assert_eq!(
            fixture
                .store
                .load_transient_draft(INITIAL_DOCUMENT)
                .expect("load draft after replay")
                .expect("replay must not clear draft")
                .version,
            draft.version
        );
    }

    #[test]
    fn receipt_json_exposes_pending_projection_without_discarding_semantic_identity() {
        let receipt = Receipt {
            command_id: "01COMMAND".into(),
            command_kind: "checkpoint".into(),
            project_id: "01PROJECT".into(),
            schema_version: 4,
            source_revision_id: Some("01SOURCE".into()),
            result_revision_id: Some("01RESULT".into()),
            result_blob_id: Some("abc123".into()),
            request_fingerprint: Some("def456".into()),
            replayed: false,
            visible_projection: Some(VisibleProjectionState::PendingConflict {
                outbox_id: 17,
                relative_path: "manuscript/001-opening.md".into(),
            }),
            artifact_ids: vec!["01ARTIFACT".into()],
            completed_at_unix_ms: 42,
        };

        let value = serde_json::to_value(receipt).expect("serialize IPC receipt");
        assert_eq!(value["result_revision_id"], "01RESULT");
        assert_eq!(value["visible_projection"]["status"], "pending_conflict");
        assert_eq!(value["visible_projection"]["outbox_id"], 17);
        assert_eq!(
            value["visible_projection"]["relative_path"],
            "manuscript/001-opening.md"
        );
    }
}
