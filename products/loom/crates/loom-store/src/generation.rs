use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;

use loom_document::DocumentContent;
use loom_types::{
    ArtifactId, ArtifactKind, AuthorityPolicy, AuthorshipAttestation, BlobId, BranchCandidate,
    BranchId, ByteRange, CancelGenerationCommand, CandidateId, CommandId, CommandKind,
    CommandReceipt, ContextRecipe, ContributionKind, DocumentId, DocumentKind, GeneratedSpan,
    GenerationEvent, GenerationEventId, GenerationEventKind, GenerationRunId, GenerationStart,
    GenerationTerminalEvent, GenerationTerminalStatus, KeepAlternativeCommand, ModelEnvironment,
    ModelRole, OperationId, PromoteCandidateCommand, PromptRecipe, RevisionId, SelectionDecision,
    SelectionEvent, SelectionId, TokenTrace, now_unix_ms,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::provenance::{
    StoredSegment, document_media_type, insert_blob_row, insert_revision_segments,
    merge_adjacent_segments, slice_segments, validate_active_in_transaction,
    validate_expected_source,
};
use crate::store::{OutboxResult, ProjectStore, SaveOutcome, persist_receipt_in};
use crate::{MAX_DOCUMENT_BYTES, Result, StoreError};

const MAX_PROVENANCE_JSON_BYTES: usize = 16 * 1024 * 1024;
const MAX_EVENT_JSON_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RecordedArtifact {
    pub artifact_id: ArtifactId,
    pub blob_id: BlobId,
    pub operation_id: OperationId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GenerationStarted {
    pub run_artifact_id: ArtifactId,
    pub operation_id: OperationId,
    pub generation: GenerationStart,
    pub queued_event: GenerationEvent,
    pub receipt: CommandReceipt,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TerminalCandidateInput {
    pub output_bytes: Vec<u8>,
    pub token_trace: TokenTrace,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TerminalCandidateOutcome {
    pub candidate: BranchCandidate,
    pub operation_id: OperationId,
    pub candidate_ready_event: GenerationEvent,
    pub terminal_event: GenerationTerminalEvent,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CancelGenerationOutcome {
    pub event: GenerationEvent,
    pub receipt: CommandReceipt,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PromotionOutcome {
    pub save: SaveOutcome,
    pub candidate_id: CandidateId,
    pub selection_artifact_id: ArtifactId,
    pub attestation_artifact_id: ArtifactId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct KeepAlternativeOutcome {
    pub candidate_id: CandidateId,
    pub selection_artifact_id: ArtifactId,
    pub operation_id: OperationId,
    pub receipt: CommandReceipt,
}

impl ProjectStore {
    pub fn store_provenance_blob(&mut self, bytes: &[u8]) -> Result<BlobId> {
        ensure_payload_size("blob", bytes.len(), max_document_bytes_usize())?;
        let blob_id = self.put_blob(bytes)?;
        self.connection.execute(
            "INSERT OR IGNORE INTO blobs(blob_id, byte_len, media_type, created_at_ms)
             VALUES (?1, ?2, 'application/octet-stream', ?3)",
            params![
                blob_id.to_string(),
                i64::try_from(bytes.len()).map_err(|_| StoreError::DocumentTooLarge {
                    actual_bytes: u64::MAX,
                    max_bytes: MAX_DOCUMENT_BYTES,
                })?,
                now_unix_ms(),
            ],
        )?;
        Ok(blob_id)
    }

    pub fn record_model_environment(
        &mut self,
        environment: &ModelEnvironment,
    ) -> Result<RecordedArtifact> {
        self.record_registered_artifact(
            ArtifactKind::ModelEnvironment,
            environment,
            &[],
            |transaction, artifact_id, created_at_ms| {
                transaction.execute(
                    "INSERT INTO model_environments(artifact_id, environment_id, created_at_ms)
                     VALUES (?1, ?2, ?3)",
                    params![
                        artifact_id.to_string(),
                        environment.environment_id.to_string(),
                        created_at_ms,
                    ],
                )?;
                Ok(())
            },
        )
    }

    pub fn record_prompt_recipe(&mut self, recipe: &PromptRecipe) -> Result<RecordedArtifact> {
        self.require_blob(recipe.exact_prompt_blob_id)?;
        for artifact_id in &recipe.ordered_input_artifact_ids {
            self.require_artifact(*artifact_id)?;
        }
        self.record_registered_artifact(
            ArtifactKind::PromptRecipe,
            recipe,
            &recipe.ordered_input_artifact_ids,
            |transaction, artifact_id, created_at_ms| {
                transaction.execute(
                    "INSERT INTO prompt_recipes(artifact_id, exact_prompt_blob_id, created_at_ms)
                     VALUES (?1, ?2, ?3)",
                    params![
                        artifact_id.to_string(),
                        recipe.exact_prompt_blob_id.to_string(),
                        created_at_ms,
                    ],
                )?;
                insert_ordered_artifact_references(
                    transaction,
                    "prompt_recipe_inputs",
                    "input_artifact_id",
                    artifact_id,
                    &recipe.ordered_input_artifact_ids,
                )?;
                Ok(())
            },
        )
    }

    pub fn record_context_recipe(&mut self, recipe: &ContextRecipe) -> Result<RecordedArtifact> {
        self.require_revision(recipe.source_revision_id)?;
        for artifact_id in &recipe.ordered_source_artifact_ids {
            self.require_artifact(*artifact_id)?;
        }
        if let Some(blob_id) = recipe.retrieval_evidence_blob_id {
            self.require_blob(blob_id)?;
        }
        let source_revision_artifact: String = self.connection.query_row(
            "SELECT artifact_id FROM revisions WHERE revision_id = ?1",
            [recipe.source_revision_id.to_string()],
            |row| row.get(0),
        )?;
        let mut operation_inputs = vec![parse_id(
            &source_revision_artifact,
            "source revision artifact_id",
        )?];
        operation_inputs.extend(&recipe.ordered_source_artifact_ids);
        self.record_registered_artifact(
            ArtifactKind::ContextRecipe,
            recipe,
            &operation_inputs,
            |transaction, artifact_id, created_at_ms| {
                transaction.execute(
                    "INSERT INTO context_recipes(artifact_id, source_revision_id, retrieval_evidence_blob_id, created_at_ms)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        artifact_id.to_string(),
                        recipe.source_revision_id.to_string(),
                        recipe.retrieval_evidence_blob_id.map(|id| id.to_string()),
                        created_at_ms,
                    ],
                )?;
                insert_ordered_artifact_references(
                    transaction,
                    "context_recipe_sources",
                    "source_artifact_id",
                    artifact_id,
                    &recipe.ordered_source_artifact_ids,
                )?;
                Ok(())
            },
        )
    }

    pub fn record_authority_policy(
        &mut self,
        policy: &AuthorityPolicy,
    ) -> Result<RecordedArtifact> {
        validate_authority_policy(policy)?;
        for artifact_id in policy
            .writer_environment_artifact_ids
            .iter()
            .chain(&policy.critic_environment_artifact_ids)
        {
            self.require_registered_artifact(
                "model_environments",
                "artifact_id",
                *artifact_id,
                "model environment",
            )?;
        }
        let operation_inputs: Vec<_> = policy
            .writer_environment_artifact_ids
            .iter()
            .chain(&policy.critic_environment_artifact_ids)
            .copied()
            .collect();
        self.record_registered_artifact(
            ArtifactKind::AuthorityPolicy,
            policy,
            &operation_inputs,
            |transaction, artifact_id, created_at_ms| {
                transaction.execute(
                    "INSERT INTO authority_policies(artifact_id, policy_version, created_at_ms)
                     VALUES (?1, ?2, ?3)",
                    params![
                        artifact_id.to_string(),
                        i64::from(policy.policy_version),
                        created_at_ms,
                    ],
                )?;
                insert_policy_members(
                    transaction,
                    artifact_id,
                    ModelRole::Writer,
                    &policy.writer_environment_artifact_ids,
                )?;
                insert_policy_members(
                    transaction,
                    artifact_id,
                    ModelRole::Critic,
                    &policy.critic_environment_artifact_ids,
                )?;
                Ok(())
            },
        )
    }

    pub fn start_generation(&mut self, start: GenerationStart) -> Result<GenerationStarted> {
        self.start_generation_with_command(CommandId::new(), start)
    }

    #[allow(clippy::too_many_lines)]
    pub fn start_generation_with_command(
        &mut self,
        command_id: CommandId,
        start: GenerationStart,
    ) -> Result<GenerationStarted> {
        let started_at_ms = now_unix_ms();
        let document = self
            .document_by_id(start.document_id)?
            .ok_or_else(|| StoreError::NoActiveRevision(start.document_id.to_string()))?;
        let active = self
            .active_revision(start.document_id)?
            .ok_or_else(|| StoreError::NoActiveRevision(document.relative_path.clone()))?;
        if active.revision_id != start.source_revision_id {
            return Err(StoreError::SourceRevisionMismatch {
                expected: start.source_revision_id,
                actual: active.revision_id,
            });
        }
        self.verify_visible_source(&document.relative_path, active.blob_id)?;
        let source_bytes = self.read_blob(active.blob_id)?;
        validate_utf8_range(&source_bytes, start.target_range)?;
        self.require_generation_references(&start)?;
        let context_source: String = self.connection.query_row(
            "SELECT source_revision_id FROM context_recipes WHERE artifact_id = ?1",
            [start.context_recipe_artifact_id.to_string()],
            |row| row.get(0),
        )?;
        if parse_id::<RevisionId>(&context_source, "source_revision_id")?
            != start.source_revision_id
        {
            return Err(StoreError::SourceRevisionMismatch {
                expected: start.source_revision_id,
                actual: parse_id(&context_source, "source_revision_id")?,
            });
        }
        self.authority_role(
            start.authority_policy_artifact_id,
            start.model_environment_artifact_id,
        )?;

        let run_payload = bounded_json("generation run", &start, MAX_PROVENANCE_JSON_BYTES)?;
        let run_blob_id = self.put_blob(&run_payload)?;
        let run_artifact_id = ArtifactId::new();
        let operation_id = OperationId::new();
        let queued_event = GenerationEvent {
            event_id: GenerationEventId::new(),
            run_id: start.run_id,
            branch_id: start.branch_id,
            sequence: 0,
            kind: GenerationEventKind::Queued,
            occurred_at_ms: started_at_ms,
        };
        let receipt = CommandReceipt {
            command_id,
            command: CommandKind::Weave,
            project_id: self.manifest.project_id,
            project_schema_version: self.manifest.schema_version,
            source_revision_id: Some(start.source_revision_id),
            resulting_artifact_ids: vec![run_artifact_id],
            resulting_operation_ids: vec![operation_id],
            resulting_revision_ids: Vec::new(),
            started_at_ms,
            completed_at_ms: now_unix_ms(),
        };

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_active_in_transaction(&transaction, start.document_id, active)?;
        insert_blob_row(&transaction, run_blob_id, run_payload.len(), started_at_ms)?;
        insert_artifact(
            &transaction,
            run_artifact_id,
            run_blob_id,
            ArtifactKind::GenerationRun,
            "application/json",
            &json!({"run_id": start.run_id}),
            started_at_ms,
        )?;
        transaction.execute(
            "INSERT INTO operations(operation_id, operation_kind, metadata_json, created_at_ms)
             VALUES (?1, 'generate', ?2, ?3)",
            params![
                operation_id.to_string(),
                serde_json::to_string(&json!({"run_id": start.run_id}))?,
                started_at_ms,
            ],
        )?;
        for (position, artifact_id) in [
            active.artifact_id,
            start.model_environment_artifact_id,
            start.prompt_recipe_artifact_id,
            start.context_recipe_artifact_id,
            start.authority_policy_artifact_id,
        ]
        .into_iter()
        .enumerate()
        {
            transaction.execute(
                "INSERT INTO operation_inputs(operation_id, position, artifact_id)
                 VALUES (?1, ?2, ?3)",
                params![
                    operation_id.to_string(),
                    i64::try_from(position).map_err(|_| StoreError::CorruptDatabase(
                        "operation input position overflow".into()
                    ))?,
                    artifact_id.to_string(),
                ],
            )?;
        }
        transaction.execute(
            "INSERT INTO operation_outputs(operation_id, position, artifact_id) VALUES (?1, 0, ?2)",
            params![operation_id.to_string(), run_artifact_id.to_string()],
        )?;
        transaction.execute(
            "INSERT INTO branches(branch_id, document_id, source_revision_id, created_at_ms)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                start.branch_id.to_string(),
                start.document_id.to_string(),
                start.source_revision_id.to_string(),
                started_at_ms,
            ],
        )?;
        transaction.execute(
            "INSERT INTO generation_runs(
                run_id, branch_id, run_artifact_id, document_id, source_revision_id, source_blob_id,
                target_start_byte, target_end_byte, model_environment_artifact_id,
                prompt_recipe_artifact_id, context_recipe_artifact_id, authority_policy_artifact_id,
                created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                start.run_id.to_string(),
                start.branch_id.to_string(),
                run_artifact_id.to_string(),
                start.document_id.to_string(),
                start.source_revision_id.to_string(),
                active.blob_id.to_string(),
                range_start_i64(start.target_range)?,
                range_end_i64(start.target_range)?,
                start.model_environment_artifact_id.to_string(),
                start.prompt_recipe_artifact_id.to_string(),
                start.context_recipe_artifact_id.to_string(),
                start.authority_policy_artifact_id.to_string(),
                started_at_ms,
            ],
        )?;
        insert_generation_event(&transaction, &queued_event, false)?;
        persist_receipt_in(&transaction, &receipt)?;
        transaction.commit()?;
        Ok(GenerationStarted {
            run_artifact_id,
            operation_id,
            generation: start,
            queued_event,
            receipt,
        })
    }

    pub fn append_generation_event(
        &mut self,
        run_id: GenerationRunId,
        kind: GenerationEventKind,
    ) -> Result<GenerationEvent> {
        if matches!(kind, GenerationEventKind::CandidateReady { .. }) {
            return Err(StoreError::CandidateReadyRequiresTerminalCandidate);
        }
        let run = self.run_identity(run_id)?;
        let payload = bounded_json("generation event", &kind, MAX_EVENT_JSON_BYTES)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_generation_open(&transaction, run_id)?;
        let event = GenerationEvent {
            event_id: GenerationEventId::new(),
            run_id,
            branch_id: run.branch_id,
            sequence: next_sequence(&transaction, run_id)?,
            kind,
            occurred_at_ms: now_unix_ms(),
        };
        insert_generation_event_with_payload(&transaction, &event, &payload, false)?;
        transaction.commit()?;
        Ok(event)
    }

    pub fn request_cancel_generation(
        &mut self,
        command: CancelGenerationCommand,
    ) -> Result<CancelGenerationOutcome> {
        self.request_cancel_generation_with_command(CommandId::new(), command)
    }

    pub fn request_cancel_generation_with_command(
        &mut self,
        command_id: CommandId,
        command: CancelGenerationCommand,
    ) -> Result<CancelGenerationOutcome> {
        let CancelGenerationCommand { run_id } = command;
        let started_at_ms = now_unix_ms();
        let run = self.run_identity(run_id)?;
        let kind = GenerationEventKind::CancellationRequested;
        let payload = bounded_json("generation event", &kind, MAX_EVENT_JSON_BYTES)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_generation_open(&transaction, run_id)?;
        let event = GenerationEvent {
            event_id: GenerationEventId::new(),
            run_id,
            branch_id: run.branch_id,
            sequence: next_sequence(&transaction, run_id)?,
            kind,
            occurred_at_ms: now_unix_ms(),
        };
        let receipt = CommandReceipt {
            command_id,
            command: CommandKind::CancelGeneration,
            project_id: self.manifest.project_id,
            project_schema_version: self.manifest.schema_version,
            source_revision_id: Some(run.source_revision_id),
            resulting_artifact_ids: Vec::new(),
            resulting_operation_ids: Vec::new(),
            resulting_revision_ids: Vec::new(),
            started_at_ms,
            completed_at_ms: event.occurred_at_ms,
        };
        insert_generation_event_with_payload(&transaction, &event, &payload, false)?;
        persist_receipt_in(&transaction, &receipt)?;
        transaction.commit()?;
        Ok(CancelGenerationOutcome { event, receipt })
    }

    pub fn finish_generation(
        &mut self,
        run_id: GenerationRunId,
        status: GenerationTerminalStatus,
        error: Option<String>,
    ) -> Result<GenerationTerminalEvent> {
        if status == GenerationTerminalStatus::Completed {
            return Err(StoreError::CompletedGenerationRequiresCandidate);
        }
        if status == GenerationTerminalStatus::Failed && error.as_ref().is_none_or(String::is_empty)
        {
            return Err(StoreError::FailedGenerationRequiresError);
        }
        let run = self.run_identity(run_id)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_generation_open(&transaction, run_id)?;
        let event = GenerationTerminalEvent {
            event_id: GenerationEventId::new(),
            run_id,
            branch_id: run.branch_id,
            sequence: next_sequence(&transaction, run_id)?,
            status,
            candidate_id: None,
            error,
            occurred_at_ms: now_unix_ms(),
        };
        insert_terminal_event(&transaction, &event)?;
        transaction.commit()?;
        Ok(event)
    }

    #[allow(clippy::too_many_lines)]
    pub fn finish_generation_candidate(
        &mut self,
        run_id: GenerationRunId,
        input: TerminalCandidateInput,
    ) -> Result<TerminalCandidateOutcome> {
        let TerminalCandidateInput {
            output_bytes,
            token_trace,
        } = input;
        ensure_payload_size(
            "generated output",
            output_bytes.len(),
            max_document_bytes_usize(),
        )?;
        std::str::from_utf8(&output_bytes).map_err(|_| {
            StoreError::CorruptDatabase("generated output is not valid UTF-8".into())
        })?;
        let run = self.run_identity(run_id)?;
        self.ensure_generation_not_terminal(run_id)?;
        self.require_blob(token_trace.raw_event_stream_blob_id)?;
        if let Some(provenance) = &token_trace.provenance {
            if let Some(blob_id) = provenance.backend_receipt_blob_id {
                self.require_blob(blob_id)?;
            }
            if let Some(blob_id) = provenance.sequence_state_blob_id {
                self.require_blob(blob_id)?;
            }
        }

        let candidate_id = CandidateId::new();
        let token_trace_artifact_id = ArtifactId::new();
        let generated_span_artifact_id = ArtifactId::new();
        let operation_id = OperationId::new();
        let output_blob_id = self.put_blob(&output_bytes)?;
        let trace_payload = bounded_json("token trace", &token_trace, MAX_PROVENANCE_JSON_BYTES)?;
        let trace_blob_id = self.put_blob(&trace_payload)?;
        let output_end =
            u64::try_from(output_bytes.len()).map_err(|_| StoreError::DocumentTooLarge {
                actual_bytes: u64::MAX,
                max_bytes: MAX_DOCUMENT_BYTES,
            })?;
        let span = GeneratedSpan {
            candidate_id,
            run_id,
            branch_id: run.branch_id,
            output_blob_id,
            output_byte_range: ByteRange {
                start: 0,
                end: output_end,
            },
            token_trace_artifact_id,
        };
        bounded_json("generated span", &span, MAX_EVENT_JSON_BYTES)?;
        let created_at_ms = now_unix_ms();

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_generation_open(&transaction, run_id)?;
        insert_blob_row(
            &transaction,
            output_blob_id,
            output_bytes.len(),
            created_at_ms,
        )?;
        insert_blob_row(
            &transaction,
            trace_blob_id,
            trace_payload.len(),
            created_at_ms,
        )?;
        insert_artifact(
            &transaction,
            token_trace_artifact_id,
            trace_blob_id,
            ArtifactKind::TokenTrace,
            "application/json",
            &json!({"run_id": run_id}),
            created_at_ms,
        )?;
        insert_artifact(
            &transaction,
            generated_span_artifact_id,
            output_blob_id,
            ArtifactKind::GeneratedSpan,
            "text/plain; charset=utf-8",
            &span,
            created_at_ms,
        )?;
        transaction.execute(
            "INSERT INTO operations(operation_id, operation_kind, metadata_json, created_at_ms)
             VALUES (?1, 'generate', ?2, ?3)",
            params![
                operation_id.to_string(),
                serde_json::to_string(&json!({"run_id": run_id, "candidate_id": candidate_id}))?,
                created_at_ms,
            ],
        )?;
        transaction.execute(
            "INSERT INTO operation_inputs(operation_id, position, artifact_id) VALUES (?1, 0, ?2)",
            params![operation_id.to_string(), run.run_artifact_id.to_string()],
        )?;
        for (position, artifact_id) in [token_trace_artifact_id, generated_span_artifact_id]
            .into_iter()
            .enumerate()
        {
            transaction.execute(
                "INSERT INTO operation_outputs(operation_id, position, artifact_id) VALUES (?1, ?2, ?3)",
                params![
                    operation_id.to_string(),
                    i64::try_from(position).map_err(|_| StoreError::CorruptDatabase(
                        "operation output position overflow".into()
                    ))?,
                    artifact_id.to_string(),
                ],
            )?;
        }
        transaction.execute(
            "INSERT INTO generation_candidates(candidate_id, run_id, generated_span_artifact_id, token_trace_artifact_id, output_blob_id, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                candidate_id.to_string(),
                run_id.to_string(),
                generated_span_artifact_id.to_string(),
                token_trace_artifact_id.to_string(),
                output_blob_id.to_string(),
                created_at_ms,
            ],
        )?;
        let ready_event = GenerationEvent {
            event_id: GenerationEventId::new(),
            run_id,
            branch_id: run.branch_id,
            sequence: next_sequence(&transaction, run_id)?,
            kind: GenerationEventKind::CandidateReady {
                candidate_id,
                generated_span_artifact_id,
            },
            occurred_at_ms: created_at_ms,
        };
        insert_generation_event(&transaction, &ready_event, false)?;
        let terminal_event = GenerationTerminalEvent {
            event_id: GenerationEventId::new(),
            run_id,
            branch_id: run.branch_id,
            sequence: ready_event.sequence + 1,
            status: GenerationTerminalStatus::Completed,
            candidate_id: Some(candidate_id),
            error: None,
            occurred_at_ms: created_at_ms,
        };
        insert_terminal_event(&transaction, &terminal_event)?;
        transaction.commit()?;
        Ok(TerminalCandidateOutcome {
            candidate: BranchCandidate {
                candidate_id,
                run_id,
                branch_id: run.branch_id,
                generated_span_artifact_id,
                token_trace_artifact_id,
                output_blob_id,
            },
            operation_id,
            candidate_ready_event: ready_event,
            terminal_event,
        })
    }

    pub fn promote_candidate(
        &mut self,
        command: PromoteCandidateCommand,
    ) -> Result<PromotionOutcome> {
        self.promote_candidate_with_command(CommandId::new(), command)
    }

    #[allow(clippy::too_many_lines)]
    pub fn promote_candidate_with_command(
        &mut self,
        command_id: CommandId,
        command: PromoteCandidateCommand,
    ) -> Result<PromotionOutcome> {
        let PromoteCandidateCommand {
            candidate_id,
            expected_source_revision_id,
            expected_visible_blob_id,
        } = command;
        let started_at_ms = now_unix_ms();
        let candidate = self.candidate_context(candidate_id)?;
        match self.authority_role(
            candidate.authority_policy_artifact_id,
            candidate.model_environment_artifact_id,
        )? {
            ModelRole::Writer => {}
            ModelRole::Critic => return Err(StoreError::CriticCannotPromote),
        }
        if candidate.source_revision_id != expected_source_revision_id {
            return Err(StoreError::SourceRevisionMismatch {
                expected: expected_source_revision_id,
                actual: candidate.source_revision_id,
            });
        }
        if candidate.source_blob_id != expected_visible_blob_id {
            return Err(StoreError::SourceBlobMismatch {
                expected: expected_visible_blob_id,
                actual: candidate.source_blob_id,
            });
        }
        let active = self
            .active_revision(candidate.document_id)?
            .ok_or_else(|| StoreError::NoActiveRevision(candidate.relative_path.clone()))?;
        validate_expected_source(
            active,
            expected_source_revision_id,
            expected_visible_blob_id,
        )?;
        self.verify_visible_source(&candidate.relative_path, expected_visible_blob_id)?;

        let source_bytes = self.read_blob(candidate.source_blob_id)?;
        validate_utf8_range(&source_bytes, candidate.target_range)?;
        let generated_bytes = self.read_blob(candidate.output_blob_id)?;
        if generated_bytes.is_empty() {
            return Err(StoreError::InvalidGenerationRange);
        }
        let generated_content =
            DocumentContent::from_visible(candidate.document_kind, generated_bytes.clone())?;
        if generated_content.project_visible()?.bytes != generated_bytes {
            return Err(StoreError::NonCanonicalGeneratedText);
        }
        let start = usize::try_from(candidate.target_range.start)
            .map_err(|_| StoreError::InvalidGenerationRange)?;
        let end = usize::try_from(candidate.target_range.end)
            .map_err(|_| StoreError::InvalidGenerationRange)?;
        let mut promoted_bytes =
            Vec::with_capacity(source_bytes.len() - (end - start) + generated_bytes.len());
        promoted_bytes.extend_from_slice(&source_bytes[..start]);
        promoted_bytes.extend_from_slice(&generated_bytes);
        promoted_bytes.extend_from_slice(&source_bytes[end..]);
        ensure_payload_size(
            "promoted document",
            promoted_bytes.len(),
            max_document_bytes_usize(),
        )?;

        let source_segments = self.load_revision_segments(candidate.source_revision_id)?;
        let mut promoted_segments = slice_segments(&source_segments, 0, start)?;
        promoted_segments.push(StoredSegment {
            artifact_id: candidate.generated_span_artifact_id,
            start: 0,
            end: u64::try_from(generated_bytes.len())
                .map_err(|_| StoreError::InvalidGenerationRange)?,
            contribution: ContributionKind::Generated,
        });
        promoted_segments.extend(slice_segments(&source_segments, end, source_bytes.len())?);
        merge_adjacent_segments(&mut promoted_segments);

        let target_blob_id = self.put_blob(&promoted_bytes)?;
        let revision_artifact_id = ArtifactId::new();
        let selection_artifact_id = ArtifactId::new();
        let attestation_artifact_id = ArtifactId::new();
        let operation_id = OperationId::new();
        let revision_id = RevisionId::new();
        let selection_id = SelectionId::new();
        let created_at_ms = now_unix_ms();
        let selection = SelectionEvent {
            selection_id,
            candidate_id: candidate.candidate_id,
            decision: SelectionDecision::Promote,
            source_revision_id: candidate.source_revision_id,
            resulting_revision_id: Some(revision_id),
            command_id,
        };
        let attestation = AuthorshipAttestation {
            candidate_id: candidate.candidate_id,
            generated_span_artifact_id: candidate.generated_span_artifact_id,
            promoted_revision_id: revision_id,
            promotion_command_id: command_id,
            human_confirmed: true,
        };
        let selection_payload =
            bounded_json("selection event", &selection, MAX_PROVENANCE_JSON_BYTES)?;
        let attestation_payload = bounded_json(
            "authorship attestation",
            &attestation,
            MAX_PROVENANCE_JSON_BYTES,
        )?;
        let selection_blob_id = self.put_blob(&selection_payload)?;
        let attestation_blob_id = self.put_blob(&attestation_payload)?;
        let receipt = CommandReceipt {
            command_id,
            command: CommandKind::PromoteCandidate,
            project_id: self.manifest.project_id,
            project_schema_version: self.manifest.schema_version,
            source_revision_id: Some(candidate.source_revision_id),
            resulting_artifact_ids: vec![
                revision_artifact_id,
                selection_artifact_id,
                attestation_artifact_id,
            ],
            resulting_operation_ids: vec![operation_id],
            resulting_revision_ids: vec![revision_id],
            started_at_ms,
            completed_at_ms: created_at_ms,
        };

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_active_in_transaction(&transaction, candidate.document_id, active)?;
        for (blob_id, byte_len) in [
            (target_blob_id, promoted_bytes.len()),
            (selection_blob_id, selection_payload.len()),
            (attestation_blob_id, attestation_payload.len()),
        ] {
            insert_blob_row(&transaction, blob_id, byte_len, created_at_ms)?;
        }
        insert_artifact(
            &transaction,
            revision_artifact_id,
            target_blob_id,
            ArtifactKind::DocumentRevision,
            document_media_type(candidate.document_kind),
            &json!({
                "source_revision_id": candidate.source_revision_id,
                "candidate_id": candidate.candidate_id,
            }),
            created_at_ms,
        )?;
        insert_artifact(
            &transaction,
            selection_artifact_id,
            selection_blob_id,
            ArtifactKind::SelectionEvent,
            "application/json",
            &selection,
            created_at_ms,
        )?;
        insert_artifact(
            &transaction,
            attestation_artifact_id,
            attestation_blob_id,
            ArtifactKind::AuthorshipAttestation,
            "application/json",
            &attestation,
            created_at_ms,
        )?;
        transaction.execute(
            "INSERT INTO operations(operation_id, operation_kind, metadata_json, created_at_ms)
             VALUES (?1, 'select', ?2, ?3)",
            params![
                operation_id.to_string(),
                serde_json::to_string(&json!({
                    "decision": "promote",
                    "candidate_id": candidate.candidate_id,
                }))?,
                created_at_ms,
            ],
        )?;
        for (position, artifact_id) in [active.artifact_id, candidate.generated_span_artifact_id]
            .into_iter()
            .enumerate()
        {
            transaction.execute(
                "INSERT INTO operation_inputs(operation_id, position, artifact_id) VALUES (?1, ?2, ?3)",
                params![
                    operation_id.to_string(),
                    i64::try_from(position).map_err(|_| StoreError::CorruptDatabase(
                        "operation input position overflow".into()
                    ))?,
                    artifact_id.to_string(),
                ],
            )?;
        }
        for (position, artifact_id) in [
            revision_artifact_id,
            selection_artifact_id,
            attestation_artifact_id,
        ]
        .into_iter()
        .enumerate()
        {
            transaction.execute(
                "INSERT INTO operation_outputs(operation_id, position, artifact_id) VALUES (?1, ?2, ?3)",
                params![
                    operation_id.to_string(),
                    i64::try_from(position).map_err(|_| StoreError::CorruptDatabase(
                        "operation output position overflow".into()
                    ))?,
                    artifact_id.to_string(),
                ],
            )?;
        }
        transaction.execute(
            "INSERT INTO revisions(revision_id, document_id, parent_revision_id, artifact_id, reason, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, 'promoted generated candidate', ?5)",
            params![
                revision_id.to_string(),
                candidate.document_id.to_string(),
                candidate.source_revision_id.to_string(),
                revision_artifact_id.to_string(),
                created_at_ms,
            ],
        )?;
        insert_revision_segments(&transaction, revision_id, &promoted_segments)?;
        transaction.execute(
            "INSERT INTO visible_file_outbox(revision_id, relative_path, target_blob_id, expected_visible_blob_id, state, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, 'pending', ?5)",
            params![
                revision_id.to_string(),
                candidate.relative_path,
                target_blob_id.to_string(),
                candidate.source_blob_id.to_string(),
                created_at_ms,
            ],
        )?;
        let outbox_id = transaction.last_insert_rowid();
        persist_receipt_in(&transaction, &receipt)?;
        transaction.execute(
            "INSERT INTO selection_events(selection_artifact_id, selection_id, candidate_id, decision, source_revision_id, resulting_revision_id, command_id, created_at_ms)
             VALUES (?1, ?2, ?3, 'promote', ?4, ?5, ?6, ?7)",
            params![
                selection_artifact_id.to_string(),
                selection_id.to_string(),
                candidate.candidate_id.to_string(),
                candidate.source_revision_id.to_string(),
                revision_id.to_string(),
                command_id.to_string(),
                created_at_ms,
            ],
        )?;
        transaction.execute(
            "INSERT INTO authorship_attestations(attestation_artifact_id, candidate_id, generated_span_artifact_id, promoted_revision_id, promotion_command_id, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                attestation_artifact_id.to_string(),
                candidate.candidate_id.to_string(),
                candidate.generated_span_artifact_id.to_string(),
                revision_id.to_string(),
                command_id.to_string(),
                created_at_ms,
            ],
        )?;
        transaction.commit()?;

        match self.process_outbox_entry(outbox_id)? {
            OutboxResult::Applied | OutboxResult::AlreadyApplied => {}
            OutboxResult::Conflict { relative_path } => {
                return Err(StoreError::VisibleFileConflict {
                    outbox_id,
                    path: relative_path,
                });
            }
        }
        Ok(PromotionOutcome {
            save: SaveOutcome {
                blob_id: target_blob_id,
                artifact_id: revision_artifact_id,
                operation_id,
                revision_id,
                receipt,
            },
            candidate_id: candidate.candidate_id,
            selection_artifact_id,
            attestation_artifact_id,
        })
    }

    pub fn keep_alternative(
        &mut self,
        command: KeepAlternativeCommand,
    ) -> Result<KeepAlternativeOutcome> {
        self.keep_alternative_with_command(CommandId::new(), command)
    }

    pub fn keep_alternative_with_command(
        &mut self,
        command_id: CommandId,
        command: KeepAlternativeCommand,
    ) -> Result<KeepAlternativeOutcome> {
        let KeepAlternativeCommand { candidate_id } = command;
        let started_at_ms = now_unix_ms();
        let candidate = self.candidate_context(candidate_id)?;
        let selection_artifact_id = ArtifactId::new();
        let selection_id = SelectionId::new();
        let operation_id = OperationId::new();
        let selection = SelectionEvent {
            selection_id,
            candidate_id: candidate.candidate_id,
            decision: SelectionDecision::KeepAlternative,
            source_revision_id: candidate.source_revision_id,
            resulting_revision_id: None,
            command_id,
        };
        let payload = bounded_json("selection event", &selection, MAX_PROVENANCE_JSON_BYTES)?;
        let blob_id = self.put_blob(&payload)?;
        let receipt = CommandReceipt {
            command_id,
            command: CommandKind::KeepAlternative,
            project_id: self.manifest.project_id,
            project_schema_version: self.manifest.schema_version,
            source_revision_id: Some(candidate.source_revision_id),
            resulting_artifact_ids: vec![selection_artifact_id],
            resulting_operation_ids: vec![operation_id],
            resulting_revision_ids: Vec::new(),
            started_at_ms,
            completed_at_ms: now_unix_ms(),
        };
        let transaction = self.connection.transaction()?;
        insert_blob_row(&transaction, blob_id, payload.len(), started_at_ms)?;
        insert_artifact(
            &transaction,
            selection_artifact_id,
            blob_id,
            ArtifactKind::SelectionEvent,
            "application/json",
            &selection,
            started_at_ms,
        )?;
        transaction.execute(
            "INSERT INTO operations(operation_id, operation_kind, metadata_json, created_at_ms)
             VALUES (?1, 'select', ?2, ?3)",
            params![
                operation_id.to_string(),
                serde_json::to_string(&json!({"decision": "keep_alternative"}))?,
                started_at_ms,
            ],
        )?;
        transaction.execute(
            "INSERT INTO operation_inputs(operation_id, position, artifact_id) VALUES (?1, 0, ?2)",
            params![
                operation_id.to_string(),
                candidate.generated_span_artifact_id.to_string()
            ],
        )?;
        transaction.execute(
            "INSERT INTO operation_outputs(operation_id, position, artifact_id) VALUES (?1, 0, ?2)",
            params![operation_id.to_string(), selection_artifact_id.to_string()],
        )?;
        persist_receipt_in(&transaction, &receipt)?;
        transaction.execute(
            "INSERT INTO selection_events(selection_artifact_id, selection_id, candidate_id, decision, source_revision_id, resulting_revision_id, command_id, created_at_ms)
             VALUES (?1, ?2, ?3, 'keep_alternative', ?4, NULL, ?5, ?6)",
            params![
                selection_artifact_id.to_string(),
                selection_id.to_string(),
                candidate.candidate_id.to_string(),
                candidate.source_revision_id.to_string(),
                command_id.to_string(),
                started_at_ms,
            ],
        )?;
        transaction.commit()?;
        Ok(KeepAlternativeOutcome {
            candidate_id: candidate.candidate_id,
            selection_artifact_id,
            operation_id,
            receipt,
        })
    }

    pub fn generation_terminal_count(&self, run_id: GenerationRunId) -> Result<u64> {
        let count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM generation_events WHERE run_id = ?1 AND is_terminal = 1",
            [run_id.to_string()],
            |row| row.get(0),
        )?;
        u64::try_from(count)
            .map_err(|_| StoreError::CorruptDatabase("negative terminal event count".into()))
    }

    fn record_registered_artifact<T, F>(
        &mut self,
        kind: ArtifactKind,
        value: &T,
        operation_inputs: &[ArtifactId],
        specialize: F,
    ) -> Result<RecordedArtifact>
    where
        T: Serialize,
        F: FnOnce(&Transaction<'_>, ArtifactId, i64) -> Result<()>,
    {
        let payload = bounded_json("artifact", value, MAX_PROVENANCE_JSON_BYTES)?;
        let blob_id = self.put_blob(&payload)?;
        let artifact_id = ArtifactId::new();
        let operation_id = OperationId::new();
        let created_at_ms = now_unix_ms();
        let transaction = self.connection.transaction()?;
        insert_blob_row(&transaction, blob_id, payload.len(), created_at_ms)?;
        insert_artifact(
            &transaction,
            artifact_id,
            blob_id,
            kind,
            "application/json",
            &json!({}),
            created_at_ms,
        )?;
        transaction.execute(
            "INSERT INTO operations(operation_id, operation_kind, metadata_json, created_at_ms)
             VALUES (?1, 'import', ?2, ?3)",
            params![
                operation_id.to_string(),
                serde_json::to_string(&json!({"artifact_kind": kind.as_str()}))?,
                created_at_ms,
            ],
        )?;
        for (position, input_artifact_id) in operation_inputs.iter().enumerate() {
            transaction.execute(
                "INSERT INTO operation_inputs(operation_id, position, artifact_id)
                 VALUES (?1, ?2, ?3)",
                params![
                    operation_id.to_string(),
                    i64::try_from(position).map_err(|_| StoreError::CorruptDatabase(
                        "registered artifact input position overflow".into()
                    ))?,
                    input_artifact_id.to_string(),
                ],
            )?;
        }
        transaction.execute(
            "INSERT INTO operation_outputs(operation_id, position, artifact_id) VALUES (?1, 0, ?2)",
            params![operation_id.to_string(), artifact_id.to_string()],
        )?;
        specialize(&transaction, artifact_id, created_at_ms)?;
        transaction.commit()?;
        Ok(RecordedArtifact {
            artifact_id,
            blob_id,
            operation_id,
        })
    }

    fn require_blob(&self, blob_id: BlobId) -> Result<()> {
        let exists = self
            .connection
            .query_row(
                "SELECT 1 FROM blobs WHERE blob_id = ?1",
                [blob_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some();
        if !exists {
            return Err(StoreError::UnregisteredBlob(blob_id));
        }
        self.read_blob(blob_id).map(|_| ())
    }

    fn require_artifact(&self, artifact_id: ArtifactId) -> Result<()> {
        let exists = self
            .connection
            .query_row(
                "SELECT 1 FROM artifacts WHERE artifact_id = ?1",
                [artifact_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some();
        if !exists {
            return Err(StoreError::ArtifactKindMismatch {
                artifact_id,
                expected_kind: "artifact",
            });
        }
        Ok(())
    }

    fn require_revision(&self, revision_id: RevisionId) -> Result<()> {
        let exists = self
            .connection
            .query_row(
                "SELECT 1 FROM revisions WHERE revision_id = ?1",
                [revision_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some();
        if !exists {
            return Err(StoreError::NoActiveRevision(revision_id.to_string()));
        }
        Ok(())
    }

    fn require_registered_artifact(
        &self,
        table: &'static str,
        column: &'static str,
        artifact_id: ArtifactId,
        expected_kind: &'static str,
    ) -> Result<()> {
        let sql = match (table, column) {
            ("model_environments", "artifact_id") => {
                "SELECT 1 FROM model_environments WHERE artifact_id = ?1"
            }
            ("prompt_recipes", "artifact_id") => {
                "SELECT 1 FROM prompt_recipes WHERE artifact_id = ?1"
            }
            ("context_recipes", "artifact_id") => {
                "SELECT 1 FROM context_recipes WHERE artifact_id = ?1"
            }
            ("authority_policies", "artifact_id") => {
                "SELECT 1 FROM authority_policies WHERE artifact_id = ?1"
            }
            _ => {
                return Err(StoreError::CorruptDatabase(
                    "unsupported registered artifact lookup".into(),
                ));
            }
        };
        if self
            .connection
            .query_row(sql, [artifact_id.to_string()], |row| row.get::<_, i64>(0))
            .optional()?
            .is_none()
        {
            return Err(StoreError::ArtifactKindMismatch {
                artifact_id,
                expected_kind,
            });
        }
        Ok(())
    }

    fn require_generation_references(&self, start: &GenerationStart) -> Result<()> {
        for (table, artifact_id, expected) in [
            (
                "model_environments",
                start.model_environment_artifact_id,
                "model environment",
            ),
            (
                "prompt_recipes",
                start.prompt_recipe_artifact_id,
                "prompt recipe",
            ),
            (
                "context_recipes",
                start.context_recipe_artifact_id,
                "context recipe",
            ),
            (
                "authority_policies",
                start.authority_policy_artifact_id,
                "authority policy",
            ),
        ] {
            self.require_registered_artifact(table, "artifact_id", artifact_id, expected)?;
        }
        Ok(())
    }

    fn authority_role(
        &self,
        policy_artifact_id: ArtifactId,
        environment_artifact_id: ArtifactId,
    ) -> Result<ModelRole> {
        let role: Option<String> = self
            .connection
            .query_row(
                "SELECT role FROM authority_policy_members
                 WHERE policy_artifact_id = ?1 AND environment_artifact_id = ?2",
                params![
                    policy_artifact_id.to_string(),
                    environment_artifact_id.to_string()
                ],
                |row| row.get(0),
            )
            .optional()?;
        match role.as_deref() {
            Some("writer") => Ok(ModelRole::Writer),
            Some("critic") => Ok(ModelRole::Critic),
            Some(value) => Err(StoreError::CorruptDatabase(format!(
                "invalid authority role `{value}`"
            ))),
            None => Err(StoreError::ModelRoleNotAssigned),
        }
    }

    fn run_identity(&self, run_id: GenerationRunId) -> Result<RunIdentity> {
        let row: Option<(String, String, String)> = self
            .connection
            .query_row(
                "SELECT branch_id, run_artifact_id, source_revision_id
                 FROM generation_runs WHERE run_id = ?1",
                [run_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((branch_id, run_artifact_id, source_revision_id)) = row else {
            return Err(StoreError::GenerationRunNotFound(run_id));
        };
        Ok(RunIdentity {
            branch_id: parse_id(&branch_id, "branch_id")?,
            run_artifact_id: parse_id(&run_artifact_id, "run_artifact_id")?,
            source_revision_id: parse_id(&source_revision_id, "source_revision_id")?,
        })
    }

    fn ensure_generation_not_terminal(&self, run_id: GenerationRunId) -> Result<()> {
        let terminal = self
            .connection
            .query_row(
                "SELECT 1 FROM generation_terminals WHERE run_id = ?1",
                [run_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some();
        if terminal {
            return Err(StoreError::GenerationAlreadyTerminal(run_id));
        }
        Ok(())
    }

    fn document_by_id(&self, document_id: DocumentId) -> Result<Option<GenerationDocument>> {
        let relative_path: Option<String> = self
            .connection
            .query_row(
                "SELECT relative_path FROM documents WHERE document_id = ?1",
                [document_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(relative_path.map(|relative_path| GenerationDocument { relative_path }))
    }

    fn candidate_context(&self, candidate_id: CandidateId) -> Result<CandidateContext> {
        let row: Option<CandidateContextRow> = self
            .connection
            .query_row(
                "SELECT gc.generated_span_artifact_id, gc.output_blob_id,
                        gr.document_id, gr.source_revision_id, gr.source_blob_id,
                        gr.target_start_byte, gr.target_end_byte,
                        gr.model_environment_artifact_id, gr.authority_policy_artifact_id,
                        d.relative_path, d.document_kind
                 FROM generation_candidates gc
                 JOIN generation_runs gr ON gr.run_id = gc.run_id
                 JOIN documents d ON d.document_id = gr.document_id
                 WHERE gc.candidate_id = ?1",
                [candidate_id.to_string()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                    ))
                },
            )
            .optional()?;
        let Some(row) = row else {
            return Err(StoreError::CandidateNotFound(candidate_id));
        };
        Ok(CandidateContext {
            candidate_id,
            generated_span_artifact_id: parse_id(&row.0, "generated_span_artifact_id")?,
            output_blob_id: parse_blob_id(&row.1)?,
            document_id: parse_id(&row.2, "document_id")?,
            source_revision_id: parse_id(&row.3, "source_revision_id")?,
            source_blob_id: parse_blob_id(&row.4)?,
            target_range: ByteRange {
                start: u64::try_from(row.5)
                    .map_err(|_| StoreError::CorruptDatabase("negative target start".into()))?,
                end: u64::try_from(row.6)
                    .map_err(|_| StoreError::CorruptDatabase("negative target end".into()))?,
            },
            model_environment_artifact_id: parse_id(&row.7, "model_environment_artifact_id")?,
            authority_policy_artifact_id: parse_id(&row.8, "authority_policy_artifact_id")?,
            relative_path: row.9,
            document_kind: DocumentKind::from_str(&row.10)
                .map_err(|error| StoreError::CorruptDatabase(error.to_string()))?,
        })
    }
}

fn validate_authority_policy(policy: &AuthorityPolicy) -> Result<()> {
    if policy.policy_version == 0 || policy.writer_environment_artifact_ids.is_empty() {
        return Err(StoreError::InvalidAuthorityPolicy);
    }
    let mut members = HashSet::new();
    for artifact_id in policy
        .writer_environment_artifact_ids
        .iter()
        .chain(&policy.critic_environment_artifact_ids)
    {
        if !members.insert(*artifact_id) {
            return Err(StoreError::InvalidAuthorityPolicy);
        }
    }
    Ok(())
}

fn insert_policy_members(
    transaction: &Transaction<'_>,
    policy_artifact_id: ArtifactId,
    role: ModelRole,
    members: &[ArtifactId],
) -> Result<()> {
    for (position, environment_artifact_id) in members.iter().enumerate() {
        transaction.execute(
            "INSERT INTO authority_policy_members(policy_artifact_id, environment_artifact_id, role, position)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                policy_artifact_id.to_string(),
                environment_artifact_id.to_string(),
                role.as_str(),
                i64::try_from(position).map_err(|_| StoreError::InvalidAuthorityPolicy)?,
            ],
        )?;
    }
    Ok(())
}

fn bounded_json<T: Serialize>(field: &'static str, value: &T, max: usize) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value)?;
    ensure_payload_size(field, bytes.len(), max)?;
    Ok(bytes)
}

fn ensure_payload_size(field: &'static str, actual: usize, max: usize) -> Result<()> {
    if actual > max {
        return Err(StoreError::ProvenancePayloadTooLarge {
            field,
            max_bytes: max,
        });
    }
    Ok(())
}

fn insert_ordered_artifact_references(
    transaction: &Transaction<'_>,
    table: &'static str,
    column: &'static str,
    recipe_artifact_id: ArtifactId,
    artifact_ids: &[ArtifactId],
) -> Result<()> {
    let sql = match (table, column) {
        ("prompt_recipe_inputs", "input_artifact_id") => {
            "INSERT INTO prompt_recipe_inputs(recipe_artifact_id, position, input_artifact_id)
             VALUES (?1, ?2, ?3)"
        }
        ("context_recipe_sources", "source_artifact_id") => {
            "INSERT INTO context_recipe_sources(recipe_artifact_id, position, source_artifact_id)
             VALUES (?1, ?2, ?3)"
        }
        _ => {
            return Err(StoreError::CorruptDatabase(
                "unsupported ordered recipe reference table".into(),
            ));
        }
    };
    for (position, artifact_id) in artifact_ids.iter().enumerate() {
        transaction.execute(
            sql,
            params![
                recipe_artifact_id.to_string(),
                i64::try_from(position).map_err(|_| StoreError::CorruptDatabase(
                    "recipe reference position overflow".into()
                ))?,
                artifact_id.to_string(),
            ],
        )?;
    }
    Ok(())
}

fn insert_artifact<T: Serialize>(
    transaction: &Transaction<'_>,
    artifact_id: ArtifactId,
    blob_id: BlobId,
    kind: ArtifactKind,
    media_type: &str,
    metadata: &T,
    created_at_ms: i64,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO artifacts(artifact_id, blob_id, artifact_kind, media_type, metadata_json, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            artifact_id.to_string(),
            blob_id.to_string(),
            kind.as_str(),
            media_type,
            serde_json::to_string(metadata)?,
            created_at_ms,
        ],
    )?;
    Ok(())
}

fn insert_generation_event(
    transaction: &Transaction<'_>,
    event: &GenerationEvent,
    terminal: bool,
) -> Result<()> {
    let payload = bounded_json("generation event", &event.kind, MAX_EVENT_JSON_BYTES)?;
    insert_generation_event_with_payload(transaction, event, &payload, terminal)
}

fn insert_generation_event_with_payload(
    transaction: &Transaction<'_>,
    event: &GenerationEvent,
    payload: &[u8],
    terminal: bool,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO generation_events(event_id, run_id, sequence, event_kind, payload_json, is_terminal, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            event.event_id.to_string(),
            event.run_id.to_string(),
            i64::try_from(event.sequence).map_err(|_| StoreError::CorruptDatabase(
                "generation sequence overflow".into()
            ))?,
            event.kind.as_str(),
            std::str::from_utf8(payload).map_err(|_| StoreError::CorruptDatabase(
                "event JSON is not UTF-8".into()
            ))?,
            i64::from(terminal),
            event.occurred_at_ms,
        ],
    )?;
    Ok(())
}

fn insert_terminal_event(
    transaction: &Transaction<'_>,
    event: &GenerationTerminalEvent,
) -> Result<()> {
    let payload = bounded_json("terminal event", event, MAX_EVENT_JSON_BYTES)?;
    transaction.execute(
        "INSERT INTO generation_events(event_id, run_id, sequence, event_kind, payload_json, is_terminal, created_at_ms)
         VALUES (?1, ?2, ?3, 'terminal', ?4, 1, ?5)",
        params![
            event.event_id.to_string(),
            event.run_id.to_string(),
            i64::try_from(event.sequence).map_err(|_| StoreError::CorruptDatabase(
                "generation sequence overflow".into()
            ))?,
            std::str::from_utf8(&payload).map_err(|_| StoreError::CorruptDatabase(
                "terminal event JSON is not UTF-8".into()
            ))?,
            event.occurred_at_ms,
        ],
    )?;
    transaction.execute(
        "INSERT INTO generation_terminals(run_id, event_id, status, candidate_id, error, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            event.run_id.to_string(),
            event.event_id.to_string(),
            event.status.as_str(),
            event.candidate_id.map(|id| id.to_string()),
            event.error,
            event.occurred_at_ms,
        ],
    )?;
    Ok(())
}

fn ensure_generation_open(transaction: &Transaction<'_>, run_id: GenerationRunId) -> Result<()> {
    if transaction
        .query_row(
            "SELECT 1 FROM generation_terminals WHERE run_id = ?1",
            [run_id.to_string()],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .is_some()
    {
        return Err(StoreError::GenerationAlreadyTerminal(run_id));
    }
    Ok(())
}

fn next_sequence(transaction: &Transaction<'_>, run_id: GenerationRunId) -> Result<u64> {
    let next: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(sequence) + 1, 0) FROM generation_events WHERE run_id = ?1",
        [run_id.to_string()],
        |row| row.get(0),
    )?;
    u64::try_from(next)
        .map_err(|_| StoreError::CorruptDatabase("negative generation sequence".into()))
}

fn validate_utf8_range(bytes: &[u8], range: ByteRange) -> Result<()> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| StoreError::CorruptDatabase("revision blob is not UTF-8".into()))?;
    let start = usize::try_from(range.start).map_err(|_| StoreError::InvalidGenerationRange)?;
    let end = usize::try_from(range.end).map_err(|_| StoreError::InvalidGenerationRange)?;
    if start > end
        || end > bytes.len()
        || !text.is_char_boundary(start)
        || !text.is_char_boundary(end)
    {
        return Err(StoreError::InvalidGenerationRange);
    }
    Ok(())
}

fn range_start_i64(range: ByteRange) -> Result<i64> {
    i64::try_from(range.start).map_err(|_| StoreError::InvalidGenerationRange)
}

fn range_end_i64(range: ByteRange) -> Result<i64> {
    i64::try_from(range.end).map_err(|_| StoreError::InvalidGenerationRange)
}

fn parse_id<T>(value: &str, column: &str) -> Result<T>
where
    T: FromStr,
    T::Err: fmt::Display,
{
    value.parse().map_err(|error: T::Err| {
        StoreError::CorruptDatabase(format!("invalid {column} `{value}`: {error}"))
    })
}

fn parse_blob_id(value: &str) -> Result<BlobId> {
    value
        .parse()
        .map_err(|error| StoreError::CorruptDatabase(format!("invalid blob_id: {error}")))
}

#[derive(Clone, Debug)]
struct GenerationDocument {
    relative_path: String,
}

#[derive(Clone, Copy, Debug)]
#[allow(clippy::struct_field_names)]
struct RunIdentity {
    branch_id: BranchId,
    run_artifact_id: ArtifactId,
    source_revision_id: RevisionId,
}

#[derive(Clone, Debug)]
struct CandidateContext {
    candidate_id: CandidateId,
    generated_span_artifact_id: ArtifactId,
    output_blob_id: BlobId,
    document_id: DocumentId,
    source_revision_id: RevisionId,
    source_blob_id: BlobId,
    target_range: ByteRange,
    model_environment_artifact_id: ArtifactId,
    authority_policy_artifact_id: ArtifactId,
    relative_path: String,
    document_kind: DocumentKind,
}

type CandidateContextRow = (
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    String,
    String,
    String,
    String,
);

fn max_document_bytes_usize() -> usize {
    usize::try_from(MAX_DOCUMENT_BYTES).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use loom_document::DocumentContent;
    use loom_types::{
        ContextRecipe, GenerationMetrics, GenerationProvenance, InferenceEvidenceKind,
        ModelEnvironmentId, PromptMode,
    };
    use tempfile::tempdir;

    use super::*;

    struct Fixture {
        _directory: tempfile::TempDir,
        store: ProjectStore,
        loaded: crate::LoadedDocument,
        writer_environment: ArtifactId,
        critic_environment: ArtifactId,
        prompt_recipe: ArtifactId,
        context_recipe: ArtifactId,
        policy: ArtifactId,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempdir().expect("temporary project");
            let root = directory.path().join("Novel");
            let (mut store, _) = ProjectStore::initialize(&root, "Novel").expect("initialize");
            store
                .save_document(
                    "manuscript/001.md",
                    DocumentContent::Prose("Once ".into()),
                    "initial",
                )
                .expect("initial save");
            let loaded = store
                .read_document("manuscript/001.md")
                .expect("load initial document");
            let writer_environment = store
                .record_model_environment(&environment("writer"))
                .expect("record writer environment")
                .artifact_id;
            let critic_environment = store
                .record_model_environment(&environment("critic"))
                .expect("record critic environment")
                .artifact_id;
            let prompt_blob = store
                .store_provenance_blob(loaded.text.as_bytes())
                .expect("store prompt bytes");
            let prompt_recipe = store
                .record_prompt_recipe(&PromptRecipe {
                    mode: PromptMode::Completion,
                    exact_prompt_blob_id: prompt_blob,
                    exact_prompt_token_ids: None,
                    ordered_input_artifact_ids: vec![loaded.artifact_id],
                    prompt_token_count: None,
                })
                .expect("record prompt recipe")
                .artifact_id;
            let context_recipe = store
                .record_context_recipe(&ContextRecipe {
                    source_revision_id: loaded.revision_id,
                    ordered_source_artifact_ids: Vec::new(),
                    token_budget: 4_096,
                    retrieval_evidence_blob_id: None,
                })
                .expect("record context recipe")
                .artifact_id;
            let policy = store
                .record_authority_policy(&AuthorityPolicy {
                    policy_version: 1,
                    writer_environment_artifact_ids: vec![writer_environment],
                    critic_environment_artifact_ids: vec![critic_environment],
                })
                .expect("record policy")
                .artifact_id;
            Self {
                _directory: directory,
                store,
                loaded,
                writer_environment,
                critic_environment,
                prompt_recipe,
                context_recipe,
                policy,
            }
        }

        fn start(&mut self, environment: ArtifactId) -> GenerationStarted {
            let end = u64::try_from(self.loaded.text.len()).expect("document length");
            self.start_at(environment, ByteRange { start: end, end })
        }

        fn start_at(
            &mut self,
            environment: ArtifactId,
            target_range: ByteRange,
        ) -> GenerationStarted {
            self.store
                .start_generation(GenerationStart {
                    run_id: GenerationRunId::new(),
                    branch_id: BranchId::new(),
                    document_id: self.loaded.document_id,
                    source_revision_id: self.loaded.revision_id,
                    target_range,
                    model_environment_artifact_id: environment,
                    prompt_recipe_artifact_id: self.prompt_recipe,
                    context_recipe_artifact_id: self.context_recipe,
                    authority_policy_artifact_id: self.policy,
                    seed: 7,
                    sampling: json!({"temperature": 0.8}),
                })
                .expect("start generation")
        }

        fn finish(&mut self, run_id: GenerationRunId, output: &str) -> TerminalCandidateOutcome {
            let raw_event_stream_blob_id = self
                .store
                .store_provenance_blob(b"recorded event stream")
                .expect("store event stream");
            self.store
                .finish_generation_candidate(
                    run_id,
                    TerminalCandidateInput {
                        output_bytes: output.as_bytes().to_vec(),
                        token_trace: TokenTrace {
                            generated_token_ids: vec![10, 11],
                            observations: Vec::new(),
                            raw_event_stream_blob_id,
                            provenance: Some(GenerationProvenance {
                                evidence_kind: InferenceEvidenceKind::LiveInference,
                                metrics: GenerationMetrics::default(),
                                backend_receipt_blob_id: None,
                                sequence_state_blob_id: None,
                            }),
                        },
                    },
                )
                .expect("finish generation candidate")
        }
    }

    fn environment(name: &str) -> ModelEnvironment {
        ModelEnvironment {
            environment_id: ModelEnvironmentId::digest(name.as_bytes()),
            model_identifier: format!("test/{name}"),
            model_fingerprint: BlobId::digest(format!("model-{name}").as_bytes()),
            tokenizer_fingerprint: BlobId::digest(format!("tokenizer-{name}").as_bytes()),
            backend_identifier: "test-backend".into(),
            capabilities: json!({"completion": true}),
        }
    }

    #[test]
    fn private_generation_and_keep_do_not_mutate_active_manuscript() {
        let mut fixture = Fixture::new();
        let start = fixture.start(fixture.writer_environment);
        fixture
            .store
            .append_generation_event(start.generation.run_id, GenerationEventKind::Prefilling)
            .expect("prefilling event");
        let terminal = fixture.finish(start.generation.run_id, "upon a time");

        let before_promotion = fixture
            .store
            .read_document("manuscript/001.md")
            .expect("read private branch state");
        assert_eq!(before_promotion.revision_id, fixture.loaded.revision_id);
        assert_eq!(before_promotion.text, "Once ");
        fixture
            .store
            .keep_alternative(KeepAlternativeCommand {
                candidate_id: terminal.candidate.candidate_id,
            })
            .expect("keep alternative");
        let after_keep = fixture
            .store
            .read_document("manuscript/001.md")
            .expect("read after keep");
        assert_eq!(after_keep.revision_id, fixture.loaded.revision_id);
        assert_eq!(after_keep.text, "Once ");

        fixture
            .store
            .promote_candidate(PromoteCandidateCommand {
                candidate_id: terminal.candidate.candidate_id,
                expected_source_revision_id: fixture.loaded.revision_id,
                expected_visible_blob_id: fixture.loaded.blob_id,
            })
            .expect("promote candidate");
        assert_eq!(
            fixture
                .store
                .read_document("manuscript/001.md")
                .expect("read promoted document")
                .text,
            "Once upon a time"
        );
    }

    #[test]
    fn recipes_validate_and_preserve_ordered_artifact_references() {
        let mut fixture = Fixture::new();
        let prompt_input_count: i64 = fixture
            .store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM prompt_recipe_inputs WHERE recipe_artifact_id = ?1",
                [fixture.prompt_recipe.to_string()],
                |row| row.get(0),
            )
            .expect("prompt input count");
        assert_eq!(prompt_input_count, 1);

        let context = fixture
            .store
            .record_context_recipe(&ContextRecipe {
                source_revision_id: fixture.loaded.revision_id,
                ordered_source_artifact_ids: vec![fixture.loaded.artifact_id],
                token_budget: 256,
                retrieval_evidence_blob_id: None,
            })
            .expect("context with ordered source");
        let source: String = fixture
            .store
            .connection
            .query_row(
                "SELECT source_artifact_id FROM context_recipe_sources
                 WHERE recipe_artifact_id = ?1 AND position = 0",
                [context.artifact_id.to_string()],
                |row| row.get(0),
            )
            .expect("ordered context source");
        assert_eq!(source, fixture.loaded.artifact_id.to_string());

        let invalid = fixture.store.record_prompt_recipe(&PromptRecipe {
            mode: PromptMode::Completion,
            exact_prompt_blob_id: fixture.loaded.blob_id,
            exact_prompt_token_ids: None,
            ordered_input_artifact_ids: vec![ArtifactId::new()],
            prompt_token_count: None,
        });
        assert!(matches!(
            invalid,
            Err(StoreError::ArtifactKindMismatch {
                expected_kind: "artifact",
                ..
            })
        ));
    }

    #[test]
    fn duplicate_outputs_share_blob_but_keep_distinct_occurrences() {
        let mut fixture = Fixture::new();
        let first_start = fixture.start(fixture.writer_environment);
        let first = fixture.finish(first_start.generation.run_id, "same continuation");
        let second_start = fixture.start(fixture.writer_environment);
        let second = fixture.finish(second_start.generation.run_id, "same continuation");
        assert_eq!(
            first.candidate.output_blob_id,
            second.candidate.output_blob_id
        );
        assert_ne!(first.candidate.candidate_id, second.candidate.candidate_id);
        assert_ne!(
            first.candidate.generated_span_artifact_id,
            second.candidate.generated_span_artifact_id
        );
        assert_ne!(first.operation_id, second.operation_id);
    }

    #[test]
    fn critic_candidate_cannot_be_promoted() {
        let mut fixture = Fixture::new();
        let start = fixture.start(fixture.critic_environment);
        let terminal = fixture.finish(start.generation.run_id, "critic prose");
        let result = fixture.store.promote_candidate(PromoteCandidateCommand {
            candidate_id: terminal.candidate.candidate_id,
            expected_source_revision_id: fixture.loaded.revision_id,
            expected_visible_blob_id: fixture.loaded.blob_id,
        });
        assert!(matches!(result, Err(StoreError::CriticCannotPromote)));
        assert_eq!(
            fixture
                .store
                .read_document("manuscript/001.md")
                .expect("active manuscript")
                .text,
            "Once "
        );
    }

    #[test]
    fn generation_has_exactly_one_terminal_event() {
        let mut fixture = Fixture::new();
        let start = fixture.start(fixture.writer_environment);
        fixture.finish(start.generation.run_id, "one terminal");
        assert_eq!(
            fixture
                .store
                .generation_terminal_count(start.generation.run_id)
                .expect("terminal count"),
            1
        );
        assert!(matches!(
            fixture.store.finish_generation(
                start.generation.run_id,
                GenerationTerminalStatus::Cancelled,
                None,
            ),
            Err(StoreError::GenerationAlreadyTerminal(_))
        ));
        assert_eq!(
            fixture
                .store
                .generation_terminal_count(start.generation.run_id)
                .expect("terminal count"),
            1
        );
        let mutation = fixture.store.connection.execute(
            "UPDATE generation_events SET event_kind = 'warning' WHERE run_id = ?1",
            [start.generation.run_id.to_string()],
        );
        assert!(mutation.is_err());
    }

    #[test]
    fn one_character_edit_preserves_unmodified_generated_slices() {
        let mut fixture = Fixture::new();
        let start = fixture.start(fixture.writer_environment);
        let terminal = fixture.finish(start.generation.run_id, "upon a time");
        let promoted = fixture
            .store
            .promote_candidate(PromoteCandidateCommand {
                candidate_id: terminal.candidate.candidate_id,
                expected_source_revision_id: fixture.loaded.revision_id,
                expected_visible_blob_id: fixture.loaded.blob_id,
            })
            .expect("promote writer candidate");
        let edited = fixture
            .store
            .save_document_if_source(
                "manuscript/001.md",
                DocumentContent::Prose("Once upon a tome".into()),
                "one character edit",
                promoted.save.revision_id,
                promoted.save.blob_id,
            )
            .expect("source-bound edit");
        assert_eq!(
            fixture
                .store
                .reconstruct_revision(edited.save.revision_id)
                .expect("reconstruct revision"),
            b"Once upon a tome"
        );
        let provenance = fixture
            .store
            .revision_provenance(edited.save.revision_id)
            .expect("revision provenance");
        let generated_bytes: u64 = provenance
            .segments
            .iter()
            .filter(|segment| {
                segment.artifact_id == terminal.candidate.generated_span_artifact_id
                    && segment.contribution == ContributionKind::Generated
            })
            .map(|segment| segment.byte_range.len())
            .sum();
        let human_bytes: u64 = provenance
            .segments
            .iter()
            .filter(|segment| segment.contribution == ContributionKind::Human)
            .map(|segment| segment.byte_range.len())
            .sum();
        assert_eq!(generated_bytes, "upon a time".len() as u64 - 1);
        assert_eq!(human_bytes, "Once ".len() as u64 + 1);
    }

    #[test]
    fn two_disjoint_edits_preserve_generated_text_between_them() {
        let mut fixture = Fixture::new();
        let start = fixture.start_at(fixture.writer_environment, ByteRange { start: 2, end: 2 });
        let terminal = fixture.finish(start.generation.run_id, "MODEL");
        let promoted = fixture
            .store
            .promote_candidate(PromoteCandidateCommand {
                candidate_id: terminal.candidate.candidate_id,
                expected_source_revision_id: fixture.loaded.revision_id,
                expected_visible_blob_id: fixture.loaded.blob_id,
            })
            .expect("promote generated middle");
        assert_eq!(
            fixture
                .store
                .read_document("manuscript/001.md")
                .expect("promoted middle")
                .text,
            "OnMODELce "
        );
        let edited = fixture
            .store
            .save_document_if_source(
                "manuscript/001.md",
                DocumentContent::Prose("OxMODELce!".into()),
                "two disjoint edits",
                promoted.save.revision_id,
                promoted.save.blob_id,
            )
            .expect("save disjoint edits");
        let provenance = fixture
            .store
            .revision_provenance(edited.save.revision_id)
            .expect("edited provenance");
        let retained_generated_bytes: u64 = provenance
            .segments
            .iter()
            .filter(|segment| {
                segment.artifact_id == terminal.candidate.generated_span_artifact_id
                    && segment.contribution == ContributionKind::Generated
            })
            .map(|segment| segment.byte_range.len())
            .sum();
        assert_eq!(retained_generated_bytes, "MODEL".len() as u64);
        assert_eq!(
            fixture
                .store
                .reconstruct_revision(edited.save.revision_id)
                .expect("reconstruct disjoint edit"),
            b"OxMODELce!"
        );
    }

    #[test]
    fn empty_revision_survives_delete_type_and_undo() {
        let mut fixture = Fixture::new();
        let deleted = fixture
            .store
            .save_document_if_source(
                "manuscript/001.md",
                DocumentContent::Prose(String::new()),
                "delete all",
                fixture.loaded.revision_id,
                fixture.loaded.blob_id,
            )
            .expect("delete all text");
        assert!(
            fixture
                .store
                .revision_provenance(deleted.save.revision_id)
                .expect("empty provenance")
                .segments
                .is_empty()
        );
        assert!(
            fixture
                .store
                .reconstruct_revision(deleted.save.revision_id)
                .expect("reconstruct empty")
                .is_empty()
        );
        let typed = fixture
            .store
            .save_document_if_source(
                "manuscript/001.md",
                DocumentContent::Prose("x".into()),
                "type",
                deleted.save.revision_id,
                deleted.save.blob_id,
            )
            .expect("type into empty revision");
        let undone = fixture
            .store
            .save_document_if_source(
                "manuscript/001.md",
                DocumentContent::Prose(String::new()),
                "undo",
                typed.save.revision_id,
                typed.save.blob_id,
            )
            .expect("undo to empty revision");
        assert!(
            fixture
                .store
                .reconstruct_revision(undone.save.revision_id)
                .expect("reconstruct undone empty")
                .is_empty()
        );
        assert_eq!(
            fixture
                .store
                .read_document("manuscript/001.md")
                .expect("read empty document")
                .text,
            ""
        );
    }

    #[test]
    fn source_bound_save_rejects_external_edit_without_overwrite() {
        let mut fixture = Fixture::new();
        let visible = fixture.store.root.join("manuscript/001.md");
        fs::write(&visible, "external").expect("external edit");
        let result = fixture.store.save_document_if_source(
            "manuscript/001.md",
            DocumentContent::Prose("editor value".into()),
            "idle save",
            fixture.loaded.revision_id,
            fixture.loaded.blob_id,
        );
        assert!(matches!(result, Err(StoreError::SourceBlobMismatch { .. })));
        assert_eq!(
            fs::read_to_string(visible).expect("visible text"),
            "external"
        );
    }

    #[test]
    fn idempotent_checkpoint_replays_and_rejects_fingerprint_reuse() {
        let mut fixture = Fixture::new();
        let command_id = CommandId::new();
        let first = fixture
            .store
            .save_document_if_source_idempotent(
                command_id,
                "manuscript/001.md",
                DocumentContent::Prose("Once revised".into()),
                "idle save",
                fixture.loaded.revision_id,
                fixture.loaded.blob_id,
            )
            .expect("first idempotent checkpoint");
        assert!(!first.replayed);
        assert_eq!(
            first.visible_projection,
            crate::VisibleProjectionState::Applied
        );

        fixture
            .store
            .connection
            .execute(
                "UPDATE visible_file_outbox SET state = 'pending', completed_at_ms = NULL
                 WHERE revision_id = ?1",
                [first.save.revision_id.to_string()],
            )
            .expect("simulate crash before outbox acknowledgement");
        fs::write(fixture.store.root.join("manuscript/001.md"), "Once ")
            .expect("restore expected predecessor");
        let replay = fixture
            .store
            .save_document_if_source_idempotent(
                command_id,
                "manuscript/001.md",
                DocumentContent::Prose("Once revised".into()),
                "idle save",
                fixture.loaded.revision_id,
                fixture.loaded.blob_id,
            )
            .expect("replay idempotent checkpoint");
        assert!(replay.replayed);
        assert_eq!(
            replay.visible_projection,
            crate::VisibleProjectionState::Applied
        );
        assert_eq!(replay.save.revision_id, first.save.revision_id);
        assert_eq!(
            fs::read_to_string(fixture.store.root.join("manuscript/001.md"))
                .expect("replayed visible text"),
            "Once revised"
        );
        let mismatch = fixture.store.save_document_if_source_idempotent(
            command_id,
            "manuscript/001.md",
            DocumentContent::Prose("different request".into()),
            "idle save",
            fixture.loaded.revision_id,
            fixture.loaded.blob_id,
        );
        assert!(matches!(
            mismatch,
            Err(StoreError::IdempotencyConflict { .. })
        ));
    }

    #[test]
    fn committed_checkpoint_returns_typed_pending_state_at_projection_boundary() {
        let mut fixture = Fixture::new();
        let command_id = CommandId::new();
        let outcome = fixture
            .store
            .save_document_if_source_idempotent_with_boundary(
                command_id,
                "manuscript/001.md",
                DocumentContent::Prose("editor value".into()),
                "idle save",
                fixture.loaded.revision_id,
                fixture.loaded.blob_id,
                |visible| {
                    fs::write(visible, "external value")?;
                    Ok(())
                },
            )
            .expect("semantic checkpoint remains a successful typed outcome");

        assert!(!outcome.replayed);
        assert!(matches!(
            &outcome.visible_projection,
            crate::VisibleProjectionState::PendingConflict {
                relative_path,
                ..
            } if relative_path == "manuscript/001.md"
        ));
        assert_eq!(
            serde_json::to_value(&outcome.visible_projection).expect("serialize projection state")
                ["status"],
            "pending_conflict"
        );
        assert_eq!(
            fs::read_to_string(fixture.store.root.join("manuscript/001.md"))
                .expect("preserve external bytes"),
            "external value"
        );
        assert_eq!(
            fixture
                .store
                .pending_outbox_count()
                .expect("pending outbox"),
            1
        );
        assert!(
            fixture
                .store
                .load_receipt(command_id)
                .expect("load committed receipt")
                .is_some()
        );

        let replay = fixture
            .store
            .save_document_if_source_idempotent(
                command_id,
                "manuscript/001.md",
                DocumentContent::Prose("editor value".into()),
                "idle save",
                fixture.loaded.revision_id,
                fixture.loaded.blob_id,
            )
            .expect("replay exposes the same pending state without a plain error");
        assert!(replay.replayed);
        assert_eq!(replay.save.revision_id, outcome.save.revision_id);
        assert!(matches!(
            replay.visible_projection,
            crate::VisibleProjectionState::PendingConflict { .. }
        ));
    }

    #[test]
    fn committed_checkpoint_wraps_projection_failure_as_retryable_state() {
        let mut fixture = Fixture::new();
        let command_id = CommandId::new();
        let outcome = fixture
            .store
            .save_document_if_source_idempotent_with_boundary(
                command_id,
                "manuscript/001.md",
                DocumentContent::Prose("editor value".into()),
                "idle save",
                fixture.loaded.revision_id,
                fixture.loaded.blob_id,
                |_| Err(std::io::Error::other("injected projection failure").into()),
            )
            .expect("post-commit projection failure is outcome state");

        assert!(matches!(
            &outcome.visible_projection,
            crate::VisibleProjectionState::PendingRetry { error, .. }
                if error.contains("injected projection failure")
        ));
        assert_eq!(
            serde_json::to_value(&outcome.visible_projection).expect("serialize retry state")["status"],
            "pending_retry"
        );
        assert!(
            fixture
                .store
                .load_receipt(command_id)
                .expect("load committed receipt")
                .is_some()
        );

        let replay = fixture
            .store
            .save_document_if_source_idempotent(
                command_id,
                "manuscript/001.md",
                DocumentContent::Prose("editor value".into()),
                "idle save",
                fixture.loaded.revision_id,
                fixture.loaded.blob_id,
            )
            .expect("exact replay finishes the pending projection");
        assert!(replay.replayed);
        assert_eq!(replay.save.revision_id, outcome.save.revision_id);
        assert_eq!(
            replay.visible_projection,
            crate::VisibleProjectionState::Applied
        );
        assert_eq!(
            fs::read_to_string(fixture.store.root.join("manuscript/001.md"))
                .expect("read projected checkpoint"),
            "editor value"
        );
    }

    #[test]
    fn source_bound_checkpoint_rejects_unbounded_diff_work() {
        let mut fixture = Fixture::new();
        let oversized_changed_window = "z".repeat(crate::MAX_EDIT_DIFF_WINDOW_BYTES + 1);
        let result = fixture.store.save_document_if_source(
            "manuscript/001.md",
            DocumentContent::Prose(oversized_changed_window),
            "oversized edit window",
            fixture.loaded.revision_id,
            fixture.loaded.blob_id,
        );
        assert!(matches!(
            result,
            Err(StoreError::EditDiffBudgetExceeded {
                metric: "changed-window bytes",
                ..
            })
        ));
        assert_eq!(
            fixture
                .store
                .read_document("manuscript/001.md")
                .expect("source remains active")
                .text,
            "Once "
        );
    }
}
