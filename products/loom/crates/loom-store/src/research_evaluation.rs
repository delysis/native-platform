use std::collections::BTreeSet;

use loom_eval::{
    CoreHardGate, CoreHardGateObservation, HardGateEvidence, HardGateOutcome, PessimisticEnsemble,
    ValidatedBlindPairJudgment, ValidatedCriterionObservation, ValidatedEvidenceSpan,
};
use loom_research_types::{MAX_COMPLETION_PROMPT_BYTES, ManifestKey};
use loom_types::{ArtifactId, BlobId, now_unix_ms};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;

use crate::provenance::insert_blob_row;
use crate::{
    AdmittedCandidateProjection, ProjectStore, ResearchExecutionRecordKind, Result, StoreError,
};

const MAX_DESCRIPTOR_AXES: usize = 32;
const SCORE_SCALE: u32 = 1_000_000;
pub const MAX_DIAGNOSTIC_EVALUATION_PACKET_BYTES: usize = MAX_COMPLETION_PROMPT_BYTES;
pub const MAX_DIAGNOSTIC_EVALUATION_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Evaluation rows are replayable diagnostics. They do not recreate a judge,
/// evaluator, archive, benchmark, or promotion capability after deserialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticEvaluationKind {
    HardGate,
    CriterionCard,
    BlindPairwise,
    Descriptor,
    CloseRead,
    HumanReview,
}

impl DiagnosticEvaluationKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::HardGate => "hard_gate",
            Self::CriterionCard => "criterion_card",
            Self::BlindPairwise => "blind_pairwise",
            Self::Descriptor => "descriptor",
            Self::CloseRead => "close_read",
            Self::HumanReview => "human_review",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DiagnosticEvaluatorClass {
    ClaimedUnknown,
}

impl DiagnosticEvaluatorClass {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ClaimedUnknown => "claimed_unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EvaluationEvidenceAuthority {
    ClaimedDiagnostic,
    VerifiedProjection,
}

impl EvaluationEvidenceAuthority {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ClaimedDiagnostic => "claimed_diagnostic",
            Self::VerifiedProjection => "verified_projection",
        }
    }
}

/// Move-only, current-store proof that one candidate is the exact resulting
/// content of an admitted base-writer projection. Reopening or copying a store
/// cannot recreate it from rows alone.
///
/// ```compile_fail
/// fn needs_clone<T: Clone>() {}
/// needs_clone::<loom_store::VerifiedEvaluationCandidateLease>();
/// ```
///
/// ```compile_fail
/// fn needs_deserialize<T: serde::de::DeserializeOwned>() {}
/// needs_deserialize::<loom_store::VerifiedEvaluationCandidateLease>();
/// ```
#[must_use]
#[derive(Debug)]
pub struct VerifiedEvaluationCandidateLease {
    occurrence_id: ArtifactId,
    candidate_blob_id: BlobId,
    projection_binding_fingerprint: BlobId,
    candidate_byte_len: u64,
    authority: VerifiedEvaluationCandidateAuthority,
}

#[derive(Debug)]
struct VerifiedEvaluationCandidateAuthority;

impl VerifiedEvaluationCandidateLease {
    pub const fn occurrence_id(&self) -> ArtifactId {
        self.occurrence_id
    }

    pub const fn candidate_blob_id(&self) -> BlobId {
        self.candidate_blob_id
    }

    pub const fn projection_binding_fingerprint(&self) -> BlobId {
        self.projection_binding_fingerprint
    }

    pub const fn candidate_byte_len(&self) -> u64 {
        self.candidate_byte_len
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DiagnosticEvaluationTaskPersistence<'a> {
    pub task_id: ArtifactId,
    pub candidate_occurrence_id: Option<ArtifactId>,
    pub kind: DiagnosticEvaluationKind,
    pub pack_fingerprint: BlobId,
    pub exact_packet_bytes: &'a [u8],
}

#[derive(Debug)]
pub struct VerifiedEvaluationTaskPersistence<'a> {
    pub task_id: ArtifactId,
    pub kind: DiagnosticEvaluationKind,
    pub pack_fingerprint: BlobId,
    pub exact_packet_bytes: &'a [u8],
    pub candidate: Option<VerifiedEvaluationCandidateLease>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PersistedDiagnosticEvaluationTask {
    task_id: ArtifactId,
    record_fingerprint: BlobId,
    packet_blob_id: BlobId,
}

impl PersistedDiagnosticEvaluationTask {
    pub const fn task_id(self) -> ArtifactId {
        self.task_id
    }

    pub const fn record_fingerprint(self) -> BlobId {
        self.record_fingerprint
    }

    pub const fn packet_blob_id(self) -> BlobId {
        self.packet_blob_id
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DiagnosticPairwiseAssignmentPersistence {
    pub task_id: ArtifactId,
    pub first_occurrence_id: ArtifactId,
    pub second_occurrence_id: ArtifactId,
    pub label_map_fingerprint: BlobId,
    pub order_cell: bool,
    pub criterion_order_cell: bool,
    pub anchor_order_cell: bool,
}

#[derive(Debug)]
pub struct VerifiedPairwiseAssignmentPersistence {
    pub task_id: ArtifactId,
    pub first: VerifiedEvaluationCandidateLease,
    pub second: VerifiedEvaluationCandidateLease,
    pub label_map_fingerprint: BlobId,
    pub order_cell: bool,
    pub criterion_order_cell: bool,
    pub anchor_order_cell: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PersistedDiagnosticPairwiseAssignment {
    assignment_fingerprint: BlobId,
    task_id: ArtifactId,
}

impl PersistedDiagnosticPairwiseAssignment {
    pub const fn assignment_fingerprint(self) -> BlobId {
        self.assignment_fingerprint
    }

    pub const fn task_id(self) -> ArtifactId {
        self.task_id
    }
}

pub enum DiagnosticEvaluationObservation<'a> {
    HardGate(&'a CoreHardGateObservation),
    Criterion(&'a ValidatedCriterionObservation),
    BlindPairwise(&'a ValidatedBlindPairJudgment),
}

impl std::fmt::Debug for DiagnosticEvaluationObservation<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("DiagnosticEvaluationObservation")
            .field(&self.kind())
            .finish()
    }
}

impl DiagnosticEvaluationObservation<'_> {
    const fn kind(&self) -> DiagnosticEvaluationKind {
        match self {
            Self::HardGate(_) => DiagnosticEvaluationKind::HardGate,
            Self::Criterion(_) => DiagnosticEvaluationKind::CriterionCard,
            Self::BlindPairwise(_) => DiagnosticEvaluationKind::BlindPairwise,
        }
    }

    const fn outcome(&self) -> &'static str {
        match self {
            Self::HardGate(observation) => match observation.outcome() {
                HardGateOutcome::Pass => "validated",
                HardGateOutcome::Fail => "rejected",
                HardGateOutcome::Abstain => "abstained",
            },
            Self::Criterion(ValidatedCriterionObservation::Scored { .. })
            | Self::BlindPairwise(
                ValidatedBlindPairJudgment::Winner { .. } | ValidatedBlindPairJudgment::Tie { .. },
            ) => "validated",
            Self::Criterion(ValidatedCriterionObservation::Abstained { .. })
            | Self::BlindPairwise(ValidatedBlindPairJudgment::Abstained { .. }) => "abstained",
        }
    }

    fn evidence(&self) -> &[ValidatedEvidenceSpan] {
        match self {
            Self::HardGate(observation) => match observation.evidence() {
                HardGateEvidence::CandidateSpans { spans } => spans,
                HardGateEvidence::Receipt { .. } => &[],
            },
            Self::Criterion(ValidatedCriterionObservation::Scored { evidence, .. })
            | Self::BlindPairwise(
                ValidatedBlindPairJudgment::Winner { evidence, .. }
                | ValidatedBlindPairJudgment::Tie { evidence },
            ) => evidence,
            Self::Criterion(ValidatedCriterionObservation::Abstained { .. })
            | Self::BlindPairwise(ValidatedBlindPairJudgment::Abstained { .. }) => &[],
        }
    }

    fn criterion_id(&self) -> &str {
        match self {
            Self::HardGate(observation) => hard_gate_key(observation.gate()),
            Self::Criterion(ValidatedCriterionObservation::Scored { criterion_key, .. }) => {
                criterion_key
            }
            Self::Criterion(ValidatedCriterionObservation::Abstained {
                expected_criterion_key,
                ..
            }) => expected_criterion_key,
            Self::BlindPairwise(_) => "blind_pairwise",
        }
    }

    const fn candidate_occurrence_id(&self) -> Option<ArtifactId> {
        match self {
            Self::HardGate(observation) => Some(observation.candidate_occurrence_id()),
            Self::Criterion(_) | Self::BlindPairwise(_) => None,
        }
    }

    const fn receipt_evidence(&self) -> Option<(ArtifactId, BlobId)> {
        match self {
            Self::HardGate(observation) => match observation.evidence() {
                HardGateEvidence::Receipt {
                    artifact_id,
                    blob_id,
                } => Some((*artifact_id, *blob_id)),
                HardGateEvidence::CandidateSpans { .. } => None,
            },
            Self::Criterion(_) | Self::BlindPairwise(_) => None,
        }
    }
}

#[derive(Debug)]
pub struct DiagnosticEvaluationReceiptPersistence<'a> {
    pub task_id: ArtifactId,
    pub exact_raw_response_bytes: &'a [u8],
    pub observation: DiagnosticEvaluationObservation<'a>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PersistedDiagnosticEvaluationReceipt {
    receipt_fingerprint: BlobId,
    task_id: ArtifactId,
    evidence_span_count: u32,
}

impl PersistedDiagnosticEvaluationReceipt {
    pub const fn receipt_fingerprint(self) -> BlobId {
        self.receipt_fingerprint
    }

    pub const fn task_id(self) -> ArtifactId {
        self.task_id
    }

    pub const fn evidence_span_count(self) -> u32 {
        self.evidence_span_count
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DiagnosticDescriptorAxis {
    id: ManifestKey,
    score_millionths: u32,
}

impl DiagnosticDescriptorAxis {
    pub fn new(id: ManifestKey, score_millionths: u32) -> Result<Self> {
        if score_millionths > SCORE_SCALE {
            return Err(StoreError::InvalidResearchDiagnostic(
                "descriptor score exceeds one million millionths".into(),
            ));
        }
        Ok(Self {
            id,
            score_millionths,
        })
    }

    pub const fn id(&self) -> &ManifestKey {
        &self.id
    }

    pub const fn score_millionths(&self) -> u32 {
        self.score_millionths
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DiagnosticPreferenceSource {
    ClaimedUnknown,
}

impl DiagnosticPreferenceSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ClaimedUnknown => "claimed_unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifiedPreferenceSource {
    FrontierWeak,
    LocalCriticWeak,
}

impl VerifiedPreferenceSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::FrontierWeak => "frontier_weak",
            Self::LocalCriticWeak => "local_critic_weak",
        }
    }

    const fn required_evaluator_class(self) -> &'static str {
        match self {
            Self::FrontierWeak => "frontier_critic",
            Self::LocalCriticWeak => "local_critic",
        }
    }
}

/// Future backend adapters may issue this only after binding one exact raw
/// response, evaluator identity, parsed observation, candidate projection, and
/// receipt row. No production constructor exists in this crate today.
///
/// ```compile_fail
/// fn needs_clone<T: Clone>() {}
/// needs_clone::<loom_store::VerifiedPreferenceSourceLease>();
/// ```
///
/// ```compile_fail
/// fn needs_deserialize<T: serde::de::DeserializeOwned>() {}
/// needs_deserialize::<loom_store::VerifiedPreferenceSourceLease>();
/// ```
#[must_use]
#[derive(Debug)]
pub struct VerifiedPreferenceSourceLease {
    assignment_fingerprint: BlobId,
    receipt_fingerprint: BlobId,
    source: VerifiedPreferenceSource,
    judgment: ValidatedBlindPairJudgment,
    backend_verifier_fingerprint: BlobId,
    authority: VerifiedPreferenceAuthority,
}

#[derive(Debug)]
struct VerifiedPreferenceAuthority;

#[derive(Serialize)]
struct EvaluationTaskRecord {
    format: &'static str,
    task_id: ArtifactId,
    candidate_occurrence_id: Option<ArtifactId>,
    kind: DiagnosticEvaluationKind,
    pack_fingerprint: BlobId,
    packet_blob_id: BlobId,
    evidence_authority: EvaluationEvidenceAuthority,
    candidate_blob_id: Option<BlobId>,
    projection_binding_fingerprint: Option<BlobId>,
}

#[derive(Serialize)]
struct PairwiseAssignmentRecord {
    format: &'static str,
    task_id: ArtifactId,
    first_occurrence_id: ArtifactId,
    second_occurrence_id: ArtifactId,
    label_map_fingerprint: BlobId,
    order_cell: bool,
    criterion_order_cell: bool,
    anchor_order_cell: bool,
    evidence_authority: EvaluationEvidenceAuthority,
    first_candidate_blob_id: Option<BlobId>,
    first_projection_binding_fingerprint: Option<BlobId>,
    second_candidate_blob_id: Option<BlobId>,
    second_projection_binding_fingerprint: Option<BlobId>,
}

#[derive(Serialize)]
struct EvaluationReceiptRecord<'a, T: Serialize> {
    format: &'static str,
    task_id: ArtifactId,
    evaluator_class: DiagnosticEvaluatorClass,
    evaluator_fingerprint: BlobId,
    raw_response_blob_id: BlobId,
    receipt_authority: &'static str,
    observation: &'a T,
}

#[derive(Serialize)]
struct ScoreVectorRecord<'a> {
    format: &'static str,
    receipt_fingerprint: BlobId,
    ensemble: &'a PessimisticEnsemble,
}

#[derive(Serialize)]
struct DescriptorRecord<'a> {
    format: &'static str,
    receipt_fingerprint: BlobId,
    candidate_occurrence_id: ArtifactId,
    axes: &'a [DiagnosticDescriptorAxis],
}

#[derive(Serialize)]
struct PreferenceRecord<'a> {
    format: &'static str,
    assignment_fingerprint: BlobId,
    receipt_fingerprint: BlobId,
    source: &'static str,
    backend_verifier_fingerprint: Option<BlobId>,
    judgment: &'a ValidatedBlindPairJudgment,
}

#[derive(Clone, Copy)]
struct StoredTaskBinding {
    kind: DiagnosticEvaluationKind,
    candidate_occurrence_id: Option<ArtifactId>,
    candidate_blob_id: Option<BlobId>,
    projection_binding_fingerprint: Option<BlobId>,
    evidence_authority: EvaluationEvidenceAuthority,
}

#[derive(Clone, Copy)]
struct StoredCandidateBinding {
    candidate_blob_id: Option<BlobId>,
    projection_binding_fingerprint: Option<BlobId>,
}

struct PreparedEvidenceSpan {
    occurrence_id: ArtifactId,
    candidate_blob_id: BlobId,
    start: u64,
    end: u64,
    quote_blob_id: BlobId,
    quote_len: usize,
}

impl ProjectStore {
    pub fn freeze_evaluation_candidate(
        &self,
        admitted: &AdmittedCandidateProjection,
    ) -> Result<VerifiedEvaluationCandidateLease> {
        let (projection_id, candidate_blob_id, projection_binding_fingerprint, candidate_byte_len) =
            admitted
                .evaluation_evidence(self.session_nonce)
                .ok_or_else(|| {
                    StoreError::InvalidResearchDiagnostic(
                        "candidate projection belongs to another store session".into(),
                    )
                })?;
        let exact: i64 = self.connection.query_row(
            "SELECT COUNT(*)
             FROM research_candidate_projections projection
             JOIN research_admission_records admission
               ON admission.subject_kind = 'candidate_projection'
              AND admission.subject_id = projection.projection_id
             JOIN blobs resulting ON resulting.blob_id = projection.resulting_blob_id
             WHERE projection.projection_id = ?1
               AND projection.resulting_blob_id = ?2
               AND projection.resulting_byte_len = ?3
               AND resulting.byte_len = ?3",
            params![
                projection_id.to_string(),
                candidate_blob_id.to_string(),
                i64::try_from(candidate_byte_len).map_err(|_| {
                    StoreError::InvalidResearchDiagnostic(
                        "candidate projection byte length exceeds SQLite's integer domain".into(),
                    )
                })?,
            ],
            |row| row.get(0),
        )?;
        if exact != 1 {
            return Err(StoreError::InvalidResearchDiagnostic(
                "candidate projection is not an exact admitted projection".into(),
            ));
        }
        let bytes = self.read_blob(candidate_blob_id)?;
        if u64::try_from(bytes.len()).ok() != Some(candidate_byte_len)
            || BlobId::digest(&bytes) != candidate_blob_id
        {
            return Err(StoreError::InvalidResearchDiagnostic(
                "candidate projection content differs from its admitted witness".into(),
            ));
        }
        Ok(VerifiedEvaluationCandidateLease {
            occurrence_id: ArtifactId::from_ulid(projection_id.as_ulid()),
            candidate_blob_id,
            projection_binding_fingerprint,
            candidate_byte_len,
            authority: VerifiedEvaluationCandidateAuthority,
        })
    }

    pub fn persist_diagnostic_evaluation_task(
        &mut self,
        input: DiagnosticEvaluationTaskPersistence<'_>,
    ) -> Result<PersistedDiagnosticEvaluationTask> {
        self.persist_evaluation_task(
            input.task_id,
            input.candidate_occurrence_id,
            input.kind,
            input.pack_fingerprint,
            input.exact_packet_bytes,
            EvaluationEvidenceAuthority::ClaimedDiagnostic,
            None,
            None,
        )
    }

    pub fn persist_verified_evaluation_task(
        &mut self,
        input: VerifiedEvaluationTaskPersistence<'_>,
    ) -> Result<PersistedDiagnosticEvaluationTask> {
        let (candidate_occurrence_id, candidate_blob_id, projection_binding_fingerprint) =
            match input.candidate {
                Some(candidate) => (
                    Some(candidate.occurrence_id),
                    Some(candidate.candidate_blob_id),
                    Some(candidate.projection_binding_fingerprint),
                ),
                None => (None, None, None),
            };
        self.persist_evaluation_task(
            input.task_id,
            candidate_occurrence_id,
            input.kind,
            input.pack_fingerprint,
            input.exact_packet_bytes,
            EvaluationEvidenceAuthority::VerifiedProjection,
            candidate_blob_id,
            projection_binding_fingerprint,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn persist_evaluation_task(
        &mut self,
        task_id: ArtifactId,
        candidate_occurrence_id: Option<ArtifactId>,
        kind: DiagnosticEvaluationKind,
        pack_fingerprint: BlobId,
        exact_packet_bytes: &[u8],
        evidence_authority: EvaluationEvidenceAuthority,
        candidate_blob_id: Option<BlobId>,
        projection_binding_fingerprint: Option<BlobId>,
    ) -> Result<PersistedDiagnosticEvaluationTask> {
        if exact_packet_bytes.is_empty()
            || exact_packet_bytes.len() > MAX_DIAGNOSTIC_EVALUATION_PACKET_BYTES
        {
            return Err(StoreError::InvalidResearchDiagnostic(
                "evaluation packet byte length is outside its bounded domain".into(),
            ));
        }
        let blind = matches!(kind, DiagnosticEvaluationKind::BlindPairwise);
        if blind != candidate_occurrence_id.is_none()
            || candidate_blob_id.is_some() != projection_binding_fingerprint.is_some()
            || (evidence_authority == EvaluationEvidenceAuthority::VerifiedProjection
                && !blind
                && candidate_blob_id.is_none())
            || (evidence_authority == EvaluationEvidenceAuthority::ClaimedDiagnostic
                && candidate_blob_id.is_some())
        {
            return Err(StoreError::InvalidResearchDiagnostic(
                "evaluation task candidate binding does not match its authority class".into(),
            ));
        }

        let packet_blob_id = self.put_blob(exact_packet_bytes)?;
        let canonical = serde_json::to_vec(&EvaluationTaskRecord {
            format: "loom.diagnostic-evaluation-task.v1",
            task_id,
            candidate_occurrence_id,
            kind,
            pack_fingerprint,
            packet_blob_id,
            evidence_authority,
            candidate_blob_id,
            projection_binding_fingerprint,
        })?;
        let record = self.persist_research_execution_record(
            ResearchExecutionRecordKind::EvaluationTask,
            &canonical,
        )?;
        let created_at_ms = now_unix_ms();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_blob_row(
            &transaction,
            packet_blob_id,
            exact_packet_bytes.len(),
            created_at_ms,
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO research_evaluation_tasks(
                task_id, candidate_occurrence_id, evaluation_kind, pack_fingerprint,
                packet_blob_id, evidence_authority, candidate_blob_id,
                projection_binding_fingerprint, record_fingerprint, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                task_id.to_string(),
                candidate_occurrence_id.map(|id| id.to_string()),
                kind.as_str(),
                pack_fingerprint.to_string(),
                packet_blob_id.to_string(),
                evidence_authority.as_str(),
                candidate_blob_id.map(|id| id.to_string()),
                projection_binding_fingerprint.map(|id| id.to_string()),
                record.fingerprint().to_string(),
                created_at_ms,
            ],
        )?;
        let exact: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM research_evaluation_tasks
             WHERE task_id = ?1 AND candidate_occurrence_id IS ?2
               AND evaluation_kind = ?3 AND pack_fingerprint = ?4
               AND packet_blob_id = ?5 AND evidence_authority = ?6
               AND candidate_blob_id IS ?7 AND projection_binding_fingerprint IS ?8
               AND record_fingerprint = ?9",
            params![
                task_id.to_string(),
                candidate_occurrence_id.map(|id| id.to_string()),
                kind.as_str(),
                pack_fingerprint.to_string(),
                packet_blob_id.to_string(),
                evidence_authority.as_str(),
                candidate_blob_id.map(|id| id.to_string()),
                projection_binding_fingerprint.map(|id| id.to_string()),
                record.fingerprint().to_string(),
            ],
            |row| row.get(0),
        )?;
        if exact != 1 {
            return Err(StoreError::ResearchExecutionSubjectConflict {
                subject: record.fingerprint(),
            });
        }
        transaction.commit()?;
        Ok(PersistedDiagnosticEvaluationTask {
            task_id,
            record_fingerprint: record.fingerprint(),
            packet_blob_id,
        })
    }

    pub fn persist_diagnostic_pairwise_assignment(
        &mut self,
        input: DiagnosticPairwiseAssignmentPersistence,
    ) -> Result<PersistedDiagnosticPairwiseAssignment> {
        self.persist_pairwise_assignment(
            input.task_id,
            input.first_occurrence_id,
            input.second_occurrence_id,
            input.label_map_fingerprint,
            input.order_cell,
            input.criterion_order_cell,
            input.anchor_order_cell,
            EvaluationEvidenceAuthority::ClaimedDiagnostic,
            None,
            None,
            None,
            None,
        )
    }

    pub fn persist_verified_pairwise_assignment(
        &mut self,
        input: VerifiedPairwiseAssignmentPersistence,
    ) -> Result<PersistedDiagnosticPairwiseAssignment> {
        let VerifiedPairwiseAssignmentPersistence {
            task_id,
            first,
            second,
            label_map_fingerprint,
            order_cell,
            criterion_order_cell,
            anchor_order_cell,
        } = input;
        let VerifiedEvaluationCandidateLease {
            occurrence_id: first_occurrence_id,
            candidate_blob_id: first_candidate_blob_id,
            projection_binding_fingerprint: first_projection_binding_fingerprint,
            candidate_byte_len: _first_candidate_byte_len,
            authority: _first_authority,
        } = first;
        let VerifiedEvaluationCandidateLease {
            occurrence_id: second_occurrence_id,
            candidate_blob_id: second_candidate_blob_id,
            projection_binding_fingerprint: second_projection_binding_fingerprint,
            candidate_byte_len: _second_candidate_byte_len,
            authority: _second_authority,
        } = second;
        self.persist_pairwise_assignment(
            task_id,
            first_occurrence_id,
            second_occurrence_id,
            label_map_fingerprint,
            order_cell,
            criterion_order_cell,
            anchor_order_cell,
            EvaluationEvidenceAuthority::VerifiedProjection,
            Some(first_candidate_blob_id),
            Some(first_projection_binding_fingerprint),
            Some(second_candidate_blob_id),
            Some(second_projection_binding_fingerprint),
        )
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn persist_pairwise_assignment(
        &mut self,
        task_id: ArtifactId,
        first_occurrence_id: ArtifactId,
        second_occurrence_id: ArtifactId,
        label_map_fingerprint: BlobId,
        order_cell: bool,
        criterion_order_cell: bool,
        anchor_order_cell: bool,
        evidence_authority: EvaluationEvidenceAuthority,
        first_candidate_blob_id: Option<BlobId>,
        first_projection_binding_fingerprint: Option<BlobId>,
        second_candidate_blob_id: Option<BlobId>,
        second_projection_binding_fingerprint: Option<BlobId>,
    ) -> Result<PersistedDiagnosticPairwiseAssignment> {
        if first_occurrence_id == second_occurrence_id {
            return Err(StoreError::InvalidResearchDiagnostic(
                "pairwise assignment repeats a candidate occurrence".into(),
            ));
        }
        let exact_candidate_shape = [
            first_candidate_blob_id,
            first_projection_binding_fingerprint,
            second_candidate_blob_id,
            second_projection_binding_fingerprint,
        ]
        .iter()
        .all(Option::is_some);
        if (evidence_authority == EvaluationEvidenceAuthority::VerifiedProjection)
            != exact_candidate_shape
        {
            return Err(StoreError::InvalidResearchDiagnostic(
                "pairwise assignment candidate bindings do not match its authority class".into(),
            ));
        }
        let task = load_task_binding(&self.connection, task_id)?;
        if task.kind != DiagnosticEvaluationKind::BlindPairwise
            || task.candidate_occurrence_id.is_some()
            || task.evidence_authority != evidence_authority
        {
            return Err(StoreError::InvalidResearchDiagnostic(
                "pairwise assignment does not reference a matching blind-pair task".into(),
            ));
        }
        let canonical = serde_json::to_vec(&PairwiseAssignmentRecord {
            format: "loom.diagnostic-pairwise-assignment.v1",
            task_id,
            first_occurrence_id,
            second_occurrence_id,
            label_map_fingerprint,
            order_cell,
            criterion_order_cell,
            anchor_order_cell,
            evidence_authority,
            first_candidate_blob_id,
            first_projection_binding_fingerprint,
            second_candidate_blob_id,
            second_projection_binding_fingerprint,
        })?;
        let record = self.persist_research_execution_record(
            ResearchExecutionRecordKind::PairwiseAssignment,
            &canonical,
        )?;
        let created_at_ms = now_unix_ms();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT OR IGNORE INTO research_pairwise_assignments(
                assignment_fingerprint, task_id, first_occurrence_id,
                second_occurrence_id, label_map_fingerprint, order_cell,
                criterion_order_cell, anchor_order_cell, record_fingerprint,
                evidence_authority, first_candidate_blob_id,
                first_projection_binding_fingerprint, second_candidate_blob_id,
                second_projection_binding_fingerprint, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?1, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                record.fingerprint().to_string(),
                task_id.to_string(),
                first_occurrence_id.to_string(),
                second_occurrence_id.to_string(),
                label_map_fingerprint.to_string(),
                i64::from(order_cell),
                i64::from(criterion_order_cell),
                i64::from(anchor_order_cell),
                evidence_authority.as_str(),
                first_candidate_blob_id.map(|id| id.to_string()),
                first_projection_binding_fingerprint.map(|id| id.to_string()),
                second_candidate_blob_id.map(|id| id.to_string()),
                second_projection_binding_fingerprint.map(|id| id.to_string()),
                created_at_ms,
            ],
        )?;
        let exact: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM research_pairwise_assignments
             WHERE assignment_fingerprint = ?1 AND task_id = ?2
               AND first_occurrence_id = ?3 AND second_occurrence_id = ?4
               AND label_map_fingerprint = ?5 AND order_cell = ?6
               AND criterion_order_cell = ?7 AND anchor_order_cell = ?8
               AND record_fingerprint = ?1 AND evidence_authority = ?9
               AND first_candidate_blob_id IS ?10
               AND first_projection_binding_fingerprint IS ?11
               AND second_candidate_blob_id IS ?12
               AND second_projection_binding_fingerprint IS ?13",
            params![
                record.fingerprint().to_string(),
                task_id.to_string(),
                first_occurrence_id.to_string(),
                second_occurrence_id.to_string(),
                label_map_fingerprint.to_string(),
                i64::from(order_cell),
                i64::from(criterion_order_cell),
                i64::from(anchor_order_cell),
                evidence_authority.as_str(),
                first_candidate_blob_id.map(|id| id.to_string()),
                first_projection_binding_fingerprint.map(|id| id.to_string()),
                second_candidate_blob_id.map(|id| id.to_string()),
                second_projection_binding_fingerprint.map(|id| id.to_string()),
            ],
            |row| row.get(0),
        )?;
        if exact != 1 {
            return Err(StoreError::ResearchExecutionSubjectConflict {
                subject: record.fingerprint(),
            });
        }
        transaction.commit()?;
        Ok(PersistedDiagnosticPairwiseAssignment {
            assignment_fingerprint: record.fingerprint(),
            task_id,
        })
    }

    pub fn persist_diagnostic_evaluation_receipt(
        &mut self,
        input: &DiagnosticEvaluationReceiptPersistence<'_>,
    ) -> Result<PersistedDiagnosticEvaluationReceipt> {
        if input.exact_raw_response_bytes.is_empty()
            || input.exact_raw_response_bytes.len() > MAX_DIAGNOSTIC_EVALUATION_RESPONSE_BYTES
        {
            return Err(StoreError::InvalidResearchDiagnostic(
                "evaluation raw response byte length is outside its bounded domain".into(),
            ));
        }
        let task = load_task_binding(&self.connection, input.task_id)?;
        if task.kind != input.observation.kind() {
            return Err(StoreError::InvalidResearchDiagnostic(
                "validated observation kind does not match its task".into(),
            ));
        }
        let allowed_occurrences = load_allowed_occurrences(&self.connection, input.task_id, task)?;
        if input
            .observation
            .candidate_occurrence_id()
            .is_some_and(|occurrence| !allowed_occurrences.contains_key(&occurrence))
        {
            return Err(StoreError::InvalidResearchDiagnostic(
                "hard-gate observation belongs to another candidate".into(),
            ));
        }
        if let Some((artifact_id, blob_id)) = input.observation.receipt_evidence() {
            verify_artifact_blob_binding(&self.connection, artifact_id, blob_id)?;
        }
        let evidence = self.prepare_evidence(input.observation.evidence(), &allowed_occurrences)?;
        let pairwise_preference = pairwise_preference_for_observation(
            &self.connection,
            input.task_id,
            &input.observation,
        )?;
        let evaluator_class = DiagnosticEvaluatorClass::ClaimedUnknown;
        let evaluator_fingerprint = BlobId::digest(b"loom/claimed-unknown-evaluator/v1\0");
        let raw_response_blob_id = self.put_blob(input.exact_raw_response_bytes)?;
        let canonical = serialize_evaluation_receipt(
            input,
            evaluator_class,
            evaluator_fingerprint,
            "claimed_diagnostic",
            raw_response_blob_id,
        )?;
        let record = self.persist_research_execution_record(
            ResearchExecutionRecordKind::EvaluationReceipt,
            &canonical,
        )?;
        let created_at_ms = now_unix_ms();
        persist_evaluation_receipt_rows(
            &mut self.connection,
            record.fingerprint(),
            input,
            evaluator_class,
            evaluator_fingerprint,
            "claimed_diagnostic",
            pairwise_preference,
            raw_response_blob_id,
            &evidence,
            created_at_ms,
        )?;
        let evidence_span_count = u32::try_from(evidence.len()).map_err(|_| {
            StoreError::InvalidResearchDiagnostic("evidence span count overflow".into())
        })?;
        Ok(PersistedDiagnosticEvaluationReceipt {
            receipt_fingerprint: record.fingerprint(),
            task_id: input.task_id,
            evidence_span_count,
        })
    }

    pub fn persist_diagnostic_score_vector(
        &mut self,
        receipt_fingerprint: BlobId,
        ensemble: &PessimisticEnsemble,
    ) -> Result<BlobId> {
        if ensemble.criteria().is_empty() || ensemble.criteria().len() > 64 {
            return Err(StoreError::InvalidResearchDiagnostic(
                "score vector criterion count is outside 1..=64".into(),
            ));
        }
        verify_receipt_candidate(
            &self.connection,
            receipt_fingerprint,
            ensemble.candidate_occurrence_id(),
        )?;
        let canonical = serde_json::to_vec(&ScoreVectorRecord {
            format: "loom.diagnostic-score-vector.v1",
            receipt_fingerprint,
            ensemble,
        })?;
        let record = self.persist_research_execution_record(
            ResearchExecutionRecordKind::ScoreVector,
            &canonical,
        )?;
        let created_at_ms = now_unix_ms();
        let criterion_count = i64::try_from(ensemble.criteria().len()).map_err(|_| {
            StoreError::InvalidResearchDiagnostic("criterion count overflow".into())
        })?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT OR IGNORE INTO research_score_vectors(
                score_vector_fingerprint, receipt_fingerprint,
                candidate_occurrence_id, criterion_count, pessimistic,
                record_fingerprint, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, 1, ?1, ?5)",
            params![
                record.fingerprint().to_string(),
                receipt_fingerprint.to_string(),
                ensemble.candidate_occurrence_id().to_string(),
                criterion_count,
                created_at_ms,
            ],
        )?;
        let exact: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM research_score_vectors
             WHERE score_vector_fingerprint = ?1 AND receipt_fingerprint = ?2
               AND candidate_occurrence_id = ?3 AND criterion_count = ?4
               AND pessimistic = 1 AND record_fingerprint = ?1",
            params![
                record.fingerprint().to_string(),
                receipt_fingerprint.to_string(),
                ensemble.candidate_occurrence_id().to_string(),
                criterion_count,
            ],
            |row| row.get(0),
        )?;
        ensure_exact_diagnostic_row(exact, record.fingerprint())?;
        transaction.commit()?;
        Ok(record.fingerprint())
    }

    pub fn persist_diagnostic_candidate_descriptor(
        &mut self,
        receipt_fingerprint: BlobId,
        candidate_occurrence_id: ArtifactId,
        axes: &[DiagnosticDescriptorAxis],
    ) -> Result<BlobId> {
        if axes.is_empty() || axes.len() > MAX_DESCRIPTOR_AXES {
            return Err(StoreError::InvalidResearchDiagnostic(format!(
                "descriptor axis count is outside 1..={MAX_DESCRIPTOR_AXES}"
            )));
        }
        let mut unique = BTreeSet::new();
        if axes.iter().any(|axis| !unique.insert(axis.id())) {
            return Err(StoreError::InvalidResearchDiagnostic(
                "descriptor repeats an axis".into(),
            ));
        }
        verify_receipt_candidate(
            &self.connection,
            receipt_fingerprint,
            candidate_occurrence_id,
        )?;
        let canonical = serde_json::to_vec(&DescriptorRecord {
            format: "loom.diagnostic-candidate-descriptor.v1",
            receipt_fingerprint,
            candidate_occurrence_id,
            axes,
        })?;
        let record = self.persist_research_execution_record(
            ResearchExecutionRecordKind::CandidateDescriptor,
            &canonical,
        )?;
        let created_at_ms = now_unix_ms();
        let axis_count = i64::try_from(axes.len()).map_err(|_| {
            StoreError::InvalidResearchDiagnostic("descriptor axis count overflow".into())
        })?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT OR IGNORE INTO research_candidate_descriptors(
                descriptor_fingerprint, receipt_fingerprint,
                candidate_occurrence_id, axis_count, record_fingerprint,
                created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?1, ?5)",
            params![
                record.fingerprint().to_string(),
                receipt_fingerprint.to_string(),
                candidate_occurrence_id.to_string(),
                axis_count,
                created_at_ms,
            ],
        )?;
        let exact: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM research_candidate_descriptors
             WHERE descriptor_fingerprint = ?1 AND receipt_fingerprint = ?2
               AND candidate_occurrence_id = ?3 AND axis_count = ?4
               AND record_fingerprint = ?1",
            params![
                record.fingerprint().to_string(),
                receipt_fingerprint.to_string(),
                candidate_occurrence_id.to_string(),
                axis_count,
            ],
            |row| row.get(0),
        )?;
        ensure_exact_diagnostic_row(exact, record.fingerprint())?;
        transaction.commit()?;
        Ok(record.fingerprint())
    }

    pub fn persist_diagnostic_preference_label(
        &mut self,
        assignment_fingerprint: BlobId,
        receipt_fingerprint: BlobId,
        judgment: &ValidatedBlindPairJudgment,
    ) -> Result<BlobId> {
        self.persist_preference_label(
            assignment_fingerprint,
            receipt_fingerprint,
            DiagnosticPreferenceSource::ClaimedUnknown.as_str(),
            None,
            judgment,
            "claimed_diagnostic",
            DiagnosticEvaluatorClass::ClaimedUnknown.as_str(),
        )
    }

    pub fn persist_source_qualified_preference_label(
        &mut self,
        lease: VerifiedPreferenceSourceLease,
    ) -> Result<BlobId> {
        let VerifiedPreferenceSourceLease {
            assignment_fingerprint,
            receipt_fingerprint,
            source,
            judgment,
            backend_verifier_fingerprint,
            authority: _authority,
        } = lease;
        self.persist_preference_label(
            assignment_fingerprint,
            receipt_fingerprint,
            source.as_str(),
            Some(backend_verifier_fingerprint),
            &judgment,
            "backend_verified",
            source.required_evaluator_class(),
        )
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn persist_preference_label(
        &mut self,
        assignment_fingerprint: BlobId,
        receipt_fingerprint: BlobId,
        source: &'static str,
        backend_verifier_fingerprint: Option<BlobId>,
        judgment: &ValidatedBlindPairJudgment,
        required_receipt_authority: &'static str,
        required_evaluator_class: &'static str,
    ) -> Result<BlobId> {
        let (task_id, first, second) = self.connection.query_row(
            "SELECT task_id, first_occurrence_id, second_occurrence_id
             FROM research_pairwise_assignments WHERE assignment_fingerprint = ?1",
            [assignment_fingerprint.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?;
        let (receipt_task, evaluator_class, receipt_authority, recorded_preference): (
            String,
            String,
            String,
            String,
        ) = self.connection.query_row(
            "SELECT task_id, evaluator_class, receipt_authority, pairwise_preference
             FROM research_evaluation_receipts
             WHERE receipt_fingerprint = ?1",
            [receipt_fingerprint.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        if receipt_task != task_id {
            return Err(StoreError::InvalidResearchDiagnostic(
                "preference receipt belongs to another blinded task".into(),
            ));
        }
        if evaluator_class != required_evaluator_class
            || receipt_authority != required_receipt_authority
        {
            return Err(StoreError::InvalidResearchDiagnostic(
                "preference source does not match verified evaluator authority".into(),
            ));
        }
        let assignment_authority: String = self.connection.query_row(
            "SELECT evidence_authority FROM research_pairwise_assignments
             WHERE assignment_fingerprint = ?1",
            [assignment_fingerprint.to_string()],
            |row| row.get(0),
        )?;
        let expected_assignment_authority = if required_receipt_authority == "backend_verified" {
            EvaluationEvidenceAuthority::VerifiedProjection.as_str()
        } else {
            EvaluationEvidenceAuthority::ClaimedDiagnostic.as_str()
        };
        if assignment_authority != expected_assignment_authority {
            return Err(StoreError::InvalidResearchDiagnostic(
                "preference source does not match candidate evidence authority".into(),
            ));
        }
        let preference = preference_for_judgment(judgment, &first, &second)?;
        if recorded_preference != preference {
            return Err(StoreError::InvalidResearchDiagnostic(
                "preference label differs from its persisted blind judgment".into(),
            ));
        }
        let canonical = serde_json::to_vec(&PreferenceRecord {
            format: "loom.diagnostic-preference-label.v1",
            assignment_fingerprint,
            receipt_fingerprint,
            source,
            backend_verifier_fingerprint,
            judgment,
        })?;
        let record = self.persist_research_execution_record(
            ResearchExecutionRecordKind::PreferenceLabel,
            &canonical,
        )?;
        let created_at_ms = now_unix_ms();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT OR IGNORE INTO research_preference_labels(
                label_fingerprint, assignment_fingerprint, receipt_fingerprint,
                label_source, preference, source_verifier_fingerprint,
                record_fingerprint, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?1, ?7)",
            params![
                record.fingerprint().to_string(),
                assignment_fingerprint.to_string(),
                receipt_fingerprint.to_string(),
                source,
                preference,
                backend_verifier_fingerprint.map(|value| value.to_string()),
                created_at_ms,
            ],
        )?;
        let exact: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM research_preference_labels
             WHERE label_fingerprint = ?1 AND assignment_fingerprint = ?2
               AND receipt_fingerprint = ?3 AND label_source = ?4
               AND preference = ?5 AND source_verifier_fingerprint IS ?6
               AND record_fingerprint = ?1",
            params![
                record.fingerprint().to_string(),
                assignment_fingerprint.to_string(),
                receipt_fingerprint.to_string(),
                source,
                preference,
                backend_verifier_fingerprint.map(|value| value.to_string()),
            ],
            |row| row.get(0),
        )?;
        ensure_exact_diagnostic_row(exact, record.fingerprint())?;
        transaction.commit()?;
        Ok(record.fingerprint())
    }

    fn prepare_evidence(
        &self,
        spans: &[ValidatedEvidenceSpan],
        allowed_occurrences: &std::collections::BTreeMap<ArtifactId, StoredCandidateBinding>,
    ) -> Result<Vec<PreparedEvidenceSpan>> {
        let mut prepared = Vec::with_capacity(spans.len());
        let mut unique = BTreeSet::new();
        for span in spans {
            let Some(binding) = allowed_occurrences.get(&span.candidate_occurrence_id()) else {
                return Err(StoreError::InvalidResearchDiagnostic(
                    "evidence occurrence is outside its frozen task".into(),
                ));
            };
            if binding
                .candidate_blob_id
                .is_some_and(|expected| expected != span.candidate_blob_id())
            {
                return Err(StoreError::InvalidResearchDiagnostic(
                    "evidence blob differs from the occurrence's verified projection".into(),
                ));
            }
            if binding.candidate_blob_id.is_some()
                != binding.projection_binding_fingerprint.is_some()
            {
                return Err(StoreError::CorruptDatabase(
                    "evaluation candidate projection binding is incomplete".into(),
                ));
            }
            let bytes = self.read_blob(span.candidate_blob_id())?;
            let quoted = span.range().checked_str(&bytes).map_err(|_| {
                StoreError::InvalidResearchDiagnostic(
                    "evidence range is not a UTF-8 boundary in the exact candidate".into(),
                )
            })?;
            if BlobId::digest(quoted.as_bytes()) != span.quote_blob_id() {
                return Err(StoreError::InvalidResearchDiagnostic(
                    "evidence quote digest does not match its exact candidate range".into(),
                ));
            }
            let registered: i64 = self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM blobs WHERE blob_id = ?1 AND byte_len = ?2)",
                params![
                    span.candidate_blob_id().to_string(),
                    i64::try_from(bytes.len()).map_err(|_| {
                        StoreError::InvalidResearchDiagnostic(
                            "candidate byte length exceeds SQLite's integer domain".into(),
                        )
                    })?,
                ],
                |row| row.get(0),
            )?;
            if registered != 1 {
                return Err(StoreError::UnregisteredBlob(span.candidate_blob_id()));
            }
            self.put_blob(quoted.as_bytes())?;
            let key = (
                span.candidate_occurrence_id(),
                span.candidate_blob_id(),
                span.range().start(),
                span.range().end(),
                span.quote_blob_id(),
            );
            if !unique.insert(key) {
                return Err(StoreError::InvalidResearchDiagnostic(
                    "evaluation evidence repeats an exact span".into(),
                ));
            }
            prepared.push(PreparedEvidenceSpan {
                occurrence_id: span.candidate_occurrence_id(),
                candidate_blob_id: span.candidate_blob_id(),
                start: span.range().start(),
                end: span.range().end(),
                quote_blob_id: span.quote_blob_id(),
                quote_len: quoted.len(),
            });
        }
        Ok(prepared)
    }
}

fn load_task_binding(
    connection: &rusqlite::Connection,
    task_id: ArtifactId,
) -> Result<StoredTaskBinding> {
    let row = connection
        .query_row(
            "SELECT evaluation_kind, candidate_occurrence_id, candidate_blob_id,
                    projection_binding_fingerprint, evidence_authority
             FROM research_evaluation_tasks WHERE task_id = ?1",
            [task_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| {
            StoreError::InvalidResearchDiagnostic("evaluation task is not persisted".into())
        })?;
    let kind = match row.0.as_str() {
        "hard_gate" => DiagnosticEvaluationKind::HardGate,
        "criterion_card" => DiagnosticEvaluationKind::CriterionCard,
        "blind_pairwise" => DiagnosticEvaluationKind::BlindPairwise,
        "descriptor" => DiagnosticEvaluationKind::Descriptor,
        "close_read" => DiagnosticEvaluationKind::CloseRead,
        "human_review" => DiagnosticEvaluationKind::HumanReview,
        _ => {
            return Err(StoreError::CorruptDatabase(
                "unknown research evaluation task kind".into(),
            ));
        }
    };
    let candidate_occurrence_id = row
        .1
        .map(|value| value.parse::<ArtifactId>())
        .transpose()
        .map_err(|error| StoreError::CorruptDatabase(error.to_string()))?;
    let candidate_blob_id = row
        .2
        .map(|value| value.parse::<BlobId>())
        .transpose()
        .map_err(|error| StoreError::CorruptDatabase(error.to_string()))?;
    let projection_binding_fingerprint = row
        .3
        .map(|value| value.parse::<BlobId>())
        .transpose()
        .map_err(|error| StoreError::CorruptDatabase(error.to_string()))?;
    let evidence_authority = parse_evidence_authority(&row.4)?;
    Ok(StoredTaskBinding {
        kind,
        candidate_occurrence_id,
        candidate_blob_id,
        projection_binding_fingerprint,
        evidence_authority,
    })
}

fn load_allowed_occurrences(
    connection: &rusqlite::Connection,
    task_id: ArtifactId,
    task: StoredTaskBinding,
) -> Result<std::collections::BTreeMap<ArtifactId, StoredCandidateBinding>> {
    if let Some(occurrence) = task.candidate_occurrence_id {
        return Ok(std::collections::BTreeMap::from([(
            occurrence,
            StoredCandidateBinding {
                candidate_blob_id: task.candidate_blob_id,
                projection_binding_fingerprint: task.projection_binding_fingerprint,
            },
        )]));
    }
    let row = connection
        .query_row(
            "SELECT first_occurrence_id, second_occurrence_id,
                    first_candidate_blob_id, first_projection_binding_fingerprint,
                    second_candidate_blob_id, second_projection_binding_fingerprint,
                    evidence_authority
             FROM research_pairwise_assignments WHERE task_id = ?1",
            [task_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| {
            StoreError::InvalidResearchDiagnostic(
                "blind-pair task lacks its frozen assignment".into(),
            )
        })?;
    let first = row
        .0
        .parse::<ArtifactId>()
        .map_err(|error| StoreError::CorruptDatabase(error.to_string()))?;
    let second = row
        .1
        .parse::<ArtifactId>()
        .map_err(|error| StoreError::CorruptDatabase(error.to_string()))?;
    let first_blob = parse_optional_blob_id(row.2)?;
    let first_binding = parse_optional_blob_id(row.3)?;
    let second_blob = parse_optional_blob_id(row.4)?;
    let second_binding = parse_optional_blob_id(row.5)?;
    if parse_evidence_authority(&row.6)? != task.evidence_authority {
        return Err(StoreError::CorruptDatabase(
            "pairwise assignment authority differs from its task".into(),
        ));
    }
    Ok(std::collections::BTreeMap::from([
        (
            first,
            StoredCandidateBinding {
                candidate_blob_id: first_blob,
                projection_binding_fingerprint: first_binding,
            },
        ),
        (
            second,
            StoredCandidateBinding {
                candidate_blob_id: second_blob,
                projection_binding_fingerprint: second_binding,
            },
        ),
    ]))
}

fn parse_optional_blob_id(value: Option<String>) -> Result<Option<BlobId>> {
    value
        .map(|value| value.parse::<BlobId>())
        .transpose()
        .map_err(|error| StoreError::CorruptDatabase(error.to_string()))
}

fn parse_evidence_authority(value: &str) -> Result<EvaluationEvidenceAuthority> {
    match value {
        "claimed_diagnostic" => Ok(EvaluationEvidenceAuthority::ClaimedDiagnostic),
        "verified_projection" => Ok(EvaluationEvidenceAuthority::VerifiedProjection),
        _ => Err(StoreError::CorruptDatabase(
            "unknown evaluation evidence authority".into(),
        )),
    }
}

fn pairwise_preference_for_observation(
    connection: &rusqlite::Connection,
    task_id: ArtifactId,
    observation: &DiagnosticEvaluationObservation<'_>,
) -> Result<Option<&'static str>> {
    let DiagnosticEvaluationObservation::BlindPairwise(judgment) = observation else {
        return Ok(None);
    };
    let (first, second) = connection.query_row(
        "SELECT first_occurrence_id, second_occurrence_id
         FROM research_pairwise_assignments WHERE task_id = ?1",
        [task_id.to_string()],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )?;
    preference_for_judgment(judgment, &first, &second).map(Some)
}

fn verify_artifact_blob_binding(
    connection: &rusqlite::Connection,
    artifact_id: ArtifactId,
    blob_id: BlobId,
) -> Result<()> {
    let exact: i64 = connection.query_row(
        "SELECT COUNT(*)
         FROM artifacts artifact
         JOIN blobs blob ON blob.blob_id = artifact.blob_id
         WHERE artifact.artifact_id = ?1 AND artifact.blob_id = ?2
           AND blob.byte_len > 0",
        params![artifact_id.to_string(), blob_id.to_string()],
        |row| row.get(0),
    )?;
    if exact != 1 {
        return Err(StoreError::InvalidResearchDiagnostic(
            "hard-gate receipt is not an exact registered artifact".into(),
        ));
    }
    Ok(())
}

fn serialize_evaluation_receipt(
    input: &DiagnosticEvaluationReceiptPersistence<'_>,
    evaluator_class: DiagnosticEvaluatorClass,
    evaluator_fingerprint: BlobId,
    receipt_authority: &'static str,
    raw_response_blob_id: BlobId,
) -> Result<Vec<u8>> {
    macro_rules! serialize_observation {
        ($observation:expr) => {
            serde_json::to_vec(&EvaluationReceiptRecord {
                format: "loom.diagnostic-evaluation-receipt.v1",
                task_id: input.task_id,
                evaluator_class,
                evaluator_fingerprint,
                raw_response_blob_id,
                receipt_authority,
                observation: $observation,
            })
            .map_err(StoreError::from)
        };
    }
    match input.observation {
        DiagnosticEvaluationObservation::HardGate(observation) => {
            serialize_observation!(observation)
        }
        DiagnosticEvaluationObservation::Criterion(observation) => {
            serialize_observation!(observation)
        }
        DiagnosticEvaluationObservation::BlindPairwise(observation) => {
            serialize_observation!(observation)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn persist_evaluation_receipt_rows(
    connection: &mut rusqlite::Connection,
    fingerprint: BlobId,
    input: &DiagnosticEvaluationReceiptPersistence<'_>,
    evaluator_class: DiagnosticEvaluatorClass,
    evaluator_fingerprint: BlobId,
    receipt_authority: &'static str,
    pairwise_preference: Option<&'static str>,
    raw_response_blob_id: BlobId,
    evidence: &[PreparedEvidenceSpan],
    created_at_ms: i64,
) -> Result<()> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    insert_blob_row(
        &transaction,
        raw_response_blob_id,
        input.exact_raw_response_bytes.len(),
        created_at_ms,
    )?;
    for span in evidence {
        insert_blob_row(
            &transaction,
            span.quote_blob_id,
            span.quote_len,
            created_at_ms,
        )?;
    }
    transaction.execute(
        "INSERT OR IGNORE INTO research_evaluation_receipts(
            receipt_fingerprint, task_id, evaluator_class, outcome,
            evaluator_fingerprint, pairwise_preference, raw_response_blob_id,
            receipt_authority, record_fingerprint, completed_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?1, ?9)",
        params![
            fingerprint.to_string(),
            input.task_id.to_string(),
            evaluator_class.as_str(),
            input.observation.outcome(),
            evaluator_fingerprint.to_string(),
            pairwise_preference,
            raw_response_blob_id.to_string(),
            receipt_authority,
            created_at_ms,
        ],
    )?;
    verify_receipt_row(
        &transaction,
        fingerprint,
        input,
        evaluator_class,
        evaluator_fingerprint,
        receipt_authority,
        pairwise_preference,
        raw_response_blob_id,
    )?;
    persist_evidence_rows(
        &transaction,
        fingerprint,
        input.observation.criterion_id(),
        evidence,
    )?;
    transaction.commit()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn verify_receipt_row(
    transaction: &Transaction<'_>,
    fingerprint: BlobId,
    input: &DiagnosticEvaluationReceiptPersistence<'_>,
    evaluator_class: DiagnosticEvaluatorClass,
    evaluator_fingerprint: BlobId,
    receipt_authority: &'static str,
    pairwise_preference: Option<&'static str>,
    raw_response_blob_id: BlobId,
) -> Result<()> {
    let exact: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM research_evaluation_receipts
         WHERE receipt_fingerprint = ?1 AND task_id = ?2
           AND evaluator_class = ?3 AND outcome = ?4
           AND evaluator_fingerprint = ?5 AND pairwise_preference IS ?6
           AND raw_response_blob_id = ?7 AND receipt_authority = ?8
           AND record_fingerprint = ?1",
        params![
            fingerprint.to_string(),
            input.task_id.to_string(),
            evaluator_class.as_str(),
            input.observation.outcome(),
            evaluator_fingerprint.to_string(),
            pairwise_preference,
            raw_response_blob_id.to_string(),
            receipt_authority,
        ],
        |row| row.get(0),
    )?;
    ensure_exact_diagnostic_row(exact, fingerprint)
}

fn persist_evidence_rows(
    transaction: &Transaction<'_>,
    receipt_fingerprint: BlobId,
    criterion_id: &str,
    evidence: &[PreparedEvidenceSpan],
) -> Result<()> {
    for (index, span) in evidence.iter().enumerate() {
        let index = i64::try_from(index)
            .map_err(|_| StoreError::InvalidResearchDiagnostic("evidence index overflow".into()))?;
        transaction.execute(
            "INSERT OR IGNORE INTO research_evidence_spans(
                receipt_fingerprint, evidence_index, candidate_occurrence_id,
                candidate_blob_id, start_byte, end_byte, quote_blob_id, criterion_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                receipt_fingerprint.to_string(),
                index,
                span.occurrence_id.to_string(),
                span.candidate_blob_id.to_string(),
                i64::try_from(span.start).map_err(|_| {
                    StoreError::InvalidResearchDiagnostic("evidence start overflow".into())
                })?,
                i64::try_from(span.end).map_err(|_| {
                    StoreError::InvalidResearchDiagnostic("evidence end overflow".into())
                })?,
                span.quote_blob_id.to_string(),
                criterion_id,
            ],
        )?;
    }
    let stored = load_evidence_rows(transaction, receipt_fingerprint)?;
    let expected = evidence
        .iter()
        .enumerate()
        .map(|(index, span)| {
            Ok((
                i64::try_from(index).map_err(|_| {
                    StoreError::InvalidResearchDiagnostic("evidence index overflow".into())
                })?,
                span.occurrence_id.to_string(),
                span.candidate_blob_id.to_string(),
                i64::try_from(span.start).map_err(|_| {
                    StoreError::InvalidResearchDiagnostic("evidence start overflow".into())
                })?,
                i64::try_from(span.end).map_err(|_| {
                    StoreError::InvalidResearchDiagnostic("evidence end overflow".into())
                })?,
                span.quote_blob_id.to_string(),
                criterion_id.to_owned(),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    if stored != expected {
        return Err(StoreError::ResearchExecutionSubjectConflict {
            subject: receipt_fingerprint,
        });
    }
    Ok(())
}

type StoredEvidenceRow = (i64, String, String, i64, i64, String, String);

fn load_evidence_rows(
    transaction: &Transaction<'_>,
    receipt_fingerprint: BlobId,
) -> Result<Vec<StoredEvidenceRow>> {
    let mut statement = transaction.prepare(
        "SELECT evidence_index, candidate_occurrence_id, candidate_blob_id,
                start_byte, end_byte, quote_blob_id, criterion_id
         FROM research_evidence_spans WHERE receipt_fingerprint = ?1
         ORDER BY evidence_index",
    )?;
    let rows = statement.query_map([receipt_fingerprint.to_string()], |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
        ))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn verify_receipt_candidate(
    connection: &rusqlite::Connection,
    receipt_fingerprint: BlobId,
    candidate_occurrence_id: ArtifactId,
) -> Result<()> {
    let exact: i64 = connection.query_row(
        "SELECT COUNT(*)
         FROM research_evaluation_receipts receipt
         JOIN research_evaluation_tasks task USING (task_id)
         WHERE receipt.receipt_fingerprint = ?1
           AND task.candidate_occurrence_id = ?2",
        params![
            receipt_fingerprint.to_string(),
            candidate_occurrence_id.to_string(),
        ],
        |row| row.get(0),
    )?;
    if exact != 1 {
        return Err(StoreError::InvalidResearchDiagnostic(
            "evaluation receipt does not belong to the candidate".into(),
        ));
    }
    Ok(())
}

fn preference_for_judgment(
    judgment: &ValidatedBlindPairJudgment,
    first: &str,
    second: &str,
) -> Result<&'static str> {
    match judgment {
        ValidatedBlindPairJudgment::Winner {
            winner_occurrence_id,
            ..
        } if winner_occurrence_id.to_string() == first => Ok("first"),
        ValidatedBlindPairJudgment::Winner {
            winner_occurrence_id,
            ..
        } if winner_occurrence_id.to_string() == second => Ok("second"),
        ValidatedBlindPairJudgment::Winner { .. } => Err(StoreError::InvalidResearchDiagnostic(
            "blind winner is outside its assignment".into(),
        )),
        ValidatedBlindPairJudgment::Tie { .. } => Ok("tie"),
        ValidatedBlindPairJudgment::Abstained { .. } => Ok("abstain"),
    }
}

const fn hard_gate_key(gate: CoreHardGate) -> &'static str {
    match gate {
        CoreHardGate::Provenance => "hard_gate/provenance",
        CoreHardGate::Assembly => "hard_gate/assembly",
        CoreHardGate::Format => "hard_gate/format",
        CoreHardGate::StoryState => "hard_gate/story_state",
        CoreHardGate::AntiCopy => "hard_gate/anti_copy",
    }
}

fn ensure_exact_diagnostic_row(count: i64, subject: BlobId) -> Result<()> {
    if count != 1 {
        return Err(StoreError::ResearchExecutionSubjectConflict { subject });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use loom_eval::{
        BlindCandidateInput, BlindEvidenceSpanClaim, BlindPairJudgmentClaim, BlindPairSpec,
        BlindPairVerdictClaim, CandidateEvidenceSource, CoreHardGate, CoreHardGateObservation,
        CriterionClaimOutcome, CriterionObservationClaim, EvidenceQuote, EvidenceSpanClaim,
        HardGateEvidence, HardGateOutcome, build_blind_pair, validate_blind_pair_judgment,
        validate_criterion_claim,
    };
    use loom_research_types::NonEmptyByteRange;
    use loom_search::UnitScore;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn criterion_receipt_persists_exact_utf8_evidence_idempotently() {
        let directory = tempdir().expect("temporary project");
        let (mut store, _) =
            ProjectStore::initialize(directory.path(), "evaluation").expect("store");
        let occurrence_id = ArtifactId::new();
        let candidate = "Mara touched the blue door.".as_bytes();
        let candidate_blob_id = store.put_blob(candidate).expect("candidate blob");
        let created_at_ms = now_unix_ms();
        let transaction = store.connection.transaction().expect("transaction");
        insert_blob_row(
            &transaction,
            candidate_blob_id,
            candidate.len(),
            created_at_ms,
        )
        .expect("register candidate");
        transaction.commit().expect("commit candidate");

        let task_id = ArtifactId::new();
        let task = DiagnosticEvaluationTaskPersistence {
            task_id,
            candidate_occurrence_id: Some(occurrence_id),
            kind: DiagnosticEvaluationKind::CriterionCard,
            pack_fingerprint: BlobId::digest(b"fiction-core-v1"),
            exact_packet_bytes: b"blind criterion packet",
        };
        let first_task = store
            .persist_diagnostic_evaluation_task(task)
            .expect("persist task");
        let second_task = store
            .persist_diagnostic_evaluation_task(task)
            .expect("repeat task");
        assert_eq!(first_task, second_task);

        let claim = CriterionObservationClaim {
            criterion_key: "prose_precision".into(),
            outcome: CriterionClaimOutcome::Score(
                UnitScore::from_millionths(800_000).expect("score"),
            ),
            evidence: vec![EvidenceSpanClaim {
                candidate_occurrence_id: occurrence_id,
                candidate_blob_id,
                range: NonEmptyByteRange::new(5, 12).expect("UTF-8 range"),
                quote: EvidenceQuote::new("touched").expect("quote"),
            }],
        };
        let observation = validate_criterion_claim(
            "prose_precision",
            CandidateEvidenceSource {
                occurrence_id,
                blob_id: candidate_blob_id,
                utf8: candidate,
            },
            &claim,
        );
        let input = DiagnosticEvaluationReceiptPersistence {
            task_id,
            exact_raw_response_bytes: br#"{"score":800000,"quote":"touched"}"#,
            observation: DiagnosticEvaluationObservation::Criterion(&observation),
        };
        let first = store
            .persist_diagnostic_evaluation_receipt(&input)
            .expect("persist receipt");
        let second = store
            .persist_diagnostic_evaluation_receipt(&input)
            .expect("repeat receipt");
        assert_eq!(first, second);
        assert_eq!(first.evidence_span_count(), 1);
        let row = store
            .connection
            .query_row(
                "SELECT start_byte, end_byte, quote_blob_id
                 FROM research_evidence_spans WHERE receipt_fingerprint = ?1",
                [first.receipt_fingerprint().to_string()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .expect("evidence row");
        assert_eq!(row.0, 5);
        assert_eq!(row.1, 12);
        assert_eq!(row.2, BlobId::digest(b"touched").to_string());
    }

    #[test]
    fn task_shape_and_lowercase_digest_constraints_fail_closed() {
        let directory = tempdir().expect("temporary project");
        let (mut store, _) =
            ProjectStore::initialize(directory.path(), "evaluation bounds").expect("store");
        let invalid =
            store.persist_diagnostic_evaluation_task(DiagnosticEvaluationTaskPersistence {
                task_id: ArtifactId::new(),
                candidate_occurrence_id: Some(ArtifactId::new()),
                kind: DiagnosticEvaluationKind::BlindPairwise,
                pack_fingerprint: BlobId::digest(b"pack"),
                exact_packet_bytes: b"packet",
            });
        assert!(matches!(
            invalid,
            Err(StoreError::InvalidResearchDiagnostic(_))
        ));

        let uppercase = "A".repeat(64);
        let result = store.connection.execute(
            "INSERT INTO research_execution_records(
                record_fingerprint, record_kind, record_blob_id, created_at_ms
             ) VALUES (?1, 'evaluation_task', ?1, 1)",
            [uppercase],
        );
        assert!(result.is_err());
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn verified_tasks_bind_exact_admitted_projection_content_and_store_session() {
        let directory = tempdir().expect("temporary project");
        let (mut store, _) =
            ProjectStore::initialize(directory.path(), "verified evaluation").expect("store");
        let first_projection =
            crate::research_admission::tests::admitted_projection_for_evaluation_test(
                &mut store, "first",
            );
        let second_projection =
            crate::research_admission::tests::admitted_projection_for_evaluation_test(
                &mut store, "second",
            );
        let first = store
            .freeze_evaluation_candidate(&first_projection)
            .expect("first exact projection lease");
        let second = store
            .freeze_evaluation_candidate(&second_projection)
            .expect("second exact projection lease");
        let first_occurrence = first.occurrence_id();
        let first_blob = first.candidate_blob_id();
        let first_binding = first.projection_binding_fingerprint();
        let first_bytes = store.read_blob(first_blob).expect("first candidate bytes");
        let second_blob = second.candidate_blob_id();
        let second_bytes = store
            .read_blob(second_blob)
            .expect("second candidate bytes");

        let task_id = ArtifactId::new();
        store
            .persist_verified_evaluation_task(VerifiedEvaluationTaskPersistence {
                task_id,
                kind: DiagnosticEvaluationKind::CriterionCard,
                pack_fingerprint: BlobId::digest(b"fiction-core-v1"),
                exact_packet_bytes: b"verified criterion packet",
                candidate: Some(first),
            })
            .expect("verified task");
        let stored_binding: (String, String, String) = store
            .connection
            .query_row(
                "SELECT evidence_authority, candidate_blob_id,
                        projection_binding_fingerprint
                 FROM research_evaluation_tasks WHERE task_id = ?1",
                [task_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("stored exact binding");
        assert_eq!(
            stored_binding,
            (
                "verified_projection".into(),
                first_blob.to_string(),
                first_binding.to_string(),
            )
        );

        let (wrong_end, wrong_quote) = first_utf8_scalar(&second_bytes);
        let wrong_claim = CriterionObservationClaim {
            criterion_key: "continuity".into(),
            outcome: CriterionClaimOutcome::Score(
                UnitScore::from_millionths(500_000).expect("score"),
            ),
            evidence: vec![EvidenceSpanClaim {
                candidate_occurrence_id: first_occurrence,
                candidate_blob_id: second_blob,
                range: NonEmptyByteRange::new(0, wrong_end).expect("wrong range"),
                quote: EvidenceQuote::new(wrong_quote).expect("wrong quote"),
            }],
        };
        let wrong_observation = validate_criterion_claim(
            "continuity",
            CandidateEvidenceSource {
                occurrence_id: first_occurrence,
                blob_id: second_blob,
                utf8: &second_bytes,
            },
            &wrong_claim,
        );
        assert!(matches!(
            store.persist_diagnostic_evaluation_receipt(&DiagnosticEvaluationReceiptPersistence {
                task_id,
                exact_raw_response_bytes: b"cross-candidate claim",
                observation: DiagnosticEvaluationObservation::Criterion(&wrong_observation),
            }),
            Err(StoreError::InvalidResearchDiagnostic(_))
        ));

        let (right_end, right_quote) = first_utf8_scalar(&first_bytes);
        let right_claim = CriterionObservationClaim {
            criterion_key: "continuity".into(),
            outcome: CriterionClaimOutcome::Score(
                UnitScore::from_millionths(500_000).expect("score"),
            ),
            evidence: vec![EvidenceSpanClaim {
                candidate_occurrence_id: first_occurrence,
                candidate_blob_id: first_blob,
                range: NonEmptyByteRange::new(0, right_end).expect("right range"),
                quote: EvidenceQuote::new(right_quote).expect("right quote"),
            }],
        };
        let right_observation = validate_criterion_claim(
            "continuity",
            CandidateEvidenceSource {
                occurrence_id: first_occurrence,
                blob_id: first_blob,
                utf8: &first_bytes,
            },
            &right_claim,
        );
        store
            .persist_diagnostic_evaluation_receipt(&DiagnosticEvaluationReceiptPersistence {
                task_id,
                exact_raw_response_bytes: b"exact candidate claim",
                observation: DiagnosticEvaluationObservation::Criterion(&right_observation),
            })
            .expect("exact candidate receipt");

        let other_directory = tempdir().expect("other temporary project");
        let (other_store, _) =
            ProjectStore::initialize(other_directory.path(), "other store").expect("other store");
        assert!(matches!(
            other_store.freeze_evaluation_candidate(&first_projection),
            Err(StoreError::InvalidResearchDiagnostic(_))
        ));
    }

    #[test]
    fn nonexistent_projection_cannot_be_inserted_as_verified_evaluation_evidence() {
        let directory = tempdir().expect("temporary project");
        let (mut store, _) =
            ProjectStore::initialize(directory.path(), "missing projection").expect("store");
        let packet = register_test_blob(&mut store, b"verified task packet");
        let candidate = register_test_blob(&mut store, b"unbound candidate");
        let record = store
            .persist_research_execution_record(
                ResearchExecutionRecordKind::EvaluationTask,
                b"nonexistent projection task record",
            )
            .expect("diagnostic record");
        let inserted = store.connection.execute(
            "INSERT INTO research_evaluation_tasks(
                task_id, candidate_occurrence_id, evaluation_kind, pack_fingerprint,
                packet_blob_id, evidence_authority, candidate_blob_id,
                projection_binding_fingerprint, record_fingerprint, created_at_ms
             ) VALUES (?1, ?2, 'criterion_card', ?3, ?4,
                       'verified_projection', ?5, ?6, ?7, ?8)",
            params![
                ArtifactId::new().to_string(),
                ArtifactId::new().to_string(),
                BlobId::digest(b"fiction-core-v1").to_string(),
                packet.to_string(),
                candidate.to_string(),
                BlobId::digest(b"invented projection binding").to_string(),
                record.fingerprint().to_string(),
                now_unix_ms(),
            ],
        );
        assert!(inserted.is_err());
    }

    #[test]
    fn diagnostic_packet_and_response_bytes_are_bounded_before_storage() {
        let directory = tempdir().expect("temporary project");
        let (mut store, _) =
            ProjectStore::initialize(directory.path(), "evaluation byte bounds").expect("store");
        let oversized_packet = vec![b'p'; MAX_DIAGNOSTIC_EVALUATION_PACKET_BYTES + 1];
        assert!(matches!(
            store.persist_diagnostic_evaluation_task(DiagnosticEvaluationTaskPersistence {
                task_id: ArtifactId::new(),
                candidate_occurrence_id: Some(ArtifactId::new()),
                kind: DiagnosticEvaluationKind::CriterionCard,
                pack_fingerprint: BlobId::digest(b"pack"),
                exact_packet_bytes: &oversized_packet,
            }),
            Err(StoreError::InvalidResearchDiagnostic(_))
        ));

        let occurrence = ArtifactId::new();
        let candidate = b"x";
        let claim = CriterionObservationClaim {
            criterion_key: "overall".into(),
            outcome: CriterionClaimOutcome::Abstain,
            evidence: Vec::new(),
        };
        let observation = validate_criterion_claim(
            "overall",
            CandidateEvidenceSource {
                occurrence_id: occurrence,
                blob_id: BlobId::digest(candidate),
                utf8: candidate,
            },
            &claim,
        );
        let oversized_response = vec![b'r'; MAX_DIAGNOSTIC_EVALUATION_RESPONSE_BYTES + 1];
        assert!(matches!(
            store.persist_diagnostic_evaluation_receipt(&DiagnosticEvaluationReceiptPersistence {
                task_id: ArtifactId::new(),
                exact_raw_response_bytes: &oversized_response,
                observation: DiagnosticEvaluationObservation::Criterion(&observation),
            }),
            Err(StoreError::InvalidResearchDiagnostic(_))
        ));
    }

    #[test]
    fn hard_gate_receipt_requires_an_exact_registered_artifact() {
        let directory = tempdir().expect("temporary project");
        let (mut store, _) =
            ProjectStore::initialize(directory.path(), "hard gate").expect("store");
        let candidate = ArtifactId::new();
        let task_id = ArtifactId::new();
        store
            .persist_diagnostic_evaluation_task(DiagnosticEvaluationTaskPersistence {
                task_id,
                candidate_occurrence_id: Some(candidate),
                kind: DiagnosticEvaluationKind::HardGate,
                pack_fingerprint: BlobId::digest(b"fiction-core-v1"),
                exact_packet_bytes: b"hard-gate packet",
            })
            .expect("task");

        let receipt_artifact = ArtifactId::new();
        let receipt_bytes = b"exact provenance receipt";
        let receipt_blob = register_test_blob(&mut store, receipt_bytes);
        store
            .connection
            .execute(
                "INSERT INTO artifacts(
                    artifact_id, blob_id, artifact_kind, media_type,
                    metadata_json, created_at_ms
                 ) VALUES (?1, ?2, 'research_receipt', 'application/json', '{}', ?3)",
                params![
                    receipt_artifact.to_string(),
                    receipt_blob.to_string(),
                    now_unix_ms(),
                ],
            )
            .expect("artifact");
        let observation = CoreHardGateObservation::new(
            ArtifactId::new(),
            candidate,
            CoreHardGate::Provenance,
            HardGateOutcome::Pass,
            HardGateEvidence::Receipt {
                artifact_id: receipt_artifact,
                blob_id: receipt_blob,
            },
        )
        .expect("hard-gate observation");
        let persisted = store
            .persist_diagnostic_evaluation_receipt(&DiagnosticEvaluationReceiptPersistence {
                task_id,
                exact_raw_response_bytes: b"pass",
                observation: DiagnosticEvaluationObservation::HardGate(&observation),
            })
            .expect("hard-gate receipt");
        let outcome: String = store
            .connection
            .query_row(
                "SELECT outcome FROM research_evaluation_receipts
                 WHERE receipt_fingerprint = ?1",
                [persisted.receipt_fingerprint().to_string()],
                |row| row.get(0),
            )
            .expect("outcome");
        assert_eq!(outcome, "validated");
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn arbitrary_blind_input_remains_claimed_unknown_and_cannot_mint_a_source() {
        let directory = tempdir().expect("temporary project");
        let (mut store, _) =
            ProjectStore::initialize(directory.path(), "blind preference").expect("store");
        let task_id = ArtifactId::new();
        let first_occurrence = ArtifactId::new();
        let second_occurrence = ArtifactId::new();
        let first_bytes = b"Left.";
        let second_bytes = b"Right.";
        let first_blob = register_test_blob(&mut store, first_bytes);
        let second_blob = register_test_blob(&mut store, second_bytes);
        let assignment = build_blind_pair(BlindPairSpec {
            task_id,
            left: BlindCandidateInput {
                occurrence_id: first_occurrence,
                blob_id: first_blob,
                utf8: first_bytes,
            },
            right: BlindCandidateInput {
                occurrence_id: second_occurrence,
                blob_id: second_blob,
                utf8: second_bytes,
            },
            reverse_candidates: false,
            criterion_order: vec!["overall".into()],
        })
        .expect("blind assignment");
        store
            .persist_diagnostic_evaluation_task(DiagnosticEvaluationTaskPersistence {
                task_id,
                candidate_occurrence_id: None,
                kind: DiagnosticEvaluationKind::BlindPairwise,
                pack_fingerprint: BlobId::digest(b"fiction-core-v1"),
                exact_packet_bytes: b"blind packet",
            })
            .expect("task");
        let persisted_assignment = store
            .persist_diagnostic_pairwise_assignment(DiagnosticPairwiseAssignmentPersistence {
                task_id,
                first_occurrence_id: first_occurrence,
                second_occurrence_id: second_occurrence,
                label_map_fingerprint: assignment.mapping_fingerprint(),
                order_cell: false,
                criterion_order_cell: false,
                anchor_order_cell: false,
            })
            .expect("assignment");
        let evidence = vec![
            BlindEvidenceSpanClaim {
                label: assignment.packet().first().label(),
                range: NonEmptyByteRange::new(0, 5).expect("range"),
                quote: EvidenceQuote::new("Left.").expect("quote"),
            },
            BlindEvidenceSpanClaim {
                label: assignment.packet().second().label(),
                range: NonEmptyByteRange::new(0, 6).expect("range"),
                quote: EvidenceQuote::new("Right.").expect("quote"),
            },
        ];
        let winner = validate_blind_pair_judgment(
            &assignment,
            &BlindPairJudgmentClaim {
                verdict: BlindPairVerdictClaim::Winner(assignment.packet().first().label()),
                evidence: evidence.clone(),
            },
        );
        let receipt = store
            .persist_diagnostic_evaluation_receipt(&DiagnosticEvaluationReceiptPersistence {
                task_id,
                exact_raw_response_bytes: b"winner first",
                observation: DiagnosticEvaluationObservation::BlindPairwise(&winner),
            })
            .expect("receipt");
        let receipt_authority: (String, String) = store
            .connection
            .query_row(
                "SELECT evaluator_class, receipt_authority
                 FROM research_evaluation_receipts WHERE receipt_fingerprint = ?1",
                [receipt.receipt_fingerprint().to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("receipt authority");
        assert_eq!(
            receipt_authority,
            ("claimed_unknown".into(), "claimed_diagnostic".into())
        );
        let tie = validate_blind_pair_judgment(
            &assignment,
            &BlindPairJudgmentClaim {
                verdict: BlindPairVerdictClaim::Tie,
                evidence,
            },
        );
        assert!(matches!(
            store.persist_diagnostic_preference_label(
                persisted_assignment.assignment_fingerprint(),
                receipt.receipt_fingerprint(),
                &tie,
            ),
            Err(StoreError::InvalidResearchDiagnostic(_))
        ));
        store
            .persist_diagnostic_preference_label(
                persisted_assignment.assignment_fingerprint(),
                receipt.receipt_fingerprint(),
                &winner,
            )
            .expect("bound preference");
        let source: (String, Option<String>) = store
            .connection
            .query_row(
                "SELECT label_source, source_verifier_fingerprint
                 FROM research_preference_labels WHERE receipt_fingerprint = ?1",
                [receipt.receipt_fingerprint().to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("diagnostic source");
        assert_eq!(source, ("claimed_unknown".into(), None));

        let forged_record = store
            .persist_research_execution_record(
                ResearchExecutionRecordKind::PreferenceLabel,
                b"forged frontier label record",
            )
            .expect("forged record remains diagnostic evidence");
        let forged_frontier = store.connection.execute(
            "INSERT INTO research_preference_labels(
                label_fingerprint, assignment_fingerprint, receipt_fingerprint,
                label_source, preference, source_verifier_fingerprint,
                record_fingerprint, created_at_ms
             ) VALUES (?1, ?2, ?3, 'frontier_weak', 'first', ?4, ?1, ?5)",
            params![
                forged_record.fingerprint().to_string(),
                persisted_assignment.assignment_fingerprint().to_string(),
                receipt.receipt_fingerprint().to_string(),
                BlobId::digest(b"forged verifier").to_string(),
                now_unix_ms(),
            ],
        );
        assert!(forged_frontier.is_err());
    }

    fn register_test_blob(store: &mut ProjectStore, bytes: &[u8]) -> BlobId {
        let blob_id = store.put_blob(bytes).expect("blob");
        let transaction = store.connection.transaction().expect("transaction");
        insert_blob_row(&transaction, blob_id, bytes.len(), now_unix_ms()).expect("register blob");
        transaction.commit().expect("commit blob");
        blob_id
    }

    fn first_utf8_scalar(bytes: &[u8]) -> (u64, &str) {
        let text = std::str::from_utf8(bytes).expect("candidate UTF-8");
        let first = text.chars().next().expect("nonempty candidate");
        let end = first.len_utf8();
        (u64::try_from(end).expect("scalar length"), &text[..end])
    }
}
