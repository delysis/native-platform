//! Diagnostic persistence for exact prompt masks and grounded backtranslation inputs.
//!
//! Rows written here are immutable evidence only. They never recreate the
//! move-only mask, inference, evaluator, or accepted-demonstration authority
//! that was present while the record was checked.

use std::collections::{BTreeMap, BTreeSet};

use loom_research_types::{
    AppliedSurfacePromptMask, AuditionedBacktranslation, BacktranslationAuditionCase,
    BacktranslationProposal, CampaignId, CapabilityBoundFimMask, PromptSourceRange, StageAttemptId,
    SurfaceMaskKind,
};
use loom_types::{BlobId, RevisionId, now_unix_ms};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::Serialize;

use crate::provenance::insert_blob_row;
use crate::store::StoreSessionNonce;
use crate::{
    AdoptedInferenceBatch, PersistedDiagnosticEvaluationReceipt, ProjectStore,
    ResearchExecutionRecordKind, Result, StoreError,
};

const MAX_BACKTRANSLATION_EVIDENCE_ITEMS: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticPromptMaskKind {
    Entity,
    Beat,
    State,
    ContentStyle,
    Suffix,
    ModelSpecificFim,
}

impl DiagnosticPromptMaskKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Entity => "entity",
            Self::Beat => "beat",
            Self::State => "state",
            Self::ContentStyle => "content_style",
            Self::Suffix => "suffix",
            Self::ModelSpecificFim => "model_specific_fim",
        }
    }
}

impl From<SurfaceMaskKind> for DiagnosticPromptMaskKind {
    fn from(kind: SurfaceMaskKind) -> Self {
        match kind {
            SurfaceMaskKind::Entity => Self::Entity,
            SurfaceMaskKind::Beat => Self::Beat,
            SurfaceMaskKind::State => Self::State,
            SurfaceMaskKind::ContentStyle => Self::ContentStyle,
            SurfaceMaskKind::Suffix => Self::Suffix,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PersistedDiagnosticPromptMask {
    mask_fingerprint: BlobId,
    record_fingerprint: BlobId,
    kind: DiagnosticPromptMaskKind,
    source_blob_id: BlobId,
    rendered_blob_id: Option<BlobId>,
    backend_capability_fingerprint: Option<BlobId>,
}

impl PersistedDiagnosticPromptMask {
    pub const fn mask_fingerprint(self) -> BlobId {
        self.mask_fingerprint
    }

    pub const fn record_fingerprint(self) -> BlobId {
        self.record_fingerprint
    }

    pub const fn kind(self) -> DiagnosticPromptMaskKind {
        self.kind
    }

    pub const fn source_blob_id(self) -> BlobId {
        self.source_blob_id
    }

    pub const fn rendered_blob_id(self) -> Option<BlobId> {
        self.rendered_blob_id
    }

    pub const fn backend_capability_fingerprint(self) -> Option<BlobId> {
        self.backend_capability_fingerprint
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PersistedDiagnosticBacktranslationProposal {
    proposal_fingerprint: BlobId,
    record_fingerprint: BlobId,
    source_revision_id: RevisionId,
    source_blob_id: BlobId,
    grounded_field_count: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PersistedDiagnosticBacktranslationAudition {
    audition_fingerprint: BlobId,
    record_fingerprint: BlobId,
    writer_evidence_fingerprint: BlobId,
    evaluator_evidence_fingerprint: BlobId,
    writer_batch_count: u32,
    evaluator_receipt_count: u32,
}

/// Opaque live evaluator-verifier authority required for accepting an
/// audition as a reusable demonstration. No production constructor exists
/// until a backend evaluator receipt verifier can mint this exact binding.
/// Persisted evaluation rows and diagnostic receipt values cannot recreate it.
///
/// ```compile_fail
/// fn needs_clone<T: Clone>() {}
/// needs_clone::<loom_store::VerifiedBacktranslationEvaluatorLease>();
/// ```
///
/// ```compile_fail
/// fn needs_deserialize<T: serde::de::DeserializeOwned>() {}
/// needs_deserialize::<loom_store::VerifiedBacktranslationEvaluatorLease>();
/// ```
pub struct VerifiedBacktranslationEvaluatorLease {
    session_nonce: StoreSessionNonce,
    audition_fingerprint: BlobId,
    ordered_receipts: Vec<BlobId>,
    evaluator_evidence_fingerprint: BlobId,
}

impl std::fmt::Debug for VerifiedBacktranslationEvaluatorLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VerifiedBacktranslationEvaluatorLease")
            .field("audition_fingerprint", &self.audition_fingerprint)
            .field("receipt_count", &self.ordered_receipts.len())
            .field(
                "evaluator_evidence_fingerprint",
                &self.evaluator_evidence_fingerprint,
            )
            .finish_non_exhaustive()
    }
}

impl VerifiedBacktranslationEvaluatorLease {
    #[cfg(test)]
    fn for_test(
        session_nonce: StoreSessionNonce,
        audition_fingerprint: BlobId,
        ordered_receipts: Vec<BlobId>,
    ) -> Self {
        let evaluator_evidence_fingerprint = ordered_evidence_fingerprint(
            b"loom/backtranslation-evaluator-evidence/v1\0",
            &ordered_receipts,
        );
        Self {
            session_nonce,
            audition_fingerprint,
            ordered_receipts,
            evaluator_evidence_fingerprint,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(clippy::struct_field_names)]
pub struct PersistedDiagnosticBacktranslationAcceptance {
    acceptance_fingerprint: BlobId,
    audition_fingerprint: BlobId,
    record_fingerprint: BlobId,
}

impl PersistedDiagnosticBacktranslationAcceptance {
    pub const fn acceptance_fingerprint(self) -> BlobId {
        self.acceptance_fingerprint
    }

    pub const fn audition_fingerprint(self) -> BlobId {
        self.audition_fingerprint
    }

    pub const fn record_fingerprint(self) -> BlobId {
        self.record_fingerprint
    }
}

impl PersistedDiagnosticBacktranslationAudition {
    pub const fn audition_fingerprint(self) -> BlobId {
        self.audition_fingerprint
    }

    pub const fn record_fingerprint(self) -> BlobId {
        self.record_fingerprint
    }

    pub const fn writer_evidence_fingerprint(self) -> BlobId {
        self.writer_evidence_fingerprint
    }

    pub const fn evaluator_evidence_fingerprint(self) -> BlobId {
        self.evaluator_evidence_fingerprint
    }

    pub const fn writer_batch_count(self) -> u32 {
        self.writer_batch_count
    }

    pub const fn evaluator_receipt_count(self) -> u32 {
        self.evaluator_receipt_count
    }
}

impl PersistedDiagnosticBacktranslationProposal {
    pub const fn proposal_fingerprint(self) -> BlobId {
        self.proposal_fingerprint
    }

    pub const fn record_fingerprint(self) -> BlobId {
        self.record_fingerprint
    }

    pub const fn source_revision_id(self) -> RevisionId {
        self.source_revision_id
    }

    pub const fn source_blob_id(self) -> BlobId {
        self.source_blob_id
    }

    pub const fn grounded_field_count(self) -> u32 {
        self.grounded_field_count
    }
}

#[derive(Serialize)]
struct SurfaceMaskRecord<'a> {
    format: &'static str,
    campaign_id: CampaignId,
    stage_attempt_id: StageAttemptId,
    plan: &'a loom_research_types::SurfacePromptMaskPlan,
    rendered_blob_id: BlobId,
    applied_fingerprint: BlobId,
}

#[derive(Serialize)]
struct FimMaskRecord<'a> {
    format: &'static str,
    campaign_id: CampaignId,
    stage_attempt_id: StageAttemptId,
    plan: &'a loom_research_types::ModelSpecificFimMaskPlan,
    capability_receipt: &'a loom_research_types::FimCapabilityReceipt,
    binding_fingerprint: BlobId,
}

#[derive(Serialize)]
struct BacktranslationAuditionRecord<'a> {
    format: &'static str,
    proposal_fingerprint: BlobId,
    audition_receipt: &'a loom_research_types::BacktranslationAuditionReceipt,
    audition_fingerprint: BlobId,
    ordered_writer_batches: &'a [BlobId],
    writer_evidence_fingerprint: BlobId,
    ordered_evaluator_receipts: &'a [BlobId],
    evaluator_evidence_fingerprint: BlobId,
}

#[derive(Serialize)]
struct BacktranslationAcceptanceRecord {
    format: &'static str,
    audition_fingerprint: BlobId,
    writer_evidence_fingerprint: BlobId,
    evaluator_evidence_fingerprint: BlobId,
    acceptance_fingerprint: BlobId,
}

impl ProjectStore {
    /// Replays and persists one visible source mask. The returned value is a
    /// diagnostic receipt, not a prompt-compilation capability.
    pub fn persist_diagnostic_surface_prompt_mask(
        &mut self,
        campaign_id: CampaignId,
        stage_attempt_id: StageAttemptId,
        applied: &AppliedSurfacePromptMask,
    ) -> Result<PersistedDiagnosticPromptMask> {
        ensure_stage_attempt_campaign(&self.connection, campaign_id, stage_attempt_id)?;
        let source = applied.plan().source();
        let exact_source = exact_project_source(self, source)?;
        let replayed = applied
            .plan()
            .clone()
            .apply(source.revision_id(), &exact_source)
            .map_err(invalid_prompt_evidence)?;
        if replayed.fingerprint() != applied.fingerprint()
            || replayed.rendered_blob_id() != applied.rendered_blob_id()
            || replayed.rendered_bytes() != applied.rendered_bytes()
        {
            return Err(invalid_prompt_evidence(
                "surface mask differs from exact deterministic replay",
            ));
        }
        let rendered_blob_id = register_diagnostic_blob(self, applied.rendered_bytes())?;
        if rendered_blob_id != applied.rendered_blob_id() {
            return Err(invalid_prompt_evidence(
                "surface mask rendered bytes do not match their fingerprint",
            ));
        }
        let canonical = serde_json::to_vec(&SurfaceMaskRecord {
            format: "loom.diagnostic-surface-prompt-mask.v1",
            campaign_id,
            stage_attempt_id,
            plan: applied.plan(),
            rendered_blob_id,
            applied_fingerprint: applied.fingerprint(),
        })?;
        persist_prompt_mask(
            self,
            &PromptMaskRow {
                mask_fingerprint: applied.fingerprint(),
                campaign_id,
                stage_attempt_id,
                kind: applied.plan().kind().into(),
                source_blob_id: source.source_blob_id(),
                rendered_blob_id: Some(rendered_blob_id),
                backend_capability_fingerprint: None,
                canonical: &canonical,
            },
        )
    }

    /// Persists a source-checked FIM plan/capability claim without inventing
    /// rendered control bytes. This remains diagnostic until a native backend
    /// verifier authenticates the capability receipt used to build `binding`.
    pub fn persist_diagnostic_fim_prompt_mask(
        &mut self,
        campaign_id: CampaignId,
        stage_attempt_id: StageAttemptId,
        binding: &CapabilityBoundFimMask,
        exact_backend_receipt_bytes: &[u8],
    ) -> Result<PersistedDiagnosticPromptMask> {
        if exact_backend_receipt_bytes.is_empty() {
            return Err(invalid_prompt_evidence(
                "FIM backend receipt bytes cannot be empty",
            ));
        }
        ensure_stage_attempt_campaign(&self.connection, campaign_id, stage_attempt_id)?;
        let source = binding.plan().source();
        let exact_source = exact_project_source(self, source)?;
        binding
            .verify_source(source.revision_id(), &exact_source)
            .map_err(invalid_prompt_evidence)?;
        let backend_receipt_blob_id = register_diagnostic_blob(self, exact_backend_receipt_bytes)?;
        if backend_receipt_blob_id != binding.receipt().backend_receipt_blob_id() {
            return Err(invalid_prompt_evidence(
                "FIM backend receipt bytes differ from the bound receipt",
            ));
        }
        let canonical = serde_json::to_vec(&FimMaskRecord {
            format: "loom.diagnostic-model-specific-fim-mask.v1",
            campaign_id,
            stage_attempt_id,
            plan: binding.plan(),
            capability_receipt: binding.receipt(),
            binding_fingerprint: binding.fingerprint(),
        })?;
        persist_prompt_mask(
            self,
            &PromptMaskRow {
                mask_fingerprint: binding.fingerprint(),
                campaign_id,
                stage_attempt_id,
                kind: DiagnosticPromptMaskKind::ModelSpecificFim,
                source_blob_id: source.source_blob_id(),
                rendered_blob_id: None,
                backend_capability_fingerprint: Some(binding.receipt().capability_fingerprint()),
                canonical: &canonical,
            },
        )
    }

    /// Persists one source-verified proposal, including valid complete
    /// abstentions with zero grounded fields. This does not validate or mint
    /// controller-call authority.
    pub fn persist_diagnostic_backtranslation_proposal(
        &mut self,
        proposal: &BacktranslationProposal,
    ) -> Result<PersistedDiagnosticBacktranslationProposal> {
        let source = proposal.source();
        let exact_source = exact_project_source(self, source)?;
        proposal
            .verify_source(source.revision_id(), &exact_source)
            .map_err(invalid_backtranslation)?;
        let grounded_field_count = grounded_backtranslation_field_count(proposal)?;
        let canonical = serde_json::to_vec(proposal)?;
        let record = self.persist_research_execution_record(
            ResearchExecutionRecordKind::BacktranslationProposal,
            &canonical,
        )?;
        let created_at_ms = now_unix_ms();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT OR IGNORE INTO research_backtranslation_proposals(
                proposal_fingerprint, source_revision_id, source_blob_id,
                source_start_byte, source_end_byte, field_count,
                record_fingerprint, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                proposal.fingerprint().to_string(),
                source.revision_id().to_string(),
                source.source_blob_id().to_string(),
                checked_i64(source.range().start(), "backtranslation source start")?,
                checked_i64(source.range().end(), "backtranslation source end")?,
                i64::from(grounded_field_count),
                record.fingerprint().to_string(),
                created_at_ms,
            ],
        )?;
        let exact: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM research_backtranslation_proposals
             WHERE proposal_fingerprint = ?1 AND source_revision_id = ?2
               AND source_blob_id = ?3 AND source_start_byte = ?4
               AND source_end_byte = ?5 AND field_count = ?6
               AND record_fingerprint = ?7",
            params![
                proposal.fingerprint().to_string(),
                source.revision_id().to_string(),
                source.source_blob_id().to_string(),
                checked_i64(source.range().start(), "backtranslation source start")?,
                checked_i64(source.range().end(), "backtranslation source end")?,
                i64::from(grounded_field_count),
                record.fingerprint().to_string(),
            ],
            |row| row.get(0),
        )?;
        if exact != 1 {
            return Err(invalid_backtranslation(
                "proposal fingerprint conflicts with persisted evidence",
            ));
        }
        transaction.commit()?;
        Ok(PersistedDiagnosticBacktranslationProposal {
            proposal_fingerprint: proposal.fingerprint(),
            record_fingerprint: record.fingerprint(),
            source_revision_id: source.revision_id(),
            source_blob_id: source.source_blob_id(),
            grounded_field_count,
        })
    }

    /// Persists one structurally passing audition after matching every cited
    /// writer call/output to live, session-bound adopted batches and every
    /// cited evaluator identity to a typed diagnostic receipt returned by this
    /// store API. The evaluator receipts are still reconstructible diagnostics,
    /// so this method deliberately returns no acceptance capability.
    #[allow(clippy::too_many_lines)]
    pub fn persist_diagnostic_backtranslation_audition(
        &mut self,
        auditioned: &AuditionedBacktranslation,
        writer_batches: &[&AdoptedInferenceBatch],
        evaluator_receipts: &[PersistedDiagnosticEvaluationReceipt],
    ) -> Result<PersistedDiagnosticBacktranslationAudition> {
        ensure_exact_proposal(self, auditioned.proposal())?;
        let source = auditioned.proposal().source();
        let exact_source = exact_project_source(self, source)?;
        auditioned
            .proposal()
            .verify_source(source.revision_id(), &exact_source)
            .map_err(invalid_backtranslation)?;
        for case in auditioned.receipt().cases() {
            let _ = exact_project_source(self, case.source())?;
        }

        let writer = validate_live_writer_evidence(self, auditioned, writer_batches)?;
        let ordered_evaluators = validate_diagnostic_evaluator_evidence(
            &self.connection,
            auditioned,
            evaluator_receipts,
        )?;
        let evaluator_evidence_fingerprint = ordered_evidence_fingerprint(
            b"loom/backtranslation-evaluator-evidence/v1\0",
            &ordered_evaluators,
        );
        let canonical = serde_json::to_vec(&BacktranslationAuditionRecord {
            format: "loom.diagnostic-backtranslation-audition.v1",
            proposal_fingerprint: auditioned.proposal().fingerprint(),
            audition_receipt: auditioned.receipt(),
            audition_fingerprint: auditioned.fingerprint(),
            ordered_writer_batches: &writer.ordered_batches,
            writer_evidence_fingerprint: writer.fingerprint,
            ordered_evaluator_receipts: &ordered_evaluators,
            evaluator_evidence_fingerprint,
        })?;
        let record = self.persist_research_execution_record(
            ResearchExecutionRecordKind::BacktranslationAudition,
            &canonical,
        )?;
        let writer_batch_count = checked_u32(
            writer.ordered_batches.len(),
            "backtranslation writer batch count",
        )?;
        let evaluator_receipt_count = checked_u32(
            ordered_evaluators.len(),
            "backtranslation evaluator receipt count",
        )?;
        let created_at_ms = now_unix_ms();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (ordinal, fingerprint) in writer.ordered_batches.iter().enumerate() {
            transaction.execute(
                "INSERT OR IGNORE INTO research_backtranslation_audition_batches(
                    audition_fingerprint, batch_ordinal, batch_verification_fingerprint
                 ) VALUES (?1, ?2, ?3)",
                params![
                    auditioned.fingerprint().to_string(),
                    checked_i64_usize(ordinal, "writer batch ordinal")?,
                    fingerprint.to_string(),
                ],
            )?;
        }
        for (ordinal, fingerprint) in ordered_evaluators.iter().enumerate() {
            transaction.execute(
                "INSERT OR IGNORE INTO research_backtranslation_audition_evaluator_receipts(
                    audition_fingerprint, receipt_ordinal, evaluator_receipt_fingerprint
                 ) VALUES (?1, ?2, ?3)",
                params![
                    auditioned.fingerprint().to_string(),
                    checked_i64_usize(ordinal, "evaluator receipt ordinal")?,
                    fingerprint.to_string(),
                ],
            )?;
        }
        transaction.execute(
            "INSERT OR IGNORE INTO research_backtranslation_auditions(
                audition_fingerprint, proposal_fingerprint,
                writer_evidence_fingerprint, writer_batch_count,
                evaluator_evidence_fingerprint, evaluator_receipt_count,
                work_disjoint, causal_transfer_decision, leakage_decision,
                record_fingerprint, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 'improved', 'clear', ?7, ?8)",
            params![
                auditioned.fingerprint().to_string(),
                auditioned.proposal().fingerprint().to_string(),
                writer.fingerprint.to_string(),
                i64::from(writer_batch_count),
                evaluator_evidence_fingerprint.to_string(),
                i64::from(evaluator_receipt_count),
                record.fingerprint().to_string(),
                created_at_ms,
            ],
        )?;
        ensure_exact_audition_rows(
            &transaction,
            auditioned.fingerprint(),
            auditioned.proposal().fingerprint(),
            &writer,
            &ordered_evaluators,
            evaluator_evidence_fingerprint,
            record.fingerprint(),
        )?;
        transaction.commit()?;
        Ok(PersistedDiagnosticBacktranslationAudition {
            audition_fingerprint: auditioned.fingerprint(),
            record_fingerprint: record.fingerprint(),
            writer_evidence_fingerprint: writer.fingerprint,
            evaluator_evidence_fingerprint,
            writer_batch_count,
            evaluator_receipt_count,
        })
    }

    /// Consumes the exact structural audition, every live adopted writer batch,
    /// and an opaque evaluator-verifier lease before recording acceptance.
    /// This entrypoint is intentionally unreachable in production until the
    /// evaluator backend can mint `VerifiedBacktranslationEvaluatorLease`.
    #[allow(clippy::needless_pass_by_value)]
    pub fn persist_diagnostic_backtranslation_acceptance(
        &mut self,
        auditioned: AuditionedBacktranslation,
        writer_batches: Vec<AdoptedInferenceBatch>,
        evaluator: VerifiedBacktranslationEvaluatorLease,
    ) -> Result<PersistedDiagnosticBacktranslationAcceptance> {
        let batch_refs = writer_batches.iter().collect::<Vec<_>>();
        let writer = validate_live_writer_evidence(self, &auditioned, &batch_refs)?;
        let ordered_receipts = auditioned
            .receipt()
            .cases()
            .iter()
            .map(BacktranslationAuditionCase::evaluator_receipt_fingerprint)
            .collect::<Vec<_>>();
        if evaluator.session_nonce != self.session_nonce
            || evaluator.audition_fingerprint != auditioned.fingerprint()
            || evaluator.ordered_receipts != ordered_receipts
            || evaluator.evaluator_evidence_fingerprint
                != ordered_evidence_fingerprint(
                    b"loom/backtranslation-evaluator-evidence/v1\0",
                    &ordered_receipts,
                )
        {
            return Err(invalid_backtranslation(
                "live evaluator authority differs from the exact audition",
            ));
        }
        ensure_persisted_audition_for_acceptance(
            &self.connection,
            auditioned.fingerprint(),
            &writer,
            &ordered_receipts,
            evaluator.evaluator_evidence_fingerprint,
        )?;
        let acceptance_fingerprint = acceptance_fingerprint(
            auditioned.fingerprint(),
            writer.fingerprint,
            evaluator.evaluator_evidence_fingerprint,
        );
        let canonical = serde_json::to_vec(&BacktranslationAcceptanceRecord {
            format: "loom.diagnostic-backtranslation-acceptance.v1",
            audition_fingerprint: auditioned.fingerprint(),
            writer_evidence_fingerprint: writer.fingerprint,
            evaluator_evidence_fingerprint: evaluator.evaluator_evidence_fingerprint,
            acceptance_fingerprint,
        })?;
        let record = self.persist_research_execution_record(
            ResearchExecutionRecordKind::BacktranslationAcceptance,
            &canonical,
        )?;
        let created_at_ms = now_unix_ms();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT OR IGNORE INTO research_backtranslation_acceptances(
                acceptance_fingerprint, audition_fingerprint,
                record_fingerprint, accepted_at_ms
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                acceptance_fingerprint.to_string(),
                auditioned.fingerprint().to_string(),
                record.fingerprint().to_string(),
                created_at_ms,
            ],
        )?;
        let exact: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM research_backtranslation_acceptances
             WHERE acceptance_fingerprint = ?1 AND audition_fingerprint = ?2
               AND record_fingerprint = ?3",
            params![
                acceptance_fingerprint.to_string(),
                auditioned.fingerprint().to_string(),
                record.fingerprint().to_string(),
            ],
            |row| row.get(0),
        )?;
        if exact != 1 {
            return Err(invalid_backtranslation(
                "acceptance fingerprint conflicts with persisted evidence",
            ));
        }
        transaction.commit()?;
        Ok(PersistedDiagnosticBacktranslationAcceptance {
            acceptance_fingerprint,
            audition_fingerprint: auditioned.fingerprint(),
            record_fingerprint: record.fingerprint(),
        })
    }
}

struct WriterEvidenceBinding {
    ordered_batches: Vec<BlobId>,
    fingerprint: BlobId,
}

#[derive(Clone, Copy)]
struct WriterCallEvidence {
    batch_fingerprint: BlobId,
    model_fingerprint: BlobId,
    tokenizer_fingerprint: BlobId,
    prompt_fingerprint: BlobId,
    raw_output_blob_id: BlobId,
    displayed_output_blob_id: Option<BlobId>,
}

fn validate_live_writer_evidence(
    store: &ProjectStore,
    auditioned: &AuditionedBacktranslation,
    batches: &[&AdoptedInferenceBatch],
) -> Result<WriterEvidenceBinding> {
    if batches.is_empty() || batches.len() > MAX_BACKTRANSLATION_EVIDENCE_ITEMS {
        return Err(invalid_backtranslation(
            "writer evidence batch count is outside 1..=256",
        ));
    }
    let mut input_order = Vec::with_capacity(batches.len());
    let mut batch_ids = BTreeSet::new();
    let mut calls = BTreeMap::new();
    for batch in batches {
        if !batch.belongs_to_session(store.session_nonce) {
            return Err(invalid_backtranslation(
                "writer batch belongs to another project-store session",
            ));
        }
        let batch_fingerprint = batch.verification_fingerprint();
        if !batch_ids.insert(batch_fingerprint) {
            return Err(invalid_backtranslation(
                "writer evidence repeats a verified batch",
            ));
        }
        input_order.push(batch_fingerprint);
        load_batch_writer_calls(&store.connection, batch_fingerprint, &mut calls)?;
    }

    let canonical_order =
        validate_writer_receipt_bindings(auditioned.receipt().cases(), &input_order, &calls)?;
    Ok(WriterEvidenceBinding {
        fingerprint: ordered_evidence_fingerprint(
            b"loom/backtranslation-writer-evidence/v1\0",
            &canonical_order,
        ),
        ordered_batches: canonical_order,
    })
}

fn validate_writer_receipt_bindings(
    cases: &[BacktranslationAuditionCase],
    input_order: &[BlobId],
    calls: &BTreeMap<BlobId, WriterCallEvidence>,
) -> Result<Vec<BlobId>> {
    let mut canonical_order = Vec::new();
    let mut seen_batches = BTreeSet::new();
    for case in cases {
        let treated = calls.get(&case.call_fingerprint()).ok_or_else(|| {
            invalid_backtranslation("treated writer call is absent from live adopted batches")
        })?;
        validate_treated_writer_case(case, *treated)?;
        append_batch_once(
            treated.batch_fingerprint,
            &mut canonical_order,
            &mut seen_batches,
        );

        let baseline = calls
            .get(&case.baseline_call_fingerprint())
            .ok_or_else(|| {
                invalid_backtranslation("baseline writer call is absent from live adopted batches")
            })?;
        validate_baseline_writer_case(case, *baseline)?;
        append_batch_once(
            baseline.batch_fingerprint,
            &mut canonical_order,
            &mut seen_batches,
        );
    }
    if canonical_order != input_order {
        return Err(invalid_backtranslation(
            "writer batches are missing, extra, or not in first-use receipt order",
        ));
    }
    Ok(canonical_order)
}

fn load_batch_writer_calls(
    connection: &rusqlite::Connection,
    batch_fingerprint: BlobId,
    calls: &mut BTreeMap<BlobId, WriterCallEvidence>,
) -> Result<()> {
    let sealed: i64 = connection.query_row(
        "SELECT COUNT(*) FROM research_verified_inference_batch_seals
         WHERE batch_verification_fingerprint = ?1",
        [batch_fingerprint.to_string()],
        |row| row.get(0),
    )?;
    if sealed != 1 {
        return Err(invalid_backtranslation(
            "live writer batch has no exact persisted seal",
        ));
    }
    let mut statement = connection.prepare(
        "SELECT item.case_verification_fingerprint,
                call.model_fingerprint, call.tokenizer_fingerprint,
                call.prompt_fingerprint, terminal.raw_output_blob_id,
                completed.displayed_output_blob_id
         FROM research_verified_inference_batch_calls item
         JOIN research_model_calls call USING (call_id)
         JOIN research_call_terminals terminal USING (call_id)
         JOIN research_completed_call_evidence completed USING (call_id)
         WHERE item.batch_verification_fingerprint = ?1
           AND item.outcome = 'completed'
         ORDER BY item.position",
    )?;
    let rows = statement.query_map([batch_fingerprint.to_string()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
        ))
    })?;
    for row in rows {
        let (verification, model, tokenizer, prompt, raw_output, displayed_output) = row?;
        let verification = parse_fingerprint(&verification, "writer call verification")?;
        let evidence = WriterCallEvidence {
            batch_fingerprint,
            model_fingerprint: parse_fingerprint(&model, "writer model")?,
            tokenizer_fingerprint: parse_fingerprint(&tokenizer, "writer tokenizer")?,
            prompt_fingerprint: parse_fingerprint(&prompt, "writer prompt")?,
            raw_output_blob_id: parse_fingerprint(&raw_output, "writer raw output")?,
            displayed_output_blob_id: displayed_output
                .map(|value| parse_fingerprint(&value, "writer displayed output"))
                .transpose()?,
        };
        if calls.insert(verification, evidence).is_some() {
            return Err(invalid_backtranslation(
                "writer call verification fingerprint is repeated across batches",
            ));
        }
    }
    Ok(())
}

fn validate_treated_writer_case(
    case: &BacktranslationAuditionCase,
    evidence: WriterCallEvidence,
) -> Result<()> {
    if evidence.model_fingerprint != case.writer_model_fingerprint()
        || evidence.tokenizer_fingerprint != case.writer_tokenizer_fingerprint()
        || evidence.prompt_fingerprint != case.prompt_fingerprint()
        || evidence.raw_output_blob_id != case.raw_output_blob_id()
        || evidence.displayed_output_blob_id != Some(case.selected_output_blob_id())
    {
        return Err(invalid_backtranslation(
            "treated writer call/model/prompt/raw/selected output binding differs",
        ));
    }
    Ok(())
}

fn validate_baseline_writer_case(
    case: &BacktranslationAuditionCase,
    evidence: WriterCallEvidence,
) -> Result<()> {
    if evidence.model_fingerprint != case.writer_model_fingerprint()
        || evidence.tokenizer_fingerprint != case.writer_tokenizer_fingerprint()
        || evidence.displayed_output_blob_id != Some(case.baseline_output_blob_id())
    {
        return Err(invalid_backtranslation(
            "baseline writer call/model/selected output binding differs",
        ));
    }
    Ok(())
}

fn append_batch_once(fingerprint: BlobId, ordered: &mut Vec<BlobId>, seen: &mut BTreeSet<BlobId>) {
    if seen.insert(fingerprint) {
        ordered.push(fingerprint);
    }
}

fn validate_diagnostic_evaluator_evidence(
    connection: &rusqlite::Connection,
    auditioned: &AuditionedBacktranslation,
    receipts: &[PersistedDiagnosticEvaluationReceipt],
) -> Result<Vec<BlobId>> {
    let expected = auditioned.receipt().cases();
    if receipts.len() != expected.len() || receipts.len() > MAX_BACKTRANSLATION_EVIDENCE_ITEMS {
        return Err(invalid_backtranslation(
            "evaluator receipt count does not exactly cover audition cases",
        ));
    }
    let mut ordered = Vec::with_capacity(receipts.len());
    let mut seen = BTreeSet::new();
    for (case, receipt) in expected.iter().zip(receipts) {
        let fingerprint = receipt.receipt_fingerprint();
        if fingerprint != case.evaluator_receipt_fingerprint() || !seen.insert(fingerprint) {
            return Err(invalid_backtranslation(
                "evaluator receipts are missing, repeated, or not in case order",
            ));
        }
        let exact: i64 = connection.query_row(
            "SELECT COUNT(*) FROM research_evaluation_receipts
             WHERE receipt_fingerprint = ?1 AND outcome = 'validated'
               AND evaluator_class IN ('local_critic', 'frontier_critic')",
            [fingerprint.to_string()],
            |row| row.get(0),
        )?;
        if exact != 1 {
            return Err(invalid_backtranslation(
                "evaluator receipt is absent, abstained, rejected, or not an eligible critic",
            ));
        }
        ordered.push(fingerprint);
    }
    Ok(ordered)
}

fn ensure_exact_proposal(store: &ProjectStore, proposal: &BacktranslationProposal) -> Result<()> {
    let exact: i64 = store.connection.query_row(
        "SELECT COUNT(*) FROM research_backtranslation_proposals
         WHERE proposal_fingerprint = ?1 AND source_revision_id = ?2
           AND source_blob_id = ?3",
        params![
            proposal.fingerprint().to_string(),
            proposal.source().revision_id().to_string(),
            proposal.source().source_blob_id().to_string(),
        ],
        |row| row.get(0),
    )?;
    if exact != 1 {
        return Err(invalid_backtranslation(
            "audition proposal is absent or differs from persisted proposal evidence",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn ensure_exact_audition_rows(
    transaction: &rusqlite::Transaction<'_>,
    audition_fingerprint: BlobId,
    proposal_fingerprint: BlobId,
    writer: &WriterEvidenceBinding,
    evaluator_receipts: &[BlobId],
    evaluator_evidence_fingerprint: BlobId,
    record_fingerprint: BlobId,
) -> Result<()> {
    let exact: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM research_backtranslation_auditions
         WHERE audition_fingerprint = ?1 AND proposal_fingerprint = ?2
           AND writer_evidence_fingerprint = ?3 AND writer_batch_count = ?4
           AND evaluator_evidence_fingerprint = ?5 AND evaluator_receipt_count = ?6
           AND work_disjoint = 1 AND causal_transfer_decision = 'improved'
           AND leakage_decision = 'clear' AND record_fingerprint = ?7",
        params![
            audition_fingerprint.to_string(),
            proposal_fingerprint.to_string(),
            writer.fingerprint.to_string(),
            checked_i64_usize(writer.ordered_batches.len(), "writer batch count")?,
            evaluator_evidence_fingerprint.to_string(),
            checked_i64_usize(evaluator_receipts.len(), "evaluator receipt count")?,
            record_fingerprint.to_string(),
        ],
        |row| row.get(0),
    )?;
    if exact != 1
        || load_ordered_child_fingerprints(
            transaction,
            "research_backtranslation_audition_batches",
            "batch_ordinal",
            "batch_verification_fingerprint",
            audition_fingerprint,
        )? != writer.ordered_batches
        || load_ordered_child_fingerprints(
            transaction,
            "research_backtranslation_audition_evaluator_receipts",
            "receipt_ordinal",
            "evaluator_receipt_fingerprint",
            audition_fingerprint,
        )? != evaluator_receipts
    {
        return Err(invalid_backtranslation(
            "audition fingerprint conflicts with exact persisted evidence",
        ));
    }
    Ok(())
}

fn ensure_persisted_audition_for_acceptance(
    connection: &rusqlite::Connection,
    audition_fingerprint: BlobId,
    writer: &WriterEvidenceBinding,
    evaluator_receipts: &[BlobId],
    evaluator_evidence_fingerprint: BlobId,
) -> Result<()> {
    let exact: i64 = connection.query_row(
        "SELECT COUNT(*) FROM research_backtranslation_auditions
         WHERE audition_fingerprint = ?1
           AND writer_evidence_fingerprint = ?2 AND writer_batch_count = ?3
           AND evaluator_evidence_fingerprint = ?4 AND evaluator_receipt_count = ?5
           AND work_disjoint = 1 AND causal_transfer_decision = 'improved'
           AND leakage_decision = 'clear'",
        params![
            audition_fingerprint.to_string(),
            writer.fingerprint.to_string(),
            checked_i64_usize(writer.ordered_batches.len(), "writer batch count")?,
            evaluator_evidence_fingerprint.to_string(),
            checked_i64_usize(evaluator_receipts.len(), "evaluator receipt count")?,
        ],
        |row| row.get(0),
    )?;
    if exact != 1
        || load_ordered_connection_fingerprints(
            connection,
            "research_backtranslation_audition_batches",
            "batch_ordinal",
            "batch_verification_fingerprint",
            audition_fingerprint,
        )? != writer.ordered_batches
        || load_ordered_connection_fingerprints(
            connection,
            "research_backtranslation_audition_evaluator_receipts",
            "receipt_ordinal",
            "evaluator_receipt_fingerprint",
            audition_fingerprint,
        )? != evaluator_receipts
    {
        return Err(invalid_backtranslation(
            "accepted audition differs from its exact diagnostic evidence",
        ));
    }
    Ok(())
}

fn load_ordered_child_fingerprints(
    transaction: &rusqlite::Transaction<'_>,
    table: &str,
    ordinal_column: &str,
    fingerprint_column: &str,
    audition_fingerprint: BlobId,
) -> Result<Vec<BlobId>> {
    let sql = format!(
        "SELECT {fingerprint_column} FROM {table}
         WHERE audition_fingerprint = ?1 ORDER BY {ordinal_column}"
    );
    let mut statement = transaction.prepare(&sql)?;
    let rows = statement.query_map([audition_fingerprint.to_string()], |row| {
        row.get::<_, String>(0)
    })?;
    rows.map(|row| {
        let encoded = row?;
        parse_fingerprint(&encoded, "backtranslation child evidence")
    })
    .collect()
}

fn load_ordered_connection_fingerprints(
    connection: &rusqlite::Connection,
    table: &str,
    ordinal_column: &str,
    fingerprint_column: &str,
    audition_fingerprint: BlobId,
) -> Result<Vec<BlobId>> {
    let sql = format!(
        "SELECT {fingerprint_column} FROM {table}
         WHERE audition_fingerprint = ?1 ORDER BY {ordinal_column}"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([audition_fingerprint.to_string()], |row| {
        row.get::<_, String>(0)
    })?;
    rows.map(|row| {
        let encoded = row?;
        parse_fingerprint(&encoded, "backtranslation acceptance evidence")
    })
    .collect()
}

fn ordered_evidence_fingerprint(domain: &[u8], ordered: &[BlobId]) -> BlobId {
    let mut bytes = Vec::with_capacity(domain.len() + 8 + ordered.len() * 32);
    bytes.extend_from_slice(domain);
    bytes.extend_from_slice(&(ordered.len() as u64).to_be_bytes());
    for fingerprint in ordered {
        bytes.extend_from_slice(fingerprint.as_bytes());
    }
    BlobId::digest(&bytes)
}

fn acceptance_fingerprint(
    audition_fingerprint: BlobId,
    writer_evidence_fingerprint: BlobId,
    evaluator_evidence_fingerprint: BlobId,
) -> BlobId {
    let mut bytes = Vec::with_capacity(128);
    bytes.extend_from_slice(b"loom/backtranslation-acceptance/v1\0");
    bytes.extend_from_slice(audition_fingerprint.as_bytes());
    bytes.extend_from_slice(writer_evidence_fingerprint.as_bytes());
    bytes.extend_from_slice(evaluator_evidence_fingerprint.as_bytes());
    BlobId::digest(&bytes)
}

fn parse_fingerprint(encoded: &str, field: &str) -> Result<BlobId> {
    encoded.parse().map_err(|error| {
        StoreError::CorruptDatabase(format!("invalid {field} fingerprint: {error}"))
    })
}

struct PromptMaskRow<'a> {
    mask_fingerprint: BlobId,
    campaign_id: CampaignId,
    stage_attempt_id: StageAttemptId,
    kind: DiagnosticPromptMaskKind,
    source_blob_id: BlobId,
    rendered_blob_id: Option<BlobId>,
    backend_capability_fingerprint: Option<BlobId>,
    canonical: &'a [u8],
}

fn persist_prompt_mask(
    store: &mut ProjectStore,
    row: &PromptMaskRow<'_>,
) -> Result<PersistedDiagnosticPromptMask> {
    let record = store.persist_research_execution_record(
        ResearchExecutionRecordKind::PromptMask,
        row.canonical,
    )?;
    let created_at_ms = now_unix_ms();
    let rendered = row
        .rendered_blob_id
        .map(|fingerprint| fingerprint.to_string());
    let capability = row
        .backend_capability_fingerprint
        .map(|fingerprint| fingerprint.to_string());
    let transaction = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute(
        "INSERT OR IGNORE INTO research_prompt_masks(
            mask_fingerprint, campaign_id, stage_attempt_id, mask_kind,
            source_blob_id, rendered_blob_id, backend_capability_fingerprint,
            record_fingerprint, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            row.mask_fingerprint.to_string(),
            row.campaign_id.to_string(),
            row.stage_attempt_id.to_string(),
            row.kind.as_str(),
            row.source_blob_id.to_string(),
            rendered,
            capability,
            record.fingerprint().to_string(),
            created_at_ms,
        ],
    )?;
    let exact: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM research_prompt_masks
         WHERE mask_fingerprint = ?1 AND campaign_id = ?2
           AND stage_attempt_id = ?3 AND mask_kind = ?4
           AND source_blob_id = ?5 AND rendered_blob_id IS ?6
           AND backend_capability_fingerprint IS ?7
           AND record_fingerprint = ?8",
        params![
            row.mask_fingerprint.to_string(),
            row.campaign_id.to_string(),
            row.stage_attempt_id.to_string(),
            row.kind.as_str(),
            row.source_blob_id.to_string(),
            rendered,
            capability,
            record.fingerprint().to_string(),
        ],
        |sqlite_row| sqlite_row.get(0),
    )?;
    if exact != 1 {
        return Err(invalid_prompt_evidence(
            "prompt-mask fingerprint conflicts with persisted evidence",
        ));
    }
    transaction.commit()?;
    Ok(PersistedDiagnosticPromptMask {
        mask_fingerprint: row.mask_fingerprint,
        record_fingerprint: record.fingerprint(),
        kind: row.kind,
        source_blob_id: row.source_blob_id,
        rendered_blob_id: row.rendered_blob_id,
        backend_capability_fingerprint: row.backend_capability_fingerprint,
    })
}

fn ensure_stage_attempt_campaign(
    connection: &rusqlite::Connection,
    campaign_id: CampaignId,
    stage_attempt_id: StageAttemptId,
) -> Result<()> {
    let exact: i64 = connection.query_row(
        "SELECT COUNT(*)
         FROM research_campaign_stage_attempts attempt
         JOIN research_campaign_stage_specs stage USING (stage_id)
         JOIN research_trial_specs trial USING (trial_fingerprint)
         WHERE attempt.stage_attempt_id = ?1 AND trial.campaign_id = ?2",
        params![stage_attempt_id.to_string(), campaign_id.to_string()],
        |row| row.get(0),
    )?;
    if exact != 1 {
        return Err(invalid_prompt_evidence(
            "stage attempt does not belong to the declared campaign",
        ));
    }
    Ok(())
}

fn exact_project_source(store: &ProjectStore, source: PromptSourceRange) -> Result<Vec<u8>> {
    let stored_blob = store
        .connection
        .query_row(
            "SELECT artifact.blob_id
             FROM revisions revision
             JOIN artifacts artifact ON artifact.artifact_id = revision.artifact_id
             WHERE revision.revision_id = ?1",
            [source.revision_id().to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| invalid_prompt_evidence("source revision is absent from this project"))?;
    let stored_blob: BlobId = stored_blob.parse().map_err(|error| {
        StoreError::CorruptDatabase(format!("invalid prompt source blob fingerprint: {error}"))
    })?;
    if stored_blob != source.source_blob_id() {
        return Err(invalid_prompt_evidence(
            "source revision does not resolve to the declared blob",
        ));
    }
    let exact = store.read_blob(stored_blob)?;
    let _ = source
        .range()
        .checked_str(&exact)
        .map_err(invalid_prompt_evidence)?;
    Ok(exact)
}

fn register_diagnostic_blob(store: &mut ProjectStore, bytes: &[u8]) -> Result<BlobId> {
    let blob_id = store.put_blob(bytes)?;
    let transaction = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    insert_blob_row(&transaction, blob_id, bytes.len(), now_unix_ms())?;
    transaction.commit()?;
    Ok(blob_id)
}

fn grounded_backtranslation_field_count(proposal: &BacktranslationProposal) -> Result<u32> {
    let sections = proposal.sections();
    let count = [
        sections.causal_events.fields().len(),
        sections.knowledge_changes.fields().len(),
        sections.objects.fields().len(),
        sections.physical_positions.fields().len(),
        sections.dialogue_tactics.fields().len(),
        sections.resulting_state.fields().len(),
    ]
    .into_iter()
    .try_fold(0_usize, usize::checked_add)
    .ok_or_else(|| invalid_backtranslation("grounded field count overflow"))?;
    u32::try_from(count).map_err(|_| invalid_backtranslation("grounded field count exceeds u32"))
}

fn checked_i64(value: u64, field: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        StoreError::InvalidResearchDiagnostic(format!(
            "{field} exceeds SQLite's signed integer domain"
        ))
    })
}

fn checked_i64_usize(value: usize, field: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        StoreError::InvalidResearchDiagnostic(format!(
            "{field} exceeds SQLite's signed integer domain"
        ))
    })
}

fn checked_u32(value: usize, field: &str) -> Result<u32> {
    u32::try_from(value).map_err(|_| {
        StoreError::InvalidResearchDiagnostic(format!("{field} exceeds u32's integer domain"))
    })
}

fn invalid_prompt_evidence(error: impl std::fmt::Display) -> StoreError {
    StoreError::InvalidResearchDiagnostic(format!("invalid prompt-mask evidence: {error}"))
}

fn invalid_backtranslation(error: impl std::fmt::Display) -> StoreError {
    StoreError::InvalidResearchDiagnostic(format!("invalid backtranslation evidence: {error}"))
}

#[cfg(test)]
mod tests {
    use loom_document::DocumentContent;
    use loom_research_types::{
        BacktranslationAbstentionReason, BacktranslationAuditionCase, BacktranslationProposal,
        BacktranslationSection, BacktranslationSections, CausalTransferDecision,
        FimCapabilityReceipt, LeakageDecision, ModelSpecificFimMaskPlan, NonEmptyByteRange,
        PromptSourceRange, SurfaceMaskReplacement, SurfaceMaskSpan, SurfacePromptMaskPlan,
        TrialCaseId,
    };
    use tempfile::tempdir;

    use super::*;

    #[allow(clippy::too_many_lines)]
    fn seed_research_scope(store: &mut ProjectStore) -> (CampaignId, StageAttemptId) {
        let campaign_id = CampaignId::new();
        let trial_fingerprint = BlobId::digest(b"prompt evidence trial");
        let stage_id = loom_research_types::StageId::new();
        let stage_attempt_id = StageAttemptId::new();
        let trial_run_id = loom_research_types::TrialRunId::new();
        let manifest = b"prompt evidence campaign";
        let manifest_blob_id = register_diagnostic_blob(store, manifest).expect("manifest blob");
        let campaign_record = store
            .persist_research_execution_record(
                ResearchExecutionRecordKind::Campaign,
                b"prompt evidence campaign record",
            )
            .expect("campaign record");
        let trial_record = store
            .persist_research_execution_record(
                ResearchExecutionRecordKind::TrialSpec,
                b"prompt evidence trial record",
            )
            .expect("trial record");
        let stage_record = store
            .persist_research_execution_record(
                ResearchExecutionRecordKind::StageSpec,
                b"prompt evidence stage record",
            )
            .expect("stage record");
        let attempt_record = store
            .persist_research_execution_record(
                ResearchExecutionRecordKind::StageAttempt,
                b"prompt evidence attempt record",
            )
            .expect("attempt record");
        let run_record = store
            .persist_research_execution_record(
                ResearchExecutionRecordKind::TrialRun,
                b"prompt evidence trial run record",
            )
            .expect("trial run record");
        let transaction = store.connection.transaction().expect("scope transaction");
        transaction
            .execute(
                "INSERT INTO research_campaigns(
                    campaign_id, campaign_fingerprint, project_id, manifest_source_blob_id,
                    manifest_fingerprint, project_input_fingerprint, seed_decimal,
                    maximum_writer_tokens, maximum_controller_tokens,
                    maximum_evaluations, maximum_wall_time_ms,
                    record_fingerprint, created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, '1', 100, 100, 2, 1000, ?7, 1)",
                params![
                    campaign_id.to_string(),
                    BlobId::digest(b"campaign fingerprint").to_string(),
                    store.manifest.project_id.to_string(),
                    manifest_blob_id.to_string(),
                    BlobId::digest(manifest).to_string(),
                    BlobId::digest(b"project input").to_string(),
                    campaign_record.fingerprint().to_string(),
                ],
            )
            .expect("campaign row");
        transaction
            .execute(
                "INSERT INTO research_trial_specs(
                    trial_fingerprint, campaign_id, trial_case_id,
                    treatment_fingerprint, prompt_content_fingerprint,
                    model_binding_fingerprint, expected_writer_call_count,
                    declared_writer_token_maximum,
                    maximum_writer_tokens, maximum_controller_tokens,
                    maximum_evaluations, maximum_wall_time_ms,
                    record_fingerprint, created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 100, 100, 100, 2, 1000, ?7, 1)",
                params![
                    trial_fingerprint.to_string(),
                    campaign_id.to_string(),
                    TrialCaseId::new().to_string(),
                    BlobId::digest(b"treatment").to_string(),
                    BlobId::digest(b"prompt").to_string(),
                    BlobId::digest(b"binding").to_string(),
                    trial_record.fingerprint().to_string(),
                ],
            )
            .expect("trial row");
        transaction
            .execute(
                "INSERT INTO research_trial_runs(
                    trial_run_id, trial_fingerprint, origin_kind,
                    record_fingerprint, created_at_ms
                 ) VALUES (?1, ?2, 'standalone', ?3, 1)",
                params![
                    trial_run_id.to_string(),
                    trial_fingerprint.to_string(),
                    run_record.fingerprint().to_string(),
                ],
            )
            .expect("trial run row");
        transaction
            .execute(
                "INSERT INTO research_campaign_stage_specs(
                    stage_id, trial_fingerprint, stage_ordinal, stage_kind,
                    stage_spec_fingerprint, maximum_writer_tokens,
                    maximum_controller_tokens, maximum_evaluations,
                    maximum_wall_time_ms, record_fingerprint, created_at_ms
                 ) VALUES (?1, ?2, 0, 'backtranslate_mask', ?3, 0, 100, 0, 1000, ?4, 1)",
                params![
                    stage_id.to_string(),
                    trial_fingerprint.to_string(),
                    BlobId::digest(b"stage fingerprint").to_string(),
                    stage_record.fingerprint().to_string(),
                ],
            )
            .expect("stage row");
        transaction
            .execute(
                "INSERT INTO research_campaign_stage_attempts(
                    stage_attempt_id, trial_run_id, stage_id, attempt_ordinal,
                    record_fingerprint, created_at_ms
                 ) VALUES (?1, ?2, ?3, 1, ?4, 1)",
                params![
                    stage_attempt_id.to_string(),
                    trial_run_id.to_string(),
                    stage_id.to_string(),
                    attempt_record.fingerprint().to_string(),
                ],
            )
            .expect("stage attempt row");
        transaction.commit().expect("commit research scope");
        (campaign_id, stage_attempt_id)
    }

    fn exact_source(store: &mut ProjectStore) -> (RevisionId, BlobId, Vec<u8>) {
        let bytes = b"Mara lifted the key and waited.".to_vec();
        store
            .save_document(
                "manuscript/source.md",
                DocumentContent::Prose(String::from_utf8(bytes.clone()).expect("UTF-8")),
                "prompt evidence source",
            )
            .expect("save source");
        let source = store
            .read_document("manuscript/source.md")
            .expect("read source");
        (source.revision_id, source.blob_id, bytes)
    }

    fn source_range(revision_id: RevisionId, blob_id: BlobId, len: usize) -> PromptSourceRange {
        PromptSourceRange::new(
            revision_id,
            blob_id,
            NonEmptyByteRange::new(0, len as u64).expect("source range"),
        )
        .expect("bounded source")
    }

    #[test]
    fn surface_and_fim_masks_persist_exactly_without_conflating_rendered_bytes() {
        let directory = tempdir().expect("temporary project");
        let (mut store, _) = ProjectStore::initialize(directory.path(), "masks").expect("store");
        let (campaign_id, stage_attempt_id) = seed_research_scope(&mut store);
        let (revision_id, source_blob_id, bytes) = exact_source(&mut store);
        let source = source_range(revision_id, source_blob_id, bytes.len());
        let suffix_range = NonEmptyByteRange::new(19, bytes.len() as u64).expect("suffix range");
        let applied = SurfacePromptMaskPlan::new(
            source,
            SurfaceMaskKind::Suffix,
            vec![SurfaceMaskSpan::new(
                suffix_range,
                SurfaceMaskReplacement::Omit,
            )],
        )
        .expect("suffix plan")
        .apply(revision_id, &bytes)
        .expect("apply source mask");
        let first = store
            .persist_diagnostic_surface_prompt_mask(campaign_id, stage_attempt_id, &applied)
            .expect("persist surface mask");
        let second = store
            .persist_diagnostic_surface_prompt_mask(campaign_id, stage_attempt_id, &applied)
            .expect("repeat surface mask");
        assert_eq!(first, second);
        assert_eq!(first.rendered_blob_id(), Some(applied.rendered_blob_id()));
        assert_eq!(first.backend_capability_fingerprint(), None);

        let model = BlobId::digest(b"fim model");
        let tokenizer = BlobId::digest(b"fim tokenizer");
        let capability = BlobId::digest(b"fim capability");
        let backend_receipt = b"verified backend FIM receipt";
        let plan = ModelSpecificFimMaskPlan::new(
            source,
            NonEmptyByteRange::new(5, 11).expect("missing range"),
            model,
            tokenizer,
            capability,
        )
        .expect("FIM plan");
        let receipt = FimCapabilityReceipt::new(
            model,
            tokenizer,
            capability,
            vec![1],
            vec![2],
            vec![3],
            BlobId::digest(backend_receipt),
        )
        .expect("FIM receipt");
        let binding = plan.bind_capability(receipt).expect("bind FIM capability");
        assert!(
            store
                .persist_diagnostic_fim_prompt_mask(
                    campaign_id,
                    stage_attempt_id,
                    &binding,
                    b"substituted receipt",
                )
                .is_err(),
            "the exact backend receipt bytes are mandatory"
        );
        let persisted = store
            .persist_diagnostic_fim_prompt_mask(
                campaign_id,
                stage_attempt_id,
                &binding,
                backend_receipt,
            )
            .expect("persist FIM diagnostic");
        assert_eq!(persisted.rendered_blob_id(), None);
        assert_eq!(persisted.backend_capability_fingerprint(), Some(capability));
    }

    #[test]
    fn fully_abstained_backtranslation_is_preserved_and_wrong_source_is_rejected() {
        let directory = tempdir().expect("temporary project");
        let (mut store, _) =
            ProjectStore::initialize(directory.path(), "backtranslation").expect("store");
        let (revision_id, source_blob_id, bytes) = exact_source(&mut store);
        let source = source_range(revision_id, source_blob_id, bytes.len());
        let abstained = || {
            BacktranslationSection::abstained(BacktranslationAbstentionReason::NoObservableEvidence)
        };
        let proposal = BacktranslationProposal::new(
            source,
            BlobId::digest(b"source work"),
            BlobId::digest(b"controller model"),
            BlobId::digest(b"controller prompt"),
            BlobId::digest(b"controller call"),
            BlobId::digest(b"ontology"),
            vec![],
            vec![],
            BacktranslationSections {
                causal_events: abstained(),
                knowledge_changes: abstained(),
                objects: abstained(),
                physical_positions: abstained(),
                dialogue_tactics: abstained(),
                resulting_state: abstained(),
            },
        )
        .expect("fully abstained proposal");
        let first = store
            .persist_diagnostic_backtranslation_proposal(&proposal)
            .expect("persist abstention");
        let second = store
            .persist_diagnostic_backtranslation_proposal(&proposal)
            .expect("repeat abstention");
        assert_eq!(first, second);
        assert_eq!(first.grounded_field_count(), 0);

        let wrong_source = source_range(revision_id, BlobId::digest(b"wrong source"), bytes.len());
        let wrong = BacktranslationProposal::new(
            wrong_source,
            BlobId::digest(b"source work two"),
            BlobId::digest(b"controller model"),
            BlobId::digest(b"controller prompt"),
            BlobId::digest(b"controller call two"),
            BlobId::digest(b"ontology"),
            vec![],
            vec![],
            BacktranslationSections {
                causal_events: abstained(),
                knowledge_changes: abstained(),
                objects: abstained(),
                physical_positions: abstained(),
                dialogue_tactics: abstained(),
                resulting_state: abstained(),
            },
        )
        .expect("structural wrong-source proposal");
        assert!(
            store
                .persist_diagnostic_backtranslation_proposal(&wrong)
                .is_err()
        );
    }

    fn audition_case(label: &str) -> BacktranslationAuditionCase {
        let bytes = format!("fresh source {label}");
        BacktranslationAuditionCase::new(
            TrialCaseId::new(),
            BlobId::digest(format!("work {label}").as_bytes()),
            source_range(
                RevisionId::new(),
                BlobId::digest(bytes.as_bytes()),
                bytes.len(),
            ),
            BlobId::digest(b"writer model"),
            BlobId::digest(b"writer tokenizer"),
            BlobId::digest(format!("prompt {label}").as_bytes()),
            BlobId::digest(format!("treated call {label}").as_bytes()),
            BlobId::digest(format!("raw {label}").as_bytes()),
            BlobId::digest(format!("selected {label}").as_bytes()),
            BlobId::digest(format!("baseline call {label}").as_bytes()),
            BlobId::digest(format!("baseline output {label}").as_bytes()),
            BlobId::digest(format!("evaluator {label}").as_bytes()),
            CausalTransferDecision::Improved,
            LeakageDecision::Clear,
        )
    }

    fn evidence(
        case: &BacktranslationAuditionCase,
        batch_fingerprint: BlobId,
        baseline: bool,
    ) -> (BlobId, WriterCallEvidence) {
        let (call, prompt, raw, displayed) = if baseline {
            (
                case.baseline_call_fingerprint(),
                BlobId::digest(b"baseline prompt"),
                BlobId::digest(b"baseline raw"),
                case.baseline_output_blob_id(),
            )
        } else {
            (
                case.call_fingerprint(),
                case.prompt_fingerprint(),
                case.raw_output_blob_id(),
                case.selected_output_blob_id(),
            )
        };
        (
            call,
            WriterCallEvidence {
                batch_fingerprint,
                model_fingerprint: case.writer_model_fingerprint(),
                tokenizer_fingerprint: case.writer_tokenizer_fingerprint(),
                prompt_fingerprint: prompt,
                raw_output_blob_id: raw,
                displayed_output_blob_id: Some(displayed),
            },
        )
    }

    #[test]
    fn audition_writer_evidence_rejects_missing_extra_reordered_and_replayed_batches() {
        let cases = vec![audition_case("one"), audition_case("two")];
        let batch_ids = [
            BlobId::digest(b"batch one treated"),
            BlobId::digest(b"batch one baseline"),
            BlobId::digest(b"batch two treated"),
            BlobId::digest(b"batch two baseline"),
        ];
        let mut calls = BTreeMap::new();
        for (index, case) in cases.iter().enumerate() {
            let treated = evidence(case, batch_ids[index * 2], false);
            let baseline = evidence(case, batch_ids[index * 2 + 1], true);
            calls.insert(treated.0, treated.1);
            calls.insert(baseline.0, baseline.1);
        }
        assert_eq!(
            validate_writer_receipt_bindings(&cases, &batch_ids, &calls)
                .expect("exact first-use order"),
            batch_ids
        );
        let mut missing = calls.clone();
        missing.remove(&cases[0].call_fingerprint());
        assert!(validate_writer_receipt_bindings(&cases, &batch_ids, &missing).is_err());
        let reordered = [batch_ids[1], batch_ids[0], batch_ids[2], batch_ids[3]];
        assert!(validate_writer_receipt_bindings(&cases, &reordered, &calls).is_err());
        let mut extra = batch_ids.to_vec();
        extra.push(BlobId::digest(b"unrelated batch"));
        assert!(validate_writer_receipt_bindings(&cases, &extra, &calls).is_err());

        let first_directory = tempdir().expect("first project");
        let second_directory = tempdir().expect("second project");
        let (first, _) = ProjectStore::initialize(first_directory.path(), "first").expect("first");
        let (second, _) =
            ProjectStore::initialize(second_directory.path(), "second").expect("second");
        let replayed = AdoptedInferenceBatch::diagnostic_for_test(
            second.session_nonce,
            BlobId::digest(b"replayed live batch"),
        );
        assert!(!replayed.belongs_to_session(first.session_nonce));

        let ordered_receipts = cases
            .iter()
            .map(BacktranslationAuditionCase::evaluator_receipt_fingerprint)
            .collect::<Vec<_>>();
        let evaluator = VerifiedBacktranslationEvaluatorLease::for_test(
            first.session_nonce,
            BlobId::digest(b"audition"),
            ordered_receipts.clone(),
        );
        assert_eq!(evaluator.ordered_receipts, ordered_receipts);
    }
}
