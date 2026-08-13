use std::path::Path;

use loom_document::DocumentContent;
use loom_types::{
    ArtifactId, BlobId, CommandId, CommandKind, CommandReceipt, OperationId, RevisionId,
    now_unix_ms,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::file_io::read_bounded;
use crate::paths::{inspect_document_path, normalize_document_path};
use crate::provenance::{
    StoredSegment, build_utf8_edit_plan, document_media_type, ensure_bounded_document,
    insert_blob_row, insert_revision_segments, merge_adjacent_segments,
    validate_active_in_transaction, validate_expected_source, validate_segment_projection,
};
use crate::store::{
    ProjectStore, SaveOutcome, VisibleProjectionState, persist_receipt_in, visible_hash_if_present,
};
use crate::{MAX_DOCUMENT_BYTES, Result, StoreError};

const MAX_REASON_BYTES: usize = 4 * 1024;
const RECONCILIATION_WORKFLOW: &str = "external_reconciliation";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExternalReconciliationRequest {
    pub relative_path: String,
    pub expected_active_revision_id: RevisionId,
    pub expected_base_blob_id: BlobId,
    pub expected_visible_blob_id: BlobId,
    pub resolved_content: DocumentContent,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExternalReconciliationOutcome {
    pub save: SaveOutcome,
    pub visible_projection: VisibleProjectionState,
    pub external_snapshot_artifact_id: ArtifactId,
    pub external_import_operation_id: OperationId,
    pub request_fingerprint: BlobId,
    pub replayed: bool,
}

impl ProjectStore {
    /// Durably reconciles one externally changed visible file against the
    /// immutable base returned by `reconciliation_snapshot`.
    ///
    /// The command binds both the base identities and the exact external
    /// visible hash. A missing visible file is deliberately unsupported.
    pub fn reconcile_external_idempotent(
        &mut self,
        command_id: CommandId,
        request: ExternalReconciliationRequest,
    ) -> Result<ExternalReconciliationOutcome> {
        self.reconcile_external_with_boundary(command_id, request, |_| Ok(()))
    }

    #[allow(clippy::too_many_lines)]
    fn reconcile_external_with_boundary<F>(
        &mut self,
        command_id: CommandId,
        request: ExternalReconciliationRequest,
        before_projection_boundary: F,
    ) -> Result<ExternalReconciliationOutcome>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        let ExternalReconciliationRequest {
            relative_path: requested_relative_path,
            expected_active_revision_id,
            expected_base_blob_id,
            expected_visible_blob_id,
            resolved_content,
            reason,
        } = request;
        let started_at_ms = now_unix_ms();
        let relative_path = normalize_document_path(Path::new(&requested_relative_path))?;
        if reason.len() > MAX_REASON_BYTES {
            return Err(StoreError::ReasonTooLong {
                max_bytes: MAX_REASON_BYTES,
            });
        }
        let document_kind = resolved_content.kind();
        let projection = resolved_content.project_visible()?;
        drop(resolved_content);
        ensure_bounded_document(&projection.bytes)?;
        let request_fingerprint = reconciliation_fingerprint(
            &relative_path,
            document_kind,
            &projection.bytes,
            &reason,
            expected_active_revision_id,
            expected_base_blob_id,
            expected_visible_blob_id,
        )?;
        if let Some(outcome) =
            self.replay_external_reconciliation(command_id, request_fingerprint)?
        {
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
        validate_expected_source(active, expected_active_revision_id, expected_base_blob_id)?;
        let visible_path = inspect_document_path(&self.root, &relative_path)?;
        let visible_blob_id = visible_hash_if_present(&visible_path)?
            .ok_or_else(|| StoreError::ExternalVisibleFileDeleted(relative_path.clone()))?;
        if visible_blob_id != expected_visible_blob_id {
            return Err(StoreError::ExternalVisibleBlobMismatch {
                expected: expected_visible_blob_id,
                actual: visible_blob_id,
            });
        }
        let external_bytes = read_bounded(&visible_path, MAX_DOCUMENT_BYTES)?;
        if std::str::from_utf8(&external_bytes).is_err() {
            return Err(StoreError::ExternalVisibleInvalidUtf8(relative_path));
        }

        let base_bytes = self.read_blob(active.blob_id)?;
        let source_segments = self.load_revision_segments(active.revision_id)?;
        validate_segment_projection(self, &source_segments, &base_bytes)?;
        let edit_plan = build_utf8_edit_plan(&source_segments, &base_bytes, &projection.bytes)?;
        let target_blob_id = self.put_blob(&projection.bytes)?;
        let external_blob_id = self.put_blob(&external_bytes)?;
        if external_blob_id != expected_visible_blob_id {
            return Err(StoreError::ExternalVisibleBlobMismatch {
                expected: expected_visible_blob_id,
                actual: external_blob_id,
            });
        }
        let human_blob_id = if edit_plan.human_bytes.is_empty() {
            None
        } else {
            Some(self.put_blob(&edit_plan.human_bytes)?)
        };

        let revision_artifact_id = ArtifactId::new();
        let external_artifact_id = ArtifactId::new();
        let human_artifact_id = human_blob_id.map(|_| ArtifactId::new());
        let merge_operation_id = OperationId::new();
        let import_operation_id = OperationId::new();
        let human_operation_id = human_artifact_id.map(|_| OperationId::new());
        let revision_id = RevisionId::new();
        let created_at_ms = now_unix_ms();
        let mut resulting_artifact_ids = vec![revision_artifact_id, external_artifact_id];
        resulting_artifact_ids.extend(human_artifact_id);
        let mut resulting_operation_ids = vec![merge_operation_id, import_operation_id];
        resulting_operation_ids.extend(human_operation_id);
        let receipt = CommandReceipt {
            command_id,
            command: CommandKind::ReconcileExternal,
            project_id: self.manifest.project_id,
            project_schema_version: self.manifest.schema_version,
            source_revision_id: Some(active.revision_id),
            resulting_artifact_ids,
            resulting_operation_ids,
            resulting_revision_ids: vec![revision_id],
            started_at_ms,
            completed_at_ms: created_at_ms,
        };
        let human_byte_len = edit_plan.human_bytes.len();
        let mut final_segments = edit_plan.materialize(human_artifact_id)?;
        merge_adjacent_segments(&mut final_segments);
        let write = ReconciliationWrite {
            command_id,
            request_fingerprint,
            document_id: document.id,
            document_kind: document.kind,
            relative_path: &relative_path,
            reason: &reason,
            expected_active: active,
            expected_visible_blob_id,
            target_blob_id,
            target_byte_len: projection.bytes.len(),
            external_blob_id,
            external_byte_len: external_bytes.len(),
            human_blob_id,
            human_byte_len,
            revision_artifact_id,
            external_artifact_id,
            human_artifact_id,
            merge_operation_id,
            import_operation_id,
            human_operation_id,
            revision_id,
            segments: &final_segments,
            receipt: &receipt,
            created_at_ms,
        };
        let outbox_id = self.insert_reconciliation(&write)?;
        let Some(outbox_id) = outbox_id else {
            return self
                .replay_external_reconciliation(command_id, request_fingerprint)?
                .ok_or_else(|| {
                    StoreError::CorruptDatabase(
                        "reconciliation vanished after concurrent insertion".into(),
                    )
                });
        };
        let visible_projection = self.settle_outbox_entry_with_boundary(
            outbox_id,
            &relative_path,
            before_projection_boundary,
        );
        Ok(ExternalReconciliationOutcome {
            save: SaveOutcome {
                blob_id: target_blob_id,
                artifact_id: revision_artifact_id,
                operation_id: merge_operation_id,
                revision_id,
                receipt,
            },
            visible_projection,
            external_snapshot_artifact_id: external_artifact_id,
            external_import_operation_id: import_operation_id,
            request_fingerprint,
            replayed: false,
        })
    }

    fn replay_external_reconciliation(
        &mut self,
        command_id: CommandId,
        request_fingerprint: BlobId,
    ) -> Result<Option<ExternalReconciliationOutcome>> {
        let row: Option<(String, String, String)> = self
            .connection
            .query_row(
                "SELECT cr.request_fingerprint, cr.command_kind, r.receipt_json
                 FROM command_requests cr
                 JOIN command_receipts r ON r.command_id = cr.command_id
                 WHERE cr.command_id = ?1",
                [command_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((recorded_fingerprint, command_kind, receipt_json)) = row else {
            return Ok(None);
        };
        if parse_blob_id(&recorded_fingerprint)? != request_fingerprint
            || command_kind != CommandKind::ReconcileExternal.as_str()
        {
            return Err(StoreError::IdempotencyConflict { command_id });
        }
        let receipt: CommandReceipt = serde_json::from_str(&receipt_json)?;
        if receipt.command != CommandKind::ReconcileExternal {
            return Err(StoreError::CorruptDatabase(
                "reconciliation receipt has the wrong command kind".into(),
            ));
        }
        let revision_id = at(&receipt.resulting_revision_ids, 0, "revision")?;
        let revision_artifact_id = at(&receipt.resulting_artifact_ids, 0, "revision artifact")?;
        let external_artifact_id = at(&receipt.resulting_artifact_ids, 1, "external artifact")?;
        let merge_operation_id = at(&receipt.resulting_operation_ids, 0, "merge operation")?;
        let import_operation_id = at(&receipt.resulting_operation_ids, 1, "import operation")?;
        let target_blob_id: String = self.connection.query_row(
            "SELECT blob_id FROM artifacts WHERE artifact_id = ?1",
            [revision_artifact_id.to_string()],
            |row| row.get(0),
        )?;
        let (outbox_id, relative_path): (i64, String) = self.connection.query_row(
            "SELECT outbox_id, relative_path FROM visible_file_outbox WHERE revision_id = ?1",
            [revision_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let visible_projection = self.settle_outbox_entry(outbox_id, &relative_path);
        Ok(Some(ExternalReconciliationOutcome {
            save: SaveOutcome {
                blob_id: parse_blob_id(&target_blob_id)?,
                artifact_id: revision_artifact_id,
                operation_id: merge_operation_id,
                revision_id,
                receipt,
            },
            visible_projection,
            external_snapshot_artifact_id: external_artifact_id,
            external_import_operation_id: import_operation_id,
            request_fingerprint,
            replayed: true,
        }))
    }

    #[allow(clippy::too_many_lines)]
    fn insert_reconciliation(&mut self, write: &ReconciliationWrite<'_>) -> Result<Option<i64>> {
        let root = self.root.clone();
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
        validate_active_in_transaction(&transaction, write.document_id, write.expected_active)?;
        let visible_path = inspect_document_path(&root, write.relative_path)?;
        let actual_visible = visible_hash_if_present(&visible_path)?
            .ok_or_else(|| StoreError::ExternalVisibleFileDeleted(write.relative_path.into()))?;
        if actual_visible != write.expected_visible_blob_id {
            return Err(StoreError::ExternalVisibleBlobMismatch {
                expected: write.expected_visible_blob_id,
                actual: actual_visible,
            });
        }

        for (blob_id, byte_len) in [
            (write.target_blob_id, write.target_byte_len),
            (write.external_blob_id, write.external_byte_len),
        ] {
            insert_blob_row(&transaction, blob_id, byte_len, write.created_at_ms)?;
        }
        if let Some(human_blob_id) = write.human_blob_id {
            insert_blob_row(
                &transaction,
                human_blob_id,
                write.human_byte_len,
                write.created_at_ms,
            )?;
        }
        insert_reconciliation_artifacts(&transaction, write)?;
        insert_reconciliation_operations(&transaction, write)?;
        transaction.execute(
            "INSERT INTO revisions(revision_id, document_id, parent_revision_id, artifact_id, reason, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                write.revision_id.to_string(),
                write.document_id.to_string(),
                write.expected_active.revision_id.to_string(),
                write.revision_artifact_id.to_string(),
                format!("external reconciliation: {}", write.reason),
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
                write.expected_visible_blob_id.to_string(),
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
                CommandKind::ReconcileExternal.as_str(),
                write.created_at_ms,
            ],
        )?;
        transaction.commit()?;
        Ok(Some(outbox_id))
    }
}

fn insert_reconciliation_artifacts(
    transaction: &Transaction<'_>,
    write: &ReconciliationWrite<'_>,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO artifacts(artifact_id, blob_id, artifact_kind, media_type, metadata_json, created_at_ms)
         VALUES (?1, ?2, 'document_revision', ?3, ?4, ?5)",
        params![
            write.revision_artifact_id.to_string(),
            write.target_blob_id.to_string(),
            document_media_type(write.document_kind),
            serde_json::to_string(&json!({
                "workflow": RECONCILIATION_WORKFLOW,
                "relative_path": write.relative_path,
                "reason": write.reason,
                "base_revision_id": write.expected_active.revision_id,
                "base_blob_id": write.expected_active.blob_id,
                "external_visible_blob_id": write.expected_visible_blob_id,
            }))?,
            write.created_at_ms,
        ],
    )?;
    transaction.execute(
        "INSERT INTO artifacts(artifact_id, blob_id, artifact_kind, media_type, metadata_json, created_at_ms)
         VALUES (?1, ?2, 'human_contribution', ?3, ?4, ?5)",
        params![
            write.external_artifact_id.to_string(),
            write.external_blob_id.to_string(),
            document_media_type(write.document_kind),
            serde_json::to_string(&json!({
                "workflow": RECONCILIATION_WORKFLOW,
                "source": "external_visible_snapshot",
                "relative_path": write.relative_path,
            }))?,
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
                    "workflow": RECONCILIATION_WORKFLOW,
                    "source": "resolved_delta_against_base",
                    "reason": write.reason,
                }))?,
                write.created_at_ms,
            ],
        )?;
    }
    Ok(())
}

fn insert_reconciliation_operations(
    transaction: &Transaction<'_>,
    write: &ReconciliationWrite<'_>,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO operations(operation_id, operation_kind, metadata_json, created_at_ms)
         VALUES (?1, 'import', ?2, ?3)",
        params![
            write.import_operation_id.to_string(),
            serde_json::to_string(&json!({
                "workflow": RECONCILIATION_WORKFLOW,
                "source": "external_visible_file",
                "relative_path": write.relative_path,
                "external_visible_blob_id": write.expected_visible_blob_id,
            }))?,
            write.created_at_ms,
        ],
    )?;
    transaction.execute(
        "INSERT INTO operation_outputs(operation_id, position, artifact_id) VALUES (?1, 0, ?2)",
        params![
            write.import_operation_id.to_string(),
            write.external_artifact_id.to_string()
        ],
    )?;
    if let (Some(human_operation_id), Some(human_artifact_id)) =
        (write.human_operation_id, write.human_artifact_id)
    {
        transaction.execute(
            "INSERT INTO operations(operation_id, operation_kind, metadata_json, created_at_ms)
             VALUES (?1, 'human_edit', ?2, ?3)",
            params![
                human_operation_id.to_string(),
                serde_json::to_string(&json!({
                    "workflow": RECONCILIATION_WORKFLOW,
                    "source": "resolved_delta_against_base",
                    "reason": write.reason,
                }))?,
                write.created_at_ms,
            ],
        )?;
        for (position, input) in [
            write.expected_active.artifact_id,
            write.external_artifact_id,
        ]
        .into_iter()
        .enumerate()
        {
            insert_operation_edge(
                transaction,
                "operation_inputs",
                human_operation_id,
                position,
                input,
            )?;
        }
        insert_operation_edge(
            transaction,
            "operation_outputs",
            human_operation_id,
            0,
            human_artifact_id,
        )?;
    }
    transaction.execute(
        "INSERT INTO operations(operation_id, operation_kind, metadata_json, created_at_ms)
         VALUES (?1, 'merge', ?2, ?3)",
        params![
            write.merge_operation_id.to_string(),
            serde_json::to_string(&json!({
                "workflow": RECONCILIATION_WORKFLOW,
                "reason": write.reason,
                "base_revision_id": write.expected_active.revision_id,
                "base_blob_id": write.expected_active.blob_id,
                "external_visible_blob_id": write.expected_visible_blob_id,
                "target_blob_id": write.target_blob_id,
            }))?,
            write.created_at_ms,
        ],
    )?;
    let mut merge_inputs = vec![
        write.expected_active.artifact_id,
        write.external_artifact_id,
    ];
    merge_inputs.extend(write.human_artifact_id);
    for (position, input) in merge_inputs.into_iter().enumerate() {
        insert_operation_edge(
            transaction,
            "operation_inputs",
            write.merge_operation_id,
            position,
            input,
        )?;
    }
    insert_operation_edge(
        transaction,
        "operation_outputs",
        write.merge_operation_id,
        0,
        write.revision_artifact_id,
    )?;
    Ok(())
}

fn insert_operation_edge(
    transaction: &Transaction<'_>,
    table: &'static str,
    operation_id: OperationId,
    position: usize,
    artifact_id: ArtifactId,
) -> Result<()> {
    let sql = match table {
        "operation_inputs" => {
            "INSERT INTO operation_inputs(operation_id, position, artifact_id) VALUES (?1, ?2, ?3)"
        }
        "operation_outputs" => {
            "INSERT INTO operation_outputs(operation_id, position, artifact_id) VALUES (?1, ?2, ?3)"
        }
        _ => {
            return Err(StoreError::CorruptDatabase(
                "unsupported operation edge table".into(),
            ));
        }
    };
    transaction.execute(
        sql,
        params![
            operation_id.to_string(),
            i64::try_from(position)
                .map_err(|_| StoreError::CorruptDatabase("operation position overflow".into()))?,
            artifact_id.to_string(),
        ],
    )?;
    Ok(())
}

fn reconciliation_fingerprint(
    relative_path: &str,
    document_kind: loom_types::DocumentKind,
    resolved_bytes: &[u8],
    reason: &str,
    expected_active_revision_id: RevisionId,
    expected_base_blob_id: BlobId,
    expected_visible_blob_id: BlobId,
) -> Result<BlobId> {
    let canonical = serde_json::to_vec(&json!({
        "protocol": "loom.reconcile-external.v1",
        "relative_path": relative_path,
        "document_kind": document_kind.as_str(),
        "resolved_blob_id": BlobId::digest(resolved_bytes),
        "reason": reason,
        "expected_active_revision_id": expected_active_revision_id,
        "expected_base_blob_id": expected_base_blob_id,
        "expected_visible_blob_id": expected_visible_blob_id,
    }))?;
    Ok(BlobId::digest(&canonical))
}

fn at<T: Copy>(values: &[T], index: usize, label: &str) -> Result<T> {
    values
        .get(index)
        .copied()
        .ok_or_else(|| StoreError::CorruptDatabase(format!("receipt has no {label}")))
}

fn parse_blob_id(value: &str) -> Result<BlobId> {
    value.parse().map_err(|error| {
        StoreError::CorruptDatabase(format!("invalid reconciliation blob id: {error}"))
    })
}

struct ReconciliationWrite<'a> {
    command_id: CommandId,
    request_fingerprint: BlobId,
    document_id: loom_types::DocumentId,
    document_kind: loom_types::DocumentKind,
    relative_path: &'a str,
    reason: &'a str,
    expected_active: crate::store::ActiveRevision,
    expected_visible_blob_id: BlobId,
    target_blob_id: BlobId,
    target_byte_len: usize,
    external_blob_id: BlobId,
    external_byte_len: usize,
    human_blob_id: Option<BlobId>,
    human_byte_len: usize,
    revision_artifact_id: ArtifactId,
    external_artifact_id: ArtifactId,
    human_artifact_id: Option<ArtifactId>,
    merge_operation_id: OperationId,
    import_operation_id: OperationId,
    human_operation_id: Option<OperationId>,
    revision_id: RevisionId,
    segments: &'a [StoredSegment],
    receipt: &'a CommandReceipt,
    created_at_ms: i64,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use loom_document::DocumentContent;
    use loom_types::{CommandKind, ContributionKind, OperationKind};
    use tempfile::tempdir;

    use super::*;

    struct Fixture {
        _directory: tempfile::TempDir,
        store: ProjectStore,
        base: crate::LoadedDocument,
    }

    impl Fixture {
        fn new(text: &str) -> Self {
            let directory = tempdir().expect("temporary project");
            let root = directory.path().join("Novel");
            let (mut store, _) = ProjectStore::initialize(&root, "Novel").expect("initialize");
            store
                .save_document(
                    "manuscript/001.md",
                    DocumentContent::Prose(text.into()),
                    "initial",
                )
                .expect("save base");
            let base = store.read_document("manuscript/001.md").expect("load base");
            Self {
                _directory: directory,
                store,
                base,
            }
        }

        fn set_external(&self, text: &str) -> BlobId {
            fs::write(self.store.root.join("manuscript/001.md"), text)
                .expect("write external text");
            BlobId::digest(text.as_bytes())
        }

        fn request(
            &self,
            external_blob_id: BlobId,
            resolved: &str,
        ) -> ExternalReconciliationRequest {
            ExternalReconciliationRequest {
                relative_path: "manuscript/001.md".into(),
                expected_active_revision_id: self.base.revision_id,
                expected_base_blob_id: self.base.blob_id,
                expected_visible_blob_id: external_blob_id,
                resolved_content: DocumentContent::Prose(resolved.into()),
                reason: "merge external edit".into(),
            }
        }
    }

    #[test]
    fn reconciliation_preserves_base_segments_and_records_explicit_import_merge() {
        let mut fixture = Fixture::new("abc");
        let initial_artifact_id = fixture.base.artifact_id;
        let edited = fixture
            .store
            .save_document_if_source(
                "manuscript/001.md",
                DocumentContent::Prose("aXc".into()),
                "human middle edit",
                fixture.base.revision_id,
                fixture.base.blob_id,
            )
            .expect("create segmented base");
        fixture.base = fixture
            .store
            .read_document("manuscript/001.md")
            .expect("load segmented base");
        let base_provenance = fixture
            .store
            .revision_provenance(edited.save.revision_id)
            .expect("base provenance");
        let preserved_middle = base_provenance
            .segments
            .iter()
            .find(|segment| segment.artifact_id != initial_artifact_id)
            .expect("middle contribution")
            .artifact_id;

        let external_blob_id = fixture.set_external("eXc");
        let snapshot = fixture
            .store
            .reconciliation_snapshot("manuscript/001.md")
            .expect("reconciliation snapshot");
        assert_eq!(snapshot.active_revision_id, fixture.base.revision_id);
        assert_eq!(snapshot.active_blob_id, fixture.base.blob_id);
        assert_eq!(
            snapshot.visible.as_ref().expect("external visible").blob_id,
            external_blob_id
        );
        let command_id = CommandId::new();
        let request = fixture.request(external_blob_id, "eX!");
        let outcome = fixture
            .store
            .reconcile_external_idempotent(command_id, request)
            .expect("reconcile external edit");

        assert!(!outcome.replayed);
        assert_eq!(outcome.visible_projection, VisibleProjectionState::Applied);
        assert_eq!(outcome.save.receipt.command, CommandKind::ReconcileExternal);
        assert_eq!(outcome.save.receipt.resulting_operation_ids.len(), 3);
        assert_eq!(
            fixture
                .store
                .read_document("manuscript/001.md")
                .expect("read reconciled document")
                .text,
            "eX!"
        );
        let provenance = fixture
            .store
            .revision_provenance(outcome.save.revision_id)
            .expect("reconciled provenance");
        assert!(provenance.segments.iter().any(|segment| {
            segment.artifact_id == preserved_middle
                && segment.contribution == ContributionKind::Human
        }));
        let external_artifact_blob: String = fixture
            .store
            .connection
            .query_row(
                "SELECT blob_id FROM artifacts WHERE artifact_id = ?1",
                [outcome.external_snapshot_artifact_id.to_string()],
                |row| row.get(0),
            )
            .expect("external artifact blob");
        assert_eq!(external_artifact_blob, external_blob_id.to_string());
        let (operation_kind, metadata): (String, String) = fixture
            .store
            .connection
            .query_row(
                "SELECT operation_kind, metadata_json FROM operations WHERE operation_id = ?1",
                [outcome.save.operation_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("merge operation metadata");
        assert_eq!(operation_kind, OperationKind::Merge.as_str());
        assert!(metadata.contains(RECONCILIATION_WORKFLOW));
        assert!(metadata.contains(&external_blob_id.to_string()));
    }

    #[test]
    fn exact_retry_replays_one_revision_and_different_request_conflicts() {
        let mut fixture = Fixture::new("base");
        let external_blob_id = fixture.set_external("external");
        let command_id = CommandId::new();
        let request = fixture.request(external_blob_id, "resolved");
        let first = fixture
            .store
            .reconcile_external_idempotent(command_id, request.clone())
            .expect("first reconciliation");
        let counts = fixture.store.counts().expect("counts after first write");

        fixture
            .store
            .connection
            .execute(
                "UPDATE visible_file_outbox SET state = 'pending', completed_at_ms = NULL
                 WHERE revision_id = ?1",
                [first.save.revision_id.to_string()],
            )
            .expect("simulate crash before outbox acknowledgement");
        fs::write(fixture.store.root.join("manuscript/001.md"), "external")
            .expect("restore bound external predecessor");
        let replay = fixture
            .store
            .reconcile_external_idempotent(command_id, request)
            .expect("replay reconciliation");
        assert!(replay.replayed);
        assert_eq!(replay.visible_projection, VisibleProjectionState::Applied);
        assert_eq!(replay.save.revision_id, first.save.revision_id);
        assert_eq!(
            replay.external_snapshot_artifact_id,
            first.external_snapshot_artifact_id
        );
        assert_eq!(fixture.store.counts().expect("counts after replay"), counts);
        assert_eq!(
            fs::read_to_string(fixture.store.root.join("manuscript/001.md"))
                .expect("replayed visible text"),
            "resolved"
        );

        let mismatch = fixture.store.reconcile_external_idempotent(
            command_id,
            ExternalReconciliationRequest {
                reason: "different request".into(),
                ..fixture.request(external_blob_id, "different")
            },
        );
        assert!(matches!(
            mismatch,
            Err(StoreError::IdempotencyConflict { .. })
        ));
    }

    #[test]
    fn second_external_edit_at_projection_boundary_is_never_overwritten() {
        let mut fixture = Fixture::new("base");
        let external_blob_id = fixture.set_external("external one");
        let command_id = CommandId::new();
        let request = fixture.request(external_blob_id, "resolved");
        let result = fixture
            .store
            .reconcile_external_with_boundary(command_id, request.clone(), |visible| {
                fs::write(visible, "external two")?;
                Ok(())
            })
            .expect("committed reconciliation returns typed projection state");
        assert!(matches!(
            &result.visible_projection,
            VisibleProjectionState::PendingConflict {
                relative_path,
                ..
            } if relative_path == "manuscript/001.md"
        ));
        assert_eq!(
            serde_json::to_value(&result.visible_projection).expect("serialize projection state")["status"],
            "pending_conflict"
        );
        assert_eq!(
            fs::read_to_string(fixture.store.root.join("manuscript/001.md"))
                .expect("second external edit"),
            "external two"
        );
        assert_eq!(
            fixture
                .store
                .pending_outbox_count()
                .expect("pending outbox"),
            1
        );
        let receipt = fixture
            .store
            .load_receipt(command_id)
            .expect("load receipt")
            .expect("committed reconciliation receipt");
        let external_artifact_id = receipt.resulting_artifact_ids[1];
        let external_artifact_blob: String = fixture
            .store
            .connection
            .query_row(
                "SELECT blob_id FROM artifacts WHERE artifact_id = ?1",
                [external_artifact_id.to_string()],
                |row| row.get(0),
            )
            .expect("preserved first external blob");
        assert_eq!(external_artifact_blob, external_blob_id.to_string());

        let replay = fixture
            .store
            .reconcile_external_idempotent(command_id, request)
            .expect("replay retains typed pending conflict");
        assert!(replay.replayed);
        assert_eq!(replay.save.revision_id, result.save.revision_id);
        assert!(matches!(
            replay.visible_projection,
            VisibleProjectionState::PendingConflict { .. }
        ));
        assert_eq!(
            fs::read_to_string(fixture.store.root.join("manuscript/001.md"))
                .expect("second edit after replay"),
            "external two"
        );
    }

    #[test]
    fn committed_reconciliation_wraps_projection_failure_as_retryable_state() {
        let mut fixture = Fixture::new("base");
        let external_blob_id = fixture.set_external("external");
        let command_id = CommandId::new();
        let outcome = fixture
            .store
            .reconcile_external_with_boundary(
                command_id,
                fixture.request(external_blob_id, "resolved"),
                |_| Err(std::io::Error::other("injected projection failure").into()),
            )
            .expect("post-commit projection failure is outcome state");

        assert!(matches!(
            &outcome.visible_projection,
            VisibleProjectionState::PendingRetry { error, .. }
                if error.contains("injected projection failure")
        ));
        assert!(
            fixture
                .store
                .load_receipt(command_id)
                .expect("load committed receipt")
                .is_some()
        );
        assert_eq!(
            fixture
                .store
                .pending_outbox_count()
                .expect("pending outbox"),
            1
        );
    }

    #[test]
    fn visible_deletion_and_stale_external_hash_fail_closed_without_history() {
        let mut fixture = Fixture::new("base");
        let counts = fixture.store.counts().expect("initial counts");
        fs::remove_file(fixture.store.root.join("manuscript/001.md")).expect("delete visible file");
        let deleted = fixture.store.reconcile_external_idempotent(
            CommandId::new(),
            fixture.request(BlobId::digest(b"base"), "resolved"),
        );
        assert!(matches!(
            deleted,
            Err(StoreError::ExternalVisibleFileDeleted(_))
        ));
        assert_eq!(
            fixture.store.counts().expect("counts after deletion"),
            counts
        );

        fs::write(fixture.store.root.join("manuscript/001.md"), "changed")
            .expect("restore changed visible file");
        let stale = fixture.store.reconcile_external_idempotent(
            CommandId::new(),
            fixture.request(BlobId::digest(b"stale"), "resolved"),
        );
        assert!(matches!(
            stale,
            Err(StoreError::ExternalVisibleBlobMismatch { .. })
        ));
        assert_eq!(
            fixture.store.counts().expect("counts after stale hash"),
            counts
        );

        let invalid_utf8 = [0xff, 0xfe];
        fs::write(fixture.store.root.join("manuscript/001.md"), invalid_utf8)
            .expect("write invalid UTF-8");
        let invalid_request = fixture.request(BlobId::digest(&invalid_utf8), "resolved");
        let invalid = fixture
            .store
            .reconcile_external_idempotent(CommandId::new(), invalid_request);
        assert!(matches!(
            invalid,
            Err(StoreError::ExternalVisibleInvalidUtf8(_))
        ));
        assert_eq!(
            fixture.store.counts().expect("counts after invalid UTF-8"),
            counts
        );
    }
}
