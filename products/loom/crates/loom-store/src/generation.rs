use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use loom_document::DocumentContent;
use loom_types::{
    ArtifactId, ArtifactKind, AuthorityPolicy, AuthorshipAttestation, BlobId, BranchCandidate,
    BranchId, ByteRange, CancelGenerationCommand, CandidateId, CommandId, CommandKind,
    CommandReceipt, ContextRecipe, ContributionKind, DocumentId, DocumentKind, GeneratedSpan,
    GenerationEvent, GenerationEventId, GenerationEventKind, GenerationRunId, GenerationStart,
    GenerationTerminalEvent, GenerationTerminalStatus, KeepAlternativeCommand, ModelEnvironment,
    ModelEnvironmentId, ModelRole, OperationId, PromoteCandidateCommand, PromptRecipe, RevisionId,
    SelectionDecision, SelectionEvent, SelectionId, TokenTrace, now_unix_ms,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::provenance::{
    StoredSegment, document_media_type, insert_blob_row, insert_revision_segments,
    merge_adjacent_segments, slice_segments, validate_active_in_transaction,
    validate_expected_source,
};
use crate::store::{
    ActiveRevision, ProjectStore, SaveOutcome, VisibleProjectionState, persist_receipt_in,
};
use crate::{MAX_DOCUMENT_BYTES, Result, StoreError};

const MAX_PROVENANCE_JSON_BYTES: usize = 16 * 1024 * 1024;
const MAX_EVENT_JSON_BYTES: usize = 1024 * 1024;
const MAX_BRANCH_ERROR_CHARACTERS: usize = 1_024;
const MAX_INDEXED_MODEL_IDENTIFIER_BYTES: usize = 4_096;
pub const MAX_BRANCH_PAGE_SIZE: usize = 64;
pub const MAX_BRANCH_BODY_BYTES: u64 = 4 * 1024 * 1024;
const BRANCH_SUMMARY_SELECT: &str = "SELECT gr.run_id, gr.branch_id, gr.source_revision_id,
            gr.target_start_byte, gr.target_end_byte, gr.created_at_ms,
            gri.sequence, gri.seed_decimal, gri.model_identifier,
            gt.status,
            substr(gt.error, 1, 1024),
            CASE WHEN length(gt.error) > 1024 THEN 1 ELSE 0 END,
            gc.candidate_id,
            gc.output_blob_id,
            output_blob.byte_len,
            (SELECT se.decision FROM selection_events se
             WHERE se.candidate_id = gc.candidate_id
             ORDER BY se.created_at_ms DESC, se.selection_id DESC
             LIMIT 1)
     FROM generation_runs gr
     JOIN generation_run_index gri ON gri.run_id = gr.run_id
     LEFT JOIN generation_terminals gt ON gt.run_id = gr.run_id
     LEFT JOIN generation_candidates gc ON gc.run_id = gr.run_id
     LEFT JOIN blobs output_blob
       ON output_blob.blob_id = gc.output_blob_id";

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
pub struct GenerationFamilyStarted {
    pub generations: Vec<GenerationStarted>,
    pub receipt: CommandReceipt,
    pub request_fingerprint: BlobId,
    pub replayed: bool,
}

/// Terminal error recorded when a fresh host explicitly reconciles generation
/// runs that have durable starts but no surviving in-process worker.
pub const INTERRUPTED_GENERATION_ERROR: &str =
    "generation worker did not survive the previous Loom process";
const UNSUPPLIED_TERMINAL_EVIDENCE: &[u8] =
    br#"{"evidence":"unavailable","reason":"legacy terminal API supplied no backend event stream"}"#;

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
    pub evidence: GenerationTerminalEvidence,
}

/// Evidence supplied when a run terminates without a promotable candidate.
/// Empty partial output and an unavailable backend receipt are represented
/// explicitly; terminal evidence is never inferred from UI deltas.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TerminalEvidenceInput {
    pub partial_output_bytes: Vec<u8>,
    pub token_trace: TokenTrace,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TerminalGenerationInput {
    pub status: GenerationTerminalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub evidence: TerminalEvidenceInput,
}

/// Immutable identities binding a terminal occurrence to its raw output and
/// token evidence, even when no candidate may be promoted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GenerationTerminalEvidence {
    pub run_id: GenerationRunId,
    pub status: GenerationTerminalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_id: Option<CandidateId>,
    pub operation_id: OperationId,
    pub output_artifact_id: ArtifactId,
    pub output_blob_id: BlobId,
    pub token_trace_artifact_id: ArtifactId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredGenerationTerminalEvidence {
    pub evidence: GenerationTerminalEvidence,
    pub output_bytes: Vec<u8>,
    pub token_trace: TokenTrace,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TerminalGenerationOutcome {
    pub terminal_event: GenerationTerminalEvent,
    pub evidence: GenerationTerminalEvidence,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CancelGenerationOutcome {
    pub event: GenerationEvent,
    pub receipt: CommandReceipt,
    pub request_fingerprint: BlobId,
    pub replayed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PromotionOutcome {
    pub save: SaveOutcome,
    pub candidate_id: CandidateId,
    pub selection_artifact_id: ArtifactId,
    pub attestation_artifact_id: ArtifactId,
    pub visible_projection: VisibleProjectionState,
    pub request_fingerprint: BlobId,
    pub replayed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct KeepAlternativeOutcome {
    pub candidate_id: CandidateId,
    pub selection_artifact_id: ArtifactId,
    pub operation_id: OperationId,
    pub receipt: CommandReceipt,
    pub request_fingerprint: BlobId,
    pub replayed: bool,
}

/// Durable branch state reconstructed from immutable generation records.
///
/// This is deliberately a store projection rather than editor state. A
/// browser may lose every in-memory card and rebuild the branch shelf from
/// this record after restart without inventing generation evidence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredBranchRecord {
    pub run_id: GenerationRunId,
    pub branch_id: BranchId,
    pub document_id: DocumentId,
    pub source_revision_id: RevisionId,
    pub target_range: ByteRange,
    pub model_identifier: String,
    pub seed: u64,
    pub status: StoredBranchStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_id: Option<CandidateId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_blob_id: Option<BlobId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_byte_len: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<SelectionDecision>,
    pub created_at_ms: i64,
}

/// Opaque stable position in the immutable branch index.
///
/// `sequence` is allocated monotonically in the same transaction as a run;
/// new runs therefore always sort before an already-issued descending cursor.
/// `run_id` binds the position to the occurrence that produced it and lets the
/// store reject forged or cross-document cursors.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BranchPageCursor {
    pub sequence: u64,
    pub run_id: GenerationRunId,
}

/// Bounded branch shelf metadata. Candidate bytes are deliberately absent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StoredBranchSummary {
    pub run_id: GenerationRunId,
    pub branch_id: BranchId,
    pub document_id: DocumentId,
    pub source_revision_id: RevisionId,
    pub target_range: ByteRange,
    pub cursor: BranchPageCursor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_identifier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    pub status: StoredBranchStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_id: Option<CandidateId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_blob_id: Option<BlobId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_byte_len: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub error_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<SelectionDecision>,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StoredBranchPage {
    pub branches: Vec<StoredBranchSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<BranchPageCursor>,
    pub has_more: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StoredBranchBody {
    pub run_id: GenerationRunId,
    pub output_blob_id: BlobId,
    pub byte_len: u64,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredBranchStatus {
    /// The process that owns this store has not yet recorded a terminal. On a
    /// fresh desktop session this is presented as interrupted, never as live.
    Interrupted,
    Completed,
    Cancelled,
    Failed,
    Pruned,
    Rejected,
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
        let payload = bounded_json("artifact", environment, MAX_PROVENANCE_JSON_BYTES)?;
        let expected_blob_id = BlobId::digest(&payload);

        if let Some(recorded) =
            find_recorded_model_environment(&self.connection, environment.environment_id)?
        {
            if recorded.blob_id != expected_blob_id || self.read_blob(recorded.blob_id)? != payload
            {
                return Err(StoreError::ModelEnvironmentContentConflict {
                    environment_id: environment.environment_id,
                });
            }
            return Ok(recorded);
        }

        let blob_id = self.put_blob(&payload)?;
        let artifact_id = ArtifactId::new();
        let operation_id = OperationId::new();
        let created_at_ms = now_unix_ms();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some(recorded) =
            find_recorded_model_environment(&transaction, environment.environment_id)?
        {
            if recorded.blob_id != expected_blob_id {
                return Err(StoreError::ModelEnvironmentContentConflict {
                    environment_id: environment.environment_id,
                });
            }
            transaction.commit()?;
            return Ok(recorded);
        }

        insert_blob_row(&transaction, blob_id, payload.len(), created_at_ms)?;
        insert_artifact(
            &transaction,
            artifact_id,
            blob_id,
            ArtifactKind::ModelEnvironment,
            "application/json",
            &json!({}),
            created_at_ms,
        )?;
        transaction.execute(
            "INSERT INTO operations(operation_id, operation_kind, metadata_json, created_at_ms)
             VALUES (?1, 'import', ?2, ?3)",
            params![
                operation_id.to_string(),
                serde_json::to_string(
                    &json!({"artifact_kind": ArtifactKind::ModelEnvironment.as_str()})
                )?,
                created_at_ms,
            ],
        )?;
        transaction.execute(
            "INSERT INTO operation_outputs(operation_id, position, artifact_id)
             VALUES (?1, 0, ?2)",
            params![operation_id.to_string(), artifact_id.to_string()],
        )?;
        transaction.execute(
            "INSERT INTO model_environments(artifact_id, environment_id, created_at_ms)
             VALUES (?1, ?2, ?3)",
            params![
                artifact_id.to_string(),
                environment.environment_id.to_string(),
                created_at_ms,
            ],
        )?;
        transaction.commit()?;
        Ok(RecordedArtifact {
            artifact_id,
            blob_id,
            operation_id,
        })
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

    /// Reconstructs an already-committed Weave command without creating any
    /// artifacts or requiring the caller to know its recorded fingerprint.
    /// This is the lost-reply preflight: callers can compare the immutable
    /// starts with their semantic request before deciding whether any new
    /// inference work is necessary.
    pub fn generation_family_for_command(
        &self,
        command_id: CommandId,
    ) -> Result<Option<GenerationFamilyStarted>> {
        let request: Option<(String, String)> = self
            .connection
            .query_row(
                "SELECT request_fingerprint, command_kind
                 FROM command_requests WHERE command_id = ?1",
                [command_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((request_fingerprint, command_kind)) = request else {
            if self.load_receipt(command_id)?.is_some() {
                return Err(StoreError::IdempotencyConflict { command_id });
            }
            return Ok(None);
        };
        if command_kind != CommandKind::Weave.as_str() {
            return Err(StoreError::IdempotencyConflict { command_id });
        }
        let request_fingerprint = parse_blob_id(&request_fingerprint)?;
        self.replay_generation_family(command_id, request_fingerprint)
    }

    pub fn start_generation_with_command(
        &mut self,
        command_id: CommandId,
        start: GenerationStart,
    ) -> Result<GenerationStarted> {
        let mut family = self.start_generation_family_with_command(command_id, vec![start])?;
        family.generations.pop().ok_or_else(|| {
            StoreError::CorruptDatabase("single-run family returned no generation".into())
        })
    }

    /// Atomically records one raw-generation branch family under one command
    /// receipt. Every run is validated before SQL insertion, and the immediate
    /// transaction contains all run artifacts, operations, branches, queued
    /// events, and the sole family receipt.
    #[allow(clippy::too_many_lines)]
    pub fn start_generation_family_with_command(
        &mut self,
        command_id: CommandId,
        starts: Vec<GenerationStart>,
    ) -> Result<GenerationFamilyStarted> {
        let request_fingerprint = generation_family_fingerprint(&starts)?;
        if let Some(replay) = self.replay_generation_family(command_id, request_fingerprint)? {
            return Ok(replay);
        }
        let started_at_ms = now_unix_ms();
        let first = starts.first().ok_or(StoreError::EmptyGenerationFamily)?;
        let family_document_id = first.document_id;
        let family_source_revision_id = first.source_revision_id;
        let mut run_ids = HashSet::with_capacity(starts.len());
        let mut branch_ids = HashSet::with_capacity(starts.len());
        for start in &starts {
            if start.document_id != family_document_id
                || start.source_revision_id != family_source_revision_id
            {
                return Err(StoreError::GenerationFamilySourceMismatch);
            }
            if !run_ids.insert(start.run_id) {
                return Err(StoreError::DuplicateGenerationRun(start.run_id));
            }
            if !branch_ids.insert(start.branch_id) {
                return Err(StoreError::DuplicateGenerationBranch(start.branch_id));
            }
        }

        let document = self
            .document_by_id(family_document_id)?
            .ok_or_else(|| StoreError::NoActiveRevision(family_document_id.to_string()))?;
        let active = self
            .active_revision(family_document_id)?
            .ok_or_else(|| StoreError::NoActiveRevision(document.relative_path.clone()))?;
        if active.revision_id != family_source_revision_id {
            return Err(StoreError::SourceRevisionMismatch {
                expected: family_source_revision_id,
                actual: active.revision_id,
            });
        }
        self.verify_visible_source(&document.relative_path, active.blob_id)?;
        let source_bytes = self.read_blob(active.blob_id)?;
        let mut prepared = Vec::with_capacity(starts.len());
        let mut indexed_model_identifiers = HashMap::<ArtifactId, Option<String>>::new();
        for start in starts {
            validate_utf8_range(&source_bytes, start.target_range)?;
            self.require_generation_references(&start)?;
            let context_source: String = self.connection.query_row(
                "SELECT source_revision_id FROM context_recipes WHERE artifact_id = ?1",
                [start.context_recipe_artifact_id.to_string()],
                |row| row.get(0),
            )?;
            let context_source = parse_id::<RevisionId>(&context_source, "source_revision_id")?;
            if context_source != start.source_revision_id {
                return Err(StoreError::SourceRevisionMismatch {
                    expected: start.source_revision_id,
                    actual: context_source,
                });
            }
            self.authority_role(
                start.authority_policy_artifact_id,
                start.model_environment_artifact_id,
            )?;

            let indexed_model_identifier = if let Some(identifier) =
                indexed_model_identifiers.get(&start.model_environment_artifact_id)
            {
                identifier.clone()
            } else {
                let environment: ModelEnvironment =
                    self.read_json_artifact(start.model_environment_artifact_id)?;
                let identifier = (environment.model_identifier.len()
                    <= MAX_INDEXED_MODEL_IDENTIFIER_BYTES)
                    .then_some(environment.model_identifier);
                indexed_model_identifiers
                    .insert(start.model_environment_artifact_id, identifier.clone());
                identifier
            };

            let run_payload = bounded_json("generation run", &start, MAX_PROVENANCE_JSON_BYTES)?;
            let run_blob_id = self.put_blob(&run_payload)?;
            prepared.push(PreparedGenerationStart {
                run_artifact_id: ArtifactId::new(),
                operation_id: OperationId::new(),
                queued_event: GenerationEvent {
                    event_id: GenerationEventId::new(),
                    run_id: start.run_id,
                    branch_id: start.branch_id,
                    sequence: 0,
                    kind: GenerationEventKind::Queued,
                    occurred_at_ms: started_at_ms,
                },
                generation: start,
                indexed_model_identifier,
                run_payload,
                run_blob_id,
            });
        }

        let receipt = CommandReceipt {
            command_id,
            command: CommandKind::Weave,
            project_id: self.manifest.project_id,
            project_schema_version: self.manifest.schema_version,
            source_revision_id: Some(family_source_revision_id),
            resulting_artifact_ids: prepared
                .iter()
                .map(|generation| generation.run_artifact_id)
                .collect(),
            resulting_operation_ids: prepared
                .iter()
                .map(|generation| generation.operation_id)
                .collect(),
            resulting_revision_ids: Vec::new(),
            started_at_ms,
            completed_at_ms: now_unix_ms(),
        };

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_active_in_transaction(&transaction, family_document_id, active)?;
        for generation in &prepared {
            insert_prepared_generation(&transaction, generation, active, started_at_ms)?;
        }
        persist_receipt_in(&transaction, &receipt)?;
        insert_command_request(
            &transaction,
            command_id,
            request_fingerprint,
            CommandKind::Weave,
            started_at_ms,
        )?;
        transaction.commit()?;
        Ok(GenerationFamilyStarted {
            generations: prepared
                .into_iter()
                .map(|generation| GenerationStarted {
                    run_artifact_id: generation.run_artifact_id,
                    operation_id: generation.operation_id,
                    generation: generation.generation,
                    queued_event: generation.queued_event,
                    receipt: receipt.clone(),
                })
                .collect(),
            receipt,
            request_fingerprint,
            replayed: false,
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
        let request_fingerprint = cancel_generation_fingerprint(command)?;
        if let Some(replay) =
            self.replay_cancel_generation(command_id, request_fingerprint, command)?
        {
            return Ok(replay);
        }
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
        insert_command_request(
            &transaction,
            command_id,
            request_fingerprint,
            CommandKind::CancelGeneration,
            started_at_ms,
        )?;
        transaction.execute(
            "INSERT INTO generation_command_events(command_id, event_id) VALUES (?1, ?2)",
            params![command_id.to_string(), event.event_id.to_string()],
        )?;
        transaction.commit()?;
        Ok(CancelGenerationOutcome {
            event,
            receipt,
            request_fingerprint,
            replayed: false,
        })
    }

    pub fn finish_generation(
        &mut self,
        run_id: GenerationRunId,
        status: GenerationTerminalStatus,
        error: Option<String>,
    ) -> Result<GenerationTerminalEvent> {
        validate_non_candidate_terminal(status, error.as_deref())?;
        self.run_identity(run_id)?;
        self.ensure_generation_not_terminal(run_id)?;
        let raw_event_stream_blob_id = self.store_provenance_blob(UNSUPPLIED_TERMINAL_EVIDENCE)?;
        self.finish_generation_with_evidence(
            run_id,
            TerminalGenerationInput {
                status,
                error,
                evidence: TerminalEvidenceInput {
                    partial_output_bytes: Vec::new(),
                    token_trace: TokenTrace {
                        generated_token_ids: Vec::new(),
                        observations: Vec::new(),
                        raw_event_stream_blob_id,
                        provenance: None,
                    },
                },
            },
        )
        .map(|outcome| outcome.terminal_event)
    }

    /// Atomically records a non-promotable terminal and the exact evidence
    /// supplied by the backend. The partial bytes and token trace remain
    /// immutable and queryable for every terminal status.
    #[allow(clippy::too_many_lines)]
    pub fn finish_generation_with_evidence(
        &mut self,
        run_id: GenerationRunId,
        input: TerminalGenerationInput,
    ) -> Result<TerminalGenerationOutcome> {
        let TerminalGenerationInput {
            status,
            error,
            evidence:
                TerminalEvidenceInput {
                    partial_output_bytes,
                    token_trace,
                },
        } = input;
        validate_non_candidate_terminal(status, error.as_deref())?;
        ensure_payload_size(
            "partial generated output",
            partial_output_bytes.len(),
            max_document_bytes_usize(),
        )?;
        std::str::from_utf8(&partial_output_bytes).map_err(|_| {
            StoreError::CorruptDatabase("partial generated output is not valid UTF-8".into())
        })?;
        let run = self.run_identity(run_id)?;
        self.ensure_generation_not_terminal(run_id)?;
        self.require_token_trace_blobs(&token_trace)?;

        let output_blob_id = self.put_blob(&partial_output_bytes)?;
        let trace_payload = bounded_json("token trace", &token_trace, MAX_PROVENANCE_JSON_BYTES)?;
        let trace_blob_id = self.put_blob(&trace_payload)?;
        let output_artifact_id = ArtifactId::new();
        let token_trace_artifact_id = ArtifactId::new();
        let operation_id = OperationId::new();
        let created_at_ms = now_unix_ms();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_generation_open(&transaction, run_id)?;
        insert_blob_row(
            &transaction,
            output_blob_id,
            partial_output_bytes.len(),
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
            &json!({"run_id": run_id, "terminal_status": status}),
            created_at_ms,
        )?;
        insert_artifact(
            &transaction,
            output_artifact_id,
            output_blob_id,
            ArtifactKind::TextBlob,
            "text/plain; charset=utf-8",
            &json!({
                "run_id": run_id,
                "terminal_status": status,
                "promotable": false,
            }),
            created_at_ms,
        )?;
        transaction.execute(
            "INSERT INTO operations(operation_id, operation_kind, metadata_json, created_at_ms)
             VALUES (?1, 'generate', ?2, ?3)",
            params![
                operation_id.to_string(),
                serde_json::to_string(&json!({
                    "run_id": run_id,
                    "terminal_status": status,
                    "promotable": false,
                }))?,
                created_at_ms,
            ],
        )?;
        transaction.execute(
            "INSERT INTO operation_inputs(operation_id, position, artifact_id) VALUES (?1, 0, ?2)",
            params![operation_id.to_string(), run.run_artifact_id.to_string()],
        )?;
        for (position, artifact_id) in [token_trace_artifact_id, output_artifact_id]
            .into_iter()
            .enumerate()
        {
            transaction.execute(
                "INSERT INTO operation_outputs(operation_id, position, artifact_id) VALUES (?1, ?2, ?3)",
                params![
                    operation_id.to_string(),
                    i64::try_from(position).map_err(|_| StoreError::CorruptDatabase(
                        "terminal evidence output position overflow".into()
                    ))?,
                    artifact_id.to_string(),
                ],
            )?;
        }
        let event = GenerationTerminalEvent {
            event_id: GenerationEventId::new(),
            run_id,
            branch_id: run.branch_id,
            sequence: next_sequence(&transaction, run_id)?,
            status,
            candidate_id: None,
            error,
            occurred_at_ms: created_at_ms,
        };
        insert_terminal_event(&transaction, &event)?;
        let evidence = GenerationTerminalEvidence {
            run_id,
            status,
            candidate_id: None,
            operation_id,
            output_artifact_id,
            output_blob_id,
            token_trace_artifact_id,
        };
        insert_terminal_evidence(&transaction, &evidence, created_at_ms)?;
        transaction.commit()?;
        Ok(TerminalGenerationOutcome {
            terminal_event: event,
            evidence,
        })
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
        self.require_token_trace_blobs(&token_trace)?;

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
        let evidence = GenerationTerminalEvidence {
            run_id,
            status: GenerationTerminalStatus::Completed,
            candidate_id: Some(candidate_id),
            operation_id,
            output_artifact_id: generated_span_artifact_id,
            output_blob_id,
            token_trace_artifact_id,
        };
        insert_terminal_evidence(&transaction, &evidence, created_at_ms)?;
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
            evidence,
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
        self.promote_candidate_with_command_inner(command_id, command, |_| Ok(()))
    }

    #[cfg(test)]
    fn promote_candidate_with_command_and_boundary<F>(
        &mut self,
        command_id: CommandId,
        command: PromoteCandidateCommand,
        before_projection_boundary: F,
    ) -> Result<PromotionOutcome>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        self.promote_candidate_with_command_inner(command_id, command, before_projection_boundary)
    }

    #[allow(clippy::too_many_lines)]
    fn promote_candidate_with_command_inner<F>(
        &mut self,
        command_id: CommandId,
        command: PromoteCandidateCommand,
        before_projection_boundary: F,
    ) -> Result<PromotionOutcome>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        let request_fingerprint = promote_candidate_fingerprint(command)?;
        if let Some(replay) =
            self.replay_promote_candidate(command_id, request_fingerprint, command)?
        {
            return Ok(replay);
        }
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
                &candidate.relative_path,
                target_blob_id.to_string(),
                candidate.source_blob_id.to_string(),
                created_at_ms,
            ],
        )?;
        let outbox_id = transaction.last_insert_rowid();
        persist_receipt_in(&transaction, &receipt)?;
        insert_command_request(
            &transaction,
            command_id,
            request_fingerprint,
            CommandKind::PromoteCandidate,
            started_at_ms,
        )?;
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

        let visible_projection = self.settle_outbox_entry_with_boundary(
            outbox_id,
            &candidate.relative_path,
            before_projection_boundary,
        );
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
            visible_projection,
            request_fingerprint,
            replayed: false,
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
        let request_fingerprint = keep_alternative_fingerprint(command)?;
        if let Some(replay) =
            self.replay_keep_alternative(command_id, request_fingerprint, command)?
        {
            return Ok(replay);
        }
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
        insert_command_request(
            &transaction,
            command_id,
            request_fingerprint,
            CommandKind::KeepAlternative,
            started_at_ms,
        )?;
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
            request_fingerprint,
            replayed: false,
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

    /// Loads the immutable output and token evidence attached to a terminal.
    /// `None` is possible only for databases created before store schema 5.
    #[allow(clippy::too_many_lines)]
    pub fn generation_terminal_evidence(
        &self,
        run_id: GenerationRunId,
    ) -> Result<Option<StoredGenerationTerminalEvidence>> {
        type Row = (
            String,
            Option<String>,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            String,
            String,
        );
        let row: Option<Row> = self
            .connection
            .query_row(
                "SELECT gt.status, gt.candidate_id,
                        gte.operation_id, gte.output_artifact_id, gte.output_blob_id,
                        gte.token_trace_artifact_id, oa.artifact_kind, gte.candidate_id,
                        ta.artifact_kind, ta.blob_id
                 FROM generation_terminal_evidence gte
                 JOIN generation_terminals gt ON gt.run_id = gte.run_id
                 JOIN artifacts oa ON oa.artifact_id = gte.output_artifact_id
                 JOIN artifacts ta ON ta.artifact_id = gte.token_trace_artifact_id
                 WHERE gte.run_id = ?1",
                [run_id.to_string()],
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
                    ))
                },
            )
            .optional()?;
        let Some(row) = row else {
            return Ok(None);
        };
        let status = parse_terminal_status(&row.0)?;
        let terminal_candidate_id = row
            .1
            .as_deref()
            .map(|value| parse_id(value, "terminal candidate_id"))
            .transpose()?;
        let evidence_candidate_id = row
            .7
            .as_deref()
            .map(|value| parse_id(value, "evidence candidate_id"))
            .transpose()?;
        if terminal_candidate_id != evidence_candidate_id {
            return Err(StoreError::CorruptDatabase(
                "terminal and evidence candidate identities disagree".into(),
            ));
        }
        let expected_output_kind = if evidence_candidate_id.is_some() {
            ArtifactKind::GeneratedSpan
        } else {
            ArtifactKind::TextBlob
        };
        if row.6 != expected_output_kind.as_str() || row.8 != ArtifactKind::TokenTrace.as_str() {
            return Err(StoreError::CorruptDatabase(
                "terminal evidence artifact kind mismatch".into(),
            ));
        }
        let output_blob_id = parse_blob_id(&row.4)?;
        let output_artifact_id: ArtifactId = parse_id(&row.3, "output_artifact_id")?;
        let artifact_output_blob: String = self.connection.query_row(
            "SELECT blob_id FROM artifacts WHERE artifact_id = ?1",
            [output_artifact_id.to_string()],
            |row| row.get(0),
        )?;
        if parse_blob_id(&artifact_output_blob)? != output_blob_id {
            return Err(StoreError::CorruptDatabase(
                "terminal evidence output artifact points to a different blob".into(),
            ));
        }
        let trace_blob_id = parse_blob_id(&row.9)?;
        let token_trace: TokenTrace = serde_json::from_slice(&self.read_blob(trace_blob_id)?)?;
        self.require_token_trace_blobs(&token_trace)?;
        Ok(Some(StoredGenerationTerminalEvidence {
            evidence: GenerationTerminalEvidence {
                run_id,
                status,
                candidate_id: evidence_candidate_id,
                operation_id: parse_id(&row.2, "terminal evidence operation_id")?,
                output_artifact_id,
                output_blob_id,
                token_trace_artifact_id: parse_id(&row.5, "token_trace_artifact_id")?,
            },
            output_bytes: self.read_blob(output_blob_id)?,
            token_trace,
        }))
    }

    /// Marks every durably open generation run as failed after its owning
    /// process has been lost.
    ///
    /// This is an explicit startup recovery boundary: a live coordinator must
    /// never call it while it still owns native generation handles. The
    /// immediate transaction discovers and terminalizes all open runs
    /// together. Repeating the call is a no-op because terminal rows are
    /// immutable and uniquely keyed by run.
    #[allow(clippy::too_many_lines)]
    pub fn recover_interrupted_generations(&mut self) -> Result<Vec<GenerationTerminalEvent>> {
        let open_count: i64 = self.connection.query_row(
            "SELECT COUNT(*)
             FROM generation_runs gr
             LEFT JOIN generation_terminals gt ON gt.run_id = gr.run_id
             WHERE gt.run_id IS NULL",
            [],
            |row| row.get(0),
        )?;
        if open_count == 0 {
            return Ok(Vec::new());
        }
        let raw_event_stream_blob_id = self.put_blob(UNSUPPLIED_TERMINAL_EVIDENCE)?;
        let token_trace = TokenTrace {
            generated_token_ids: Vec::new(),
            observations: Vec::new(),
            raw_event_stream_blob_id,
            provenance: None,
        };
        let trace_payload = bounded_json("token trace", &token_trace, MAX_PROVENANCE_JSON_BYTES)?;
        let trace_blob_id = self.put_blob(&trace_payload)?;
        let output_blob_id = self.put_blob(&[])?;
        let evidence_created_at_ms = now_unix_ms();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_blob_row(
            &transaction,
            raw_event_stream_blob_id,
            UNSUPPLIED_TERMINAL_EVIDENCE.len(),
            evidence_created_at_ms,
        )?;
        insert_blob_row(&transaction, output_blob_id, 0, evidence_created_at_ms)?;
        insert_blob_row(
            &transaction,
            trace_blob_id,
            trace_payload.len(),
            evidence_created_at_ms,
        )?;
        let open_runs = {
            let mut statement = transaction.prepare(
                "SELECT gr.run_id, gr.branch_id, gr.run_artifact_id
                 FROM generation_runs gr
                 LEFT JOIN generation_terminals gt ON gt.run_id = gr.run_id
                 WHERE gt.run_id IS NULL
                 ORDER BY gr.created_at_ms, gr.run_id",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut recovered = Vec::with_capacity(open_runs.len());
        for (run_id, branch_id, run_artifact_id) in open_runs {
            let run_id = parse_id(&run_id, "run_id")?;
            let branch_id = parse_id(&branch_id, "branch_id")?;
            let run_artifact_id = parse_id(&run_artifact_id, "run_artifact_id")?;
            let created_at_ms = now_unix_ms();
            let output_artifact_id = ArtifactId::new();
            let token_trace_artifact_id = ArtifactId::new();
            let operation_id = OperationId::new();
            insert_artifact(
                &transaction,
                token_trace_artifact_id,
                trace_blob_id,
                ArtifactKind::TokenTrace,
                "application/json",
                &json!({
                    "run_id": run_id,
                    "terminal_status": GenerationTerminalStatus::Failed,
                    "recovered": true,
                }),
                created_at_ms,
            )?;
            insert_artifact(
                &transaction,
                output_artifact_id,
                output_blob_id,
                ArtifactKind::TextBlob,
                "text/plain; charset=utf-8",
                &json!({
                    "run_id": run_id,
                    "terminal_status": GenerationTerminalStatus::Failed,
                    "promotable": false,
                    "recovered": true,
                }),
                created_at_ms,
            )?;
            insert_terminal_evidence_operation(
                &transaction,
                operation_id,
                run_id,
                run_artifact_id,
                GenerationTerminalStatus::Failed,
                token_trace_artifact_id,
                output_artifact_id,
                created_at_ms,
            )?;
            let event = GenerationTerminalEvent {
                event_id: GenerationEventId::new(),
                run_id,
                branch_id,
                sequence: next_sequence(&transaction, run_id)?,
                status: GenerationTerminalStatus::Failed,
                candidate_id: None,
                error: Some(INTERRUPTED_GENERATION_ERROR.to_owned()),
                occurred_at_ms: created_at_ms,
            };
            insert_terminal_event(&transaction, &event)?;
            insert_terminal_evidence(
                &transaction,
                &GenerationTerminalEvidence {
                    run_id,
                    status: GenerationTerminalStatus::Failed,
                    candidate_id: None,
                    operation_id,
                    output_artifact_id,
                    output_blob_id,
                    token_trace_artifact_id,
                },
                created_at_ms,
            )?;
            recovered.push(event);
        }
        transaction.commit()?;
        Ok(recovered)
    }

    /// Returns one descending page of branch metadata without reading any
    /// provenance or candidate blob. The page size has a hard process-wide
    /// ceiling, and a cursor is bound to both this document and its indexed
    /// run occurrence before it is accepted.
    pub fn branch_page(
        &self,
        document_id: DocumentId,
        after: Option<BranchPageCursor>,
        limit: usize,
    ) -> Result<StoredBranchPage> {
        if limit == 0 || limit > MAX_BRANCH_PAGE_SIZE {
            return Err(StoreError::InvalidBranchPageLimit {
                requested: limit,
                max: MAX_BRANCH_PAGE_SIZE,
            });
        }
        if let Some(cursor) = after {
            let sequence =
                i64::try_from(cursor.sequence).map_err(|_| StoreError::InvalidBranchPageCursor)?;
            let valid = self
                .connection
                .query_row(
                    "SELECT 1
                     FROM generation_run_index gri
                     JOIN generation_runs gr ON gr.run_id = gri.run_id
                     WHERE gri.sequence = ?1 AND gri.run_id = ?2 AND gr.document_id = ?3",
                    params![sequence, cursor.run_id.to_string(), document_id.to_string(),],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .is_some();
            if !valid {
                return Err(StoreError::InvalidBranchPageCursor);
            }
        }

        let fetch_limit = i64::try_from(limit.saturating_add(1)).map_err(|_| {
            StoreError::InvalidBranchPageLimit {
                requested: limit,
                max: MAX_BRANCH_PAGE_SIZE,
            }
        })?;
        let rows = if let Some(cursor) = after {
            let sql = format!(
                "{BRANCH_SUMMARY_SELECT}
                 WHERE gr.document_id = ?1 AND gri.sequence < ?2
                 ORDER BY gri.sequence DESC
                 LIMIT ?3"
            );
            query_branch_summary_rows(
                &self.connection,
                &sql,
                params![
                    document_id.to_string(),
                    i64::try_from(cursor.sequence)
                        .map_err(|_| StoreError::InvalidBranchPageCursor)?,
                    fetch_limit,
                ],
            )?
        } else {
            let sql = format!(
                "{BRANCH_SUMMARY_SELECT}
                 WHERE gr.document_id = ?1
                 ORDER BY gri.sequence DESC
                 LIMIT ?2"
            );
            query_branch_summary_rows(
                &self.connection,
                &sql,
                params![document_id.to_string(), fetch_limit],
            )?
        };
        let mut branches = rows
            .into_iter()
            .map(|row| parse_branch_summary_row(row, document_id))
            .collect::<Result<Vec<_>>>()?;
        let has_more = branches.len() > limit;
        if has_more {
            branches.pop();
        }
        let next_cursor = has_more
            .then(|| branches.last().map(|branch| branch.cursor))
            .flatten();
        Ok(StoredBranchPage {
            branches,
            next_cursor,
            has_more,
        })
    }

    /// Looks up one branch's bounded metadata without scanning its document or
    /// loading any blob. This is used for command recovery and exact UI state
    /// checks where pagination would be the wrong primitive.
    pub fn branch_summary(
        &self,
        document_id: DocumentId,
        run_id: GenerationRunId,
    ) -> Result<Option<StoredBranchSummary>> {
        let sql = format!(
            "{BRANCH_SUMMARY_SELECT}
             WHERE gr.document_id = ?1 AND gr.run_id = ?2"
        );
        let mut rows = query_branch_summary_rows(
            &self.connection,
            &sql,
            params![document_id.to_string(), run_id.to_string()],
        )?;
        match rows.len() {
            0 => Ok(None),
            1 => parse_branch_summary_row(rows.pop().expect("one row"), document_id).map(Some),
            _ => Err(StoreError::CorruptDatabase(
                "one generation run produced multiple branch index rows".into(),
            )),
        }
    }

    /// Loads one canonical candidate output under both a caller budget and
    /// Loom's hard ceiling. Arbitrary terminal partial evidence remains in the
    /// provenance API because cancellation may end between UTF-8 boundaries.
    /// The database length is checked before filesystem access; the bounded
    /// reader checks the file again while reading so replacement or growth
    /// cannot inflate the allocation.
    pub fn branch_body(
        &self,
        document_id: DocumentId,
        run_id: GenerationRunId,
        max_bytes: u64,
    ) -> Result<Option<StoredBranchBody>> {
        if max_bytes == 0 || max_bytes > MAX_BRANCH_BODY_BYTES {
            return Err(StoreError::InvalidBranchBodyLimit {
                requested: max_bytes,
                max: MAX_BRANCH_BODY_BYTES,
            });
        }
        let row: Option<(Option<String>, Option<i64>)> = self
            .connection
            .query_row(
                "SELECT gc.output_blob_id, output_blob.byte_len
                 FROM generation_runs gr
                 LEFT JOIN generation_candidates gc ON gc.run_id = gr.run_id
                 LEFT JOIN blobs output_blob
                   ON output_blob.blob_id = gc.output_blob_id
                 WHERE gr.document_id = ?1 AND gr.run_id = ?2",
                params![document_id.to_string(), run_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((output_blob_id, indexed_byte_len)) = row else {
            return Err(StoreError::GenerationRunNotFound(run_id));
        };
        let Some(output_blob_id) = output_blob_id else {
            return Ok(None);
        };
        let output_blob_id = parse_blob_id(&output_blob_id)?;
        let indexed_byte_len = indexed_byte_len.ok_or_else(|| {
            StoreError::CorruptDatabase(
                "generation output references a blob without indexed length".into(),
            )
        })?;
        let indexed_byte_len = u64::try_from(indexed_byte_len).map_err(|_| {
            StoreError::CorruptDatabase("generation output has a negative byte length".into())
        })?;
        if indexed_byte_len > max_bytes {
            return Err(StoreError::BranchBodyTooLarge {
                run_id,
                actual_bytes: indexed_byte_len,
                max_bytes,
            });
        }
        let bytes = match self.read_blob_bounded(output_blob_id, max_bytes) {
            Err(StoreError::DocumentTooLarge { actual_bytes, .. }) => {
                return Err(StoreError::BranchBodyTooLarge {
                    run_id,
                    actual_bytes,
                    max_bytes,
                });
            }
            result => result?,
        };
        let actual_byte_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if actual_byte_len != indexed_byte_len {
            return Err(StoreError::CorruptDatabase(format!(
                "generation output blob length is {actual_byte_len}, indexed as {indexed_byte_len}"
            )));
        }
        let text = String::from_utf8(bytes).map_err(|_| {
            StoreError::CorruptDatabase("generated output blob is not valid UTF-8".into())
        })?;
        Ok(Some(StoredBranchBody {
            run_id,
            output_blob_id,
            byte_len: actual_byte_len,
            text,
        }))
    }

    /// Reconstructs one complete branch record for bounded command recovery.
    /// Unlike the old document-wide listing, this can allocate at most one
    /// capped body plus two individually bounded JSON artifacts.
    pub fn branch_record(
        &self,
        document_id: DocumentId,
        run_id: GenerationRunId,
        max_body_bytes: u64,
    ) -> Result<Option<StoredBranchRecord>> {
        let Some(summary) = self.branch_summary(document_id, run_id)? else {
            return Ok(None);
        };
        let identity: (String, String) = self.connection.query_row(
            "SELECT run_artifact_id, model_environment_artifact_id
             FROM generation_runs WHERE run_id = ?1 AND document_id = ?2",
            params![run_id.to_string(), document_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let start: GenerationStart =
            self.read_json_artifact(parse_id(&identity.0, "run_artifact_id")?)?;
        let environment: ModelEnvironment =
            self.read_json_artifact(parse_id(&identity.1, "model_environment_artifact_id")?)?;
        if start.run_id != summary.run_id
            || start.branch_id != summary.branch_id
            || start.document_id != summary.document_id
            || start.source_revision_id != summary.source_revision_id
            || start.target_range != summary.target_range
        {
            return Err(StoreError::CorruptDatabase(
                "generation run artifact disagrees with indexed branch identity".into(),
            ));
        }
        let body = self.branch_body(document_id, run_id, max_body_bytes)?;
        if body.as_ref().map(|body| body.output_blob_id) != summary.output_blob_id
            || body.as_ref().map(|body| body.byte_len) != summary.output_byte_len
        {
            return Err(StoreError::CorruptDatabase(
                "generation body disagrees with branch metadata".into(),
            ));
        }
        let (output_text, output_blob_id, output_byte_len) = match body {
            Some(body) => (
                Some(body.text),
                Some(body.output_blob_id),
                Some(body.byte_len),
            ),
            None => (None, None, None),
        };
        Ok(Some(StoredBranchRecord {
            run_id: summary.run_id,
            branch_id: summary.branch_id,
            document_id: summary.document_id,
            source_revision_id: summary.source_revision_id,
            target_range: summary.target_range,
            model_identifier: environment.model_identifier,
            seed: start.seed,
            status: summary.status,
            candidate_id: summary.candidate_id,
            output_text,
            output_blob_id,
            output_byte_len,
            error: summary.error,
            selection: summary.selection,
            created_at_ms: summary.created_at_ms,
        }))
    }

    fn replay_generation_family(
        &self,
        command_id: CommandId,
        request_fingerprint: BlobId,
    ) -> Result<Option<GenerationFamilyStarted>> {
        let Some(receipt) =
            self.replay_command_receipt(command_id, request_fingerprint, CommandKind::Weave)?
        else {
            return Ok(None);
        };
        if receipt.resulting_artifact_ids.is_empty()
            || receipt.resulting_artifact_ids.len() != receipt.resulting_operation_ids.len()
        {
            return Err(StoreError::CorruptDatabase(
                "weave receipt has inconsistent run identities".into(),
            ));
        }

        let mut generations = Vec::with_capacity(receipt.resulting_artifact_ids.len());
        for (&run_artifact_id, &operation_id) in receipt
            .resulting_artifact_ids
            .iter()
            .zip(&receipt.resulting_operation_ids)
        {
            let generation: GenerationStart = self.read_json_artifact(run_artifact_id)?;
            let row: (String, String, String, String, i64, i64, String) =
                self.connection.query_row(
                    "SELECT gr.run_id, gr.branch_id, ge.event_id, ge.payload_json,
                            ge.is_terminal, ge.created_at_ms, oo.operation_id
                     FROM generation_runs gr
                     JOIN generation_events ge ON ge.run_id = gr.run_id AND ge.sequence = 0
                     JOIN operation_outputs oo ON oo.artifact_id = gr.run_artifact_id
                     WHERE gr.run_artifact_id = ?1",
                    [run_artifact_id.to_string()],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                        ))
                    },
                )?;
            let run_id = parse_id(&row.0, "replayed run_id")?;
            let branch_id = parse_id(&row.1, "replayed branch_id")?;
            let kind: GenerationEventKind = serde_json::from_str(&row.3)?;
            if generation.run_id != run_id
                || generation.branch_id != branch_id
                || !matches!(kind, GenerationEventKind::Queued)
                || row.4 != 0
                || parse_id::<OperationId>(&row.6, "replayed operation_id")? != operation_id
            {
                return Err(StoreError::CorruptDatabase(
                    "weave replay identities disagree with immutable run records".into(),
                ));
            }
            generations.push(GenerationStarted {
                run_artifact_id,
                operation_id,
                generation,
                queued_event: GenerationEvent {
                    event_id: parse_id(&row.2, "queued event_id")?,
                    run_id,
                    branch_id,
                    sequence: 0,
                    kind,
                    occurred_at_ms: row.5,
                },
                receipt: receipt.clone(),
            });
        }
        let starts: Vec<_> = generations
            .iter()
            .map(|generation| generation.generation.clone())
            .collect();
        if generation_family_fingerprint(&starts)? != request_fingerprint {
            return Err(StoreError::CorruptDatabase(
                "weave run artifacts do not match the recorded request fingerprint".into(),
            ));
        }
        Ok(Some(GenerationFamilyStarted {
            generations,
            receipt,
            request_fingerprint,
            replayed: true,
        }))
    }

    fn replay_cancel_generation(
        &self,
        command_id: CommandId,
        request_fingerprint: BlobId,
        command: CancelGenerationCommand,
    ) -> Result<Option<CancelGenerationOutcome>> {
        let Some(receipt) = self.replay_command_receipt(
            command_id,
            request_fingerprint,
            CommandKind::CancelGeneration,
        )?
        else {
            return Ok(None);
        };
        let row: (String, String, String, i64, String, i64, i64) = self.connection.query_row(
            "SELECT ge.event_id, ge.run_id, gr.branch_id, ge.sequence,
                    ge.payload_json, ge.is_terminal, ge.created_at_ms
             FROM generation_command_events gce
             JOIN generation_events ge ON ge.event_id = gce.event_id
             JOIN generation_runs gr ON gr.run_id = ge.run_id
             WHERE gce.command_id = ?1",
            [command_id.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )?;
        let run_id = parse_id(&row.1, "cancel run_id")?;
        let kind: GenerationEventKind = serde_json::from_str(&row.4)?;
        if run_id != command.run_id
            || !matches!(kind, GenerationEventKind::CancellationRequested)
            || row.5 != 0
        {
            return Err(StoreError::CorruptDatabase(
                "cancel replay mapping points to the wrong generation event".into(),
            ));
        }
        Ok(Some(CancelGenerationOutcome {
            event: GenerationEvent {
                event_id: parse_id(&row.0, "cancel event_id")?,
                run_id,
                branch_id: parse_id(&row.2, "cancel branch_id")?,
                sequence: u64::try_from(row.3).map_err(|_| {
                    StoreError::CorruptDatabase("negative cancel event sequence".into())
                })?,
                kind,
                occurred_at_ms: row.6,
            },
            receipt,
            request_fingerprint,
            replayed: true,
        }))
    }

    fn replay_keep_alternative(
        &self,
        command_id: CommandId,
        request_fingerprint: BlobId,
        command: KeepAlternativeCommand,
    ) -> Result<Option<KeepAlternativeOutcome>> {
        let Some(receipt) = self.replay_command_receipt(
            command_id,
            request_fingerprint,
            CommandKind::KeepAlternative,
        )?
        else {
            return Ok(None);
        };
        let row: (String, String, String) = self.connection.query_row(
            "SELECT selection_artifact_id, candidate_id, decision
             FROM selection_events WHERE command_id = ?1",
            [command_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let candidate_id = parse_id(&row.1, "kept candidate_id")?;
        if candidate_id != command.candidate_id
            || row.2 != SelectionDecision::KeepAlternative.as_str()
        {
            return Err(StoreError::CorruptDatabase(
                "keep-alternative replay points to the wrong selection".into(),
            ));
        }
        let selection_artifact_id: ArtifactId = parse_id(&row.0, "selection_artifact_id")?;
        let operation_id = only_id(&receipt.resulting_operation_ids, "keep operation")?;
        let recorded_operation: String = self.connection.query_row(
            "SELECT operation_id FROM operation_outputs WHERE artifact_id = ?1",
            [selection_artifact_id.to_string()],
            |row| row.get(0),
        )?;
        if parse_id::<OperationId>(&recorded_operation, "keep operation_id")? != operation_id {
            return Err(StoreError::CorruptDatabase(
                "keep-alternative receipt operation does not produce its selection".into(),
            ));
        }
        Ok(Some(KeepAlternativeOutcome {
            candidate_id,
            selection_artifact_id,
            operation_id,
            receipt,
            request_fingerprint,
            replayed: true,
        }))
    }

    #[allow(clippy::too_many_lines)]
    fn replay_promote_candidate(
        &mut self,
        command_id: CommandId,
        request_fingerprint: BlobId,
        command: PromoteCandidateCommand,
    ) -> Result<Option<PromotionOutcome>> {
        let Some(receipt) = self.replay_command_receipt(
            command_id,
            request_fingerprint,
            CommandKind::PromoteCandidate,
        )?
        else {
            return Ok(None);
        };
        let revision_id = only_id(&receipt.resulting_revision_ids, "promoted revision")?;
        let operation_id = only_id(&receipt.resulting_operation_ids, "promotion operation")?;
        let row: (String, String, String, String, String, i64, String) =
            self.connection.query_row(
                "SELECT se.selection_artifact_id, se.candidate_id, se.decision,
                    aa.attestation_artifact_id, r.artifact_id,
                    vo.outbox_id, vo.relative_path
             FROM selection_events se
             JOIN authorship_attestations aa ON aa.promotion_command_id = se.command_id
             JOIN revisions r ON r.revision_id = se.resulting_revision_id
             JOIN visible_file_outbox vo ON vo.revision_id = r.revision_id
             WHERE se.command_id = ?1 AND se.resulting_revision_id = ?2",
                params![command_id.to_string(), revision_id.to_string()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )?;
        let candidate_id = parse_id(&row.1, "promoted candidate_id")?;
        if candidate_id != command.candidate_id || row.2 != SelectionDecision::Promote.as_str() {
            return Err(StoreError::CorruptDatabase(
                "promotion replay points to the wrong selection".into(),
            ));
        }
        let candidate = self.candidate_context(candidate_id)?;
        if candidate.source_revision_id != command.expected_source_revision_id
            || candidate.source_blob_id != command.expected_visible_blob_id
        {
            return Err(StoreError::CorruptDatabase(
                "promotion replay candidate disagrees with the recorded source claim".into(),
            ));
        }
        let artifact_id: ArtifactId = parse_id(&row.4, "promoted revision artifact_id")?;
        let blob_id: String = self.connection.query_row(
            "SELECT blob_id FROM artifacts WHERE artifact_id = ?1",
            [artifact_id.to_string()],
            |row| row.get(0),
        )?;
        let selection_artifact_id = parse_id(&row.0, "promotion selection_artifact_id")?;
        let attestation_artifact_id = parse_id(&row.3, "attestation_artifact_id")?;
        let expected_artifacts = [artifact_id, selection_artifact_id, attestation_artifact_id];
        if receipt.resulting_artifact_ids != expected_artifacts {
            return Err(StoreError::CorruptDatabase(
                "promotion receipt artifact identities disagree with committed rows".into(),
            ));
        }
        let visible_projection = self.settle_outbox_entry(row.5, &row.6);
        Ok(Some(PromotionOutcome {
            save: SaveOutcome {
                blob_id: parse_blob_id(&blob_id)?,
                artifact_id,
                operation_id,
                revision_id,
                receipt,
            },
            candidate_id,
            selection_artifact_id,
            attestation_artifact_id,
            visible_projection,
            request_fingerprint,
            replayed: true,
        }))
    }

    fn replay_command_receipt(
        &self,
        command_id: CommandId,
        request_fingerprint: BlobId,
        command_kind: CommandKind,
    ) -> Result<Option<CommandReceipt>> {
        let request: Option<(String, String)> = self
            .connection
            .query_row(
                "SELECT request_fingerprint, command_kind
                 FROM command_requests WHERE command_id = ?1",
                [command_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((recorded_fingerprint, recorded_kind)) = request else {
            if self.load_receipt(command_id)?.is_some() {
                return Err(StoreError::IdempotencyConflict { command_id });
            }
            return Ok(None);
        };
        if parse_blob_id(&recorded_fingerprint)? != request_fingerprint
            || recorded_kind != command_kind.as_str()
        {
            return Err(StoreError::IdempotencyConflict { command_id });
        }
        let receipt = self.load_receipt(command_id)?.ok_or_else(|| {
            StoreError::CorruptDatabase(format!(
                "command request {command_id} has no durable receipt"
            ))
        })?;
        if receipt.command != command_kind
            || receipt.command_id != command_id
            || receipt.project_id != self.manifest.project_id
        {
            return Err(StoreError::CorruptDatabase(format!(
                "command receipt {command_id} disagrees with its request"
            )));
        }
        Ok(Some(receipt))
    }

    fn require_token_trace_blobs(&self, token_trace: &TokenTrace) -> Result<()> {
        self.require_blob(token_trace.raw_event_stream_blob_id)?;
        if let Some(provenance) = &token_trace.provenance {
            if let Some(blob_id) = provenance.backend_receipt_blob_id {
                self.require_blob(blob_id)?;
            }
            if let Some(blob_id) = provenance.sequence_state_blob_id {
                self.require_blob(blob_id)?;
            }
        }
        Ok(())
    }

    fn read_json_artifact<T: DeserializeOwned>(&self, artifact_id: ArtifactId) -> Result<T> {
        let blob_id: String = self.connection.query_row(
            "SELECT blob_id FROM artifacts WHERE artifact_id = ?1",
            [artifact_id.to_string()],
            |row| row.get(0),
        )?;
        let payload = self.read_blob(parse_blob_id(&blob_id)?)?;
        Ok(serde_json::from_slice(&payload)?)
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

fn parse_stored_branch_status(value: Option<&str>) -> Result<StoredBranchStatus> {
    match value {
        None => Ok(StoredBranchStatus::Interrupted),
        Some("completed") => Ok(StoredBranchStatus::Completed),
        Some("cancelled") => Ok(StoredBranchStatus::Cancelled),
        Some("failed") => Ok(StoredBranchStatus::Failed),
        Some("pruned") => Ok(StoredBranchStatus::Pruned),
        Some("rejected") => Ok(StoredBranchStatus::Rejected),
        Some(value) => Err(StoreError::CorruptDatabase(format!(
            "invalid generation terminal status `{value}`"
        ))),
    }
}

fn parse_terminal_status(value: &str) -> Result<GenerationTerminalStatus> {
    match value {
        "cancelled" => Ok(GenerationTerminalStatus::Cancelled),
        "completed" => Ok(GenerationTerminalStatus::Completed),
        "failed" => Ok(GenerationTerminalStatus::Failed),
        "pruned" => Ok(GenerationTerminalStatus::Pruned),
        "rejected" => Ok(GenerationTerminalStatus::Rejected),
        value => Err(StoreError::CorruptDatabase(format!(
            "invalid generation terminal status `{value}`"
        ))),
    }
}

fn parse_selection_decision(value: &str) -> Result<SelectionDecision> {
    match value {
        "keep_alternative" => Ok(SelectionDecision::KeepAlternative),
        "promote" => Ok(SelectionDecision::Promote),
        "reject" => Ok(SelectionDecision::Reject),
        value => Err(StoreError::CorruptDatabase(format!(
            "invalid candidate selection decision `{value}`"
        ))),
    }
}

fn find_recorded_model_environment(
    connection: &Connection,
    environment_id: ModelEnvironmentId,
) -> Result<Option<RecordedArtifact>> {
    let row = connection
        .query_row(
            "SELECT me.artifact_id, a.blob_id, a.artifact_kind, oo.operation_id
             FROM model_environments me
             JOIN artifacts a ON a.artifact_id = me.artifact_id
             LEFT JOIN operation_outputs oo ON oo.artifact_id = me.artifact_id
             WHERE me.environment_id = ?1",
            [environment_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((artifact_id, blob_id, artifact_kind, operation_id)) = row else {
        return Ok(None);
    };
    if artifact_kind != ArtifactKind::ModelEnvironment.as_str() {
        return Err(StoreError::CorruptDatabase(format!(
            "model environment {environment_id} points to {artifact_kind} artifact"
        )));
    }
    let operation_id = operation_id.ok_or_else(|| {
        StoreError::CorruptDatabase(format!(
            "model environment {environment_id} has no producing operation"
        ))
    })?;
    Ok(Some(RecordedArtifact {
        artifact_id: parse_id(&artifact_id, "artifact_id")?,
        blob_id: parse_blob_id(&blob_id)?,
        operation_id: parse_id(&operation_id, "operation_id")?,
    }))
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

fn generation_family_fingerprint(starts: &[GenerationStart]) -> Result<BlobId> {
    request_fingerprint("loom.weave-family.v1", &json!({ "starts": starts }))
}

fn cancel_generation_fingerprint(command: CancelGenerationCommand) -> Result<BlobId> {
    request_fingerprint(
        "loom.cancel-generation.v1",
        &json!({ "run_id": command.run_id }),
    )
}

fn keep_alternative_fingerprint(command: KeepAlternativeCommand) -> Result<BlobId> {
    request_fingerprint(
        "loom.keep-alternative.v1",
        &json!({ "candidate_id": command.candidate_id }),
    )
}

fn promote_candidate_fingerprint(command: PromoteCandidateCommand) -> Result<BlobId> {
    request_fingerprint(
        "loom.promote-candidate.v1",
        &json!({
            "candidate_id": command.candidate_id,
            "expected_source_revision_id": command.expected_source_revision_id,
            "expected_visible_blob_id": command.expected_visible_blob_id,
        }),
    )
}

fn request_fingerprint<T: Serialize>(protocol: &'static str, request: &T) -> Result<BlobId> {
    let canonical = serde_json::to_vec(&json!({
        "protocol": protocol,
        "request": request,
    }))?;
    ensure_payload_size(
        "command request",
        canonical.len(),
        MAX_PROVENANCE_JSON_BYTES,
    )?;
    Ok(BlobId::digest(&canonical))
}

fn validate_non_candidate_terminal(
    status: GenerationTerminalStatus,
    error: Option<&str>,
) -> Result<()> {
    if status == GenerationTerminalStatus::Completed {
        return Err(StoreError::CompletedGenerationRequiresCandidate);
    }
    if status == GenerationTerminalStatus::Failed && error.is_none_or(str::is_empty) {
        return Err(StoreError::FailedGenerationRequiresError);
    }
    Ok(())
}

fn only_id<T: Copy>(values: &[T], label: &'static str) -> Result<T> {
    if let [value] = values {
        Ok(*value)
    } else {
        Err(StoreError::CorruptDatabase(format!(
            "receipt must contain exactly one {label}"
        )))
    }
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

#[allow(clippy::too_many_lines)]
fn insert_prepared_generation(
    transaction: &Transaction<'_>,
    prepared: &PreparedGenerationStart,
    active: ActiveRevision,
    created_at_ms: i64,
) -> Result<()> {
    let start = &prepared.generation;
    insert_blob_row(
        transaction,
        prepared.run_blob_id,
        prepared.run_payload.len(),
        created_at_ms,
    )?;
    insert_artifact(
        transaction,
        prepared.run_artifact_id,
        prepared.run_blob_id,
        ArtifactKind::GenerationRun,
        "application/json",
        &json!({"run_id": start.run_id}),
        created_at_ms,
    )?;
    transaction.execute(
        "INSERT INTO operations(operation_id, operation_kind, metadata_json, created_at_ms)
         VALUES (?1, 'generate', ?2, ?3)",
        params![
            prepared.operation_id.to_string(),
            serde_json::to_string(&json!({"run_id": start.run_id}))?,
            created_at_ms,
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
                prepared.operation_id.to_string(),
                i64::try_from(position).map_err(|_| StoreError::CorruptDatabase(
                    "operation input position overflow".into()
                ))?,
                artifact_id.to_string(),
            ],
        )?;
    }
    transaction.execute(
        "INSERT INTO operation_outputs(operation_id, position, artifact_id) VALUES (?1, 0, ?2)",
        params![
            prepared.operation_id.to_string(),
            prepared.run_artifact_id.to_string()
        ],
    )?;
    transaction.execute(
        "INSERT INTO branches(branch_id, document_id, source_revision_id, created_at_ms)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            start.branch_id.to_string(),
            start.document_id.to_string(),
            start.source_revision_id.to_string(),
            created_at_ms,
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
            prepared.run_artifact_id.to_string(),
            start.document_id.to_string(),
            start.source_revision_id.to_string(),
            active.blob_id.to_string(),
            range_start_i64(start.target_range)?,
            range_end_i64(start.target_range)?,
            start.model_environment_artifact_id.to_string(),
            start.prompt_recipe_artifact_id.to_string(),
            start.context_recipe_artifact_id.to_string(),
            start.authority_policy_artifact_id.to_string(),
            created_at_ms,
        ],
    )?;
    transaction.execute(
        "INSERT INTO generation_run_index(run_id, seed_decimal, model_identifier)
         VALUES (?1, ?2, ?3)",
        params![
            start.run_id.to_string(),
            start.seed.to_string(),
            prepared.indexed_model_identifier.as_deref(),
        ],
    )?;
    insert_generation_event(transaction, &prepared.queued_event, false)
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

fn insert_command_request(
    transaction: &Transaction<'_>,
    command_id: CommandId,
    request_fingerprint: BlobId,
    command_kind: CommandKind,
    created_at_ms: i64,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO command_requests(command_id, request_fingerprint, command_kind, created_at_ms)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            command_id.to_string(),
            request_fingerprint.to_string(),
            command_kind.as_str(),
            created_at_ms,
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_terminal_evidence_operation(
    transaction: &Transaction<'_>,
    operation_id: OperationId,
    run_id: GenerationRunId,
    run_artifact_id: ArtifactId,
    status: GenerationTerminalStatus,
    token_trace_artifact_id: ArtifactId,
    output_artifact_id: ArtifactId,
    created_at_ms: i64,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO operations(operation_id, operation_kind, metadata_json, created_at_ms)
         VALUES (?1, 'generate', ?2, ?3)",
        params![
            operation_id.to_string(),
            serde_json::to_string(&json!({
                "run_id": run_id,
                "terminal_status": status,
                "promotable": false,
            }))?,
            created_at_ms,
        ],
    )?;
    transaction.execute(
        "INSERT INTO operation_inputs(operation_id, position, artifact_id) VALUES (?1, 0, ?2)",
        params![operation_id.to_string(), run_artifact_id.to_string()],
    )?;
    for (position, artifact_id) in [token_trace_artifact_id, output_artifact_id]
        .into_iter()
        .enumerate()
    {
        transaction.execute(
            "INSERT INTO operation_outputs(operation_id, position, artifact_id) VALUES (?1, ?2, ?3)",
            params![
                operation_id.to_string(),
                i64::try_from(position).map_err(|_| StoreError::CorruptDatabase(
                    "terminal evidence output position overflow".into()
                ))?,
                artifact_id.to_string(),
            ],
        )?;
    }
    Ok(())
}

fn insert_terminal_evidence(
    transaction: &Transaction<'_>,
    evidence: &GenerationTerminalEvidence,
    created_at_ms: i64,
) -> Result<()> {
    if (evidence.status == GenerationTerminalStatus::Completed) != evidence.candidate_id.is_some() {
        return Err(StoreError::CorruptDatabase(
            "terminal evidence candidate/status mismatch".into(),
        ));
    }
    transaction.execute(
        "INSERT INTO generation_terminal_evidence(
            run_id, operation_id, output_artifact_id, output_blob_id,
            token_trace_artifact_id, candidate_id, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            evidence.run_id.to_string(),
            evidence.operation_id.to_string(),
            evidence.output_artifact_id.to_string(),
            evidence.output_blob_id.to_string(),
            evidence.token_trace_artifact_id.to_string(),
            evidence
                .candidate_id
                .map(|candidate_id| candidate_id.to_string()),
            created_at_ms,
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

#[derive(Clone, Debug)]
struct PreparedGenerationStart {
    generation: GenerationStart,
    indexed_model_identifier: Option<String>,
    run_payload: Vec<u8>,
    run_blob_id: BlobId,
    run_artifact_id: ArtifactId,
    operation_id: OperationId,
    queued_event: GenerationEvent,
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

type BranchSummaryRow = (
    String,
    String,
    String,
    i64,
    i64,
    i64,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<String>,
);

fn query_branch_summary_rows<P>(
    connection: &Connection,
    sql: &str,
    parameters: P,
) -> Result<Vec<BranchSummaryRow>>
where
    P: rusqlite::Params,
{
    let mut statement = connection.prepare(sql)?;
    statement
        .query_map(parameters, |row| {
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
                row.get(11)?,
                row.get(12)?,
                row.get(13)?,
                row.get(14)?,
                row.get(15)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn parse_branch_summary_row(
    row: BranchSummaryRow,
    document_id: DocumentId,
) -> Result<StoredBranchSummary> {
    let run_id = parse_id(&row.0, "run_id")?;
    let target_range = ByteRange {
        start: u64::try_from(row.3)
            .map_err(|_| StoreError::CorruptDatabase("negative generation target start".into()))?,
        end: u64::try_from(row.4)
            .map_err(|_| StoreError::CorruptDatabase("negative generation target end".into()))?,
    };
    if target_range.end < target_range.start {
        return Err(StoreError::CorruptDatabase(
            "generation target end precedes its start".into(),
        ));
    }
    let sequence = u64::try_from(row.6).map_err(|_| {
        StoreError::CorruptDatabase("generation run index has a non-positive sequence".into())
    })?;
    if sequence == 0 {
        return Err(StoreError::CorruptDatabase(
            "generation run index has a zero sequence".into(),
        ));
    }
    let seed = row
        .7
        .as_deref()
        .map(|value| {
            value.parse::<u64>().map_err(|error| {
                StoreError::CorruptDatabase(format!(
                    "invalid indexed generation seed `{value}`: {error}"
                ))
            })
        })
        .transpose()?;
    if row
        .10
        .as_ref()
        .is_some_and(|error| error.chars().count() > MAX_BRANCH_ERROR_CHARACTERS)
    {
        return Err(StoreError::CorruptDatabase(
            "branch error preview exceeds its bounded projection".into(),
        ));
    }
    let error_truncated = match row.11 {
        0 => false,
        1 => true,
        _ => {
            return Err(StoreError::CorruptDatabase(
                "branch error truncation flag is not boolean".into(),
            ));
        }
    };
    let output_blob_id = row.13.as_deref().map(parse_blob_id).transpose()?;
    let output_byte_len = row
        .14
        .map(|value| {
            u64::try_from(value).map_err(|_| {
                StoreError::CorruptDatabase("generation output has a negative byte length".into())
            })
        })
        .transpose()?;
    if output_blob_id.is_some() != output_byte_len.is_some() {
        return Err(StoreError::CorruptDatabase(
            "generation output blob identity and byte length disagree".into(),
        ));
    }
    Ok(StoredBranchSummary {
        run_id,
        branch_id: parse_id(&row.1, "branch_id")?,
        document_id,
        source_revision_id: parse_id(&row.2, "source_revision_id")?,
        target_range,
        cursor: BranchPageCursor { sequence, run_id },
        model_identifier: row.8,
        seed,
        status: parse_stored_branch_status(row.9.as_deref())?,
        candidate_id: row
            .12
            .as_deref()
            .map(|value| parse_id(value, "candidate_id"))
            .transpose()?,
        output_blob_id,
        output_byte_len,
        error: row.10,
        error_truncated,
        selection: row
            .15
            .as_deref()
            .map(parse_selection_decision)
            .transpose()?,
        created_at_ms: row.5,
    })
}

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
                .start_generation(self.generation_start(environment, target_range, 7))
                .expect("start generation")
        }

        fn generation_start(
            &self,
            environment: ArtifactId,
            target_range: ByteRange,
            seed: u64,
        ) -> GenerationStart {
            GenerationStart {
                run_id: GenerationRunId::new(),
                branch_id: BranchId::new(),
                document_id: self.loaded.document_id,
                source_revision_id: self.loaded.revision_id,
                target_range,
                model_environment_artifact_id: environment,
                prompt_recipe_artifact_id: self.prompt_recipe,
                context_recipe_artifact_id: self.context_recipe,
                authority_policy_artifact_id: self.policy,
                seed,
                sampling: json!({"temperature": 0.8}),
            }
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
    fn model_environment_recording_is_content_idempotent_and_rejects_identity_reuse() {
        let directory = tempdir().expect("temporary project");
        let root = directory.path().join("Novel");
        let (mut store, _) = ProjectStore::initialize(&root, "Novel").expect("initialize");
        let original = environment("stable-writer");

        let first = store
            .record_model_environment(&original)
            .expect("record model environment");
        let replay = store
            .record_model_environment(&original)
            .expect("replay model environment");
        assert_eq!(replay, first);
        assert_eq!(replay.operation_id, first.operation_id);

        let mut conflicting = original.clone();
        conflicting.backend_identifier = "different-backend".into();
        assert!(matches!(
            store.record_model_environment(&conflicting),
            Err(StoreError::ModelEnvironmentContentConflict { environment_id })
                if environment_id == original.environment_id
        ));

        let (environment_count, output_count): (i64, i64) = store
            .connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM model_environments WHERE environment_id = ?1),
                    (SELECT COUNT(*) FROM operation_outputs WHERE artifact_id = ?2)",
                params![
                    original.environment_id.to_string(),
                    first.artifact_id.to_string(),
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("model environment occurrence counts");
        assert_eq!(environment_count, 1);
        assert_eq!(output_count, 1);
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
    fn generation_family_rejects_empty_and_duplicate_identities_without_writes() {
        let mut fixture = Fixture::new();
        assert!(matches!(
            fixture
                .store
                .start_generation_family_with_command(CommandId::new(), Vec::new()),
            Err(StoreError::EmptyGenerationFamily)
        ));

        let end = u64::try_from(fixture.loaded.text.len()).expect("document length");
        let target = ByteRange { start: end, end };
        let first = fixture.generation_start(fixture.writer_environment, target, 11);
        let mut duplicate_run = fixture.generation_start(fixture.writer_environment, target, 12);
        duplicate_run.run_id = first.run_id;
        assert!(matches!(
            fixture.store.start_generation_family_with_command(
                CommandId::new(),
                vec![first.clone(), duplicate_run]
            ),
            Err(StoreError::DuplicateGenerationRun(run_id)) if run_id == first.run_id
        ));

        let mut duplicate_branch = fixture.generation_start(fixture.writer_environment, target, 13);
        duplicate_branch.branch_id = first.branch_id;
        assert!(matches!(
            fixture.store.start_generation_family_with_command(
                CommandId::new(),
                vec![first.clone(), duplicate_branch]
            ),
            Err(StoreError::DuplicateGenerationBranch(branch_id)) if branch_id == first.branch_id
        ));

        let mut mixed_source = fixture.generation_start(fixture.writer_environment, target, 14);
        mixed_source.source_revision_id = RevisionId::new();
        assert!(matches!(
            fixture
                .store
                .start_generation_family_with_command(CommandId::new(), vec![first, mixed_source]),
            Err(StoreError::GenerationFamilySourceMismatch)
        ));
        let run_count: i64 = fixture
            .store
            .connection
            .query_row("SELECT COUNT(*) FROM generation_runs", [], |row| row.get(0))
            .expect("count generation runs");
        assert_eq!(run_count, 0);
    }

    #[test]
    fn invalid_later_family_run_rolls_back_the_whole_sql_family() {
        let mut fixture = Fixture::new();
        let end = u64::try_from(fixture.loaded.text.len()).expect("document length");
        let valid = fixture.generation_start(
            fixture.writer_environment,
            ByteRange { start: end, end },
            21,
        );
        let invalid = fixture.generation_start(
            fixture.writer_environment,
            ByteRange {
                start: end + 1,
                end: end + 1,
            },
            22,
        );
        let command_id = CommandId::new();
        assert!(matches!(
            fixture
                .store
                .start_generation_family_with_command(command_id, vec![valid, invalid]),
            Err(StoreError::InvalidGenerationRange)
        ));
        for table in ["branches", "generation_runs", "generation_events"] {
            let count: i64 = fixture
                .store
                .connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("count family table");
            assert_eq!(count, 0, "{table} must remain empty");
        }
        assert!(
            fixture
                .store
                .load_receipt(command_id)
                .expect("load absent family receipt")
                .is_none()
        );
    }

    #[test]
    fn generation_family_commits_all_runs_under_one_shared_receipt() {
        let mut fixture = Fixture::new();
        let end = u64::try_from(fixture.loaded.text.len()).expect("document length");
        let target = ByteRange { start: end, end };
        let starts = vec![
            fixture.generation_start(fixture.writer_environment, target, 31),
            fixture.generation_start(fixture.writer_environment, target, 32),
            fixture.generation_start(fixture.writer_environment, target, 33),
        ];
        let command_id = CommandId::new();
        let family = fixture
            .store
            .start_generation_family_with_command(command_id, starts)
            .expect("start atomic generation family");

        assert_eq!(family.generations.len(), 3);
        assert_eq!(family.receipt.command_id, command_id);
        assert_eq!(family.receipt.command, CommandKind::Weave);
        assert_eq!(family.receipt.resulting_artifact_ids.len(), 3);
        assert_eq!(family.receipt.resulting_operation_ids.len(), 3);
        assert!(
            family
                .generations
                .iter()
                .all(|generation| generation.receipt == family.receipt)
        );
        let stored_receipts: i64 = fixture
            .store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM command_receipts WHERE command_id = ?1",
                [command_id.to_string()],
                |row| row.get(0),
            )
            .expect("count shared receipt");
        assert_eq!(stored_receipts, 1);
        let run_count: i64 = fixture
            .store
            .connection
            .query_row("SELECT COUNT(*) FROM generation_runs", [], |row| row.get(0))
            .expect("count family runs");
        assert_eq!(run_count, 3);
    }

    #[test]
    fn generation_family_exact_retry_replays_identities_and_rejects_command_reuse() {
        let mut fixture = Fixture::new();
        let end = u64::try_from(fixture.loaded.text.len()).expect("document length");
        let target = ByteRange { start: end, end };
        let starts = vec![
            fixture.generation_start(fixture.writer_environment, target, 34),
            fixture.generation_start(fixture.writer_environment, target, 35),
        ];
        let command_id = CommandId::new();
        let first = fixture
            .store
            .start_generation_family_with_command(command_id, starts.clone())
            .expect("start generation family");
        let replay = fixture
            .store
            .start_generation_family_with_command(command_id, starts)
            .expect("replay generation family");
        assert!(!first.replayed);
        assert!(replay.replayed);
        assert_eq!(replay.request_fingerprint, first.request_fingerprint);
        assert_eq!(replay.generations, first.generations);
        assert_eq!(replay.receipt, first.receipt);

        let conflicting = vec![fixture.generation_start(fixture.writer_environment, target, 36)];
        assert!(matches!(
            fixture
                .store
                .start_generation_family_with_command(command_id, conflicting),
            Err(StoreError::IdempotencyConflict { command_id: conflict }) if conflict == command_id
        ));
        let (runs, requests): (i64, i64) = fixture
            .store
            .connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM generation_runs),
                    (SELECT COUNT(*) FROM command_requests WHERE command_id = ?1)",
                [command_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("count idempotent generation rows");
        assert_eq!((runs, requests), (2, 1));
    }

    #[test]
    fn generation_family_preflight_returns_none_for_an_unknown_command_without_writes() {
        let fixture = Fixture::new();
        let before = fixture.store.counts().expect("counts before preflight");
        assert!(
            fixture
                .store
                .generation_family_for_command(CommandId::new())
                .expect("unknown command preflight")
                .is_none()
        );
        assert_eq!(
            fixture.store.counts().expect("counts after preflight"),
            before
        );
    }

    #[test]
    fn generation_family_preflight_reconstructs_existing_weave_without_writes() {
        let mut fixture = Fixture::new();
        let end = u64::try_from(fixture.loaded.text.len()).expect("document length");
        let starts = vec![
            fixture.generation_start(
                fixture.writer_environment,
                ByteRange { start: end, end },
                37,
            ),
            fixture.generation_start(
                fixture.writer_environment,
                ByteRange { start: end, end },
                38,
            ),
        ];
        let command_id = CommandId::new();
        let started = fixture
            .store
            .start_generation_family_with_command(command_id, starts)
            .expect("start weave for preflight");
        let before = fixture
            .store
            .counts()
            .expect("counts before replay preflight");
        let replay = fixture
            .store
            .generation_family_for_command(command_id)
            .expect("existing weave preflight")
            .expect("recorded weave family");

        assert!(replay.replayed);
        assert_eq!(replay.generations, started.generations);
        assert_eq!(replay.receipt, started.receipt);
        assert_eq!(replay.request_fingerprint, started.request_fingerprint);
        assert_eq!(
            fixture
                .store
                .counts()
                .expect("counts after replay preflight"),
            before
        );
    }

    #[test]
    fn generation_family_preflight_rejects_a_non_weave_command_id() {
        let mut fixture = Fixture::new();
        let start = fixture.start(fixture.writer_environment);
        let command_id = CommandId::new();
        fixture
            .store
            .request_cancel_generation_with_command(
                command_id,
                CancelGenerationCommand {
                    run_id: start.generation.run_id,
                },
            )
            .expect("record non-weave command");

        assert!(matches!(
            fixture.store.generation_family_for_command(command_id),
            Err(StoreError::IdempotencyConflict { command_id: conflict }) if conflict == command_id
        ));
    }

    #[test]
    fn cancel_exact_retry_survives_terminal_and_rejects_command_reuse() {
        let mut fixture = Fixture::new();
        let first = fixture.start(fixture.writer_environment);
        let second = fixture.start(fixture.writer_environment);
        let command_id = CommandId::new();
        let command = CancelGenerationCommand {
            run_id: first.generation.run_id,
        };
        let requested = fixture
            .store
            .request_cancel_generation_with_command(command_id, command)
            .expect("request cancellation");
        fixture
            .store
            .finish_generation(
                first.generation.run_id,
                GenerationTerminalStatus::Cancelled,
                None,
            )
            .expect("record cancelled terminal");
        let replay = fixture
            .store
            .request_cancel_generation_with_command(command_id, command)
            .expect("replay cancellation after terminal");
        assert!(!requested.replayed);
        assert!(replay.replayed);
        assert_eq!(replay.event, requested.event);
        assert_eq!(replay.receipt, requested.receipt);
        assert_eq!(replay.request_fingerprint, requested.request_fingerprint);

        assert!(matches!(
            fixture.store.request_cancel_generation_with_command(
                command_id,
                CancelGenerationCommand {
                    run_id: second.generation.run_id,
                },
            ),
            Err(StoreError::IdempotencyConflict { command_id: conflict }) if conflict == command_id
        ));
        let cancellations: i64 = fixture
            .store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM generation_events
                 WHERE run_id = ?1 AND event_kind = 'cancellation_requested'",
                [first.generation.run_id.to_string()],
                |row| row.get(0),
            )
            .expect("count cancellation requests");
        assert_eq!(cancellations, 1);
    }

    #[test]
    fn keep_exact_retry_replays_selection_and_rejects_command_reuse() {
        let mut fixture = Fixture::new();
        let first_start = fixture.start(fixture.writer_environment);
        let first = fixture.finish(first_start.generation.run_id, "first strand");
        let second_start = fixture.start(fixture.writer_environment);
        let second = fixture.finish(second_start.generation.run_id, "second strand");
        let command_id = CommandId::new();
        let command = KeepAlternativeCommand {
            candidate_id: first.candidate.candidate_id,
        };
        let kept = fixture
            .store
            .keep_alternative_with_command(command_id, command)
            .expect("keep candidate");
        let replay = fixture
            .store
            .keep_alternative_with_command(command_id, command)
            .expect("replay keep");
        assert!(!kept.replayed);
        assert!(replay.replayed);
        assert_eq!(replay.candidate_id, kept.candidate_id);
        assert_eq!(replay.selection_artifact_id, kept.selection_artifact_id);
        assert_eq!(replay.operation_id, kept.operation_id);
        assert_eq!(replay.receipt, kept.receipt);

        assert!(matches!(
            fixture.store.keep_alternative_with_command(
                command_id,
                KeepAlternativeCommand {
                    candidate_id: second.candidate.candidate_id,
                },
            ),
            Err(StoreError::IdempotencyConflict { command_id: conflict }) if conflict == command_id
        ));
        let selections: i64 = fixture
            .store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM selection_events WHERE command_id = ?1",
                [command_id.to_string()],
                |row| row.get(0),
            )
            .expect("count keep selection");
        assert_eq!(selections, 1);
    }

    #[test]
    fn promotion_exact_retry_returns_committed_identity_and_rejects_command_reuse() {
        let mut fixture = Fixture::new();
        let first_start = fixture.start(fixture.writer_environment);
        let first = fixture.finish(first_start.generation.run_id, "first promoted");
        let second_start = fixture.start(fixture.writer_environment);
        let second = fixture.finish(second_start.generation.run_id, "second private");
        let command_id = CommandId::new();
        let command = PromoteCandidateCommand {
            candidate_id: first.candidate.candidate_id,
            expected_source_revision_id: fixture.loaded.revision_id,
            expected_visible_blob_id: fixture.loaded.blob_id,
        };
        let promoted = fixture
            .store
            .promote_candidate_with_command(command_id, command)
            .expect("promote candidate");
        let replay = fixture
            .store
            .promote_candidate_with_command(command_id, command)
            .expect("replay promotion after active revision changed");
        assert!(!promoted.replayed);
        assert!(replay.replayed);
        assert_eq!(replay.save, promoted.save);
        assert_eq!(replay.candidate_id, promoted.candidate_id);
        assert_eq!(replay.selection_artifact_id, promoted.selection_artifact_id);
        assert_eq!(
            replay.attestation_artifact_id,
            promoted.attestation_artifact_id
        );
        assert_eq!(replay.visible_projection, VisibleProjectionState::Applied);

        assert!(matches!(
            fixture.store.promote_candidate_with_command(
                command_id,
                PromoteCandidateCommand {
                    candidate_id: second.candidate.candidate_id,
                    expected_source_revision_id: fixture.loaded.revision_id,
                    expected_visible_blob_id: fixture.loaded.blob_id,
                },
            ),
            Err(StoreError::IdempotencyConflict { command_id: conflict }) if conflict == command_id
        ));
        let revisions: i64 = fixture
            .store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM selection_events WHERE command_id = ?1",
                [command_id.to_string()],
                |row| row.get(0),
            )
            .expect("count promotion selection");
        assert_eq!(revisions, 1);
    }

    #[test]
    fn promotion_returns_committed_identity_with_pending_conflict_then_replays_to_applied() {
        let mut fixture = Fixture::new();
        let start = fixture.start(fixture.writer_environment);
        let candidate = fixture.finish(start.generation.run_id, "conflicted promotion");
        let command_id = CommandId::new();
        let command = PromoteCandidateCommand {
            candidate_id: candidate.candidate.candidate_id,
            expected_source_revision_id: fixture.loaded.revision_id,
            expected_visible_blob_id: fixture.loaded.blob_id,
        };
        let committed = fixture
            .store
            .promote_candidate_with_command_and_boundary(command_id, command, |visible| {
                fs::write(visible, "external edit at promotion boundary")?;
                Ok(())
            })
            .expect("promotion remains a committed outcome");
        assert!(!committed.replayed);
        assert!(matches!(
            committed.visible_projection,
            VisibleProjectionState::PendingConflict { .. }
        ));
        assert_eq!(
            fixture
                .store
                .load_receipt(command_id)
                .expect("load promotion receipt"),
            Some(committed.save.receipt.clone())
        );

        fs::write(
            fixture.store.root.join("manuscript/001.md"),
            fixture.loaded.text.as_bytes(),
        )
        .expect("restore acknowledged source bytes");
        let replay = fixture
            .store
            .promote_candidate_with_command(command_id, command)
            .expect("settle committed promotion");
        assert!(replay.replayed);
        assert_eq!(replay.save, committed.save);
        assert_eq!(replay.visible_projection, VisibleProjectionState::Applied);
    }

    #[test]
    fn promotion_wraps_postcommit_projection_error_as_pending_retry() {
        let mut fixture = Fixture::new();
        let start = fixture.start(fixture.writer_environment);
        let candidate = fixture.finish(start.generation.run_id, "retryable promotion");
        let command_id = CommandId::new();
        let command = PromoteCandidateCommand {
            candidate_id: candidate.candidate.candidate_id,
            expected_source_revision_id: fixture.loaded.revision_id,
            expected_visible_blob_id: fixture.loaded.blob_id,
        };
        let committed = fixture
            .store
            .promote_candidate_with_command_and_boundary(command_id, command, |_| {
                Err(StoreError::Io(std::io::Error::other(
                    "injected projection failure",
                )))
            })
            .expect("projection failure is typed state");
        assert!(matches!(
            committed.visible_projection,
            VisibleProjectionState::PendingRetry { .. }
        ));
        assert_eq!(
            fixture
                .store
                .pending_outbox_count()
                .expect("pending outbox"),
            1
        );

        let replay = fixture
            .store
            .promote_candidate_with_command(command_id, command)
            .expect("retry committed projection");
        assert!(replay.replayed);
        assert_eq!(replay.save, committed.save);
        assert_eq!(replay.visible_projection, VisibleProjectionState::Applied);
        assert_eq!(
            fixture
                .store
                .pending_outbox_count()
                .expect("settled outbox"),
            0
        );
    }

    #[test]
    fn interrupted_generation_recovery_is_explicit_atomic_and_idempotent() {
        let mut fixture = Fixture::new();
        let end = u64::try_from(fixture.loaded.text.len()).expect("document length");
        let target = ByteRange { start: end, end };
        let family = fixture
            .store
            .start_generation_family_with_command(
                CommandId::new(),
                vec![
                    fixture.generation_start(fixture.writer_environment, target, 41),
                    fixture.generation_start(fixture.writer_environment, target, 42),
                ],
            )
            .expect("start family for restart recovery");
        let already_terminal = family.generations[0].generation.run_id;
        let interrupted = family.generations[1].generation.run_id;
        fixture
            .store
            .finish_generation(already_terminal, GenerationTerminalStatus::Cancelled, None)
            .expect("finish sibling before restart");

        let recovered = fixture
            .store
            .recover_interrupted_generations()
            .expect("recover interrupted run");
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].run_id, interrupted);
        assert_eq!(recovered[0].status, GenerationTerminalStatus::Failed);
        assert_eq!(
            recovered[0].error.as_deref(),
            Some(INTERRUPTED_GENERATION_ERROR)
        );
        assert!(
            fixture
                .store
                .recover_interrupted_generations()
                .expect("repeat recovery")
                .is_empty()
        );
        assert_eq!(
            fixture
                .store
                .generation_terminal_count(already_terminal)
                .expect("cancelled terminal count"),
            1
        );
        assert_eq!(
            fixture
                .store
                .generation_terminal_count(interrupted)
                .expect("recovered terminal count"),
            1
        );
        let recovered_record = fixture
            .store
            .branch_record(
                fixture.loaded.document_id,
                interrupted,
                MAX_BRANCH_BODY_BYTES,
            )
            .expect("load recovered branch")
            .expect("recovered branch record");
        assert_eq!(recovered_record.status, StoredBranchStatus::Failed);
        assert_eq!(
            recovered_record.error.as_deref(),
            Some(INTERRUPTED_GENERATION_ERROR)
        );
        let recovered_evidence = fixture
            .store
            .generation_terminal_evidence(interrupted)
            .expect("load recovered terminal evidence")
            .expect("recovery evidence row");
        assert_eq!(
            recovered_evidence.evidence.status,
            GenerationTerminalStatus::Failed
        );
        assert!(recovered_evidence.output_bytes.is_empty());
        assert!(recovered_evidence.token_trace.provenance.is_none());
    }

    #[test]
    fn bounded_branch_page_and_exact_records_rebuild_durable_state() {
        let mut fixture = Fixture::new();
        let completed_start = fixture.start(fixture.writer_environment);
        let completed = fixture.finish(completed_start.generation.run_id, "durable strand");
        fixture
            .store
            .keep_alternative(KeepAlternativeCommand {
                candidate_id: completed.candidate.candidate_id,
            })
            .expect("keep completed candidate");
        let interrupted = fixture.start(fixture.writer_environment);

        let page = fixture
            .store
            .branch_page(fixture.loaded.document_id, None, 2)
            .expect("rebuild branch shelf");
        assert_eq!(page.branches.len(), 2);
        assert!(!page.has_more);
        assert!(page.next_cursor.is_none());
        let completed_summary = page
            .branches
            .iter()
            .find(|record| record.run_id == completed_start.generation.run_id)
            .expect("completed summary");
        assert_eq!(completed_summary.status, StoredBranchStatus::Completed);
        assert_eq!(completed_summary.seed, Some(7));
        assert_eq!(
            completed_summary.model_identifier.as_deref(),
            Some("test/writer")
        );
        assert_eq!(
            completed_summary.output_blob_id,
            Some(completed.candidate.output_blob_id)
        );
        assert_eq!(completed_summary.output_byte_len, Some(14));
        let completed_record = fixture
            .store
            .branch_record(
                fixture.loaded.document_id,
                completed_start.generation.run_id,
                MAX_BRANCH_BODY_BYTES,
            )
            .expect("load completed branch")
            .expect("completed record");
        assert_eq!(completed_record.status, StoredBranchStatus::Completed);
        assert_eq!(
            completed_record.output_text.as_deref(),
            Some("durable strand")
        );
        assert_eq!(
            completed_record.candidate_id,
            Some(completed.candidate.candidate_id)
        );
        assert_eq!(
            completed_record.selection,
            Some(SelectionDecision::KeepAlternative)
        );
        assert_eq!(completed_record.model_identifier, "test/writer");
        assert_eq!(completed_record.seed, 7);

        let interrupted_record = fixture
            .store
            .branch_record(
                fixture.loaded.document_id,
                interrupted.generation.run_id,
                MAX_BRANCH_BODY_BYTES,
            )
            .expect("load interrupted branch")
            .expect("interrupted record");
        assert_eq!(interrupted_record.status, StoredBranchStatus::Interrupted);
        assert!(interrupted_record.candidate_id.is_none());
        assert!(interrupted_record.output_text.is_none());
    }

    #[test]
    fn branch_cursor_is_stable_across_newer_insertions_and_bound_to_its_run() {
        let mut fixture = Fixture::new();
        for seed in 10..15 {
            let end = u64::try_from(fixture.loaded.text.len()).expect("document length");
            let start = fixture.generation_start(
                fixture.writer_environment,
                ByteRange { start: end, end },
                seed,
            );
            fixture
                .store
                .start_generation(start)
                .expect("start indexed generation");
        }
        let baseline = fixture
            .store
            .branch_page(fixture.loaded.document_id, None, MAX_BRANCH_PAGE_SIZE)
            .expect("baseline page");
        let first = fixture
            .store
            .branch_page(fixture.loaded.document_id, None, 2)
            .expect("first page");
        assert!(first.has_more);
        let cursor = first.next_cursor.expect("next cursor");

        let newer = fixture.start(fixture.writer_environment);
        let remainder = fixture
            .store
            .branch_page(
                fixture.loaded.document_id,
                Some(cursor),
                MAX_BRANCH_PAGE_SIZE,
            )
            .expect("continue stable cursor");
        let combined = first
            .branches
            .iter()
            .chain(&remainder.branches)
            .map(|branch| branch.run_id)
            .collect::<Vec<_>>();
        let expected = baseline
            .branches
            .iter()
            .map(|branch| branch.run_id)
            .collect::<Vec<_>>();
        assert_eq!(combined, expected);
        assert!(!combined.contains(&newer.generation.run_id));

        let forged = BranchPageCursor {
            sequence: cursor.sequence,
            run_id: baseline.branches[2].run_id,
        };
        assert!(matches!(
            fixture
                .store
                .branch_page(fixture.loaded.document_id, Some(forged), 2),
            Err(StoreError::InvalidBranchPageCursor)
        ));
    }

    #[test]
    fn branch_body_checks_indexed_and_filesystem_lengths_before_allocating() {
        let mut fixture = Fixture::new();
        let start = fixture.start(fixture.writer_environment);
        let terminal = fixture.finish(start.generation.run_id, "bounded body");
        assert!(matches!(
            fixture
                .store
                .branch_body(fixture.loaded.document_id, start.generation.run_id, 4),
            Err(StoreError::BranchBodyTooLarge {
                actual_bytes: 12,
                max_bytes: 4,
                ..
            })
        ));

        let hash = terminal.candidate.output_blob_id.to_hex();
        let path = fixture
            .store
            .root()
            .join(".loom/blobs/sha256")
            .join(&hash[..2])
            .join(&hash[2..]);
        fs::write(&path, b"this replacement grew beyond the read budget")
            .expect("replace fixture blob");
        assert!(matches!(
            fixture
                .store
                .branch_body(fixture.loaded.document_id, start.generation.run_id, 12,),
            Err(StoreError::BranchBodyTooLarge { max_bytes: 12, .. })
        ));
    }

    #[test]
    fn branch_page_truncates_error_metadata_and_rejects_unbounded_limits() {
        let mut fixture = Fixture::new();
        let start = fixture.start(fixture.writer_environment);
        fixture
            .store
            .finish_generation(
                start.generation.run_id,
                GenerationTerminalStatus::Failed,
                Some("x".repeat(MAX_BRANCH_ERROR_CHARACTERS + 17)),
            )
            .expect("finish failed generation");
        let page = fixture
            .store
            .branch_page(fixture.loaded.document_id, None, 1)
            .expect("bounded error page");
        assert_eq!(
            page.branches[0]
                .error
                .as_deref()
                .expect("error preview")
                .chars()
                .count(),
            MAX_BRANCH_ERROR_CHARACTERS
        );
        assert!(page.branches[0].error_truncated);
        assert!(matches!(
            fixture
                .store
                .branch_page(fixture.loaded.document_id, None, MAX_BRANCH_PAGE_SIZE + 1,),
            Err(StoreError::InvalidBranchPageLimit { .. })
        ));
        assert!(matches!(
            fixture.store.branch_body(
                fixture.loaded.document_id,
                start.generation.run_id,
                MAX_BRANCH_BODY_BYTES + 1,
            ),
            Err(StoreError::InvalidBranchBodyLimit { .. })
        ));
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
    fn completed_terminal_links_candidate_output_and_token_evidence() {
        let mut fixture = Fixture::new();
        let start = fixture.start(fixture.writer_environment);
        let completed = fixture.finish(start.generation.run_id, "preserved completion");
        let stored = fixture
            .store
            .generation_terminal_evidence(start.generation.run_id)
            .expect("load completed terminal evidence")
            .expect("completed evidence row");

        assert_eq!(stored.evidence, completed.evidence);
        assert_eq!(stored.evidence.status, GenerationTerminalStatus::Completed);
        assert_eq!(
            stored.evidence.candidate_id,
            Some(completed.candidate.candidate_id)
        );
        assert_eq!(stored.output_bytes, b"preserved completion");
        assert_eq!(stored.token_trace.generated_token_ids, vec![10, 11]);
        assert_eq!(
            fixture
                .store
                .read_blob(stored.token_trace.raw_event_stream_blob_id)
                .expect("read raw event evidence"),
            b"recorded event stream"
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn every_non_candidate_terminal_preserves_partial_output_receipt_trace_and_timings() {
        let mut fixture = Fixture::new();
        let cases = [
            (GenerationTerminalStatus::Cancelled, None),
            (
                GenerationTerminalStatus::Failed,
                Some("backend decode failed".to_owned()),
            ),
            (GenerationTerminalStatus::Pruned, None),
            (GenerationTerminalStatus::Rejected, None),
        ];

        for (index, (status, error)) in cases.into_iter().enumerate() {
            let index_u32 = u32::try_from(index).expect("small terminal fixture index");
            let index_u64 = u64::try_from(index).expect("small terminal fixture index");
            let start = fixture.start(fixture.writer_environment);
            let partial = format!("partial-{index}");
            let raw_bytes = format!("raw-events-{index}").into_bytes();
            let receipt_bytes = format!("backend-receipt-{index}").into_bytes();
            let raw_event_stream_blob_id = fixture
                .store
                .store_provenance_blob(&raw_bytes)
                .expect("store raw events");
            let backend_receipt_blob_id = fixture
                .store
                .store_provenance_blob(&receipt_bytes)
                .expect("store backend receipt");
            let outcome = fixture
                .store
                .finish_generation_with_evidence(
                    start.generation.run_id,
                    TerminalGenerationInput {
                        status,
                        error: error.clone(),
                        evidence: TerminalEvidenceInput {
                            partial_output_bytes: partial.as_bytes().to_vec(),
                            token_trace: TokenTrace {
                                generated_token_ids: vec![100 + index_u32],
                                observations: Vec::new(),
                                raw_event_stream_blob_id,
                                provenance: Some(GenerationProvenance {
                                    evidence_kind: InferenceEvidenceKind::LiveInference,
                                    metrics: GenerationMetrics {
                                        completion_tokens: Some(1),
                                        duration_ms: Some(20 + index_u64),
                                        first_token_ms: Some(5),
                                        ..GenerationMetrics::default()
                                    },
                                    backend_receipt_blob_id: Some(backend_receipt_blob_id),
                                    sequence_state_blob_id: None,
                                }),
                            },
                        },
                    },
                )
                .expect("finish non-candidate terminal with evidence");
            assert_eq!(outcome.terminal_event.status, status);
            assert!(outcome.terminal_event.candidate_id.is_none());

            let stored = fixture
                .store
                .generation_terminal_evidence(start.generation.run_id)
                .expect("load terminal evidence")
                .expect("terminal evidence row");
            assert_eq!(stored.evidence, outcome.evidence);
            assert_eq!(stored.output_bytes, partial.as_bytes());
            let summary = fixture
                .store
                .branch_summary(fixture.loaded.document_id, start.generation.run_id)
                .expect("load non-candidate branch metadata")
                .expect("non-candidate branch summary");
            assert!(summary.output_blob_id.is_none());
            assert!(summary.output_byte_len.is_none());
            assert!(
                fixture
                    .store
                    .branch_body(
                        fixture.loaded.document_id,
                        start.generation.run_id,
                        MAX_BRANCH_BODY_BYTES,
                    )
                    .expect("query non-candidate body")
                    .is_none()
            );
            assert_eq!(
                stored
                    .token_trace
                    .provenance
                    .as_ref()
                    .and_then(|provenance| provenance.metrics.duration_ms),
                Some(20 + index_u64)
            );
            assert_eq!(
                fixture
                    .store
                    .read_blob(stored.token_trace.raw_event_stream_blob_id)
                    .expect("read stored raw events"),
                raw_bytes
            );
            let stored_receipt_id = stored
                .token_trace
                .provenance
                .as_ref()
                .and_then(|provenance| provenance.backend_receipt_blob_id)
                .expect("backend receipt identity");
            assert_eq!(
                fixture
                    .store
                    .read_blob(stored_receipt_id)
                    .expect("read stored backend receipt"),
                receipt_bytes
            );
        }

        let (terminals, evidence, candidates): (i64, i64, i64) = fixture
            .store
            .connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM generation_terminals),
                    (SELECT COUNT(*) FROM generation_terminal_evidence),
                    (SELECT COUNT(*) FROM generation_candidates)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("count terminal evidence rows");
        assert_eq!((terminals, evidence, candidates), (4, 4, 0));
        assert!(
            fixture
                .store
                .connection
                .execute(
                    "UPDATE generation_terminal_evidence SET created_at_ms = 0",
                    [],
                )
                .is_err()
        );
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
