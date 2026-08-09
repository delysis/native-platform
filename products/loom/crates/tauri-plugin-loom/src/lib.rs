#![forbid(unsafe_code)]

mod model_download;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, TryLockError};
use std::time::Duration;

use loom_backend_llama::{
    ContinuationCase, DownloadControl, DownloadError, ExactContinuationRequest,
    ExactContinuationResult, GgufDownloadRequest, GgufHeaderStatus, LlamaBackend,
    LlamaBackendError, LlamaGenerationHandle, LocalModelProfile, MAX_MODEL_DOWNLOAD_BYTES,
    ModelDiscoveryOptions, SamplingConfig, Sha256Digest, VerifiedModelDescriptor,
    discover_gguf_models, download_gguf, model_environment_from_verified,
    validate_candidate_receipt_binding, validate_gguf_download_request,
};
use loom_document::{DocumentContent, MergeError, MergeOutcome, three_way_merge};
use loom_host::{
    AgencyGate, BranchCancellation, GenerationFamilyIdentity, GenerationRegistry,
    GenerationRegistryError,
};
use loom_store::{
    BranchPageCursor, DocumentReconciliationSnapshot, ExternalReconciliationOutcome,
    ExternalReconciliationRequest, IdempotentSaveOutcome, LoadedDocument, MAX_BRANCH_BODY_BYTES,
    ProjectStore, StoredBranchBody, StoredBranchRecord, StoredBranchStatus, StoredBranchSummary,
    TerminalCandidateInput, TerminalEvidenceInput, TerminalGenerationInput, TransientDraft,
    VisibleProjectionState,
};
use loom_types::{
    AuthorityPolicy, BlobId, BranchId, ByteRange, CancelGenerationCommand, CandidateId, CommandId,
    CommandReceipt, ContextRecipe, DocumentId, DocumentKind, GenerationEventKind, GenerationRunId,
    GenerationStart, GenerationTerminalStatus, LoomEvent, ModelEnvironment, ProjectId,
    PromoteCandidateCommand, PromptMode, PromptRecipe, RevisionId, SelectionDecision,
    derive_weave_case_ids, now_unix_ms,
};
use serde::{Deserialize, Serialize};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::{AppHandle, Emitter, Manager, Runtime, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;

use crate::model_download::{
    ModelDownloadRegistry, ModelDownloadRegistryError, ModelDownloadSnapshot, ModelDownloadSpec,
    ModelLibraryError, ReservationOutcome, model_target_path, prepare_model_library,
};

const INITIAL_DOCUMENT: &str = "manuscript/Untitled.md";
const DEFAULT_PROJECT_DIRECTORY: &str = "writing";
const PROJECT_CLOSE_GENERATION_WAIT: Duration = Duration::from_secs(3);
const TESTED_GEMMA_4_E2B_BASE_Q8_SHA256: &str =
    "aa0a9a03993440f45176f19f8189a2e84c210ff8628ec13dc6edf42d017f7670";
const TESTED_GEMMA_4_E2B_BASE_PROFILE: &str = "gemma_4_e2b_base_q8_loom_v1";
const MAX_MODEL_DOWNLOAD_URL_BYTES: usize = 16 * 1024;

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

#[derive(Debug)]
pub struct PluginState {
    session: Mutex<Session>,
    backend: Arc<LlamaBackend>,
    model: Mutex<ModelRegistry>,
    model_lifecycle: Mutex<()>,
    user_model_paths: Mutex<BTreeSet<PathBuf>>,
    generations: GenerationRegistry,
    downloads: Arc<ModelDownloadRegistry>,
    model_library_root: Option<PathBuf>,
}

impl Default for PluginState {
    fn default() -> Self {
        Self::with_model_library_root(None)
    }
}

impl PluginState {
    fn with_model_library_root(model_library_root: Option<PathBuf>) -> Self {
        Self {
            session: Mutex::new(Session::default()),
            backend: Arc::new(LlamaBackend::default()),
            model: Mutex::new(ModelRegistry::default()),
            model_lifecycle: Mutex::new(()),
            user_model_paths: Mutex::new(BTreeSet::new()),
            generations: GenerationRegistry::default(),
            downloads: Arc::new(ModelDownloadRegistry::default()),
            model_library_root,
        }
    }
}

#[derive(Clone, Debug)]
struct LoadedModel {
    profile: LocalModelProfile,
    descriptor: VerifiedModelDescriptor,
}

#[derive(Clone, Debug)]
struct GenerationResultBinding {
    exact_prompt_blob_id: BlobId,
    model_environment: ModelEnvironment,
    model: VerifiedModelDescriptor,
    generations: BTreeMap<GenerationRunId, GenerationStart>,
}

#[derive(Clone, Debug, Default)]
enum ModelRegistry {
    #[default]
    Empty,
    Loading {
        path: PathBuf,
        previous: Option<Box<LoadedModel>>,
    },
    Loaded(Box<LoadedModel>),
}

enum ModelLoadPlan {
    Ready(ModelCapabilitySummary),
    Inspect {
        canonical_path: PathBuf,
        profile: LocalModelProfile,
    },
}

struct PreparedModelDownload {
    command_id: CommandId,
    request: GgufDownloadRequest,
    spec: ModelDownloadSpec,
}

#[derive(Debug)]
struct LlamaCancellation {
    handle: Arc<LlamaGenerationHandle>,
}

impl BranchCancellation for LlamaCancellation {
    fn cancel_branch(&self, branch_id: BranchId) -> bool {
        self.handle.cancel_branch(branch_id)
    }
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
                project_open_default,
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
                model_choose,
                model_load,
                model_unload,
                model_download_start,
                model_download_cancel,
                model_download_status,
                model_download_list,
                branch_page,
                branch_get,
                branch_body,
                weave_status,
                weave_start,
                generation_cancel,
                candidate_keep,
                candidate_promote,
                suggestions_set,
                focus_mode_set,
                application_close,
            ])
            .setup(|app, _api| {
                let model_library_root = app.path().app_local_data_dir().ok();
                app.manage(PluginState::with_model_library_root(model_library_root));
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
            StoreError::ProjectAlreadyOpen(_) => "project_already_open",
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
            StoreError::ModelEnvironmentContentConflict { .. } => {
                "model_environment_content_conflict"
            }
            StoreError::GenerationRunNotFound(_) => "generation_run_not_found",
            StoreError::InvalidBranchPageLimit { .. } => "invalid_branch_page_limit",
            StoreError::InvalidBranchPageCursor => "invalid_branch_page_cursor",
            StoreError::InvalidBranchBodyLimit { .. } => "invalid_branch_body_limit",
            StoreError::BranchBodyTooLarge { .. } => "branch_body_too_large",
            StoreError::EmptyGenerationFamily => "empty_generation_family",
            StoreError::DuplicateGenerationRun(_) => "duplicate_generation_run",
            StoreError::DuplicateGenerationBranch(_) => "duplicate_generation_branch",
            StoreError::GenerationFamilySourceMismatch => "generation_family_source_mismatch",
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
        let retryable = matches!(error, StoreError::ProjectAlreadyOpen(_));
        Self::new(code, error.to_string(), retryable)
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

    fn backend(error: &LlamaBackendError) -> Self {
        let retryable = matches!(error, LlamaBackendError::ResultTimeout);
        Self::new("local_model_error", error.to_string(), retryable)
    }

    fn generation_registry(error: &GenerationRegistryError) -> Self {
        let retryable = matches!(error, GenerationRegistryError::CapacityExceeded { .. });
        Self::new("generation_lifecycle_error", error.to_string(), retryable)
    }

    fn model_download_registry(error: &ModelDownloadRegistryError) -> Self {
        let (code, retryable) = match error {
            ModelDownloadRegistryError::IdempotencyConflict { .. } => {
                ("model_download_idempotency_conflict", false)
            }
            ModelDownloadRegistryError::ActiveCapacity { .. }
            | ModelDownloadRegistryError::RetainedCapacity { .. } => {
                ("model_download_capacity", true)
            }
            ModelDownloadRegistryError::NotFound(_) => ("model_download_not_found", false),
            ModelDownloadRegistryError::AlreadyTerminal(_) => {
                ("model_download_already_terminal", false)
            }
            ModelDownloadRegistryError::Poisoned => ("model_download_state_error", false),
        };
        Self::new(code, error.to_string(), retryable)
    }

    fn model_library(error: &ModelLibraryError) -> Self {
        let (code, retryable) = match error {
            ModelLibraryError::InvalidFileName => ("invalid_model_file_name", false),
            ModelLibraryError::Symlink(_) => ("model_library_symlink_refused", false),
            ModelLibraryError::NotDirectory(_) => ("model_library_not_directory", false),
            ModelLibraryError::Io { .. } => ("model_library_io_error", true),
        };
        Self::new(code, error.to_string(), retryable)
    }

    fn model_download_request(error: &DownloadError) -> Self {
        Self::new("invalid_model_download", error.to_string(), false)
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
    chat: bool,
    completion: bool,
    fill_in_middle: bool,
    output_tokens: bool,
    logprobs: bool,
    model_path: String,
    file_bytes: u64,
    header_verified: bool,
    architecture: Option<String>,
    context_tokens: Option<u32>,
    model_sha256: Option<String>,
    projector_present: Option<bool>,
    media_kinds: Vec<&'static str>,
    tested_profile: Option<&'static str>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelUnloadOutcome {
    model_id: Option<String>,
    resident_slot_released: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct BranchSnapshot {
    run_id: String,
    branch_id: String,
    candidate_id: Option<String>,
    source_revision_id: String,
    target_start_byte: u64,
    target_end_byte: u64,
    text: String,
    output_blob_id: Option<String>,
    output_byte_len: Option<u64>,
    status: &'static str,
    seed: String,
    model_id: String,
    selection: Option<&'static str>,
    error: Option<String>,
    error_truncated: bool,
    created_at_unix_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BranchCursorSnapshot {
    /// Decimal u64, preserved as text across the JavaScript boundary.
    sequence: String,
    run_id: String,
}

impl TryFrom<BranchCursorSnapshot> for BranchPageCursor {
    type Error = IpcFailure;

    fn try_from(cursor: BranchCursorSnapshot) -> Result<Self, Self::Error> {
        let sequence = cursor.sequence.parse::<u64>().map_err(|_| {
            IpcFailure::new(
                "invalid_branch_page_cursor",
                "branch cursor sequence is not a decimal u64",
                false,
            )
        })?;
        let run_id = cursor.run_id.parse::<GenerationRunId>().map_err(|_| {
            IpcFailure::new(
                "invalid_branch_page_cursor",
                "branch cursor run ID is not a valid ULID",
                false,
            )
        })?;
        Ok(Self { sequence, run_id })
    }
}

impl From<BranchPageCursor> for BranchCursorSnapshot {
    fn from(cursor: BranchPageCursor) -> Self {
        Self {
            sequence: cursor.sequence.to_string(),
            run_id: cursor.run_id.to_string(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct BranchSummarySnapshot {
    run_id: String,
    branch_id: String,
    candidate_id: Option<String>,
    source_revision_id: String,
    target_start_byte: u64,
    target_end_byte: u64,
    output_blob_id: Option<String>,
    output_byte_len: Option<u64>,
    status: &'static str,
    seed: Option<String>,
    model_id: Option<String>,
    selection: Option<&'static str>,
    error: Option<String>,
    error_truncated: bool,
    created_at_unix_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct BranchPageSnapshot {
    branches: Vec<BranchSummarySnapshot>,
    next_cursor: Option<BranchCursorSnapshot>,
    has_more: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct BranchBodySnapshot {
    run_id: String,
    output_blob_id: String,
    byte_len: u64,
    text: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct WeaveStarted {
    command_id: String,
    request_id: String,
    project_id: String,
    session_id: String,
    document_id: String,
    source_revision_id: String,
    exact_prompt_blob_id: String,
    branches: Vec<BranchSnapshot>,
}

#[derive(Clone, Debug, Serialize)]
struct DesktopLoomEvent {
    project_id: String,
    session_id: String,
    document_id: String,
    request_id: String,
    event: LoomEvent,
}

/// Opens the app-owned writing folder, creating its first empty manuscript on
/// first launch. The folder is still an ordinary Loom project; this command
/// merely removes file-management ceremony from the default authoring path.
#[tauri::command]
async fn project_open_default<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<ProjectSnapshot, IpcFailure> {
    reserve_project_choice(&state)?;
    let result = app
        .path()
        .app_local_data_dir()
        .map_err(|error| {
            IpcFailure::new(
                "default_project_directory_unavailable",
                format!("the local writing directory is unavailable: {error}"),
                true,
            )
        })
        .and_then(|root| open_or_initialize_default_project(&root.join(DEFAULT_PROJECT_DIRECTORY)));
    finish_project_choice(&state, result)
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

fn choose_model_file<R: Runtime>(app: &AppHandle<R>) -> Result<Option<PathBuf>, IpcFailure> {
    app.dialog()
        .file()
        .add_filter("GGUF model", &["gguf"])
        .blocking_pick_file()
        .map(|selected| {
            selected.into_path().map_err(|error| {
                IpcFailure::new(
                    "selected_model_unavailable",
                    format!("the selected model is not a local filesystem path: {error}"),
                    false,
                )
            })
        })
        .transpose()
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

fn open_or_initialize_default_project(path: &Path) -> Result<ProjectStore, IpcFailure> {
    let manifest = path.join(".loom/project.json");
    let initialized = manifest.try_exists().map_err(|error| {
        IpcFailure::new(
            "default_project_inspection_failed",
            format!("the local writing folder could not be inspected: {error}"),
            true,
        )
    })?;
    let mut store = if initialized {
        ProjectStore::open(path).map_err(IpcFailure::store)?
    } else {
        validate_default_document_candidate(path)?;
        ProjectStore::initialize(path, "My Writing")
            .map(|(store, _)| store)
            .map_err(IpcFailure::store)?
    };

    // Settle an initialization/adoption transaction before deciding whether
    // the default document is absent. A registered document whose visible file
    // was later deleted is an external deletion, never permission to recreate
    // an empty file over the author's history.
    store.recover().map_err(IpcFailure::store)?;
    store
        .recover_interrupted_generations()
        .map_err(IpcFailure::store)?;
    ensure_default_document(&mut store)?;
    store.record_open().map_err(IpcFailure::store)?;
    Ok(store)
}

fn validate_default_document_candidate(path: &Path) -> Result<(), IpcFailure> {
    let visible = path.join(INITIAL_DOCUMENT);
    match std::fs::symlink_metadata(&visible) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(IpcFailure::new(
            "default_document_symlink",
            "Loom will not adopt an app-owned manuscript path that is a symbolic link",
            false,
        )),
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(IpcFailure::new(
            "default_document_not_file",
            "the app-owned manuscript path exists but is not a regular file",
            false,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(IpcFailure::new(
            "default_document_inspection_failed",
            format!("the app-owned manuscript path could not be inspected: {error}"),
            true,
        )),
    }
}

fn ensure_default_document(store: &mut ProjectStore) -> Result<(), IpcFailure> {
    if store
        .list_documents()
        .map_err(IpcFailure::store)?
        .iter()
        .any(|document| document.relative_path == INITIAL_DOCUMENT)
    {
        return Ok(());
    }

    let visible = store.root().join(INITIAL_DOCUMENT);
    match std::fs::symlink_metadata(&visible) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(IpcFailure::new(
            "default_document_symlink",
            "Loom will not adopt an app-owned manuscript path that is a symbolic link",
            false,
        )),
        Ok(metadata) if metadata.is_file() => {
            store
                .adopt_visible_document_if_absent(
                    INITIAL_DOCUMENT,
                    DocumentKind::Prose,
                    "recover existing app-owned manuscript",
                )
                .map_err(IpcFailure::store)?;
            Ok(())
        }
        Ok(_) => Err(IpcFailure::new(
            "default_document_not_file",
            "the app-owned manuscript path exists but is not a regular file",
            false,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            store
                .create_document_if_absent(
                    INITIAL_DOCUMENT,
                    DocumentContent::Prose(String::new()),
                    "initial manuscript",
                )
                .map_err(IpcFailure::store)?;
            Ok(())
        }
        Err(error) => Err(IpcFailure::new(
            "default_document_inspection_failed",
            format!("the app-owned manuscript path could not be inspected: {error}"),
            true,
        )),
    }
}

#[tauri::command]
async fn project_choose_open<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<ProjectSnapshot, IpcFailure> {
    reserve_project_choice(&state)?;
    let result = choose_project_folder(&app).and_then(|path| {
        let mut store = ProjectStore::open(path).map_err(IpcFailure::store)?;
        store
            .recover_interrupted_generations()
            .map_err(IpcFailure::store)?;
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
    close_project_with_wait(
        &state,
        project_id,
        session_id,
        command_id,
        PROJECT_CLOSE_GENERATION_WAIT,
    )
}

fn close_project_with_wait(
    state: &PluginState,
    project_id: String,
    session_id: String,
    command_id: CommandId,
    generation_wait: Duration,
) -> Result<ProjectCloseReceipt, IpcFailure> {
    let (typed_project_id, typed_session_id) = {
        let mut session = lock_session(state)?;
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
        require_bound_store(&mut session, &project_id, &session_id)?;
        let typed_project_id = session
            .store
            .as_ref()
            .ok_or_else(|| IpcFailure::new("project_not_open", "open a Loom project first", false))?
            .manifest()
            .project_id;
        let typed_session_id = session.active_session_id.ok_or_else(|| {
            IpcFailure::new(
                "corrupt_project_session",
                "the live project session is missing its session ID",
                false,
            )
        })?;
        // Admission and route reservation use this same session mutex. Once
        // these flags change, an admitted family is either already visible to
        // the registry below or it cannot reserve routes at all.
        session.agency.set_automation_enabled(false);
        session.agency.set_focus_mode(true);
        (typed_project_id, typed_session_id)
    };

    cancel_and_drain_generation_session(
        state,
        typed_project_id,
        typed_session_id,
        generation_wait,
    )?;

    let mut session = lock_session(state)?;
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
    require_bound_store(&mut session, &project_id, &session_id)?;
    if session.active_session_id != Some(typed_session_id)
        || session
            .store
            .as_ref()
            .is_none_or(|store| store.manifest().project_id != typed_project_id)
    {
        return Err(IpcFailure::new(
            "stale_project_session",
            "the project session changed while Loom cancelled its active strands",
            false,
        ));
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

fn cancel_and_drain_generation_session(
    state: &PluginState,
    project_id: ProjectId,
    session_id: CommandId,
    wait: Duration,
) -> Result<(), IpcFailure> {
    state
        .generations
        .cancel_session(project_id, session_id)
        .map_err(|error| IpcFailure::generation_registry(&error))?;
    let mut generation_idle = state
        .generations
        .wait_for_session_idle(project_id, session_id, wait)
        .map_err(|error| IpcFailure::generation_registry(&error))?;
    if !generation_idle {
        let failures = state
            .generations
            .terminal_persistence_failures(project_id, session_id)
            .map_err(|error| IpcFailure::generation_registry(&error))?;
        for failure in failures {
            terminalize_open_runs(state, &failure.identity, &failure.runs, &failure.error)
                .and_then(|_| {
                    release_family_after_terminal_persistence(
                        state,
                        &failure.identity,
                        &failure.runs,
                    )
                })
                .map_err(|error| {
                    IpcFailure::new(
                        "generation_terminal_persistence_failed",
                        format!(
                            "Loom could not preserve a terminal record before closing: {}",
                            error.message
                        ),
                        true,
                    )
                })?;
        }
        generation_idle = state
            .generations
            .wait_for_session_idle(project_id, session_id, Duration::ZERO)
            .map_err(|error| IpcFailure::generation_registry(&error))?;
    }
    if !generation_idle {
        return Err(IpcFailure::new(
            "generation_cancellation_in_progress",
            "Loom requested cancellation and is still preserving terminal generation evidence; retry the same close command shortly",
            true,
        ));
    }
    Ok(())
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
async fn model_list(
    state: State<'_, PluginState>,
) -> Result<Vec<ModelCapabilitySummary>, IpcFailure> {
    let loaded = {
        let registry = lock_model_registry(&state)?;
        match &*registry {
            ModelRegistry::Loaded(model) => Some(model.clone()),
            ModelRegistry::Loading { previous, .. } => previous.clone(),
            ModelRegistry::Empty => None,
        }
    };
    let options = desktop_model_discovery_options(&state)?;
    let report = discover_gguf_models(&options)
        .map_err(|error| IpcFailure::new("model_discovery_error", error.to_string(), false))?;
    let mut models = report
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
            if let Some(loaded) = loaded
                .as_ref()
                .filter(|loaded| loaded.profile.model_path == model.resolved_path)
            {
                return model_summary(loaded, true);
            }
            ModelCapabilitySummary {
                model_id: format!("discovered:{}", BlobId::digest(model_path.as_bytes())),
                display_name,
                local: true,
                loaded: false,
                chat: false,
                // A GGUF header proves only the container. Completion and
                // token capabilities stay unavailable until native inspection.
                completion: false,
                fill_in_middle: false,
                output_tokens: false,
                logprobs: false,
                model_path,
                file_bytes: model.file_bytes,
                header_verified: matches!(model.header, GgufHeaderStatus::Verified),
                architecture: None,
                context_tokens: None,
                model_sha256: None,
                projector_present: None,
                media_kinds: Vec::new(),
                tested_profile: None,
            }
        })
        .collect::<Vec<_>>();
    if let Some(loaded) = &loaded
        && !models.iter().any(|model| model.loaded)
    {
        models.push(model_summary(loaded, true));
    }
    models.sort_by(|left, right| {
        right
            .loaded
            .cmp(&left.loaded)
            .then_with(|| left.display_name.cmp(&right.display_name))
            .then_with(|| left.model_path.cmp(&right.model_path))
    });
    Ok(models)
}

#[tauri::command]
async fn model_choose<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<Option<ModelCapabilitySummary>, IpcFailure> {
    let Some(selected_path) = choose_model_file(&app)? else {
        return Ok(None);
    };
    let report = discover_gguf_models(&ModelDiscoveryOptions {
        hugging_face_cache_roots: Vec::new(),
        user_paths: vec![selected_path],
        max_entries: 1,
        max_depth: 1,
    })
    .map_err(|error| IpcFailure::new("model_discovery_error", error.to_string(), false))?;
    let selected = report.models.into_iter().next().ok_or_else(|| {
        IpcFailure::new(
            "selected_model_not_gguf",
            "the selected file is not a readable GGUF model",
            false,
        )
    })?;
    if !matches!(selected.header, GgufHeaderStatus::Verified) {
        return Err(IpcFailure::new(
            "model_header_unverified",
            "the selected file does not have a verified GGUF container header",
            false,
        ));
    }
    remember_user_model_path(&state, selected.resolved_path.clone())?;
    let canonical_path = selected.resolved_path.to_string_lossy().into_owned();
    model_list(state)
        .await?
        .into_iter()
        .find(|model| model.model_path == canonical_path)
        .map(Some)
        .ok_or_else(|| {
            IpcFailure::new(
                "selected_model_disappeared",
                "the selected model changed before Loom could add it to the local library",
                true,
            )
        })
}

#[tauri::command]
async fn model_load(
    model_path: String,
    state: State<'_, PluginState>,
) -> Result<ModelCapabilitySummary, IpcFailure> {
    let (canonical_path, profile) = match prepare_model_load(&model_path, &state)? {
        ModelLoadPlan::Ready(summary) => return Ok(summary),
        ModelLoadPlan::Inspect {
            canonical_path,
            profile,
        } => (canonical_path, profile),
    };
    let worker_profile = profile.clone();
    let backend = Arc::clone(&state.backend);
    let inspected =
        tauri::async_runtime::spawn_blocking(move || backend.inspect_model(&worker_profile))
            .await
            .map_err(|error| {
                IpcFailure::new(
                    "model_worker_failed",
                    format!("the local model verification worker stopped: {error}"),
                    true,
                )
            });
    let descriptor = resolve_model_inspection(&state, &canonical_path, &profile, inspected)?;
    commit_model_load(
        &state,
        &canonical_path,
        LoadedModel {
            profile,
            descriptor,
        },
    )
}

fn prepare_model_load(
    model_path: &str,
    state: &State<'_, PluginState>,
) -> Result<ModelLoadPlan, IpcFailure> {
    let requested = PathBuf::from(model_path);
    let canonical = requested.canonicalize().map_err(|error| {
        IpcFailure::new(
            "model_path_error",
            format!("the selected model path cannot be opened: {error}"),
            false,
        )
    })?;
    let discovered = discover_loadable_model(state, &canonical)?;
    let _lifecycle = lock_model_lifecycle(state)?;
    let mut registry = lock_model_registry(state)?;
    match &*registry {
        ModelRegistry::Loaded(loaded) if loaded.profile.model_path == canonical => {
            return Ok(ModelLoadPlan::Ready(model_summary(loaded, true)));
        }
        ModelRegistry::Loading { path, .. } => {
            return Err(IpcFailure::new(
                "model_load_in_progress",
                format!("Loom is already verifying {}", path.display()),
                true,
            ));
        }
        ModelRegistry::Loaded(_) | ModelRegistry::Empty => {}
    }
    ensure_no_active_generations(state, "switching local models")?;
    let previous = match std::mem::take(&mut *registry) {
        ModelRegistry::Loaded(previous) => Some(previous),
        ModelRegistry::Empty => None,
        ModelRegistry::Loading { .. } => {
            unreachable!("the loading state was rejected while holding the registry lock")
        }
    };
    *registry = ModelRegistry::Loading {
        path: canonical.clone(),
        previous,
    };
    Ok(ModelLoadPlan::Inspect {
        canonical_path: canonical,
        profile: LocalModelProfile::for_gguf(discovered.resolved_path),
    })
}

fn resolve_model_inspection(
    state: &State<'_, PluginState>,
    canonical_path: &Path,
    profile: &LocalModelProfile,
    inspected: Result<Result<VerifiedModelDescriptor, LlamaBackendError>, IpcFailure>,
) -> Result<VerifiedModelDescriptor, IpcFailure> {
    Ok(match inspected {
        Ok(Ok(descriptor)) => descriptor,
        Ok(Err(error)) => {
            release_staged_model(state, canonical_path, profile)?;
            return Err(IpcFailure::backend(&error));
        }
        Err(error) => {
            release_staged_model(state, canonical_path, profile)?;
            return Err(error);
        }
    })
}

fn commit_model_load(
    state: &State<'_, PluginState>,
    canonical_path: &Path,
    loaded: LoadedModel,
) -> Result<ModelCapabilitySummary, IpcFailure> {
    let summary = model_summary(&loaded, true);
    let mut registry = lock_model_registry(state)?;
    match std::mem::take(&mut *registry) {
        ModelRegistry::Loading { path, previous } if path == canonical_path => {
            if let Some(previous) = previous
                && let Err(error) = state.backend.release_model(&previous.profile)
            {
                let _ = state.backend.release_model(&loaded.profile);
                *registry = ModelRegistry::Loaded(previous);
                return Err(IpcFailure::new(
                    "model_release_failed",
                    format!("the previous local model could not be released safely: {error}"),
                    true,
                ));
            }
            *registry = ModelRegistry::Loaded(Box::new(loaded));
        }
        current => {
            *registry = current;
            let _ = state.backend.release_model(&loaded.profile);
            return Err(IpcFailure::new(
                "model_load_state_changed",
                "the selected model changed while native verification was running",
                true,
            ));
        }
    }
    Ok(summary)
}

#[tauri::command]
async fn model_unload(state: State<'_, PluginState>) -> Result<ModelUnloadOutcome, IpcFailure> {
    let _lifecycle = lock_model_lifecycle(&state)?;
    ensure_no_active_generations(&state, "unloading the local model")?;
    let profile = {
        let mut registry = lock_model_registry(&state)?;
        match std::mem::take(&mut *registry) {
            ModelRegistry::Empty => {
                return Ok(ModelUnloadOutcome {
                    model_id: None,
                    resident_slot_released: false,
                });
            }
            loading @ ModelRegistry::Loading { .. } => {
                *registry = loading;
                return Err(IpcFailure::new(
                    "model_load_in_progress",
                    "wait for local model verification to finish before unloading",
                    true,
                ));
            }
            ModelRegistry::Loaded(loaded) => {
                let profile = loaded.profile.clone();
                *registry = ModelRegistry::Loading {
                    path: profile.model_path.clone(),
                    previous: Some(loaded),
                };
                profile
            }
        }
    };
    let release = state.backend.release_model(&profile);
    let mut registry = lock_model_registry(&state)?;
    let current = std::mem::take(&mut *registry);
    let (path, previous) = match current {
        ModelRegistry::Loading {
            path,
            previous: Some(previous),
        } => (path, previous),
        current => {
            *registry = current;
            return Err(IpcFailure::new(
                "model_load_state_changed",
                "the selected model changed while native unload was running",
                true,
            ));
        }
    };
    if path != profile.model_path {
        *registry = ModelRegistry::Loading {
            path,
            previous: Some(previous),
        };
        return Err(IpcFailure::new(
            "model_load_state_changed",
            "the selected model changed while native unload was running",
            true,
        ));
    }
    let model_id = previous.descriptor.stable_model_id.clone();
    match release {
        Ok(resident_slot_released) => {
            *registry = ModelRegistry::Empty;
            Ok(ModelUnloadOutcome {
                model_id: Some(model_id),
                resident_slot_released,
            })
        }
        Err(error) => {
            *registry = ModelRegistry::Loaded(previous);
            Err(IpcFailure::new(
                "model_release_failed",
                format!("the selected local model could not be released safely: {error}"),
                true,
            ))
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn model_download_start<R: Runtime>(
    command_id: String,
    url: String,
    file_name: String,
    expected_sha256: String,
    expected_bytes: Option<u64>,
    max_bytes: u64,
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<ModelDownloadSnapshot, IpcFailure> {
    let PreparedModelDownload {
        command_id,
        request,
        spec,
    } = prepare_model_download(
        &state,
        &command_id,
        url,
        file_name,
        &expected_sha256,
        expected_bytes,
        max_bytes,
    )?;
    let (reservation, snapshot) = state
        .downloads
        .reserve(spec, now_unix_ms())
        .map_err(|error| IpcFailure::model_download_registry(&error))?;
    if reservation == ReservationOutcome::Replayed {
        return Ok(snapshot);
    }
    emit_model_download_snapshot(
        &app,
        &state.downloads,
        "loom://model-download-progress",
        command_id,
        &snapshot,
    );
    let cancellation = state
        .downloads
        .cancellation(command_id)
        .map_err(|error| IpcFailure::model_download_registry(&error))?;
    spawn_model_download(
        app,
        Arc::clone(&state.downloads),
        command_id,
        request,
        cancellation,
    );
    Ok(snapshot)
}

#[allow(clippy::too_many_arguments)]
fn prepare_model_download(
    state: &State<'_, PluginState>,
    command_id: &str,
    url: String,
    file_name: String,
    expected_sha256: &str,
    expected_bytes: Option<u64>,
    max_bytes: u64,
) -> Result<PreparedModelDownload, IpcFailure> {
    let command_id = parse_model_download_command_id(command_id)?;
    if url.is_empty() || url.len() > MAX_MODEL_DOWNLOAD_URL_BYTES {
        return Err(IpcFailure::new(
            "invalid_model_download_url",
            format!(
                "model download URL must contain 1 to {MAX_MODEL_DOWNLOAD_URL_BYTES} UTF-8 bytes"
            ),
            false,
        ));
    }
    if max_bytes == 0 || max_bytes > MAX_MODEL_DOWNLOAD_BYTES {
        return Err(IpcFailure::new(
            "invalid_model_download_limit",
            format!(
                "maximum model download size must be between 1 and {MAX_MODEL_DOWNLOAD_BYTES} bytes"
            ),
            false,
        ));
    }
    if expected_bytes.is_some_and(|bytes| bytes == 0 || bytes > max_bytes) {
        return Err(IpcFailure::new(
            "invalid_model_download_size",
            "expected model size must be positive and no larger than the download limit",
            false,
        ));
    }

    let root = state.model_library_root.as_deref().ok_or_else(|| {
        IpcFailure::new(
            "model_library_unavailable",
            "the operating system did not provide an application data directory",
            false,
        )
    })?;
    let library = prepare_model_library(root).map_err(|error| IpcFailure::model_library(&error))?;
    let target_path = model_target_path(&library, &file_name)
        .map_err(|error| IpcFailure::model_library(&error))?;
    let expected_sha256 = Sha256Digest::from_hex(expected_sha256)
        .map_err(|error| IpcFailure::model_download_request(&error))?;
    let mut request =
        GgufDownloadRequest::new(url, target_path.clone(), expected_sha256, max_bytes);
    request.expected_bytes = expected_bytes;
    validate_gguf_download_request(&request)
        .map_err(|error| IpcFailure::model_download_request(&error))?;
    let request_fingerprint = model_download_fingerprint(&request, &file_name);
    Ok(PreparedModelDownload {
        command_id,
        request,
        spec: ModelDownloadSpec {
            command_id,
            request_fingerprint,
            display_name: file_name,
            target_path,
            expected_sha256: expected_sha256.to_string(),
            expected_bytes,
        },
    })
}

fn spawn_model_download<R: Runtime>(
    app: AppHandle<R>,
    downloads: Arc<ModelDownloadRegistry>,
    command_id: CommandId,
    request: GgufDownloadRequest,
    cancellation: loom_backend_llama::DownloadCancellation,
) {
    std::mem::drop(tauri::async_runtime::spawn(async move {
        let progress_downloads = Arc::clone(&downloads);
        let progress_app = app.clone();
        let result = download_gguf(
            &request,
            &cancellation,
            move |progress| match progress_downloads.record_progress(
                command_id,
                progress,
                now_unix_ms(),
            ) {
                Ok(snapshot) => {
                    emit_model_download_snapshot(
                        &progress_app,
                        &progress_downloads,
                        "loom://model-download-progress",
                        command_id,
                        &snapshot,
                    );
                    DownloadControl::Continue
                }
                Err(_) => DownloadControl::Cancel,
            },
        )
        .await;
        let terminal = match result {
            Ok(result) => downloads.complete(command_id, &result, now_unix_ms()),
            Err(error) if error.is_cancelled() => {
                downloads.finish_cancelled(command_id, now_unix_ms())
            }
            Err(error) => downloads.fail(
                command_id,
                error.to_string(),
                error.is_retryable(),
                now_unix_ms(),
            ),
        };
        match terminal {
            Ok(snapshot) => emit_model_download_snapshot(
                &app,
                &downloads,
                "loom://model-download-terminal",
                command_id,
                &snapshot,
            ),
            Err(error) => eprintln!("Loom model download terminal state failed: {error}"),
        }
    }));
}

#[tauri::command]
async fn model_download_cancel<R: Runtime>(
    command_id: String,
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<ModelDownloadSnapshot, IpcFailure> {
    let command_id = parse_model_download_command_id(&command_id)?;
    let snapshot = state
        .downloads
        .request_cancel(command_id, now_unix_ms())
        .map_err(|error| IpcFailure::model_download_registry(&error))?;
    if !snapshot.status.is_terminal() {
        emit_model_download_snapshot(
            &app,
            &state.downloads,
            "loom://model-download-progress",
            command_id,
            &snapshot,
        );
    }
    Ok(snapshot)
}

#[tauri::command]
async fn model_download_status(
    command_id: String,
    state: State<'_, PluginState>,
) -> Result<ModelDownloadSnapshot, IpcFailure> {
    let command_id = parse_model_download_command_id(&command_id)?;
    state
        .downloads
        .status(command_id)
        .map_err(|error| IpcFailure::model_download_registry(&error))
}

#[tauri::command]
async fn model_download_list(
    state: State<'_, PluginState>,
) -> Result<Vec<ModelDownloadSnapshot>, IpcFailure> {
    state
        .downloads
        .list()
        .map_err(|error| IpcFailure::model_download_registry(&error))
}

fn parse_model_download_command_id(command_id: &str) -> Result<CommandId, IpcFailure> {
    command_id.parse().map_err(|_| {
        IpcFailure::new(
            "invalid_command_id",
            "model download command ID is not a valid ULID",
            false,
        )
    })
}

fn model_download_fingerprint(request: &GgufDownloadRequest, file_name: &str) -> String {
    let mut bytes = Vec::with_capacity(
        request
            .url
            .len()
            .saturating_add(file_name.len())
            .saturating_add(160),
    );
    append_fingerprint_field(&mut bytes, b"loom-model-download-v1");
    append_fingerprint_field(&mut bytes, request.url.as_bytes());
    append_fingerprint_field(&mut bytes, file_name.as_bytes());
    append_fingerprint_field(&mut bytes, request.expected_sha256.to_string().as_bytes());
    append_fingerprint_field(&mut bytes, &request.max_bytes.to_be_bytes());
    match request.expected_bytes {
        Some(expected) => {
            bytes.push(1);
            bytes.extend_from_slice(&expected.to_be_bytes());
        }
        None => bytes.push(0),
    }
    BlobId::digest(&bytes).to_string()
}

fn append_fingerprint_field(target: &mut Vec<u8>, value: &[u8]) {
    let length = u64::try_from(value.len()).unwrap_or(u64::MAX);
    target.extend_from_slice(&length.to_be_bytes());
    target.extend_from_slice(value);
}

fn emit_model_download_snapshot<R: Runtime>(
    app: &AppHandle<R>,
    registry: &ModelDownloadRegistry,
    event: &str,
    command_id: CommandId,
    snapshot: &ModelDownloadSnapshot,
) {
    if app.emit(event, snapshot.clone()).is_err()
        && let Err(error) = registry.record_delivery_failure(command_id)
    {
        eprintln!("Loom model download event reconciliation failed: {error}");
    }
}

fn discover_loadable_model(
    state: &State<'_, PluginState>,
    canonical: &Path,
) -> Result<loom_backend_llama::DiscoveredGguf, IpcFailure> {
    let options = desktop_model_discovery_options(state)?;
    let report = discover_gguf_models(&options)
        .map_err(|error| IpcFailure::new("model_discovery_error", error.to_string(), false))?;
    let model = report
        .models
        .into_iter()
        .find(|model| model.resolved_path == canonical)
        .ok_or_else(|| {
            IpcFailure::new(
                "model_not_discovered",
                "the selected path is not one of Loom's bounded, local GGUF discoveries",
                false,
            )
        })?;
    if !matches!(model.header, GgufHeaderStatus::Verified) {
        return Err(IpcFailure::new(
            "model_header_unverified",
            "the selected file does not have a verified GGUF container header",
            false,
        ));
    }
    Ok(model)
}

fn desktop_model_discovery_options(
    state: &State<'_, PluginState>,
) -> Result<ModelDiscoveryOptions, IpcFailure> {
    let mut options = ModelDiscoveryOptions::default();
    options.max_entries = options.max_entries.min(20_000);
    options.max_depth = options.max_depth.min(12);
    if let Some(path) = std::env::var_os("LOOM_GGUF_MODEL_PATH") {
        options.user_paths.push(PathBuf::from(path));
    }
    if let Some(root) = &state.model_library_root {
        options.user_paths.push(root.join("models"));
    }
    options.user_paths.extend(
        state
            .user_model_paths
            .lock()
            .map_err(|_| {
                IpcFailure::new(
                    "model_path_registry_poisoned",
                    "the selected-model registry entered an invalid state; restart Loom",
                    false,
                )
            })?
            .iter()
            .cloned(),
    );
    Ok(options)
}

fn remember_user_model_path(
    state: &State<'_, PluginState>,
    path: PathBuf,
) -> Result<(), IpcFailure> {
    state
        .user_model_paths
        .lock()
        .map_err(|_| {
            IpcFailure::new(
                "model_path_registry_poisoned",
                "the selected-model registry entered an invalid state; restart Loom",
                false,
            )
        })?
        .insert(path);
    Ok(())
}

fn release_staged_model(
    state: &State<'_, PluginState>,
    path: &Path,
    profile: &LocalModelProfile,
) -> Result<(), IpcFailure> {
    let release = state.backend.release_model(profile);
    let mut registry = lock_model_registry(state)?;
    let current = std::mem::take(&mut *registry);
    match current {
        ModelRegistry::Loading {
            path: loading,
            previous,
        } if loading == path => {
            *registry = previous.map_or(ModelRegistry::Empty, ModelRegistry::Loaded);
        }
        current => {
            *registry = current;
            return Err(IpcFailure::new(
                "model_load_state_changed",
                "the selected model changed while native verification was running",
                true,
            ));
        }
    }
    release.map(|_| ()).map_err(|error| {
        IpcFailure::new(
            "model_release_failed",
            format!("the rejected local model could not be released safely: {error}"),
            true,
        )
    })
}

fn model_summary(model: &LoadedModel, header_verified: bool) -> ModelCapabilitySummary {
    ModelCapabilitySummary {
        model_id: model.descriptor.stable_model_id.clone(),
        display_name: model.descriptor.display_name.clone(),
        local: true,
        loaded: true,
        chat: model.descriptor.capabilities.chat.is_supported(),
        completion: model.descriptor.capabilities.completion_text.is_supported(),
        fill_in_middle: model
            .descriptor
            .capabilities
            .fill_in_middle_contract_id
            .is_some(),
        output_tokens: model
            .descriptor
            .capabilities
            .generated_token_ids
            .is_supported(),
        logprobs: !model
            .descriptor
            .capabilities
            .log_probability_stages
            .is_empty(),
        model_path: model.profile.model_path.to_string_lossy().into_owned(),
        file_bytes: model.descriptor.model_file_bytes,
        header_verified,
        architecture: model.descriptor.architecture.clone(),
        context_tokens: Some(model.descriptor.context_tokens),
        model_sha256: Some(model.descriptor.model_sha256.clone()),
        projector_present: Some(model.descriptor.projector_sha256.is_some()),
        media_kinds: model
            .descriptor
            .capabilities
            .media
            .iter()
            .map(|media| match media.kind {
                loom_backend_llama::VerifiedMediaKind::Image => "image",
                loom_backend_llama::VerifiedMediaKind::Audio => "audio",
            })
            .collect(),
        tested_profile: (model.descriptor.model_sha256 == TESTED_GEMMA_4_E2B_BASE_Q8_SHA256)
            .then_some(TESTED_GEMMA_4_E2B_BASE_PROFILE),
    }
}

#[tauri::command]
async fn branch_page(
    project_id: String,
    session_id: String,
    document_id: String,
    after: Option<BranchCursorSnapshot>,
    limit: u32,
    state: State<'_, PluginState>,
) -> Result<BranchPageSnapshot, IpcFailure> {
    let document_id = document_id.parse::<DocumentId>().map_err(|_| {
        IpcFailure::new(
            "invalid_document_id",
            "document ID is not a valid ULID",
            false,
        )
    })?;
    let after = after.map(BranchPageCursor::try_from).transpose()?;
    let limit = usize::try_from(limit).map_err(|_| {
        IpcFailure::new(
            "invalid_branch_page_limit",
            "branch page limit does not fit this platform",
            false,
        )
    })?;
    let page = {
        let mut session = lock_session(&state)?;
        let store = require_bound_store(&mut session, &project_id, &session_id)?;
        store
            .branch_page(document_id, after, limit)
            .map_err(IpcFailure::store)?
    };
    let branches = page
        .branches
        .into_iter()
        .map(|summary| {
            let active = state
                .generations
                .route_for_run(summary.run_id)
                .map_err(|error| IpcFailure::generation_registry(&error))?
                .is_some();
            Ok(branch_summary_snapshot(summary, active))
        })
        .collect::<Result<Vec<_>, IpcFailure>>()?;
    Ok(BranchPageSnapshot {
        branches,
        next_cursor: page.next_cursor.map(BranchCursorSnapshot::from),
        has_more: page.has_more,
    })
}

#[tauri::command]
async fn branch_get(
    project_id: String,
    session_id: String,
    document_id: String,
    run_id: String,
    state: State<'_, PluginState>,
) -> Result<Option<BranchSummarySnapshot>, IpcFailure> {
    let document_id = parse_document_id(&document_id)?;
    let run_id = parse_generation_run_id(&run_id)?;
    let summary = {
        let mut session = lock_session(&state)?;
        let store = require_bound_store(&mut session, &project_id, &session_id)?;
        store
            .branch_summary(document_id, run_id)
            .map_err(IpcFailure::store)?
    };
    summary
        .map(|summary| {
            let active = state
                .generations
                .route_for_run(summary.run_id)
                .map_err(|error| IpcFailure::generation_registry(&error))?
                .is_some();
            Ok(branch_summary_snapshot(summary, active))
        })
        .transpose()
}

#[tauri::command]
async fn branch_body(
    project_id: String,
    session_id: String,
    document_id: String,
    run_id: String,
    max_bytes: u32,
    state: State<'_, PluginState>,
) -> Result<Option<BranchBodySnapshot>, IpcFailure> {
    let document_id = parse_document_id(&document_id)?;
    let run_id = parse_generation_run_id(&run_id)?;
    let body = {
        let mut session = lock_session(&state)?;
        let store = require_bound_store(&mut session, &project_id, &session_id)?;
        store
            .branch_body(document_id, run_id, u64::from(max_bytes))
            .map_err(IpcFailure::store)?
    };
    Ok(body.map(branch_body_snapshot))
}

fn branch_snapshot(record: StoredBranchRecord, active: bool) -> BranchSnapshot {
    BranchSnapshot {
        run_id: record.run_id.to_string(),
        branch_id: record.branch_id.to_string(),
        candidate_id: record.candidate_id.map(|id| id.to_string()),
        source_revision_id: record.source_revision_id.to_string(),
        target_start_byte: record.target_range.start,
        target_end_byte: record.target_range.end,
        text: record.output_text.unwrap_or_default(),
        output_blob_id: record.output_blob_id.map(|id| id.to_string()),
        output_byte_len: record.output_byte_len,
        status: branch_status(record.status, active),
        seed: record.seed.to_string(),
        model_id: record.model_identifier,
        selection: record.selection.map(selection_label),
        error: record.error,
        error_truncated: false,
        created_at_unix_ms: record.created_at_ms,
    }
}

fn branch_summary_snapshot(summary: StoredBranchSummary, active: bool) -> BranchSummarySnapshot {
    BranchSummarySnapshot {
        run_id: summary.run_id.to_string(),
        branch_id: summary.branch_id.to_string(),
        candidate_id: summary.candidate_id.map(|id| id.to_string()),
        source_revision_id: summary.source_revision_id.to_string(),
        target_start_byte: summary.target_range.start,
        target_end_byte: summary.target_range.end,
        output_blob_id: summary.output_blob_id.map(|id| id.to_string()),
        output_byte_len: summary.output_byte_len,
        status: branch_status(summary.status, active),
        seed: summary.seed.map(|seed| seed.to_string()),
        model_id: summary.model_identifier,
        selection: summary.selection.map(selection_label),
        error: summary.error,
        error_truncated: summary.error_truncated,
        created_at_unix_ms: summary.created_at_ms,
    }
}

fn branch_body_snapshot(body: StoredBranchBody) -> BranchBodySnapshot {
    BranchBodySnapshot {
        run_id: body.run_id.to_string(),
        output_blob_id: body.output_blob_id.to_string(),
        byte_len: body.byte_len,
        text: body.text,
    }
}

fn branch_status(status: StoredBranchStatus, active: bool) -> &'static str {
    if active {
        return "generating";
    }
    match status {
        StoredBranchStatus::Interrupted => "interrupted",
        StoredBranchStatus::Completed => "ready",
        StoredBranchStatus::Cancelled => "cancelled",
        StoredBranchStatus::Failed => "failed",
        StoredBranchStatus::Pruned => "pruned",
        StoredBranchStatus::Rejected => "rejected",
    }
}

const fn selection_label(selection: SelectionDecision) -> &'static str {
    match selection {
        SelectionDecision::KeepAlternative => "keep_alternative",
        SelectionDecision::Promote => "promote",
        SelectionDecision::Reject => "reject",
    }
}

fn branch_records_for_runs(
    store: &ProjectStore,
    document_id: DocumentId,
    run_ids: &[GenerationRunId],
) -> Result<Vec<StoredBranchRecord>, IpcFailure> {
    if run_ids.len() > 4 {
        return Err(IpcFailure::new(
            "generation_provenance_mismatch",
            "a recorded manual Weave family exceeds the four-branch recovery limit",
            false,
        ));
    }
    run_ids
        .iter()
        .map(|&run_id| {
            store
                .branch_record(document_id, run_id, MAX_BRANCH_BODY_BYTES)
                .map_err(IpcFailure::store)?
                .ok_or_else(|| {
                    IpcFailure::new(
                        "generation_provenance_missing",
                        "the recorded Weave family is missing a durable branch projection",
                        false,
                    )
                })
        })
        .collect()
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn replay_weave_if_recorded(
    state: &State<'_, PluginState>,
    project_id: &str,
    session_id: &str,
    command_id: CommandId,
    document_id: DocumentId,
    relative_path: &str,
    source_revision_id: RevisionId,
    expected_visible_blob_id: BlobId,
    cursor_byte: u64,
    branch_count: u32,
    max_tokens: u32,
    temperature: f32,
) -> Result<Option<WeaveStarted>, IpcFailure> {
    let replay = {
        let mut session = lock_session(state)?;
        let store = require_bound_store(&mut session, project_id, session_id)?;
        let Some(family) = store
            .generation_family_for_command(command_id)
            .map_err(IpcFailure::store)?
        else {
            return Ok(None);
        };
        let document_matches = store
            .list_documents()
            .map_err(IpcFailure::store)?
            .into_iter()
            .any(|document| {
                document.document_id == document_id && document.relative_path == relative_path
            });
        let source_bytes = store
            .reconstruct_revision(source_revision_id)
            .map_err(IpcFailure::store)?;
        let cursor = usize::try_from(cursor_byte).map_err(|_| {
            IpcFailure::new(
                "idempotency_conflict",
                "the recorded Weave cursor exceeds this platform's addressable range",
                false,
            )
        })?;
        let source_text = std::str::from_utf8(&source_bytes).map_err(|_| {
            IpcFailure::new(
                "generation_provenance_mismatch",
                "the recorded Weave source revision is not valid UTF-8",
                false,
            )
        })?;
        let expected_range = ByteRange::new(cursor_byte, cursor_byte).ok_or_else(|| {
            IpcFailure::new(
                "idempotency_conflict",
                "the replayed Weave cursor range is invalid",
                false,
            )
        })?;
        let family_matches = family.generations.len() == branch_count as usize
            && family.receipt.source_revision_id == Some(source_revision_id)
            && document_matches
            && BlobId::digest(&source_bytes) == expected_visible_blob_id
            && cursor <= source_text.len()
            && source_text.is_char_boundary(cursor)
            && family
                .generations
                .iter()
                .enumerate()
                .all(|(index, started)| {
                    let Ok(case_index) = u32::try_from(index) else {
                        return false;
                    };
                    let (run_id, branch_id) = derive_weave_case_ids(command_id, case_index);
                    let sampling = SamplingConfig {
                        seed: generation_seed(command_id, case_index),
                        temperature,
                        max_tokens,
                        ..SamplingConfig::default()
                    };
                    serde_json::to_value(sampling).is_ok_and(|sampling| {
                        started.generation.run_id == run_id
                            && started.generation.branch_id == branch_id
                            && started.generation.document_id == document_id
                            && started.generation.source_revision_id == source_revision_id
                            && started.generation.target_range == expected_range
                            && started.generation.seed
                                == u64::from(generation_seed(command_id, case_index))
                            && started.generation.sampling == sampling
                    })
                });
        if !family_matches {
            return Err(IpcFailure::new(
                "idempotency_conflict",
                "this command ID already identifies a different Weave request",
                false,
            ));
        }

        let run_order = family
            .generations
            .iter()
            .map(|started| started.generation.run_id)
            .collect::<Vec<_>>();
        let ordered_records = branch_records_for_runs(store, document_id, &run_order)?;
        (
            BlobId::digest(&source_text.as_bytes()[..cursor]),
            ordered_records,
        )
    };

    let branches = replay
        .1
        .into_iter()
        .map(|record| {
            let active = state
                .generations
                .route_for_run(record.run_id)
                .map_err(|error| IpcFailure::generation_registry(&error))?
                .is_some();
            Ok(branch_snapshot(record, active))
        })
        .collect::<Result<Vec<_>, IpcFailure>>()?;
    Ok(Some(WeaveStarted {
        command_id: command_id.to_string(),
        request_id: format!("weave-{command_id}"),
        project_id: project_id.to_string(),
        session_id: session_id.to_string(),
        document_id: document_id.to_string(),
        source_revision_id: source_revision_id.to_string(),
        exact_prompt_blob_id: replay.0.to_string(),
        branches,
    }))
}

#[tauri::command]
async fn weave_status(
    project_id: String,
    session_id: String,
    command_id: String,
    state: State<'_, PluginState>,
) -> Result<Option<WeaveStarted>, IpcFailure> {
    let command_id = parse_command_id(&command_id)?;
    let recorded = {
        let mut session = lock_session(&state)?;
        let store = require_bound_store(&mut session, &project_id, &session_id)?;
        let Some(family) = store
            .generation_family_for_command(command_id)
            .map_err(IpcFailure::store)?
        else {
            return Ok(None);
        };
        let first = family.generations.first().ok_or_else(|| {
            IpcFailure::new(
                "generation_provenance_missing",
                "the recorded Weave command contains no generation runs",
                false,
            )
        })?;
        let document_id = first.generation.document_id;
        let source_revision_id = first.generation.source_revision_id;
        let target = first.generation.target_range;
        if !target.is_empty()
            || family.generations.iter().any(|started| {
                started.generation.document_id != document_id
                    || started.generation.source_revision_id != source_revision_id
                    || started.generation.target_range != target
            })
        {
            return Err(IpcFailure::new(
                "generation_provenance_mismatch",
                "the recorded Weave family does not share one exact continuation boundary",
                false,
            ));
        }
        let source_bytes = store
            .reconstruct_revision(source_revision_id)
            .map_err(IpcFailure::store)?;
        let cursor = usize::try_from(target.start).map_err(|_| {
            IpcFailure::new(
                "generation_provenance_mismatch",
                "the recorded Weave cursor exceeds this platform's addressable range",
                false,
            )
        })?;
        let source_text = std::str::from_utf8(&source_bytes).map_err(|_| {
            IpcFailure::new(
                "generation_provenance_mismatch",
                "the recorded Weave source revision is not valid UTF-8",
                false,
            )
        })?;
        if cursor > source_text.len() || !source_text.is_char_boundary(cursor) {
            return Err(IpcFailure::new(
                "generation_provenance_mismatch",
                "the recorded Weave cursor is not a UTF-8 source boundary",
                false,
            ));
        }
        let run_order = family
            .generations
            .iter()
            .map(|started| started.generation.run_id)
            .collect::<Vec<_>>();
        let records = branch_records_for_runs(store, document_id, &run_order)?;
        (
            document_id,
            source_revision_id,
            BlobId::digest(&source_text.as_bytes()[..cursor]),
            records,
        )
    };
    let branches = recorded
        .3
        .into_iter()
        .map(|record| {
            let active = state
                .generations
                .route_for_run(record.run_id)
                .map_err(|error| IpcFailure::generation_registry(&error))?
                .is_some();
            Ok(branch_snapshot(record, active))
        })
        .collect::<Result<Vec<_>, IpcFailure>>()?;
    Ok(Some(WeaveStarted {
        command_id: command_id.to_string(),
        request_id: format!("weave-{command_id}"),
        project_id,
        session_id,
        document_id: recorded.0.to_string(),
        source_revision_id: recorded.1.to_string(),
        exact_prompt_blob_id: recorded.2.to_string(),
        branches,
    }))
}

#[tauri::command]
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn weave_start<R: Runtime>(
    project_id: String,
    session_id: String,
    command_id: String,
    document_id: String,
    relative_path: String,
    source_revision_id: String,
    expected_visible_blob_id: String,
    cursor_byte: u64,
    branch_count: u32,
    max_tokens: u32,
    temperature: f32,
    automatic: bool,
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<WeaveStarted, IpcFailure> {
    let command_id = parse_command_id(&command_id)?;
    let document_id = document_id.parse::<DocumentId>().map_err(|_| {
        IpcFailure::new(
            "invalid_document_id",
            "document ID is not a valid ULID",
            false,
        )
    })?;
    let source_revision_id = source_revision_id.parse::<RevisionId>().map_err(|_| {
        IpcFailure::new(
            "invalid_revision_id",
            "source revision ID is not a valid ULID",
            false,
        )
    })?;
    let expected_visible_blob_id = expected_visible_blob_id.parse::<BlobId>().map_err(|_| {
        IpcFailure::new(
            "invalid_blob_id",
            "visible blob ID is not a valid SHA-256 digest",
            false,
        )
    })?;
    if branch_count == 0 || branch_count > 4 {
        return Err(IpcFailure::new(
            "invalid_branch_count",
            "a weave must request between one and four branches",
            false,
        ));
    }
    if max_tokens == 0 || max_tokens > 2_048 {
        return Err(IpcFailure::new(
            "invalid_generation_budget",
            "a weave must request between one and 2,048 tokens per branch",
            false,
        ));
    }
    if !temperature.is_finite() || !(0.0..=2.0).contains(&temperature) {
        return Err(IpcFailure::new(
            "invalid_temperature",
            "temperature must be a finite value from 0 through 2",
            false,
        ));
    }

    if let Some(replay) = replay_weave_if_recorded(
        &state,
        &project_id,
        &session_id,
        command_id,
        document_id,
        &relative_path,
        source_revision_id,
        expected_visible_blob_id,
        cursor_byte,
        branch_count,
        max_tokens,
        temperature,
    )? {
        return Ok(replay);
    }

    // Serialize the loaded-model snapshot through native startup and family
    // registration. A switch cannot observe zero active branches in the gap.
    let _model_lifecycle = lock_model_lifecycle(&state)?;
    let loaded_model = loaded_model(&state)?;
    let max_cases = loaded_model
        .descriptor
        .capabilities
        .max_cases
        .min(loaded_model.profile.max_parallel_cases);
    if branch_count > max_cases {
        return Err(IpcFailure::new(
            "model_branch_limit",
            format!("the verified model supports at most {max_cases} parallel branches"),
            false,
        ));
    }
    let model_environment = model_environment_from_verified(&loaded_model.descriptor)
        .map_err(|error| IpcFailure::backend(&error))?;

    let request_id = format!("weave-{command_id}");
    let (identity, exact_prefix, prompt_recipe, cases, queued_branches, runs) = {
        let mut session = lock_session(&state)?;
        let admission = if automatic {
            session.agency.admit_automation()
        } else {
            session.agency.admit_manual_generation()
        };
        admission
            .map_err(|error| IpcFailure::new("generation_blocked", error.to_string(), false))?;
        let active_session_id = session.active_session_id.ok_or_else(|| {
            IpcFailure::new(
                "corrupt_project_session",
                "the live project session is missing its session ID",
                false,
            )
        })?;
        let store = require_bound_store(&mut session, &project_id, &session_id)?;
        let loaded = store
            .read_document(&relative_path)
            .map_err(IpcFailure::store)?;
        ensure_document_id(&loaded, &document_id.to_string())?;
        if loaded.revision_id != source_revision_id {
            return Err(IpcFailure::new(
                "source_revision_conflict",
                "the manuscript revision changed before generation began",
                false,
            ));
        }
        if loaded.blob_id != expected_visible_blob_id {
            return Err(IpcFailure::new(
                "source_blob_conflict",
                "the visible manuscript bytes changed before generation began",
                false,
            ));
        }
        let cursor = usize::try_from(cursor_byte).map_err(|_| {
            IpcFailure::new(
                "invalid_cursor_boundary",
                "the generation cursor exceeds this platform's addressable range",
                false,
            )
        })?;
        if cursor > loaded.text.len() || !loaded.text.is_char_boundary(cursor) {
            return Err(IpcFailure::new(
                "invalid_cursor_boundary",
                "the generation cursor is not a UTF-8 boundary in the source revision",
                false,
            ));
        }
        if cursor == 0 {
            return Err(IpcFailure::new(
                "empty_completion_prefix",
                "write or place the cursor after at least one manuscript character before weaving",
                false,
            ));
        }
        let exact_prefix = loaded.text[..cursor].to_owned();
        let exact_prompt_blob_id = store
            .store_provenance_blob(exact_prefix.as_bytes())
            .map_err(IpcFailure::store)?;
        if exact_prompt_blob_id != BlobId::digest(exact_prefix.as_bytes()) {
            return Err(IpcFailure::new(
                "prompt_identity_mismatch",
                "the persisted prompt bytes do not match the exact manuscript prefix",
                false,
            ));
        }
        let environment_artifact = store
            .record_model_environment(&model_environment)
            .map_err(IpcFailure::store)?;
        let prompt_recipe = PromptRecipe {
            mode: PromptMode::Completion,
            exact_prompt_blob_id,
            exact_prompt_token_ids: None,
            ordered_input_artifact_ids: vec![loaded.artifact_id],
            prompt_token_count: None,
        };
        let prompt_artifact = store
            .record_prompt_recipe(&prompt_recipe)
            .map_err(IpcFailure::store)?;
        let context_artifact = store
            .record_context_recipe(&ContextRecipe {
                source_revision_id,
                ordered_source_artifact_ids: Vec::new(),
                token_budget: u64::from(loaded_model.profile.context_tokens),
                retrieval_evidence_blob_id: None,
            })
            .map_err(IpcFailure::store)?;
        let authority_artifact = store
            .record_authority_policy(&AuthorityPolicy {
                policy_version: 1,
                writer_environment_artifact_ids: vec![environment_artifact.artifact_id],
                critic_environment_artifact_ids: Vec::new(),
            })
            .map_err(IpcFailure::store)?;
        let target_range = ByteRange::new(cursor_byte, cursor_byte).ok_or_else(|| {
            IpcFailure::new(
                "invalid_cursor_boundary",
                "the generation target range is invalid",
                false,
            )
        })?;
        let mut cases = Vec::with_capacity(branch_count as usize);
        for index in 0..branch_count {
            let (run_id, branch_id) = derive_weave_case_ids(command_id, index);
            let sampling = SamplingConfig {
                seed: generation_seed(command_id, index),
                temperature,
                max_tokens,
                ..SamplingConfig::default()
            };
            let generation = GenerationStart {
                run_id,
                branch_id,
                document_id,
                source_revision_id,
                target_range,
                model_environment_artifact_id: environment_artifact.artifact_id,
                prompt_recipe_artifact_id: prompt_artifact.artifact_id,
                context_recipe_artifact_id: context_artifact.artifact_id,
                authority_policy_artifact_id: authority_artifact.artifact_id,
                seed: u64::from(sampling.seed),
                sampling: serde_json::Value::Null,
            };
            cases.push(
                ContinuationCase::bind_sampling(generation, sampling).map_err(|error| {
                    IpcFailure::new("sampling_serialize_failed", error.to_string(), false)
                })?,
            );
        }
        let identity = GenerationFamilyIdentity {
            request_id: request_id.clone(),
            project_id: store.manifest().project_id,
            session_id: active_session_id,
            document_id,
        };
        let runs = cases
            .iter()
            .map(|case| (case.generation.run_id, case.generation.branch_id))
            .collect::<Vec<_>>();
        // Reserve cancellable routes while the admission mutex is still held.
        // A concurrent opt-out/Focus/close command can therefore never observe
        // an admitted-but-unregistered family. Cancellation arriving before
        // the native handle is attached is retained by GenerationRegistry.
        state
            .generations
            .reserve(identity.clone(), runs.clone())
            .map_err(|error| IpcFailure::generation_registry(&error))?;
        let family = match store.start_generation_family_with_command(
            command_id,
            cases.iter().map(|case| case.generation.clone()).collect(),
        ) {
            Ok(family) => family,
            Err(error) => {
                let _ = state.generations.complete_family(&request_id);
                return Err(IpcFailure::store(error));
            }
        };
        let queued_branches = family
            .generations
            .into_iter()
            .map(|started| BranchSnapshot {
                run_id: started.generation.run_id.to_string(),
                branch_id: started.generation.branch_id.to_string(),
                candidate_id: None,
                source_revision_id: started.generation.source_revision_id.to_string(),
                target_start_byte: started.generation.target_range.start,
                target_end_byte: started.generation.target_range.end,
                text: String::new(),
                output_blob_id: None,
                output_byte_len: None,
                status: "queued",
                seed: started.generation.seed.to_string(),
                model_id: model_environment.model_identifier.clone(),
                selection: None,
                error: None,
                error_truncated: false,
                created_at_unix_ms: started.queued_event.occurred_at_ms,
            })
            .collect::<Vec<_>>();
        (
            identity,
            exact_prefix,
            prompt_recipe,
            cases,
            queued_branches,
            runs,
        )
    };
    let exact_prompt_blob_id = BlobId::digest(exact_prefix.as_bytes());
    let result_binding = GenerationResultBinding {
        exact_prompt_blob_id,
        model_environment: model_environment.clone(),
        model: loaded_model.descriptor.clone(),
        generations: cases
            .iter()
            .map(|case| (case.generation.run_id, case.generation.clone()))
            .collect(),
    };
    let native_request = ExactContinuationRequest {
        request_id: request_id.clone(),
        model: loaded_model.profile,
        exact_manuscript_prefix: exact_prefix,
        prompt_recipe,
        cases,
    };
    let handle = match state.backend.start_exact_continuation(native_request) {
        Ok(handle) => Arc::new(handle),
        Err(error) => {
            if let Err(persistence) =
                fail_and_release_open_runs(&state, &identity, &runs, &error.to_string(), &app)
            {
                let _ = state
                    .generations
                    .mark_terminal_persistence_failure(&request_id, persistence.message.clone());
                return Err(persistence);
            }
            return Err(IpcFailure::backend(&error));
        }
    };
    if let Err(error) = state.generations.attach_cancellation(
        &request_id,
        Arc::new(LlamaCancellation {
            handle: Arc::clone(&handle),
        }),
    ) {
        for (_, branch_id) in &runs {
            let _ = handle.cancel_branch(*branch_id);
        }
        if let Err(persistence) =
            fail_and_release_open_runs(&state, &identity, &runs, &error.to_string(), &app)
        {
            let _ = state
                .generations
                .mark_terminal_persistence_failure(&request_id, persistence.message.clone());
            return Err(persistence);
        }
        return Err(IpcFailure::generation_registry(&error));
    }
    let worker_app = app.clone();
    let worker_identity = identity.clone();
    let worker_runs = runs.clone();
    let worker_binding = result_binding.clone();
    let worker_handle = Arc::clone(&handle);
    if let Err(error) = std::thread::Builder::new()
        .name("loom-desktop-generation".to_string())
        .spawn(move || {
            run_desktop_generation(
                &worker_app,
                &worker_identity,
                &worker_runs,
                &worker_binding,
                &worker_handle,
            );
        })
    {
        for (_, branch_id) in &runs {
            let _ = handle.cancel_branch(*branch_id);
        }
        if let Err(persistence) =
            fail_and_release_open_runs(&state, &identity, &runs, &error.to_string(), &app)
        {
            let _ = state
                .generations
                .mark_terminal_persistence_failure(&request_id, persistence.message.clone());
            return Err(persistence);
        }
        return Err(IpcFailure::new(
            "generation_worker_spawn_failed",
            format!("the desktop generation worker could not start: {error}"),
            true,
        ));
    }

    Ok(WeaveStarted {
        command_id: command_id.to_string(),
        request_id,
        project_id,
        session_id,
        document_id: document_id.to_string(),
        source_revision_id: source_revision_id.to_string(),
        exact_prompt_blob_id: exact_prompt_blob_id.to_string(),
        branches: queued_branches,
    })
}

fn generation_seed(command_id: CommandId, index: u32) -> u32 {
    let material = format!("{command_id}:{index}");
    let digest = BlobId::digest(material.as_bytes());
    u32::from_le_bytes(
        digest.as_bytes()[..4]
            .try_into()
            .expect("a SHA-256 digest always contains four seed bytes"),
    )
}

fn loaded_model(state: &State<'_, PluginState>) -> Result<LoadedModel, IpcFailure> {
    let registry = lock_model_registry(state)?;
    match &*registry {
        ModelRegistry::Loaded(model) => Ok((**model).clone()),
        ModelRegistry::Loading { .. } => Err(IpcFailure::new(
            "model_load_in_progress",
            "wait for local model verification to finish before weaving",
            true,
        )),
        ModelRegistry::Empty => Err(IpcFailure::new(
            "model_not_loaded",
            "load and verify a local raw-completion model before weaving",
            false,
        )),
    }
}

fn run_desktop_generation<R: Runtime>(
    app: &AppHandle<R>,
    identity: &GenerationFamilyIdentity,
    runs: &[(GenerationRunId, BranchId)],
    binding: &GenerationResultBinding,
    handle: &Arc<LlamaGenerationHandle>,
) {
    let result = loop {
        match handle.receive_event_timeout(Duration::from_millis(10)) {
            Ok(Some(event)) => {
                if let Err(error) = persist_backend_event(app, identity, event) {
                    for (_, branch_id) in runs {
                        let _ = handle.cancel_branch(*branch_id);
                    }
                    break Err(error.message);
                }
            }
            Ok(None) | Err(LlamaBackendError::ResultDisconnected) => {}
            Err(error) => break Err(error.to_string()),
        }
        match handle.wait_timeout(Duration::ZERO) {
            Ok(result) => break Ok(result),
            Err(LlamaBackendError::ResultTimeout) => {}
            Err(error) => break Err(error.to_string()),
        }
    };

    let state = app.state::<PluginState>();
    let persistence = match result {
        Ok(result) => {
            let drained = drain_backend_events(app, identity, handle);
            drained.and_then(|()| persist_generation_result(app, identity, runs, binding, result))
        }
        Err(error) => Err(IpcFailure::new("generation_runtime_failed", error, true)),
    };
    let terminalized = match persistence {
        Ok(()) => Ok(()),
        Err(primary) => fail_open_runs(&state, identity, runs, &primary.message, app).map_err(
            |fallback| {
                IpcFailure::new(
                    "generation_terminal_persistence_failed",
                    format!(
                        "generation result persistence failed: {}; fallback terminal persistence also failed: {}",
                        primary.message, fallback.message
                    ),
                    true,
                )
            },
        ),
    };
    let finalized = terminalized
        .and_then(|()| release_family_after_terminal_persistence(&state, identity, runs));
    if let Err(error) = finalized {
        let _ = state
            .generations
            .mark_terminal_persistence_failure(&identity.request_id, error.message);
    }
}

fn drain_backend_events<R: Runtime>(
    app: &AppHandle<R>,
    identity: &GenerationFamilyIdentity,
    handle: &LlamaGenerationHandle,
) -> Result<(), IpcFailure> {
    loop {
        match handle.receive_event_timeout(Duration::ZERO) {
            Ok(Some(event)) => persist_backend_event(app, identity, event)?,
            Ok(None) | Err(LlamaBackendError::ResultDisconnected) => return Ok(()),
            Err(error) => return Err(IpcFailure::backend(&error)),
        }
    }
}

fn persist_backend_event<R: Runtime>(
    app: &AppHandle<R>,
    identity: &GenerationFamilyIdentity,
    event: LoomEvent,
) -> Result<(), IpcFailure> {
    let LoomEvent::Generation(event) = event else {
        // Candidate and terminal identities emitted by the backend are
        // provisional. The store mints and emits the only promotable IDs.
        return Ok(());
    };
    if matches!(
        event.kind,
        GenerationEventKind::Queued
            | GenerationEventKind::CandidateReady { .. }
            | GenerationEventKind::CancellationRequested
    ) {
        return Ok(());
    }
    let canonical = {
        let state = app.state::<PluginState>();
        let mut session = lock_session_internal(&state)?;
        let store = require_bound_store(
            &mut session,
            &identity.project_id.to_string(),
            &identity.session_id.to_string(),
        )?;
        store
            .append_generation_event(event.run_id, event.kind)
            .map_err(IpcFailure::store)?
    };
    emit_desktop_event(app, identity, LoomEvent::Generation(canonical))
}

#[allow(clippy::too_many_lines)]
fn persist_generation_result<R: Runtime>(
    app: &AppHandle<R>,
    identity: &GenerationFamilyIdentity,
    runs: &[(GenerationRunId, BranchId)],
    binding: &GenerationResultBinding,
    result: ExactContinuationResult,
) -> Result<(), IpcFailure> {
    let rebuilt_environment = model_environment_from_verified(&result.model)
        .map_err(|error| IpcFailure::backend(&error))?;
    if result.request_id != identity.request_id
        || result.exact_prompt_blob_id != binding.exact_prompt_blob_id
        || BlobId::digest(result.exact_manuscript_prefix.as_bytes()) != binding.exact_prompt_blob_id
        || result.model_environment != binding.model_environment
        || result.model != binding.model
        || rebuilt_environment != binding.model_environment
        || result.candidates.len() != runs.len()
        || binding.generations.len() != runs.len()
    {
        return Err(IpcFailure::new(
            "generation_provenance_mismatch",
            "the native batch result does not match its persisted prompt, model environment, or active branch family",
            false,
        ));
    }
    let expected = runs.iter().copied().collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    for (input_index, candidate) in result.candidates.into_iter().enumerate() {
        let expected_branch = expected.get(&candidate.generation.run_id).ok_or_else(|| {
            IpcFailure::new(
                "generation_result_identity_mismatch",
                "the native batch returned an unknown generation run",
                false,
            )
        })?;
        let expected_generation = binding
            .generations
            .get(&candidate.generation.run_id)
            .ok_or_else(|| {
                IpcFailure::new(
                    "generation_result_identity_mismatch",
                    "the native batch returned an unknown persisted generation run",
                    false,
                )
            })?;
        if *expected_branch != candidate.generation.branch_id
            || candidate.generation.document_id != identity.document_id
            || &candidate.generation != expected_generation
            || !seen.insert(candidate.generation.run_id)
        {
            return Err(IpcFailure::new(
                "generation_result_identity_mismatch",
                "the native batch returned a branch under the wrong document identity",
                false,
            ));
        }
        validate_candidate_receipt_binding(
            &candidate,
            &identity.request_id,
            binding.exact_prompt_blob_id,
            binding.model_environment.environment_id,
            &binding.model.local_model_id,
            input_index,
        )
        .map_err(|error| {
            IpcFailure::new(
                "generation_provenance_mismatch",
                format!("native backend receipt validation failed: {error}"),
                false,
            )
        })?;

        let canonical_events = {
            let state = app.state::<PluginState>();
            let mut session = lock_session_internal(&state)?;
            let store = require_bound_store(
                &mut session,
                &identity.project_id.to_string(),
                &identity.session_id.to_string(),
            )?;
            let raw_event_blob = store
                .store_provenance_blob(&candidate.raw_event_stream_bytes)
                .map_err(IpcFailure::store)?;
            if raw_event_blob != candidate.token_trace.raw_event_stream_blob_id {
                return Err(IpcFailure::new(
                    "generation_provenance_mismatch",
                    "the native raw event stream does not match its preserved digest",
                    false,
                ));
            }
            let backend_receipt_blob = store
                .store_provenance_blob(&candidate.backend_receipt_bytes)
                .map_err(IpcFailure::store)?;
            let declared_receipt = candidate
                .token_trace
                .provenance
                .as_ref()
                .and_then(|provenance| provenance.backend_receipt_blob_id)
                .ok_or_else(|| {
                    IpcFailure::new(
                        "generation_provenance_missing",
                        "the native generation did not preserve a backend receipt digest",
                        false,
                    )
                })?;
            if backend_receipt_blob != declared_receipt {
                return Err(IpcFailure::new(
                    "generation_provenance_mismatch",
                    "the native backend receipt does not match its preserved digest",
                    false,
                ));
            }

            match candidate.terminal.status {
                GenerationTerminalStatus::Completed => {
                    let outcome = store
                        .finish_generation_candidate(
                            candidate.generation.run_id,
                            TerminalCandidateInput {
                                output_bytes: candidate.output_text.into_bytes(),
                                token_trace: candidate.token_trace,
                            },
                        )
                        .map_err(IpcFailure::store)?;
                    vec![
                        LoomEvent::Generation(outcome.candidate_ready_event),
                        LoomEvent::GenerationTerminal(outcome.terminal_event),
                    ]
                }
                status @ (GenerationTerminalStatus::Cancelled
                | GenerationTerminalStatus::Pruned
                | GenerationTerminalStatus::Rejected) => {
                    let outcome = store
                        .finish_generation_with_evidence(
                            candidate.generation.run_id,
                            TerminalGenerationInput {
                                status,
                                error: None,
                                evidence: TerminalEvidenceInput {
                                    partial_output_bytes: candidate.output_text.into_bytes(),
                                    token_trace: candidate.token_trace,
                                },
                            },
                        )
                        .map_err(IpcFailure::store)?;
                    vec![LoomEvent::GenerationTerminal(outcome.terminal_event)]
                }
                GenerationTerminalStatus::Failed => {
                    let outcome = store
                        .finish_generation_with_evidence(
                            candidate.generation.run_id,
                            TerminalGenerationInput {
                                status: GenerationTerminalStatus::Failed,
                                error: Some(format!(
                                    "native generation failed: {}",
                                    candidate.finish_reason
                                )),
                                evidence: TerminalEvidenceInput {
                                    partial_output_bytes: candidate.output_text.into_bytes(),
                                    token_trace: candidate.token_trace,
                                },
                            },
                        )
                        .map_err(IpcFailure::store)?;
                    vec![LoomEvent::GenerationTerminal(outcome.terminal_event)]
                }
            }
        };
        for event in canonical_events {
            let _ = emit_desktop_event(app, identity, event);
        }
    }
    Ok(())
}

fn fail_open_runs<R: Runtime>(
    state: &PluginState,
    identity: &GenerationFamilyIdentity,
    runs: &[(GenerationRunId, BranchId)],
    error: &str,
    app: &AppHandle<R>,
) -> Result<(), IpcFailure> {
    let terminals = terminalize_open_runs(state, identity, runs, error)?;
    for terminal in terminals {
        let _ = emit_desktop_event(app, identity, terminal);
    }
    Ok(())
}

fn fail_and_release_open_runs<R: Runtime>(
    state: &PluginState,
    identity: &GenerationFamilyIdentity,
    runs: &[(GenerationRunId, BranchId)],
    error: &str,
    app: &AppHandle<R>,
) -> Result<(), IpcFailure> {
    fail_open_runs(state, identity, runs, error, app)?;
    release_family_after_terminal_persistence(state, identity, runs)
}

fn terminalize_open_runs(
    state: &PluginState,
    identity: &GenerationFamilyIdentity,
    runs: &[(GenerationRunId, BranchId)],
    error: &str,
) -> Result<Vec<LoomEvent>, IpcFailure> {
    let message = if error.trim().is_empty() {
        "local generation failed without an error message".to_string()
    } else {
        error.to_string()
    };
    let mut session = lock_session_internal(state)?;
    let store = require_bound_store(
        &mut session,
        &identity.project_id.to_string(),
        &identity.session_id.to_string(),
    )?;
    let mut terminals = Vec::new();
    for (run_id, _) in runs {
        match store
            .generation_terminal_count(*run_id)
            .map_err(IpcFailure::store)?
        {
            0 => terminals.push(LoomEvent::GenerationTerminal(
                store
                    .finish_generation(
                        *run_id,
                        GenerationTerminalStatus::Failed,
                        Some(message.clone()),
                    )
                    .map_err(IpcFailure::store)?,
            )),
            1 => {}
            count => {
                return Err(IpcFailure::new(
                    "generation_terminal_count_invalid",
                    format!(
                        "generation run {run_id} has {count} terminal events; expected exactly one"
                    ),
                    false,
                ));
            }
        }
    }
    Ok(terminals)
}

fn release_family_after_terminal_persistence(
    state: &PluginState,
    identity: &GenerationFamilyIdentity,
    runs: &[(GenerationRunId, BranchId)],
) -> Result<(), IpcFailure> {
    {
        let mut session = lock_session_internal(state)?;
        let store = require_bound_store(
            &mut session,
            &identity.project_id.to_string(),
            &identity.session_id.to_string(),
        )?;
        for (run_id, _) in runs {
            let count = store
                .generation_terminal_count(*run_id)
                .map_err(IpcFailure::store)?;
            if count != 1 {
                return Err(IpcFailure::new(
                    "generation_terminal_not_durable",
                    format!("generation run {run_id} has {count} durable terminal events"),
                    true,
                ));
            }
        }
    }
    state
        .generations
        .complete_family(&identity.request_id)
        .map_err(|error| IpcFailure::generation_registry(&error))?
        .ok_or_else(|| {
            IpcFailure::new(
                "generation_family_not_active",
                "the terminalized generation family was not active in the lifecycle registry",
                false,
            )
        })?;
    Ok(())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn generation_cancel<R: Runtime>(
    project_id: String,
    session_id: String,
    command_id: String,
    run_id: String,
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<Receipt, IpcFailure> {
    let command_id = parse_command_id(&command_id)?;
    let run_id = run_id.parse::<GenerationRunId>().map_err(|_| {
        IpcFailure::new(
            "invalid_generation_run_id",
            "generation run ID is not a valid ULID",
            false,
        )
    })?;
    let route = state
        .generations
        .route_for_run(run_id)
        .map_err(|error| IpcFailure::generation_registry(&error))?
        .ok_or_else(|| {
            IpcFailure::new(
                "generation_not_active",
                "the requested generation is no longer active",
                false,
            )
        })?;
    if route.identity.project_id.to_string() != project_id
        || route.identity.session_id.to_string() != session_id
    {
        return Err(IpcFailure::new(
            "stale_project_session",
            "this generation belongs to another project session",
            false,
        ));
    }
    let outcome = {
        let mut session = lock_session(&state)?;
        let store = require_bound_store(&mut session, &project_id, &session_id)?;
        store
            .request_cancel_generation_with_command(command_id, CancelGenerationCommand { run_id })
            .map_err(IpcFailure::store)?
    };
    // Persist the user's request before delivering the process-local side
    // effect. Reaching a terminal state in this interval is benign: a cancel
    // request is not a promise that the terminal status will be Cancelled.
    let _delivered = state
        .generations
        .cancel_run(route.identity.project_id, route.identity.session_id, run_id)
        .map_err(|error| IpcFailure::generation_registry(&error))?;
    emit_desktop_event(
        &app,
        &route.identity,
        LoomEvent::Generation(outcome.event.clone()),
    )?;
    let mut receipt = Receipt::from(outcome.receipt);
    receipt.request_fingerprint = Some(outcome.request_fingerprint.to_string());
    receipt.replayed = outcome.replayed;
    Ok(receipt)
}

#[tauri::command]
async fn candidate_keep(
    project_id: String,
    session_id: String,
    command_id: String,
    candidate_id: String,
    state: State<'_, PluginState>,
) -> Result<Receipt, IpcFailure> {
    let command_id = parse_command_id(&command_id)?;
    let candidate_id = parse_candidate_id(&candidate_id)?;
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    let outcome = store
        .keep_alternative_with_command(
            command_id,
            loom_types::KeepAlternativeCommand { candidate_id },
        )
        .map_err(IpcFailure::store)?;
    let mut receipt = Receipt::from(outcome.receipt);
    receipt.request_fingerprint = Some(outcome.request_fingerprint.to_string());
    receipt.replayed = outcome.replayed;
    Ok(receipt)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn candidate_promote(
    project_id: String,
    session_id: String,
    command_id: String,
    candidate_id: String,
    expected_source_revision_id: String,
    expected_visible_blob_id: String,
    state: State<'_, PluginState>,
) -> Result<Receipt, IpcFailure> {
    let command_id = parse_command_id(&command_id)?;
    let candidate_id = parse_candidate_id(&candidate_id)?;
    let expected_source_revision_id =
        expected_source_revision_id
            .parse::<RevisionId>()
            .map_err(|_| {
                IpcFailure::new(
                    "invalid_revision_id",
                    "source revision ID is not a valid ULID",
                    false,
                )
            })?;
    let expected_visible_blob_id = expected_visible_blob_id.parse::<BlobId>().map_err(|_| {
        IpcFailure::new(
            "invalid_blob_id",
            "visible blob ID is not a valid SHA-256 digest",
            false,
        )
    })?;
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    let outcome = store
        .promote_candidate_with_command(
            command_id,
            PromoteCandidateCommand {
                candidate_id,
                expected_source_revision_id,
                expected_visible_blob_id,
            },
        )
        .map_err(IpcFailure::store)?;
    let request_fingerprint = outcome.request_fingerprint.to_string();
    let replayed = outcome.replayed;
    let visible_projection = outcome.visible_projection;
    let mut receipt = Receipt::from(outcome.save.receipt);
    receipt.result_revision_id = Some(outcome.save.revision_id.to_string());
    receipt.result_blob_id = Some(outcome.save.blob_id.to_string());
    receipt.request_fingerprint = Some(request_fingerprint);
    receipt.replayed = replayed;
    receipt.visible_projection = Some(visible_projection);
    Ok(receipt)
}

fn parse_command_id(value: &str) -> Result<CommandId, IpcFailure> {
    value.parse::<CommandId>().map_err(|_| {
        IpcFailure::new(
            "invalid_command_id",
            "command ID is not a valid ULID",
            false,
        )
    })
}

fn parse_candidate_id(value: &str) -> Result<CandidateId, IpcFailure> {
    value.parse::<CandidateId>().map_err(|_| {
        IpcFailure::new(
            "invalid_candidate_id",
            "candidate ID is not a valid ULID",
            false,
        )
    })
}

fn parse_document_id(value: &str) -> Result<DocumentId, IpcFailure> {
    value.parse::<DocumentId>().map_err(|_| {
        IpcFailure::new(
            "invalid_document_id",
            "document ID is not a valid ULID",
            false,
        )
    })
}

fn parse_generation_run_id(value: &str) -> Result<GenerationRunId, IpcFailure> {
    value.parse::<GenerationRunId>().map_err(|_| {
        IpcFailure::new(
            "invalid_generation_run_id",
            "generation run ID is not a valid ULID",
            false,
        )
    })
}

fn emit_desktop_event<R: Runtime>(
    app: &AppHandle<R>,
    identity: &GenerationFamilyIdentity,
    event: LoomEvent,
) -> Result<(), IpcFailure> {
    app.emit(
        "loom://generation",
        DesktopLoomEvent {
            project_id: identity.project_id.to_string(),
            session_id: identity.session_id.to_string(),
            document_id: identity.document_id.to_string(),
            request_id: identity.request_id.clone(),
            event,
        },
    )
    .map_err(|error| {
        IpcFailure::new(
            "generation_event_emit_failed",
            format!("the desktop could not publish generation state: {error}"),
            true,
        )
    })
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn application_close<R: Runtime>(
    window: WebviewWindow<R>,
    state: State<'_, PluginState>,
) -> Result<(), IpcFailure> {
    if state
        .generations
        .active_branch_count()
        .map_err(|error| IpcFailure::generation_registry(&error))?
        != 0
    {
        return Err(IpcFailure::new(
            "generation_active",
            "cancel or finish every active strand before closing Loom",
            true,
        ));
    }
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
    let project_id_typed = require_bound_store(&mut session, &project_id, &session_id)?
        .manifest()
        .project_id;
    let session_id_typed = session.active_session_id.ok_or_else(|| {
        IpcFailure::new(
            "corrupt_project_session",
            "the live project session is missing its session ID",
            false,
        )
    })?;
    session.agency.set_focus_mode(enabled);
    drop(session);
    if enabled {
        state
            .generations
            .cancel_session(project_id_typed, session_id_typed)
            .map_err(|error| IpcFailure::generation_registry(&error))?;
    }
    Ok(())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn suggestions_set(
    project_id: String,
    session_id: String,
    enabled: bool,
    state: State<'_, PluginState>,
) -> Result<(), IpcFailure> {
    let mut session = lock_session(&state)?;
    let project_id_typed = require_bound_store(&mut session, &project_id, &session_id)?
        .manifest()
        .project_id;
    let session_id_typed = session.active_session_id.ok_or_else(|| {
        IpcFailure::new(
            "corrupt_project_session",
            "the live project session is missing its session ID",
            false,
        )
    })?;
    session.agency.set_automation_enabled(enabled);
    drop(session);
    if !enabled {
        state
            .generations
            .cancel_session(project_id_typed, session_id_typed)
            .map_err(|error| IpcFailure::generation_registry(&error))?;
    }
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

fn lock_session(state: &PluginState) -> Result<std::sync::MutexGuard<'_, Session>, IpcFailure> {
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

fn lock_session_internal(
    state: &PluginState,
) -> Result<std::sync::MutexGuard<'_, Session>, IpcFailure> {
    state.session.lock().map_err(|_| {
        IpcFailure::new(
            "session_poisoned",
            "the project session entered an invalid state; restart Loom",
            false,
        )
    })
}

fn lock_model_registry<'a>(
    state: &'a State<'_, PluginState>,
) -> Result<std::sync::MutexGuard<'a, ModelRegistry>, IpcFailure> {
    state.model.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => IpcFailure::new(
            "model_registry_busy",
            "another bounded model operation is still running; retry shortly",
            true,
        ),
        TryLockError::Poisoned(_) => IpcFailure::new(
            "model_registry_poisoned",
            "the model registry entered an invalid state; restart Loom",
            false,
        ),
    })
}

fn lock_model_lifecycle<'a>(
    state: &'a State<'_, PluginState>,
) -> Result<std::sync::MutexGuard<'a, ()>, IpcFailure> {
    state
        .model_lifecycle
        .try_lock()
        .map_err(|error| match error {
            TryLockError::WouldBlock => IpcFailure::new(
                "model_lifecycle_busy",
                "another bounded model lifecycle operation is still running; retry shortly",
                true,
            ),
            TryLockError::Poisoned(_) => IpcFailure::new(
                "model_lifecycle_poisoned",
                "the model lifecycle entered an invalid state; restart Loom",
                false,
            ),
        })
}

fn ensure_no_active_generations(
    state: &State<'_, PluginState>,
    action: &str,
) -> Result<(), IpcFailure> {
    if state
        .generations
        .active_branch_count()
        .map_err(|error| IpcFailure::generation_registry(&error))?
        == 0
    {
        return Ok(());
    }
    Err(IpcFailure::new(
        "generation_active",
        format!("finish or cancel active strands before {action}"),
        true,
    ))
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

    #[derive(Debug, Default)]
    struct RecordingCancellation {
        branches: Mutex<Vec<BranchId>>,
        signal: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    }

    impl BranchCancellation for RecordingCancellation {
        fn cancel_branch(&self, branch_id: BranchId) -> bool {
            self.branches
                .lock()
                .expect("recording cancellation lock")
                .push(branch_id);
            if let Some(signal) = self.signal.lock().expect("signal lock").take() {
                let _ = signal.send(());
            }
            true
        }
    }

    fn start_persisted_test_generation(
        store: &mut ProjectStore,
    ) -> (GenerationRunId, BranchId, DocumentId) {
        let initial = store
            .read_document(INITIAL_DOCUMENT)
            .expect("read initial manuscript");
        store
            .save_document_if_source(
                INITIAL_DOCUMENT,
                DocumentContent::Prose("Once ".to_owned()),
                "establish generation prefix",
                initial.revision_id,
                initial.blob_id,
            )
            .expect("save generation prefix");
        let loaded = store
            .read_document(INITIAL_DOCUMENT)
            .expect("read generation prefix");
        let environment = ModelEnvironment {
            environment_id: loom_types::ModelEnvironmentId::digest(b"test-close-environment"),
            model_identifier: "test-close-model".to_owned(),
            model_fingerprint: BlobId::digest(b"test-close-model"),
            tokenizer_fingerprint: BlobId::digest(b"test-close-tokenizer"),
            backend_identifier: "test-close-backend".to_owned(),
            capabilities: serde_json::json!({"completion": true}),
        };
        let environment_artifact = store
            .record_model_environment(&environment)
            .expect("record test environment")
            .artifact_id;
        let prompt_blob = store
            .store_provenance_blob(loaded.text.as_bytes())
            .expect("store test prompt");
        let prompt_artifact = store
            .record_prompt_recipe(&PromptRecipe {
                mode: PromptMode::Completion,
                exact_prompt_blob_id: prompt_blob,
                exact_prompt_token_ids: None,
                ordered_input_artifact_ids: vec![loaded.artifact_id],
                prompt_token_count: None,
            })
            .expect("record test prompt recipe")
            .artifact_id;
        let context_artifact = store
            .record_context_recipe(&ContextRecipe {
                source_revision_id: loaded.revision_id,
                ordered_source_artifact_ids: Vec::new(),
                token_budget: 128,
                retrieval_evidence_blob_id: None,
            })
            .expect("record test context")
            .artifact_id;
        let policy_artifact = store
            .record_authority_policy(&AuthorityPolicy {
                policy_version: 1,
                writer_environment_artifact_ids: vec![environment_artifact],
                critic_environment_artifact_ids: Vec::new(),
            })
            .expect("record test authority")
            .artifact_id;
        let run_id = GenerationRunId::new();
        let branch_id = BranchId::new();
        let cursor = u64::try_from(loaded.text.len()).expect("test prefix length");
        store
            .start_generation(GenerationStart {
                run_id,
                branch_id,
                document_id: loaded.document_id,
                source_revision_id: loaded.revision_id,
                target_range: ByteRange::new(cursor, cursor).expect("test target range"),
                model_environment_artifact_id: environment_artifact,
                prompt_recipe_artifact_id: prompt_artifact,
                context_recipe_artifact_id: context_artifact,
                authority_policy_artifact_id: policy_artifact,
                seed: 7,
                sampling: serde_json::json!({"temperature": 0.8}),
            })
            .expect("persist queued generation");
        (run_id, branch_id, loaded.document_id)
    }

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
    fn generation_seeds_are_deterministic_and_branch_specific() {
        let command_id = CommandId::new();
        assert_eq!(
            generation_seed(command_id, 0),
            generation_seed(command_id, 0)
        );
        assert_ne!(
            generation_seed(command_id, 0),
            generation_seed(command_id, 1)
        );
    }

    #[test]
    fn branch_snapshot_never_presents_an_orphan_as_live_generation() {
        let record = StoredBranchRecord {
            run_id: GenerationRunId::new(),
            branch_id: BranchId::new(),
            document_id: DocumentId::new(),
            source_revision_id: RevisionId::new(),
            target_range: ByteRange::new(7, 7).expect("target range"),
            model_identifier: "sha256:model".to_string(),
            seed: 42,
            status: StoredBranchStatus::Interrupted,
            candidate_id: None,
            output_text: None,
            output_blob_id: None,
            output_byte_len: None,
            error: None,
            selection: None,
            created_at_ms: 12,
        };
        let interrupted = branch_snapshot(record.clone(), false);
        assert_eq!(interrupted.status, "interrupted");
        let live = branch_snapshot(record, true);
        assert_eq!(live.status, "generating");
    }

    #[test]
    fn branch_cursor_preserves_u64_sequence_as_decimal_text() {
        let cursor = BranchPageCursor {
            sequence: u64::MAX,
            run_id: GenerationRunId::new(),
        };
        let snapshot = BranchCursorSnapshot::from(cursor);
        let json = serde_json::to_value(&snapshot).expect("serialize branch cursor");
        assert_eq!(json["sequence"], u64::MAX.to_string());
        assert_eq!(
            BranchPageCursor::try_from(snapshot).expect("parse branch cursor"),
            cursor
        );
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
    fn default_project_opens_directly_and_reuses_the_same_plain_text_workspace() {
        let temporary = tempfile::tempdir().expect("temporary app data");
        let root = temporary.path().join(DEFAULT_PROJECT_DIRECTORY);
        let first = open_or_initialize_default_project(&root).expect("create default project");
        let project_id = first.manifest().project_id;
        let document = first
            .read_document(INITIAL_DOCUMENT)
            .expect("read initial manuscript");
        assert_eq!(document.text, "");
        assert_eq!(
            std::fs::read_to_string(root.join(INITIAL_DOCUMENT)).expect("read visible manuscript"),
            ""
        );
        drop(first);

        let second = open_or_initialize_default_project(&root).expect("reopen default project");
        assert_eq!(second.manifest().project_id, project_id);
        assert_eq!(
            second
                .read_document(INITIAL_DOCUMENT)
                .expect("read reopened manuscript")
                .text,
            ""
        );
    }

    #[test]
    fn default_project_repairs_interruption_after_manifest_before_document() {
        let temporary = tempfile::tempdir().expect("temporary app data");
        let root = temporary.path().join(DEFAULT_PROJECT_DIRECTORY);
        let (store, _) =
            ProjectStore::initialize(&root, "My Writing").expect("initialize sidecar only");
        let project_id = store.manifest().project_id;
        assert!(store.list_documents().expect("list documents").is_empty());
        drop(store);

        let repaired = open_or_initialize_default_project(&root)
            .expect("repair interrupted default initialization");
        assert_eq!(repaired.manifest().project_id, project_id);
        assert_eq!(
            repaired
                .read_document(INITIAL_DOCUMENT)
                .expect("read repaired manuscript")
                .text,
            ""
        );
        assert_eq!(
            std::fs::read(root.join(INITIAL_DOCUMENT)).expect("read visible manuscript"),
            b""
        );
    }

    #[test]
    fn default_project_adopts_exact_visible_bytes_when_sidecar_is_absent() {
        let temporary = tempfile::tempdir().expect("temporary app data");
        let root = temporary.path().join(DEFAULT_PROJECT_DIRECTORY);
        let manuscript = root.join(INITIAL_DOCUMENT);
        std::fs::create_dir_all(manuscript.parent().expect("manuscript parent"))
            .expect("create manuscript directory");
        let original = "  opening\r\n\r\ncafé\t \r\n".as_bytes();
        std::fs::write(&manuscript, original).expect("write surviving manuscript");

        let recovered =
            open_or_initialize_default_project(&root).expect("adopt surviving manuscript");
        assert_eq!(
            recovered
                .read_document(INITIAL_DOCUMENT)
                .expect("read adopted manuscript")
                .text
                .as_bytes(),
            original
        );
        assert_eq!(
            std::fs::read(&manuscript).expect("read unchanged visible manuscript"),
            original
        );
        assert!(root.join(".loom/project.json").is_file());
    }

    #[test]
    fn default_project_recovers_after_complete_sidecar_loss_without_rewriting_text() {
        let temporary = tempfile::tempdir().expect("temporary app data");
        let root = temporary.path().join(DEFAULT_PROJECT_DIRECTORY);
        let first = open_or_initialize_default_project(&root).expect("create default project");
        let first_project_id = first.manifest().project_id;
        drop(first);
        let original = "surviving\r\nexternal  text\r\n".as_bytes();
        std::fs::write(root.join(INITIAL_DOCUMENT), original).expect("write surviving text");
        std::fs::rename(root.join(".loom"), root.join("lost-sidecar"))
            .expect("preserve lost sidecar outside the active location");

        let recovered =
            open_or_initialize_default_project(&root).expect("recreate sidecar from manuscript");
        assert_ne!(recovered.manifest().project_id, first_project_id);
        assert_eq!(
            recovered
                .read_document(INITIAL_DOCUMENT)
                .expect("read recovered manuscript")
                .text
                .as_bytes(),
            original
        );
        assert_eq!(
            std::fs::read(root.join(INITIAL_DOCUMENT)).expect("read preserved manuscript"),
            original
        );
    }

    #[test]
    fn default_project_never_recreates_a_registered_external_deletion() {
        let temporary = tempfile::tempdir().expect("temporary app data");
        let root = temporary.path().join(DEFAULT_PROJECT_DIRECTORY);
        let store = open_or_initialize_default_project(&root).expect("create default project");
        let project_id = store.manifest().project_id;
        drop(store);
        std::fs::remove_file(root.join(INITIAL_DOCUMENT)).expect("simulate external deletion");

        let reopened =
            open_or_initialize_default_project(&root).expect("reopen deleted manuscript project");
        assert_eq!(reopened.manifest().project_id, project_id);
        assert!(
            reopened
                .list_documents()
                .expect("list registered document")
                .iter()
                .any(|document| document.relative_path == INITIAL_DOCUMENT)
        );
        assert!(!root.join(INITIAL_DOCUMENT).exists());
    }

    #[cfg(unix)]
    #[test]
    fn default_project_refuses_visible_symlink_before_creating_sidecar() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().expect("temporary app data");
        let root = temporary.path().join(DEFAULT_PROJECT_DIRECTORY);
        let manuscript = root.join(INITIAL_DOCUMENT);
        std::fs::create_dir_all(manuscript.parent().expect("manuscript parent"))
            .expect("create manuscript directory");
        let outside = temporary.path().join("outside.md");
        let outside_bytes = b"outside remains untouched\n";
        std::fs::write(&outside, outside_bytes).expect("write outside target");
        symlink(&outside, &manuscript).expect("create visible symlink");

        let error = open_or_initialize_default_project(&root)
            .expect_err("default project must refuse visible symlink");
        assert_eq!(error.code, "default_document_symlink");
        assert!(!root.join(".loom").exists());
        assert_eq!(
            std::fs::read(&outside).expect("outside target survives"),
            outside_bytes
        );
    }

    #[test]
    fn close_cancels_active_family_waits_for_terminal_release_and_replays() {
        let temporary = tempfile::tempdir().expect("temporary parent");
        let root = temporary.path().join("Closing Novel");
        let mut store = initialize_project(&root, "Closing Novel".to_owned()).expect("initialize");
        let project_id = store.manifest().project_id;
        let session_id = CommandId::new();
        let (run_id, branch_id, document_id) = start_persisted_test_generation(&mut store);
        let state = Arc::new(PluginState::default());
        {
            let mut session = state.session.lock().expect("session lock");
            session.phase = SessionPhase::Open;
            session.store = Some(store);
            session.active_session_id = Some(session_id);
            session.agency.set_automation_enabled(true);
        }
        let request_id = "close-active-family".to_owned();
        let (signal, cancelled) = std::sync::mpsc::channel();
        let cancellation = Arc::new(RecordingCancellation {
            branches: Mutex::new(Vec::new()),
            signal: Mutex::new(Some(signal)),
        });
        state
            .generations
            .register(loom_host::GenerationFamilyRegistration {
                identity: GenerationFamilyIdentity {
                    request_id: request_id.clone(),
                    project_id,
                    session_id,
                    document_id,
                },
                branches: vec![(run_id, branch_id)],
                cancellation: cancellation.clone(),
            })
            .expect("register active family");
        let completing_state = Arc::clone(&state);
        let completing_identity = GenerationFamilyIdentity {
            request_id: request_id.clone(),
            project_id,
            session_id,
            document_id,
        };
        let completion = std::thread::spawn(move || {
            cancelled
                .recv_timeout(Duration::from_secs(1))
                .expect("close must request cancellation");
            terminalize_open_runs(
                &completing_state,
                &completing_identity,
                &[(run_id, branch_id)],
                "cancelled while closing",
            )
            .expect("persist terminal generation before release");
            release_family_after_terminal_persistence(
                &completing_state,
                &completing_identity,
                &[(run_id, branch_id)],
            )
            .expect("release terminal family");
        });

        let command_id = CommandId::new();
        let receipt = close_project_with_wait(
            &state,
            project_id.to_string(),
            session_id.to_string(),
            command_id,
            Duration::from_secs(1),
        )
        .expect("close after cancellation terminalizes");
        completion.join().expect("completion thread");
        assert_eq!(
            *cancellation.branches.lock().expect("cancelled branches"),
            vec![branch_id]
        );
        assert_eq!(receipt.command_id, command_id.to_string());
        assert_eq!(
            state.session.lock().expect("session lock").phase,
            SessionPhase::Closed
        );
        let reopened = ProjectStore::open(&root).expect("reopen closed project");
        assert_eq!(
            reopened
                .generation_terminal_count(run_id)
                .expect("count durable terminal"),
            1
        );
        drop(reopened);

        let replay = close_project_with_wait(
            &state,
            project_id.to_string(),
            session_id.to_string(),
            command_id,
            Duration::ZERO,
        )
        .expect("same close command replays");
        assert_eq!(replay.command_id, receipt.command_id);
        assert_eq!(replay.closed_at_unix_ms, receipt.closed_at_unix_ms);
    }

    #[test]
    fn close_timeout_is_bounded_and_leaves_session_revoked_for_exact_retry() {
        let temporary = tempfile::tempdir().expect("temporary parent");
        let root = temporary.path().join("Slow Closing Novel");
        let store = initialize_project(&root, "Slow Closing Novel".to_owned()).expect("initialize");
        let project_id = store.manifest().project_id;
        let session_id = CommandId::new();
        let state = PluginState::default();
        {
            let mut session = state.session.lock().expect("session lock");
            session.phase = SessionPhase::Open;
            session.store = Some(store);
            session.active_session_id = Some(session_id);
            session.agency.set_automation_enabled(true);
        }
        let run_id = GenerationRunId::new();
        let branch_id = BranchId::new();
        let cancellation = Arc::new(RecordingCancellation::default());
        state
            .generations
            .register(loom_host::GenerationFamilyRegistration {
                identity: GenerationFamilyIdentity {
                    request_id: "slow-close".to_owned(),
                    project_id,
                    session_id,
                    document_id: DocumentId::new(),
                },
                branches: vec![(run_id, branch_id)],
                cancellation: cancellation.clone(),
            })
            .expect("register active family");

        let error = close_project_with_wait(
            &state,
            project_id.to_string(),
            session_id.to_string(),
            CommandId::new(),
            Duration::ZERO,
        )
        .expect_err("zero wait must return a retryable bounded result");
        assert_eq!(error.code, "generation_cancellation_in_progress");
        assert!(error.retryable);
        let session = state.session.lock().expect("session lock");
        assert_eq!(session.phase, SessionPhase::Open);
        assert!(session.agency.focus_mode());
        assert!(!session.agency.automation_enabled());
        drop(session);
        assert_eq!(
            *cancellation.branches.lock().expect("cancelled branches"),
            vec![branch_id]
        );
        state
            .generations
            .complete_family("slow-close")
            .expect("release test family");
    }

    #[test]
    fn close_repairs_recorded_terminal_persistence_failure_before_releasing_store() {
        let temporary = tempfile::tempdir().expect("temporary parent");
        let root = temporary.path().join("Persistence Repair Novel");
        let mut store =
            initialize_project(&root, "Persistence Repair Novel".to_owned()).expect("initialize");
        let project_id = store.manifest().project_id;
        let session_id = CommandId::new();
        let (run_id, branch_id, document_id) = start_persisted_test_generation(&mut store);
        let state = PluginState::default();
        {
            let mut session = state.session.lock().expect("session lock");
            session.phase = SessionPhase::Open;
            session.store = Some(store);
            session.active_session_id = Some(session_id);
            session.agency.set_automation_enabled(true);
        }
        let request_id = "persistence-repair";
        state
            .generations
            .register(loom_host::GenerationFamilyRegistration {
                identity: GenerationFamilyIdentity {
                    request_id: request_id.to_owned(),
                    project_id,
                    session_id,
                    document_id,
                },
                branches: vec![(run_id, branch_id)],
                cancellation: Arc::new(RecordingCancellation::default()),
            })
            .expect("register failed-persistence family");
        state
            .generations
            .mark_terminal_persistence_failure(request_id, "simulated SQLite interruption")
            .expect("record persistence failure");

        close_project_with_wait(
            &state,
            project_id.to_string(),
            session_id.to_string(),
            CommandId::new(),
            Duration::ZERO,
        )
        .expect("close repairs terminal before releasing project");
        assert_eq!(
            state.session.lock().expect("session lock").phase,
            SessionPhase::Closed
        );
        let reopened = ProjectStore::open(&root).expect("reopen repaired project");
        assert_eq!(
            reopened
                .generation_terminal_count(run_id)
                .expect("count repaired terminal"),
            1
        );
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
