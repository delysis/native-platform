#![forbid(unsafe_code)]

mod draft;
mod error;
mod file_io;
mod generation;
mod paths;
mod provenance;
mod reconciliation;
mod schema;
mod store;

pub use draft::{TransientDraft, TransientDraftClaim, TransientDraftWriteOutcome};
pub use error::{Result, StoreError};
pub use generation::{
    CancelGenerationOutcome, GenerationStarted, KeepAlternativeOutcome, PromotionOutcome,
    RecordedArtifact, TerminalCandidateInput, TerminalCandidateOutcome,
};
pub use provenance::{
    IdempotentSaveOutcome, MAX_EDIT_DIFF_WINDOW_BYTES, MAX_EDIT_DIFF_WINDOW_CHARACTERS,
    MAX_EDIT_DIFF_WORK, MAX_REVISION_SEGMENTS, ProvenanceSegment, RevisionProvenance,
};
pub use reconciliation::{ExternalReconciliationOutcome, ExternalReconciliationRequest};
pub use schema::{CURRENT_SCHEMA_VERSION, CURRENT_STORE_SCHEMA_VERSION};
pub use store::{
    DocumentReconciliationSnapshot, DocumentSummary, LoadedDocument, MAX_DOCUMENT_BYTES,
    ProjectStore, RecoveryConflict, RecoveryReport, SaveOutcome, StoreCounts,
    VisibleDocumentSnapshot, VisibleProjectionState,
};
