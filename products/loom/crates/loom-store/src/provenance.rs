use std::fmt;
use std::path::Path;
use std::str::FromStr;

use loom_document::DocumentContent;
use loom_types::{
    ArtifactId, BlobId, ByteRange, CommandId, CommandKind, CommandReceipt, ContributionKind,
    DocumentId, DocumentKind, OperationId, OperationKind, RevisionId, now_unix_ms,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use similar::{Algorithm, DiffTag, capture_diff_slices};

use crate::draft::{TransientDraftClaim, consume_transient_draft_in_transaction};
use crate::paths::{inspect_document_path, normalize_document_path};
use crate::store::{
    ActiveRevision, ProjectStore, SaveOutcome, VisibleProjectionState, persist_receipt_in,
    visible_hash_if_present,
};
use crate::{MAX_DOCUMENT_BYTES, Result, StoreError};

const MAX_REASON_BYTES: usize = 4 * 1024;

/// Maximum UTF-8 bytes in the changed middle window passed to the diff engine.
/// Common unchanged prefix and suffix bytes are excluded from this budget.
pub const MAX_EDIT_DIFF_WINDOW_BYTES: usize = 64 * 1024;
/// Maximum Unicode scalar values in the changed middle window.
pub const MAX_EDIT_DIFF_WINDOW_CHARACTERS: usize = 16 * 1024;
/// Conservative upper bound for quadratic diff work (`(old_chars + new_chars)^2`).
pub const MAX_EDIT_DIFF_WORK: u64 = 16 * 1024 * 1024;
/// Hard bound on immutable slices referenced by one semantic revision.
pub const MAX_REVISION_SEGMENTS: usize = 16 * 1024;
const MAX_DIFF_SEGMENT_VISITS: u64 = 1_000_000;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IdempotentSaveOutcome {
    pub save: SaveOutcome,
    pub visible_projection: VisibleProjectionState,
    pub request_fingerprint: BlobId,
    pub replayed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProvenanceSegment {
    pub artifact_id: ArtifactId,
    pub byte_range: ByteRange,
    pub contribution: ContributionKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RevisionProvenance {
    pub revision_id: RevisionId,
    pub segments: Vec<ProvenanceSegment>,
}

impl ProjectStore {
    pub fn save_document_if_source(
        &mut self,
        relative_path: impl AsRef<Path>,
        content: DocumentContent,
        reason: impl Into<String>,
        expected_revision_id: RevisionId,
        expected_visible_blob_id: BlobId,
    ) -> Result<IdempotentSaveOutcome> {
        self.save_document_if_source_idempotent(
            CommandId::new(),
            relative_path,
            content,
            reason,
            expected_revision_id,
            expected_visible_blob_id,
        )
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn save_document_if_source_idempotent(
        &mut self,
        command_id: CommandId,
        relative_path: impl AsRef<Path>,
        content: DocumentContent,
        reason: impl Into<String>,
        expected_revision_id: RevisionId,
        expected_visible_blob_id: BlobId,
    ) -> Result<IdempotentSaveOutcome> {
        self.save_document_if_source_idempotent_inner(
            command_id,
            relative_path,
            content,
            reason,
            expected_revision_id,
            expected_visible_blob_id,
            None,
            |_| Ok(()),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn save_document_if_source_idempotent_consuming_draft(
        &mut self,
        command_id: CommandId,
        relative_path: impl AsRef<Path>,
        content: DocumentContent,
        reason: impl Into<String>,
        expected_revision_id: RevisionId,
        expected_visible_blob_id: BlobId,
        observed_draft_version: u64,
    ) -> Result<IdempotentSaveOutcome> {
        self.save_document_if_source_idempotent_inner(
            command_id,
            relative_path,
            content,
            reason,
            expected_revision_id,
            expected_visible_blob_id,
            Some(observed_draft_version),
            |_| Ok(()),
        )
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn save_document_if_source_idempotent_with_boundary<F>(
        &mut self,
        command_id: CommandId,
        relative_path: impl AsRef<Path>,
        content: DocumentContent,
        reason: impl Into<String>,
        expected_revision_id: RevisionId,
        expected_visible_blob_id: BlobId,
        before_projection_boundary: F,
    ) -> Result<IdempotentSaveOutcome>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        self.save_document_if_source_idempotent_inner(
            command_id,
            relative_path,
            content,
            reason,
            expected_revision_id,
            expected_visible_blob_id,
            None,
            before_projection_boundary,
        )
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn save_document_if_source_idempotent_inner<F>(
        &mut self,
        command_id: CommandId,
        relative_path: impl AsRef<Path>,
        content: DocumentContent,
        reason: impl Into<String>,
        expected_revision_id: RevisionId,
        expected_visible_blob_id: BlobId,
        observed_draft_version: Option<u64>,
        before_projection_boundary: F,
    ) -> Result<IdempotentSaveOutcome>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        let started_at_ms = now_unix_ms();
        let relative_path = normalize_document_path(relative_path.as_ref())?;
        let reason = reason.into();
        if reason.len() > MAX_REASON_BYTES {
            return Err(StoreError::ReasonTooLong {
                max_bytes: MAX_REASON_BYTES,
            });
        }
        let document_kind = content.kind();
        let projection = content.project_visible()?;
        drop(content);
        ensure_bounded_document(&projection.bytes)?;
        let target_blob_id = BlobId::digest(&projection.bytes);
        let draft_claim = observed_draft_version.map(|version| TransientDraftClaim {
            version,
            source_revision_id: expected_revision_id,
            blob_id: target_blob_id,
        });
        let request_fingerprint = checkpoint_fingerprint(
            &relative_path,
            document_kind,
            &projection.bytes,
            &reason,
            expected_revision_id,
            expected_visible_blob_id,
            draft_claim,
        )?;

        if let Some(outcome) = self.replay_idempotent_save(command_id, request_fingerprint)? {
            // The database claim is authoritative. Slot files are bounded
            // recovery caches, so cleanup must not turn a committed command
            // into an ambiguous failure.
            if draft_claim.is_some()
                && let Ok(Some(document)) = self.document_by_path(&relative_path)
            {
                let _ = self.cleanup_draft_slots_if_unreferenced(document.id);
            }
            return Ok(outcome);
        }

        let document = self
            .document_by_path(&relative_path)?
            .ok_or_else(|| StoreError::NoActiveRevision(relative_path.clone()))?;
        if document.kind != document_kind {
            return Err(StoreError::DocumentKindMismatch {
                path: relative_path,
                stored: document.kind,
                requested: document_kind,
            });
        }
        let active = self
            .active_revision(document.id)?
            .ok_or_else(|| StoreError::NoActiveRevision(relative_path.clone()))?;
        validate_expected_source(active, expected_revision_id, expected_visible_blob_id)?;
        self.verify_visible_source(&relative_path, expected_visible_blob_id)?;

        let source_bytes = self.read_blob(active.blob_id)?;
        let source_segments = self.load_revision_segments(expected_revision_id)?;
        validate_segment_projection(self, &source_segments, &source_bytes)?;
        let edit_plan = build_utf8_edit_plan(&source_segments, &source_bytes, &projection.bytes)?;

        let stored_target_blob_id = self.put_blob(&projection.bytes)?;
        if stored_target_blob_id != target_blob_id {
            return Err(StoreError::CorruptDatabase(
                "content-addressed blob write returned the wrong identity".into(),
            ));
        }
        let human_blob_id = if edit_plan.human_bytes.is_empty() {
            None
        } else {
            Some(self.put_blob(&edit_plan.human_bytes)?)
        };
        let revision_artifact_id = ArtifactId::new();
        let human_artifact_id = human_blob_id.map(|_| ArtifactId::new());
        let operation_id = OperationId::new();
        let revision_id = RevisionId::new();
        let created_at_ms = now_unix_ms();
        let receipt = CommandReceipt {
            command_id,
            command: CommandKind::Checkpoint,
            project_id: self.manifest.project_id,
            project_schema_version: self.manifest.schema_version,
            source_revision_id: Some(expected_revision_id),
            resulting_artifact_ids: std::iter::once(revision_artifact_id)
                .chain(human_artifact_id)
                .collect(),
            resulting_operation_ids: vec![operation_id],
            resulting_revision_ids: vec![revision_id],
            started_at_ms,
            completed_at_ms: created_at_ms,
        };

        let human_byte_len = edit_plan.human_bytes.len();
        let mut final_segments = edit_plan.materialize(human_artifact_id)?;
        merge_adjacent_segments(&mut final_segments);

        let write = SourceBoundRevision {
            command_id,
            request_fingerprint,
            document_id: document.id,
            document_kind: document.kind,
            relative_path: &relative_path,
            reason: &reason,
            expected: active,
            outbox_expected_visible_blob_id: active.blob_id,
            workflow: "source_bound_checkpoint",
            operation_kind: OperationKind::HumanEdit,
            target_blob_id,
            target_byte_len: projection.bytes.len(),
            revision_artifact_id,
            human_artifact_id,
            human_blob_id,
            human_byte_len,
            operation_id,
            revision_id,
            segments: &final_segments,
            receipt: &receipt,
            created_at_ms,
            draft_claim,
        };
        let outbox_id = self.insert_source_bound_revision(&write)?;
        let Some(outbox_id) = outbox_id else {
            let outcome = self
                .replay_idempotent_save(command_id, request_fingerprint)?
                .ok_or_else(|| {
                    StoreError::CorruptDatabase(
                        "idempotent command vanished after concurrent insertion".into(),
                    )
                })?;
            if draft_claim.is_some() {
                let _ = self.cleanup_draft_slots_if_unreferenced(document.id);
            }
            return Ok(outcome);
        };
        if draft_claim.is_some() {
            let _ = self.cleanup_draft_slots_if_unreferenced(document.id);
        }

        let visible_projection = self.settle_outbox_entry_with_boundary(
            outbox_id,
            &relative_path,
            before_projection_boundary,
        );
        Ok(IdempotentSaveOutcome {
            save: SaveOutcome {
                blob_id: target_blob_id,
                artifact_id: revision_artifact_id,
                operation_id,
                revision_id,
                receipt,
            },
            visible_projection,
            request_fingerprint,
            replayed: false,
        })
    }

    pub fn revision_provenance(&self, revision_id: RevisionId) -> Result<RevisionProvenance> {
        Ok(RevisionProvenance {
            revision_id,
            segments: self
                .load_revision_segments(revision_id)?
                .into_iter()
                .map(StoredSegment::into_public)
                .collect(),
        })
    }

    pub fn reconstruct_revision(&self, revision_id: RevisionId) -> Result<Vec<u8>> {
        let segments = self.load_revision_segments(revision_id)?;
        reconstruct_segments(self, &segments)
    }

    pub(crate) fn verify_visible_source(
        &self,
        relative_path: &str,
        expected: BlobId,
    ) -> Result<()> {
        let visible_path = inspect_document_path(&self.root, relative_path)?;
        let actual = visible_hash_if_present(&visible_path)?
            .ok_or_else(|| StoreError::UncheckpointedVisibleChange(relative_path.to_owned()))?;
        if actual != expected {
            return Err(StoreError::SourceBlobMismatch { expected, actual });
        }
        Ok(())
    }

    pub(crate) fn replay_idempotent_save(
        &mut self,
        command_id: CommandId,
        request_fingerprint: BlobId,
    ) -> Result<Option<IdempotentSaveOutcome>> {
        let row: Option<(String, String)> = self
            .connection
            .query_row(
                "SELECT cr.request_fingerprint, r.receipt_json
                 FROM command_requests cr
                 JOIN command_receipts r ON r.command_id = cr.command_id
                 WHERE cr.command_id = ?1",
                [command_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((recorded_fingerprint, receipt_json)) = row else {
            return Ok(None);
        };
        if parse_blob_id(&recorded_fingerprint)? != request_fingerprint {
            return Err(StoreError::IdempotencyConflict { command_id });
        }
        let receipt: CommandReceipt = serde_json::from_str(&receipt_json)?;
        let revision_id = first(&receipt.resulting_revision_ids, "revision")?;
        let artifact_id = first(&receipt.resulting_artifact_ids, "artifact")?;
        let operation_id = first(&receipt.resulting_operation_ids, "operation")?;
        let blob_id: String = self.connection.query_row(
            "SELECT blob_id FROM artifacts WHERE artifact_id = ?1",
            [artifact_id.to_string()],
            |row| row.get(0),
        )?;
        let (outbox_id, relative_path): (i64, String) = self.connection.query_row(
            "SELECT outbox_id, relative_path FROM visible_file_outbox WHERE revision_id = ?1",
            [revision_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let visible_projection = self.settle_outbox_entry(outbox_id, &relative_path);
        Ok(Some(IdempotentSaveOutcome {
            save: SaveOutcome {
                blob_id: parse_blob_id(&blob_id)?,
                artifact_id,
                operation_id,
                revision_id,
                receipt,
            },
            visible_projection,
            request_fingerprint,
            replayed: true,
        }))
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn insert_source_bound_revision(
        &mut self,
        write: &SourceBoundRevision<'_>,
    ) -> Result<Option<i64>> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(recorded) = transaction
            .query_row(
                "SELECT request_fingerprint FROM command_requests WHERE command_id = ?1",
                [write.command_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            if parse_blob_id(&recorded)? == write.request_fingerprint {
                transaction.rollback()?;
                return Ok(None);
            }
            return Err(StoreError::IdempotencyConflict {
                command_id: write.command_id,
            });
        }
        if transaction
            .query_row(
                "SELECT 1 FROM command_receipts WHERE command_id = ?1",
                [write.command_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some()
        {
            return Err(StoreError::IdempotencyConflict {
                command_id: write.command_id,
            });
        }
        validate_active_in_transaction(&transaction, write.document_id, write.expected)?;
        if let Some(claim) = write.draft_claim {
            consume_transient_draft_in_transaction(&transaction, write.document_id, claim)?;
        }

        insert_blob_row(
            &transaction,
            write.target_blob_id,
            write.target_byte_len,
            write.created_at_ms,
        )?;
        if let Some(human_blob_id) = write.human_blob_id {
            insert_blob_row(
                &transaction,
                human_blob_id,
                write.human_byte_len,
                write.created_at_ms,
            )?;
        }
        let metadata = serde_json::to_string(&json!({
            "workflow": write.workflow,
            "relative_path": write.relative_path,
            "reason": write.reason,
            "source_revision_id": write.expected.revision_id,
        }))?;
        transaction.execute(
            "INSERT INTO artifacts(artifact_id, blob_id, artifact_kind, media_type, metadata_json, created_at_ms)
             VALUES (?1, ?2, 'document_revision', ?3, ?4, ?5)",
            params![
                write.revision_artifact_id.to_string(),
                write.target_blob_id.to_string(),
                document_media_type(write.document_kind),
                metadata,
                write.created_at_ms,
            ],
        )?;
        if let (Some(human_artifact_id), Some(human_blob_id)) =
            (write.human_artifact_id, write.human_blob_id)
        {
            transaction.execute(
                "INSERT INTO artifacts(artifact_id, blob_id, artifact_kind, media_type, metadata_json, created_at_ms)
                 VALUES (?1, ?2, 'human_contribution', ?3, ?4, ?5)",
                params![
                    human_artifact_id.to_string(),
                    human_blob_id.to_string(),
                    document_media_type(write.document_kind),
                    serde_json::to_string(&json!({
                        "workflow": write.workflow,
                        "reason": write.reason,
                    }))?,
                    write.created_at_ms,
                ],
            )?;
        }
        transaction.execute(
            "INSERT INTO operations(operation_id, operation_kind, metadata_json, created_at_ms)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                write.operation_id.to_string(),
                write.operation_kind.as_str(),
                serde_json::to_string(&json!({
                    "workflow": write.workflow,
                    "relative_path": write.relative_path,
                    "reason": write.reason,
                    "source_revision_id": write.expected.revision_id,
                    "expected_visible_blob_id": write.outbox_expected_visible_blob_id,
                }))?,
                write.created_at_ms,
            ],
        )?;
        transaction.execute(
            "INSERT INTO operation_inputs(operation_id, position, artifact_id) VALUES (?1, 0, ?2)",
            params![
                write.operation_id.to_string(),
                write.expected.artifact_id.to_string()
            ],
        )?;
        transaction.execute(
            "INSERT INTO operation_outputs(operation_id, position, artifact_id) VALUES (?1, 0, ?2)",
            params![
                write.operation_id.to_string(),
                write.revision_artifact_id.to_string()
            ],
        )?;
        if let Some(human_artifact_id) = write.human_artifact_id {
            transaction.execute(
                "INSERT INTO operation_outputs(operation_id, position, artifact_id) VALUES (?1, 1, ?2)",
                params![write.operation_id.to_string(), human_artifact_id.to_string()],
            )?;
        }
        transaction.execute(
            "INSERT INTO revisions(revision_id, document_id, parent_revision_id, artifact_id, reason, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                write.revision_id.to_string(),
                write.document_id.to_string(),
                write.expected.revision_id.to_string(),
                write.revision_artifact_id.to_string(),
                write.reason,
                write.created_at_ms,
            ],
        )?;
        insert_revision_segments(&transaction, write.revision_id, write.segments)?;
        transaction.execute(
            "INSERT INTO visible_file_outbox(revision_id, relative_path, target_blob_id, expected_visible_blob_id, state, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, 'pending', ?5)",
            params![
                write.revision_id.to_string(),
                write.relative_path,
                write.target_blob_id.to_string(),
                write.outbox_expected_visible_blob_id.to_string(),
                write.created_at_ms,
            ],
        )?;
        let outbox_id = transaction.last_insert_rowid();
        persist_receipt_in(&transaction, write.receipt)?;
        transaction.execute(
            "INSERT INTO command_requests(command_id, request_fingerprint, command_kind, created_at_ms)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                write.command_id.to_string(),
                write.request_fingerprint.to_string(),
                write.receipt.command.as_str(),
                write.created_at_ms,
            ],
        )?;
        transaction.commit()?;
        Ok(Some(outbox_id))
    }

    pub(crate) fn load_revision_segments(
        &self,
        revision_id: RevisionId,
    ) -> Result<Vec<StoredSegment>> {
        let mut statement = self.connection.prepare(
            "SELECT artifact_id, start_byte, end_byte, contribution_kind
             FROM revision_segments WHERE revision_id = ?1 ORDER BY position",
        )?;
        let rows = statement.query_map([revision_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut segments = Vec::new();
        for row in rows {
            let (artifact_id, start, end, contribution) = row?;
            segments.push(StoredSegment {
                artifact_id: parse_id(&artifact_id, "artifact_id")?,
                start: u64::try_from(start)
                    .map_err(|_| StoreError::CorruptDatabase("negative segment start".into()))?,
                end: u64::try_from(end)
                    .map_err(|_| StoreError::CorruptDatabase("negative segment end".into()))?,
                contribution: parse_contribution(&contribution)?,
            });
        }
        if segments.is_empty() {
            let byte_len: i64 = self.connection.query_row(
                "SELECT b.byte_len
                 FROM revisions r
                 JOIN artifacts a ON a.artifact_id = r.artifact_id
                 JOIN blobs b ON b.blob_id = a.blob_id
                 WHERE r.revision_id = ?1",
                [revision_id.to_string()],
                |row| row.get(0),
            )?;
            if byte_len != 0 {
                return Err(StoreError::CorruptDatabase(format!(
                    "non-empty revision {revision_id} has no segments"
                )));
            }
        }
        Ok(segments)
    }
}

fn checkpoint_fingerprint(
    relative_path: &str,
    kind: DocumentKind,
    bytes: &[u8],
    reason: &str,
    expected_revision_id: RevisionId,
    expected_visible_blob_id: BlobId,
    draft_claim: Option<TransientDraftClaim>,
) -> Result<BlobId> {
    let canonical = if let Some(draft_claim) = draft_claim {
        serde_json::to_vec(&json!({
            "protocol": "loom.checkpoint-if-source.v2",
            "relative_path": relative_path,
            "document_kind": kind.as_str(),
            "content_blob_id": BlobId::digest(bytes),
            "reason": reason,
            "expected_revision_id": expected_revision_id,
            "expected_visible_blob_id": expected_visible_blob_id,
            "draft_claim": draft_claim,
        }))?
    } else {
        // Keep v1 byte-for-byte stable so commands recorded before the draft
        // journal existed remain replayable.
        serde_json::to_vec(&json!({
            "protocol": "loom.checkpoint-if-source.v1",
            "relative_path": relative_path,
            "document_kind": kind.as_str(),
            "content_blob_id": BlobId::digest(bytes),
            "reason": reason,
            "expected_revision_id": expected_revision_id,
            "expected_visible_blob_id": expected_visible_blob_id,
        }))?
    };
    Ok(BlobId::digest(&canonical))
}

pub(crate) fn validate_expected_source(
    active: ActiveRevision,
    expected_revision_id: RevisionId,
    expected_blob_id: BlobId,
) -> Result<()> {
    if active.revision_id != expected_revision_id {
        return Err(StoreError::SourceRevisionMismatch {
            expected: expected_revision_id,
            actual: active.revision_id,
        });
    }
    if active.blob_id != expected_blob_id {
        return Err(StoreError::SourceBlobMismatch {
            expected: expected_blob_id,
            actual: active.blob_id,
        });
    }
    Ok(())
}

pub(crate) fn validate_active_in_transaction(
    transaction: &Transaction<'_>,
    document_id: DocumentId,
    expected: ActiveRevision,
) -> Result<()> {
    let row: (String, String, String) = transaction.query_row(
        "SELECT r.revision_id, r.artifact_id, a.blob_id
         FROM revisions r JOIN artifacts a ON a.artifact_id = r.artifact_id
         WHERE r.document_id = ?1
         ORDER BY r.created_at_ms DESC, r.revision_id DESC LIMIT 1",
        [document_id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let actual = ActiveRevision {
        revision_id: parse_id(&row.0, "revision_id")?,
        artifact_id: parse_id(&row.1, "artifact_id")?,
        blob_id: parse_blob_id(&row.2)?,
    };
    validate_expected_source(actual, expected.revision_id, expected.blob_id)
}

pub(crate) fn insert_blob_row(
    transaction: &Transaction<'_>,
    blob_id: BlobId,
    byte_len: usize,
    created_at_ms: i64,
) -> Result<()> {
    let byte_len = i64::try_from(byte_len).map_err(|_| StoreError::DocumentTooLarge {
        actual_bytes: u64::MAX,
        max_bytes: MAX_DOCUMENT_BYTES,
    })?;
    transaction.execute(
        "INSERT OR IGNORE INTO blobs(blob_id, byte_len, media_type, created_at_ms)
         VALUES (?1, ?2, 'application/octet-stream', ?3)",
        params![blob_id.to_string(), byte_len, created_at_ms],
    )?;
    Ok(())
}

pub(crate) fn insert_revision_segments(
    transaction: &Transaction<'_>,
    revision_id: RevisionId,
    segments: &[StoredSegment],
) -> Result<()> {
    ensure_revision_segment_budget(segments.len())?;
    for (position, segment) in segments.iter().enumerate() {
        transaction.execute(
            "INSERT INTO revision_segments(revision_id, position, artifact_id, start_byte, end_byte, contribution_kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                revision_id.to_string(),
                i64::try_from(position).map_err(|_| StoreError::CorruptDatabase(
                    "too many revision segments".into()
                ))?,
                segment.artifact_id.to_string(),
                i64::try_from(segment.start).map_err(|_| StoreError::CorruptDatabase(
                    "segment start does not fit SQLite".into()
                ))?,
                i64::try_from(segment.end).map_err(|_| StoreError::CorruptDatabase(
                    "segment end does not fit SQLite".into()
                ))?,
                segment.contribution.as_str(),
            ],
        )?;
    }
    Ok(())
}

pub(crate) fn ensure_bounded_document(bytes: &[u8]) -> Result<()> {
    let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if length > MAX_DOCUMENT_BYTES {
        return Err(StoreError::DocumentTooLarge {
            actual_bytes: length,
            max_bytes: MAX_DOCUMENT_BYTES,
        });
    }
    Ok(())
}

pub(crate) fn build_utf8_edit_plan(
    source_segments: &[StoredSegment],
    old_bytes: &[u8],
    new_bytes: &[u8],
) -> Result<EditPlan> {
    let old = std::str::from_utf8(old_bytes)
        .map_err(|_| StoreError::CorruptDatabase("source revision is not UTF-8".into()))?;
    let new = std::str::from_utf8(new_bytes)
        .map_err(|_| StoreError::CorruptDatabase("edited revision is not UTF-8".into()))?;
    ensure_revision_segment_budget(source_segments.len())?;
    let (prefix_end, suffix_length) = common_utf8_edges(old, new);
    let old_middle_end = old.len().saturating_sub(suffix_length);
    let new_middle_end = new.len().saturating_sub(suffix_length);
    let old_middle = &old[prefix_end..old_middle_end];
    let new_middle = &new[prefix_end..new_middle_end];
    ensure_edit_diff_budget(old_middle, new_middle)?;

    let old_characters: Vec<char> = old_middle.chars().collect();
    let new_characters: Vec<char> = new_middle.chars().collect();
    let old_offsets = utf8_offsets(old_middle);
    let new_offsets = utf8_offsets(new_middle);
    let operations = capture_diff_slices(Algorithm::Myers, &old_characters, &new_characters);
    let segment_visits = u64::try_from(source_segments.len())
        .unwrap_or(u64::MAX)
        .saturating_mul(u64::try_from(operations.len()).unwrap_or(u64::MAX));
    if segment_visits > MAX_DIFF_SEGMENT_VISITS {
        return Err(StoreError::EditDiffBudgetExceeded {
            metric: "segment visits",
            actual: segment_visits,
            limit: MAX_DIFF_SEGMENT_VISITS,
        });
    }

    let mut plan = EditPlan {
        segments: Vec::new(),
        human_bytes: Vec::new(),
    };
    let mut reconstructed = Vec::with_capacity(new_bytes.len());
    if prefix_end != 0 {
        plan.segments.extend(
            slice_segments(source_segments, 0, prefix_end)?
                .into_iter()
                .map(PlannedSegment::Existing),
        );
        ensure_revision_segment_budget(plan.segments.len())?;
        reconstructed.extend_from_slice(&old_bytes[..prefix_end]);
    }
    for operation in operations {
        let old_range = operation.old_range();
        let new_range = operation.new_range();
        let old_start = prefix_end + old_offsets[old_range.start];
        let old_end = prefix_end + old_offsets[old_range.end];
        let new_start = prefix_end + new_offsets[new_range.start];
        let new_end = prefix_end + new_offsets[new_range.end];
        match operation.tag() {
            DiffTag::Equal => {
                plan.segments.extend(
                    slice_segments(source_segments, old_start, old_end)?
                        .into_iter()
                        .map(PlannedSegment::Existing),
                );
                ensure_revision_segment_budget(plan.segments.len())?;
                reconstructed.extend_from_slice(&old_bytes[old_start..old_end]);
            }
            DiffTag::Insert | DiffTag::Replace => {
                let human_start = u64::try_from(plan.human_bytes.len()).map_err(|_| {
                    StoreError::DocumentTooLarge {
                        actual_bytes: u64::MAX,
                        max_bytes: MAX_DOCUMENT_BYTES,
                    }
                })?;
                plan.human_bytes
                    .extend_from_slice(&new_bytes[new_start..new_end]);
                let human_end = u64::try_from(plan.human_bytes.len()).map_err(|_| {
                    StoreError::DocumentTooLarge {
                        actual_bytes: u64::MAX,
                        max_bytes: MAX_DOCUMENT_BYTES,
                    }
                })?;
                if human_start != human_end {
                    plan.segments.push(PlannedSegment::Human {
                        start: human_start,
                        end: human_end,
                    });
                    ensure_revision_segment_budget(plan.segments.len())?;
                }
                reconstructed.extend_from_slice(&new_bytes[new_start..new_end]);
            }
            DiffTag::Delete => {}
        }
    }
    if suffix_length != 0 {
        plan.segments.extend(
            slice_segments(source_segments, old_middle_end, old.len())?
                .into_iter()
                .map(PlannedSegment::Existing),
        );
        ensure_revision_segment_budget(plan.segments.len())?;
        reconstructed.extend_from_slice(&old_bytes[old_middle_end..]);
    }
    if reconstructed != new_bytes {
        return Err(StoreError::CorruptDatabase(
            "UTF-8 edit plan does not reconstruct the edited revision".into(),
        ));
    }
    Ok(plan)
}

fn common_utf8_edges(old: &str, new: &str) -> (usize, usize) {
    let prefix_end = old
        .chars()
        .zip(new.chars())
        .take_while(|(old_character, new_character)| old_character == new_character)
        .map(|(character, _)| character.len_utf8())
        .sum();
    let old_remaining = &old[prefix_end..];
    let new_remaining = &new[prefix_end..];
    let suffix_length = old_remaining
        .chars()
        .rev()
        .zip(new_remaining.chars().rev())
        .take_while(|(old_character, new_character)| old_character == new_character)
        .map(|(character, _)| character.len_utf8())
        .sum();
    (prefix_end, suffix_length)
}

fn ensure_edit_diff_budget(old_middle: &str, new_middle: &str) -> Result<()> {
    let window_bytes = old_middle.len().saturating_add(new_middle.len());
    if window_bytes > MAX_EDIT_DIFF_WINDOW_BYTES {
        return Err(StoreError::EditDiffBudgetExceeded {
            metric: "changed-window bytes",
            actual: u64::try_from(window_bytes).unwrap_or(u64::MAX),
            limit: u64::try_from(MAX_EDIT_DIFF_WINDOW_BYTES).unwrap_or(u64::MAX),
        });
    }
    let character_count = old_middle
        .chars()
        .count()
        .saturating_add(new_middle.chars().count());
    if character_count > MAX_EDIT_DIFF_WINDOW_CHARACTERS {
        return Err(StoreError::EditDiffBudgetExceeded {
            metric: "changed-window characters",
            actual: u64::try_from(character_count).unwrap_or(u64::MAX),
            limit: u64::try_from(MAX_EDIT_DIFF_WINDOW_CHARACTERS).unwrap_or(u64::MAX),
        });
    }
    let character_count = u64::try_from(character_count).unwrap_or(u64::MAX);
    let work = character_count.saturating_mul(character_count);
    if work > MAX_EDIT_DIFF_WORK {
        return Err(StoreError::EditDiffBudgetExceeded {
            metric: "quadratic diff work",
            actual: work,
            limit: MAX_EDIT_DIFF_WORK,
        });
    }
    Ok(())
}

fn ensure_revision_segment_budget(actual: usize) -> Result<()> {
    if actual > MAX_REVISION_SEGMENTS {
        return Err(StoreError::RevisionSegmentLimitExceeded {
            actual,
            limit: MAX_REVISION_SEGMENTS,
        });
    }
    Ok(())
}

fn utf8_offsets(text: &str) -> Vec<usize> {
    text.char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(text.len()))
        .collect()
}

enum PlannedSegment {
    Existing(StoredSegment),
    Human { start: u64, end: u64 },
}

pub(crate) struct EditPlan {
    segments: Vec<PlannedSegment>,
    pub(crate) human_bytes: Vec<u8>,
}

impl EditPlan {
    pub(crate) fn materialize(
        self,
        human_artifact_id: Option<ArtifactId>,
    ) -> Result<Vec<StoredSegment>> {
        self.segments
            .into_iter()
            .map(|segment| match segment {
                PlannedSegment::Existing(segment) => Ok(segment),
                PlannedSegment::Human { start, end } => Ok(StoredSegment {
                    artifact_id: human_artifact_id.ok_or_else(|| {
                        StoreError::CorruptDatabase(
                            "edit plan has human ranges without a human artifact".into(),
                        )
                    })?,
                    start,
                    end,
                    contribution: ContributionKind::Human,
                }),
            })
            .collect()
    }
}

pub(crate) fn slice_segments(
    segments: &[StoredSegment],
    selection_start: usize,
    selection_end: usize,
) -> Result<Vec<StoredSegment>> {
    if selection_start > selection_end {
        return Err(StoreError::InvalidGenerationRange);
    }
    let mut selected = Vec::new();
    let mut document_offset = 0_usize;
    for segment in segments {
        let segment_len = usize::try_from(segment.end.saturating_sub(segment.start))
            .map_err(|_| StoreError::CorruptDatabase("segment length overflow".into()))?;
        let document_end = document_offset
            .checked_add(segment_len)
            .ok_or_else(|| StoreError::CorruptDatabase("revision length overflow".into()))?;
        let overlap_start = selection_start.max(document_offset);
        let overlap_end = selection_end.min(document_end);
        if overlap_start < overlap_end {
            let local_start = u64::try_from(overlap_start - document_offset)
                .map_err(|_| StoreError::CorruptDatabase("segment offset overflow".into()))?;
            let local_end = u64::try_from(overlap_end - document_offset)
                .map_err(|_| StoreError::CorruptDatabase("segment offset overflow".into()))?;
            selected.push(StoredSegment {
                artifact_id: segment.artifact_id,
                start: segment.start + local_start,
                end: segment.start + local_end,
                contribution: segment.contribution,
            });
        }
        document_offset = document_end;
    }
    if selection_end > document_offset {
        return Err(StoreError::InvalidGenerationRange);
    }
    Ok(selected)
}

pub(crate) fn reconstruct_segments(
    store: &ProjectStore,
    segments: &[StoredSegment],
) -> Result<Vec<u8>> {
    let mut reconstructed = Vec::new();
    for segment in segments {
        let blob_id: String = store.connection.query_row(
            "SELECT blob_id FROM artifacts WHERE artifact_id = ?1",
            [segment.artifact_id.to_string()],
            |row| row.get(0),
        )?;
        let bytes = store.read_blob(parse_blob_id(&blob_id)?)?;
        let start = usize::try_from(segment.start)
            .map_err(|_| StoreError::CorruptDatabase("segment start overflow".into()))?;
        let end = usize::try_from(segment.end)
            .map_err(|_| StoreError::CorruptDatabase("segment end overflow".into()))?;
        let slice = bytes.get(start..end).ok_or_else(|| {
            StoreError::CorruptDatabase(format!(
                "segment range is outside artifact {}",
                segment.artifact_id
            ))
        })?;
        std::str::from_utf8(slice).map_err(|_| {
            StoreError::CorruptDatabase(format!(
                "segment range splits UTF-8 in artifact {}",
                segment.artifact_id
            ))
        })?;
        reconstructed.extend_from_slice(slice);
    }
    Ok(reconstructed)
}

pub(crate) fn validate_segment_projection(
    store: &ProjectStore,
    segments: &[StoredSegment],
    expected: &[u8],
) -> Result<()> {
    if reconstruct_segments(store, segments)? != expected {
        return Err(StoreError::CorruptDatabase(
            "revision segments do not reconstruct the revision artifact".into(),
        ));
    }
    Ok(())
}

pub(crate) fn merge_adjacent_segments(segments: &mut Vec<StoredSegment>) {
    let mut merged: Vec<StoredSegment> = Vec::with_capacity(segments.len());
    for segment in segments.drain(..) {
        if let Some(previous) = merged.last_mut()
            && previous.artifact_id == segment.artifact_id
            && previous.contribution == segment.contribution
            && previous.end == segment.start
        {
            previous.end = segment.end;
        } else {
            merged.push(segment);
        }
    }
    *segments = merged;
}

fn parse_contribution(value: &str) -> Result<ContributionKind> {
    match value {
        "generated" => Ok(ContributionKind::Generated),
        "human" => Ok(ContributionKind::Human),
        "mixed" => Ok(ContributionKind::Mixed),
        "source" => Ok(ContributionKind::Source),
        _ => Err(StoreError::CorruptDatabase(format!(
            "invalid contribution kind `{value}`"
        ))),
    }
}

pub(crate) fn document_media_type(kind: DocumentKind) -> &'static str {
    match kind {
        DocumentKind::Hybrid | DocumentKind::Prose => "text/markdown; charset=utf-8",
        DocumentKind::Verse => "text/plain; charset=utf-8",
    }
}

fn first<T: Copy>(values: &[T], label: &str) -> Result<T> {
    values
        .first()
        .copied()
        .ok_or_else(|| StoreError::CorruptDatabase(format!("receipt has no {label} result")))
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

#[derive(Clone, Copy, Debug)]
pub(crate) struct StoredSegment {
    pub(crate) artifact_id: ArtifactId,
    pub(crate) start: u64,
    pub(crate) end: u64,
    pub(crate) contribution: ContributionKind,
}

impl StoredSegment {
    fn into_public(self) -> ProvenanceSegment {
        ProvenanceSegment {
            artifact_id: self.artifact_id,
            byte_range: ByteRange {
                start: self.start,
                end: self.end,
            },
            contribution: self.contribution,
        }
    }
}

pub(crate) struct SourceBoundRevision<'a> {
    pub(crate) command_id: CommandId,
    pub(crate) request_fingerprint: BlobId,
    pub(crate) document_id: DocumentId,
    pub(crate) document_kind: DocumentKind,
    pub(crate) relative_path: &'a str,
    pub(crate) reason: &'a str,
    pub(crate) expected: ActiveRevision,
    pub(crate) outbox_expected_visible_blob_id: BlobId,
    pub(crate) workflow: &'a str,
    pub(crate) operation_kind: OperationKind,
    pub(crate) target_blob_id: BlobId,
    pub(crate) target_byte_len: usize,
    pub(crate) revision_artifact_id: ArtifactId,
    pub(crate) human_artifact_id: Option<ArtifactId>,
    pub(crate) human_blob_id: Option<BlobId>,
    pub(crate) human_byte_len: usize,
    pub(crate) operation_id: OperationId,
    pub(crate) revision_id: RevisionId,
    pub(crate) segments: &'a [StoredSegment],
    pub(crate) receipt: &'a CommandReceipt,
    pub(crate) created_at_ms: i64,
    pub(crate) draft_claim: Option<TransientDraftClaim>,
}
