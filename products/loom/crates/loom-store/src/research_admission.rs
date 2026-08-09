use std::collections::BTreeMap;

use loom_research_types::{
    CallEvidenceClass, CandidateAssemblyRecord, CandidateProjectionRecord, ExactCallEvidence,
    GeneratedSpanOccurrenceRecord, JoinBefore, MixedAuthorshipAssemblyRecord, ModelCall,
    OperationGraph, PipelineEligibility, PipelineOperationKind, PromotionAuthority,
    UserPresenceKind,
};
use loom_types::{BlobId, CommandId, CommandKind, now_unix_ms};
use rusqlite::{Transaction, TransactionBehavior, params};
#[cfg(test)]
use serde::Deserialize;

use crate::provenance::insert_blob_row;
use crate::{ProjectStore, Result, StoreError};

#[cfg(test)]
const STRICT_RECEIPT_FORMAT: &str = "loom.native-base-writer-receipt.v1";
#[cfg(test)]
const STRICT_EVENT_STREAM_FORMAT: &str = "loom.native-call-events.v1";
#[cfg(test)]
const MAX_RECEIPT_BYTES: usize = 8 * 1024 * 1024;
#[cfg(test)]
const MAX_EVENT_COUNT: usize = 1_048_578;

/// Content-addressed identity of one completed store admission occurrence.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ResearchAdmissionId(BlobId);

impl ResearchAdmissionId {
    pub const fn fingerprint(self) -> BlobId {
        self.0
    }
}

/// Non-serializable proof that the store replayed a complete native call.
///
/// The private exact evidence is intentional. A database row, a receipt hash,
/// or a caller-supplied evidence enum cannot recreate this lease.
#[derive(Debug)]
pub struct AdmittedModelCall {
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
    admission_id: ResearchAdmissionId,
    record: CandidateAssemblyRecord,
    exact_calls: Vec<OwnedExactCall>,
}

impl AdmittedCandidateAssembly {
    pub const fn admission_id(&self) -> ResearchAdmissionId {
        self.admission_id
    }

    pub const fn assembly_id(&self) -> loom_research_types::CandidateAssemblyId {
        self.record.id()
    }
}

/// Non-serializable proof for a projection pinned to exact source bytes.
#[derive(Debug)]
pub struct AdmittedCandidateProjection {
    admission_id: ResearchAdmissionId,
    record: CandidateProjectionRecord,
}

impl AdmittedCandidateProjection {
    pub const fn admission_id(&self) -> ResearchAdmissionId {
        self.admission_id
    }

    pub const fn projection_id(&self) -> loom_research_types::CandidateProjectionId {
        self.record.id()
    }
}

/// Explicitly ineligible but inspectable mixed-authorship persistence result.
#[derive(Debug)]
pub struct MixedAuthorshipAdmission {
    admission_id: ResearchAdmissionId,
    record: MixedAuthorshipAssemblyRecord,
}

/// Host-owned, non-serializable proof of one foreground user gesture.
///
/// There is intentionally no constructor in `loom-store`. A later host bridge
/// will move this lease behind a native event seal; deserialized
/// `UserPresenceEvidence` cannot manufacture it.
#[derive(Debug)]
pub struct VerifiedUserPresence {
    command_id: CommandId,
    kind: UserPresenceKind,
    session_fingerprint: BlobId,
    event_receipt_blob_id: BlobId,
    event_receipt_bytes: Vec<u8>,
    monotonic_event_index: u64,
    occurred_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordedPromotionAuthority {
    command_id: CommandId,
    record_blob_id: BlobId,
}

impl RecordedPromotionAuthority {
    pub const fn command_id(self) -> CommandId {
        self.command_id
    }

    pub const fn record_blob_id(self) -> BlobId {
        self.record_blob_id
    }
}

impl MixedAuthorshipAdmission {
    pub const fn admission_id(&self) -> ResearchAdmissionId {
        self.admission_id
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
                verification_replay_fingerprint, call_record_blob_id, created_at_ms
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
                verification_replay_fingerprint, span_record_blob_id, created_at_ms
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
        let admission_id = admission_id("candidate_assembly", &record.id().to_string());

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
            "INSERT INTO research_admissions(
                admission_id, subject_kind, subject_id, admitted_at_ms
             ) VALUES (?1, 'candidate_assembly', ?2, ?3)",
            params![
                admission_id.fingerprint().to_string(),
                record.id().to_string(),
                created_at_ms,
            ],
        )?;
        transaction.commit()?;

        Ok(AdmittedCandidateAssembly {
            admission_id,
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
        if record.assembly_id() != admitted_assembly.record.id() {
            return Err(admission_error("projection names a different assembly"));
        }
        let source_bytes = self.read_blob(record.source_blob_id())?;
        let exact = exact_evidence(&admitted_assembly.exact_calls);
        let resulting = record.apply(&admitted_assembly.record, &source_bytes, &exact)?;
        let admission_id = admission_id("candidate_projection", &record.id().to_string());

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
            "INSERT INTO research_admissions(
                admission_id, subject_kind, subject_id, admitted_at_ms
             ) VALUES (?1, 'candidate_projection', ?2, ?3)",
            params![
                admission_id.fingerprint().to_string(),
                record.id().to_string(),
                created_at_ms,
            ],
        )?;
        transaction.commit()?;
        Ok(AdmittedCandidateProjection {
            admission_id,
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
        let admission_id = admission_id("mixed_authorship", &record.id().to_string());
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
            "INSERT INTO research_admissions(
                admission_id, subject_kind, subject_id, admitted_at_ms
             ) VALUES (?1, 'mixed_authorship', ?2, ?3)",
            params![
                admission_id.fingerprint().to_string(),
                record.id().to_string(),
                created_at_ms,
            ],
        )?;
        transaction.commit()?;
        Ok(MixedAuthorshipAdmission {
            admission_id,
            record,
        })
    }

    /// Persists authority only when a non-serializable host lease agrees with
    /// the exact command, source, presence receipt, and command lifetime.
    pub fn record_promotion_authority(
        &mut self,
        lease: &VerifiedUserPresence,
        authority: &PromotionAuthority,
    ) -> Result<RecordedPromotionAuthority> {
        let presence = authority.user_presence();
        if authority.command_id() != lease.command_id
            || presence.kind() != lease.kind
            || presence.session_fingerprint() != lease.session_fingerprint
            || presence.event_receipt_blob_id() != lease.event_receipt_blob_id
            || presence.monotonic_event_index() != lease.monotonic_event_index
            || presence.occurred_at_ms() != lease.occurred_at_ms
            || BlobId::digest(&lease.event_receipt_bytes) != lease.event_receipt_blob_id
        {
            return Err(admission_error(
                "promotion authority differs from its host-owned presence lease",
            ));
        }
        let receipt = self
            .load_receipt(authority.command_id())?
            .ok_or_else(|| admission_error("promotion command has no durable receipt"))?;
        if receipt.command != CommandKind::PromoteCandidate
            || receipt.command_id != authority.command_id()
            || receipt.project_id != self.manifest.project_id
            || receipt.source_revision_id != Some(authority.source_revision_id())
            || lease.occurred_at_ms < receipt.started_at_ms
            || lease.occurred_at_ms > receipt.completed_at_ms
        {
            return Err(admission_error(
                "presence lease is not within the exact promotion command receipt",
            ));
        }
        let request: (String, i64) = self.connection.query_row(
            "SELECT command_kind, created_at_ms
             FROM command_requests WHERE command_id = ?1",
            [authority.command_id().to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if request.0 != CommandKind::PromoteCandidate.as_str()
            || request.1 > lease.occurred_at_ms
            || receipt.started_at_ms != request.1
        {
            return Err(admission_error(
                "promotion request, receipt, and presence lifetime disagree",
            ));
        }

        let authority_bytes = serde_json::to_vec(authority)?;
        let authority_record_blob_id = self.put_blob(&authority_bytes)?;
        let event_receipt_blob_id = self.put_blob(&lease.event_receipt_bytes)?;
        let created_at_ms = receipt.completed_at_ms.max(1);
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (blob_id, byte_len) in [
            (event_receipt_blob_id, lease.event_receipt_bytes.len()),
            (authority_record_blob_id, authority_bytes.len()),
        ] {
            insert_blob_row(&transaction, blob_id, byte_len, created_at_ms)?;
        }
        transaction.execute(
            "INSERT INTO research_user_presence_events(
                event_receipt_blob_id, command_id, user_presence_kind,
                session_fingerprint, monotonic_event_index,
                occurred_at_ms, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                event_receipt_blob_id.to_string(),
                authority.command_id().to_string(),
                user_presence_kind_name(lease.kind),
                lease.session_fingerprint.to_string(),
                checked_sql_u64(lease.monotonic_event_index, "presence event index")?,
                lease.occurred_at_ms,
                created_at_ms,
            ],
        )?;
        transaction.execute(
            "INSERT INTO research_promotion_authorities(
                command_id, actor, source_revision_id, source_blob_id,
                user_presence_kind, session_fingerprint, event_receipt_blob_id,
                monotonic_event_index, occurred_at_ms,
                authority_record_blob_id, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                authority.command_id().to_string(),
                authority.actor().as_str(),
                authority.source_revision_id().to_string(),
                authority.source_blob_id().to_string(),
                user_presence_kind_name(lease.kind),
                lease.session_fingerprint.to_string(),
                event_receipt_blob_id.to_string(),
                checked_sql_u64(lease.monotonic_event_index, "presence event index")?,
                lease.occurred_at_ms,
                authority_record_blob_id.to_string(),
                created_at_ms,
            ],
        )?;
        transaction.commit()?;
        Ok(RecordedPromotionAuthority {
            command_id: authority.command_id(),
            record_blob_id: authority_record_blob_id,
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

fn admission_id(kind: &str, subject: &str) -> ResearchAdmissionId {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"loom/research-admission/v1\0");
    bytes.extend_from_slice(kind.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(subject.as_bytes());
    ResearchAdmissionId(BlobId::digest(&bytes))
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
    use loom_document::DocumentContent;
    use loom_research_types::{
        AssemblyPartRecord, CallIdentity, CallScope, CallTerminal, CampaignId, CandidateAssemblyId,
        CandidateProjectionId, CompletedCall, GeneratedSpanOccurrenceId, ModelCallId,
        OutputProjection, StageAttemptId, StageId, TokenEvidence, TrialCaseId,
    };
    use tempfile::tempdir;

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
    fn admission_ids_are_domain_separated() {
        assert_ne!(
            admission_id("candidate_assembly", "same"),
            admission_id("candidate_projection", "same")
        );
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
            admitted_assembly.admission_id(),
            admitted_projection.admission_id()
        );
    }
}
