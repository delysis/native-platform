#![forbid(unsafe_code)]

mod draft;
mod error;
mod file_io;
mod generation;
mod paths;
mod provenance;
mod reconciliation;
mod research_admission;
mod research_archive;
mod research_benchmark;
mod research_campaign_authority;
mod research_evaluation;
mod research_execution;
mod research_journal;
mod research_prompt_evidence;
mod research_session;
mod research_story;
mod schema;
mod store;

pub use draft::{TransientDraft, TransientDraftClaim, TransientDraftWriteOutcome};
pub use error::{Result, StoreError};
pub use generation::{
    BranchPageCursor, CancelGenerationOutcome, GenerationFamilyStarted, GenerationStarted,
    GenerationTerminalEvidence, INTERRUPTED_GENERATION_ERROR, KeepAlternativeOutcome,
    MAX_BRANCH_BODY_BYTES, MAX_BRANCH_PAGE_SIZE, PromotionOutcome, RecordedArtifact,
    StoredBranchBody, StoredBranchPage, StoredBranchRecord, StoredBranchStatus,
    StoredBranchSummary, StoredGenerationTerminalEvidence, TerminalCandidateInput,
    TerminalCandidateOutcome, TerminalEvidenceInput, TerminalGenerationInput,
    TerminalGenerationOutcome,
};
pub use provenance::{
    IdempotentSaveOutcome, MAX_EDIT_DIFF_WINDOW_BYTES, MAX_EDIT_DIFF_WINDOW_CHARACTERS,
    MAX_EDIT_DIFF_WORK, MAX_REVISION_SEGMENTS, ProvenanceSegment, RevisionProvenance,
};
pub use reconciliation::{ExternalReconciliationOutcome, ExternalReconciliationRequest};
pub use research_admission::{
    AdmittedCandidateAssembly, AdmittedCandidateProjection, AdmittedGeneratedSpan,
    AdmittedModelCall, AdoptedInferenceBatch, FrozenPromptSourceLease, MixedAuthorshipAdmission,
    PromotionSubjectLease, RecordedPromotionAuthority, RecordedPromotionRequest,
    ReplayedInferenceBatchEvidence, ResearchAdmissionRecordId, VerifiedUserPresence,
};
pub use research_archive::{
    DiagnosticArchiveSnapshotPersistence, PersistedDiagnosticArchiveSnapshot,
};
pub use research_benchmark::{
    PersistedBenchmarkJournal, PersistedBenchmarkJournalStatus, PersistedDiagnosticBenchmarkResult,
    PersistedDiagnosticBenchmarkSeal, PersistedDiagnosticHumanLabelArchive,
};
pub use research_campaign_authority::{
    CampaignPoolCandidateLease, CheckedCampaignNestedPoolEvidence,
    MAX_VERIFIED_CAMPAIGN_POOL_CANDIDATES,
};
pub use research_evaluation::{
    DiagnosticDescriptorAxis, DiagnosticEvaluationKind, DiagnosticEvaluationObservation,
    DiagnosticEvaluationReceiptPersistence, DiagnosticEvaluationTaskPersistence,
    DiagnosticPairwiseAssignmentPersistence, MAX_DIAGNOSTIC_EVALUATION_PACKET_BYTES,
    MAX_DIAGNOSTIC_EVALUATION_RESPONSE_BYTES, PersistedDiagnosticEvaluationReceipt,
    PersistedDiagnosticEvaluationTask, PersistedDiagnosticPairwiseAssignment,
    VerifiedEvaluationCandidateLease, VerifiedEvaluationTaskPersistence,
    VerifiedPairwiseAssignmentPersistence, VerifiedPreferenceSource, VerifiedPreferenceSourceLease,
};
pub use research_execution::{
    FrozenCampaignPersistence, FrozenCampaignTopologyPersistence,
    FrozenCampaignTrialTopologyPersistence, FrozenStagePersistence, FrozenTrialPersistence,
    MAX_PERSISTED_CAMPAIGN_TRIAL_DEPENDENCIES, MAX_PERSISTED_CAMPAIGN_TRIALS,
    MAX_RESEARCH_EXECUTION_RECORD_BYTES, PersistedFrozenCampaign, PersistedFrozenCampaignTopology,
    PersistedFrozenTrial, PersistedResearchExecutionRecord, PersistedTrialRun,
    ResearchBudgetMaximum, ResearchExecutionRecordKind, StandaloneTrialRunPersistence,
};
pub use research_journal::{
    CampaignJournalEventPersistence, CampaignJournalMutation, CampaignTrialOutcome,
    MAX_PERSISTED_CAMPAIGN_JOURNAL_EVENTS, MAX_PERSISTED_TRIAL_JOURNAL_EVENTS,
    MAX_RESEARCH_JOURNAL_EVENT_BYTES, MAX_RESEARCH_JOURNAL_TOTAL_BYTES,
    PersistedResearchJournalEvent, ResearchJournalBudget, ResearchJournalWriter,
    SearchDecisionPersistenceKind, TrialJournalEventPersistence, TrialJournalMutation,
    TrialStageOutcome,
};
pub use research_prompt_evidence::{
    DiagnosticPromptMaskKind, PersistedDiagnosticBacktranslationAcceptance,
    PersistedDiagnosticBacktranslationAudition, PersistedDiagnosticBacktranslationProposal,
    PersistedDiagnosticPromptMask, VerifiedBacktranslationEvaluatorLease,
};
pub use research_session::{
    ExclusiveResearchSessionLease, PersistedCampaignSubjectSnapshot,
    PersistedCampaignTrialSnapshot, PersistedResearchSubjectSnapshot, PersistedTrialStageSnapshot,
    PersistedTrialSubjectSnapshot, ResearchSessionKind,
};
pub use research_story::{PersistedDiagnosticStoryGraph, PersistedDiagnosticStoryState};
pub use schema::{CURRENT_SCHEMA_VERSION, CURRENT_STORE_SCHEMA_VERSION};
pub use store::{
    DocumentReconciliationSnapshot, DocumentSummary, LoadedDocument, MAX_DOCUMENT_BYTES,
    ProjectStore, RecoveryConflict, RecoveryReport, SaveOutcome, StoreCounts,
    VisibleDocumentSnapshot, VisibleProjectionState,
};
