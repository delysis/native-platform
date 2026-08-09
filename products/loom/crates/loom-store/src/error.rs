use std::path::PathBuf;

use loom_types::{
    ArtifactId, BlobId, BranchId, CandidateId, CommandId, DocumentKind, GenerationRunId,
    ModelEnvironmentId, RevisionId,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("I/O failure: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQLite failure: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("JSON failure: {0}")]
    Json(#[from] serde_json::Error),
    #[error("document projection failed: {0}")]
    Document(#[from] loom_document::DocumentError),
    #[error("research call evidence failed validation: {0}")]
    ResearchCall(#[from] loom_research_types::CallError),
    #[error("research assembly evidence failed validation: {0}")]
    ResearchAssembly(#[from] loom_research_types::AssemblyError),
    #[error("research admission failed: {0}")]
    ResearchAdmission(String),
    #[error("could not obtain entropy for a project-store session: {0}")]
    SessionEntropy(String),
    #[error("project path is not valid UTF-8: {0:?}")]
    NonUtf8Path(PathBuf),
    #[error("unsafe project-relative path `{0}`")]
    UnsafeRelativePath(String),
    #[error("path traverses a symbolic link: {0:?}")]
    SymbolicLink(PathBuf),
    #[error("path is not a directory: {0:?}")]
    NotDirectory(PathBuf),
    #[error("path is not a regular file: {0:?}")]
    NotRegularFile(PathBuf),
    #[error("project is already initialized at {0:?}")]
    AlreadyInitialized(PathBuf),
    #[error("project is already open in another Loom process: {0:?}")]
    ProjectAlreadyOpen(PathBuf),
    #[error("no Loom project manifest exists at {0:?}")]
    NotAProject(PathBuf),
    #[error("unsupported project manifest format `{0}`")]
    UnsupportedFormat(String),
    #[error("project schema {found} is newer than supported schema {supported}")]
    UnsupportedSchema { found: u32, supported: u32 },
    #[error("project name must contain 1 to {max_bytes} UTF-8 bytes")]
    InvalidProjectName { max_bytes: usize },
    #[error("reason must contain at most {max_bytes} UTF-8 bytes")]
    ReasonTooLong { max_bytes: usize },
    #[error("document has {actual_bytes} bytes; limit is {max_bytes} bytes")]
    DocumentTooLarge { actual_bytes: u64, max_bytes: u64 },
    #[error("document `{path}` is registered as {stored:?}, not {requested:?}")]
    DocumentKindMismatch {
        path: String,
        stored: DocumentKind,
        requested: DocumentKind,
    },
    #[error("visible file `{path}` changed while outbox entry {outbox_id} was pending")]
    VisibleFileConflict { outbox_id: i64, path: String },
    #[error("visible file `{0}` already exists")]
    VisibleFileAlreadyExists(String),
    #[error("document `{0}` is already registered")]
    DocumentAlreadyExists(String),
    #[error("visible file `{0}` has uncheckpointed changes")]
    UncheckpointedVisibleChange(String),
    #[error("visible file `{0}` was deleted; deletion reconciliation requires a distinct command")]
    ExternalVisibleFileDeleted(String),
    #[error("external visible file `{0}` is not valid UTF-8")]
    ExternalVisibleInvalidUtf8(String),
    #[error("external visible blob is {actual}, expected {expected}")]
    ExternalVisibleBlobMismatch { expected: BlobId, actual: BlobId },
    #[error("content blob {blob_id} is missing at {path:?}")]
    MissingBlob { blob_id: BlobId, path: PathBuf },
    #[error("content blob {0} is not registered")]
    UnregisteredBlob(BlobId),
    #[error("content blob at {path:?} hashes to {actual}, expected {expected}")]
    CorruptBlob {
        path: PathBuf,
        expected: BlobId,
        actual: BlobId,
    },
    #[error("database contains invalid data: {0}")]
    CorruptDatabase(String),
    #[error("document `{0}` has no active revision")]
    NoActiveRevision(String),
    #[error("active revision is {actual}, expected {expected}")]
    SourceRevisionMismatch {
        expected: RevisionId,
        actual: RevisionId,
    },
    #[error("active visible blob is {actual}, expected {expected}")]
    SourceBlobMismatch { expected: BlobId, actual: BlobId },
    #[error("artifact {artifact_id} is not a registered {expected_kind}")]
    ArtifactKindMismatch {
        artifact_id: ArtifactId,
        expected_kind: &'static str,
    },
    #[error(
        "model environment ID {environment_id} is already registered with different canonical content"
    )]
    ModelEnvironmentContentConflict { environment_id: ModelEnvironmentId },
    #[error("generation run {0} does not exist")]
    GenerationRunNotFound(GenerationRunId),
    #[error("branch page limit {requested} is outside the supported range 1 through {max}")]
    InvalidBranchPageLimit { requested: usize, max: usize },
    #[error("branch page cursor does not identify a run in the requested document")]
    InvalidBranchPageCursor,
    #[error("branch body limit {requested} is outside the supported range 1 through {max} bytes")]
    InvalidBranchBodyLimit { requested: u64, max: u64 },
    #[error(
        "branch body for generation run {run_id} has {actual_bytes} bytes; requested limit is {max_bytes} bytes"
    )]
    BranchBodyTooLarge {
        run_id: GenerationRunId,
        actual_bytes: u64,
        max_bytes: u64,
    },
    #[error("generation family must contain at least one run")]
    EmptyGenerationFamily,
    #[error("generation family repeats run ID {0}")]
    DuplicateGenerationRun(GenerationRunId),
    #[error("generation family repeats branch ID {0}")]
    DuplicateGenerationBranch(BranchId),
    #[error("every run in a generation family must share one document and source revision")]
    GenerationFamilySourceMismatch,
    #[error("candidate {0} does not exist")]
    CandidateNotFound(CandidateId),
    #[error(
        "legacy generation candidates are diagnostic evidence and cannot be promoted; promote an admitted projection with explicit user-presence authority"
    )]
    LegacyCandidateNotAdmitted,
    #[error("generation run {0} already has a terminal event")]
    GenerationAlreadyTerminal(GenerationRunId),
    #[error("completed generation requires a terminal candidate")]
    CompletedGenerationRequiresCandidate,
    #[error("candidate-ready events are created only by terminal candidate recording")]
    CandidateReadyRequiresTerminalCandidate,
    #[error("failed generation requires a non-empty error")]
    FailedGenerationRequiresError,
    #[error("model environment is a critic under the generation authority policy")]
    CriticCannotPromote,
    #[error("model environment is not assigned a role under the generation authority policy")]
    ModelRoleNotAssigned,
    #[error("authority policy must have distinct members and at least one writer")]
    InvalidAuthorityPolicy,
    #[error("generation target range is invalid for the source revision")]
    InvalidGenerationRange,
    #[error("generated bytes are not canonical for the target document kind")]
    NonCanonicalGeneratedText,
    #[error("request for command {command_id} does not match its recorded fingerprint")]
    IdempotencyConflict { command_id: CommandId },
    #[error(
        "edit diff exceeds the bounded {metric} budget: observed {actual}, limit {limit}; submit a validated editor changeset or checkpoint smaller edits"
    )]
    EditDiffBudgetExceeded {
        metric: &'static str,
        actual: u64,
        limit: u64,
    },
    #[error("revision has {actual} provenance segments; limit is {limit}")]
    RevisionSegmentLimitExceeded { actual: usize, limit: usize },
    #[error(
        "transient draft version does not match: expected {expected}, current version is {actual:?}"
    )]
    TransientDraftVersionConflict { expected: u64, actual: Option<u64> },
    #[error(
        "transient draft identity does not match: expected source {expected_source_revision_id} and blob {expected_blob_id}, current source is {actual_source_revision_id} and blob is {actual_blob_id}"
    )]
    TransientDraftIdentityMismatch {
        expected_source_revision_id: RevisionId,
        actual_source_revision_id: RevisionId,
        expected_blob_id: BlobId,
        actual_blob_id: BlobId,
    },
    #[error("request exceeds the {field} limit of {max_bytes} bytes")]
    ProvenancePayloadTooLarge {
        field: &'static str,
        max_bytes: usize,
    },
}

pub type Result<T> = std::result::Result<T, StoreError>;
