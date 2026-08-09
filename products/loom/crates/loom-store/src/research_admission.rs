use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use loom_inference::{
    BaseWriterBinding, CancelledBaseWriterDiagnosticParts, ExactPromptEvidence,
    MAX_BASE_PROMPT_BYTES, PersistedBindingEvidenceRef, PersistedCaseOutcomeRef,
    PersistedInferenceBatchRef, PersistedInferenceCaseRef, PersistedPromptEvidenceRef,
    PromptFormEvidence, PromptTokenPolicyEvidence, VerifiedBaseWriterCallParts,
    VerifiedCaseOutcomeParts, VerifiedDiagnosticParts, VerifiedInferenceOutcome,
    VerifiedInferenceOutcomeParts, VerifiedInferenceParts, verify_persisted_batch_evidence,
};
use loom_research_types::{
    CallEvidenceClass, CallScope, CallTerminal, CandidateAssemblyRecord, CandidateProjectionRecord,
    CompiledBaseCompletionPrompt, CompletionTailOrigin, ExactCallEvidence,
    FrozenBaseCompletionPrompt, GeneratedSpanOccurrenceRecord, JoinBefore,
    MAX_BACKEND_EVIDENCE_BYTES, MAX_BASE_WRITER_BATCH_CASES, MAX_FROZEN_PROMPT_SPECIFICATION_BYTES,
    MAX_GENERATED_TOKENS, MAX_RAW_OUTPUT_BYTES, MixedAuthorshipAssemblyRecord, ModelCall,
    ModelRole, NonEmptyByteRange, OperationGraph, OutputProjection, PipelineEligibility,
    PipelineOperationKind, PromotionAuthority, PromotionCommandRequest, PromotionSubject,
    TokenEvidence, UserPresenceKind, compile_manifest,
};
use loom_types::{BlobId, CommandId, ProjectId, now_unix_ms};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
#[cfg(test)]
use serde::Deserialize;

use crate::provenance::insert_blob_row;
use crate::store::StoreSessionNonce;
use crate::{ProjectStore, Result, StoreError};

#[cfg(test)]
const STRICT_RECEIPT_FORMAT: &str = "loom.native-base-writer-receipt.v1";
#[cfg(test)]
const STRICT_EVENT_STREAM_FORMAT: &str = "loom.native-call-events.v1";
#[cfg(test)]
const MAX_EVENT_COUNT: usize = 1_048_578;

/// Deterministic identifier for one persisted admission audit row.
///
/// This is neither a content hash nor runtime authority. Only opaque,
/// session-bound admission leases authorize downstream operations.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ResearchAdmissionRecordId(BlobId);

impl ResearchAdmissionRecordId {
    pub const fn as_blob_id(self) -> BlobId {
        self.0
    }
}

/// Non-serializable proof that the store replayed a complete native call.
///
/// The private exact evidence is intentional. A database row, a receipt hash,
/// or a caller-supplied evidence enum cannot recreate this lease.
pub struct AdmittedModelCall {
    session_nonce: StoreSessionNonce,
    call: ModelCall,
    raw_output: Vec<u8>,
    token_ids: Vec<u32>,
    token_byte_boundaries: Option<Vec<u64>>,
    verification_fingerprint: BlobId,
}

impl fmt::Debug for AdmittedModelCall {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdmittedModelCall")
            .field("call_id", &self.call.id())
            .field("raw_output_bytes", &self.raw_output.len())
            .field("token_count", &self.token_ids.len())
            .field(
                "token_byte_boundary_count",
                &self
                    .token_byte_boundaries
                    .as_ref()
                    .map_or(0, std::vec::Vec::len),
            )
            .field("verification_fingerprint", &self.verification_fingerprint)
            .finish_non_exhaustive()
    }
}

impl AdmittedModelCall {
    pub const fn call_id(&self) -> loom_research_types::ModelCallId {
        self.call.id()
    }
}

/// Atomic store adoption result for one consumed backend batch authority.
///
/// Completed calls carry session-bound leases. Cancelled calls are persisted
/// as diagnostics and can never appear in `admitted_calls`.
pub struct AdoptedInferenceBatch {
    session_nonce: StoreSessionNonce,
    verification_fingerprint: BlobId,
    admitted_calls: Vec<AdmittedModelCall>,
    cancelled_call_count: usize,
}

/// One-use proof that an inference-ready prompt was checked against the live
/// project before the backend call began.
///
/// This capability is deliberately neither cloneable nor serializable. Its
/// private source snapshot cannot be reconstructed from persisted rows.
///
/// ```compile_fail
/// fn needs_clone<T: Clone>() {}
/// needs_clone::<loom_store::FrozenPromptSourceLease>();
/// ```
///
/// ```compile_fail
/// fn needs_serialize<T: serde::Serialize>() {}
/// needs_serialize::<loom_store::FrozenPromptSourceLease>();
/// ```
///
/// ```compile_fail
/// fn needs_deserialize<T: serde::de::DeserializeOwned>() {}
/// needs_deserialize::<loom_store::FrozenPromptSourceLease>();
/// ```
pub struct FrozenPromptSourceLease {
    session_nonce: StoreSessionNonce,
    snapshot: FrozenPromptSourceSnapshot,
    frozen_at_ms: i64,
    freeze_fingerprint: BlobId,
}

impl fmt::Debug for FrozenPromptSourceLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FrozenPromptSourceLease")
            .field("project_id", &self.snapshot.project_id)
            .field("scope", &self.snapshot.scope)
            .field("frozen_at_ms", &self.frozen_at_ms)
            .field("freeze_fingerprint", &self.freeze_fingerprint)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for AdoptedInferenceBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdoptedInferenceBatch")
            .field("verification_fingerprint", &self.verification_fingerprint)
            .field("admitted_call_count", &self.admitted_calls.len())
            .field("cancelled_call_count", &self.cancelled_call_count)
            .finish_non_exhaustive()
    }
}

impl AdoptedInferenceBatch {
    pub const fn verification_fingerprint(&self) -> BlobId {
        self.verification_fingerprint
    }

    pub fn admitted_calls(&self) -> &[AdmittedModelCall] {
        &self.admitted_calls
    }

    pub const fn cancelled_call_count(&self) -> usize {
        self.cancelled_call_count
    }

    pub fn into_admitted_calls(self) -> Vec<AdmittedModelCall> {
        self.admitted_calls
    }

    pub(crate) fn belongs_to_session(&self, session_nonce: StoreSessionNonce) -> bool {
        self.session_nonce == session_nonce
            && self
                .admitted_calls
                .iter()
                .all(|call| call.session_nonce == session_nonce)
    }

    #[cfg(test)]
    pub(crate) const fn diagnostic_for_test(
        session_nonce: StoreSessionNonce,
        verification_fingerprint: BlobId,
    ) -> Self {
        Self {
            session_nonce,
            verification_fingerprint,
            admitted_calls: Vec::new(),
            cancelled_call_count: 0,
        }
    }
}

/// Read-only proof that one sealed inference batch was reconstructed from its
/// immutable manifests, prompt bytes, call records, terminals, and projections.
///
/// This value is intentionally not an admission lease and cannot authorize
/// assembly, manuscript mutation, or benchmark inclusion. Campaign/stage/
/// treatment existence is deliberately outside this diagnostic replay type.
pub struct ReplayedInferenceBatchEvidence {
    batch_fingerprint: BlobId,
    binding_fingerprint: BlobId,
    manifest_artifact_hash: BlobId,
    prompt_content_fingerprint: BlobId,
    compiled_prompt_fingerprint: BlobId,
    completed_call_count: usize,
    cancelled_call_count: usize,
}

impl fmt::Debug for ReplayedInferenceBatchEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReplayedInferenceBatchEvidence")
            .field("batch_fingerprint", &self.batch_fingerprint)
            .field("binding_fingerprint", &self.binding_fingerprint)
            .field("manifest_artifact_hash", &self.manifest_artifact_hash)
            .field(
                "prompt_content_fingerprint",
                &self.prompt_content_fingerprint,
            )
            .field(
                "compiled_prompt_fingerprint",
                &self.compiled_prompt_fingerprint,
            )
            .field("completed_call_count", &self.completed_call_count)
            .field("cancelled_call_count", &self.cancelled_call_count)
            .finish_non_exhaustive()
    }
}

impl ReplayedInferenceBatchEvidence {
    pub const fn batch_fingerprint(&self) -> BlobId {
        self.batch_fingerprint
    }

    pub const fn binding_fingerprint(&self) -> BlobId {
        self.binding_fingerprint
    }

    pub const fn manifest_artifact_hash(&self) -> BlobId {
        self.manifest_artifact_hash
    }

    pub const fn prompt_content_fingerprint(&self) -> BlobId {
        self.prompt_content_fingerprint
    }

    pub const fn compiled_prompt_fingerprint(&self) -> BlobId {
        self.compiled_prompt_fingerprint
    }

    pub const fn completed_call_count(&self) -> usize {
        self.completed_call_count
    }

    pub const fn cancelled_call_count(&self) -> usize {
        self.cancelled_call_count
    }
}

/// Non-serializable proof that a declared span was checked against an admitted
/// call and, when present, exact token-to-byte boundaries.
pub struct AdmittedGeneratedSpan {
    session_nonce: StoreSessionNonce,
    record: GeneratedSpanOccurrenceRecord,
    call: ModelCall,
    raw_output: Vec<u8>,
    token_ids: Vec<u32>,
}

impl fmt::Debug for AdmittedGeneratedSpan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdmittedGeneratedSpan")
            .field("occurrence_id", &self.record.id())
            .field("call_id", &self.call.id())
            .field("raw_output_bytes", &self.raw_output.len())
            .field("token_count", &self.token_ids.len())
            .finish_non_exhaustive()
    }
}

impl AdmittedGeneratedSpan {
    pub const fn occurrence_id(&self) -> loom_research_types::GeneratedSpanOccurrenceId {
        self.record.id()
    }

    pub(crate) fn belongs_to_session(&self, session_nonce: StoreSessionNonce) -> bool {
        self.session_nonce == session_nonce
    }
}

/// Non-serializable proof that all assembly parts were independently admitted
/// and the exact bytes, graph, and witness were replayed by the store.
pub struct AdmittedCandidateAssembly {
    session_nonce: StoreSessionNonce,
    admission_record_id: ResearchAdmissionRecordId,
    record: CandidateAssemblyRecord,
    exact_calls: Vec<OwnedExactCall>,
}

impl fmt::Debug for AdmittedCandidateAssembly {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdmittedCandidateAssembly")
            .field("admission_record_id", &self.admission_record_id)
            .field("assembly_id", &self.record.id())
            .field("part_count", &self.record.parts().len())
            .field("exact_call_count", &self.exact_calls.len())
            .field(
                "assembled_blob_id",
                &self.record.witness().assembled_blob_id(),
            )
            .field(
                "assembled_byte_len",
                &self.record.witness().assembled_byte_len(),
            )
            .finish_non_exhaustive()
    }
}

impl AdmittedCandidateAssembly {
    pub const fn admission_record_id(&self) -> ResearchAdmissionRecordId {
        self.admission_record_id
    }

    pub const fn assembly_id(&self) -> loom_research_types::CandidateAssemblyId {
        self.record.id()
    }
}

/// Non-serializable proof for a projection pinned to exact source bytes.
pub struct AdmittedCandidateProjection {
    session_nonce: StoreSessionNonce,
    admission_record_id: ResearchAdmissionRecordId,
    record: CandidateProjectionRecord,
}

impl fmt::Debug for AdmittedCandidateProjection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdmittedCandidateProjection")
            .field("admission_record_id", &self.admission_record_id)
            .field("projection_id", &self.record.id())
            .field("assembly_id", &self.record.assembly_id())
            .field("source_revision_id", &self.record.source_revision_id())
            .field("source_blob_id", &self.record.source_blob_id())
            .field(
                "resulting_blob_id",
                &self.record.witness().resulting_blob_id(),
            )
            .field(
                "resulting_byte_len",
                &self.record.witness().resulting_byte_len(),
            )
            .finish_non_exhaustive()
    }
}

impl AdmittedCandidateProjection {
    pub const fn admission_record_id(&self) -> ResearchAdmissionRecordId {
        self.admission_record_id
    }

    pub const fn projection_id(&self) -> loom_research_types::CandidateProjectionId {
        self.record.id()
    }

    pub(crate) fn belongs_to_session(&self, session_nonce: StoreSessionNonce) -> bool {
        self.session_nonce == session_nonce
    }

    pub(crate) fn evaluation_evidence(
        &self,
        expected_session: StoreSessionNonce,
    ) -> Option<(
        loom_research_types::CandidateProjectionId,
        BlobId,
        BlobId,
        u64,
    )> {
        (self.session_nonce == expected_session).then_some((
            self.record.id(),
            self.record.witness().resulting_blob_id(),
            self.record.witness().binding_fingerprint(),
            self.record.witness().resulting_byte_len(),
        ))
    }
}

/// Explicitly ineligible but inspectable mixed-authorship persistence result.
pub struct MixedAuthorshipAdmission {
    session_nonce: StoreSessionNonce,
    admission_record_id: ResearchAdmissionRecordId,
    record: MixedAuthorshipAssemblyRecord,
}

impl fmt::Debug for MixedAuthorshipAdmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MixedAuthorshipAdmission")
            .field("admission_record_id", &self.admission_record_id)
            .field("mixed_assembly_id", &self.record.id())
            .field("output_blob_id", &self.record.output_blob_id())
            .field("output_byte_len", &self.record.output_byte_len())
            .finish_non_exhaustive()
    }
}

/// Host-owned, non-serializable proof of one foreground user gesture.
///
/// There is intentionally no constructor in `loom-store`. A later host bridge
/// will move this lease behind a native event seal; deserialized
/// `UserPresenceEvidence` cannot manufacture it.
pub struct VerifiedUserPresence {
    session_nonce: StoreSessionNonce,
    command_id: CommandId,
    command_request_fingerprint: BlobId,
    kind: UserPresenceKind,
    session_fingerprint: BlobId,
    event_receipt_blob_id: BlobId,
    event_receipt_bytes: Vec<u8>,
    monotonic_event_index: u64,
    occurred_at_ms: i64,
    actor: loom_research_types::PromotionActor,
}

impl fmt::Debug for VerifiedUserPresence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedUserPresence")
            .field("command_id", &self.command_id)
            .field(
                "command_request_fingerprint",
                &self.command_request_fingerprint,
            )
            .field("session_fingerprint", &self.session_fingerprint)
            .field("event_receipt_blob_id", &self.event_receipt_blob_id)
            .field("event_receipt_byte_len", &self.event_receipt_bytes.len())
            .finish_non_exhaustive()
    }
}

/// Opaque proof that the exact promotion command request was durably recorded
/// in this process before user-presence authority.
pub struct RecordedPromotionRequest {
    session_nonce: StoreSessionNonce,
    command_id: CommandId,
    request_fingerprint: BlobId,
    recorded_at_ms: i64,
}

impl fmt::Debug for RecordedPromotionRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RecordedPromotionRequest")
            .field("command_id", &self.command_id)
            .field("request_fingerprint", &self.request_fingerprint)
            .finish_non_exhaustive()
    }
}

/// Exact runtime admission lease selected for promotion. Persisted admission
/// rows cannot construct either variant.
pub enum PromotionSubjectLease<'a> {
    CandidateProjection(&'a AdmittedCandidateProjection),
    MixedAuthorship(&'a MixedAuthorshipAdmission),
}

impl fmt::Debug for PromotionSubjectLease<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CandidateProjection(admitted) => formatter
                .debug_struct("CandidateProjectionLease")
                .field("admission_record_id", &admitted.admission_record_id)
                .field("projection_id", &admitted.record.id())
                .finish_non_exhaustive(),
            Self::MixedAuthorship(admitted) => formatter
                .debug_struct("MixedAuthorshipLease")
                .field("admission_record_id", &admitted.admission_record_id)
                .field("mixed_assembly_id", &admitted.record.id())
                .finish_non_exhaustive(),
        }
    }
}

/// Opaque pre-mutation authority. It is intentionally neither `Clone` nor
/// serializable and has no constructor or SQL reload path.
pub struct RecordedPromotionAuthority {
    session_nonce: StoreSessionNonce,
    command_id: CommandId,
    record_blob_id: BlobId,
    source_revision_id: loom_types::RevisionId,
    source_blob_id: BlobId,
    intended_result_blob_id: BlobId,
    intended_result_byte_len: u64,
}

impl fmt::Debug for RecordedPromotionAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RecordedPromotionAuthority")
            .field("command_id", &self.command_id)
            .field("record_blob_id", &self.record_blob_id)
            .field("source_revision_id", &self.source_revision_id)
            .field("source_blob_id", &self.source_blob_id)
            .field("intended_result_blob_id", &self.intended_result_blob_id)
            .field("intended_result_byte_len", &self.intended_result_byte_len)
            .finish_non_exhaustive()
    }
}

impl RecordedPromotionAuthority {
    /// Returns true only for the exact still-open store session that minted
    /// this capability. This does not recreate or consume authority.
    pub fn belongs_to(&self, store: &ProjectStore) -> bool {
        self.session_nonce == store.session_nonce
    }

    pub const fn command_id(&self) -> CommandId {
        self.command_id
    }

    pub const fn record_blob_id(&self) -> BlobId {
        self.record_blob_id
    }

    pub const fn source_revision_id(&self) -> loom_types::RevisionId {
        self.source_revision_id
    }

    pub const fn source_blob_id(&self) -> BlobId {
        self.source_blob_id
    }

    pub const fn intended_result_blob_id(&self) -> BlobId {
        self.intended_result_blob_id
    }

    pub const fn intended_result_byte_len(&self) -> u64 {
        self.intended_result_byte_len
    }
}

impl MixedAuthorshipAdmission {
    pub const fn admission_record_id(&self) -> ResearchAdmissionRecordId {
        self.admission_record_id
    }

    pub const fn record(&self) -> &MixedAuthorshipAssemblyRecord {
        &self.record
    }
}

struct OwnedExactCall {
    call: ModelCall,
    raw_output: Vec<u8>,
    token_ids: Vec<u32>,
}

impl fmt::Debug for OwnedExactCall {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OwnedExactCall")
            .field("call_id", &self.call.id())
            .field("raw_output_byte_len", &self.raw_output.len())
            .field("token_count", &self.token_ids.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BatchAdoptionKind {
    Admitted,
    DiagnosticOnly,
}

struct BatchAdoptionMaterial {
    kind: BatchAdoptionKind,
    project_id: ProjectId,
    binding: StoreBindingEvidence,
    prompt_evidence: StorePromptEvidence,
    backend_request_id: String,
    outcomes: Vec<CaseAdoptionMaterial>,
    verification_fingerprint: BlobId,
    prompt_freeze: Option<PromptFreezeEvidence>,
}

#[derive(Clone, Copy)]
struct PromptFreezeEvidence {
    frozen_at_ms: i64,
    fingerprint: BlobId,
}

/// Store-owned immutable copy of every compiled binding fact needed to replay
/// why this exact model was eligible for a base-writer call. This is evidence,
/// never inference authority.
struct StoreBindingEvidence {
    binding_id: String,
    manifest_source_bytes: Vec<u8>,
    manifest_source_hash: BlobId,
    manifest_canonical_bytes: Vec<u8>,
    manifest_artifact_hash: BlobId,
    binding_fingerprint: BlobId,
    declared_role: ModelRole,
    model_sha256: BlobId,
    model_byte_len: u64,
    tokenizer_sha256: BlobId,
    projector_sha256: Option<BlobId>,
    architecture: String,
    context_tokens: u32,
    capabilities: Vec<String>,
}

struct StoredBindingRow {
    evidence: StoreBindingEvidence,
}

struct RawStoredBindingRow {
    source_blob_id: String,
    source_byte_len: i64,
    source_hash: String,
    canonical_blob_id: String,
    canonical_byte_len: i64,
    artifact_hash: String,
    binding_id: String,
    declared_role: String,
    model_sha256: String,
    model_byte_len: i64,
    tokenizer_sha256: String,
    projector_sha256: Option<String>,
    architecture: String,
    context_tokens: i64,
    capabilities_blob_id: String,
    capabilities_byte_len: i64,
    capability_count: i64,
}

struct StoredBatchRow {
    project_id: String,
    binding_fingerprint: BlobId,
    binding_source_hash: BlobId,
    runtime_model_fingerprint: BlobId,
    prompt_specification_blob_id: BlobId,
    prompt_specification_byte_len: usize,
    source_prompt_fingerprint: BlobId,
    prompt_content_fingerprint: BlobId,
    treatment_recipe_fingerprint: BlobId,
    prompt_source_count: usize,
    prompt_freeze_fingerprint: BlobId,
    prompt_frozen_at_ms: i64,
    prompt_campaign_id: String,
    prompt_stage_id: String,
    prompt_stage_attempt_id: String,
    prompt_trial_case_id: String,
    tail_prompt_start_byte: u64,
    tail_prompt_end_byte: u64,
    source_tail_revision_id: Option<String>,
    source_tail_blob_id: BlobId,
    source_tail_start_byte: u64,
    source_tail_end_byte: u64,
    source_tail_origin: String,
    source_tail_assembly_id: Option<String>,
    exact_prompt_blob_id: BlobId,
    exact_prompt_byte_len: usize,
    prompt_form: String,
    prompt_token_policy: String,
    prompt_token_ids_blob_id: BlobId,
    prompt_token_count: usize,
    prompt_token_ids_fingerprint: BlobId,
    compiled_prompt_fingerprint: BlobId,
    expected_case_count: usize,
    completed_call_count: usize,
    cancelled_call_count: usize,
    native_request_id: String,
}

struct RawStoredBatchRow {
    project_id: String,
    binding_fingerprint: String,
    binding_source_hash: String,
    runtime_model_fingerprint: String,
    prompt_specification_blob_id: String,
    prompt_specification_byte_len: i64,
    source_prompt_fingerprint: String,
    prompt_content_fingerprint: String,
    treatment_recipe_fingerprint: String,
    prompt_source_count: i64,
    prompt_freeze_fingerprint: String,
    prompt_frozen_at_ms: i64,
    prompt_campaign_id: String,
    prompt_stage_id: String,
    prompt_stage_attempt_id: String,
    prompt_trial_case_id: String,
    tail_prompt_start_byte: i64,
    tail_prompt_end_byte: i64,
    source_tail_revision_id: Option<String>,
    source_tail_blob_id: String,
    source_tail_start_byte: i64,
    source_tail_end_byte: i64,
    source_tail_origin: String,
    source_tail_assembly_id: Option<String>,
    exact_prompt_blob_id: String,
    exact_prompt_byte_len: i64,
    prompt_form: String,
    prompt_token_policy: String,
    prompt_token_ids_blob_id: String,
    prompt_token_count: i64,
    prompt_token_ids_fingerprint: String,
    compiled_prompt_fingerprint: String,
    expected_case_count: i64,
    completed_call_count: i64,
    cancelled_call_count: i64,
    native_request_id: String,
}

struct StoredBatchCaseRow {
    position: usize,
    call_id: String,
    campaign_id: String,
    stage_id: String,
    stage_attempt_id: String,
    trial_case_id: String,
    seed_decimal: String,
    outcome: String,
    verification_fingerprint: BlobId,
}

struct StoredCallRow {
    campaign_id: String,
    stage_id: String,
    stage_attempt_id: String,
    trial_case_id: String,
    seed_decimal: String,
    model_fingerprint: BlobId,
    tokenizer_fingerprint: BlobId,
    prompt_fingerprint: BlobId,
    sampler_fingerprint: BlobId,
    control_program_fingerprint: BlobId,
    evidence_class: String,
    verification_fingerprint: Option<BlobId>,
    call_record_blob_id: BlobId,
}

struct RawCompletedCaseRow {
    raw_output_blob_id: String,
    raw_output_byte_len: i64,
    token_ids_blob_id: String,
    token_count: i64,
    token_ids_fingerprint: String,
    event_blob_id: String,
    receipt_blob_id: String,
    evidence_raw_output_byte_len: i64,
    has_projection: i64,
    displayed_blob_id: Option<String>,
    displayed_byte_len: Option<i64>,
    displayed_start: Option<i64>,
    displayed_end: Option<i64>,
    endpoint_start: Option<i64>,
    endpoint_end: Option<i64>,
    stop_start: Option<i64>,
    stop_end: Option<i64>,
    terminal_sampled_token_id: Option<i64>,
    evidence_verification_fingerprint: String,
}

impl From<BaseWriterBinding> for StoreBindingEvidence {
    fn from(binding: BaseWriterBinding) -> Self {
        Self {
            binding_id: binding.binding_id().to_owned(),
            manifest_source_bytes: binding.manifest_source_bytes().to_vec(),
            manifest_source_hash: binding.manifest_source_hash().as_blob_id(),
            manifest_canonical_bytes: binding.manifest_canonical_bytes().to_vec(),
            manifest_artifact_hash: binding.manifest_fingerprint().as_blob_id(),
            binding_fingerprint: binding.fingerprint(),
            declared_role: binding.declared_role(),
            model_sha256: binding.model_sha256(),
            model_byte_len: binding.model_bytes(),
            tokenizer_sha256: binding.tokenizer_sha256(),
            projector_sha256: binding.multimodal_projector_sha256(),
            architecture: binding.architecture().to_owned(),
            context_tokens: binding.context_tokens(),
            capabilities: binding
                .capabilities()
                .iter()
                .map(ToString::to_string)
                .collect(),
        }
    }
}

/// Store-owned, private copy of the exact prompt facts carried by the
/// move-only inference authority. Keeping the extracted fields private lets
/// store tests attack persistence without adding a production constructor to
/// `loom-inference` or a caller-facing raw-admission bypass here.
struct StorePromptEvidence {
    specification_bytes: Vec<u8>,
    specification_blob_id: BlobId,
    project_id: ProjectId,
    scope: CallScope,
    treatment_recipe_fingerprint: BlobId,
    source_prompt_fingerprint: BlobId,
    content_fingerprint: BlobId,
    tail_prompt_range: NonEmptyByteRange,
    source_tail_revision_id: Option<String>,
    source_tail_blob_id: BlobId,
    source_tail_range: NonEmptyByteRange,
    source_tail_origin: StoreTailOrigin,
    sources: Vec<StorePromptSourceEvidence>,
    raw_utf8: Vec<u8>,
    raw_blob_id: BlobId,
    form: PromptFormEvidence,
    token_policy: PromptTokenPolicyEvidence,
    ordered_token_ids: Vec<u32>,
    token_fingerprint: BlobId,
    compiled_fingerprint: BlobId,
}

#[derive(Clone, Eq, PartialEq)]
enum StoreTailOrigin {
    LiveManuscript,
    AdmittedAssembly(String),
}

#[derive(Clone, Eq, PartialEq)]
struct StorePromptSourceEvidence {
    block_index: i64,
    source_index: usize,
    kind: StorePromptSourceKind,
    revision_id: Option<String>,
    blob_id: BlobId,
    range: NonEmptyByteRange,
    assembly_id: Option<String>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum StorePromptSourceKind {
    PrecedingExact,
    TailLive,
    TailAdmittedAssembly,
}

#[derive(Eq, PartialEq)]
struct FrozenPromptSourceSnapshot {
    specification_bytes: Vec<u8>,
    specification_blob_id: BlobId,
    project_id: ProjectId,
    scope: CallScope,
    treatment_recipe_fingerprint: BlobId,
    source_prompt_fingerprint: BlobId,
    content_fingerprint: BlobId,
    tail_prompt_range: NonEmptyByteRange,
    source_tail_revision_id: Option<String>,
    source_tail_blob_id: BlobId,
    source_tail_range: NonEmptyByteRange,
    source_tail_origin: StoreTailOrigin,
    sources: Vec<StorePromptSourceEvidence>,
    raw_utf8: Vec<u8>,
    raw_blob_id: BlobId,
}

impl FrozenPromptSourceSnapshot {
    fn from_compiled(prompt: &CompiledBaseCompletionPrompt) -> Result<Self> {
        let specification = prompt.specification();
        let specification_bytes = serde_json::to_vec(specification)?;
        validate_prompt_specification_size(
            specification_bytes.len(),
            MAX_FROZEN_PROMPT_SPECIFICATION_BYTES,
        )?;
        specification
            .verify_compiled_evidence(
                prompt.exact_bytes(),
                prompt.tail_prompt_range(),
                prompt.fingerprint(),
            )
            .map_err(|error| admission_error(format!("compiled prompt replay failed: {error}")))?;
        let tail = specification.tail();
        let source_tail_origin = match tail.origin() {
            CompletionTailOrigin::LiveManuscript => StoreTailOrigin::LiveManuscript,
            CompletionTailOrigin::AdmittedAssembly { assembly_id } => {
                StoreTailOrigin::AdmittedAssembly(assembly_id.to_string())
            }
        };
        Ok(Self {
            specification_blob_id: BlobId::digest(&specification_bytes),
            specification_bytes,
            project_id: prompt.project_id(),
            scope: prompt.scope(),
            treatment_recipe_fingerprint: prompt.treatment_recipe_fingerprint(),
            source_prompt_fingerprint: prompt.fingerprint(),
            content_fingerprint: prompt.content_fingerprint(),
            tail_prompt_range: prompt.tail_prompt_range(),
            source_tail_revision_id: tail.source_revision_id().map(|id| id.to_string()),
            source_tail_blob_id: tail.source_blob_id(),
            source_tail_range: tail.range(),
            source_tail_origin,
            sources: exact_prompt_sources(specification)?,
            raw_utf8: prompt.exact_bytes().to_vec(),
            raw_blob_id: BlobId::digest(prompt.exact_bytes()),
        })
    }

    fn matches_store_prompt(&self, prompt: &StorePromptEvidence) -> bool {
        self.specification_bytes == prompt.specification_bytes
            && self.specification_blob_id == prompt.specification_blob_id
            && self.project_id == prompt.project_id
            && self.scope == prompt.scope
            && self.treatment_recipe_fingerprint == prompt.treatment_recipe_fingerprint
            && self.source_prompt_fingerprint == prompt.source_prompt_fingerprint
            && self.content_fingerprint == prompt.content_fingerprint
            && self.tail_prompt_range == prompt.tail_prompt_range
            && self.source_tail_revision_id == prompt.source_tail_revision_id
            && self.source_tail_blob_id == prompt.source_tail_blob_id
            && self.source_tail_range == prompt.source_tail_range
            && self.source_tail_origin == prompt.source_tail_origin
            && self.sources == prompt.sources
            && self.raw_utf8 == prompt.raw_utf8
            && self.raw_blob_id == prompt.raw_blob_id
    }
}

fn exact_prompt_sources(
    specification: &FrozenBaseCompletionPrompt,
) -> Result<Vec<StorePromptSourceEvidence>> {
    let mut sources = Vec::with_capacity(specification.preceding_blocks().len() + 1);
    for (block_index, block) in specification.preceding_blocks().iter().enumerate() {
        if block.witness().is_transformation() {
            return Err(admission_error(
                "transformed prompt blocks require a persisted verified controller receipt",
            ));
        }
        let [source] = block.witness().sources() else {
            return Err(admission_error(
                "exact prompt block does not have exactly one source range",
            ));
        };
        sources.push(StorePromptSourceEvidence {
            block_index: i64::try_from(block_index)
                .map_err(|_| admission_error("prompt block index exceeds SQLite integer range"))?,
            source_index: 0,
            kind: StorePromptSourceKind::PrecedingExact,
            revision_id: Some(source.revision_id().to_string()),
            blob_id: source.source_blob_id(),
            range: source.range(),
            assembly_id: None,
        });
    }
    let tail = specification.tail();
    let (kind, revision_id, assembly_id) = match tail {
        loom_research_types::CompletionPromptTail::LiveManuscript {
            source_revision_id, ..
        } => (
            StorePromptSourceKind::TailLive,
            Some(source_revision_id.to_string()),
            None,
        ),
        loom_research_types::CompletionPromptTail::AdmittedAssembly { assembly_id, .. } => (
            StorePromptSourceKind::TailAdmittedAssembly,
            None,
            Some(assembly_id.to_string()),
        ),
    };
    sources.push(StorePromptSourceEvidence {
        block_index: -1,
        source_index: 0,
        kind,
        revision_id,
        blob_id: tail.source_blob_id(),
        range: tail.range(),
        assembly_id,
    });
    Ok(sources)
}

impl StorePromptEvidence {
    fn try_from_exact(evidence: &ExactPromptEvidence) -> Result<Self> {
        let specification = evidence.frozen_specification();
        let specification_bytes = serde_json::to_vec(specification)?;
        validate_prompt_specification_size(
            specification_bytes.len(),
            MAX_FROZEN_PROMPT_SPECIFICATION_BYTES,
        )?;
        let tail = specification.tail();
        let sources = exact_prompt_sources(specification)?;
        let source_tail_origin = match tail.origin() {
            CompletionTailOrigin::LiveManuscript => StoreTailOrigin::LiveManuscript,
            CompletionTailOrigin::AdmittedAssembly { assembly_id } => {
                StoreTailOrigin::AdmittedAssembly(assembly_id.to_string())
            }
        };
        Ok(Self {
            specification_blob_id: BlobId::digest(&specification_bytes),
            specification_bytes,
            project_id: evidence.project_id(),
            scope: evidence.scope(),
            treatment_recipe_fingerprint: evidence.treatment_recipe_fingerprint(),
            source_prompt_fingerprint: evidence.source_prompt_fingerprint(),
            content_fingerprint: evidence.content_fingerprint(),
            tail_prompt_range: evidence.tail_prompt_range(),
            source_tail_revision_id: tail.source_revision_id().map(|id| id.to_string()),
            source_tail_blob_id: tail.source_blob_id(),
            source_tail_range: tail.range(),
            source_tail_origin,
            sources,
            raw_utf8: evidence.raw_utf8().to_vec(),
            raw_blob_id: evidence.raw_blob_id(),
            form: evidence.form(),
            token_policy: evidence.token_policy(),
            ordered_token_ids: evidence.ordered_token_ids().to_vec(),
            token_fingerprint: evidence.token_fingerprint(),
            compiled_fingerprint: evidence.compiled_fingerprint(),
        })
    }
}

fn validate_prompt_specification_size(actual: usize, maximum: usize) -> Result<()> {
    if actual == 0 || actual > maximum {
        return Err(admission_error(
            "frozen prompt specification is empty or exceeds its serialized bound",
        ));
    }
    Ok(())
}

fn validate_prompt_source_graph(
    store: &ProjectStore,
    prompt: &StorePromptEvidence,
    require_live_tail_current: bool,
) -> Result<()> {
    validate_prompt_source_graph_parts(
        store,
        &prompt.specification_bytes,
        &prompt.sources,
        require_live_tail_current,
    )
}

fn validate_prompt_source_graph_parts(
    store: &ProjectStore,
    specification_bytes: &[u8],
    sources: &[StorePromptSourceEvidence],
    require_live_tail_current: bool,
) -> Result<()> {
    let specification: FrozenBaseCompletionPrompt = serde_json::from_slice(specification_bytes)?;
    let expected = exact_prompt_sources(&specification)?;
    if expected.len() != sources.len() {
        return Err(admission_error(
            "frozen prompt source count differs from indexed source evidence",
        ));
    }
    for (position, (expected, stored)) in expected.iter().zip(sources).enumerate() {
        if expected.block_index != stored.block_index
            || expected.source_index != stored.source_index
            || expected.kind != stored.kind
            || expected.revision_id != stored.revision_id
            || expected.blob_id != stored.blob_id
            || expected.range != stored.range
            || expected.assembly_id != stored.assembly_id
        {
            return Err(admission_error(format!(
                "frozen prompt source {position} differs from its indexed evidence"
            )));
        }
        if !prompt_source_is_relationally_bound(store, stored, require_live_tail_current)? {
            return Err(admission_error(format!(
                "frozen prompt source {position} is not immutable project evidence"
            )));
        }
        validate_prompt_source_bytes(store, &specification, stored)?;
    }
    Ok(())
}

fn validate_frozen_prompt_source_snapshot(
    store: &ProjectStore,
    snapshot: &FrozenPromptSourceSnapshot,
    require_live_tail_current: bool,
) -> Result<()> {
    if snapshot.project_id != store.manifest.project_id
        || snapshot.specification_bytes.is_empty()
        || BlobId::digest(&snapshot.specification_bytes) != snapshot.specification_blob_id
        || snapshot.raw_utf8.is_empty()
        || snapshot.raw_utf8.len() > MAX_BASE_PROMPT_BYTES
        || BlobId::digest(&snapshot.raw_utf8) != snapshot.raw_blob_id
    {
        return Err(admission_error(
            "frozen prompt source snapshot is malformed or belongs to another project",
        ));
    }
    validate_prompt_source_graph_parts(
        store,
        &snapshot.specification_bytes,
        &snapshot.sources,
        require_live_tail_current,
    )?;
    let source_bytes = store.read_blob(snapshot.source_tail_blob_id)?;
    validate_exact_prompt_tail(
        &snapshot.raw_utf8,
        snapshot.tail_prompt_range,
        &source_bytes,
        snapshot.source_tail_range,
    )
}

fn prompt_source_is_relationally_bound(
    store: &ProjectStore,
    source: &StorePromptSourceEvidence,
    require_live_tail_current: bool,
) -> Result<bool> {
    let exact: i64 = match source.kind {
        StorePromptSourceKind::TailLive if require_live_tail_current => {
            store.connection.query_row(
                "SELECT EXISTS(
                SELECT 1 FROM revisions revision
                JOIN artifacts artifact USING (artifact_id)
                WHERE revision.revision_id = ?1
                  AND artifact.blob_id = ?2
                  AND NOT EXISTS (
                      SELECT 1 FROM revisions later
                      WHERE later.document_id = revision.document_id
                        AND (later.created_at_ms > revision.created_at_ms
                             OR (later.created_at_ms = revision.created_at_ms
                                 AND later.revision_id > revision.revision_id))
                  )
             )",
                params![source.revision_id.as_deref(), source.blob_id.to_string()],
                |row| row.get(0),
            )?
        }
        StorePromptSourceKind::PrecedingExact | StorePromptSourceKind::TailLive => {
            stored_revision_blob_exists(store, source)?
        }
        StorePromptSourceKind::TailAdmittedAssembly => store.connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM research_candidate_assemblies assembly
                JOIN research_admission_records admission
                  ON admission.subject_kind = 'candidate_assembly'
                 AND admission.subject_id = assembly.assembly_id
                WHERE assembly.assembly_id = ?1 AND assembly.assembled_blob_id = ?2
             )",
            params![source.assembly_id.as_deref(), source.blob_id.to_string()],
            |row| row.get(0),
        )?,
    };
    Ok(exact == 1)
}

fn stored_revision_blob_exists(
    store: &ProjectStore,
    source: &StorePromptSourceEvidence,
) -> Result<i64> {
    Ok(store.connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM revisions revision
            JOIN artifacts artifact USING (artifact_id)
            WHERE revision.revision_id = ?1 AND artifact.blob_id = ?2
         )",
        params![source.revision_id.as_deref(), source.blob_id.to_string()],
        |row| row.get(0),
    )?)
}

fn revision_blob_is_current(
    store: &ProjectStore,
    revision_id: &str,
    blob_id: BlobId,
) -> Result<bool> {
    let current: i64 = store.connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM revisions revision
            JOIN artifacts artifact USING (artifact_id)
            WHERE revision.revision_id = ?1
              AND artifact.blob_id = ?2
              AND NOT EXISTS (
                  SELECT 1 FROM revisions later
                  WHERE later.document_id = revision.document_id
                    AND (later.created_at_ms > revision.created_at_ms
                         OR (later.created_at_ms = revision.created_at_ms
                             AND later.revision_id > revision.revision_id))
              )
         )",
        params![revision_id, blob_id.to_string()],
        |row| row.get(0),
    )?;
    Ok(current == 1)
}

fn validate_prompt_source_bytes(
    store: &ProjectStore,
    specification: &FrozenBaseCompletionPrompt,
    source: &StorePromptSourceEvidence,
) -> Result<()> {
    let source_bytes = store.read_blob(source.blob_id)?;
    if matches!(
        source.kind,
        StorePromptSourceKind::TailLive | StorePromptSourceKind::TailAdmittedAssembly
    ) && source.range.end() != source_bytes.len() as u64
    {
        return Err(admission_error("frozen prompt tail is not at source EOF"));
    }
    let excerpt = source
        .range
        .as_range()
        .checked_slice(&source_bytes)
        .map_err(|error| admission_error(format!("invalid prompt source range: {error}")))?;
    if source.kind != StorePromptSourceKind::PrecedingExact {
        return Ok(());
    }
    let block_index = usize::try_from(source.block_index)
        .map_err(|_| admission_error("negative preceding prompt block index"))?;
    let block = specification
        .preceding_blocks()
        .get(block_index)
        .ok_or_else(|| admission_error("prompt source block index is out of bounds"))?;
    if block.bytes().as_bytes() != excerpt {
        return Err(admission_error(
            "exact preceding prompt block differs from its immutable source range",
        ));
    }
    Ok(())
}

enum CaseAdoptionMaterial {
    Completed(CompletedAdoptionMaterial),
    Cancelled(CancelledAdoptionMaterial),
}

struct CompletedAdoptionMaterial {
    input_index: usize,
    call: ModelCall,
    raw_output: Vec<u8>,
    generated_token_ids: Vec<u32>,
    event_json: Vec<u8>,
    backend_audit_json: Vec<u8>,
    displayed_output: Vec<u8>,
    output_projection: Option<OutputProjection>,
    terminal_sampled_token_id: Option<i32>,
    verification_fingerprint: BlobId,
}

struct CancelledAdoptionMaterial {
    input_index: usize,
    call: ModelCall,
    partial_raw_output: Vec<u8>,
    generated_token_ids: Vec<u32>,
    event_json: Vec<u8>,
    backend_audit_json: Vec<u8>,
    verification_fingerprint: BlobId,
}

struct StoredCaseBlobs {
    call_record_blob_id: BlobId,
    call_record_byte_len: usize,
    raw_output_blob_id: BlobId,
    raw_output_byte_len: usize,
    token_ids_blob_id: BlobId,
    token_ids_byte_len: usize,
    event_blob_id: BlobId,
    event_byte_len: usize,
    receipt_blob_id: BlobId,
    receipt_byte_len: usize,
    displayed_output: Option<(BlobId, usize)>,
}

struct StoredBindingBlobs {
    source: BlobId,
    canonical: BlobId,
    capabilities: BlobId,
    capabilities_byte_len: usize,
}

struct StagedBatchBlobs {
    binding: StoredBindingBlobs,
    binding_capabilities_bytes: Vec<u8>,
    prompt_specification: BlobId,
    prompt: BlobId,
    prompt_tokens: BlobId,
    prompt_token_bytes: Vec<u8>,
    source_tail_bytes: Vec<u8>,
}

struct StagedBatchAdoption {
    blobs: StagedBatchBlobs,
    cases: Vec<StoredCaseAdoption>,
    completed_count: usize,
    cancelled_count: usize,
}

enum StoredCaseAdoption {
    Completed {
        material: CompletedAdoptionMaterial,
        blobs: StoredCaseBlobs,
    },
    Cancelled {
        material: CancelledAdoptionMaterial,
        blobs: StoredCaseBlobs,
    },
}

impl BatchAdoptionMaterial {
    fn from_outcome(outcome: VerifiedInferenceOutcome) -> Result<Self> {
        match outcome.into_parts() {
            VerifiedInferenceOutcomeParts::Admitted(parts) => Self::from_admitted_parts(parts),
            VerifiedInferenceOutcomeParts::DiagnosticOnly(parts) => {
                Self::from_diagnostic_parts(parts)
            }
        }
    }

    fn from_admitted_parts(parts: VerifiedInferenceParts) -> Result<Self> {
        parts.consume(
            |project_id,
             binding,
             prompt_evidence,
             backend_request_id,
             outcomes,
             verification_fingerprint| {
                Self::from_parts(
                    BatchAdoptionKind::Admitted,
                    project_id,
                    binding,
                    &prompt_evidence,
                    backend_request_id,
                    outcomes,
                    verification_fingerprint,
                )
            },
        )
    }

    fn from_diagnostic_parts(parts: VerifiedDiagnosticParts) -> Result<Self> {
        parts.consume(
            |project_id,
             binding,
             prompt_evidence,
             backend_request_id,
             outcomes,
             verification_fingerprint| {
                Self::from_parts(
                    BatchAdoptionKind::DiagnosticOnly,
                    project_id,
                    binding,
                    &prompt_evidence,
                    backend_request_id,
                    outcomes,
                    verification_fingerprint,
                )
            },
        )
    }

    fn from_parts(
        kind: BatchAdoptionKind,
        project_id: ProjectId,
        binding: BaseWriterBinding,
        prompt_evidence: &ExactPromptEvidence,
        backend_request_id: String,
        outcome_parts: Vec<VerifiedCaseOutcomeParts>,
        verification_fingerprint: BlobId,
    ) -> Result<Self> {
        if outcome_parts.is_empty()
            || outcome_parts
                .iter()
                .enumerate()
                .any(|(expected, outcome)| outcome.input_index() != expected)
        {
            return Err(admission_error(
                "verified inference outcomes are empty or not in contiguous request order",
            ));
        }
        let outcomes = outcome_parts
            .into_iter()
            .map(case_adoption_material)
            .collect::<Vec<_>>();
        let completed_count = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, CaseAdoptionMaterial::Completed(_)))
            .count();
        if (kind == BatchAdoptionKind::Admitted && completed_count == 0)
            || (kind == BatchAdoptionKind::DiagnosticOnly && completed_count != 0)
        {
            return Err(admission_error(
                "verified inference outcome class disagrees with its completed call count",
            ));
        }
        Ok(Self {
            kind,
            project_id,
            binding: binding.into(),
            prompt_evidence: StorePromptEvidence::try_from_exact(prompt_evidence)?,
            backend_request_id,
            outcomes,
            verification_fingerprint,
            prompt_freeze: None,
        })
    }
}

fn case_adoption_material(parts: VerifiedCaseOutcomeParts) -> CaseAdoptionMaterial {
    parts.consume(
        |input_index, completed| {
            CaseAdoptionMaterial::Completed(completed_adoption_material(input_index, completed))
        },
        |input_index, cancelled| {
            CaseAdoptionMaterial::Cancelled(cancelled_adoption_material(input_index, cancelled))
        },
    )
}

fn completed_adoption_material(
    input_index: usize,
    parts: VerifiedBaseWriterCallParts,
) -> CompletedAdoptionMaterial {
    parts.consume(
        |call,
         raw_output,
         generated_token_ids,
         event_json,
         backend_audit_json,
         displayed_output,
         output_projection,
         terminal_sampled_token_id,
         verification_fingerprint| CompletedAdoptionMaterial {
            input_index,
            call,
            raw_output,
            generated_token_ids,
            event_json,
            backend_audit_json,
            displayed_output,
            output_projection,
            terminal_sampled_token_id,
            verification_fingerprint,
        },
    )
}

fn cancelled_adoption_material(
    input_index: usize,
    parts: CancelledBaseWriterDiagnosticParts,
) -> CancelledAdoptionMaterial {
    parts.consume(
        |call,
         partial_raw_output,
         generated_token_ids,
         event_json,
         backend_audit_json,
         verification_fingerprint| CancelledAdoptionMaterial {
            input_index,
            call,
            partial_raw_output,
            generated_token_ids,
            event_json,
            backend_audit_json,
            verification_fingerprint,
        },
    )
}

fn validate_prompt_adoption(prompt: &StorePromptEvidence) -> Result<()> {
    if prompt.specification_bytes.is_empty()
        || BlobId::digest(&prompt.specification_bytes) != prompt.specification_blob_id
    {
        return Err(admission_error(
            "frozen prompt specification bytes differ from their blob ID",
        ));
    }
    let specification: FrozenBaseCompletionPrompt =
        serde_json::from_slice(&prompt.specification_bytes)?;
    if serde_json::to_vec(&specification)? != prompt.specification_bytes {
        return Err(admission_error(
            "frozen prompt specification is not canonically encoded",
        ));
    }
    let tail = specification.tail();
    let origin_matches = matches!(
        (tail.origin(), &prompt.source_tail_origin),
        (
            CompletionTailOrigin::LiveManuscript,
            StoreTailOrigin::LiveManuscript
        )
    ) || matches!(
        (tail.origin(), &prompt.source_tail_origin),
        (
            CompletionTailOrigin::AdmittedAssembly { assembly_id },
            StoreTailOrigin::AdmittedAssembly(stored)
        ) if assembly_id.to_string() == stored.as_str()
    );
    if specification.project_id() != prompt.project_id
        || specification.scope() != prompt.scope
        || specification.treatment_recipe_fingerprint() != prompt.treatment_recipe_fingerprint
        || tail.source_revision_id().map(|id| id.to_string()) != prompt.source_tail_revision_id
        || tail.source_blob_id() != prompt.source_tail_blob_id
        || tail.range() != prompt.source_tail_range
        || !origin_matches
    {
        return Err(admission_error(
            "frozen prompt specification fields differ from their indexed evidence",
        ));
    }
    if prompt.raw_utf8.is_empty()
        || prompt.raw_utf8.len() > MAX_BASE_PROMPT_BYTES
        || std::str::from_utf8(&prompt.raw_utf8).is_err()
    {
        return Err(admission_error(
            "verified prompt must be non-empty bounded UTF-8",
        ));
    }
    if BlobId::digest(&prompt.raw_utf8) != prompt.raw_blob_id {
        return Err(admission_error(
            "verified prompt bytes differ from their raw blob ID",
        ));
    }
    if prompt.ordered_token_ids.is_empty()
        || prompt.ordered_token_ids.len() > MAX_GENERATED_TOKENS as usize
        || prompt
            .ordered_token_ids
            .iter()
            .any(|token| *token > i32::MAX as u32)
    {
        return Err(admission_error(
            "verified prompt token evidence is empty, oversized, or non-native",
        ));
    }
    if prompt_token_fingerprint(&prompt.ordered_token_ids) != prompt.token_fingerprint {
        return Err(admission_error(
            "verified prompt token fingerprint does not match its ordered tokens",
        ));
    }
    if compiled_prompt_fingerprint(
        prompt.source_prompt_fingerprint,
        prompt.raw_blob_id,
        prompt.form,
        prompt.token_policy,
        &prompt.ordered_token_ids,
    ) != prompt.compiled_fingerprint
    {
        return Err(admission_error(
            "compiled prompt fingerprint does not match the exact prompt evidence",
        ));
    }
    specification
        .verify_compiled_evidence(
            &prompt.raw_utf8,
            prompt.tail_prompt_range,
            prompt.source_prompt_fingerprint,
        )
        .map_err(|error| admission_error(format!("frozen prompt replay failed: {error}")))?;
    specification
        .verify_compiled_content_evidence(
            &prompt.raw_utf8,
            prompt.tail_prompt_range,
            prompt.content_fingerprint,
        )
        .map_err(|error| {
            admission_error(format!("frozen prompt content replay failed: {error}"))
        })?;
    Ok(())
}

fn validate_binding_evidence(binding: &StoreBindingEvidence) -> Result<()> {
    if binding.declared_role != ModelRole::BaseWriter {
        return Err(admission_error(
            "compiled model binding is not declared as a base writer",
        ));
    }
    if BlobId::digest(&binding.manifest_source_bytes) != binding.manifest_source_hash
        || BlobId::digest(&binding.manifest_canonical_bytes) != binding.manifest_artifact_hash
    {
        return Err(admission_error(
            "compiled model binding manifest bytes differ from their hashes",
        ));
    }
    let compiled = compile_manifest(&binding.manifest_source_bytes).map_err(|error| {
        admission_error(format!("model binding manifest cannot replay: {error}"))
    })?;
    if compiled.source_hash().as_blob_id() != binding.manifest_source_hash
        || compiled.canonical_bytes() != binding.manifest_canonical_bytes
        || compiled.artifact_hash().as_blob_id() != binding.manifest_artifact_hash
    {
        return Err(admission_error(
            "recompiled model binding manifest differs from persisted evidence",
        ));
    }
    let replayed = BaseWriterBinding::compile(&compiled, &binding.binding_id)
        .map_err(|error| admission_error(format!("base-writer binding cannot replay: {error}")))?;
    let replayed_capabilities = replayed
        .capabilities()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if replayed.fingerprint() != binding.binding_fingerprint
        || replayed.declared_role() != binding.declared_role
        || replayed.model_sha256() != binding.model_sha256
        || replayed.model_bytes() != binding.model_byte_len
        || replayed.tokenizer_sha256() != binding.tokenizer_sha256
        || replayed.multimodal_projector_sha256() != binding.projector_sha256
        || replayed.architecture() != binding.architecture
        || replayed.context_tokens() != binding.context_tokens
        || replayed_capabilities != binding.capabilities
    {
        return Err(admission_error(
            "selected model binding differs from its recompiled manifest",
        ));
    }
    Ok(())
}

fn validate_completed_adoption(
    material: &CompletedAdoptionMaterial,
    compiled_prompt_fingerprint: BlobId,
) -> Result<()> {
    if material.call.evidence_class() != CallEvidenceClass::LiveBaseWriterClaim
        || material.call.identity().prompt_fingerprint() != compiled_prompt_fingerprint
    {
        return Err(admission_error(
            "completed call is not bound to the batch's live base-writer prompt",
        ));
    }
    if material.raw_output.len() as u64 > MAX_RAW_OUTPUT_BYTES
        || std::str::from_utf8(&material.raw_output).is_err()
    {
        return Err(admission_error(
            "completed raw writer output is oversized or not UTF-8",
        ));
    }
    validate_native_json_evidence(&material.event_json, "event stream")?;
    validate_native_json_evidence(&material.backend_audit_json, "backend receipt")?;

    let CallTerminal::Completed(completed) = material.call.terminal() else {
        return Err(admission_error(
            "completed adoption material does not contain a completed terminal",
        ));
    };
    if completed.raw_output_blob_id() != BlobId::digest(&material.raw_output)
        || completed.raw_output_byte_len() != material.raw_output.len() as u64
    {
        return Err(admission_error(
            "completed terminal does not bind the exact raw output",
        ));
    }
    completed
        .token_evidence()
        .verify(&material.generated_token_ids)?;
    if material
        .generated_token_ids
        .iter()
        .any(|token| *token > i32::MAX as u32)
    {
        return Err(admission_error(
            "completed call contains a non-native token ID",
        ));
    }
    if completed.raw_event_stream_blob_id() != BlobId::digest(&material.event_json)
        || completed.backend_receipt_blob_id() != Some(BlobId::digest(&material.backend_audit_json))
    {
        return Err(admission_error(
            "completed terminal does not bind its exact event stream and backend receipt",
        ));
    }
    match &material.output_projection {
        Some(projection) => {
            projection.verify_raw_bytes(&material.raw_output)?;
            if projection.displayed_str(&material.raw_output)?.as_bytes()
                != material.displayed_output
            {
                return Err(admission_error(
                    "displayed output differs from its exact output projection",
                ));
            }
        }
        None if !material.displayed_output.is_empty() => {
            return Err(admission_error(
                "displayed output is present without an exact output projection",
            ));
        }
        None => {}
    }
    if material
        .terminal_sampled_token_id
        .is_some_and(|token| token < 0)
    {
        return Err(admission_error(
            "completed call contains a negative terminal sampled token ID",
        ));
    }
    Ok(())
}

fn validate_cancelled_adoption(
    material: &CancelledAdoptionMaterial,
    compiled_prompt_fingerprint: BlobId,
) -> Result<()> {
    if material.call.evidence_class() != CallEvidenceClass::LiveBaseWriterClaim
        || material.call.identity().prompt_fingerprint() != compiled_prompt_fingerprint
        || !matches!(material.call.terminal(), CallTerminal::Cancelled { .. })
    {
        return Err(admission_error(
            "cancelled diagnostic is not bound to the batch's live base-writer prompt",
        ));
    }
    if material.partial_raw_output.len() as u64 > MAX_RAW_OUTPUT_BYTES
        || std::str::from_utf8(&material.partial_raw_output).is_err()
    {
        return Err(admission_error(
            "cancelled partial output is oversized or not UTF-8",
        ));
    }
    TokenEvidence::from_exact(&material.generated_token_ids)?;
    if material
        .generated_token_ids
        .iter()
        .any(|token| *token > i32::MAX as u32)
    {
        return Err(admission_error(
            "cancelled call contains a non-native token ID",
        ));
    }
    validate_native_json_evidence(&material.event_json, "cancelled event stream")?;
    validate_native_json_evidence(&material.backend_audit_json, "cancelled backend receipt")?;
    Ok(())
}

fn validate_native_json_evidence(bytes: &[u8], field: &'static str) -> Result<()> {
    validate_native_json_evidence_with_limit(bytes, field, MAX_BACKEND_EVIDENCE_BYTES)
}

fn validate_native_json_evidence_with_limit(
    bytes: &[u8],
    field: &'static str,
    maximum_bytes: usize,
) -> Result<()> {
    if bytes.is_empty() || bytes.len() > maximum_bytes {
        return Err(admission_error(format!(
            "{field} is empty or exceeds the bounded evidence size"
        )));
    }
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    if !value.is_object() {
        return Err(admission_error(format!(
            "{field} is not a JSON object envelope"
        )));
    }
    Ok(())
}

fn insert_model_binding(
    transaction: &Transaction<'_>,
    binding: &StoreBindingEvidence,
    blobs: &StoredBindingBlobs,
    created_at_ms: i64,
) -> Result<()> {
    let source_byte_len = checked_sql_usize(
        binding.manifest_source_bytes.len(),
        "model binding manifest source length",
    )?;
    insert_model_binding_identity(transaction, binding, blobs, created_at_ms)?;
    insert_model_binding_source(
        transaction,
        binding,
        blobs.source,
        source_byte_len,
        created_at_ms,
    )
}

fn insert_model_binding_identity(
    transaction: &Transaction<'_>,
    binding: &StoreBindingEvidence,
    blobs: &StoredBindingBlobs,
    created_at_ms: i64,
) -> Result<()> {
    let canonical_byte_len = checked_sql_usize(
        binding.manifest_canonical_bytes.len(),
        "model binding canonical artifact length",
    )?;
    let capabilities_byte_len = checked_sql_usize(
        blobs.capabilities_byte_len,
        "model binding capabilities length",
    )?;
    let capability_count =
        checked_sql_usize(binding.capabilities.len(), "model binding capability count")?;
    let model_byte_len = checked_sql_u64(binding.model_byte_len, "bound model length")?;
    let projector_sha256 = binding.projector_sha256.map(|value| value.to_string());
    transaction.execute(
        "INSERT OR IGNORE INTO research_model_bindings(
            binding_fingerprint,
            manifest_canonical_blob_id, manifest_canonical_byte_len, manifest_artifact_hash,
            binding_id, declared_role, model_sha256, model_byte_len,
            tokenizer_sha256, projector_sha256, architecture, context_tokens,
            capabilities_blob_id, capabilities_byte_len, capability_count, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, 'base_writer', ?6, ?7, ?8, ?9,
                   ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            binding.binding_fingerprint.to_string(),
            blobs.canonical.to_string(),
            canonical_byte_len,
            binding.manifest_artifact_hash.to_string(),
            binding.binding_id.as_str(),
            binding.model_sha256.to_string(),
            model_byte_len,
            binding.tokenizer_sha256.to_string(),
            projector_sha256,
            binding.architecture.as_str(),
            i64::from(binding.context_tokens),
            blobs.capabilities.to_string(),
            capabilities_byte_len,
            capability_count,
            created_at_ms,
        ],
    )?;
    let exact: i64 = transaction.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM research_model_bindings
            WHERE binding_fingerprint = ?1
              AND manifest_canonical_blob_id = ?2
              AND manifest_canonical_byte_len = ?3
              AND manifest_artifact_hash = ?4
              AND binding_id = ?5
              AND declared_role = 'base_writer'
              AND model_sha256 = ?6
              AND model_byte_len = ?7
              AND tokenizer_sha256 = ?8
              AND projector_sha256 IS ?9
              AND architecture = ?10
              AND context_tokens = ?11
              AND capabilities_blob_id = ?12
              AND capabilities_byte_len = ?13
              AND capability_count = ?14
        )",
        params![
            binding.binding_fingerprint.to_string(),
            blobs.canonical.to_string(),
            canonical_byte_len,
            binding.manifest_artifact_hash.to_string(),
            binding.binding_id.as_str(),
            binding.model_sha256.to_string(),
            model_byte_len,
            binding.tokenizer_sha256.to_string(),
            binding.projector_sha256.map(|value| value.to_string()),
            binding.architecture.as_str(),
            i64::from(binding.context_tokens),
            blobs.capabilities.to_string(),
            capabilities_byte_len,
            capability_count,
        ],
        |row| row.get(0),
    )?;
    if exact != 1 {
        return Err(admission_error(
            "persisted model binding conflicts with the compiled evidence",
        ));
    }
    Ok(())
}

fn insert_model_binding_source(
    transaction: &Transaction<'_>,
    binding: &StoreBindingEvidence,
    source_blob_id: BlobId,
    source_byte_len: i64,
    created_at_ms: i64,
) -> Result<()> {
    transaction.execute(
        "INSERT OR IGNORE INTO research_model_binding_sources(
            binding_fingerprint, manifest_source_hash,
            manifest_source_blob_id, manifest_source_byte_len, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            binding.binding_fingerprint.to_string(),
            binding.manifest_source_hash.to_string(),
            source_blob_id.to_string(),
            source_byte_len,
            created_at_ms,
        ],
    )?;
    let exact_source: i64 = transaction.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM research_model_binding_sources
            WHERE binding_fingerprint = ?1
              AND manifest_source_hash = ?2
              AND manifest_source_blob_id = ?3
              AND manifest_source_byte_len = ?4
        )",
        params![
            binding.binding_fingerprint.to_string(),
            binding.manifest_source_hash.to_string(),
            source_blob_id.to_string(),
            source_byte_len,
        ],
        |row| row.get(0),
    )?;
    if exact_source != 1 {
        return Err(admission_error(
            "persisted model binding source conflicts with exact source evidence",
        ));
    }
    Ok(())
}

fn insert_completed_adoption(
    transaction: &Transaction<'_>,
    material: &CompletedAdoptionMaterial,
    blobs: &StoredCaseBlobs,
    batch_fingerprint: BlobId,
    created_at_ms: i64,
) -> Result<()> {
    let CallTerminal::Completed(completed) = material.call.terminal() else {
        return Err(admission_error(
            "completed adoption changed terminal class before persistence",
        ));
    };
    insert_adopted_model_call(
        transaction,
        &material.call,
        Some(material.verification_fingerprint),
        blobs.call_record_blob_id,
        created_at_ms,
    )?;
    transaction.execute(
        "INSERT INTO research_call_terminals(
            call_id, status, raw_output_blob_id, raw_output_byte_len,
            token_ids_blob_id, token_count, token_ids_fingerprint,
            raw_event_stream_blob_id, backend_receipt_blob_id,
            terminal_message, created_at_ms
         ) VALUES (?1, 'completed', ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9)",
        params![
            material.call.id().to_string(),
            blobs.raw_output_blob_id.to_string(),
            checked_sql_usize(material.raw_output.len(), "completed raw output length")?,
            blobs.token_ids_blob_id.to_string(),
            checked_sql_usize(material.generated_token_ids.len(), "completed token count")?,
            completed
                .token_evidence()
                .token_ids_fingerprint()
                .to_string(),
            blobs.event_blob_id.to_string(),
            blobs.receipt_blob_id.to_string(),
            created_at_ms,
        ],
    )?;
    insert_completed_projection_evidence(transaction, material, blobs, created_at_ms)?;
    insert_batch_case(
        transaction,
        batch_fingerprint,
        material.input_index,
        &material.call,
        "completed",
        material.verification_fingerprint,
    )
}

fn insert_completed_projection_evidence(
    transaction: &Transaction<'_>,
    material: &CompletedAdoptionMaterial,
    blobs: &StoredCaseBlobs,
    created_at_ms: i64,
) -> Result<()> {
    let (
        has_projection,
        displayed_blob_id,
        displayed_byte_len,
        displayed_start,
        displayed_end,
        endpoint_start,
        endpoint_end,
        stop_start,
        stop_end,
    ) = match (&material.output_projection, blobs.displayed_output) {
        (Some(projection), Some((blob_id, byte_len))) => (
            1_i64,
            Some(blob_id.to_string()),
            Some(checked_sql_usize(byte_len, "displayed output length")?),
            Some(checked_sql_u64(
                projection.displayed().start(),
                "displayed projection start",
            )?),
            Some(checked_sql_u64(
                projection.displayed().end(),
                "displayed projection end",
            )?),
            Some(checked_sql_u64(
                projection.endpoint_excluded_tail().start(),
                "endpoint tail start",
            )?),
            Some(checked_sql_u64(
                projection.endpoint_excluded_tail().end(),
                "endpoint tail end",
            )?),
            Some(checked_sql_u64(
                projection.trimmed_stop_suffix().start(),
                "stop suffix start",
            )?),
            Some(checked_sql_u64(
                projection.trimmed_stop_suffix().end(),
                "stop suffix end",
            )?),
        ),
        (None, None) => (0_i64, None, None, None, None, None, None, None, None),
        _ => {
            return Err(admission_error(
                "stored displayed output disagrees with its output projection",
            ));
        }
    };
    transaction.execute(
        "INSERT INTO research_completed_call_evidence(
            call_id, raw_output_byte_len, has_output_projection,
            displayed_output_blob_id, displayed_output_byte_len,
            displayed_start_byte, displayed_end_byte,
            endpoint_tail_start_byte, endpoint_tail_end_byte,
            stop_suffix_start_byte, stop_suffix_end_byte,
            terminal_sampled_token_id, verification_fingerprint, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            material.call.id().to_string(),
            checked_sql_usize(material.raw_output.len(), "completed raw output length")?,
            has_projection,
            displayed_blob_id,
            displayed_byte_len,
            displayed_start,
            displayed_end,
            endpoint_start,
            endpoint_end,
            stop_start,
            stop_end,
            material.terminal_sampled_token_id,
            material.verification_fingerprint.to_string(),
            created_at_ms,
        ],
    )?;
    Ok(())
}

fn insert_cancelled_adoption(
    transaction: &Transaction<'_>,
    material: &CancelledAdoptionMaterial,
    blobs: &StoredCaseBlobs,
    batch_fingerprint: BlobId,
    created_at_ms: i64,
) -> Result<()> {
    let CallTerminal::Cancelled { message } = material.call.terminal() else {
        return Err(admission_error(
            "cancelled adoption changed terminal class before persistence",
        ));
    };
    insert_adopted_model_call(
        transaction,
        &material.call,
        None,
        blobs.call_record_blob_id,
        created_at_ms,
    )?;
    transaction.execute(
        "INSERT INTO research_call_terminals(
            call_id, status, raw_output_blob_id, raw_output_byte_len,
            token_ids_blob_id, token_count, token_ids_fingerprint,
            raw_event_stream_blob_id, backend_receipt_blob_id,
            terminal_message, created_at_ms
         ) VALUES (?1, 'cancelled', NULL, NULL, NULL, NULL, NULL,
                   NULL, NULL, ?2, ?3)",
        params![
            material.call.id().to_string(),
            message.as_str(),
            created_at_ms,
        ],
    )?;
    let token_evidence = TokenEvidence::from_exact(&material.generated_token_ids)?;
    transaction.execute(
        "INSERT INTO research_cancelled_call_diagnostics(
            call_id, partial_raw_output_blob_id, partial_raw_output_byte_len,
            token_ids_blob_id, token_count, token_ids_fingerprint,
            raw_event_stream_blob_id, backend_receipt_blob_id,
            verification_fingerprint, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            material.call.id().to_string(),
            blobs.raw_output_blob_id.to_string(),
            checked_sql_usize(
                material.partial_raw_output.len(),
                "cancelled partial output length",
            )?,
            blobs.token_ids_blob_id.to_string(),
            checked_sql_usize(material.generated_token_ids.len(), "cancelled token count")?,
            token_evidence.token_ids_fingerprint().to_string(),
            blobs.event_blob_id.to_string(),
            blobs.receipt_blob_id.to_string(),
            material.verification_fingerprint.to_string(),
            created_at_ms,
        ],
    )?;
    insert_batch_case(
        transaction,
        batch_fingerprint,
        material.input_index,
        &material.call,
        "cancelled",
        material.verification_fingerprint,
    )
}

fn insert_adopted_model_call(
    transaction: &Transaction<'_>,
    call: &ModelCall,
    verification_fingerprint: Option<BlobId>,
    call_record_blob_id: BlobId,
    created_at_ms: i64,
) -> Result<()> {
    let identity = call.identity();
    transaction.execute(
        "INSERT INTO research_model_calls(
            call_id, campaign_id, stage_id, stage_attempt_id, trial_case_id,
            seed_decimal, model_fingerprint, tokenizer_fingerprint, prompt_fingerprint,
            sampler_fingerprint, control_program_fingerprint, evidence_class,
            verification_audit_fingerprint, call_record_blob_id, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                   'live_base_writer_claim', ?12, ?13, ?14)",
        params![
            call.id().to_string(),
            identity.scope().campaign_id().to_string(),
            identity.scope().stage_id().to_string(),
            identity.scope().attempt_id().to_string(),
            identity.scope().case_id().to_string(),
            identity.seed().to_string(),
            identity.model_fingerprint().to_string(),
            identity.tokenizer_fingerprint().to_string(),
            identity.prompt_fingerprint().to_string(),
            identity.sampler_fingerprint().to_string(),
            identity.control_program_fingerprint().to_string(),
            verification_fingerprint.map(|fingerprint| fingerprint.to_string()),
            call_record_blob_id.to_string(),
            created_at_ms,
        ],
    )?;
    Ok(())
}

fn insert_batch_case(
    transaction: &Transaction<'_>,
    batch_fingerprint: BlobId,
    input_index: usize,
    call: &ModelCall,
    outcome: &'static str,
    case_fingerprint: BlobId,
) -> Result<()> {
    let identity = call.identity();
    let scope = identity.scope();
    transaction.execute(
        "INSERT INTO research_verified_inference_batch_calls(
            batch_verification_fingerprint, position, call_id,
            campaign_id, stage_id, stage_attempt_id, trial_case_id,
            seed_decimal, outcome, case_verification_fingerprint
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            batch_fingerprint.to_string(),
            checked_sql_usize(input_index, "verified case position")?,
            call.id().to_string(),
            scope.campaign_id().to_string(),
            scope.stage_id().to_string(),
            scope.attempt_id().to_string(),
            scope.case_id().to_string(),
            identity.seed().to_string(),
            outcome,
            case_fingerprint.to_string(),
        ],
    )?;
    Ok(())
}

fn insert_prompt_sources(
    transaction: &Transaction<'_>,
    batch_fingerprint: BlobId,
    prompt: &StorePromptEvidence,
) -> Result<()> {
    for (position, source) in prompt.sources.iter().enumerate() {
        let source_kind = match source.kind {
            StorePromptSourceKind::PrecedingExact => "preceding_exact",
            StorePromptSourceKind::TailLive => "tail_live",
            StorePromptSourceKind::TailAdmittedAssembly => "tail_admitted_assembly",
        };
        transaction.execute(
            "INSERT INTO research_verified_prompt_sources(
                batch_verification_fingerprint, position, block_index, source_index,
                source_kind, source_revision_id, source_blob_id,
                source_start_byte, source_end_byte, assembly_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                batch_fingerprint.to_string(),
                checked_sql_usize(position, "prompt source position")?,
                source.block_index,
                checked_sql_usize(source.source_index, "prompt source index")?,
                source_kind,
                source.revision_id.as_deref(),
                source.blob_id.to_string(),
                checked_sql_u64(source.range.start(), "prompt source start")?,
                checked_sql_u64(source.range.end(), "prompt source end")?,
                source.assembly_id.as_deref(),
            ],
        )?;
    }
    Ok(())
}

fn prompt_token_fingerprint(token_ids: &[u32]) -> BlobId {
    let mut bytes = canonical_prompt_digest_prefix("loom/exact-native-token-ids/v1");
    append_canonical_token_ids(&mut bytes, token_ids);
    BlobId::digest(&bytes)
}

fn compiled_prompt_fingerprint(
    source_prompt_fingerprint: BlobId,
    raw_blob_id: BlobId,
    form: PromptFormEvidence,
    token_policy: PromptTokenPolicyEvidence,
    token_ids: &[u32],
) -> BlobId {
    let mut bytes = canonical_prompt_digest_prefix("loom/compiled-base-completion-prompt/v1");
    bytes.extend_from_slice(source_prompt_fingerprint.as_bytes());
    bytes.extend_from_slice(raw_blob_id.as_bytes());
    bytes.extend_from_slice(&prompt_form_code(form).to_be_bytes());
    bytes.extend_from_slice(&prompt_token_policy_code(token_policy).to_be_bytes());
    append_canonical_token_ids(&mut bytes, token_ids);
    BlobId::digest(&bytes)
}

fn canonical_prompt_digest_prefix(domain: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(12 + domain.len());
    bytes.extend_from_slice(&(domain.len() as u64).to_be_bytes());
    bytes.extend_from_slice(domain.as_bytes());
    bytes.extend_from_slice(&1_u32.to_be_bytes());
    bytes
}

fn append_canonical_token_ids(bytes: &mut Vec<u8>, token_ids: &[u32]) {
    bytes.extend_from_slice(&(token_ids.len() as u64).to_be_bytes());
    for token_id in token_ids {
        bytes.extend_from_slice(&token_id.to_be_bytes());
    }
}

fn append_freeze_bytes(target: &mut Vec<u8>, value: &[u8]) {
    target.extend_from_slice(&(value.len() as u64).to_be_bytes());
    target.extend_from_slice(value);
}

fn append_freeze_text(target: &mut Vec<u8>, value: &str) {
    append_freeze_bytes(target, value.as_bytes());
}

fn append_freeze_optional_text(target: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            target.push(1);
            append_freeze_text(target, value);
        }
        None => target.push(0),
    }
}

fn frozen_prompt_source_fingerprint(
    snapshot: &FrozenPromptSourceSnapshot,
    frozen_at_ms: i64,
) -> BlobId {
    let mut bytes = canonical_prompt_digest_prefix("loom/frozen-prompt-source-lease/v1");
    append_freeze_text(&mut bytes, &snapshot.project_id.to_string());
    for id in [
        snapshot.scope.campaign_id().to_string(),
        snapshot.scope.stage_id().to_string(),
        snapshot.scope.attempt_id().to_string(),
        snapshot.scope.case_id().to_string(),
    ] {
        append_freeze_text(&mut bytes, &id);
    }
    bytes.extend_from_slice(snapshot.treatment_recipe_fingerprint.as_bytes());
    bytes.extend_from_slice(snapshot.specification_blob_id.as_bytes());
    bytes.extend_from_slice(&(snapshot.specification_bytes.len() as u64).to_be_bytes());
    bytes.extend_from_slice(snapshot.source_prompt_fingerprint.as_bytes());
    bytes.extend_from_slice(snapshot.raw_blob_id.as_bytes());
    bytes.extend_from_slice(&(snapshot.raw_utf8.len() as u64).to_be_bytes());
    bytes.extend_from_slice(&snapshot.tail_prompt_range.start().to_be_bytes());
    bytes.extend_from_slice(&snapshot.tail_prompt_range.end().to_be_bytes());
    append_freeze_optional_text(&mut bytes, snapshot.source_tail_revision_id.as_deref());
    bytes.extend_from_slice(snapshot.source_tail_blob_id.as_bytes());
    bytes.extend_from_slice(&snapshot.source_tail_range.start().to_be_bytes());
    bytes.extend_from_slice(&snapshot.source_tail_range.end().to_be_bytes());
    match &snapshot.source_tail_origin {
        StoreTailOrigin::LiveManuscript => bytes.push(0),
        StoreTailOrigin::AdmittedAssembly(assembly_id) => {
            bytes.push(1);
            append_freeze_text(&mut bytes, assembly_id);
        }
    }
    bytes.extend_from_slice(&(snapshot.sources.len() as u64).to_be_bytes());
    for source in &snapshot.sources {
        bytes.extend_from_slice(&source.block_index.to_be_bytes());
        bytes.extend_from_slice(&(source.source_index as u64).to_be_bytes());
        bytes.push(match source.kind {
            StorePromptSourceKind::PrecedingExact => 0,
            StorePromptSourceKind::TailLive => 1,
            StorePromptSourceKind::TailAdmittedAssembly => 2,
        });
        append_freeze_optional_text(&mut bytes, source.revision_id.as_deref());
        bytes.extend_from_slice(source.blob_id.as_bytes());
        bytes.extend_from_slice(&source.range.start().to_be_bytes());
        bytes.extend_from_slice(&source.range.end().to_be_bytes());
        append_freeze_optional_text(&mut bytes, source.assembly_id.as_deref());
    }
    bytes.extend_from_slice(&frozen_at_ms.to_be_bytes());
    BlobId::digest(&bytes)
}

fn snapshot_from_store_prompt(prompt: &StorePromptEvidence) -> FrozenPromptSourceSnapshot {
    FrozenPromptSourceSnapshot {
        specification_bytes: prompt.specification_bytes.clone(),
        specification_blob_id: prompt.specification_blob_id,
        project_id: prompt.project_id,
        scope: prompt.scope,
        treatment_recipe_fingerprint: prompt.treatment_recipe_fingerprint,
        source_prompt_fingerprint: prompt.source_prompt_fingerprint,
        content_fingerprint: prompt.content_fingerprint,
        tail_prompt_range: prompt.tail_prompt_range,
        source_tail_revision_id: prompt.source_tail_revision_id.clone(),
        source_tail_blob_id: prompt.source_tail_blob_id,
        source_tail_range: prompt.source_tail_range,
        source_tail_origin: prompt.source_tail_origin.clone(),
        sources: prompt.sources.clone(),
        raw_utf8: prompt.raw_utf8.clone(),
        raw_blob_id: prompt.raw_blob_id,
    }
}

const fn prompt_form_code(form: PromptFormEvidence) -> u32 {
    match form {
        PromptFormEvidence::Completion => 1,
    }
}

const fn prompt_token_policy_code(policy: PromptTokenPolicyEvidence) -> u32 {
    match policy {
        PromptTokenPolicyEvidence::NoBosParseSpecial => 1,
    }
}

const fn prompt_form_sql(form: PromptFormEvidence) -> &'static str {
    match form {
        PromptFormEvidence::Completion => "completion",
    }
}

const fn prompt_token_policy_sql(policy: PromptTokenPolicyEvidence) -> &'static str {
    match policy {
        PromptTokenPolicyEvidence::NoBosParseSpecial => "no_bos_parse_special",
    }
}

fn sql_blob_id(value: &str, field: &'static str) -> Result<BlobId> {
    BlobId::from_str(value)
        .map_err(|error| StoreError::CorruptDatabase(format!("invalid {field}: {error}")))
}

fn read_backend_evidence_blob(
    store: &ProjectStore,
    encoded_blob_id: &str,
    field: &'static str,
) -> Result<Vec<u8>> {
    let maximum = u64::try_from(MAX_BACKEND_EVIDENCE_BYTES)
        .map_err(|_| admission_error("backend evidence bound exceeds u64"))?;
    store.read_blob_bounded(sql_blob_id(encoded_blob_id, field)?, maximum)
}

fn sql_usize(value: i64, field: &'static str) -> Result<usize> {
    usize::try_from(value)
        .map_err(|_| StoreError::CorruptDatabase(format!("invalid {field}: {value}")))
}

fn decode_token_ids(bytes: &[u8], expected_count: usize) -> Result<Vec<u32>> {
    if bytes.len() != expected_count.saturating_mul(4) || !bytes.len().is_multiple_of(4) {
        return Err(StoreError::CorruptDatabase(
            "token evidence byte length is inconsistent".into(),
        ));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

fn load_stored_binding(
    store: &ProjectStore,
    binding_fingerprint: BlobId,
    manifest_source_hash: BlobId,
) -> Result<StoredBindingRow> {
    let raw = load_raw_stored_binding(store, binding_fingerprint, manifest_source_hash)?;
    stored_binding_from_raw(store, binding_fingerprint, raw)
}

fn load_raw_stored_binding(
    store: &ProjectStore,
    binding_fingerprint: BlobId,
    manifest_source_hash: BlobId,
) -> Result<RawStoredBindingRow> {
    Ok(store.connection.query_row(
        "SELECT source.manifest_source_blob_id, source.manifest_source_byte_len,
                source.manifest_source_hash, binding.manifest_canonical_blob_id,
                binding.manifest_canonical_byte_len, binding.manifest_artifact_hash,
                binding.binding_id, binding.declared_role, binding.model_sha256,
                binding.model_byte_len, binding.tokenizer_sha256,
                binding.projector_sha256, binding.architecture, binding.context_tokens,
                binding.capabilities_blob_id, binding.capabilities_byte_len,
                binding.capability_count
         FROM research_model_bindings binding
         JOIN research_model_binding_sources source USING (binding_fingerprint)
         WHERE binding.binding_fingerprint = ?1 AND source.manifest_source_hash = ?2",
        params![
            binding_fingerprint.to_string(),
            manifest_source_hash.to_string(),
        ],
        |row| {
            Ok(RawStoredBindingRow {
                source_blob_id: row.get(0)?,
                source_byte_len: row.get(1)?,
                source_hash: row.get(2)?,
                canonical_blob_id: row.get(3)?,
                canonical_byte_len: row.get(4)?,
                artifact_hash: row.get(5)?,
                binding_id: row.get(6)?,
                declared_role: row.get(7)?,
                model_sha256: row.get(8)?,
                model_byte_len: row.get(9)?,
                tokenizer_sha256: row.get(10)?,
                projector_sha256: row.get(11)?,
                architecture: row.get(12)?,
                context_tokens: row.get(13)?,
                capabilities_blob_id: row.get(14)?,
                capabilities_byte_len: row.get(15)?,
                capability_count: row.get(16)?,
            })
        },
    )?)
}

fn stored_binding_from_raw(
    store: &ProjectStore,
    binding_fingerprint: BlobId,
    raw: RawStoredBindingRow,
) -> Result<StoredBindingRow> {
    if raw.declared_role != "base_writer" {
        return Err(StoreError::CorruptDatabase(
            "stored model binding has an ineligible role".into(),
        ));
    }
    let source_blob_id = sql_blob_id(&raw.source_blob_id, "manifest source blob ID")?;
    let canonical_blob_id = sql_blob_id(&raw.canonical_blob_id, "canonical artifact blob ID")?;
    let capabilities_blob_id = sql_blob_id(&raw.capabilities_blob_id, "capabilities blob ID")?;
    let source_byte_len = sql_usize(raw.source_byte_len, "manifest source byte length")?;
    let canonical_byte_len = sql_usize(raw.canonical_byte_len, "canonical artifact byte length")?;
    let capabilities_byte_len = sql_usize(raw.capabilities_byte_len, "capabilities byte length")?;
    let capability_count = sql_usize(raw.capability_count, "capability count")?;
    let source_bytes = store.read_blob(source_blob_id)?;
    let canonical_bytes = store.read_blob(canonical_blob_id)?;
    let capabilities_bytes = store.read_blob(capabilities_blob_id)?;
    if source_bytes.len() != source_byte_len
        || canonical_bytes.len() != canonical_byte_len
        || capabilities_bytes.len() != capabilities_byte_len
    {
        return Err(StoreError::CorruptDatabase(
            "stored model binding blob length mismatch".into(),
        ));
    }
    let capabilities: Vec<String> = serde_json::from_slice(&capabilities_bytes)?;
    if capabilities.len() != capability_count
        || serde_json::to_vec(&capabilities)? != capabilities_bytes
    {
        return Err(StoreError::CorruptDatabase(
            "stored model capabilities are not canonical".into(),
        ));
    }
    let context_tokens = u32::try_from(raw.context_tokens).map_err(|_| {
        StoreError::CorruptDatabase("stored binding context tokens are invalid".into())
    })?;
    let model_byte_len = u64::try_from(raw.model_byte_len).map_err(|_| {
        StoreError::CorruptDatabase("stored binding model length is invalid".into())
    })?;
    let evidence = StoreBindingEvidence {
        binding_id: raw.binding_id,
        manifest_source_bytes: source_bytes,
        manifest_source_hash: sql_blob_id(&raw.source_hash, "manifest source hash")?,
        manifest_canonical_bytes: canonical_bytes,
        manifest_artifact_hash: sql_blob_id(&raw.artifact_hash, "manifest artifact hash")?,
        binding_fingerprint,
        declared_role: ModelRole::BaseWriter,
        model_sha256: sql_blob_id(&raw.model_sha256, "model digest")?,
        model_byte_len,
        tokenizer_sha256: sql_blob_id(&raw.tokenizer_sha256, "tokenizer digest")?,
        projector_sha256: raw
            .projector_sha256
            .as_deref()
            .map(|value| sql_blob_id(value, "projector digest"))
            .transpose()?,
        architecture: raw.architecture,
        context_tokens,
        capabilities,
    };
    if source_blob_id != evidence.manifest_source_hash
        || canonical_blob_id != evidence.manifest_artifact_hash
    {
        return Err(StoreError::CorruptDatabase(
            "stored manifest hash does not name its exact blob".into(),
        ));
    }
    validate_binding_evidence(&evidence)?;
    Ok(StoredBindingRow { evidence })
}

fn load_stored_batch(store: &ProjectStore, batch_fingerprint: BlobId) -> Result<StoredBatchRow> {
    let raw = load_raw_stored_batch(store, batch_fingerprint)?;
    stored_batch_from_raw(raw)
}

fn load_raw_stored_batch(
    store: &ProjectStore,
    batch_fingerprint: BlobId,
) -> Result<RawStoredBatchRow> {
    Ok(store.connection.query_row(
        "SELECT batch.project_id, batch.model_binding_fingerprint,
                batch.model_binding_source_hash, batch.runtime_model_fingerprint,
                batch.prompt_specification_blob_id,
                batch.prompt_specification_byte_len, batch.source_prompt_fingerprint,
                batch.prompt_content_fingerprint,
                batch.treatment_recipe_fingerprint, batch.prompt_source_count,
                batch.prompt_freeze_fingerprint, batch.prompt_frozen_at_ms,
                batch.prompt_campaign_id,
                batch.prompt_stage_id, batch.prompt_stage_attempt_id,
                batch.prompt_trial_case_id, batch.tail_prompt_start_byte,
                batch.tail_prompt_end_byte, batch.source_tail_revision_id,
                batch.source_tail_blob_id, batch.source_tail_start_byte,
                batch.source_tail_end_byte, batch.source_tail_origin,
                batch.source_tail_assembly_id, batch.exact_prompt_blob_id,
                batch.exact_prompt_byte_len, batch.prompt_form, batch.prompt_token_policy,
                batch.prompt_token_ids_blob_id, batch.prompt_token_count,
                batch.prompt_token_ids_fingerprint, batch.compiled_prompt_fingerprint,
                batch.expected_case_count, seal.completed_call_count, seal.cancelled_call_count,
                batch.native_request_id
         FROM research_verified_inference_batches batch
         JOIN research_verified_inference_batch_seals seal USING (batch_verification_fingerprint)
         WHERE batch.batch_verification_fingerprint = ?1",
        [batch_fingerprint.to_string()],
        |row| {
            Ok(RawStoredBatchRow {
                project_id: row.get(0)?,
                binding_fingerprint: row.get(1)?,
                binding_source_hash: row.get(2)?,
                runtime_model_fingerprint: row.get(3)?,
                prompt_specification_blob_id: row.get(4)?,
                prompt_specification_byte_len: row.get(5)?,
                source_prompt_fingerprint: row.get(6)?,
                prompt_content_fingerprint: row.get(7)?,
                treatment_recipe_fingerprint: row.get(8)?,
                prompt_source_count: row.get(9)?,
                prompt_freeze_fingerprint: row.get(10)?,
                prompt_frozen_at_ms: row.get(11)?,
                prompt_campaign_id: row.get(12)?,
                prompt_stage_id: row.get(13)?,
                prompt_stage_attempt_id: row.get(14)?,
                prompt_trial_case_id: row.get(15)?,
                tail_prompt_start_byte: row.get(16)?,
                tail_prompt_end_byte: row.get(17)?,
                source_tail_revision_id: row.get(18)?,
                source_tail_blob_id: row.get(19)?,
                source_tail_start_byte: row.get(20)?,
                source_tail_end_byte: row.get(21)?,
                source_tail_origin: row.get(22)?,
                source_tail_assembly_id: row.get(23)?,
                exact_prompt_blob_id: row.get(24)?,
                exact_prompt_byte_len: row.get(25)?,
                prompt_form: row.get(26)?,
                prompt_token_policy: row.get(27)?,
                prompt_token_ids_blob_id: row.get(28)?,
                prompt_token_count: row.get(29)?,
                prompt_token_ids_fingerprint: row.get(30)?,
                compiled_prompt_fingerprint: row.get(31)?,
                expected_case_count: row.get(32)?,
                completed_call_count: row.get(33)?,
                cancelled_call_count: row.get(34)?,
                native_request_id: row.get(35)?,
            })
        },
    )?)
}

fn stored_batch_from_raw(raw: RawStoredBatchRow) -> Result<StoredBatchRow> {
    Ok(StoredBatchRow {
        project_id: raw.project_id,
        binding_fingerprint: sql_blob_id(&raw.binding_fingerprint, "batch binding fingerprint")?,
        binding_source_hash: sql_blob_id(&raw.binding_source_hash, "batch binding source hash")?,
        runtime_model_fingerprint: sql_blob_id(
            &raw.runtime_model_fingerprint,
            "batch runtime model fingerprint",
        )?,
        prompt_specification_blob_id: sql_blob_id(
            &raw.prompt_specification_blob_id,
            "prompt specification blob ID",
        )?,
        prompt_specification_byte_len: sql_usize(
            raw.prompt_specification_byte_len,
            "prompt specification byte length",
        )?,
        source_prompt_fingerprint: sql_blob_id(
            &raw.source_prompt_fingerprint,
            "source prompt fingerprint",
        )?,
        prompt_content_fingerprint: sql_blob_id(
            &raw.prompt_content_fingerprint,
            "prompt content fingerprint",
        )?,
        treatment_recipe_fingerprint: sql_blob_id(
            &raw.treatment_recipe_fingerprint,
            "treatment recipe fingerprint",
        )?,
        prompt_source_count: sql_usize(raw.prompt_source_count, "prompt source count")?,
        prompt_freeze_fingerprint: sql_blob_id(
            &raw.prompt_freeze_fingerprint,
            "prompt freeze fingerprint",
        )?,
        prompt_frozen_at_ms: raw.prompt_frozen_at_ms,
        prompt_campaign_id: raw.prompt_campaign_id,
        prompt_stage_id: raw.prompt_stage_id,
        prompt_stage_attempt_id: raw.prompt_stage_attempt_id,
        prompt_trial_case_id: raw.prompt_trial_case_id,
        tail_prompt_start_byte: u64::try_from(raw.tail_prompt_start_byte).map_err(|_| {
            StoreError::CorruptDatabase("invalid compiled prompt tail start".into())
        })?,
        tail_prompt_end_byte: u64::try_from(raw.tail_prompt_end_byte)
            .map_err(|_| StoreError::CorruptDatabase("invalid compiled prompt tail end".into()))?,
        source_tail_revision_id: raw.source_tail_revision_id,
        source_tail_blob_id: sql_blob_id(&raw.source_tail_blob_id, "source tail blob ID")?,
        source_tail_start_byte: u64::try_from(raw.source_tail_start_byte)
            .map_err(|_| StoreError::CorruptDatabase("invalid source tail start".into()))?,
        source_tail_end_byte: u64::try_from(raw.source_tail_end_byte)
            .map_err(|_| StoreError::CorruptDatabase("invalid source tail end".into()))?,
        source_tail_origin: raw.source_tail_origin,
        source_tail_assembly_id: raw.source_tail_assembly_id,
        exact_prompt_blob_id: sql_blob_id(&raw.exact_prompt_blob_id, "exact prompt blob ID")?,
        exact_prompt_byte_len: sql_usize(raw.exact_prompt_byte_len, "exact prompt byte length")?,
        prompt_form: raw.prompt_form,
        prompt_token_policy: raw.prompt_token_policy,
        prompt_token_ids_blob_id: sql_blob_id(
            &raw.prompt_token_ids_blob_id,
            "prompt token blob ID",
        )?,
        prompt_token_count: sql_usize(raw.prompt_token_count, "prompt token count")?,
        prompt_token_ids_fingerprint: sql_blob_id(
            &raw.prompt_token_ids_fingerprint,
            "prompt token fingerprint",
        )?,
        compiled_prompt_fingerprint: sql_blob_id(
            &raw.compiled_prompt_fingerprint,
            "compiled prompt fingerprint",
        )?,
        expected_case_count: sql_usize(raw.expected_case_count, "expected case count")?,
        completed_call_count: sql_usize(raw.completed_call_count, "completed call count")?,
        cancelled_call_count: sql_usize(raw.cancelled_call_count, "cancelled call count")?,
        native_request_id: raw.native_request_id,
    })
}

fn load_stored_batch_cases(
    store: &ProjectStore,
    batch_fingerprint: BlobId,
) -> Result<Vec<StoredBatchCaseRow>> {
    struct RawRow {
        position: i64,
        call_id: String,
        campaign_id: String,
        stage_id: String,
        stage_attempt_id: String,
        trial_case_id: String,
        seed_decimal: String,
        outcome: String,
        verification_fingerprint: String,
    }
    let mut statement = store.connection.prepare(
        "SELECT position, call_id, campaign_id, stage_id, stage_attempt_id,
                trial_case_id, seed_decimal, outcome, case_verification_fingerprint
         FROM research_verified_inference_batch_calls
         WHERE batch_verification_fingerprint = ?1
         ORDER BY position",
    )?;
    let rows = statement.query_map([batch_fingerprint.to_string()], |row| {
        Ok(RawRow {
            position: row.get(0)?,
            call_id: row.get(1)?,
            campaign_id: row.get(2)?,
            stage_id: row.get(3)?,
            stage_attempt_id: row.get(4)?,
            trial_case_id: row.get(5)?,
            seed_decimal: row.get(6)?,
            outcome: row.get(7)?,
            verification_fingerprint: row.get(8)?,
        })
    })?;
    rows.map(|row| {
        let row = row?;
        Ok(StoredBatchCaseRow {
            position: sql_usize(row.position, "batch case position")?,
            call_id: row.call_id,
            campaign_id: row.campaign_id,
            stage_id: row.stage_id,
            stage_attempt_id: row.stage_attempt_id,
            trial_case_id: row.trial_case_id,
            seed_decimal: row.seed_decimal,
            outcome: row.outcome,
            verification_fingerprint: sql_blob_id(
                &row.verification_fingerprint,
                "case verification fingerprint",
            )?,
        })
    })
    .collect()
}

fn load_stored_prompt_sources(
    store: &ProjectStore,
    batch_fingerprint: BlobId,
) -> Result<Vec<StorePromptSourceEvidence>> {
    struct RawRow {
        position: i64,
        block_index: i64,
        source_index: i64,
        source_kind: String,
        revision_id: Option<String>,
        blob_id: String,
        start_byte: i64,
        end_byte: i64,
        assembly_id: Option<String>,
    }
    let mut statement = store.connection.prepare(
        "SELECT position, block_index, source_index, source_kind,
                source_revision_id, source_blob_id, source_start_byte,
                source_end_byte, assembly_id
         FROM research_verified_prompt_sources
         WHERE batch_verification_fingerprint = ?1
         ORDER BY position",
    )?;
    let rows = statement.query_map([batch_fingerprint.to_string()], |row| {
        Ok(RawRow {
            position: row.get(0)?,
            block_index: row.get(1)?,
            source_index: row.get(2)?,
            source_kind: row.get(3)?,
            revision_id: row.get(4)?,
            blob_id: row.get(5)?,
            start_byte: row.get(6)?,
            end_byte: row.get(7)?,
            assembly_id: row.get(8)?,
        })
    })?;
    rows.enumerate()
        .map(|(expected_position, row)| {
            let row = row?;
            if sql_usize(row.position, "prompt source position")? != expected_position {
                return Err(StoreError::CorruptDatabase(
                    "prompt source rows are not contiguous".into(),
                ));
            }
            let kind = match row.source_kind.as_str() {
                "preceding_exact" => StorePromptSourceKind::PrecedingExact,
                "tail_live" => StorePromptSourceKind::TailLive,
                "tail_admitted_assembly" => StorePromptSourceKind::TailAdmittedAssembly,
                _ => {
                    return Err(StoreError::CorruptDatabase(
                        "prompt source row has an unknown kind".into(),
                    ));
                }
            };
            let start = u64::try_from(row.start_byte)
                .map_err(|_| StoreError::CorruptDatabase("negative prompt source start".into()))?;
            let end = u64::try_from(row.end_byte)
                .map_err(|_| StoreError::CorruptDatabase("negative prompt source end".into()))?;
            Ok(StorePromptSourceEvidence {
                block_index: row.block_index,
                source_index: sql_usize(row.source_index, "prompt source index")?,
                kind,
                revision_id: row.revision_id,
                blob_id: sql_blob_id(&row.blob_id, "prompt source blob ID")?,
                range: NonEmptyByteRange::new(start, end).map_err(|error| {
                    StoreError::CorruptDatabase(format!("invalid prompt source range: {error}"))
                })?,
                assembly_id: row.assembly_id,
            })
        })
        .collect()
}

fn verify_source_tail_reference(store: &ProjectStore, batch: &StoredBatchRow) -> Result<()> {
    let exact: i64 = match batch.source_tail_origin.as_str() {
        "live_manuscript" => store.connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM revisions revision
                JOIN artifacts artifact USING (artifact_id)
                WHERE revision.revision_id = ?1 AND artifact.blob_id = ?2
             )",
            params![
                batch.source_tail_revision_id.as_deref(),
                batch.source_tail_blob_id.to_string(),
            ],
            |row| row.get(0),
        )?,
        "admitted_assembly" => store.connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM research_candidate_assemblies assembly
                WHERE assembly.assembly_id = ?1 AND assembly.assembled_blob_id = ?2
             )",
            params![
                batch.source_tail_assembly_id.as_deref(),
                batch.source_tail_blob_id.to_string(),
            ],
            |row| row.get(0),
        )?,
        _ => 0,
    };
    if exact != 1 {
        return Err(StoreError::CorruptDatabase(
            "source tail origin does not bind its immutable source".into(),
        ));
    }
    Ok(())
}

fn load_stored_call(store: &ProjectStore, call_id: &str) -> Result<StoredCallRow> {
    struct RawRow {
        campaign_id: String,
        stage_id: String,
        stage_attempt_id: String,
        trial_case_id: String,
        seed_decimal: String,
        model_fingerprint: String,
        tokenizer_fingerprint: String,
        prompt_fingerprint: String,
        sampler_fingerprint: String,
        control_program_fingerprint: String,
        evidence_class: String,
        verification_fingerprint: Option<String>,
        call_record_blob_id: String,
    }
    let raw = store.connection.query_row(
        "SELECT campaign_id, stage_id, stage_attempt_id, trial_case_id, seed_decimal,
                model_fingerprint, tokenizer_fingerprint, prompt_fingerprint,
                sampler_fingerprint, control_program_fingerprint,
                evidence_class, verification_audit_fingerprint, call_record_blob_id
         FROM research_model_calls WHERE call_id = ?1",
        [call_id],
        |row| {
            Ok(RawRow {
                campaign_id: row.get(0)?,
                stage_id: row.get(1)?,
                stage_attempt_id: row.get(2)?,
                trial_case_id: row.get(3)?,
                seed_decimal: row.get(4)?,
                model_fingerprint: row.get(5)?,
                tokenizer_fingerprint: row.get(6)?,
                prompt_fingerprint: row.get(7)?,
                sampler_fingerprint: row.get(8)?,
                control_program_fingerprint: row.get(9)?,
                evidence_class: row.get(10)?,
                verification_fingerprint: row.get(11)?,
                call_record_blob_id: row.get(12)?,
            })
        },
    )?;
    Ok(StoredCallRow {
        campaign_id: raw.campaign_id,
        stage_id: raw.stage_id,
        stage_attempt_id: raw.stage_attempt_id,
        trial_case_id: raw.trial_case_id,
        seed_decimal: raw.seed_decimal,
        model_fingerprint: sql_blob_id(&raw.model_fingerprint, "call model fingerprint")?,
        tokenizer_fingerprint: sql_blob_id(
            &raw.tokenizer_fingerprint,
            "call tokenizer fingerprint",
        )?,
        prompt_fingerprint: sql_blob_id(&raw.prompt_fingerprint, "call prompt fingerprint")?,
        sampler_fingerprint: sql_blob_id(&raw.sampler_fingerprint, "call sampler fingerprint")?,
        control_program_fingerprint: sql_blob_id(
            &raw.control_program_fingerprint,
            "call control-program fingerprint",
        )?,
        evidence_class: raw.evidence_class,
        verification_fingerprint: raw
            .verification_fingerprint
            .as_deref()
            .map(|value| sql_blob_id(value, "call verification fingerprint"))
            .transpose()?,
        call_record_blob_id: sql_blob_id(&raw.call_record_blob_id, "call record blob ID")?,
    })
}

fn verify_call_record(
    store: &ProjectStore,
    case: &StoredBatchCaseRow,
    stored: &StoredCallRow,
    batch: &StoredBatchRow,
    binding: &StoreBindingEvidence,
) -> Result<ModelCall> {
    let bytes = store.read_blob(stored.call_record_blob_id)?;
    let call: ModelCall = serde_json::from_slice(&bytes)?;
    if serde_json::to_vec(&call)? != bytes {
        return Err(StoreError::CorruptDatabase(
            "model call record is not in its canonical encoding".into(),
        ));
    }
    let identity = call.identity();
    let scope = identity.scope();
    if call.id().to_string() != case.call_id
        || call.evidence_class() != CallEvidenceClass::LiveBaseWriterClaim
        || stored.evidence_class != "live_base_writer_claim"
        || stored.campaign_id != case.campaign_id
        || stored.campaign_id != batch.prompt_campaign_id
        || stored.stage_id != case.stage_id
        || stored.stage_id != batch.prompt_stage_id
        || stored.stage_attempt_id != case.stage_attempt_id
        || stored.stage_attempt_id != batch.prompt_stage_attempt_id
        || stored.trial_case_id != case.trial_case_id
        || stored.trial_case_id != batch.prompt_trial_case_id
        || stored.seed_decimal != case.seed_decimal
        || scope.campaign_id().to_string() != case.campaign_id
        || scope.stage_id().to_string() != case.stage_id
        || scope.attempt_id().to_string() != case.stage_attempt_id
        || scope.case_id().to_string() != case.trial_case_id
        || identity.seed().to_string() != case.seed_decimal
        || stored.model_fingerprint != batch.runtime_model_fingerprint
        || identity.model_fingerprint() != batch.runtime_model_fingerprint
        || stored.tokenizer_fingerprint != binding.tokenizer_sha256
        || identity.tokenizer_fingerprint() != binding.tokenizer_sha256
        || stored.prompt_fingerprint != batch.compiled_prompt_fingerprint
        || identity.prompt_fingerprint() != batch.compiled_prompt_fingerprint
        || stored.sampler_fingerprint != identity.sampler_fingerprint()
        || stored.control_program_fingerprint != identity.control_program_fingerprint()
    {
        return Err(StoreError::CorruptDatabase(
            "model call record does not match its exact batch scope or fingerprints".into(),
        ));
    }
    Ok(call)
}

fn verify_completed_case(
    store: &ProjectStore,
    case: &StoredBatchCaseRow,
    call: ModelCall,
    stored_call: &StoredCallRow,
    compiled_prompt_fingerprint: BlobId,
) -> Result<CaseAdoptionMaterial> {
    let raw = load_raw_completed_case(store, &case.call_id)?;
    let (raw_output, generated_token_ids) = load_completed_output(store, &raw)?;
    let event_json = read_backend_evidence_blob(store, &raw.event_blob_id, "event stream blob ID")?;
    let backend_audit_json =
        read_backend_evidence_blob(store, &raw.receipt_blob_id, "backend receipt blob ID")?;
    let (displayed_output, output_projection) =
        load_completed_projection(store, &raw, &raw_output)?;
    let terminal_sampled_token_id = raw
        .terminal_sampled_token_id
        .map(|value| {
            i32::try_from(value).map_err(|_| {
                StoreError::CorruptDatabase("terminal sampled token exceeds i32".into())
            })
        })
        .transpose()?;
    let evidence_verification_fingerprint = sql_blob_id(
        &raw.evidence_verification_fingerprint,
        "completed evidence verification fingerprint",
    )?;
    if stored_call.verification_fingerprint != Some(case.verification_fingerprint)
        || evidence_verification_fingerprint != case.verification_fingerprint
    {
        return Err(StoreError::CorruptDatabase(
            "completed call verification fingerprints disagree".into(),
        ));
    }
    let material = CompletedAdoptionMaterial {
        input_index: case.position,
        call,
        raw_output,
        generated_token_ids,
        event_json,
        backend_audit_json,
        displayed_output,
        output_projection,
        terminal_sampled_token_id,
        verification_fingerprint: case.verification_fingerprint,
    };
    validate_completed_adoption(&material, compiled_prompt_fingerprint)?;
    Ok(CaseAdoptionMaterial::Completed(material))
}

fn load_raw_completed_case(store: &ProjectStore, call_id: &str) -> Result<RawCompletedCaseRow> {
    Ok(store.connection.query_row(
        "SELECT terminal.raw_output_blob_id, terminal.raw_output_byte_len,
                terminal.token_ids_blob_id, terminal.token_count,
                terminal.token_ids_fingerprint, terminal.raw_event_stream_blob_id,
                terminal.backend_receipt_blob_id, completed.raw_output_byte_len,
                completed.has_output_projection, completed.displayed_output_blob_id,
                completed.displayed_output_byte_len, completed.displayed_start_byte,
                completed.displayed_end_byte, completed.endpoint_tail_start_byte,
                completed.endpoint_tail_end_byte, completed.stop_suffix_start_byte,
                completed.stop_suffix_end_byte, completed.terminal_sampled_token_id,
                completed.verification_fingerprint
         FROM research_call_terminals terminal
         JOIN research_completed_call_evidence completed USING (call_id)
         WHERE terminal.call_id = ?1 AND terminal.status = 'completed'",
        [call_id],
        |row| {
            Ok(RawCompletedCaseRow {
                raw_output_blob_id: row.get(0)?,
                raw_output_byte_len: row.get(1)?,
                token_ids_blob_id: row.get(2)?,
                token_count: row.get(3)?,
                token_ids_fingerprint: row.get(4)?,
                event_blob_id: row.get(5)?,
                receipt_blob_id: row.get(6)?,
                evidence_raw_output_byte_len: row.get(7)?,
                has_projection: row.get(8)?,
                displayed_blob_id: row.get(9)?,
                displayed_byte_len: row.get(10)?,
                displayed_start: row.get(11)?,
                displayed_end: row.get(12)?,
                endpoint_start: row.get(13)?,
                endpoint_end: row.get(14)?,
                stop_start: row.get(15)?,
                stop_end: row.get(16)?,
                terminal_sampled_token_id: row.get(17)?,
                evidence_verification_fingerprint: row.get(18)?,
            })
        },
    )?)
}

fn load_completed_output(
    store: &ProjectStore,
    raw: &RawCompletedCaseRow,
) -> Result<(Vec<u8>, Vec<u32>)> {
    let raw_output_byte_len = sql_usize(raw.raw_output_byte_len, "raw output byte length")?;
    if raw_output_byte_len
        != sql_usize(
            raw.evidence_raw_output_byte_len,
            "projected raw output byte length",
        )?
    {
        return Err(StoreError::CorruptDatabase(
            "completed call raw output lengths disagree".into(),
        ));
    }
    let raw_output = store.read_blob(sql_blob_id(
        &raw.raw_output_blob_id,
        "completed raw output blob ID",
    )?)?;
    if raw_output.len() != raw_output_byte_len {
        return Err(StoreError::CorruptDatabase(
            "completed raw output blob length mismatch".into(),
        ));
    }
    let token_count = sql_usize(raw.token_count, "generated token count")?;
    let token_bytes = store.read_blob(sql_blob_id(
        &raw.token_ids_blob_id,
        "completed token blob ID",
    )?)?;
    let generated_token_ids = decode_token_ids(&token_bytes, token_count)?;
    let token_fingerprint = sql_blob_id(&raw.token_ids_fingerprint, "terminal token fingerprint")?;
    if TokenEvidence::from_exact(&generated_token_ids)?.token_ids_fingerprint() != token_fingerprint
    {
        return Err(StoreError::CorruptDatabase(
            "generated token evidence fingerprint mismatch".into(),
        ));
    }
    Ok((raw_output, generated_token_ids))
}

fn load_completed_projection(
    store: &ProjectStore,
    raw: &RawCompletedCaseRow,
    raw_output: &[u8],
) -> Result<(Vec<u8>, Option<OutputProjection>)> {
    let projection = match raw.has_projection {
        0 => (Vec::new(), None),
        1 => {
            let displayed_blob_id = raw.displayed_blob_id.as_deref().ok_or_else(|| {
                StoreError::CorruptDatabase("completed projection has no displayed blob".into())
            })?;
            let displayed_output =
                store.read_blob(sql_blob_id(displayed_blob_id, "displayed output blob ID")?)?;
            let displayed_byte_len = sql_usize(
                raw.displayed_byte_len.ok_or_else(|| {
                    StoreError::CorruptDatabase(
                        "completed projection has no displayed length".into(),
                    )
                })?,
                "displayed output byte length",
            )?;
            if displayed_output.len() != displayed_byte_len {
                return Err(StoreError::CorruptDatabase(
                    "displayed output blob length mismatch".into(),
                ));
            }
            let range = |value: Option<i64>, field: &'static str| -> Result<u64> {
                u64::try_from(value.ok_or_else(|| {
                    StoreError::CorruptDatabase(format!("completed projection has no {field}"))
                })?)
                .map_err(|_| StoreError::CorruptDatabase(format!("invalid {field}")))
            };
            let displayed_start = range(raw.displayed_start, "displayed start")?;
            let displayed_end = range(raw.displayed_end, "displayed end")?;
            let endpoint_start = range(raw.endpoint_start, "endpoint start")?;
            let endpoint_end = range(raw.endpoint_end, "endpoint end")?;
            let stop_start = range(raw.stop_start, "stop start")?;
            let stop_end = range(raw.stop_end, "stop end")?;
            let projection = OutputProjection::new(raw_output, displayed_end, endpoint_end)?;
            if projection.displayed().start() != displayed_start
                || projection.endpoint_excluded_tail().start() != endpoint_start
                || projection.trimmed_stop_suffix().start() != stop_start
                || projection.trimmed_stop_suffix().end() != stop_end
            {
                return Err(StoreError::CorruptDatabase(
                    "completed output projection ranges do not reconstruct".into(),
                ));
            }
            (displayed_output, Some(projection))
        }
        _ => {
            return Err(StoreError::CorruptDatabase(
                "invalid output projection discriminator".into(),
            ));
        }
    };
    Ok(projection)
}

fn verify_cancelled_case(
    store: &ProjectStore,
    case: &StoredBatchCaseRow,
    call: ModelCall,
    stored_call: &StoredCallRow,
    compiled_prompt_fingerprint: BlobId,
) -> Result<CaseAdoptionMaterial> {
    struct RawRow {
        terminal_message: String,
        partial_output_blob_id: String,
        partial_output_byte_len: i64,
        token_ids_blob_id: String,
        token_count: i64,
        token_ids_fingerprint: String,
        event_blob_id: String,
        receipt_blob_id: String,
        verification_fingerprint: String,
    }
    let raw = store.connection.query_row(
        "SELECT terminal.terminal_message, diagnostic.partial_raw_output_blob_id,
                diagnostic.partial_raw_output_byte_len, diagnostic.token_ids_blob_id,
                diagnostic.token_count, diagnostic.token_ids_fingerprint,
                diagnostic.raw_event_stream_blob_id, diagnostic.backend_receipt_blob_id,
                diagnostic.verification_fingerprint
         FROM research_call_terminals terminal
         JOIN research_cancelled_call_diagnostics diagnostic USING (call_id)
         WHERE terminal.call_id = ?1 AND terminal.status = 'cancelled'",
        [case.call_id.as_str()],
        |row| {
            Ok(RawRow {
                terminal_message: row.get(0)?,
                partial_output_blob_id: row.get(1)?,
                partial_output_byte_len: row.get(2)?,
                token_ids_blob_id: row.get(3)?,
                token_count: row.get(4)?,
                token_ids_fingerprint: row.get(5)?,
                event_blob_id: row.get(6)?,
                receipt_blob_id: row.get(7)?,
                verification_fingerprint: row.get(8)?,
            })
        },
    )?;
    let CallTerminal::Cancelled { message } = call.terminal() else {
        return Err(StoreError::CorruptDatabase(
            "cancelled batch case contains another terminal class".into(),
        ));
    };
    if message.as_str() != raw.terminal_message || stored_call.verification_fingerprint.is_some() {
        return Err(StoreError::CorruptDatabase(
            "cancelled terminal message or authority state mismatch".into(),
        ));
    }
    let partial_raw_output = store.read_blob(sql_blob_id(
        &raw.partial_output_blob_id,
        "cancelled partial output blob ID",
    )?)?;
    if partial_raw_output.len()
        != sql_usize(
            raw.partial_output_byte_len,
            "cancelled partial output length",
        )?
    {
        return Err(StoreError::CorruptDatabase(
            "cancelled partial output length mismatch".into(),
        ));
    }
    let token_count = sql_usize(raw.token_count, "cancelled token count")?;
    let token_bytes = store.read_blob(sql_blob_id(
        &raw.token_ids_blob_id,
        "cancelled token blob ID",
    )?)?;
    let generated_token_ids = decode_token_ids(&token_bytes, token_count)?;
    if TokenEvidence::from_exact(&generated_token_ids)?.token_ids_fingerprint()
        != sql_blob_id(&raw.token_ids_fingerprint, "cancelled token fingerprint")?
    {
        return Err(StoreError::CorruptDatabase(
            "cancelled token fingerprint mismatch".into(),
        ));
    }
    let verification_fingerprint = sql_blob_id(
        &raw.verification_fingerprint,
        "cancelled verification fingerprint",
    )?;
    if verification_fingerprint != case.verification_fingerprint {
        return Err(StoreError::CorruptDatabase(
            "cancelled verification fingerprint mismatch".into(),
        ));
    }
    let material = CancelledAdoptionMaterial {
        input_index: case.position,
        call,
        partial_raw_output,
        generated_token_ids,
        event_json: read_backend_evidence_blob(
            store,
            &raw.event_blob_id,
            "cancelled event blob ID",
        )?,
        backend_audit_json: read_backend_evidence_blob(
            store,
            &raw.receipt_blob_id,
            "cancelled receipt blob ID",
        )?,
        verification_fingerprint,
    };
    validate_cancelled_adoption(&material, compiled_prompt_fingerprint)?;
    Ok(CaseAdoptionMaterial::Cancelled(material))
}

#[cfg(test)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeBaseWriterReceiptV1 {
    format: String,
    call_id: loom_research_types::ModelCallId,
    evidence_class: CallEvidenceClass,
    scope: loom_research_types::CallScope,
    seed: u64,
    model_fingerprint: BlobId,
    tokenizer_fingerprint: BlobId,
    prompt_fingerprint: BlobId,
    sampler_fingerprint: BlobId,
    control_program_fingerprint: BlobId,
    raw_output_blob_id: BlobId,
    raw_output_byte_len: u64,
    token_count: u32,
    token_ids_fingerprint: BlobId,
    raw_event_stream_blob_id: BlobId,
    execution_instance_fingerprint: BlobId,
    token_byte_boundaries: Option<Vec<u64>>,
    started_at_ms: i64,
    completed_at_ms: i64,
}

#[cfg(test)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeCallEventStreamV1 {
    format: String,
    call_id: loom_research_types::ModelCallId,
    events: Vec<NativeCallEventV1>,
}

#[cfg(test)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeCallEventV1 {
    sequence: u64,
    occurred_at_ms: i64,
    kind: NativeCallEventKind,
    evidence_fingerprint: BlobId,
}

#[cfg(test)]
#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum NativeCallEventKind {
    CallStarted,
    BackendEvent,
    CallCompleted,
}

struct ReplayedPromptTail {
    prompt_range: NonEmptyByteRange,
    source_range: NonEmptyByteRange,
    origin: StoreTailOrigin,
}

fn validate_stored_batch_header(store: &ProjectStore, batch: &StoredBatchRow) -> Result<()> {
    if batch.project_id != store.manifest.project_id.to_string()
        || batch.expected_case_count == 0
        || batch.expected_case_count > MAX_BASE_WRITER_BATCH_CASES
        || batch.completed_call_count + batch.cancelled_call_count != batch.expected_case_count
        || batch.native_request_id.is_empty()
        || batch.native_request_id.len() > 256
        || batch.prompt_frozen_at_ms <= 0
    {
        return Err(StoreError::CorruptDatabase(
            "sealed inference batch has invalid project or case counts".into(),
        ));
    }
    Ok(())
}

fn load_replayed_prompt_tail(
    store: &ProjectStore,
    batch: &StoredBatchRow,
    prompt_bytes: &[u8],
) -> Result<ReplayedPromptTail> {
    let prompt_range =
        NonEmptyByteRange::new(batch.tail_prompt_start_byte, batch.tail_prompt_end_byte).map_err(
            |error| {
                StoreError::CorruptDatabase(format!("invalid compiled prompt tail range: {error}"))
            },
        )?;
    let source_range =
        NonEmptyByteRange::new(batch.source_tail_start_byte, batch.source_tail_end_byte).map_err(
            |error| {
                StoreError::CorruptDatabase(format!("invalid source prompt tail range: {error}"))
            },
        )?;
    let origin = match batch.source_tail_origin.as_str() {
        "live_manuscript"
            if batch.source_tail_revision_id.is_some()
                && batch.source_tail_assembly_id.is_none() =>
        {
            StoreTailOrigin::LiveManuscript
        }
        "admitted_assembly" if batch.source_tail_revision_id.is_none() => {
            StoreTailOrigin::AdmittedAssembly(batch.source_tail_assembly_id.clone().ok_or_else(
                || StoreError::CorruptDatabase("admitted source tail has no assembly ID".into()),
            )?)
        }
        _ => {
            return Err(StoreError::CorruptDatabase(
                "invalid source tail origin".into(),
            ));
        }
    };
    verify_source_tail_reference(store, batch)?;
    let source_bytes = store.read_blob(batch.source_tail_blob_id)?;
    if batch.source_tail_end_byte != source_bytes.len() as u64 {
        return Err(StoreError::CorruptDatabase(
            "source tail does not end at its exact source EOF".into(),
        ));
    }
    let source_tail = source_range
        .as_range()
        .checked_slice(&source_bytes)
        .map_err(|error| StoreError::CorruptDatabase(format!("invalid source tail: {error}")))?;
    let prompt_tail = prompt_range
        .as_range()
        .checked_slice(prompt_bytes)
        .map_err(|error| StoreError::CorruptDatabase(format!("invalid prompt tail: {error}")))?;
    if source_tail != prompt_tail {
        return Err(StoreError::CorruptDatabase(
            "source tail bytes differ from the final compiled prompt bytes".into(),
        ));
    }
    Ok(ReplayedPromptTail {
        prompt_range,
        source_range,
        origin,
    })
}

fn validate_stored_prompt_sources(
    store: &ProjectStore,
    batch_fingerprint: BlobId,
    batch: &StoredBatchRow,
    prompt: &StorePromptEvidence,
) -> Result<()> {
    let stored = load_stored_prompt_sources(store, batch_fingerprint)?;
    if stored.len() != batch.prompt_source_count || stored.len() != prompt.sources.len() {
        return Err(StoreError::CorruptDatabase(
            "stored prompt source count does not replay".into(),
        ));
    }
    if stored
        .iter()
        .zip(&prompt.sources)
        .any(|(stored, expected)| {
            stored.block_index != expected.block_index
                || stored.source_index != expected.source_index
                || stored.kind != expected.kind
                || stored.revision_id != expected.revision_id
                || stored.blob_id != expected.blob_id
                || stored.range != expected.range
                || stored.assembly_id != expected.assembly_id
        })
    {
        return Err(StoreError::CorruptDatabase(
            "stored prompt source row differs from the frozen specification".into(),
        ));
    }
    Ok(())
}

fn validate_stored_prompt_specification(
    batch: &StoredBatchRow,
    specification: &FrozenBaseCompletionPrompt,
) -> Result<()> {
    let scope = specification.scope();
    if specification.project_id().to_string() != batch.project_id
        || specification.treatment_recipe_fingerprint() != batch.treatment_recipe_fingerprint
        || scope.campaign_id().to_string() != batch.prompt_campaign_id
        || scope.stage_id().to_string() != batch.prompt_stage_id
        || scope.attempt_id().to_string() != batch.prompt_stage_attempt_id
        || scope.case_id().to_string() != batch.prompt_trial_case_id
    {
        return Err(StoreError::CorruptDatabase(
            "frozen prompt specification differs from its batch index".into(),
        ));
    }
    Ok(())
}

fn load_replayed_prompt_evidence(
    store: &ProjectStore,
    batch_fingerprint: BlobId,
    batch: &StoredBatchRow,
) -> Result<StorePromptEvidence> {
    let specification_bytes = store.read_blob(batch.prompt_specification_blob_id)?;
    let prompt_bytes = store.read_blob(batch.exact_prompt_blob_id)?;
    if specification_bytes.len() != batch.prompt_specification_byte_len
        || prompt_bytes.len() != batch.exact_prompt_byte_len
    {
        return Err(StoreError::CorruptDatabase(
            "frozen specification or exact prompt blob length mismatch".into(),
        ));
    }
    let specification: FrozenBaseCompletionPrompt = serde_json::from_slice(&specification_bytes)?;
    validate_stored_prompt_specification(batch, &specification)?;
    let tail = load_replayed_prompt_tail(store, batch, &prompt_bytes)?;
    let token_bytes = store.read_blob(batch.prompt_token_ids_blob_id)?;
    let ordered_token_ids = decode_token_ids(&token_bytes, batch.prompt_token_count)?;
    let form = match batch.prompt_form.as_str() {
        "completion" => PromptFormEvidence::Completion,
        _ => {
            return Err(StoreError::CorruptDatabase(
                "unsupported stored prompt form".into(),
            ));
        }
    };
    let token_policy = match batch.prompt_token_policy.as_str() {
        "no_bos_parse_special" => PromptTokenPolicyEvidence::NoBosParseSpecial,
        _ => {
            return Err(StoreError::CorruptDatabase(
                "unsupported stored prompt token policy".into(),
            ));
        }
    };
    let prompt = StorePromptEvidence {
        specification_blob_id: batch.prompt_specification_blob_id,
        specification_bytes,
        project_id: store.manifest.project_id,
        scope: specification.scope(),
        treatment_recipe_fingerprint: batch.treatment_recipe_fingerprint,
        source_prompt_fingerprint: batch.source_prompt_fingerprint,
        content_fingerprint: batch.prompt_content_fingerprint,
        tail_prompt_range: tail.prompt_range,
        source_tail_revision_id: batch.source_tail_revision_id.clone(),
        source_tail_blob_id: batch.source_tail_blob_id,
        source_tail_range: tail.source_range,
        source_tail_origin: tail.origin,
        sources: exact_prompt_sources(&specification)?,
        raw_blob_id: batch.exact_prompt_blob_id,
        raw_utf8: prompt_bytes,
        form,
        token_policy,
        ordered_token_ids,
        token_fingerprint: batch.prompt_token_ids_fingerprint,
        compiled_fingerprint: batch.compiled_prompt_fingerprint,
    };
    validate_prompt_adoption(&prompt)?;
    validate_prompt_source_graph(store, &prompt, false)?;
    validate_stored_prompt_sources(store, batch_fingerprint, batch, &prompt)?;
    let snapshot = snapshot_from_store_prompt(&prompt);
    if frozen_prompt_source_fingerprint(&snapshot, batch.prompt_frozen_at_ms)
        != batch.prompt_freeze_fingerprint
    {
        return Err(StoreError::CorruptDatabase(
            "stored prompt-source freeze fingerprint does not replay".into(),
        ));
    }
    Ok(prompt)
}

struct ReplayedBatchCases {
    cases: Vec<CaseAdoptionMaterial>,
    completed: usize,
    cancelled: usize,
}

fn replay_stored_batch_cases(
    store: &ProjectStore,
    batch_fingerprint: BlobId,
    batch: &StoredBatchRow,
    binding: &StoreBindingEvidence,
) -> Result<ReplayedBatchCases> {
    let indexed = load_stored_batch_cases(store, batch_fingerprint)?;
    if indexed.len() != batch.expected_case_count {
        return Err(StoreError::CorruptDatabase(
            "sealed inference batch case count mismatch".into(),
        ));
    }
    let mut call_ids = BTreeSet::new();
    let mut replayed = ReplayedBatchCases {
        cases: Vec::with_capacity(indexed.len()),
        completed: 0,
        cancelled: 0,
    };
    for (expected_position, case) in indexed.iter().enumerate() {
        if case.position != expected_position || !call_ids.insert(case.call_id.as_str()) {
            return Err(StoreError::CorruptDatabase(
                "sealed inference cases are not contiguous and unique".into(),
            ));
        }
        let stored_call = load_stored_call(store, &case.call_id)?;
        let call = verify_call_record(store, case, &stored_call, batch, binding)?;
        let material = match case.outcome.as_str() {
            "completed" => {
                replayed.completed += 1;
                verify_completed_case(
                    store,
                    case,
                    call,
                    &stored_call,
                    batch.compiled_prompt_fingerprint,
                )?
            }
            "cancelled" => {
                replayed.cancelled += 1;
                verify_cancelled_case(
                    store,
                    case,
                    call,
                    &stored_call,
                    batch.compiled_prompt_fingerprint,
                )?
            }
            _ => {
                return Err(StoreError::CorruptDatabase(
                    "sealed inference case has an unknown outcome".into(),
                ));
            }
        };
        replayed.cases.push(material);
    }
    if replayed.completed != batch.completed_call_count
        || replayed.cancelled != batch.cancelled_call_count
    {
        return Err(StoreError::CorruptDatabase(
            "sealed inference outcome counts do not replay".into(),
        ));
    }
    Ok(replayed)
}

fn persisted_case_ref(case: &CaseAdoptionMaterial) -> PersistedInferenceCaseRef<'_> {
    match case {
        CaseAdoptionMaterial::Completed(material) => PersistedInferenceCaseRef {
            input_index: material.input_index,
            model_call: &material.call,
            raw_output: &material.raw_output,
            generated_token_ids: &material.generated_token_ids,
            event_json: &material.event_json,
            backend_audit_json: &material.backend_audit_json,
            terminal_sampled_token_id: material.terminal_sampled_token_id,
            outcome: PersistedCaseOutcomeRef::Completed {
                displayed_output: &material.displayed_output,
                output_projection: material.output_projection.as_ref(),
            },
            verification_fingerprint: material.verification_fingerprint,
        },
        CaseAdoptionMaterial::Cancelled(material) => PersistedInferenceCaseRef {
            input_index: material.input_index,
            model_call: &material.call,
            raw_output: &material.partial_raw_output,
            generated_token_ids: &material.generated_token_ids,
            event_json: &material.event_json,
            backend_audit_json: &material.backend_audit_json,
            terminal_sampled_token_id: None,
            outcome: PersistedCaseOutcomeRef::Cancelled,
            verification_fingerprint: material.verification_fingerprint,
        },
    }
}

fn verify_native_batch_replay(
    binding: &StoreBindingEvidence,
    prompt: &StorePromptEvidence,
    batch: &StoredBatchRow,
    replayed: &ReplayedBatchCases,
    batch_fingerprint: BlobId,
) -> Result<()> {
    verify_native_material(NativeMaterialReplay {
        binding,
        prompt,
        backend_request_id: &batch.native_request_id,
        cases: &replayed.cases,
        runtime_model_fingerprint: batch.runtime_model_fingerprint,
        batch_fingerprint,
        expected_completed: replayed.completed,
        expected_cancelled: replayed.cancelled,
    })
}

#[derive(Clone, Copy)]
struct NativeMaterialReplay<'a> {
    binding: &'a StoreBindingEvidence,
    prompt: &'a StorePromptEvidence,
    backend_request_id: &'a str,
    cases: &'a [CaseAdoptionMaterial],
    runtime_model_fingerprint: BlobId,
    batch_fingerprint: BlobId,
    expected_completed: usize,
    expected_cancelled: usize,
}

fn verify_native_material(input: NativeMaterialReplay<'_>) -> Result<()> {
    let cases = input
        .cases
        .iter()
        .map(persisted_case_ref)
        .collect::<Vec<_>>();
    let checked = verify_persisted_batch_evidence(&PersistedInferenceBatchRef {
        binding: PersistedBindingEvidenceRef {
            binding_id: &input.binding.binding_id,
            binding_fingerprint: input.binding.binding_fingerprint,
            model_sha256: input.binding.model_sha256,
            model_byte_len: input.binding.model_byte_len,
            tokenizer_sha256: input.binding.tokenizer_sha256,
            multimodal_projector_sha256: input.binding.projector_sha256,
            context_tokens: input.binding.context_tokens,
        },
        prompt: PersistedPromptEvidenceRef {
            project_id: input.prompt.project_id,
            scope: input.prompt.scope,
            source_prompt_fingerprint: input.prompt.source_prompt_fingerprint,
            content_fingerprint: input.prompt.content_fingerprint,
            treatment_recipe_fingerprint: input.prompt.treatment_recipe_fingerprint,
            raw_utf8: &input.prompt.raw_utf8,
            raw_blob_id: input.prompt.raw_blob_id,
            form: input.prompt.form,
            token_policy: input.prompt.token_policy,
            ordered_token_ids: &input.prompt.ordered_token_ids,
            token_fingerprint: input.prompt.token_fingerprint,
            compiled_fingerprint: input.prompt.compiled_fingerprint,
        },
        runtime_model_fingerprint: input.runtime_model_fingerprint,
        backend_request_id: input.backend_request_id,
        cases: &cases,
        verification_fingerprint: input.batch_fingerprint,
    })
    .map_err(|error| {
        StoreError::CorruptDatabase(format!("native inference evidence replay failed: {error}"))
    })?;
    if checked.request_id() != input.backend_request_id
        || checked.runtime_model_fingerprint() != input.runtime_model_fingerprint
        || checked.verification_fingerprint() != input.batch_fingerprint
        || checked.completed_case_count() != input.expected_completed
        || checked.cancelled_case_count() != input.expected_cancelled
        || checked.cases().len() != cases.len()
    {
        return Err(StoreError::CorruptDatabase(
            "checked native inference facts differ from the sealed batch".into(),
        ));
    }
    Ok(())
}

fn preflight_batch_adoption(
    store: &ProjectStore,
    material: &BatchAdoptionMaterial,
) -> Result<BlobId> {
    if material.project_id != store.manifest.project_id
        || material.prompt_evidence.project_id != material.project_id
    {
        return Err(admission_error(
            "verified inference authority belongs to another project",
        ));
    }
    if material.backend_request_id.is_empty() || material.backend_request_id.len() > 256 {
        return Err(admission_error(
            "verified backend request ID is empty or exceeds 256 bytes",
        ));
    }
    let freeze = material.prompt_freeze.ok_or_else(|| {
        admission_error("verified inference adoption has no consumed prompt-source lease")
    })?;
    if freeze.frozen_at_ms <= 0 {
        return Err(admission_error(
            "verified inference prompt-source lease has an invalid freeze time",
        ));
    }
    validate_binding_evidence(&material.binding)?;
    validate_prompt_adoption(&material.prompt_evidence)?;
    validate_prompt_source_graph(store, &material.prompt_evidence, false)?;
    let snapshot = snapshot_from_store_prompt(&material.prompt_evidence);
    if frozen_prompt_source_fingerprint(&snapshot, freeze.frozen_at_ms) != freeze.fingerprint {
        return Err(admission_error(
            "verified inference prompt-source freeze fingerprint does not replay",
        ));
    }
    preflight_adoption_cases(material)
}

fn consume_prompt_source_lease(
    store: &ProjectStore,
    material: &mut BatchAdoptionMaterial,
    lease: FrozenPromptSourceLease,
) -> Result<()> {
    let FrozenPromptSourceLease {
        session_nonce,
        snapshot,
        frozen_at_ms,
        freeze_fingerprint,
    } = lease;
    if session_nonce != store.session_nonce {
        return Err(admission_error(
            "prompt-source lease belongs to another project-store session",
        ));
    }
    if !snapshot.matches_store_prompt(&material.prompt_evidence) {
        return Err(admission_error(
            "verified inference prompt differs from its frozen source lease",
        ));
    }
    if frozen_prompt_source_fingerprint(&snapshot, frozen_at_ms) != freeze_fingerprint {
        return Err(admission_error(
            "verified inference prompt-source lease fingerprint does not replay",
        ));
    }
    material.prompt_freeze = Some(PromptFreezeEvidence {
        frozen_at_ms,
        fingerprint: freeze_fingerprint,
    });
    Ok(())
}

fn preflight_adoption_cases(material: &BatchAdoptionMaterial) -> Result<BlobId> {
    if material.outcomes.is_empty() || material.outcomes.len() > MAX_BASE_WRITER_BATCH_CASES {
        return Err(admission_error(
            "verified inference batch has an invalid case count",
        ));
    }
    let mut call_ids = BTreeSet::new();
    let mut completed_count = 0_usize;
    let mut runtime_model = None;
    for (expected_index, outcome) in material.outcomes.iter().enumerate() {
        let (input_index, call) = match outcome {
            CaseAdoptionMaterial::Completed(completed) => {
                completed_count += 1;
                validate_completed_adoption(
                    completed,
                    material.prompt_evidence.compiled_fingerprint,
                )?;
                (completed.input_index, &completed.call)
            }
            CaseAdoptionMaterial::Cancelled(cancelled) => {
                validate_cancelled_adoption(
                    cancelled,
                    material.prompt_evidence.compiled_fingerprint,
                )?;
                (cancelled.input_index, &cancelled.call)
            }
        };
        let identity = call.identity();
        if identity.scope() != material.prompt_evidence.scope
            || identity.tokenizer_fingerprint() != material.binding.tokenizer_sha256
        {
            return Err(admission_error(
                "verified call differs from the frozen prompt scope or binding tokenizer",
            ));
        }
        if runtime_model.is_some_and(|expected| identity.model_fingerprint() != expected) {
            return Err(admission_error(
                "verified batch contains more than one runtime model fingerprint",
            ));
        }
        runtime_model.get_or_insert(identity.model_fingerprint());
        if input_index != expected_index || !call_ids.insert(call.id().to_string()) {
            return Err(admission_error(
                "verified inference cases are not contiguous and uniquely identified",
            ));
        }
    }
    if (material.kind == BatchAdoptionKind::Admitted && completed_count == 0)
        || (material.kind == BatchAdoptionKind::DiagnosticOnly && completed_count != 0)
    {
        return Err(admission_error(
            "verified inference outcome class disagrees with its completed call count",
        ));
    }
    runtime_model.ok_or_else(|| admission_error("verified batch has no runtime model"))
}

fn stage_common_batch_blobs(
    store: &ProjectStore,
    material: &BatchAdoptionMaterial,
) -> Result<StagedBatchBlobs> {
    let prompt = &material.prompt_evidence;
    let prompt_token_bytes = encode_token_ids(&prompt.ordered_token_ids);
    let binding_capabilities_bytes = serde_json::to_vec(&material.binding.capabilities)?;
    let blobs = StagedBatchBlobs {
        binding: StoredBindingBlobs {
            source: store.put_blob(&material.binding.manifest_source_bytes)?,
            canonical: store.put_blob(&material.binding.manifest_canonical_bytes)?,
            capabilities: store.put_blob(&binding_capabilities_bytes)?,
            capabilities_byte_len: binding_capabilities_bytes.len(),
        },
        binding_capabilities_bytes,
        prompt_specification: store.put_blob(&prompt.specification_bytes)?,
        prompt: store.put_blob(&prompt.raw_utf8)?,
        prompt_tokens: store.put_blob(&prompt_token_bytes)?,
        prompt_token_bytes,
        source_tail_bytes: store.read_blob(prompt.source_tail_blob_id)?,
    };
    if blobs.prompt != prompt.raw_blob_id
        || blobs.prompt_specification != prompt.specification_blob_id
    {
        return Err(admission_error(
            "stored prompt bytes differ from their exact prompt evidence",
        ));
    }
    validate_staged_prompt_tail(prompt, &blobs.source_tail_bytes)?;
    Ok(blobs)
}

fn validate_staged_prompt_tail(prompt: &StorePromptEvidence, source_bytes: &[u8]) -> Result<()> {
    validate_exact_prompt_tail(
        &prompt.raw_utf8,
        prompt.tail_prompt_range,
        source_bytes,
        prompt.source_tail_range,
    )
}

fn validate_exact_prompt_tail(
    prompt_bytes: &[u8],
    prompt_range: NonEmptyByteRange,
    source_bytes: &[u8],
    source_range: NonEmptyByteRange,
) -> Result<()> {
    if source_range.end() != source_bytes.len() as u64 {
        return Err(admission_error(
            "frozen prompt source tail does not end at its exact source EOF",
        ));
    }
    let source_tail = source_range
        .as_range()
        .checked_slice(source_bytes)
        .map_err(|error| admission_error(format!("invalid source tail range: {error}")))?;
    let prompt_tail = prompt_range
        .as_range()
        .checked_slice(prompt_bytes)
        .map_err(|error| admission_error(format!("invalid compiled tail range: {error}")))?;
    if source_tail != prompt_tail {
        return Err(admission_error(
            "compiled prompt tail differs from the exact immutable source tail",
        ));
    }
    Ok(())
}

fn stage_case_adoptions(
    store: &ProjectStore,
    outcomes: Vec<CaseAdoptionMaterial>,
) -> Result<Vec<StoredCaseAdoption>> {
    outcomes
        .into_iter()
        .map(|outcome| match outcome {
            CaseAdoptionMaterial::Completed(material) => {
                let blobs = store.store_case_blobs(
                    &material.call,
                    &material.raw_output,
                    &material.generated_token_ids,
                    &material.event_json,
                    &material.backend_audit_json,
                    material
                        .output_projection
                        .as_ref()
                        .map(|_| material.displayed_output.as_slice()),
                )?;
                Ok(StoredCaseAdoption::Completed { material, blobs })
            }
            CaseAdoptionMaterial::Cancelled(material) => {
                let blobs = store.store_case_blobs(
                    &material.call,
                    &material.partial_raw_output,
                    &material.generated_token_ids,
                    &material.event_json,
                    &material.backend_audit_json,
                    None,
                )?;
                Ok(StoredCaseAdoption::Cancelled { material, blobs })
            }
        })
        .collect()
}

fn stage_batch_adoption(
    store: &ProjectStore,
    material: &mut BatchAdoptionMaterial,
) -> Result<StagedBatchAdoption> {
    let blobs = stage_common_batch_blobs(store, material)?;
    let cases = stage_case_adoptions(store, std::mem::take(&mut material.outcomes))?;
    let completed_count = cases
        .iter()
        .filter(|case| matches!(case, StoredCaseAdoption::Completed { .. }))
        .count();
    let cancelled_count = cases.len() - completed_count;
    if (material.kind == BatchAdoptionKind::Admitted && completed_count == 0)
        || (material.kind == BatchAdoptionKind::DiagnosticOnly && completed_count != 0)
    {
        return Err(admission_error(
            "verified inference outcome class changed before persistence",
        ));
    }
    Ok(StagedBatchAdoption {
        blobs,
        cases,
        completed_count,
        cancelled_count,
    })
}

const INSERT_VERIFIED_BATCH_SQL: &str = "INSERT INTO research_verified_inference_batches(
        batch_verification_fingerprint, project_id,
        model_binding_fingerprint, model_binding_source_hash,
        runtime_model_fingerprint,
        prompt_specification_blob_id, prompt_specification_byte_len,
        source_prompt_fingerprint, prompt_content_fingerprint,
        treatment_recipe_fingerprint,
        prompt_source_count, prompt_freeze_fingerprint, prompt_frozen_at_ms,
        prompt_campaign_id, prompt_stage_id,
        prompt_stage_attempt_id, prompt_trial_case_id,
        tail_prompt_start_byte, tail_prompt_end_byte,
        source_tail_revision_id, source_tail_blob_id,
        source_tail_start_byte, source_tail_end_byte,
        source_tail_origin, source_tail_assembly_id,
        native_request_id,
        exact_prompt_blob_id, exact_prompt_byte_len,
        prompt_form, prompt_token_policy,
        prompt_token_ids_blob_id, prompt_token_count,
        prompt_token_ids_fingerprint, compiled_prompt_fingerprint,
        expected_case_count, created_at_ms
     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
               ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
               ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30,
               ?31, ?32, ?33, ?34, ?35, ?36)";

fn register_staged_blob_rows(
    transaction: &Transaction<'_>,
    material: &BatchAdoptionMaterial,
    staged: &StagedBatchAdoption,
    created_at_ms: i64,
) -> Result<()> {
    let prompt = &material.prompt_evidence;
    for (blob_id, byte_len) in [
        (
            staged.blobs.binding.source,
            material.binding.manifest_source_bytes.len(),
        ),
        (
            staged.blobs.binding.canonical,
            material.binding.manifest_canonical_bytes.len(),
        ),
        (
            staged.blobs.binding.capabilities,
            staged.blobs.binding_capabilities_bytes.len(),
        ),
        (staged.blobs.prompt, prompt.raw_utf8.len()),
        (
            staged.blobs.prompt_specification,
            prompt.specification_bytes.len(),
        ),
        (
            prompt.source_tail_blob_id,
            staged.blobs.source_tail_bytes.len(),
        ),
        (
            staged.blobs.prompt_tokens,
            staged.blobs.prompt_token_bytes.len(),
        ),
    ] {
        insert_blob_row(transaction, blob_id, byte_len, created_at_ms)?;
    }
    for case in &staged.cases {
        let blobs = match case {
            StoredCaseAdoption::Completed { blobs, .. }
            | StoredCaseAdoption::Cancelled { blobs, .. } => blobs,
        };
        for (blob_id, byte_len) in [
            (blobs.call_record_blob_id, blobs.call_record_byte_len),
            (blobs.raw_output_blob_id, blobs.raw_output_byte_len),
            (blobs.token_ids_blob_id, blobs.token_ids_byte_len),
            (blobs.event_blob_id, blobs.event_byte_len),
            (blobs.receipt_blob_id, blobs.receipt_byte_len),
        ] {
            insert_blob_row(transaction, blob_id, byte_len, created_at_ms)?;
        }
        if let Some((blob_id, byte_len)) = blobs.displayed_output {
            insert_blob_row(transaction, blob_id, byte_len, created_at_ms)?;
        }
    }
    Ok(())
}

fn insert_verified_batch_header(
    transaction: &Transaction<'_>,
    material: &BatchAdoptionMaterial,
    staged: &StagedBatchAdoption,
    runtime_model_fingerprint: BlobId,
    created_at_ms: i64,
) -> Result<()> {
    let prompt = &material.prompt_evidence;
    let freeze = material.prompt_freeze.ok_or_else(|| {
        admission_error("verified inference batch has no prompt-source freeze evidence")
    })?;
    let scope = prompt.scope;
    let (tail_origin, assembly_id) = match &prompt.source_tail_origin {
        StoreTailOrigin::LiveManuscript => ("live_manuscript", None),
        StoreTailOrigin::AdmittedAssembly(assembly_id) => {
            ("admitted_assembly", Some(assembly_id.as_str()))
        }
    };
    transaction.execute(
        INSERT_VERIFIED_BATCH_SQL,
        params![
            material.verification_fingerprint.to_string(),
            material.project_id.to_string(),
            material.binding.binding_fingerprint.to_string(),
            material.binding.manifest_source_hash.to_string(),
            runtime_model_fingerprint.to_string(),
            staged.blobs.prompt_specification.to_string(),
            checked_sql_usize(
                prompt.specification_bytes.len(),
                "prompt specification length"
            )?,
            prompt.source_prompt_fingerprint.to_string(),
            prompt.content_fingerprint.to_string(),
            prompt.treatment_recipe_fingerprint.to_string(),
            checked_sql_usize(prompt.sources.len(), "prompt source count")?,
            freeze.fingerprint.to_string(),
            freeze.frozen_at_ms,
            scope.campaign_id().to_string(),
            scope.stage_id().to_string(),
            scope.attempt_id().to_string(),
            scope.case_id().to_string(),
            checked_sql_u64(prompt.tail_prompt_range.start(), "compiled tail start")?,
            checked_sql_u64(prompt.tail_prompt_range.end(), "compiled tail end")?,
            prompt.source_tail_revision_id.as_deref(),
            prompt.source_tail_blob_id.to_string(),
            checked_sql_u64(prompt.source_tail_range.start(), "source tail start")?,
            checked_sql_u64(prompt.source_tail_range.end(), "source tail end")?,
            tail_origin,
            assembly_id,
            material.backend_request_id.as_str(),
            staged.blobs.prompt.to_string(),
            checked_sql_usize(prompt.raw_utf8.len(), "exact prompt length")?,
            prompt_form_sql(prompt.form),
            prompt_token_policy_sql(prompt.token_policy),
            staged.blobs.prompt_tokens.to_string(),
            checked_sql_usize(prompt.ordered_token_ids.len(), "exact prompt token count")?,
            prompt.token_fingerprint.to_string(),
            prompt.compiled_fingerprint.to_string(),
            checked_sql_usize(staged.cases.len(), "verified case count")?,
            created_at_ms,
        ],
    )?;
    insert_prompt_sources(transaction, material.verification_fingerprint, prompt)
}

fn persist_staged_cases(
    transaction: &Transaction<'_>,
    cases: Vec<StoredCaseAdoption>,
    batch_fingerprint: BlobId,
    session_nonce: StoreSessionNonce,
    created_at_ms: i64,
    completed_count: usize,
) -> Result<Vec<AdmittedModelCall>> {
    let mut admitted = Vec::with_capacity(completed_count);
    for case in cases {
        match case {
            StoredCaseAdoption::Completed { material, blobs } => {
                insert_completed_adoption(
                    transaction,
                    &material,
                    &blobs,
                    batch_fingerprint,
                    created_at_ms,
                )?;
                admitted.push(AdmittedModelCall {
                    session_nonce,
                    call: material.call,
                    raw_output: material.raw_output,
                    token_ids: material.generated_token_ids,
                    token_byte_boundaries: None,
                    verification_fingerprint: material.verification_fingerprint,
                });
            }
            StoredCaseAdoption::Cancelled { material, blobs } => insert_cancelled_adoption(
                transaction,
                &material,
                &blobs,
                batch_fingerprint,
                created_at_ms,
            )?,
        }
    }
    Ok(admitted)
}

fn persist_batch_adoption(
    store: &mut ProjectStore,
    material: &BatchAdoptionMaterial,
    runtime_model_fingerprint: BlobId,
    staged: StagedBatchAdoption,
) -> Result<AdoptedInferenceBatch> {
    let created_at_ms = now_unix_ms().max(1);
    let session_nonce = store.session_nonce;
    let transaction = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    register_staged_blob_rows(&transaction, material, &staged, created_at_ms)?;
    insert_model_binding(
        &transaction,
        &material.binding,
        &staged.blobs.binding,
        created_at_ms,
    )?;
    insert_verified_batch_header(
        &transaction,
        material,
        &staged,
        runtime_model_fingerprint,
        created_at_ms,
    )?;
    let admitted_calls = persist_staged_cases(
        &transaction,
        staged.cases,
        material.verification_fingerprint,
        session_nonce,
        created_at_ms,
        staged.completed_count,
    )?;
    transaction.execute(
        "INSERT INTO research_verified_inference_batch_seals(
            batch_verification_fingerprint, completed_call_count,
            cancelled_call_count, sealed_at_ms
         ) VALUES (?1, ?2, ?3, ?4)",
        params![
            material.verification_fingerprint.to_string(),
            checked_sql_usize(staged.completed_count, "completed call count")?,
            checked_sql_usize(staged.cancelled_count, "cancelled call count")?,
            created_at_ms,
        ],
    )?;
    transaction.commit()?;
    Ok(AdoptedInferenceBatch {
        session_nonce,
        verification_fingerprint: material.verification_fingerprint,
        admitted_calls,
        cancelled_call_count: staged.cancelled_count,
    })
}

fn persist_candidate_assembly_rows(
    store: &mut ProjectStore,
    record: &CandidateAssemblyRecord,
    assembled_bytes: &[u8],
    admission_record_id: ResearchAdmissionRecordId,
) -> Result<()> {
    let record_bytes = serde_json::to_vec(record)?;
    let record_blob_id = store.put_blob(&record_bytes)?;
    let assembled_blob_id = store.put_blob(assembled_bytes)?;
    let graph_bytes = serde_json::to_vec(record.operation_graph())?;
    let graph_blob_id = store.put_blob(&graph_bytes)?;
    let created_at_ms = now_unix_ms().max(1);
    let transaction = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    for (blob_id, byte_len) in [
        (record_blob_id, record_bytes.len()),
        (assembled_blob_id, assembled_bytes.len()),
        (graph_blob_id, graph_bytes.len()),
    ] {
        insert_blob_row(&transaction, blob_id, byte_len, created_at_ms)?;
    }
    insert_operation_graph(
        &transaction,
        record.operation_graph(),
        graph_blob_id,
        created_at_ms,
    )?;
    transaction.execute(
        "INSERT INTO research_candidate_assemblies(
            assembly_id, graph_fingerprint, part_count,
            part_order_fingerprint, assembled_blob_id, assembled_byte_len,
            assembly_record_blob_id, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            record.id().to_string(),
            record.operation_graph().fingerprint().to_string(),
            checked_sql_usize(record.parts().len(), "assembly part count")?,
            record.witness().part_order_fingerprint().to_string(),
            assembled_blob_id.to_string(),
            checked_sql_u64(record.witness().assembled_byte_len(), "assembly length")?,
            record_blob_id.to_string(),
            created_at_ms,
        ],
    )?;
    for (position, part) in record.parts().iter().enumerate() {
        transaction.execute(
            "INSERT INTO research_candidate_assembly_parts(
                assembly_id, position, join_before, occurrence_id
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                record.id().to_string(),
                checked_sql_usize(position, "assembly part position")?,
                join_before_name(part.join_before()),
                part.span().id().to_string(),
            ],
        )?;
    }
    transaction.execute(
        "INSERT INTO research_admission_records(
            admission_record_id, subject_kind, subject_id, admitted_at_ms
         ) VALUES (?1, 'candidate_assembly', ?2, ?3)",
        params![
            admission_record_id.as_blob_id().to_string(),
            record.id().to_string(),
            created_at_ms,
        ],
    )?;
    transaction.commit()?;
    Ok(())
}

impl ProjectStore {
    fn require_research_session(&self, nonce: StoreSessionNonce) -> Result<()> {
        if nonce != self.session_nonce {
            return Err(admission_error(
                "opaque research capability belongs to another project-store session",
            ));
        }
        Ok(())
    }

    /// Freeze an inference-ready prompt against the exact current project
    /// sources. The returned one-use capability must cross the backend call
    /// boundary and is consumed when its verified outcome is adopted.
    pub fn verify_compiled_prompt_for_inference(
        &self,
        prompt: &CompiledBaseCompletionPrompt,
    ) -> Result<FrozenPromptSourceLease> {
        let snapshot = FrozenPromptSourceSnapshot::from_compiled(prompt)?;
        validate_frozen_prompt_source_snapshot(self, &snapshot, true)?;
        let frozen_at_ms = now_unix_ms().max(1);
        let freeze_fingerprint = frozen_prompt_source_fingerprint(&snapshot, frozen_at_ms);
        Ok(FrozenPromptSourceLease {
            session_nonce: self.session_nonce,
            snapshot,
            frozen_at_ms,
            freeze_fingerprint,
        })
    }

    /// Consume one live backend authority and persist its complete ordered
    /// evidence in a single semantic transaction.
    ///
    /// This is the only production entry point that mints
    /// [`AdmittedModelCall`] leases. Serializable calls, receipts, database
    /// rows, fixture text, and historical evidence cannot invoke it.
    pub fn adopt_verified_inference(
        &mut self,
        outcome: VerifiedInferenceOutcome,
        lease: FrozenPromptSourceLease,
    ) -> Result<AdoptedInferenceBatch> {
        let mut material = BatchAdoptionMaterial::from_outcome(outcome)?;
        consume_prompt_source_lease(self, &mut material, lease)?;
        self.adopt_verified_material_with_freeze(material)
    }

    /// Reconstructs a sealed batch as a closed native receipt graph. This
    /// checks exact byte and commitment coherence but does not independently
    /// prove that inference happened, establish campaign/stage/treatment
    /// existence, or grant benchmark eligibility.
    pub fn replay_inference_batch_evidence(
        &self,
        batch_fingerprint: BlobId,
    ) -> Result<ReplayedInferenceBatchEvidence> {
        let batch = load_stored_batch(self, batch_fingerprint)?;
        validate_stored_batch_header(self, &batch)?;
        let binding =
            load_stored_binding(self, batch.binding_fingerprint, batch.binding_source_hash)?;
        let prompt = load_replayed_prompt_evidence(self, batch_fingerprint, &batch)?;
        let replayed =
            replay_stored_batch_cases(self, batch_fingerprint, &batch, &binding.evidence)?;
        verify_native_batch_replay(
            &binding.evidence,
            &prompt,
            &batch,
            &replayed,
            batch_fingerprint,
        )?;
        Ok(ReplayedInferenceBatchEvidence {
            batch_fingerprint,
            binding_fingerprint: binding.evidence.binding_fingerprint,
            manifest_artifact_hash: binding.evidence.manifest_artifact_hash,
            prompt_content_fingerprint: batch.prompt_content_fingerprint,
            compiled_prompt_fingerprint: batch.compiled_prompt_fingerprint,
            completed_call_count: replayed.completed,
            cancelled_call_count: replayed.cancelled,
        })
    }

    fn adopt_verified_material_with_freeze(
        &mut self,
        material: BatchAdoptionMaterial,
    ) -> Result<AdoptedInferenceBatch> {
        let runtime_model_fingerprint = preflight_batch_adoption(self, &material)?;
        let completed = material
            .outcomes
            .iter()
            .filter(|case| matches!(case, CaseAdoptionMaterial::Completed(_)))
            .count();
        verify_native_material(NativeMaterialReplay {
            binding: &material.binding,
            prompt: &material.prompt_evidence,
            backend_request_id: &material.backend_request_id,
            cases: &material.outcomes,
            runtime_model_fingerprint,
            batch_fingerprint: material.verification_fingerprint,
            expected_completed: completed,
            expected_cancelled: material.outcomes.len() - completed,
        })?;
        self.persist_preflighted_adoption(material, runtime_model_fingerprint)
    }

    fn persist_preflighted_adoption(
        &mut self,
        mut material: BatchAdoptionMaterial,
        runtime_model_fingerprint: BlobId,
    ) -> Result<AdoptedInferenceBatch> {
        let staged = stage_batch_adoption(self, &mut material)?;
        persist_batch_adoption(self, &material, runtime_model_fingerprint, staged)
    }

    #[cfg(test)]
    fn preflight_untrusted_test_material(
        &self,
        mut material: BatchAdoptionMaterial,
    ) -> Result<BlobId> {
        let snapshot = snapshot_from_store_prompt(&material.prompt_evidence);
        validate_frozen_prompt_source_snapshot(self, &snapshot, true)?;
        let frozen_at_ms = now_unix_ms().max(1);
        material.prompt_freeze = Some(PromptFreezeEvidence {
            frozen_at_ms,
            fingerprint: frozen_prompt_source_fingerprint(&snapshot, frozen_at_ms),
        });
        preflight_batch_adoption(self, &material)
    }

    fn store_case_blobs(
        &self,
        call: &ModelCall,
        raw_output: &[u8],
        token_ids: &[u32],
        event_json: &[u8],
        receipt_json: &[u8],
        displayed_output: Option<&[u8]>,
    ) -> Result<StoredCaseBlobs> {
        let call_record = serde_json::to_vec(call)?;
        let token_bytes = encode_token_ids(token_ids);
        let displayed_output = match displayed_output {
            Some(bytes) => Some((self.put_blob(bytes)?, bytes.len())),
            None => None,
        };
        Ok(StoredCaseBlobs {
            call_record_blob_id: self.put_blob(&call_record)?,
            call_record_byte_len: call_record.len(),
            raw_output_blob_id: self.put_blob(raw_output)?,
            raw_output_byte_len: raw_output.len(),
            token_ids_blob_id: self.put_blob(&token_bytes)?,
            token_ids_byte_len: token_bytes.len(),
            event_blob_id: self.put_blob(event_json)?,
            event_byte_len: event_json.len(),
            receipt_blob_id: self.put_blob(receipt_json)?,
            receipt_byte_len: receipt_json.len(),
            displayed_output,
        })
    }

    /// Temporary internal replay plumbing for the verifier integration tests.
    ///
    /// This is deliberately crate-private: coherent caller-authored JSON is
    /// not proof of inference. Production admission will route an opaque
    /// native-engine completion seal through `loom-inference` before this
    /// persistence path becomes reachable outside `loom-store`.
    #[cfg(test)]
    fn verify_and_record_base_writer_call(
        &mut self,
        call: ModelCall,
        raw_output: Vec<u8>,
        token_ids: Vec<u32>,
        raw_event_stream: &[u8],
        backend_receipt: &[u8],
    ) -> Result<AdmittedModelCall> {
        let replay = replay_base_writer_call(
            &call,
            &raw_output,
            &token_ids,
            raw_event_stream,
            backend_receipt,
        )?;
        let call_record = serde_json::to_vec(&call)?;
        let call_record_blob_id = self.put_blob(&call_record)?;
        let raw_output_blob_id = self.put_blob(&raw_output)?;
        let token_bytes = encode_token_ids(&token_ids);
        let token_ids_blob_id = self.put_blob(&token_bytes)?;
        let raw_event_stream_blob_id = self.put_blob(raw_event_stream)?;
        let backend_receipt_blob_id = self.put_blob(backend_receipt)?;
        let completed = call.completed()?;
        let created_at_ms = now_unix_ms().max(1);

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (blob_id, byte_len) in [
            (call_record_blob_id, call_record.len()),
            (raw_output_blob_id, raw_output.len()),
            (token_ids_blob_id, token_bytes.len()),
            (raw_event_stream_blob_id, raw_event_stream.len()),
            (backend_receipt_blob_id, backend_receipt.len()),
        ] {
            insert_blob_row(&transaction, blob_id, byte_len, created_at_ms)?;
        }
        let identity = call.identity();
        transaction.execute(
            "INSERT INTO research_model_calls(
                call_id, campaign_id, stage_id, stage_attempt_id, trial_case_id,
                seed_decimal, model_fingerprint, tokenizer_fingerprint, prompt_fingerprint,
                sampler_fingerprint, control_program_fingerprint, evidence_class,
                verification_audit_fingerprint, call_record_blob_id, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                       'live_base_writer_claim', ?12, ?13, ?14)",
            params![
                call.id().to_string(),
                identity.scope().campaign_id().to_string(),
                identity.scope().stage_id().to_string(),
                identity.scope().attempt_id().to_string(),
                identity.scope().case_id().to_string(),
                identity.seed().to_string(),
                identity.model_fingerprint().to_string(),
                identity.tokenizer_fingerprint().to_string(),
                identity.prompt_fingerprint().to_string(),
                identity.sampler_fingerprint().to_string(),
                identity.control_program_fingerprint().to_string(),
                replay.verification_fingerprint.to_string(),
                call_record_blob_id.to_string(),
                created_at_ms,
            ],
        )?;
        transaction.execute(
            "INSERT INTO research_call_terminals(
                call_id, status, raw_output_blob_id, raw_output_byte_len,
                token_ids_blob_id, token_count, token_ids_fingerprint,
                raw_event_stream_blob_id, backend_receipt_blob_id,
                terminal_message, created_at_ms
             ) VALUES (?1, 'completed', ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9)",
            params![
                call.id().to_string(),
                raw_output_blob_id.to_string(),
                checked_sql_u64(completed.raw_output_byte_len(), "raw output length")?,
                token_ids_blob_id.to_string(),
                i64::from(completed.token_evidence().token_count()),
                completed
                    .token_evidence()
                    .token_ids_fingerprint()
                    .to_string(),
                raw_event_stream_blob_id.to_string(),
                backend_receipt_blob_id.to_string(),
                created_at_ms,
            ],
        )?;
        transaction.commit()?;

        Ok(AdmittedModelCall {
            session_nonce: self.session_nonce,
            call,
            raw_output,
            token_ids,
            token_byte_boundaries: replay.token_byte_boundaries,
            verification_fingerprint: replay.verification_fingerprint,
        })
    }

    /// Verifies and persists a non-empty occurrence from one currently
    /// admitted call. Persisted call/span records alone cannot invoke this API.
    pub fn verify_and_record_generated_span(
        &mut self,
        admitted_call: &AdmittedModelCall,
        record: GeneratedSpanOccurrenceRecord,
    ) -> Result<AdmittedGeneratedSpan> {
        self.require_research_session(admitted_call.session_nonce)?;
        if record.call_id() != admitted_call.call.id() || !record.has_live_base_writer_claim() {
            return Err(admission_error(
                "span is not a live base-writer claim for the admitted call",
            ));
        }
        let exact = ExactCallEvidence::new(
            &admitted_call.call,
            &admitted_call.raw_output,
            &admitted_call.token_ids,
        );
        record.verify_exact(&exact)?;
        verify_declared_token_mapping(&record, admitted_call)?;

        let record_bytes = serde_json::to_vec(&record)?;
        let record_blob_id = self.put_blob(&record_bytes)?;
        let created_at_ms = now_unix_ms().max(1);
        let projection = record.projection();
        let token_range = record.token_range();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_blob_row(
            &transaction,
            record_blob_id,
            record_bytes.len(),
            created_at_ms,
        )?;
        transaction.execute(
            "INSERT INTO research_output_projections(
                occurrence_id, call_id, raw_output_byte_len,
                displayed_start_byte, displayed_end_byte,
                endpoint_tail_start_byte, endpoint_tail_end_byte,
                stop_suffix_start_byte, stop_suffix_end_byte, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                record.id().to_string(),
                record.call_id().to_string(),
                checked_sql_u64(projection.raw_output_byte_len(), "projection output length")?,
                checked_sql_u64(projection.displayed().start(), "display start")?,
                checked_sql_u64(projection.displayed().end(), "display end")?,
                checked_sql_u64(
                    projection.endpoint_excluded_tail().start(),
                    "endpoint start"
                )?,
                checked_sql_u64(projection.endpoint_excluded_tail().end(), "endpoint end")?,
                checked_sql_u64(projection.trimmed_stop_suffix().start(), "stop start")?,
                checked_sql_u64(projection.trimmed_stop_suffix().end(), "stop end")?,
                created_at_ms,
            ],
        )?;
        transaction.execute(
            "INSERT INTO research_generated_span_occurrences(
                occurrence_id, call_id, raw_output_blob_id,
                output_start_byte, output_end_byte, token_start, token_end,
                evidence_class, extraction_receipt_fingerprint,
                verification_audit_fingerprint, span_record_blob_id, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7,
                       'live_base_writer_claim', ?8, ?9, ?10, ?11)",
            params![
                record.id().to_string(),
                record.call_id().to_string(),
                record.raw_output_blob_id().to_string(),
                checked_sql_u64(record.output_byte_range().start(), "span start")?,
                checked_sql_u64(record.output_byte_range().end(), "span end")?,
                token_range.map(|range| i64::from(range.start())),
                token_range.map(|range| i64::from(range.end())),
                record.extraction_receipt().fingerprint().to_string(),
                admitted_call.verification_fingerprint.to_string(),
                record_blob_id.to_string(),
                created_at_ms,
            ],
        )?;
        transaction.commit()?;

        Ok(AdmittedGeneratedSpan {
            session_nonce: self.session_nonce,
            record,
            call: admitted_call.call.clone(),
            raw_output: admitted_call.raw_output.clone(),
            token_ids: admitted_call.token_ids.clone(),
        })
    }

    /// Reconstructs every part from admitted exact calls, rehashes the graph
    /// and witness, then inserts the final admission row last.
    pub fn verify_and_record_candidate_assembly(
        &mut self,
        record: CandidateAssemblyRecord,
        admitted_spans: &[&AdmittedGeneratedSpan],
    ) -> Result<AdmittedCandidateAssembly> {
        if admitted_spans
            .iter()
            .any(|span| span.session_nonce != self.session_nonce)
        {
            return Err(admission_error(
                "assembly contains a span lease from another project-store session",
            ));
        }
        if record.declared_pipeline_eligibility() != PipelineEligibility::DeclaredBaseWriterOnly {
            return Err(admission_error(
                "assembly graph contains non-base-writer text",
            ));
        }
        let by_id = admitted_spans
            .iter()
            .map(|span| (span.record.id(), *span))
            .collect::<BTreeMap<_, _>>();
        if by_id.len() != admitted_spans.len() || by_id.len() != record.parts().len() {
            return Err(admission_error(
                "admitted span leases do not exactly cover assembly parts",
            ));
        }
        let mut exact_calls = Vec::with_capacity(record.parts().len());
        for part in record.parts() {
            let admitted = by_id.get(&part.span().id()).ok_or_else(|| {
                admission_error("assembly part has no matching admitted span lease")
            })?;
            if admitted.record != *part.span() {
                return Err(admission_error(
                    "assembly part differs from its admitted span record",
                ));
            }
            exact_calls.push(OwnedExactCall {
                call: admitted.call.clone(),
                raw_output: admitted.raw_output.clone(),
                token_ids: admitted.token_ids.clone(),
            });
        }
        let exact = exact_evidence(&exact_calls);
        let assembled_bytes = record.reconstruct(&exact)?;
        let admission_record_id =
            admission_record_id("candidate_assembly", &record.id().to_string());
        persist_candidate_assembly_rows(self, &record, &assembled_bytes, admission_record_id)?;

        Ok(AdmittedCandidateAssembly {
            session_nonce: self.session_nonce,
            admission_record_id,
            record,
            exact_calls,
        })
    }

    /// Pins a verified assembly to an exact source revision/range and inserts
    /// the projection admission only after replaying its resulting bytes.
    pub fn verify_and_record_candidate_projection(
        &mut self,
        admitted_assembly: &AdmittedCandidateAssembly,
        record: CandidateProjectionRecord,
    ) -> Result<AdmittedCandidateProjection> {
        self.require_research_session(admitted_assembly.session_nonce)?;
        if record.assembly_id() != admitted_assembly.record.id() {
            return Err(admission_error("projection names a different assembly"));
        }
        if !revision_blob_is_current(
            self,
            &record.source_revision_id().to_string(),
            record.source_blob_id(),
        )? {
            return Err(admission_error(
                "candidate projection source revision is no longer current",
            ));
        }
        let source_bytes = self.read_blob(record.source_blob_id())?;
        let exact = exact_evidence(&admitted_assembly.exact_calls);
        let resulting = record.apply(&admitted_assembly.record, &source_bytes, &exact)?;
        let admission_record_id =
            admission_record_id("candidate_projection", &record.id().to_string());

        let record_bytes = serde_json::to_vec(&record)?;
        let record_blob_id = self.put_blob(&record_bytes)?;
        let resulting_blob_id = self.put_blob(&resulting)?;
        let graph_bytes = serde_json::to_vec(record.operation_graph())?;
        let graph_blob_id = self.put_blob(&graph_bytes)?;
        let created_at_ms = now_unix_ms().max(1);
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (blob_id, byte_len) in [
            (record_blob_id, record_bytes.len()),
            (resulting_blob_id, resulting.len()),
            (graph_blob_id, graph_bytes.len()),
        ] {
            insert_blob_row(&transaction, blob_id, byte_len, created_at_ms)?;
        }
        insert_operation_graph(
            &transaction,
            record.operation_graph(),
            graph_blob_id,
            created_at_ms,
        )?;
        let range = record.target_range();
        transaction.execute(
            "INSERT INTO research_candidate_projections(
                projection_id, assembly_id, source_revision_id, source_blob_id,
                target_start_byte, target_end_byte, graph_fingerprint,
                assembly_blob_id, resulting_blob_id, resulting_byte_len,
                projection_record_blob_id, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                record.id().to_string(),
                record.assembly_id().to_string(),
                record.source_revision_id().to_string(),
                record.source_blob_id().to_string(),
                checked_sql_u64(range.start(), "projection range start")?,
                checked_sql_u64(range.end(), "projection range end")?,
                record.operation_graph().fingerprint().to_string(),
                record.witness().assembly_blob_id().to_string(),
                resulting_blob_id.to_string(),
                checked_sql_u64(record.witness().resulting_byte_len(), "projection length")?,
                record_blob_id.to_string(),
                created_at_ms,
            ],
        )?;
        transaction.execute(
            "INSERT INTO research_admission_records(
                admission_record_id, subject_kind, subject_id, admitted_at_ms
             ) VALUES (?1, 'candidate_projection', ?2, ?3)",
            params![
                admission_record_id.as_blob_id().to_string(),
                record.id().to_string(),
                created_at_ms,
            ],
        )?;
        transaction.commit()?;
        Ok(AdmittedCandidateProjection {
            session_nonce: self.session_nonce,
            admission_record_id,
            record,
        })
    }

    /// Persists an explicit mixed-authorship lane. It is inspectable and may
    /// later be promoted with authority, but is never base-writer evidence.
    pub fn record_mixed_authorship_assembly(
        &mut self,
        record: MixedAuthorshipAssemblyRecord,
        exact_output: &[u8],
    ) -> Result<MixedAuthorshipAdmission> {
        record.verify_output(exact_output)?;
        if record.declared_pipeline_eligibility() == PipelineEligibility::DeclaredBaseWriterOnly {
            return Err(admission_error(
                "mixed-authorship record has no text-affecting mixed operation",
            ));
        }
        let admission_record_id = admission_record_id("mixed_authorship", &record.id().to_string());
        let record_bytes = serde_json::to_vec(&record)?;
        let record_blob_id = self.put_blob(&record_bytes)?;
        let output_blob_id = self.put_blob(exact_output)?;
        let graph_bytes = serde_json::to_vec(record.operation_graph())?;
        let graph_blob_id = self.put_blob(&graph_bytes)?;
        let created_at_ms = now_unix_ms().max(1);
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (blob_id, byte_len) in [
            (record_blob_id, record_bytes.len()),
            (output_blob_id, exact_output.len()),
            (graph_blob_id, graph_bytes.len()),
        ] {
            insert_blob_row(&transaction, blob_id, byte_len, created_at_ms)?;
        }
        insert_operation_graph(
            &transaction,
            record.operation_graph(),
            graph_blob_id,
            created_at_ms,
        )?;
        transaction.execute(
            "INSERT INTO research_mixed_authorship_assemblies(
                mixed_assembly_id, output_blob_id, output_byte_len,
                graph_fingerprint, mixed_record_blob_id, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                record.id().to_string(),
                output_blob_id.to_string(),
                checked_sql_u64(record.output_byte_len(), "mixed output length")?,
                record.operation_graph().fingerprint().to_string(),
                record_blob_id.to_string(),
                created_at_ms,
            ],
        )?;
        transaction.execute(
            "INSERT INTO research_admission_records(
                admission_record_id, subject_kind, subject_id, admitted_at_ms
             ) VALUES (?1, 'mixed_authorship', ?2, ?3)",
            params![
                admission_record_id.as_blob_id().to_string(),
                record.id().to_string(),
                created_at_ms,
            ],
        )?;
        transaction.commit()?;
        Ok(MixedAuthorshipAdmission {
            session_nonce: self.session_nonce,
            admission_record_id,
            record,
        })
    }

    /// Persists the exact promotion command request before user confirmation.
    ///
    /// The returned marker is process-local and cannot be reconstructed from
    /// the row. The row is durable audit/recovery evidence only.
    pub fn record_promotion_command_request(
        &mut self,
        subject_lease: PromotionSubjectLease<'_>,
        request: &PromotionCommandRequest,
    ) -> Result<RecordedPromotionRequest> {
        if request.project_id() != self.manifest.project_id
            || !promotion_subject_lease_matches(self.session_nonce, subject_lease, request)
        {
            return Err(admission_error(
                "promotion request differs from its exact runtime admission lease",
            ));
        }
        if !revision_blob_is_current(
            self,
            &request.source_revision_id().to_string(),
            request.source_blob_id(),
        )? {
            return Err(admission_error(
                "promotion request source revision is no longer current",
            ));
        }
        let subject = request.subject();
        let recorded_at_ms = now_unix_ms().max(request.command_requested_at_ms());
        let canonical_request_blob_id = self.put_blob(request.canonical_request_bytes())?;
        if canonical_request_blob_id != request.command_request_fingerprint() {
            return Err(admission_error(
                "promotion request digest differs from its canonical bytes",
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_blob_row(
            &transaction,
            canonical_request_blob_id,
            request.canonical_request_bytes().len(),
            recorded_at_ms,
        )?;
        let inserted = transaction.execute(
            "INSERT INTO research_promotion_command_requests(
                command_id, command_request_fingerprint,
                canonical_request_blob_id, canonical_request_byte_len, project_id,
                source_revision_id, source_blob_id,
                subject_kind, subject_id, admission_record_id,
                intended_result_blob_id, intended_result_byte_len,
                requested_at_ms, recorded_at_ms
             ) SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14
               WHERE NOT EXISTS (
                   SELECT 1 FROM command_receipts receipt WHERE receipt.command_id = ?1
               )",
            params![
                request.command_id().to_string(),
                request.command_request_fingerprint().to_string(),
                canonical_request_blob_id.to_string(),
                checked_sql_usize(
                    request.canonical_request_bytes().len(),
                    "canonical promotion request length",
                )?,
                request.project_id().to_string(),
                request.source_revision_id().to_string(),
                request.source_blob_id().to_string(),
                subject.kind_name(),
                subject.id_string(),
                request.admission_record_id().to_string(),
                request.intended_result_blob_id().to_string(),
                checked_sql_u64(
                    request.intended_result_byte_len(),
                    "intended promotion result length",
                )?,
                request.command_requested_at_ms(),
                recorded_at_ms,
            ],
        )?;
        if inserted != 1 {
            return Err(admission_error(
                "promotion request command already has a terminal receipt",
            ));
        }
        transaction.commit()?;
        Ok(RecordedPromotionRequest {
            session_nonce: self.session_nonce,
            command_id: request.command_id(),
            request_fingerprint: request.command_request_fingerprint(),
            recorded_at_ms,
        })
    }

    /// Durably records promotion intent before any manuscript mutation.
    ///
    /// A completed command receipt must not exist yet. The non-serializable
    /// host lease binds the exact command-request fingerprint to one foreground
    /// presence event; the authority additionally pins this project, source,
    /// admission record, typed subject, and intended result bytes. Applying
    /// this authority remains deliberately unsupported.
    // Passing these opaque tokens by value is intentional: one host presence
    // gesture and one recorded request may authorize at most one attempt.
    #[allow(clippy::needless_pass_by_value)]
    pub fn record_promotion_authority(
        &mut self,
        recorded_request: RecordedPromotionRequest,
        subject_lease: PromotionSubjectLease<'_>,
        lease: VerifiedUserPresence,
        authority: &PromotionAuthority,
    ) -> Result<RecordedPromotionAuthority> {
        let presence = authority.user_presence();
        if recorded_request.session_nonce != self.session_nonce
            || lease.session_nonce != self.session_nonce
            || authority.project_id() != self.manifest.project_id
            || authority.command_id() != recorded_request.command_id
            || authority.command_request_fingerprint() != recorded_request.request_fingerprint
            || authority.command_id() != lease.command_id
            || authority.command_request_fingerprint() != lease.command_request_fingerprint
            || presence.kind() != lease.kind
            || presence.session_fingerprint() != lease.session_fingerprint
            || presence.event_receipt_blob_id() != lease.event_receipt_blob_id
            || presence.monotonic_event_index() != lease.monotonic_event_index
            || presence.occurred_at_ms() != lease.occurred_at_ms
            || authority.actor() != &lease.actor
            || BlobId::digest(&lease.event_receipt_bytes) != lease.event_receipt_blob_id
        {
            return Err(admission_error(
                "promotion authority differs from its host-owned presence lease",
            ));
        }
        if !promotion_subject_lease_matches(self.session_nonce, subject_lease, authority.request())
        {
            return Err(admission_error(
                "promotion authority lacks the exact runtime admission lease",
            ));
        }
        if recorded_request.recorded_at_ms > lease.occurred_at_ms {
            return Err(admission_error(
                "promotion presence occurred before its durable command request",
            ));
        }

        self.persist_promotion_authority(&recorded_request, &lease, authority)
    }

    fn persist_promotion_authority(
        &mut self,
        recorded_request: &RecordedPromotionRequest,
        lease: &VerifiedUserPresence,
        authority: &PromotionAuthority,
    ) -> Result<RecordedPromotionAuthority> {
        let authority_bytes = serde_json::to_vec(authority)?;
        let authority_record_blob_id = self.put_blob(&authority_bytes)?;
        let event_receipt_blob_id = self.put_blob(&lease.event_receipt_bytes)?;
        let intent_recorded_at_ms = now_unix_ms().max(lease.occurred_at_ms);
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let has_terminal_receipt: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM command_receipts WHERE command_id = ?1)",
            [authority.command_id().to_string()],
            |row| row.get(0),
        )?;
        if has_terminal_receipt {
            return Err(admission_error(
                "promotion authority must be recorded before command completion",
            ));
        }
        if !durable_promotion_request_matches(&transaction, recorded_request, authority)? {
            return Err(admission_error(
                "promotion authority does not match its durable command request",
            ));
        }
        if !persisted_promotion_subject_matches(&transaction, authority)? {
            return Err(admission_error(
                "promotion intent does not match its exact source, admission record, subject, and result",
            ));
        }
        for (blob_id, byte_len) in [
            (event_receipt_blob_id, lease.event_receipt_bytes.len()),
            (authority_record_blob_id, authority_bytes.len()),
        ] {
            insert_blob_row(&transaction, blob_id, byte_len, intent_recorded_at_ms)?;
        }
        insert_promotion_presence(
            &transaction,
            lease,
            authority,
            event_receipt_blob_id,
            intent_recorded_at_ms,
        )?;
        insert_promotion_authority_record(
            &transaction,
            lease,
            authority,
            event_receipt_blob_id,
            authority_record_blob_id,
            intent_recorded_at_ms,
        )?;
        transaction.commit()?;
        Ok(RecordedPromotionAuthority {
            session_nonce: self.session_nonce,
            command_id: authority.command_id(),
            record_blob_id: authority_record_blob_id,
            source_revision_id: authority.source_revision_id(),
            source_blob_id: authority.source_blob_id(),
            intended_result_blob_id: authority.intended_result_blob_id(),
            intended_result_byte_len: authority.intended_result_byte_len(),
        })
    }

    pub(crate) fn quarantine_pending_legacy_candidates(&mut self) -> Result<()> {
        let pending = {
            let mut statement = self.connection.prepare(
                "SELECT candidate_id
                 FROM research_legacy_candidate_review_events
                 WHERE sequence = 0 AND disposition = 'pending'
                   AND NOT EXISTS (
                       SELECT 1 FROM research_legacy_candidate_review_events terminal
                       WHERE terminal.candidate_id = research_legacy_candidate_review_events.candidate_id
                         AND terminal.sequence > 0
                   )
                 ORDER BY candidate_id",
            )?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        if pending.is_empty() {
            return Ok(());
        }
        let created_at_ms = now_unix_ms().max(1);
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for candidate_id in pending {
            transaction.execute(
                "INSERT INTO research_legacy_candidate_review_events(
                    candidate_id, sequence, disposition, assembly_id, reason, created_at_ms
                 ) VALUES (?1, 1, 'quarantined', NULL, ?2, ?3)",
                params![
                    candidate_id,
                    "legacy candidate predates verifier-owned exact replay; preserved as diagnostic evidence",
                    created_at_ms,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
}

#[cfg(test)]
struct ReplayedCall {
    verification_fingerprint: BlobId,
    token_byte_boundaries: Option<Vec<u64>>,
}

#[cfg(test)]
fn replay_base_writer_call(
    call: &ModelCall,
    raw_output: &[u8],
    token_ids: &[u32],
    raw_event_stream: &[u8],
    backend_receipt: &[u8],
) -> Result<ReplayedCall> {
    if backend_receipt.len() > MAX_BACKEND_EVIDENCE_BYTES
        || raw_event_stream.len() > MAX_BACKEND_EVIDENCE_BYTES
    {
        return Err(admission_error(
            "native receipt or event stream exceeds its bound",
        ));
    }
    if call.evidence_class() != CallEvidenceClass::LiveBaseWriterClaim {
        return Err(admission_error(
            "model call is not a live base-writer claim",
        ));
    }
    let completed = call.completed()?;
    if BlobId::digest(raw_output) != completed.raw_output_blob_id()
        || raw_output.len() as u64 != completed.raw_output_byte_len()
    {
        return Err(admission_error(
            "raw output does not match the call terminal",
        ));
    }
    completed.token_evidence().verify(token_ids)?;
    if BlobId::digest(raw_event_stream) != completed.raw_event_stream_blob_id() {
        return Err(admission_error(
            "raw event stream does not match the call terminal",
        ));
    }
    let expected_receipt_blob = completed
        .backend_receipt_blob_id()
        .ok_or_else(|| admission_error("live call has no backend receipt"))?;
    if BlobId::digest(backend_receipt) != expected_receipt_blob {
        return Err(admission_error(
            "backend receipt bytes do not match the call terminal",
        ));
    }

    let receipt: NativeBaseWriterReceiptV1 = serde_json::from_slice(backend_receipt)?;
    let identity = call.identity();
    let receipt_matches = receipt.format == STRICT_RECEIPT_FORMAT
        && receipt.call_id == call.id()
        && receipt.evidence_class == CallEvidenceClass::LiveBaseWriterClaim
        && receipt.scope == identity.scope()
        && receipt.seed == identity.seed()
        && receipt.model_fingerprint == identity.model_fingerprint()
        && receipt.tokenizer_fingerprint == identity.tokenizer_fingerprint()
        && receipt.prompt_fingerprint == identity.prompt_fingerprint()
        && receipt.sampler_fingerprint == identity.sampler_fingerprint()
        && receipt.control_program_fingerprint == identity.control_program_fingerprint()
        && receipt.raw_output_blob_id == completed.raw_output_blob_id()
        && receipt.raw_output_byte_len == completed.raw_output_byte_len()
        && receipt.token_count == completed.token_evidence().token_count()
        && receipt.token_ids_fingerprint == completed.token_evidence().token_ids_fingerprint()
        && receipt.raw_event_stream_blob_id == completed.raw_event_stream_blob_id()
        && receipt.started_at_ms > 0
        && receipt.completed_at_ms >= receipt.started_at_ms;
    if !receipt_matches {
        return Err(admission_error(
            "backend receipt is not bound to every exact call fact",
        ));
    }
    validate_token_boundaries(
        receipt.token_byte_boundaries.as_deref(),
        token_ids.len(),
        raw_output,
    )?;
    replay_event_stream(call, completed, raw_event_stream, &receipt)?;

    let mut verification = Vec::new();
    verification.extend_from_slice(b"loom/store-call-replay/v1\0");
    verification.extend_from_slice(expected_receipt_blob.as_bytes());
    verification.extend_from_slice(completed.raw_event_stream_blob_id().as_bytes());
    verification.extend_from_slice(completed.raw_output_blob_id().as_bytes());
    verification.extend_from_slice(
        completed
            .token_evidence()
            .token_ids_fingerprint()
            .as_bytes(),
    );
    verification.extend_from_slice(receipt.execution_instance_fingerprint.as_bytes());
    verification.extend_from_slice(&receipt.started_at_ms.to_be_bytes());
    verification.extend_from_slice(&receipt.completed_at_ms.to_be_bytes());
    Ok(ReplayedCall {
        verification_fingerprint: BlobId::digest(&verification),
        token_byte_boundaries: receipt.token_byte_boundaries,
    })
}

#[cfg(test)]
fn replay_event_stream(
    call: &ModelCall,
    completed: &loom_research_types::CompletedCall,
    raw_event_stream: &[u8],
    receipt: &NativeBaseWriterReceiptV1,
) -> Result<()> {
    let stream: NativeCallEventStreamV1 = serde_json::from_slice(raw_event_stream)?;
    if stream.format != STRICT_EVENT_STREAM_FORMAT
        || stream.call_id != call.id()
        || stream.events.len() < 2
        || stream.events.len() > MAX_EVENT_COUNT
    {
        return Err(admission_error("native event stream envelope is invalid"));
    }
    for (index, event) in stream.events.iter().enumerate() {
        if event.sequence != index as u64
            || event.occurred_at_ms < receipt.started_at_ms
            || event.occurred_at_ms > receipt.completed_at_ms
        {
            return Err(admission_error(
                "native event stream is not contiguous and time-bounded",
            ));
        }
    }
    let first = &stream.events[0];
    let last = stream.events.last().expect("length checked above");
    if first.kind != NativeCallEventKind::CallStarted
        || first.evidence_fingerprint != call_start_fingerprint(call)
        || last.kind != NativeCallEventKind::CallCompleted
        || last.evidence_fingerprint != completed_call_fingerprint(completed)
        || stream.events[1..stream.events.len() - 1]
            .iter()
            .any(|event| event.kind != NativeCallEventKind::BackendEvent)
    {
        return Err(admission_error(
            "native event stream start or terminal evidence does not match the call",
        ));
    }
    Ok(())
}

#[cfg(test)]
fn call_start_fingerprint(call: &ModelCall) -> BlobId {
    let identity = call.identity();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"loom/call-start/v1\0");
    bytes.extend_from_slice(&call.id().as_ulid().to_bytes());
    bytes.extend_from_slice(&identity.scope().campaign_id().as_ulid().to_bytes());
    bytes.extend_from_slice(&identity.scope().stage_id().as_ulid().to_bytes());
    bytes.extend_from_slice(&identity.scope().attempt_id().as_ulid().to_bytes());
    bytes.extend_from_slice(&identity.scope().case_id().as_ulid().to_bytes());
    for fingerprint in [
        identity.model_fingerprint(),
        identity.tokenizer_fingerprint(),
        identity.prompt_fingerprint(),
        identity.sampler_fingerprint(),
        identity.control_program_fingerprint(),
    ] {
        bytes.extend_from_slice(fingerprint.as_bytes());
    }
    bytes.extend_from_slice(&identity.seed().to_be_bytes());
    BlobId::digest(&bytes)
}

#[cfg(test)]
fn completed_call_fingerprint(completed: &loom_research_types::CompletedCall) -> BlobId {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"loom/call-completed/v1\0");
    bytes.extend_from_slice(completed.raw_output_blob_id().as_bytes());
    bytes.extend_from_slice(&completed.raw_output_byte_len().to_be_bytes());
    bytes.extend_from_slice(&completed.token_evidence().token_count().to_be_bytes());
    bytes.extend_from_slice(
        completed
            .token_evidence()
            .token_ids_fingerprint()
            .as_bytes(),
    );
    // The event stream and backend receipt bind this terminal fingerprint.
    // Including either digest here would create an impossible self-reference.
    BlobId::digest(&bytes)
}

#[cfg(test)]
fn validate_token_boundaries(
    boundaries: Option<&[u64]>,
    token_count: usize,
    raw_output: &[u8],
) -> Result<()> {
    let Some(boundaries) = boundaries else {
        return Ok(());
    };
    let text = std::str::from_utf8(raw_output)
        .map_err(|_| admission_error("raw writer output is not UTF-8"))?;
    if boundaries.len() != token_count.saturating_add(1)
        || boundaries.first() != Some(&0)
        || boundaries.last() != Some(&(raw_output.len() as u64))
        || boundaries.windows(2).any(|pair| pair[0] > pair[1])
        || boundaries.iter().any(|offset| {
            usize::try_from(*offset)
                .ok()
                .is_none_or(|offset| !text.is_char_boundary(offset))
        })
    {
        return Err(admission_error(
            "token-byte boundaries do not exactly cover the UTF-8 output",
        ));
    }
    Ok(())
}

fn verify_declared_token_mapping(
    record: &GeneratedSpanOccurrenceRecord,
    admitted_call: &AdmittedModelCall,
) -> Result<()> {
    match (
        record.token_range(),
        record.token_boundaries_fingerprint_claim(),
    ) {
        (None, None) => Ok(()),
        (Some(range), Some(claim)) => {
            let boundaries = admitted_call
                .token_byte_boundaries
                .as_deref()
                .ok_or_else(|| admission_error("span claims token mapping absent from receipt"))?;
            if token_boundaries_fingerprint(boundaries) != claim {
                return Err(admission_error(
                    "span token-boundary claim differs from replayed receipt",
                ));
            }
            let start = boundaries
                .get(range.start() as usize)
                .copied()
                .ok_or_else(|| admission_error("span token start is out of bounds"))?;
            let end = boundaries
                .get(range.end() as usize)
                .copied()
                .ok_or_else(|| admission_error("span token end is out of bounds"))?;
            if start != record.output_byte_range().start()
                || end != record.output_byte_range().end()
            {
                return Err(admission_error(
                    "span byte and token ranges do not identify the same output",
                ));
            }
            Ok(())
        }
        _ => Err(admission_error("span has a partial token-mapping claim")),
    }
}

fn token_boundaries_fingerprint(boundaries: &[u64]) -> BlobId {
    let mut bytes = Vec::with_capacity(40 + boundaries.len() * 8);
    bytes.extend_from_slice(b"loom/token-byte-boundaries/v1\0");
    bytes.extend_from_slice(&(boundaries.len() as u64).to_be_bytes());
    for boundary in boundaries {
        bytes.extend_from_slice(&boundary.to_be_bytes());
    }
    BlobId::digest(&bytes)
}

fn insert_operation_graph(
    transaction: &Transaction<'_>,
    graph: &OperationGraph,
    graph_record_blob_id: BlobId,
    created_at_ms: i64,
) -> Result<()> {
    let graph_fingerprint = graph.fingerprint();
    transaction.execute(
        "INSERT INTO research_operation_graphs(
            graph_fingerprint, graph_record_blob_id, output_operation_id,
            node_count, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            graph_fingerprint.to_string(),
            graph_record_blob_id.to_string(),
            graph.output().to_string(),
            checked_sql_usize(graph.nodes().len(), "operation node count")?,
            created_at_ms,
        ],
    )?;
    for (position, operation) in graph.nodes().iter().enumerate() {
        let (kind, reference, evidence, producer_call_id) = operation_columns(operation.kind());
        transaction.execute(
            "INSERT INTO research_pipeline_operations(
                graph_fingerprint, position, operation_id, operation_kind,
                reference_id, producer_call_id, evidence_class
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                graph_fingerprint.to_string(),
                checked_sql_usize(position, "operation position")?,
                operation.id().to_string(),
                kind,
                reference,
                producer_call_id,
                evidence,
            ],
        )?;
    }
    for operation in graph.nodes() {
        for (position, input) in operation.inputs().iter().enumerate() {
            transaction.execute(
                "INSERT INTO research_pipeline_operation_inputs(
                    graph_fingerprint, operation_id, position, input_operation_id
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![
                    graph_fingerprint.to_string(),
                    operation.id().to_string(),
                    checked_sql_usize(position, "operation input position")?,
                    input.to_string(),
                ],
            )?;
        }
    }
    Ok(())
}

fn operation_columns(
    kind: &PipelineOperationKind,
) -> (&'static str, String, Option<&'static str>, Option<String>) {
    match kind {
        PipelineOperationKind::ModelCall {
            call_id,
            evidence_class,
        } => (
            "model_call",
            call_id.to_string(),
            Some(evidence_class_name(*evidence_class)),
            None,
        ),
        PipelineOperationKind::ExtractSpan { occurrence_id } => {
            ("extract_span", occurrence_id.to_string(), None, None)
        }
        PipelineOperationKind::Assemble { assembly_id } => {
            ("assemble", assembly_id.to_string(), None, None)
        }
        PipelineOperationKind::Project { projection_id } => {
            ("project", projection_id.to_string(), None, None)
        }
        PipelineOperationKind::HumanTransformation { content_blob_id } => (
            "human_transformation",
            content_blob_id.to_string(),
            None,
            None,
        ),
        PipelineOperationKind::InstructEditorTransformation {
            call_id,
            output_blob_id,
        } => (
            "instruct_editor_transformation",
            output_blob_id.to_string(),
            None,
            Some(call_id.to_string()),
        ),
        PipelineOperationKind::CriticText {
            call_id,
            output_blob_id,
        } => (
            "critic_text",
            output_blob_id.to_string(),
            None,
            Some(call_id.to_string()),
        ),
        PipelineOperationKind::CodexText { content_blob_id } => {
            ("codex_text", content_blob_id.to_string(), None, None)
        }
        PipelineOperationKind::FixtureText { content_blob_id } => {
            ("fixture_text", content_blob_id.to_string(), None, None)
        }
        PipelineOperationKind::HistoricalText { content_blob_id } => {
            ("historical_text", content_blob_id.to_string(), None, None)
        }
        PipelineOperationKind::LiteralText { content_blob_id } => {
            ("literal_text", content_blob_id.to_string(), None, None)
        }
    }
}

const fn evidence_class_name(class: CallEvidenceClass) -> &'static str {
    match class {
        CallEvidenceClass::LiveBaseWriterClaim => "live_base_writer_claim",
        CallEvidenceClass::LiveInstructEditorClaim => "live_instruct_editor_claim",
        CallEvidenceClass::LiveLocalCriticClaim => "live_local_critic_claim",
        CallEvidenceClass::LiveCodexCriticClaim => "live_codex_critic_claim",
        CallEvidenceClass::Fixture => "fixture",
        CallEvidenceClass::Mock => "mock",
        CallEvidenceClass::HistoricalReceipt => "historical_receipt",
    }
}

const fn join_before_name(join: JoinBefore) -> &'static str {
    match join {
        JoinBefore::None => "none",
        JoinBefore::Space => "space",
        JoinBefore::LineBreak => "line_break",
        JoinBefore::ParagraphBreak => "paragraph_break",
    }
}

const fn user_presence_kind_name(kind: UserPresenceKind) -> &'static str {
    match kind {
        UserPresenceKind::EditorGesture => "editor_gesture",
        UserPresenceKind::CliInteractiveConfirmation => "cli_interactive_confirmation",
        UserPresenceKind::NativeDialogConfirmation => "native_dialog_confirmation",
        UserPresenceKind::HumanReviewSubmission => "human_review_submission",
    }
}

fn insert_promotion_presence(
    transaction: &Transaction<'_>,
    lease: &VerifiedUserPresence,
    authority: &PromotionAuthority,
    event_receipt_blob_id: BlobId,
    intent_recorded_at_ms: i64,
) -> Result<()> {
    let inserted = transaction.execute(
        "INSERT INTO research_user_presence_events(
            event_receipt_blob_id, command_id, command_request_fingerprint,
            actor, user_presence_kind, session_fingerprint,
            monotonic_event_index, occurred_at_ms, created_at_ms
         ) SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9
           WHERE NOT EXISTS (
               SELECT 1 FROM command_receipts receipt WHERE receipt.command_id = ?2
           )",
        params![
            event_receipt_blob_id.to_string(),
            authority.command_id().to_string(),
            authority.command_request_fingerprint().to_string(),
            lease.actor.as_str(),
            user_presence_kind_name(lease.kind),
            lease.session_fingerprint.to_string(),
            checked_sql_u64(lease.monotonic_event_index, "presence event index")?,
            lease.occurred_at_ms,
            intent_recorded_at_ms,
        ],
    )?;
    if inserted != 1 {
        return Err(admission_error(
            "promotion command became terminal before presence admission",
        ));
    }
    Ok(())
}

fn insert_promotion_authority_record(
    transaction: &Transaction<'_>,
    lease: &VerifiedUserPresence,
    authority: &PromotionAuthority,
    event_receipt_blob_id: BlobId,
    authority_record_blob_id: BlobId,
    intent_recorded_at_ms: i64,
) -> Result<()> {
    let subject = authority.subject();
    let inserted = transaction.execute(
        "INSERT INTO research_promotion_authorities(
            command_id, command_request_fingerprint, actor, project_id,
            source_revision_id, source_blob_id, subject_kind, subject_id,
            admission_record_id, intended_result_blob_id, intended_result_byte_len,
            user_presence_kind, session_fingerprint, event_receipt_blob_id,
            monotonic_event_index, occurred_at_ms,
            authority_record_blob_id, intent_recorded_at_ms
         ) SELECT
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
            ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18
           WHERE NOT EXISTS (
               SELECT 1 FROM command_receipts receipt WHERE receipt.command_id = ?1
           )",
        params![
            authority.command_id().to_string(),
            authority.command_request_fingerprint().to_string(),
            authority.actor().as_str(),
            authority.project_id().to_string(),
            authority.source_revision_id().to_string(),
            authority.source_blob_id().to_string(),
            subject.kind_name(),
            subject.id_string(),
            authority.admission_record_id().to_string(),
            authority.intended_result_blob_id().to_string(),
            checked_sql_u64(
                authority.intended_result_byte_len(),
                "intended promotion result length",
            )?,
            user_presence_kind_name(lease.kind),
            lease.session_fingerprint.to_string(),
            event_receipt_blob_id.to_string(),
            checked_sql_u64(lease.monotonic_event_index, "presence event index")?,
            lease.occurred_at_ms,
            authority_record_blob_id.to_string(),
            intent_recorded_at_ms,
        ],
    )?;
    if inserted != 1 {
        return Err(admission_error(
            "promotion command became terminal before authority admission",
        ));
    }
    Ok(())
}

fn durable_promotion_request_matches(
    connection: &Connection,
    recorded: &RecordedPromotionRequest,
    authority: &PromotionAuthority,
) -> Result<bool> {
    connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM research_promotion_command_requests
                WHERE command_id = ?1
                  AND command_request_fingerprint = ?2
                  AND canonical_request_blob_id = ?2
                  AND canonical_request_byte_len = ?3
                  AND project_id = ?4
                  AND source_revision_id = ?5
                  AND source_blob_id = ?6
                  AND subject_kind = ?7
                  AND subject_id = ?8
                  AND admission_record_id = ?9
                  AND intended_result_blob_id = ?10
                  AND intended_result_byte_len = ?11
                  AND requested_at_ms = ?12
                  AND recorded_at_ms = ?13
             )",
            params![
                authority.command_id().to_string(),
                authority.command_request_fingerprint().to_string(),
                checked_sql_usize(
                    authority.request().canonical_request_bytes().len(),
                    "canonical promotion request length",
                )?,
                authority.project_id().to_string(),
                authority.source_revision_id().to_string(),
                authority.source_blob_id().to_string(),
                authority.subject().kind_name(),
                authority.subject().id_string(),
                authority.admission_record_id().to_string(),
                authority.intended_result_blob_id().to_string(),
                checked_sql_u64(
                    authority.intended_result_byte_len(),
                    "intended promotion result length",
                )?,
                authority.command_requested_at_ms(),
                recorded.recorded_at_ms,
            ],
            |row| row.get(0),
        )
        .map_err(StoreError::from)
}

fn persisted_promotion_subject_matches(
    connection: &Connection,
    authority: &PromotionAuthority,
) -> Result<bool> {
    let subject_id = authority.subject().id_string();
    let query = match authority.subject() {
        PromotionSubject::CandidateProjection { .. } => {
            "SELECT EXISTS(
                SELECT 1
                FROM research_admission_records admission
                JOIN research_candidate_projections projection
                  ON projection.projection_id = admission.subject_id
                WHERE admission.admission_record_id = ?1
                  AND admission.subject_kind = 'candidate_projection'
                  AND admission.subject_id = ?2
                  AND projection.source_revision_id = ?3
                  AND projection.source_blob_id = ?4
                  AND projection.resulting_blob_id = ?5
                  AND projection.resulting_byte_len = ?6
             )"
        }
        PromotionSubject::MixedAuthorship { .. } => {
            "SELECT EXISTS(
                SELECT 1
                FROM research_admission_records admission
                JOIN research_mixed_authorship_assemblies mixed
                  ON mixed.mixed_assembly_id = admission.subject_id
                JOIN revisions source_revision ON source_revision.revision_id = ?3
                JOIN artifacts source_artifact
                  ON source_artifact.artifact_id = source_revision.artifact_id
                WHERE admission.admission_record_id = ?1
                  AND admission.subject_kind = 'mixed_authorship'
                  AND admission.subject_id = ?2
                  AND source_artifact.blob_id = ?4
                  AND mixed.output_blob_id = ?5
                  AND mixed.output_byte_len = ?6
             )"
        }
    };
    connection
        .query_row(
            query,
            params![
                authority.admission_record_id().to_string(),
                subject_id,
                authority.source_revision_id().to_string(),
                authority.source_blob_id().to_string(),
                authority.intended_result_blob_id().to_string(),
                checked_sql_u64(
                    authority.intended_result_byte_len(),
                    "intended promotion result length",
                )?,
            ],
            |row| row.get(0),
        )
        .map_err(StoreError::from)
}

fn promotion_subject_lease_matches(
    expected_session: StoreSessionNonce,
    lease: PromotionSubjectLease<'_>,
    request: &PromotionCommandRequest,
) -> bool {
    match (lease, request.subject()) {
        (
            PromotionSubjectLease::CandidateProjection(admitted),
            PromotionSubject::CandidateProjection { projection_id },
        ) => {
            admitted.session_nonce == expected_session
                && admitted.admission_record_id.as_blob_id() == request.admission_record_id()
                && admitted.record.id() == projection_id
                && admitted.record.source_revision_id() == request.source_revision_id()
                && admitted.record.source_blob_id() == request.source_blob_id()
                && admitted.record.witness().resulting_blob_id()
                    == request.intended_result_blob_id()
                && admitted.record.witness().resulting_byte_len()
                    == request.intended_result_byte_len()
        }
        (
            PromotionSubjectLease::MixedAuthorship(admitted),
            PromotionSubject::MixedAuthorship { mixed_assembly_id },
        ) => {
            admitted.session_nonce == expected_session
                && admitted.admission_record_id.as_blob_id() == request.admission_record_id()
                && admitted.record.id() == mixed_assembly_id
                && admitted.record.output_blob_id() == request.intended_result_blob_id()
                && admitted.record.output_byte_len() == request.intended_result_byte_len()
        }
        _ => false,
    }
}

fn exact_evidence(calls: &[OwnedExactCall]) -> Vec<ExactCallEvidence<'_>> {
    calls
        .iter()
        .map(|call| ExactCallEvidence::new(&call.call, &call.raw_output, &call.token_ids))
        .collect()
}

fn encode_token_ids(token_ids: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(token_ids.len() * 4);
    for token_id in token_ids {
        bytes.extend_from_slice(&token_id.to_be_bytes());
    }
    bytes
}

fn admission_record_id(kind: &str, subject: &str) -> ResearchAdmissionRecordId {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"loom/research-admission/v1\0");
    bytes.extend_from_slice(kind.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(subject.as_bytes());
    ResearchAdmissionRecordId(BlobId::digest(&bytes))
}

fn checked_sql_u64(value: u64, field: &'static str) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| admission_error(format!("{field} exceeds SQLite integer range")))
}

fn checked_sql_usize(value: usize, field: &'static str) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| admission_error(format!("{field} exceeds SQLite integer range")))
}

fn admission_error(message: impl Into<String>) -> StoreError {
    StoreError::ResearchAdmission(message.into())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    use loom_document::DocumentContent;
    use loom_inference::{
        NonauthorizingPersistedCaseOutcomeTestVector, NonauthorizingPersistedCaseTestSpec,
        NonauthorizingPersistedCaseTestVector, NonauthorizingPersistedEvidenceTestVector,
        nonauthorizing_persisted_evidence_test_vector_for_cases,
    };
    use loom_research_types::{
        AssemblyPartRecord, CallIdentity, CallScope, CallTerminal, CampaignId, CandidateAssemblyId,
        CandidateProjectionId, CompletedCall, ExactPromptSource, GeneratedSpanOccurrenceId,
        MixedAuthorshipAssemblyId, ModelCallId, OutputProjection, PipelineOperation,
        PipelineOperationId, PromotionActor, StageAttemptId, StageId, TerminalMessage,
        TokenEvidence, TrialCaseId, UserPresenceEvidence,
    };
    use loom_types::RevisionId;
    use tempfile::tempdir;

    struct MixedPromotionFixture {
        admission: MixedAuthorshipAdmission,
        source_revision_id: RevisionId,
        source_blob_id: BlobId,
    }

    #[derive(Clone, Copy)]
    struct PresenceSpec<'a> {
        authority_actor: &'a str,
        host_actor: &'a str,
        session_fingerprint: BlobId,
        monotonic_event_index: u64,
        event_receipt_bytes: &'a [u8],
    }

    fn mixed_promotion_fixture(store: &mut ProjectStore) -> MixedPromotionFixture {
        store
            .save_document(
                "manuscript/source.md",
                DocumentContent::Prose("Pinned source manuscript.".into()),
                "promotion source",
            )
            .expect("save promotion source");
        let source = store
            .read_document("manuscript/source.md")
            .expect("read promotion source");
        let exact_output = b"Human-authored continuation.";
        let output_operation_id = PipelineOperationId::new();
        let operation_graph = OperationGraph::new(
            vec![
                PipelineOperation::new(
                    output_operation_id,
                    PipelineOperationKind::LiteralText {
                        content_blob_id: BlobId::digest(exact_output),
                    },
                    Vec::new(),
                )
                .expect("literal output operation"),
            ],
            output_operation_id,
        )
        .expect("mixed operation graph");
        let record = MixedAuthorshipAssemblyRecord::new(
            MixedAuthorshipAssemblyId::new(),
            exact_output,
            operation_graph,
        )
        .expect("mixed-authorship record");
        let admission = store
            .record_mixed_authorship_assembly(record, exact_output)
            .expect("mixed-authorship admission");
        MixedPromotionFixture {
            admission,
            source_revision_id: source.revision_id,
            source_blob_id: source.blob_id,
        }
    }

    fn promotion_request(
        store: &ProjectStore,
        fixture: &MixedPromotionFixture,
        command_id: CommandId,
    ) -> PromotionCommandRequest {
        PromotionCommandRequest::new(
            store.manifest().project_id,
            fixture.source_revision_id,
            fixture.source_blob_id,
            PromotionSubject::MixedAuthorship {
                mixed_assembly_id: fixture.admission.record().id(),
            },
            fixture.admission.admission_record_id().as_blob_id(),
            fixture.admission.record().output_blob_id(),
            fixture.admission.record().output_byte_len(),
            command_id,
            now_unix_ms().max(1),
        )
        .expect("promotion request")
    }

    fn authority_and_presence(
        store: &ProjectStore,
        recorded_request: &RecordedPromotionRequest,
        request: &PromotionCommandRequest,
        spec: PresenceSpec<'_>,
    ) -> (PromotionAuthority, VerifiedUserPresence) {
        let occurred_at_ms = recorded_request.recorded_at_ms + 1;
        let event_receipt_blob_id = BlobId::digest(spec.event_receipt_bytes);
        let user_presence = UserPresenceEvidence::new(
            UserPresenceKind::EditorGesture,
            spec.session_fingerprint,
            event_receipt_blob_id,
            spec.monotonic_event_index,
            occurred_at_ms,
        )
        .expect("user-presence claim");
        let authority = PromotionAuthority::new(
            PromotionActor::new(spec.authority_actor).expect("authority actor"),
            request.clone(),
            user_presence,
        )
        .expect("promotion authority");
        let lease = VerifiedUserPresence {
            session_nonce: store.session_nonce,
            command_id: request.command_id(),
            command_request_fingerprint: request.command_request_fingerprint(),
            kind: UserPresenceKind::EditorGesture,
            session_fingerprint: spec.session_fingerprint,
            event_receipt_blob_id,
            event_receipt_bytes: spec.event_receipt_bytes.to_vec(),
            monotonic_event_index: spec.monotonic_event_index,
            occurred_at_ms,
            actor: PromotionActor::new(spec.host_actor).expect("host actor"),
        };
        (authority, lease)
    }

    fn copy_tree(source: &Path, destination: &Path) {
        fs::create_dir_all(destination).expect("create copied project directory");
        for entry in fs::read_dir(source).expect("read source project") {
            let entry = entry.expect("source project entry");
            let file_type = entry.file_type().expect("source entry type");
            assert!(
                !file_type.is_symlink(),
                "test project must not contain symlinks"
            );
            let target = destination.join(entry.file_name());
            if file_type.is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                assert!(file_type.is_file(), "test project entries must be files");
                fs::copy(entry.path(), target).expect("copy project file");
            }
        }
    }

    fn store_prompt_fixture(store: &mut ProjectStore) -> StorePromptEvidence {
        let path = format!("manuscript/prompt-{}.md", RevisionId::new());
        store
            .save_document(
                &path,
                DocumentContent::Prose("CHAPTER ONE\n\nThe rain stopped at the gate.".into()),
                "prompt fixture source",
            )
            .expect("save exact prompt source");
        let source = store
            .read_document(&path)
            .expect("read exact prompt source");
        let raw_utf8 = store
            .read_blob(source.blob_id)
            .expect("read prompt source blob");
        let ordered_token_ids = vec![17, 29, 43, 71];
        let raw_blob_id = BlobId::digest(&raw_utf8);
        let form = PromptFormEvidence::Completion;
        let token_policy = PromptTokenPolicyEvidence::NoBosParseSpecial;
        let scope = CallScope::new(
            CampaignId::new(),
            StageId::new(),
            StageAttemptId::new(),
            TrialCaseId::new(),
        );
        let treatment_recipe_fingerprint = BlobId::digest(b"test direct continuation");
        let source_tail_range =
            NonEmptyByteRange::new(0, raw_utf8.len() as u64).expect("nonempty prompt source");
        let specification = FrozenBaseCompletionPrompt::new(
            store.manifest().project_id,
            scope,
            treatment_recipe_fingerprint,
            Vec::new(),
            loom_research_types::CompletionPromptTail::live_manuscript(
                source.revision_id,
                source.blob_id,
                source_tail_range,
            )
            .expect("prompt tail"),
        )
        .expect("frozen prompt specification");
        let compiled = specification
            .clone()
            .compile(
                raw_utf8.clone(),
                &[loom_research_types::ExactPromptSource::new(
                    source.revision_id,
                    &raw_utf8,
                )],
            )
            .expect("compile prompt fixture");
        let source_prompt_fingerprint = compiled.fingerprint();
        let content_fingerprint = compiled.content_fingerprint();
        let tail_prompt_range = compiled.tail_prompt_range();
        let sources = exact_prompt_sources(&specification).expect("exact prompt sources");
        let specification_bytes = serde_json::to_vec(&specification).expect("prompt spec JSON");
        StorePromptEvidence {
            specification_blob_id: BlobId::digest(&specification_bytes),
            specification_bytes,
            project_id: store.manifest().project_id,
            scope,
            treatment_recipe_fingerprint,
            source_prompt_fingerprint,
            content_fingerprint,
            tail_prompt_range,
            source_tail_revision_id: Some(source.revision_id.to_string()),
            source_tail_blob_id: source.blob_id,
            source_tail_range,
            source_tail_origin: StoreTailOrigin::LiveManuscript,
            sources,
            raw_utf8,
            raw_blob_id,
            form,
            token_policy,
            token_fingerprint: prompt_token_fingerprint(&ordered_token_ids),
            compiled_fingerprint: compiled_prompt_fingerprint(
                source_prompt_fingerprint,
                raw_blob_id,
                form,
                token_policy,
                &ordered_token_ids,
            ),
            ordered_token_ids,
        }
    }

    fn compile_store_prompt_fixture(prompt: &StorePromptEvidence) -> CompiledBaseCompletionPrompt {
        let specification: FrozenBaseCompletionPrompt =
            serde_json::from_slice(&prompt.specification_bytes).expect("frozen prompt JSON");
        let revision_id = specification
            .tail()
            .source_revision_id()
            .expect("store test prompt uses a live revision");
        specification
            .compile(
                prompt.raw_utf8.clone(),
                &[ExactPromptSource::new(revision_id, &prompt.raw_utf8)],
            )
            .expect("recompile store prompt fixture")
    }

    fn document_path_for_revision(store: &ProjectStore, revision_id: RevisionId) -> String {
        store
            .connection
            .query_row(
                "SELECT document.relative_path
                 FROM revisions revision
                 JOIN documents document USING (document_id)
                 WHERE revision.revision_id = ?1",
                [revision_id.to_string()],
                |row| row.get(0),
            )
            .expect("prompt document path")
    }

    fn adoption_identity(prompt_fingerprint: BlobId, scope: CallScope, seed: u64) -> CallIdentity {
        CallIdentity::new(
            scope,
            BlobId::digest(b"adoption model"),
            BlobId::digest(b"adoption tokenizer"),
            prompt_fingerprint,
            BlobId::digest(b"adoption sampler"),
            BlobId::digest(b"adoption controls"),
            seed,
        )
    }

    fn test_base_writer_binding_source(source_prefix: &str) -> BaseWriterBinding {
        let model_sha256 = BlobId::digest(b"test model artifact");
        let tokenizer_sha256 = BlobId::digest(b"adoption tokenizer");
        let source = format!(
            r#"{source_prefix}format = "loom.model-bindings.v1"
name = "store-test-models"
description = "Exact store adoption fixture"

[[bindings]]
id = "writer"
role = "base_writer"
model_sha256 = "{model_sha256}"
model_bytes = 4096
tokenizer_sha256 = "{tokenizer_sha256}"
architecture = "test_architecture"
context_tokens = 256
capabilities = ["completion", "logits"]
adapters = []
"#
        );
        let manifest = compile_manifest(source.as_bytes()).expect("test model binding manifest");
        BaseWriterBinding::compile(&manifest, "writer").expect("test base-writer binding")
    }

    fn test_base_writer_binding() -> BaseWriterBinding {
        test_base_writer_binding_source("")
    }

    fn untrusted_cancelled_adoption_fixture(
        input_index: usize,
        call_id: ModelCallId,
        prompt: &StorePromptEvidence,
    ) -> CancelledAdoptionMaterial {
        let partial_raw_output = format!("Partial output {input_index}").into_bytes();
        let token_offset = u32::try_from(input_index).expect("bounded test input index");
        let generated_token_ids = vec![303 + token_offset];
        let event_json = serde_json::to_vec(&serde_json::json!({
            "format": "test-cancelled-events",
            "input_index": input_index,
        }))
        .expect("cancelled event JSON");
        let backend_audit_json = serde_json::to_vec(&serde_json::json!({
            "format": "test-cancelled-audit",
            "input_index": input_index,
        }))
        .expect("cancelled audit JSON");
        let call = ModelCall::new(
            call_id,
            adoption_identity(
                prompt.compiled_fingerprint,
                prompt.scope,
                900 + input_index as u64,
            ),
            CallEvidenceClass::LiveBaseWriterClaim,
            CallTerminal::Cancelled {
                message: TerminalMessage::new("cancelled by test branch")
                    .expect("cancelled message"),
            },
        )
        .expect("cancelled call");
        CancelledAdoptionMaterial {
            input_index,
            call,
            partial_raw_output,
            generated_token_ids,
            event_json,
            backend_audit_json,
            verification_fingerprint: BlobId::digest(
                format!("cancelled adoption {call_id}").as_bytes(),
            ),
        }
    }

    fn untrusted_batch_adoption_fixture(
        store: &ProjectStore,
        label: &str,
        kind: BatchAdoptionKind,
        prompt_evidence: StorePromptEvidence,
        outcomes: Vec<CaseAdoptionMaterial>,
    ) -> BatchAdoptionMaterial {
        BatchAdoptionMaterial {
            kind,
            project_id: store.manifest().project_id,
            binding: test_base_writer_binding().into(),
            prompt_evidence,
            backend_request_id: format!("test-request-{label}"),
            outcomes,
            verification_fingerprint: BlobId::digest(
                format!("test verified batch {label}").as_bytes(),
            ),
            prompt_freeze: None,
        }
    }

    fn strict_case_spec(
        input_index: usize,
        call_id: ModelCallId,
        completed: bool,
    ) -> NonauthorizingPersistedCaseTestSpec {
        let token_offset = u32::try_from(input_index).expect("bounded test input index");
        let seed = 100 + token_offset;
        let output = if completed {
            format!("Strict completion {}.", input_index + 1).into_bytes()
        } else {
            format!("Strict partial {}.", input_index + 1).into_bytes()
        };
        if completed {
            NonauthorizingPersistedCaseTestSpec::completed(
                call_id,
                seed,
                &output,
                &[101 + token_offset],
            )
        } else {
            NonauthorizingPersistedCaseTestSpec::cancelled(
                call_id,
                seed,
                &output,
                &[201 + token_offset],
            )
        }
        .expect("bounded strict persisted case spec")
    }

    fn strict_vector_case_material(
        case: NonauthorizingPersistedCaseTestVector,
    ) -> CaseAdoptionMaterial {
        let NonauthorizingPersistedCaseTestVector {
            input_index,
            model_call,
            raw_output,
            generated_token_ids,
            event_json,
            backend_audit_json,
            terminal_sampled_token_id,
            outcome,
            verification_fingerprint,
        } = case;
        match outcome {
            NonauthorizingPersistedCaseOutcomeTestVector::Completed {
                displayed_output,
                output_projection,
            } => CaseAdoptionMaterial::Completed(CompletedAdoptionMaterial {
                input_index,
                call: model_call,
                raw_output,
                generated_token_ids,
                event_json,
                backend_audit_json,
                displayed_output,
                output_projection,
                terminal_sampled_token_id,
                verification_fingerprint,
            }),
            NonauthorizingPersistedCaseOutcomeTestVector::Cancelled => {
                CaseAdoptionMaterial::Cancelled(CancelledAdoptionMaterial {
                    input_index,
                    call: model_call,
                    partial_raw_output: raw_output,
                    generated_token_ids,
                    event_json,
                    backend_audit_json,
                    verification_fingerprint,
                })
            }
        }
    }

    fn strict_batch_adoption_fixture(
        store: &ProjectStore,
        binding: BaseWriterBinding,
        prompt_evidence: StorePromptEvidence,
        case_specs: &[NonauthorizingPersistedCaseTestSpec],
    ) -> BatchAdoptionMaterial {
        let binding: StoreBindingEvidence = binding.into();
        let vector = nonauthorizing_persisted_evidence_test_vector_for_cases(
            PersistedBindingEvidenceRef {
                binding_id: &binding.binding_id,
                binding_fingerprint: binding.binding_fingerprint,
                model_sha256: binding.model_sha256,
                model_byte_len: binding.model_byte_len,
                tokenizer_sha256: binding.tokenizer_sha256,
                multimodal_projector_sha256: binding.projector_sha256,
                context_tokens: binding.context_tokens,
            },
            PersistedPromptEvidenceRef {
                project_id: prompt_evidence.project_id,
                scope: prompt_evidence.scope,
                source_prompt_fingerprint: prompt_evidence.source_prompt_fingerprint,
                content_fingerprint: prompt_evidence.content_fingerprint,
                treatment_recipe_fingerprint: prompt_evidence.treatment_recipe_fingerprint,
                raw_utf8: &prompt_evidence.raw_utf8,
                raw_blob_id: prompt_evidence.raw_blob_id,
                form: prompt_evidence.form,
                token_policy: prompt_evidence.token_policy,
                ordered_token_ids: &prompt_evidence.ordered_token_ids,
                token_fingerprint: prompt_evidence.token_fingerprint,
                compiled_fingerprint: prompt_evidence.compiled_fingerprint,
            },
            case_specs,
        )
        .expect("strict synthetic persisted vector");
        let NonauthorizingPersistedEvidenceTestVector {
            backend_request_id,
            cases,
            verification_fingerprint,
            ..
        } = vector;
        let outcomes = cases
            .into_iter()
            .map(strict_vector_case_material)
            .collect::<Vec<_>>();
        let completed_count = outcomes
            .iter()
            .filter(|case| matches!(case, CaseAdoptionMaterial::Completed(_)))
            .count();
        BatchAdoptionMaterial {
            kind: if completed_count == 0 {
                BatchAdoptionKind::DiagnosticOnly
            } else {
                BatchAdoptionKind::Admitted
            },
            project_id: store.manifest().project_id,
            binding,
            prompt_evidence,
            backend_request_id,
            outcomes,
            verification_fingerprint,
            prompt_freeze: None,
        }
    }

    fn adopt_strict_test_batch(
        store: &mut ProjectStore,
        mut batch: BatchAdoptionMaterial,
    ) -> AdoptedInferenceBatch {
        let compiled = compile_store_prompt_fixture(&batch.prompt_evidence);
        let lease = store
            .verify_compiled_prompt_for_inference(&compiled)
            .expect("freeze strict test prompt");
        consume_prompt_source_lease(store, &mut batch, lease)
            .expect("bind strict test vector to its frozen source");
        store
            .adopt_verified_material_with_freeze(batch)
            .expect("strict test vector passes adoption verifier")
    }

    fn research_adoption_counts(store: &ProjectStore) -> (i64, i64, i64, i64, i64) {
        store
            .connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM research_verified_inference_batches),
                    (SELECT COUNT(*) FROM research_model_calls),
                    (SELECT COUNT(*) FROM research_call_terminals),
                    (SELECT COUNT(*) FROM research_cancelled_call_diagnostics),
                    (SELECT COUNT(*) FROM research_verified_inference_batch_seals)",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("research adoption counts")
    }

    fn admit_single_call_assembly(
        store: &mut ProjectStore,
        admitted: &AdmittedModelCall,
    ) -> (AdmittedCandidateAssembly, CandidateAssemblyRecord) {
        let output_projection = OutputProjection::new(
            &admitted.raw_output,
            admitted.raw_output.len() as u64,
            admitted.raw_output.len() as u64,
        )
        .expect("complete output projection");
        let span_record = GeneratedSpanOccurrenceRecord::from_declared_call(
            GeneratedSpanOccurrenceId::new(),
            &admitted.call,
            &admitted.raw_output,
            &admitted.token_ids,
            output_projection,
        )
        .expect("span declaration");
        let admitted_span = store
            .verify_and_record_generated_span(admitted, span_record.clone())
            .expect("admit generated span");
        let exact = [ExactCallEvidence::new(
            &admitted.call,
            &admitted.raw_output,
            &admitted.token_ids,
        )];
        let assembly_record = CandidateAssemblyRecord::new(
            CandidateAssemblyId::new(),
            vec![AssemblyPartRecord::new(JoinBefore::None, span_record)],
            &exact,
        )
        .expect("assembly declaration");
        let admitted_assembly = store
            .verify_and_record_candidate_assembly(assembly_record.clone(), &[&admitted_span])
            .expect("admit assembly");
        (admitted_assembly, assembly_record)
    }

    pub(crate) struct CampaignPoolAdmissionTestCandidate {
        pub(crate) span: AdmittedGeneratedSpan,
        pub(crate) projection: AdmittedCandidateProjection,
    }

    pub(crate) struct CampaignPoolAdmissionTestFixture {
        pub(crate) campaign_id: CampaignId,
        pub(crate) case_id: TrialCaseId,
        pub(crate) stage_id: StageId,
        pub(crate) stage_attempt_id: StageAttemptId,
        pub(crate) treatment_fingerprint: BlobId,
        pub(crate) prompt_content_fingerprint: BlobId,
        pub(crate) model_binding_fingerprint: BlobId,
        pub(crate) batch_fingerprint: BlobId,
        pub(crate) candidates: Vec<CampaignPoolAdmissionTestCandidate>,
    }

    pub(crate) fn admitted_campaign_pool_for_test(
        store: &mut ProjectStore,
        candidate_count: usize,
    ) -> CampaignPoolAdmissionTestFixture {
        assert!((1..=MAX_BASE_WRITER_BATCH_CASES).contains(&candidate_count));
        let prompt = store_prompt_fixture(store);
        let campaign_id = prompt.scope.campaign_id();
        let case_id = prompt.scope.case_id();
        let stage_id = prompt.scope.stage_id();
        let stage_attempt_id = prompt.scope.attempt_id();
        let treatment_fingerprint = prompt.treatment_recipe_fingerprint;
        let prompt_content_fingerprint = prompt.content_fingerprint;
        let binding = test_base_writer_binding();
        let model_binding_fingerprint = binding.fingerprint();
        let cases = (0..candidate_count)
            .map(|index| strict_case_spec(index, ModelCallId::new(), true))
            .collect::<Vec<_>>();
        let batch = strict_batch_adoption_fixture(store, binding, prompt, &cases);
        let adopted = adopt_strict_test_batch(store, batch);
        let batch_fingerprint = adopted.verification_fingerprint();
        let candidates = admit_campaign_pool_candidates(store, &adopted);
        CampaignPoolAdmissionTestFixture {
            campaign_id,
            case_id,
            stage_id,
            stage_attempt_id,
            treatment_fingerprint,
            prompt_content_fingerprint,
            model_binding_fingerprint,
            batch_fingerprint,
            candidates,
        }
    }

    pub(crate) fn admitted_additional_campaign_pool_batch_for_test(
        store: &mut ProjectStore,
        original: &CampaignPoolAdmissionTestFixture,
        candidate_count: usize,
    ) -> CampaignPoolAdmissionTestFixture {
        assert!((1..=MAX_BASE_WRITER_BATCH_CASES).contains(&candidate_count));
        let stored = load_stored_batch(store, original.batch_fingerprint)
            .expect("load original sealed batch");
        let prompt = load_replayed_prompt_evidence(store, original.batch_fingerprint, &stored)
            .expect("replay original exact prompt");
        assert_eq!(prompt.scope.campaign_id(), original.campaign_id);
        assert_eq!(prompt.scope.case_id(), original.case_id);
        assert_eq!(prompt.scope.stage_id(), original.stage_id);
        assert_eq!(prompt.scope.attempt_id(), original.stage_attempt_id);
        assert_eq!(
            prompt.treatment_recipe_fingerprint,
            original.treatment_fingerprint
        );
        assert_eq!(
            prompt.content_fingerprint,
            original.prompt_content_fingerprint
        );
        let binding = test_base_writer_binding();
        assert_eq!(binding.fingerprint(), original.model_binding_fingerprint);
        let cases = (0..candidate_count)
            .map(|index| strict_case_spec(index, ModelCallId::new(), true))
            .collect::<Vec<_>>();
        let batch = strict_batch_adoption_fixture(store, binding, prompt, &cases);
        let adopted = adopt_strict_test_batch(store, batch);
        CampaignPoolAdmissionTestFixture {
            campaign_id: original.campaign_id,
            case_id: original.case_id,
            stage_id: original.stage_id,
            stage_attempt_id: original.stage_attempt_id,
            treatment_fingerprint: original.treatment_fingerprint,
            prompt_content_fingerprint: original.prompt_content_fingerprint,
            model_binding_fingerprint: original.model_binding_fingerprint,
            batch_fingerprint: adopted.verification_fingerprint(),
            candidates: admit_campaign_pool_candidates(store, &adopted),
        }
    }

    fn admit_campaign_pool_candidates(
        store: &mut ProjectStore,
        adopted: &AdoptedInferenceBatch,
    ) -> Vec<CampaignPoolAdmissionTestCandidate> {
        let mut candidates = Vec::with_capacity(adopted.admitted_calls().len());
        for (index, admitted) in adopted.admitted_calls().iter().enumerate() {
            let output_projection = OutputProjection::new(
                &admitted.raw_output,
                admitted.raw_output.len() as u64,
                admitted.raw_output.len() as u64,
            )
            .expect("complete output projection");
            let span_record = GeneratedSpanOccurrenceRecord::from_declared_call(
                GeneratedSpanOccurrenceId::new(),
                &admitted.call,
                &admitted.raw_output,
                &admitted.token_ids,
                output_projection,
            )
            .expect("span declaration");
            let span = store
                .verify_and_record_generated_span(admitted, span_record.clone())
                .expect("admit generated span");
            let exact = [ExactCallEvidence::new(
                &admitted.call,
                &admitted.raw_output,
                &admitted.token_ids,
            )];
            let assembly_record = CandidateAssemblyRecord::new(
                CandidateAssemblyId::new(),
                vec![AssemblyPartRecord::new(JoinBefore::None, span_record)],
                &exact,
            )
            .expect("assembly declaration");
            let assembly = store
                .verify_and_record_candidate_assembly(assembly_record.clone(), &[&span])
                .expect("admit assembly");
            let path = format!("manuscript/campaign-pool-{index}-{}.md", RevisionId::new());
            store
                .save_document(
                    &path,
                    DocumentContent::Prose(format!("Pinned pool source {index}. ")),
                    "campaign pool projection source",
                )
                .expect("save pool source");
            let source = store.read_document(&path).expect("read pool source");
            let target = loom_research_types::ByteRange::new(
                source.text.len() as u64,
                source.text.len() as u64,
            )
            .expect("append pool range");
            let projection_record = CandidateProjectionRecord::new(
                CandidateProjectionId::new(),
                &assembly_record,
                source.revision_id,
                source.blob_id,
                source.text.as_bytes(),
                target,
                &exact,
            )
            .expect("pool projection declaration");
            let projection = store
                .verify_and_record_candidate_projection(&assembly, projection_record)
                .expect("admit pool projection");
            candidates.push(CampaignPoolAdmissionTestCandidate { span, projection });
        }
        candidates
    }

    pub(crate) fn admitted_projection_for_evaluation_test(
        store: &mut ProjectStore,
        source_label: &str,
    ) -> AdmittedCandidateProjection {
        let prompt = store_prompt_fixture(store);
        let cases = [strict_case_spec(0, ModelCallId::new(), true)];
        let batch =
            strict_batch_adoption_fixture(store, test_base_writer_binding(), prompt, &cases);
        let adopted = adopt_strict_test_batch(store, batch);
        let admitted = &adopted.admitted_calls()[0];
        let (admitted_assembly, assembly_record) = admit_single_call_assembly(store, admitted);

        let path = format!("manuscript/evaluation-{}.md", RevisionId::new());
        store
            .save_document(
                &path,
                DocumentContent::Prose(format!("Pinned evaluation source {source_label}. ")),
                "evaluation projection source",
            )
            .expect("save evaluation source");
        let source = store.read_document(&path).expect("read evaluation source");
        let target =
            loom_research_types::ByteRange::new(source.text.len() as u64, source.text.len() as u64)
                .expect("append evaluation range");
        let exact = [ExactCallEvidence::new(
            &admitted.call,
            &admitted.raw_output,
            &admitted.token_ids,
        )];
        let projection = CandidateProjectionRecord::new(
            CandidateProjectionId::new(),
            &assembly_record,
            source.revision_id,
            source.blob_id,
            source.text.as_bytes(),
            target,
            &exact,
        )
        .expect("evaluation projection declaration");
        store
            .verify_and_record_candidate_projection(&admitted_assembly, projection)
            .expect("admit evaluation projection")
    }

    fn assert_stale_projection_rejected(
        store: &mut ProjectStore,
        admitted: &AdmittedModelCall,
        admitted_assembly: &AdmittedCandidateAssembly,
        assembly_record: &CandidateAssemblyRecord,
        source_revision_id: RevisionId,
        source_blob_id: BlobId,
    ) {
        let source_bytes = store.read_blob(source_blob_id).expect("stale source blob");
        let target = loom_research_types::ByteRange::new(
            source_bytes.len() as u64,
            source_bytes.len() as u64,
        )
        .expect("append range");
        let exact = [ExactCallEvidence::new(
            &admitted.call,
            &admitted.raw_output,
            &admitted.token_ids,
        )];
        let projection = CandidateProjectionRecord::new(
            CandidateProjectionId::new(),
            assembly_record,
            source_revision_id,
            source_blob_id,
            &source_bytes,
            target,
            &exact,
        )
        .expect("pure stale projection declaration");
        assert!(
            store
                .verify_and_record_candidate_projection(admitted_assembly, projection)
                .is_err(),
            "an archived call must not project onto a stale live revision"
        );
    }

    fn assert_stale_promotion_rejected(
        store: &mut ProjectStore,
        admitted: &AdmittedModelCall,
        admitted_assembly: &AdmittedCandidateAssembly,
        assembly_record: &CandidateAssemblyRecord,
    ) {
        let path = "manuscript/promotion-stale.md";
        store
            .save_document(
                path,
                DocumentContent::Prose("Current source. ".into()),
                "promotion source",
            )
            .expect("save promotion source");
        let source = store.read_document(path).expect("read promotion source");
        let target =
            loom_research_types::ByteRange::new(source.text.len() as u64, source.text.len() as u64)
                .expect("promotion append range");
        let exact = [ExactCallEvidence::new(
            &admitted.call,
            &admitted.raw_output,
            &admitted.token_ids,
        )];
        let projection = CandidateProjectionRecord::new(
            CandidateProjectionId::new(),
            assembly_record,
            source.revision_id,
            source.blob_id,
            source.text.as_bytes(),
            target,
            &exact,
        )
        .expect("promotion projection declaration");
        let admitted_projection = store
            .verify_and_record_candidate_projection(admitted_assembly, projection)
            .expect("admit projection while source is current");
        store
            .save_document(
                path,
                DocumentContent::Prose("A newer source revision. ".into()),
                "advance before promotion",
            )
            .expect("advance promotion source");
        let request = PromotionCommandRequest::new(
            store.manifest().project_id,
            admitted_projection.record.source_revision_id(),
            admitted_projection.record.source_blob_id(),
            PromotionSubject::CandidateProjection {
                projection_id: admitted_projection.record.id(),
            },
            admitted_projection.admission_record_id().as_blob_id(),
            admitted_projection.record.witness().resulting_blob_id(),
            admitted_projection.record.witness().resulting_byte_len(),
            CommandId::new(),
            now_unix_ms().max(1),
        )
        .expect("promotion request");
        assert!(
            store
                .record_promotion_command_request(
                    PromotionSubjectLease::CandidateProjection(&admitted_projection),
                    &request,
                )
                .is_err(),
            "promotion must fail after its source revision becomes stale"
        );
    }

    #[test]
    fn token_encoding_is_unambiguous_big_endian() {
        assert_eq!(
            encode_token_ids(&[0, 1, u32::MAX]),
            [0_u8; 4]
                .into_iter()
                .chain([0, 0, 0, 1])
                .chain([255; 4])
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn prompt_specification_size_validation_is_exact_at_both_boundaries() {
        assert!(validate_prompt_specification_size(1, 8).is_ok());
        assert!(validate_prompt_specification_size(8, 8).is_ok());
        assert!(validate_prompt_specification_size(0, 8).is_err());
        assert!(validate_prompt_specification_size(9, 8).is_err());
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn admission_capability_debug_exposes_only_safe_summaries() {
        const RAW_SECRET: &str = "RAW_SECRET_SENTINEL_/private/MODEL_PATH_SENTINEL.gguf";
        const SOURCE_SECRET: &str = "SOURCE_MANUSCRIPT_SECRET_SENTINEL";
        const ACTOR_SECRET: &str = "private actor sentinel";
        const TOKEN_SECRET: u32 = 987_654_321;
        const RAW_BYTE_PREFIX: &str = "82, 65, 87, 95";
        const RECEIPT_BYTE_PREFIX: &str = "222, 173, 190, 239";

        let directory = tempdir().expect("temporary project");
        let root = directory.path().join("Novel");
        let (store, _) = ProjectStore::initialize(&root, "Novel").expect("initialize");
        let session_nonce = store.session_nonce;

        let raw_output = RAW_SECRET.as_bytes().to_vec();
        let token_ids = vec![TOKEN_SECRET, 515_151_515];
        let event_blob_id = BlobId::digest(b"debug event evidence");
        let receipt_blob_id = BlobId::digest(b"debug receipt evidence");
        let completed = CompletedCall::new(
            &raw_output,
            &token_ids,
            event_blob_id,
            Some(receipt_blob_id),
        )
        .expect("completed call terminal");
        let call = ModelCall::new(
            ModelCallId::new(),
            adoption_identity(
                BlobId::digest(b"debug prompt"),
                CallScope::new(
                    CampaignId::new(),
                    StageId::new(),
                    StageAttemptId::new(),
                    TrialCaseId::new(),
                ),
                77,
            ),
            CallEvidenceClass::LiveBaseWriterClaim,
            CallTerminal::Completed(completed),
        )
        .expect("model call");
        let projection = OutputProjection::new(
            &raw_output,
            raw_output.len() as u64,
            raw_output.len() as u64,
        )
        .expect("full output projection");
        let span_record = GeneratedSpanOccurrenceRecord::from_declared_call(
            GeneratedSpanOccurrenceId::new(),
            &call,
            &raw_output,
            &token_ids,
            projection,
        )
        .expect("generated span");
        let evidence = [ExactCallEvidence::new(&call, &raw_output, &token_ids)];
        let assembly_record = CandidateAssemblyRecord::new(
            CandidateAssemblyId::new(),
            vec![AssemblyPartRecord::new(
                JoinBefore::None,
                span_record.clone(),
            )],
            &evidence,
        )
        .expect("candidate assembly");
        let source_bytes = SOURCE_SECRET.as_bytes();
        let source_blob_id = BlobId::digest(source_bytes);
        let source_revision_id = RevisionId::new();
        let target = loom_research_types::ByteRange::new(
            source_bytes.len() as u64,
            source_bytes.len() as u64,
        )
        .expect("source append range");
        let projection_record = CandidateProjectionRecord::new(
            CandidateProjectionId::new(),
            &assembly_record,
            source_revision_id,
            source_blob_id,
            source_bytes,
            target,
            &evidence,
        )
        .expect("candidate projection");

        let admitted_call = AdmittedModelCall {
            session_nonce,
            call: call.clone(),
            raw_output: raw_output.clone(),
            token_ids: token_ids.clone(),
            token_byte_boundaries: Some(vec![0, raw_output.len() as u64]),
            verification_fingerprint: BlobId::digest(b"admitted call verification"),
        };
        let admitted_span = AdmittedGeneratedSpan {
            session_nonce,
            record: span_record,
            call: call.clone(),
            raw_output: raw_output.clone(),
            token_ids: token_ids.clone(),
        };
        let owned_call = OwnedExactCall {
            call,
            raw_output: raw_output.clone(),
            token_ids,
        };
        let assembly_admission_record_id =
            admission_record_id("candidate_assembly", &assembly_record.id().to_string());
        let admitted_assembly = AdmittedCandidateAssembly {
            session_nonce,
            admission_record_id: assembly_admission_record_id,
            record: assembly_record,
            exact_calls: vec![owned_call],
        };
        let projection_admission_record_id =
            admission_record_id("candidate_projection", &projection_record.id().to_string());
        let admitted_projection = AdmittedCandidateProjection {
            session_nonce,
            admission_record_id: projection_admission_record_id,
            record: projection_record,
        };

        let mixed_output = b"MIXED_OUTPUT_SECRET_SENTINEL";
        let mixed_operation_id = PipelineOperationId::new();
        let mixed_graph = OperationGraph::new(
            vec![
                PipelineOperation::new(
                    mixed_operation_id,
                    PipelineOperationKind::LiteralText {
                        content_blob_id: BlobId::digest(mixed_output),
                    },
                    Vec::new(),
                )
                .expect("mixed operation"),
            ],
            mixed_operation_id,
        )
        .expect("mixed graph");
        let mixed_record = MixedAuthorshipAssemblyRecord::new(
            MixedAuthorshipAssemblyId::new(),
            mixed_output,
            mixed_graph,
        )
        .expect("mixed record");
        let mixed_admission_record_id =
            admission_record_id("mixed_authorship", &mixed_record.id().to_string());
        let mixed_admission = MixedAuthorshipAdmission {
            session_nonce,
            admission_record_id: mixed_admission_record_id,
            record: mixed_record,
        };

        let command_id = CommandId::new();
        let request_fingerprint = BlobId::digest(b"promotion request fingerprint");
        let recorded_request = RecordedPromotionRequest {
            session_nonce,
            command_id,
            request_fingerprint,
            recorded_at_ms: 123,
        };
        let event_receipt_bytes = vec![222, 173, 190, 239, 11, 22, 33, 44];
        let presence = VerifiedUserPresence {
            session_nonce,
            command_id,
            command_request_fingerprint: request_fingerprint,
            kind: UserPresenceKind::EditorGesture,
            session_fingerprint: BlobId::digest(b"private debug session"),
            event_receipt_blob_id: BlobId::digest(&event_receipt_bytes),
            event_receipt_bytes,
            monotonic_event_index: 999_999,
            occurred_at_ms: 456,
            actor: PromotionActor::new(ACTOR_SECRET).expect("actor"),
        };
        let recorded_authority = RecordedPromotionAuthority {
            session_nonce,
            command_id,
            record_blob_id: BlobId::digest(b"recorded authority"),
            source_revision_id,
            source_blob_id,
            intended_result_blob_id: BlobId::digest(b"intended result"),
            intended_result_byte_len: 42,
        };

        let debug_outputs = [
            ("admitted call", format!("{admitted_call:?}")),
            ("admitted span", format!("{admitted_span:?}")),
            (
                "owned exact call",
                format!("{:?}", admitted_assembly.exact_calls[0]),
            ),
            ("admitted assembly", format!("{admitted_assembly:?}")),
            ("admitted projection", format!("{admitted_projection:?}")),
            ("mixed admission", format!("{mixed_admission:?}")),
            ("verified presence", format!("{presence:?}")),
            ("recorded request", format!("{recorded_request:?}")),
            (
                "projection subject lease",
                format!(
                    "{:?}",
                    PromotionSubjectLease::CandidateProjection(&admitted_projection)
                ),
            ),
            (
                "mixed subject lease",
                format!(
                    "{:?}",
                    PromotionSubjectLease::MixedAuthorship(&mixed_admission)
                ),
            ),
            ("recorded authority", format!("{recorded_authority:?}")),
        ];
        let prohibited = [
            RAW_SECRET,
            SOURCE_SECRET,
            ACTOR_SECRET,
            "MIXED_OUTPUT_SECRET_SENTINEL",
            &TOKEN_SECRET.to_string(),
            RAW_BYTE_PREFIX,
            RECEIPT_BYTE_PREFIX,
            "StoreSessionNonce",
            "session_nonce",
            "event_receipt_bytes",
            "token_ids",
        ];
        for (label, output) in debug_outputs {
            for secret in prohibited {
                assert!(
                    !output.contains(secret),
                    "{label} Debug leaked prohibited value `{secret}`: {output}"
                );
            }
        }
    }

    #[test]
    fn diagnostic_only_batch_is_sealed_without_minting_a_call_lease() {
        let directory = tempdir().expect("temporary project");
        let root = directory.path().join("Novel");
        let (mut store, _) = ProjectStore::initialize(&root, "Novel").expect("initialize");
        let prompt = store_prompt_fixture(&mut store);
        let cases = [strict_case_spec(0, ModelCallId::new(), false)];
        let batch =
            strict_batch_adoption_fixture(&store, test_base_writer_binding(), prompt, &cases);

        let adopted = adopt_strict_test_batch(&mut store, batch);
        assert!(adopted.admitted_calls().is_empty());
        assert_eq!(adopted.cancelled_call_count(), 1);
        assert_eq!(research_adoption_counts(&store), (1, 1, 1, 1, 1));
        let seal: (i64, i64) = store
            .connection
            .query_row(
                "SELECT completed_call_count, cancelled_call_count
                 FROM research_verified_inference_batch_seals",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("diagnostic seal");
        assert_eq!(seal, (0, 1));
    }

    #[test]
    fn frozen_prompt_can_archive_after_edit_but_stale_projection_and_promotion_fail() {
        let directory = tempdir().expect("temporary project");
        let root = directory.path().join("Novel");
        let (mut store, _) = ProjectStore::initialize(&root, "Novel").expect("initialize");
        let prompt = store_prompt_fixture(&mut store);
        let compiled = compile_store_prompt_fixture(&prompt);
        let source_revision_id = compiled
            .specification()
            .tail()
            .source_revision_id()
            .expect("live prompt revision");
        let source_blob_id = compiled.specification().tail().source_blob_id();
        let source_path = document_path_for_revision(&store, source_revision_id);
        let lease = store
            .verify_compiled_prompt_for_inference(&compiled)
            .expect("freeze current prompt source");
        let cases = [strict_case_spec(0, ModelCallId::new(), true)];
        let mut batch =
            strict_batch_adoption_fixture(&store, test_base_writer_binding(), prompt, &cases);

        store
            .save_document(
                &source_path,
                DocumentContent::Prose(
                    "CHAPTER ONE\n\nThe rain stopped at the gate. Then the bell rang.".into(),
                ),
                "edit after inference freeze",
            )
            .expect("advance live manuscript after freeze");
        consume_prompt_source_lease(&store, &mut batch, lease)
            .expect("outcome still matches pre-inference freeze");
        let adopted = store
            .adopt_verified_material_with_freeze(batch)
            .expect("archive exact generated evidence after source advanced");
        let replayed = store
            .replay_inference_batch_evidence(adopted.verification_fingerprint())
            .expect("strictly replay frozen evidence after source advanced");
        assert_eq!(replayed.completed_call_count(), 1);
        assert_eq!(replayed.cancelled_call_count(), 0);
        let admitted = &adopted.admitted_calls()[0];
        let (admitted_assembly, assembly_record) = admit_single_call_assembly(&mut store, admitted);
        assert_stale_projection_rejected(
            &mut store,
            admitted,
            &admitted_assembly,
            &assembly_record,
            source_revision_id,
            source_blob_id,
        );
        assert_stale_promotion_rejected(&mut store, admitted, &admitted_assembly, &assembly_record);
    }

    #[test]
    fn prompt_source_leases_reject_wrong_prompt_and_cross_session_use() {
        let directory = tempdir().expect("temporary projects");
        let first_root = directory.path().join("First");
        let second_root = directory.path().join("Second");
        let (mut first, _) = ProjectStore::initialize(&first_root, "First").expect("first store");
        let (second, _) = ProjectStore::initialize(&second_root, "Second").expect("second store");
        let first_prompt = store_prompt_fixture(&mut first);
        let compiled = compile_store_prompt_fixture(&first_prompt);
        let wrong_prompt_lease = first
            .verify_compiled_prompt_for_inference(&compiled)
            .expect("first prompt lease");
        let cross_session_lease = first
            .verify_compiled_prompt_for_inference(&compiled)
            .expect("second one-use lease for cross-session attack");

        let wrong_prompt = store_prompt_fixture(&mut first);
        let wrong_cancelled =
            untrusted_cancelled_adoption_fixture(0, ModelCallId::new(), &wrong_prompt);
        let mut wrong_batch = untrusted_batch_adoption_fixture(
            &first,
            "wrong-prompt-lease",
            BatchAdoptionKind::DiagnosticOnly,
            wrong_prompt,
            vec![CaseAdoptionMaterial::Cancelled(wrong_cancelled)],
        );
        assert!(
            consume_prompt_source_lease(&first, &mut wrong_batch, wrong_prompt_lease).is_err(),
            "a lease must bind the exact prompt and source graph"
        );

        let first_cancelled =
            untrusted_cancelled_adoption_fixture(0, ModelCallId::new(), &first_prompt);
        let mut first_batch = untrusted_batch_adoption_fixture(
            &first,
            "cross-session-lease",
            BatchAdoptionKind::DiagnosticOnly,
            first_prompt,
            vec![CaseAdoptionMaterial::Cancelled(first_cancelled)],
        );
        assert!(
            consume_prompt_source_lease(&second, &mut first_batch, cross_session_lease).is_err(),
            "a prompt-source lease must not cross store sessions"
        );
    }

    #[test]
    fn mixed_batch_returns_only_completed_call_leases() {
        let directory = tempdir().expect("temporary project");
        let root = directory.path().join("Novel");
        let (mut store, _) = ProjectStore::initialize(&root, "Novel").expect("initialize");
        let prompt = store_prompt_fixture(&mut store);
        let completed_id = ModelCallId::new();
        let cases = [
            strict_case_spec(0, completed_id, true),
            strict_case_spec(1, ModelCallId::new(), false),
        ];
        let batch =
            strict_batch_adoption_fixture(&store, test_base_writer_binding(), prompt, &cases);

        let adopted = adopt_strict_test_batch(&mut store, batch);
        let batch_fingerprint = adopted.verification_fingerprint();
        assert_eq!(adopted.admitted_calls().len(), 1);
        assert_eq!(adopted.admitted_calls()[0].call_id(), completed_id);
        assert_eq!(adopted.cancelled_call_count(), 1);
        assert_eq!(research_adoption_counts(&store), (1, 2, 2, 1, 1));
        let null_audit_count: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM research_model_calls
                 WHERE verification_audit_fingerprint IS NULL",
                [],
                |row| row.get(0),
            )
            .expect("null audit count");
        assert_eq!(
            null_audit_count, 1,
            "only cancellation lacks live authority"
        );
        let replayed = store
            .replay_inference_batch_evidence(batch_fingerprint)
            .expect("replay sealed batch as diagnostic evidence");
        assert_eq!(replayed.batch_fingerprint(), batch_fingerprint);
        assert_eq!(replayed.completed_call_count(), 1);
        assert_eq!(replayed.cancelled_call_count(), 1);
    }

    #[test]
    fn semantic_binding_identity_preserves_distinct_exact_toml_sources() {
        let directory = tempdir().expect("temporary project");
        let root = directory.path().join("Novel");
        let (mut store, _) = ProjectStore::initialize(&root, "Novel").expect("initialize");
        let plain = test_base_writer_binding_source("");
        let commented = test_base_writer_binding_source(
            "# Operational comment retained only in the exact source occurrence.\n\n",
        );
        assert_eq!(plain.fingerprint(), commented.fingerprint());
        assert_eq!(
            plain.manifest_fingerprint(),
            commented.manifest_fingerprint()
        );
        assert_ne!(
            plain.manifest_source_hash(),
            commented.manifest_source_hash()
        );

        for (position, binding) in [plain, commented].into_iter().enumerate() {
            let prompt = store_prompt_fixture(&mut store);
            let cases = [strict_case_spec(position, ModelCallId::new(), false)];
            let batch = strict_batch_adoption_fixture(&store, binding, prompt, &cases);
            adopt_strict_test_batch(&mut store, batch);
        }

        let counts: (i64, i64) = store
            .connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM research_model_bindings),
                    (SELECT COUNT(*) FROM research_model_binding_sources)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("binding identity counts");
        assert_eq!(counts, (1, 2));
    }

    #[test]
    fn replay_detects_sampler_fingerprint_row_corruption() {
        let directory = tempdir().expect("temporary project");
        let root = directory.path().join("Novel");
        let (mut store, _) = ProjectStore::initialize(&root, "Novel").expect("initialize");
        let prompt = store_prompt_fixture(&mut store);
        let cases = [strict_case_spec(0, ModelCallId::new(), true)];
        let batch =
            strict_batch_adoption_fixture(&store, test_base_writer_binding(), prompt, &cases);
        let batch_fingerprint =
            adopt_strict_test_batch(&mut store, batch).verification_fingerprint();
        store
            .connection
            .execute_batch("DROP TRIGGER research_model_calls_immutable_update")
            .expect("simulate corruption below immutable boundary");
        store
            .connection
            .execute(
                "UPDATE research_model_calls SET sampler_fingerprint = ?1",
                [BlobId::digest(b"substituted stored sampler").to_string()],
            )
            .expect("inject sampler-row corruption");

        assert!(
            store
                .replay_inference_batch_evidence(batch_fingerprint)
                .is_err(),
            "diagnostic replay must compare SQL sampler identity to the canonical call record"
        );
    }

    #[test]
    fn replay_detects_prompt_freeze_fingerprint_corruption() {
        let directory = tempdir().expect("temporary project");
        let root = directory.path().join("Novel");
        let (mut store, _) = ProjectStore::initialize(&root, "Novel").expect("initialize");
        let prompt = store_prompt_fixture(&mut store);
        let cases = [strict_case_spec(0, ModelCallId::new(), true)];
        let batch =
            strict_batch_adoption_fixture(&store, test_base_writer_binding(), prompt, &cases);
        let batch_fingerprint =
            adopt_strict_test_batch(&mut store, batch).verification_fingerprint();
        store
            .connection
            .execute_batch("DROP TRIGGER research_verified_batches_immutable_update")
            .expect("simulate corruption below immutable boundary");
        store
            .connection
            .execute(
                "UPDATE research_verified_inference_batches
                 SET prompt_freeze_fingerprint = ?1",
                [BlobId::digest(b"substituted prompt freeze").to_string()],
            )
            .expect("inject prompt-freeze corruption");

        assert!(
            store
                .replay_inference_batch_evidence(batch_fingerprint)
                .is_err(),
            "diagnostic replay must recompute the exact pre-inference source freeze"
        );
    }

    #[test]
    fn replay_detects_prompt_content_fingerprint_corruption() {
        let directory = tempdir().expect("temporary project");
        let root = directory.path().join("Novel");
        let (mut store, _) = ProjectStore::initialize(&root, "Novel").expect("initialize");
        let prompt = store_prompt_fixture(&mut store);
        let cases = [strict_case_spec(0, ModelCallId::new(), true)];
        let batch =
            strict_batch_adoption_fixture(&store, test_base_writer_binding(), prompt, &cases);
        let batch_fingerprint =
            adopt_strict_test_batch(&mut store, batch).verification_fingerprint();
        store
            .connection
            .execute_batch("DROP TRIGGER research_verified_batches_immutable_update")
            .expect("simulate corruption below immutable boundary");
        store
            .connection
            .execute(
                "UPDATE research_verified_inference_batches
                 SET prompt_content_fingerprint = ?1",
                [BlobId::digest(b"substituted prompt content").to_string()],
            )
            .expect("inject prompt-content corruption");

        assert!(
            store
                .replay_inference_batch_evidence(batch_fingerprint)
                .is_err(),
            "diagnostic replay must recompute attempt-independent prompt content identity"
        );
    }

    #[test]
    fn reopened_self_attested_scope_replays_only_as_diagnostic_evidence() {
        let directory = tempdir().expect("temporary project");
        let root = directory.path().join("Novel");
        let batch_fingerprint = {
            let (mut store, _) = ProjectStore::initialize(&root, "Novel").expect("initialize");
            let prompt = store_prompt_fixture(&mut store);
            let cases = [strict_case_spec(0, ModelCallId::new(), false)];
            let batch =
                strict_batch_adoption_fixture(&store, test_base_writer_binding(), prompt, &cases);
            adopt_strict_test_batch(&mut store, batch).verification_fingerprint()
        };

        let reopened = ProjectStore::open(&root).expect("reopen project");
        let replayed = reopened
            .replay_inference_batch_evidence(batch_fingerprint)
            .expect("replay immutable diagnostic evidence");
        assert_eq!(replayed.completed_call_count(), 0);
        assert_eq!(replayed.cancelled_call_count(), 1);
        let authority_rows: i64 = reopened
            .connection
            .query_row(
                "SELECT COUNT(*) FROM research_admission_records",
                [],
                |row| row.get(0),
            )
            .expect("read strict admission rows");
        assert_eq!(
            authority_rows, 0,
            "diagnostic replay must not mint an admission or benchmark authority row"
        );
    }

    #[test]
    fn prompt_fingerprint_substitution_fails_before_semantic_persistence() {
        let directory = tempdir().expect("temporary project");
        let root = directory.path().join("Novel");
        let (mut store, _) = ProjectStore::initialize(&root, "Novel").expect("initialize");

        for substitution in ["raw", "content", "tokens", "compiled"] {
            let mut prompt = store_prompt_fixture(&mut store);
            match substitution {
                "raw" => prompt.raw_blob_id = BlobId::digest(b"substituted raw prompt"),
                "content" => {
                    prompt.content_fingerprint = BlobId::digest(b"substituted prompt content");
                }
                "tokens" => prompt.token_fingerprint = BlobId::digest(b"substituted prompt tokens"),
                "compiled" => {
                    prompt.compiled_fingerprint = BlobId::digest(b"substituted compiled prompt");
                }
                _ => unreachable!(),
            }
            let cancelled = untrusted_cancelled_adoption_fixture(0, ModelCallId::new(), &prompt);
            let batch = untrusted_batch_adoption_fixture(
                &store,
                substitution,
                BatchAdoptionKind::DiagnosticOnly,
                prompt,
                vec![CaseAdoptionMaterial::Cancelled(cancelled)],
            );
            assert!(
                store.preflight_untrusted_test_material(batch).is_err(),
                "{substitution} substitution must fail closed"
            );
        }
        assert_eq!(research_adoption_counts(&store), (0, 0, 0, 0, 0));
    }

    #[test]
    fn transformed_prompt_witness_cannot_enter_unverified_store_lane() {
        let directory = tempdir().expect("temporary project");
        let root = directory.path().join("Novel");
        let (mut store, _) = ProjectStore::initialize(&root, "Novel").expect("initialize");
        let mut prompt = store_prompt_fixture(&mut store);
        let source_bytes = store
            .read_blob(prompt.source_tail_blob_id)
            .expect("read transformation source");
        let source_revision_id = prompt
            .source_tail_revision_id
            .as_deref()
            .expect("live source revision")
            .parse::<RevisionId>()
            .expect("valid source revision");
        let source = loom_research_types::PromptSourceRange::new(
            source_revision_id,
            prompt.source_tail_blob_id,
            prompt.source_tail_range,
        )
        .expect("bounded transformation source");
        let rendered = b"A transformed demonstration block.\n\n";
        let witness = loom_research_types::PromptBlockWitness::transformation(
            vec![source],
            BlobId::digest(b"unadmitted transformation recipe"),
            BlobId::digest(b"unadmitted controller receipt"),
            BlobId::digest(rendered),
        )
        .expect("well-formed transformation witness");
        let block = loom_research_types::FrozenCompletionPromptBlock::new(
            loom_research_types::CompletionPromptBlockRole::OperatorDemonstration,
            loom_research_types::ExactPromptBlockBytes::new(rendered.to_vec())
                .expect("rendered block"),
            witness,
        )
        .expect("fingerprint-bound transformed block");
        let original: FrozenBaseCompletionPrompt =
            serde_json::from_slice(&prompt.specification_bytes).expect("original prompt spec");
        let specification = FrozenBaseCompletionPrompt::new(
            prompt.project_id,
            prompt.scope,
            prompt.treatment_recipe_fingerprint,
            vec![block],
            original.tail(),
        )
        .expect("transformed prompt specification");
        let mut raw_utf8 = rendered.to_vec();
        raw_utf8.extend_from_slice(&source_bytes);
        let compiled = specification
            .clone()
            .compile(
                raw_utf8.clone(),
                &[loom_research_types::ExactPromptSource::new(
                    source_revision_id,
                    &source_bytes,
                )],
            )
            .expect("compile source-grounded transformed prompt");
        prompt.specification_bytes =
            serde_json::to_vec(&specification).expect("transformed prompt spec JSON");
        prompt.specification_blob_id = BlobId::digest(&prompt.specification_bytes);
        prompt.source_prompt_fingerprint = compiled.fingerprint();
        prompt.tail_prompt_range = compiled.tail_prompt_range();
        prompt.raw_blob_id = BlobId::digest(&raw_utf8);
        prompt.raw_utf8 = raw_utf8;
        prompt.compiled_fingerprint = compiled_prompt_fingerprint(
            prompt.source_prompt_fingerprint,
            prompt.raw_blob_id,
            prompt.form,
            prompt.token_policy,
            &prompt.ordered_token_ids,
        );
        let cancelled = untrusted_cancelled_adoption_fixture(0, ModelCallId::new(), &prompt);
        let batch = untrusted_batch_adoption_fixture(
            &store,
            "unverified-transformation",
            BatchAdoptionKind::DiagnosticOnly,
            prompt,
            vec![CaseAdoptionMaterial::Cancelled(cancelled)],
        );

        let error = store
            .preflight_untrusted_test_material(batch)
            .expect_err("transformation without a store-owned controller lease must fail");
        assert!(
            error
                .to_string()
                .contains("persisted verified controller receipt")
        );
        assert_eq!(research_adoption_counts(&store), (0, 0, 0, 0, 0));
    }

    #[test]
    fn cancelled_partial_output_over_sixteen_mib_is_rejected() {
        let directory = tempdir().expect("temporary project");
        let root = directory.path().join("Novel");
        let (mut store, _) = ProjectStore::initialize(&root, "Novel").expect("initialize");
        let prompt = store_prompt_fixture(&mut store);
        let mut cancelled = untrusted_cancelled_adoption_fixture(0, ModelCallId::new(), &prompt);
        let oversized = usize::try_from(MAX_RAW_OUTPUT_BYTES).expect("test platform size") + 1;
        cancelled.partial_raw_output = vec![b'x'; oversized];
        let batch = untrusted_batch_adoption_fixture(
            &store,
            "oversized-cancellation",
            BatchAdoptionKind::DiagnosticOnly,
            prompt,
            vec![CaseAdoptionMaterial::Cancelled(cancelled)],
        );

        assert!(store.preflight_untrusted_test_material(batch).is_err());
        assert_eq!(research_adoption_counts(&store), (0, 0, 0, 0, 0));
    }

    #[test]
    fn duplicate_persisted_call_rolls_back_the_new_batch_header() {
        let directory = tempdir().expect("temporary project");
        let root = directory.path().join("Novel");
        let (mut store, _) = ProjectStore::initialize(&root, "Novel").expect("initialize");
        let call_id = ModelCallId::new();

        let first_prompt = store_prompt_fixture(&mut store);
        let first_cases = [strict_case_spec(0, call_id, true)];
        let first_batch = strict_batch_adoption_fixture(
            &store,
            test_base_writer_binding(),
            first_prompt,
            &first_cases,
        );
        adopt_strict_test_batch(&mut store, first_batch);
        assert_eq!(research_adoption_counts(&store), (1, 1, 1, 0, 1));

        let second_prompt = store_prompt_fixture(&mut store);
        let second_cases = [strict_case_spec(0, call_id, true)];
        let mut second_batch = strict_batch_adoption_fixture(
            &store,
            test_base_writer_binding(),
            second_prompt,
            &second_cases,
        );
        let second_fingerprint = second_batch.verification_fingerprint;
        let second_compiled = compile_store_prompt_fixture(&second_batch.prompt_evidence);
        let second_lease = store
            .verify_compiled_prompt_for_inference(&second_compiled)
            .expect("freeze duplicate-call prompt");
        consume_prompt_source_lease(&store, &mut second_batch, second_lease)
            .expect("bind duplicate-call prompt lease");
        assert!(
            store
                .adopt_verified_material_with_freeze(second_batch)
                .is_err()
        );
        assert_eq!(research_adoption_counts(&store), (1, 1, 1, 0, 1));
        let rolled_back_header: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM research_verified_inference_batches
                 WHERE batch_verification_fingerprint = ?1",
                [second_fingerprint.to_string()],
                |row| row.get(0),
            )
            .expect("rolled-back batch header count");
        assert_eq!(rolled_back_header, 0);
    }

    #[test]
    fn admission_record_ids_are_domain_separated() {
        assert_ne!(
            admission_record_id("candidate_assembly", "same"),
            admission_record_id("candidate_projection", "same")
        );
    }

    #[test]
    fn promotion_capabilities_expire_when_the_project_store_reopens() {
        let directory = tempdir().expect("temporary project");
        let root = directory.path().join("Novel");
        let (mut store, _) = ProjectStore::initialize(&root, "Novel").expect("initialize");
        let fixture = mixed_promotion_fixture(&mut store);
        let request = promotion_request(&store, &fixture, CommandId::new());
        let recorded_request = store
            .record_promotion_command_request(
                PromotionSubjectLease::MixedAuthorship(&fixture.admission),
                &request,
            )
            .expect("record promotion request");
        let (authority, presence) = authority_and_presence(
            &store,
            &recorded_request,
            &request,
            PresenceSpec {
                authority_actor: "foreground reviewer",
                host_actor: "foreground reviewer",
                session_fingerprint: BlobId::digest(b"reopen host session"),
                monotonic_event_index: 1,
                event_receipt_bytes: b"reopen foreground gesture",
            },
        );
        drop(store);

        let mut reopened = ProjectStore::open(&root).expect("reopen project");
        assert!(
            reopened
                .record_promotion_authority(
                    recorded_request,
                    PromotionSubjectLease::MixedAuthorship(&fixture.admission),
                    presence,
                    &authority,
                )
                .is_err(),
            "a prior-open request, admission, and presence must all be stale"
        );
        let authority_count: i64 = reopened
            .connection
            .query_row(
                "SELECT COUNT(*) FROM research_promotion_authorities",
                [],
                |row| row.get(0),
            )
            .expect("authority count");
        assert_eq!(authority_count, 0);
    }

    #[test]
    fn admission_capabilities_do_not_cross_projects_or_copied_stores() {
        let directory = tempdir().expect("temporary project");
        let source_root = directory.path().join("Source");
        let other_root = directory.path().join("Other");
        let copied_root = directory.path().join("Copied");
        let (mut source_store, _) =
            ProjectStore::initialize(&source_root, "Source").expect("initialize source");
        let fixture = mixed_promotion_fixture(&mut source_store);
        let request = promotion_request(&source_store, &fixture, CommandId::new());

        let (mut other_store, _) =
            ProjectStore::initialize(&other_root, "Other").expect("initialize other");
        assert!(
            other_store
                .record_promotion_command_request(
                    PromotionSubjectLease::MixedAuthorship(&fixture.admission),
                    &request,
                )
                .is_err(),
            "a capability from another project must fail"
        );
        drop(other_store);
        drop(source_store);

        copy_tree(&source_root, &copied_root);
        let mut copied_store = ProjectStore::open(&copied_root).expect("open copied project");
        assert!(
            copied_store
                .record_promotion_command_request(
                    PromotionSubjectLease::MixedAuthorship(&fixture.admission),
                    &request,
                )
                .is_err(),
            "copied audit rows must not recreate runtime admission authority"
        );
    }

    #[test]
    fn request_fingerprint_and_host_actor_substitution_fail_closed() {
        let directory = tempdir().expect("temporary project");
        let root = directory.path().join("Novel");
        let (mut store, _) = ProjectStore::initialize(&root, "Novel").expect("initialize");
        let fixture = mixed_promotion_fixture(&mut store);

        let fingerprint_request = promotion_request(&store, &fixture, CommandId::new());
        let mut recorded_request = store
            .record_promotion_command_request(
                PromotionSubjectLease::MixedAuthorship(&fixture.admission),
                &fingerprint_request,
            )
            .expect("record fingerprint request");
        let (fingerprint_authority, fingerprint_presence) = authority_and_presence(
            &store,
            &recorded_request,
            &fingerprint_request,
            PresenceSpec {
                authority_actor: "foreground reviewer",
                host_actor: "foreground reviewer",
                session_fingerprint: BlobId::digest(b"fingerprint session"),
                monotonic_event_index: 1,
                event_receipt_bytes: b"fingerprint gesture",
            },
        );
        recorded_request.request_fingerprint = BlobId::digest(b"substituted request fingerprint");
        assert!(
            store
                .record_promotion_authority(
                    recorded_request,
                    PromotionSubjectLease::MixedAuthorship(&fixture.admission),
                    fingerprint_presence,
                    &fingerprint_authority,
                )
                .is_err(),
            "a substituted recorded-request fingerprint must fail"
        );

        let actor_request = promotion_request(&store, &fixture, CommandId::new());
        let actor_recorded = store
            .record_promotion_command_request(
                PromotionSubjectLease::MixedAuthorship(&fixture.admission),
                &actor_request,
            )
            .expect("record actor request");
        let (actor_authority, actor_presence) = authority_and_presence(
            &store,
            &actor_recorded,
            &actor_request,
            PresenceSpec {
                authority_actor: "caller-supplied actor",
                host_actor: "host-derived actor",
                session_fingerprint: BlobId::digest(b"actor session"),
                monotonic_event_index: 1,
                event_receipt_bytes: b"actor gesture",
            },
        );
        assert!(
            store
                .record_promotion_authority(
                    actor_recorded,
                    PromotionSubjectLease::MixedAuthorship(&fixture.admission),
                    actor_presence,
                    &actor_authority,
                )
                .is_err(),
            "the serialized authority actor must equal the host-owned lease actor"
        );
        let authority_count: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM research_promotion_authorities",
                [],
                |row| row.get(0),
            )
            .expect("authority count");
        assert_eq!(authority_count, 0);
    }

    #[test]
    fn retrospective_terminal_receipt_blocks_presence_and_authority_atomically() {
        let directory = tempdir().expect("temporary project");
        let root = directory.path().join("Novel");
        let (mut store, _) = ProjectStore::initialize(&root, "Novel").expect("initialize");
        let fixture = mixed_promotion_fixture(&mut store);
        let request = promotion_request(&store, &fixture, CommandId::new());
        let recorded_request = store
            .record_promotion_command_request(
                PromotionSubjectLease::MixedAuthorship(&fixture.admission),
                &request,
            )
            .expect("record promotion request");
        let (authority, presence) = authority_and_presence(
            &store,
            &recorded_request,
            &request,
            PresenceSpec {
                authority_actor: "foreground reviewer",
                host_actor: "foreground reviewer",
                session_fingerprint: BlobId::digest(b"receipt-race session"),
                monotonic_event_index: 1,
                event_receipt_bytes: b"receipt-race gesture",
            },
        );
        store
            .connection
            .execute(
                "INSERT INTO command_receipts(
                    command_id, command_kind, receipt_json, completed_at_ms
                 ) VALUES (?1, 'promotion-test-terminal', '{}', ?2)",
                params![request.command_id().to_string(), presence.occurred_at_ms],
            )
            .expect("insert retrospective terminal receipt");

        assert!(
            store
                .record_promotion_authority(
                    recorded_request,
                    PromotionSubjectLease::MixedAuthorship(&fixture.admission),
                    presence,
                    &authority,
                )
                .is_err(),
            "a terminal receipt inserted after request recording must close admission"
        );
        let counts: (i64, i64) = store
            .connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM research_user_presence_events),
                    (SELECT COUNT(*) FROM research_promotion_authorities)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("promotion intent counts");
        assert_eq!(counts, (0, 0));
    }

    #[test]
    fn promotion_request_and_presence_capabilities_are_single_use() {
        let directory = tempdir().expect("temporary project");
        let root = directory.path().join("Novel");
        let (mut store, _) = ProjectStore::initialize(&root, "Novel").expect("initialize");
        let fixture = mixed_promotion_fixture(&mut store);
        let request = promotion_request(&store, &fixture, CommandId::new());
        let recorded_request = store
            .record_promotion_command_request(
                PromotionSubjectLease::MixedAuthorship(&fixture.admission),
                &request,
            )
            .expect("record promotion request");
        let duplicate_request = RecordedPromotionRequest {
            session_nonce: recorded_request.session_nonce,
            command_id: recorded_request.command_id,
            request_fingerprint: recorded_request.request_fingerprint,
            recorded_at_ms: recorded_request.recorded_at_ms,
        };
        let (authority, presence) = authority_and_presence(
            &store,
            &recorded_request,
            &request,
            PresenceSpec {
                authority_actor: "foreground reviewer",
                host_actor: "foreground reviewer",
                session_fingerprint: BlobId::digest(b"single-use session"),
                monotonic_event_index: 1,
                event_receipt_bytes: b"single-use gesture",
            },
        );
        let duplicate_presence = VerifiedUserPresence {
            session_nonce: presence.session_nonce,
            command_id: presence.command_id,
            command_request_fingerprint: presence.command_request_fingerprint,
            kind: presence.kind,
            session_fingerprint: presence.session_fingerprint,
            event_receipt_blob_id: presence.event_receipt_blob_id,
            event_receipt_bytes: presence.event_receipt_bytes.clone(),
            monotonic_event_index: presence.monotonic_event_index,
            occurred_at_ms: presence.occurred_at_ms,
            actor: presence.actor.clone(),
        };

        store
            .record_promotion_authority(
                recorded_request,
                PromotionSubjectLease::MixedAuthorship(&fixture.admission),
                presence,
                &authority,
            )
            .expect("first authority admission");
        assert!(
            store
                .record_promotion_authority(
                    duplicate_request,
                    PromotionSubjectLease::MixedAuthorship(&fixture.admission),
                    duplicate_presence,
                    &authority,
                )
                .is_err(),
            "even an internal duplicate of consumed tokens must not double-spend"
        );
        let counts: (i64, i64) = store
            .connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM research_user_presence_events),
                    (SELECT COUNT(*) FROM research_promotion_authorities)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("promotion intent counts");
        assert_eq!(counts, (1, 1));
    }

    #[test]
    fn presence_event_indexes_are_strictly_monotonic_per_host_session() {
        let directory = tempdir().expect("temporary project");
        let root = directory.path().join("Novel");
        let (mut store, _) = ProjectStore::initialize(&root, "Novel").expect("initialize");
        let fixture = mixed_promotion_fixture(&mut store);
        let session_fingerprint = BlobId::digest(b"monotonic host session");

        let first_request = promotion_request(&store, &fixture, CommandId::new());
        let first_recorded = store
            .record_promotion_command_request(
                PromotionSubjectLease::MixedAuthorship(&fixture.admission),
                &first_request,
            )
            .expect("record first request");
        let (first_authority, first_presence) = authority_and_presence(
            &store,
            &first_recorded,
            &first_request,
            PresenceSpec {
                authority_actor: "foreground reviewer",
                host_actor: "foreground reviewer",
                session_fingerprint,
                monotonic_event_index: 2,
                event_receipt_bytes: b"monotonic gesture two",
            },
        );
        store
            .record_promotion_authority(
                first_recorded,
                PromotionSubjectLease::MixedAuthorship(&fixture.admission),
                first_presence,
                &first_authority,
            )
            .expect("record index two");

        let second_request = promotion_request(&store, &fixture, CommandId::new());
        let second_recorded = store
            .record_promotion_command_request(
                PromotionSubjectLease::MixedAuthorship(&fixture.admission),
                &second_request,
            )
            .expect("record second request");
        let (second_authority, second_presence) = authority_and_presence(
            &store,
            &second_recorded,
            &second_request,
            PresenceSpec {
                authority_actor: "foreground reviewer",
                host_actor: "foreground reviewer",
                session_fingerprint,
                monotonic_event_index: 1,
                event_receipt_bytes: b"monotonic gesture one",
            },
        );
        assert!(
            store
                .record_promotion_authority(
                    second_recorded,
                    PromotionSubjectLease::MixedAuthorship(&fixture.admission),
                    second_presence,
                    &second_authority,
                )
                .is_err(),
            "a lower index in the same host session must never be accepted later"
        );
        let counts: (i64, i64) = store
            .connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM research_user_presence_events),
                    (SELECT COUNT(*) FROM research_promotion_authorities)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("promotion intent counts");
        assert_eq!(counts, (1, 1));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn test_only_syntax_replay_binds_every_call_fact_and_one_terminal() {
        let directory = tempdir().expect("temporary project");
        let root = directory.path().join("Novel");
        let (mut store, _) = ProjectStore::initialize(&root, "Novel").expect("initialize");
        let call_id = ModelCallId::new();
        let scope = CallScope::new(
            CampaignId::new(),
            StageId::new(),
            StageAttemptId::new(),
            TrialCaseId::new(),
        );
        let identity = CallIdentity::new(
            scope,
            BlobId::digest(b"model"),
            BlobId::digest(b"tokenizer"),
            BlobId::digest(b"prompt"),
            BlobId::digest(b"sampler"),
            BlobId::digest(b"control"),
            7,
        );
        let raw_output = b"hello".to_vec();
        let token_ids = vec![7_u32];
        let provisional_terminal = CompletedCall::new(
            &raw_output,
            &token_ids,
            BlobId::digest(b"provisional-events"),
            Some(BlobId::digest(b"provisional-receipt")),
        )
        .expect("provisional terminal");
        let provisional_call = ModelCall::new(
            call_id,
            identity.clone(),
            CallEvidenceClass::LiveBaseWriterClaim,
            CallTerminal::Completed(provisional_terminal.clone()),
        )
        .expect("provisional call");
        let raw_event_stream = serde_json::to_vec(&serde_json::json!({
            "format": STRICT_EVENT_STREAM_FORMAT,
            "call_id": call_id,
            "events": [
                {
                    "sequence": 0,
                    "occurred_at_ms": 10,
                    "kind": "call_started",
                    "evidence_fingerprint": call_start_fingerprint(&provisional_call),
                },
                {
                    "sequence": 1,
                    "occurred_at_ms": 20,
                    "kind": "call_completed",
                    "evidence_fingerprint": completed_call_fingerprint(&provisional_terminal),
                }
            ]
        }))
        .expect("event JSON");
        let token_evidence = TokenEvidence::from_exact(&token_ids).expect("token evidence");
        let backend_receipt = serde_json::to_vec(&serde_json::json!({
            "format": STRICT_RECEIPT_FORMAT,
            "call_id": call_id,
            "evidence_class": "live_base_writer_claim",
            "scope": scope,
            "seed": 7,
            "model_fingerprint": identity.model_fingerprint(),
            "tokenizer_fingerprint": identity.tokenizer_fingerprint(),
            "prompt_fingerprint": identity.prompt_fingerprint(),
            "sampler_fingerprint": identity.sampler_fingerprint(),
            "control_program_fingerprint": identity.control_program_fingerprint(),
            "raw_output_blob_id": BlobId::digest(&raw_output),
            "raw_output_byte_len": raw_output.len(),
            "token_count": token_evidence.token_count(),
            "token_ids_fingerprint": token_evidence.token_ids_fingerprint(),
            "raw_event_stream_blob_id": BlobId::digest(&raw_event_stream),
            "execution_instance_fingerprint": BlobId::digest(b"test-only-instance"),
            "token_byte_boundaries": null,
            "started_at_ms": 10,
            "completed_at_ms": 20
        }))
        .expect("receipt JSON");
        let completed = CompletedCall::new(
            &raw_output,
            &token_ids,
            BlobId::digest(&raw_event_stream),
            Some(BlobId::digest(&backend_receipt)),
        )
        .expect("completed call");
        let call = ModelCall::new(
            call_id,
            identity,
            CallEvidenceClass::LiveBaseWriterClaim,
            CallTerminal::Completed(completed),
        )
        .expect("model call");

        let admitted = store
            .verify_and_record_base_writer_call(
                call,
                raw_output,
                token_ids,
                &raw_event_stream,
                &backend_receipt,
            )
            .expect("test-only syntax replay");
        assert_eq!(admitted.call_id(), call_id);
        let terminal_count: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM research_call_terminals WHERE call_id = ?1",
                [call_id.to_string()],
                |row| row.get(0),
            )
            .expect("terminal count");
        assert_eq!(terminal_count, 1);

        store
            .save_document(
                "manuscript/001.md",
                DocumentContent::Prose("Start ".into()),
                "projection source",
            )
            .expect("source document");
        let source = store
            .read_document("manuscript/001.md")
            .expect("load source");
        let output_projection =
            OutputProjection::new(&admitted.raw_output, 5, 5).expect("exact output projection");
        let span_record = GeneratedSpanOccurrenceRecord::from_declared_call(
            GeneratedSpanOccurrenceId::new(),
            &admitted.call,
            &admitted.raw_output,
            &admitted.token_ids,
            output_projection,
        )
        .expect("span declaration");
        let assembly_span = span_record.clone();
        let admitted_span = store
            .verify_and_record_generated_span(&admitted, span_record)
            .expect("admit exact span");
        let exact = [ExactCallEvidence::new(
            &admitted.call,
            &admitted.raw_output,
            &admitted.token_ids,
        )];
        let assembly_record = CandidateAssemblyRecord::new(
            CandidateAssemblyId::new(),
            vec![AssemblyPartRecord::new(JoinBefore::None, assembly_span)],
            &exact,
        )
        .expect("assembly declaration");
        let projection_assembly_record = assembly_record.clone();
        let admitted_assembly = store
            .verify_and_record_candidate_assembly(assembly_record, &[&admitted_span])
            .expect("admit exact assembly");
        let target =
            loom_research_types::ByteRange::new(source.text.len() as u64, source.text.len() as u64)
                .expect("append range");
        let projection_record = CandidateProjectionRecord::new(
            CandidateProjectionId::new(),
            &projection_assembly_record,
            source.revision_id,
            source.blob_id,
            source.text.as_bytes(),
            target,
            &exact,
        )
        .expect("projection declaration");
        let admitted_projection = store
            .verify_and_record_candidate_projection(&admitted_assembly, projection_record)
            .expect("admit exact projection topology");
        assert_ne!(
            admitted_assembly.admission_record_id(),
            admitted_projection.admission_record_id()
        );

        let command_id = CommandId::new();
        let requested_at_ms = now_unix_ms();
        let promotion_request = PromotionCommandRequest::new(
            store.manifest.project_id,
            admitted_projection.record.source_revision_id(),
            admitted_projection.record.source_blob_id(),
            PromotionSubject::CandidateProjection {
                projection_id: admitted_projection.record.id(),
            },
            admitted_projection.admission_record_id().as_blob_id(),
            admitted_projection.record.witness().resulting_blob_id(),
            admitted_projection.record.witness().resulting_byte_len(),
            command_id,
            requested_at_ms,
        )
        .expect("promotion request");
        let request_fingerprint = promotion_request.command_request_fingerprint();
        let recorded_request = store
            .record_promotion_command_request(
                PromotionSubjectLease::CandidateProjection(&admitted_projection),
                &promotion_request,
            )
            .expect("durably record request before presence");
        let occurred_at_ms = recorded_request.recorded_at_ms + 1;
        let event_receipt_bytes = b"host-owned foreground gesture".to_vec();
        let event_receipt_blob_id = BlobId::digest(&event_receipt_bytes);
        let session_fingerprint = BlobId::digest(b"host session");
        let presence = loom_research_types::UserPresenceEvidence::new(
            UserPresenceKind::EditorGesture,
            session_fingerprint,
            event_receipt_blob_id,
            1,
            occurred_at_ms,
        )
        .expect("presence claim");
        let authority = PromotionAuthority::new(
            loom_research_types::PromotionActor::new("foreground reviewer").expect("bounded actor"),
            promotion_request,
            presence,
        )
        .expect("authority intent");
        let presence_lease = VerifiedUserPresence {
            session_nonce: store.session_nonce,
            command_id,
            command_request_fingerprint: request_fingerprint,
            kind: UserPresenceKind::EditorGesture,
            session_fingerprint,
            event_receipt_blob_id,
            event_receipt_bytes,
            monotonic_event_index: 1,
            occurred_at_ms,
            actor: loom_research_types::PromotionActor::new("foreground reviewer")
                .expect("bounded host actor"),
        };
        let recorded_authority = store
            .record_promotion_authority(
                recorded_request,
                PromotionSubjectLease::CandidateProjection(&admitted_projection),
                presence_lease,
                &authority,
            )
            .expect("record authority before mutation");
        assert_eq!(recorded_authority.command_id(), command_id);
        assert_eq!(
            recorded_authority.intended_result_blob_id(),
            admitted_projection.record.witness().resulting_blob_id()
        );
        assert!(
            store
                .load_receipt(command_id)
                .expect("receipt lookup")
                .is_none(),
            "authority must not depend on a completed command receipt"
        );
    }
}
