use std::collections::BTreeMap;

use loom_research_types::{
    CallEvidenceClass, CandidateAssemblyRecord, CandidateProjectionRecord, ExactCallEvidence,
    GeneratedSpanOccurrenceRecord, JoinBefore, MixedAuthorshipAssemblyRecord, ModelCall,
    OperationGraph, PipelineEligibility, PipelineOperationKind, PromotionAuthority,
    PromotionCommandRequest, PromotionSubject, UserPresenceKind,
};
use loom_types::{BlobId, CommandId, now_unix_ms};
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
const MAX_RECEIPT_BYTES: usize = 8 * 1024 * 1024;
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
#[derive(Debug)]
pub struct AdmittedModelCall {
    session_nonce: StoreSessionNonce,
    call: ModelCall,
    raw_output: Vec<u8>,
    token_ids: Vec<u32>,
    token_byte_boundaries: Option<Vec<u64>>,
    verification_fingerprint: BlobId,
}

impl AdmittedModelCall {
    pub const fn call_id(&self) -> loom_research_types::ModelCallId {
        self.call.id()
    }
}

/// Non-serializable proof that a declared span was checked against an admitted
/// call and, when present, exact token-to-byte boundaries.
#[derive(Debug)]
pub struct AdmittedGeneratedSpan {
    session_nonce: StoreSessionNonce,
    record: GeneratedSpanOccurrenceRecord,
    call: ModelCall,
    raw_output: Vec<u8>,
    token_ids: Vec<u32>,
}

impl AdmittedGeneratedSpan {
    pub const fn occurrence_id(&self) -> loom_research_types::GeneratedSpanOccurrenceId {
        self.record.id()
    }
}

/// Non-serializable proof that all assembly parts were independently admitted
/// and the exact bytes, graph, and witness were replayed by the store.
#[derive(Debug)]
pub struct AdmittedCandidateAssembly {
    session_nonce: StoreSessionNonce,
    admission_record_id: ResearchAdmissionRecordId,
    record: CandidateAssemblyRecord,
    exact_calls: Vec<OwnedExactCall>,
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
#[derive(Debug)]
pub struct AdmittedCandidateProjection {
    session_nonce: StoreSessionNonce,
    admission_record_id: ResearchAdmissionRecordId,
    record: CandidateProjectionRecord,
}

impl AdmittedCandidateProjection {
    pub const fn admission_record_id(&self) -> ResearchAdmissionRecordId {
        self.admission_record_id
    }

    pub const fn projection_id(&self) -> loom_research_types::CandidateProjectionId {
        self.record.id()
    }
}

/// Explicitly ineligible but inspectable mixed-authorship persistence result.
#[derive(Debug)]
pub struct MixedAuthorshipAdmission {
    session_nonce: StoreSessionNonce,
    admission_record_id: ResearchAdmissionRecordId,
    record: MixedAuthorshipAssemblyRecord,
}

/// Host-owned, non-serializable proof of one foreground user gesture.
///
/// There is intentionally no constructor in `loom-store`. A later host bridge
/// will move this lease behind a native event seal; deserialized
/// `UserPresenceEvidence` cannot manufacture it.
#[derive(Debug)]
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

/// Opaque proof that the exact promotion command request was durably recorded
/// in this process before user-presence authority.
#[derive(Debug)]
pub struct RecordedPromotionRequest {
    session_nonce: StoreSessionNonce,
    command_id: CommandId,
    request_fingerprint: BlobId,
    recorded_at_ms: i64,
}

/// Exact runtime admission lease selected for promotion. Persisted admission
/// rows cannot construct either variant.
#[derive(Debug)]
pub enum PromotionSubjectLease<'a> {
    CandidateProjection(&'a AdmittedCandidateProjection),
    MixedAuthorship(&'a MixedAuthorshipAdmission),
}

/// Opaque pre-mutation authority. It is intentionally neither `Clone` nor
/// serializable and has no constructor or SQL reload path.
#[derive(Debug)]
pub struct RecordedPromotionAuthority {
    session_nonce: StoreSessionNonce,
    command_id: CommandId,
    record_blob_id: BlobId,
    source_revision_id: loom_types::RevisionId,
    source_blob_id: BlobId,
    intended_result_blob_id: BlobId,
    intended_result_byte_len: u64,
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

#[derive(Debug)]
struct OwnedExactCall {
    call: ModelCall,
    raw_output: Vec<u8>,
    token_ids: Vec<u32>,
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

impl ProjectStore {
    fn require_research_session(&self, nonce: StoreSessionNonce) -> Result<()> {
        if nonce != self.session_nonce {
            return Err(admission_error(
                "opaque research capability belongs to another project-store session",
            ));
        }
        Ok(())
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
    #[allow(clippy::too_many_lines)]
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

        let record_bytes = serde_json::to_vec(&record)?;
        let record_blob_id = self.put_blob(&record_bytes)?;
        let assembled_blob_id = self.put_blob(&assembled_bytes)?;
        let graph_bytes = serde_json::to_vec(record.operation_graph())?;
        let graph_blob_id = self.put_blob(&graph_bytes)?;
        let created_at_ms = now_unix_ms().max(1);
        let transaction = self
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
    if backend_receipt.len() > MAX_RECEIPT_BYTES || raw_event_stream.len() > MAX_RECEIPT_BYTES {
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

#[cfg(test)]
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
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    use loom_document::DocumentContent;
    use loom_research_types::{
        AssemblyPartRecord, CallIdentity, CallScope, CallTerminal, CampaignId, CandidateAssemblyId,
        CandidateProjectionId, CompletedCall, GeneratedSpanOccurrenceId, MixedAuthorshipAssemblyId,
        ModelCallId, OutputProjection, PipelineOperation, PipelineOperationId, PromotionActor,
        StageAttemptId, StageId, TokenEvidence, TrialCaseId, UserPresenceEvidence,
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
