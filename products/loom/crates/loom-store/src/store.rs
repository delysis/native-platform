use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use loom_document::DocumentContent;
use loom_types::{
    ArtifactId, BlobId, CommandId, CommandKind, CommandReceipt, DocumentId, DocumentKind,
    OperationId, OperationKind, ProjectManifest, RevisionId, now_unix_ms,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::file_io::{
    atomic_install_if_absent, atomic_replace, atomic_replace_private,
    create_private_file_if_absent, hard_link_if_absent, read_bounded, sync_parent,
};
use crate::paths::{
    ensure_directory, ensure_document_parent, ensure_private_directory, inspect_document_path,
    normalize_document_path, reject_symlink_target,
};
use crate::schema::{CURRENT_SCHEMA_VERSION, configure, migrate};
use crate::{Result, StoreError};

const PROJECT_FORMAT: &str = "loom-project";
const DATABASE_FILE: &str = "loom.sqlite3";
const MANIFEST_FILE: &str = "project.json";
const MAX_PROJECT_NAME_BYTES: usize = 512;
const MAX_REASON_BYTES: usize = 4 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
pub const MAX_DOCUMENT_BYTES: u64 = 128 * 1024 * 1024;

pub struct ProjectStore {
    pub(crate) root: PathBuf,
    pub(crate) manifest: ProjectManifest,
    pub(crate) connection: Connection,
}

impl fmt::Debug for ProjectStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectStore")
            .field("root", &self.root)
            .field("manifest", &self.manifest)
            .finish_non_exhaustive()
    }
}

impl ProjectStore {
    pub fn initialize(
        path: impl AsRef<Path>,
        name: impl Into<String>,
    ) -> Result<(Self, CommandReceipt)> {
        let requested_root = path.as_ref();
        reject_root_symlink(requested_root)?;
        fs::create_dir_all(requested_root)?;
        let root = requested_root.canonicalize()?;

        let name = name.into();
        if name.trim().is_empty() || name.len() > MAX_PROJECT_NAME_BYTES {
            return Err(StoreError::InvalidProjectName {
                max_bytes: MAX_PROJECT_NAME_BYTES,
            });
        }

        let loom_dir = root.join(".loom");
        let manifest_path = loom_dir.join(MANIFEST_FILE);
        if manifest_path.exists() {
            return Err(StoreError::AlreadyInitialized(root));
        }

        for directory in [
            root.join("manuscript"),
            root.join("sources"),
            root.join("assets"),
        ] {
            ensure_directory(&directory)?;
        }
        for directory in [
            loom_dir.clone(),
            loom_dir.join("blobs"),
            loom_dir.join("blobs/sha256"),
            loom_dir.join("indexes"),
            loom_dir.join("backups"),
        ] {
            ensure_private_directory(&directory)?;
        }

        let started_at_ms = now_unix_ms();
        let manifest = ProjectManifest {
            format: PROJECT_FORMAT.to_owned(),
            schema_version: CURRENT_SCHEMA_VERSION,
            project_id: loom_types::ProjectId::new(),
            name,
            created_at_ms: started_at_ms,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
        atomic_replace_private(&manifest_path, &manifest_bytes)?;

        let mut store = Self::open_internal(root, manifest)?;
        let receipt =
            store.new_receipt(CommandKind::InitProject, started_at_ms, None, &[], &[], &[]);
        store.persist_receipt(&receipt)?;
        Ok((store, receipt))
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let requested_root = path.as_ref();
        reject_root_symlink(requested_root)?;
        let root = requested_root.canonicalize()?;
        if !root.is_dir() {
            return Err(StoreError::NotDirectory(root));
        }

        let loom_dir = root.join(".loom");
        if !loom_dir.exists() {
            return Err(StoreError::NotAProject(root));
        }
        ensure_private_directory(&loom_dir)?;
        let manifest_path = loom_dir.join(MANIFEST_FILE);
        reject_symlink_target(&manifest_path)?;
        if !manifest_path.exists() {
            return Err(StoreError::NotAProject(root));
        }
        let manifest: ProjectManifest =
            serde_json::from_slice(&read_bounded(&manifest_path, MAX_MANIFEST_BYTES)?)?;
        validate_manifest(&manifest)?;
        Self::open_internal(root, manifest)
    }

    fn open_internal(root: PathBuf, manifest: ProjectManifest) -> Result<Self> {
        let loom_dir = root.join(".loom");
        for directory in [
            loom_dir.clone(),
            loom_dir.join("blobs"),
            loom_dir.join("blobs/sha256"),
            loom_dir.join("indexes"),
            loom_dir.join("backups"),
        ] {
            ensure_private_directory(&directory)?;
        }
        let database_path = loom_dir.join(DATABASE_FILE);
        create_private_file_if_absent(&database_path)?;
        let mut connection = Connection::open(&database_path)?;
        configure(&connection)?;
        migrate(&mut connection)?;
        Ok(Self {
            root,
            manifest,
            connection,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub const fn manifest(&self) -> &ProjectManifest {
        &self.manifest
    }

    pub fn database_path(&self) -> PathBuf {
        self.root.join(".loom").join(DATABASE_FILE)
    }

    pub fn record_open(&mut self) -> Result<CommandReceipt> {
        let started_at_ms = now_unix_ms();
        let receipt =
            self.new_receipt(CommandKind::OpenProject, started_at_ms, None, &[], &[], &[]);
        self.persist_receipt(&receipt)?;
        Ok(receipt)
    }

    pub fn record_close(&mut self) -> Result<CommandReceipt> {
        let started_at_ms = now_unix_ms();
        let receipt = self.new_receipt(
            CommandKind::CloseProject,
            started_at_ms,
            None,
            &[],
            &[],
            &[],
        );
        self.persist_receipt(&receipt)?;
        Ok(receipt)
    }

    pub fn create_document_if_absent(
        &mut self,
        relative_path: impl AsRef<Path>,
        content: DocumentContent,
        reason: impl Into<String>,
    ) -> Result<SaveOutcome> {
        self.create_document_if_absent_with_boundary(relative_path, content, reason, |_| Ok(()))
    }

    #[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
    fn create_document_if_absent_with_boundary<F>(
        &mut self,
        relative_path: impl AsRef<Path>,
        content: DocumentContent,
        reason: impl Into<String>,
        before_projection_boundary: F,
    ) -> Result<SaveOutcome>
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
        if self.document_by_path(&relative_path)?.is_some() {
            return Err(StoreError::DocumentAlreadyExists(relative_path));
        }
        let visible_path = ensure_document_parent(&self.root, &relative_path)?;
        if visible_hash_if_present(&visible_path)?.is_some() {
            return Err(StoreError::VisibleFileAlreadyExists(relative_path));
        }
        let document_kind = content.kind();
        let projection = content.project_visible()?;
        drop(content);
        let byte_len = u64::try_from(projection.bytes.len()).unwrap_or(u64::MAX);
        if byte_len > MAX_DOCUMENT_BYTES {
            return Err(StoreError::DocumentTooLarge {
                actual_bytes: byte_len,
                max_bytes: MAX_DOCUMENT_BYTES,
            });
        }
        let blob_id = self.put_blob(&projection.bytes)?;
        let document_id = DocumentId::new();
        let artifact_id = ArtifactId::new();
        let operation_id = OperationId::new();
        let revision_id = RevisionId::new();
        let command_id = CommandId::new();
        let created_at_ms = now_unix_ms();
        let receipt = CommandReceipt {
            command_id,
            command: CommandKind::CreateDocument,
            project_id: self.manifest.project_id,
            project_schema_version: self.manifest.schema_version,
            source_revision_id: None,
            resulting_artifact_ids: vec![artifact_id],
            resulting_operation_ids: vec![operation_id],
            resulting_revision_ids: vec![revision_id],
            started_at_ms,
            completed_at_ms: created_at_ms,
        };
        let byte_len_i64 = i64::try_from(byte_len).map_err(|_| StoreError::DocumentTooLarge {
            actual_bytes: byte_len,
            max_bytes: MAX_DOCUMENT_BYTES,
        })?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if transaction
            .query_row(
                "SELECT 1 FROM documents WHERE relative_path = ?1",
                [&relative_path],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some()
        {
            return Err(StoreError::DocumentAlreadyExists(relative_path));
        }
        transaction.execute(
            "INSERT OR IGNORE INTO blobs(blob_id, byte_len, media_type, created_at_ms)
             VALUES (?1, ?2, 'application/octet-stream', ?3)",
            params![blob_id.to_string(), byte_len_i64, created_at_ms],
        )?;
        transaction.execute(
            "INSERT INTO documents(document_id, relative_path, document_kind, created_at_ms)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                document_id.to_string(),
                relative_path,
                document_kind.as_str(),
                created_at_ms
            ],
        )?;
        transaction.execute(
            "INSERT INTO artifacts(artifact_id, blob_id, artifact_kind, media_type, metadata_json, created_at_ms)
             VALUES (?1, ?2, 'human_contribution', ?3, ?4, ?5)",
            params![
                artifact_id.to_string(),
                blob_id.to_string(),
                media_type(document_kind),
                serde_json::to_string(&json!({
                    "relative_path": relative_path,
                    "reason": reason,
                }))?,
                created_at_ms,
            ],
        )?;
        transaction.execute(
            "INSERT INTO operations(operation_id, operation_kind, metadata_json, created_at_ms)
             VALUES (?1, 'import', ?2, ?3)",
            params![
                operation_id.to_string(),
                serde_json::to_string(&json!({
                    "relative_path": relative_path,
                    "reason": reason,
                    "create_if_absent": true,
                }))?,
                created_at_ms,
            ],
        )?;
        transaction.execute(
            "INSERT INTO operation_outputs(operation_id, position, artifact_id) VALUES (?1, 0, ?2)",
            params![operation_id.to_string(), artifact_id.to_string()],
        )?;
        transaction.execute(
            "INSERT INTO revisions(revision_id, document_id, parent_revision_id, artifact_id, reason, created_at_ms)
             VALUES (?1, ?2, NULL, ?3, ?4, ?5)",
            params![
                revision_id.to_string(),
                document_id.to_string(),
                artifact_id.to_string(),
                reason,
                created_at_ms,
            ],
        )?;
        if byte_len_i64 != 0 {
            transaction.execute(
                "INSERT INTO revision_segments(revision_id, position, artifact_id, start_byte, end_byte, contribution_kind)
                 VALUES (?1, 0, ?2, 0, ?3, 'human')",
                params![revision_id.to_string(), artifact_id.to_string(), byte_len_i64],
            )?;
        }
        transaction.execute(
            "INSERT INTO visible_file_outbox(revision_id, relative_path, target_blob_id, expected_visible_blob_id, state, created_at_ms)
             VALUES (?1, ?2, ?3, NULL, 'pending', ?4)",
            params![
                revision_id.to_string(),
                relative_path,
                blob_id.to_string(),
                created_at_ms,
            ],
        )?;
        let outbox_id = transaction.last_insert_rowid();
        persist_receipt_in(&transaction, &receipt)?;
        transaction.commit()?;

        match self.process_outbox_entry_with_boundary(outbox_id, before_projection_boundary)? {
            OutboxResult::Applied | OutboxResult::AlreadyApplied => {}
            OutboxResult::Conflict { relative_path } => {
                return Err(StoreError::VisibleFileConflict {
                    outbox_id,
                    path: relative_path,
                });
            }
        }
        Ok(SaveOutcome {
            blob_id,
            artifact_id,
            operation_id,
            revision_id,
            receipt,
        })
    }

    pub fn checkpoint_visible(
        &mut self,
        relative_path: impl AsRef<Path>,
        kind: DocumentKind,
        reason: impl Into<String>,
    ) -> Result<SaveOutcome> {
        let normalized = normalize_document_path(relative_path.as_ref())?;
        let target = inspect_document_path(&self.root, &normalized)?;
        let bytes = read_bounded(&target, MAX_DOCUMENT_BYTES)?;
        let content = DocumentContent::from_visible(kind, bytes)?;
        let reason = reason.into();
        self.save_content(
            &normalized,
            &content,
            &reason,
            CommandKind::Checkpoint,
            OperationKind::HumanEdit,
        )
    }

    // The command boundary takes ownership so callers cannot mutate a submitted draft in parallel.
    #[allow(clippy::needless_pass_by_value)]
    pub fn save_document(
        &mut self,
        relative_path: impl AsRef<Path>,
        content: DocumentContent,
        reason: impl Into<String>,
    ) -> Result<SaveOutcome> {
        let normalized = normalize_document_path(relative_path.as_ref())?;
        let reason = reason.into();
        self.save_content(
            &normalized,
            &content,
            &reason,
            CommandKind::Checkpoint,
            OperationKind::HumanEdit,
        )
    }

    pub fn import_file(
        &mut self,
        source: impl AsRef<Path>,
        relative_path: impl AsRef<Path>,
        kind: DocumentKind,
        reason: impl Into<String>,
    ) -> Result<SaveOutcome> {
        let bytes = read_bounded(source.as_ref(), MAX_DOCUMENT_BYTES)?;
        let content = DocumentContent::from_visible(kind, bytes)?;
        let normalized = normalize_document_path(relative_path.as_ref())?;
        let reason = reason.into();
        self.save_content(
            &normalized,
            &content,
            &reason,
            CommandKind::Import,
            OperationKind::Import,
        )
    }

    // Keep the transaction linear: its statement order is the durability contract.
    #[allow(clippy::too_many_lines)]
    fn save_content(
        &mut self,
        relative_path: &str,
        content: &DocumentContent,
        reason: &str,
        command_kind: CommandKind,
        operation_kind: OperationKind,
    ) -> Result<SaveOutcome> {
        if reason.len() > MAX_REASON_BYTES {
            return Err(StoreError::ReasonTooLong {
                max_bytes: MAX_REASON_BYTES,
            });
        }
        let started_at_ms = now_unix_ms();
        let projection = content.project_visible()?;
        let byte_len = u64::try_from(projection.bytes.len()).unwrap_or(u64::MAX);
        if byte_len > MAX_DOCUMENT_BYTES {
            return Err(StoreError::DocumentTooLarge {
                actual_bytes: byte_len,
                max_bytes: MAX_DOCUMENT_BYTES,
            });
        }

        let visible_path = ensure_document_parent(&self.root, relative_path)?;
        let expected_visible_blob_id = visible_hash_if_present(&visible_path)?;
        let blob_id = self.put_blob(&projection.bytes)?;
        let media_type = media_type(content.kind());

        let existing_document = self.document_by_path(relative_path)?;
        if let Some(existing) = &existing_document
            && existing.kind != content.kind()
        {
            return Err(StoreError::DocumentKindMismatch {
                path: relative_path.to_owned(),
                stored: existing.kind,
                requested: content.kind(),
            });
        }
        let document_id = existing_document
            .as_ref()
            .map_or_else(DocumentId::new, |document| document.id);
        let active = existing_document
            .as_ref()
            .map(|_| self.active_revision(document_id))
            .transpose()?
            .flatten();

        let artifact_id = ArtifactId::new();
        let operation_id = OperationId::new();
        let revision_id = RevisionId::new();
        let created_at_ms = now_unix_ms();
        let byte_len_i64 = i64::try_from(byte_len).map_err(|_| StoreError::DocumentTooLarge {
            actual_bytes: byte_len,
            max_bytes: MAX_DOCUMENT_BYTES,
        })?;

        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT OR IGNORE INTO blobs(blob_id, byte_len, media_type, created_at_ms) VALUES (?1, ?2, 'application/octet-stream', ?3)",
            params![blob_id.to_string(), byte_len_i64, created_at_ms],
        )?;
        if existing_document.is_none() {
            transaction.execute(
                "INSERT INTO documents(document_id, relative_path, document_kind, created_at_ms) VALUES (?1, ?2, ?3, ?4)",
                params![document_id.to_string(), relative_path, content.kind().as_str(), created_at_ms],
            )?;
        }

        let artifact_metadata = serde_json::to_string(&json!({
            "relative_path": relative_path,
            "reason": &reason,
        }))?;
        transaction.execute(
            "INSERT INTO artifacts(artifact_id, blob_id, artifact_kind, media_type, metadata_json, created_at_ms) VALUES (?1, ?2, 'human_contribution', ?3, ?4, ?5)",
            params![
                artifact_id.to_string(),
                blob_id.to_string(),
                media_type,
                artifact_metadata,
                created_at_ms,
            ],
        )?;

        let operation_metadata = serde_json::to_string(&json!({
            "relative_path": relative_path,
            "reason": &reason,
        }))?;
        transaction.execute(
            "INSERT INTO operations(operation_id, operation_kind, metadata_json, created_at_ms) VALUES (?1, ?2, ?3, ?4)",
            params![
                operation_id.to_string(),
                operation_kind.as_str(),
                operation_metadata,
                created_at_ms,
            ],
        )?;
        if let Some(active) = &active {
            transaction.execute(
                "INSERT INTO operation_inputs(operation_id, position, artifact_id) VALUES (?1, 0, ?2)",
                params![operation_id.to_string(), active.artifact_id.to_string()],
            )?;
        }
        transaction.execute(
            "INSERT INTO operation_outputs(operation_id, position, artifact_id) VALUES (?1, 0, ?2)",
            params![operation_id.to_string(), artifact_id.to_string()],
        )?;
        transaction.execute(
            "INSERT INTO revisions(revision_id, document_id, parent_revision_id, artifact_id, reason, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                revision_id.to_string(),
                document_id.to_string(),
                active.as_ref().map(|active| active.revision_id.to_string()),
                artifact_id.to_string(),
                reason,
                created_at_ms,
            ],
        )?;
        if byte_len_i64 != 0 {
            transaction.execute(
                "INSERT INTO revision_segments(revision_id, position, artifact_id, start_byte, end_byte, contribution_kind) VALUES (?1, 0, ?2, 0, ?3, 'human')",
                params![revision_id.to_string(), artifact_id.to_string(), byte_len_i64],
            )?;
        }
        transaction.execute(
            "INSERT INTO visible_file_outbox(revision_id, relative_path, target_blob_id, expected_visible_blob_id, state, created_at_ms) VALUES (?1, ?2, ?3, ?4, 'pending', ?5)",
            params![
                revision_id.to_string(),
                relative_path,
                blob_id.to_string(),
                expected_visible_blob_id.map(|id| id.to_string()),
                created_at_ms,
            ],
        )?;
        let outbox_id = transaction.last_insert_rowid();
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

        let receipt = self.new_receipt(
            command_kind,
            started_at_ms,
            active.as_ref().map(|active| active.revision_id),
            &[artifact_id],
            &[operation_id],
            &[revision_id],
        );
        self.persist_receipt(&receipt)?;
        Ok(SaveOutcome {
            blob_id,
            artifact_id,
            operation_id,
            revision_id,
            receipt,
        })
    }

    pub fn export_document(
        &mut self,
        relative_path: impl AsRef<Path>,
        destination: impl AsRef<Path>,
    ) -> Result<CommandReceipt> {
        let started_at_ms = now_unix_ms();
        let normalized = normalize_document_path(relative_path.as_ref())?;
        let document = self
            .document_by_path(&normalized)?
            .ok_or_else(|| StoreError::NoActiveRevision(normalized.clone()))?;
        let active = self
            .active_revision(document.id)?
            .ok_or_else(|| StoreError::NoActiveRevision(normalized.clone()))?;
        let source = inspect_document_path(&self.root, &normalized)?;
        let bytes = read_bounded(&source, MAX_DOCUMENT_BYTES)?;
        if BlobId::digest(&bytes) != active.blob_id {
            return Err(StoreError::UncheckpointedVisibleChange(normalized));
        }

        if let Some(parent) = destination.as_ref().parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        atomic_replace(destination.as_ref(), &bytes)?;

        let operation_id = OperationId::new();
        let created_at_ms = now_unix_ms();
        let receipt = self.new_receipt(
            CommandKind::Export,
            started_at_ms,
            Some(active.revision_id),
            &[],
            &[operation_id],
            &[],
        );
        let metadata = serde_json::to_string(&json!({
            "destination": destination.as_ref().to_string_lossy(),
            "relative_path": normalized,
        }))?;
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO operations(operation_id, operation_kind, metadata_json, created_at_ms) VALUES (?1, 'export', ?2, ?3)",
            params![operation_id.to_string(), metadata, created_at_ms],
        )?;
        transaction.execute(
            "INSERT INTO operation_inputs(operation_id, position, artifact_id) VALUES (?1, 0, ?2)",
            params![operation_id.to_string(), active.artifact_id.to_string()],
        )?;
        persist_receipt_in(&transaction, &receipt)?;
        transaction.commit()?;
        Ok(receipt)
    }

    pub fn recover(&mut self) -> Result<RecoveryReport> {
        let started_at_ms = now_unix_ms();
        let outbox_ids = {
            let mut statement = self.connection.prepare(
                "SELECT outbox_id FROM visible_file_outbox WHERE state = 'pending' ORDER BY outbox_id",
            )?;
            statement
                .query_map([], |row| row.get::<_, i64>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };

        let mut applied = 0_usize;
        let mut already_applied = 0_usize;
        let mut conflicts = Vec::new();
        for outbox_id in outbox_ids {
            match self.process_outbox_entry(outbox_id)? {
                OutboxResult::Applied => applied += 1,
                OutboxResult::AlreadyApplied => already_applied += 1,
                OutboxResult::Conflict { relative_path } => conflicts.push(RecoveryConflict {
                    outbox_id,
                    relative_path,
                }),
            }
        }

        let receipt = self.new_receipt(CommandKind::Recover, started_at_ms, None, &[], &[], &[]);
        self.persist_receipt(&receipt)?;
        Ok(RecoveryReport {
            applied,
            already_applied,
            conflicts,
            receipt,
        })
    }

    pub fn list_documents(&self) -> Result<Vec<DocumentSummary>> {
        let mut statement = self.connection.prepare(
            "SELECT d.document_id, d.relative_path, d.document_kind,
                    (SELECT r.revision_id FROM revisions r WHERE r.document_id = d.document_id ORDER BY r.created_at_ms DESC, r.revision_id DESC LIMIT 1)
             FROM documents d ORDER BY d.relative_path",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut documents = Vec::new();
        for row in rows {
            let (document_id, relative_path, kind, revision_id) = row?;
            documents.push(DocumentSummary {
                document_id: parse_id(&document_id, "document_id")?,
                relative_path,
                kind: DocumentKind::from_str(&kind)
                    .map_err(|error| StoreError::CorruptDatabase(error.to_string()))?,
                active_revision_id: revision_id
                    .map(|value| parse_id(&value, "revision_id"))
                    .transpose()?,
            });
        }
        Ok(documents)
    }

    pub fn read_document(&self, relative_path: impl AsRef<Path>) -> Result<LoadedDocument> {
        let normalized = normalize_document_path(relative_path.as_ref())?;
        let document = self
            .document_by_path(&normalized)?
            .ok_or_else(|| StoreError::NoActiveRevision(normalized.clone()))?;
        let active = self
            .active_revision(document.id)?
            .ok_or_else(|| StoreError::NoActiveRevision(normalized.clone()))?;
        let visible_path = inspect_document_path(&self.root, &normalized)?;
        let bytes = read_bounded(&visible_path, MAX_DOCUMENT_BYTES)?;
        if BlobId::digest(&bytes) != active.blob_id {
            return Err(StoreError::UncheckpointedVisibleChange(normalized));
        }
        let text = String::from_utf8(bytes).map_err(loom_document::DocumentError::from)?;
        Ok(LoadedDocument {
            document_id: document.id,
            relative_path: normalized,
            kind: document.kind,
            revision_id: active.revision_id,
            artifact_id: active.artifact_id,
            blob_id: active.blob_id,
            text,
        })
    }

    /// Captures both sides of an external-edit reconciliation without changing
    /// the database, visible manuscript, outbox, or draft journal.
    pub fn reconciliation_snapshot(
        &self,
        relative_path: impl AsRef<Path>,
    ) -> Result<DocumentReconciliationSnapshot> {
        let relative_path = normalize_document_path(relative_path.as_ref())?;
        let document = self
            .document_by_path(&relative_path)?
            .ok_or_else(|| StoreError::NoActiveRevision(relative_path.clone()))?;
        let active = self
            .active_revision(document.id)?
            .ok_or_else(|| StoreError::NoActiveRevision(relative_path.clone()))?;
        let base_bytes = self.read_blob(active.blob_id)?;
        let base_text =
            String::from_utf8(base_bytes).map_err(loom_document::DocumentError::from)?;
        let visible_path = inspect_document_path(&self.root, &relative_path)?;
        let visible = if visible_path.exists() {
            let bytes = read_bounded(&visible_path, MAX_DOCUMENT_BYTES)?;
            let blob_id = BlobId::digest(&bytes);
            let text = String::from_utf8(bytes).map_err(loom_document::DocumentError::from)?;
            Some(VisibleDocumentSnapshot { blob_id, text })
        } else {
            None
        };
        let visible_matches_active = visible
            .as_ref()
            .is_some_and(|visible| visible.blob_id == active.blob_id);
        Ok(DocumentReconciliationSnapshot {
            document_id: document.id,
            relative_path,
            kind: document.kind,
            active_revision_id: active.revision_id,
            active_artifact_id: active.artifact_id,
            active_blob_id: active.blob_id,
            base_text,
            visible,
            visible_matches_active,
        })
    }

    pub fn pending_outbox_count(&self) -> Result<u64> {
        let count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM visible_file_outbox WHERE state = 'pending'",
            [],
            |row| row.get(0),
        )?;
        u64::try_from(count).map_err(|_| StoreError::CorruptDatabase("negative row count".into()))
    }

    pub fn counts(&self) -> Result<StoreCounts> {
        Ok(StoreCounts {
            blobs: self.table_count("blobs")?,
            artifacts: self.table_count("artifacts")?,
            operations: self.table_count("operations")?,
            revisions: self.table_count("revisions")?,
            receipts: self.table_count("command_receipts")?,
        })
    }

    pub fn load_receipt(&self, command_id: CommandId) -> Result<Option<CommandReceipt>> {
        let json: Option<String> = self
            .connection
            .query_row(
                "SELECT receipt_json FROM command_receipts WHERE command_id = ?1",
                [command_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        json.map(|json| serde_json::from_str(&json).map_err(StoreError::from))
            .transpose()
    }

    pub fn read_blob(&self, blob_id: BlobId) -> Result<Vec<u8>> {
        let path = self.blob_path(blob_id);
        reject_symlink_target(&path)?;
        if !path.exists() {
            return Err(StoreError::MissingBlob { blob_id, path });
        }
        let bytes = read_bounded(&path, MAX_DOCUMENT_BYTES)?;
        let actual = BlobId::digest(&bytes);
        if actual != blob_id {
            return Err(StoreError::CorruptBlob {
                path,
                expected: blob_id,
                actual,
            });
        }
        Ok(bytes)
    }

    pub(crate) fn put_blob(&self, bytes: &[u8]) -> Result<BlobId> {
        let blob_id = BlobId::digest(bytes);
        let path = self.blob_path(blob_id);
        if path.exists() {
            let existing = self.read_blob(blob_id)?;
            if existing != bytes {
                return Err(StoreError::CorruptBlob {
                    path,
                    expected: blob_id,
                    actual: BlobId::digest(&existing),
                });
            }
            return Ok(blob_id);
        }

        let parent = path
            .parent()
            .ok_or_else(|| StoreError::CorruptDatabase("blob path has no parent".into()))?;
        ensure_private_directory(parent)?;
        atomic_replace_private(&path, bytes)?;
        Ok(blob_id)
    }

    fn blob_path(&self, blob_id: BlobId) -> PathBuf {
        let hash = blob_id.to_hex();
        self.root
            .join(".loom/blobs/sha256")
            .join(&hash[..2])
            .join(&hash[2..])
    }

    pub(crate) fn document_by_path(&self, relative_path: &str) -> Result<Option<DocumentRecord>> {
        let row: Option<(String, String)> = self
            .connection
            .query_row(
                "SELECT document_id, document_kind FROM documents WHERE relative_path = ?1",
                [relative_path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(document_id, kind)| {
            Ok(DocumentRecord {
                id: parse_id(&document_id, "document_id")?,
                kind: DocumentKind::from_str(&kind)
                    .map_err(|error| StoreError::CorruptDatabase(error.to_string()))?,
            })
        })
        .transpose()
    }

    pub(crate) fn active_revision(
        &self,
        document_id: DocumentId,
    ) -> Result<Option<ActiveRevision>> {
        let row: Option<(String, String, String)> = self
            .connection
            .query_row(
                "SELECT r.revision_id, r.artifact_id, a.blob_id
                 FROM revisions r JOIN artifacts a ON a.artifact_id = r.artifact_id
                 WHERE r.document_id = ?1
                 ORDER BY r.created_at_ms DESC, r.revision_id DESC LIMIT 1",
                [document_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        row.map(|(revision_id, artifact_id, blob_id)| {
            Ok(ActiveRevision {
                revision_id: parse_id(&revision_id, "revision_id")?,
                artifact_id: parse_id(&artifact_id, "artifact_id")?,
                blob_id: parse_blob_id(&blob_id)?,
            })
        })
        .transpose()
    }

    pub(crate) fn process_outbox_entry(&mut self, outbox_id: i64) -> Result<OutboxResult> {
        self.process_outbox_entry_with_boundary(outbox_id, |_| Ok(()))
    }

    /// Attempts one durable visible-file projection without losing the
    /// semantic command result when the outbox cannot yet be completed.
    ///
    /// Callers use this only after the revision, receipt, and pending outbox
    /// row have committed. Consequently every projection-side failure is
    /// returned as state attached to that committed outcome, never as an
    /// ambiguous command error.
    pub(crate) fn settle_outbox_entry(
        &mut self,
        outbox_id: i64,
        relative_path: &str,
    ) -> VisibleProjectionState {
        self.settle_outbox_entry_with_boundary(outbox_id, relative_path, |_| Ok(()))
    }

    pub(crate) fn settle_outbox_entry_with_boundary<F>(
        &mut self,
        outbox_id: i64,
        relative_path: &str,
        before_projection_boundary: F,
    ) -> VisibleProjectionState
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        match self.process_outbox_entry_with_boundary(outbox_id, before_projection_boundary) {
            Ok(OutboxResult::Applied | OutboxResult::AlreadyApplied) => {
                VisibleProjectionState::Applied
            }
            Ok(OutboxResult::Conflict { relative_path }) => {
                VisibleProjectionState::PendingConflict {
                    outbox_id,
                    relative_path,
                }
            }
            Err(error) => VisibleProjectionState::PendingRetry {
                outbox_id,
                relative_path: relative_path.to_owned(),
                error: error.to_string(),
            },
        }
    }

    // Keep the crash-state transitions visible in one linear routine.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn process_outbox_entry_with_boundary<F>(
        &mut self,
        outbox_id: i64,
        before_projection_boundary: F,
    ) -> Result<OutboxResult>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        let row: (String, String, String, Option<String>, String) = self.connection.query_row(
            "SELECT revision_id, relative_path, target_blob_id, expected_visible_blob_id, state
             FROM visible_file_outbox WHERE outbox_id = ?1",
            [outbox_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
        let (_revision_id, relative_path, target_blob_id, expected_visible_blob_id, state) = row;
        if state == "completed" {
            return Ok(OutboxResult::AlreadyApplied);
        }
        if state != "pending" {
            return Err(StoreError::CorruptDatabase(format!(
                "outbox {outbox_id} has invalid state `{state}`"
            )));
        }

        let normalized = normalize_document_path(Path::new(&relative_path))?;
        if normalized != relative_path {
            return Err(StoreError::CorruptDatabase(format!(
                "outbox {outbox_id} has noncanonical path"
            )));
        }
        let target_blob_id = parse_blob_id(&target_blob_id)?;
        let expected_visible_blob_id = expected_visible_blob_id
            .map(|value| parse_blob_id(&value))
            .transpose()?;
        let target_bytes = self.read_blob(target_blob_id)?;
        let visible_path = ensure_document_parent(&self.root, &relative_path)?;
        let staging_directory = self.root.join(".loom/backups/outbox");
        ensure_private_directory(&staging_directory)?;
        let previous_path = staging_directory.join(format!("{outbox_id}.previous"));
        let current_visible_blob_id = visible_hash_if_present(&visible_path)?;

        if current_visible_blob_id == Some(target_blob_id) {
            remove_if_present(&previous_path)?;
            self.complete_outbox(outbox_id)?;
            return Ok(OutboxResult::AlreadyApplied);
        }
        if previous_path.exists() {
            let previous_blob_id = visible_hash_if_present(&previous_path)?;
            if previous_blob_id != expected_visible_blob_id {
                if current_visible_blob_id.is_none() {
                    let _ = hard_link_if_absent(&previous_path, &visible_path)?;
                }
                return Ok(OutboxResult::Conflict { relative_path });
            }
            if current_visible_blob_id == expected_visible_blob_id {
                remove_if_present(&previous_path)?;
            } else if current_visible_blob_id.is_some() {
                return Ok(OutboxResult::Conflict { relative_path });
            } else {
                match atomic_install_if_absent(&visible_path, &target_bytes) {
                    Ok(true) => {}
                    Ok(false) => return Ok(OutboxResult::Conflict { relative_path }),
                    Err(error) => {
                        let _ = hard_link_if_absent(&previous_path, &visible_path);
                        return Err(error);
                    }
                }
                if visible_hash_if_present(&visible_path)? != Some(target_blob_id) {
                    return Ok(OutboxResult::Conflict { relative_path });
                }
                remove_if_present(&previous_path)?;
                self.complete_outbox(outbox_id)?;
                return Ok(OutboxResult::Applied);
            }
        }
        if current_visible_blob_id != expected_visible_blob_id {
            return Ok(OutboxResult::Conflict { relative_path });
        }

        before_projection_boundary(&visible_path)?;
        if expected_visible_blob_id.is_some() {
            match fs::rename(&visible_path, &previous_path) {
                Ok(()) => {
                    sync_parent(&visible_path)?;
                    sync_parent(&previous_path)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(OutboxResult::Conflict { relative_path });
                }
                Err(error) => return Err(error.into()),
            }
            let captured_blob_id = visible_hash_if_present(&previous_path)?;
            if captured_blob_id != expected_visible_blob_id {
                // The bytes at the replacement boundary are retained under
                // `.loom/backups/outbox` and restored without clobbering any
                // newer file an external editor may already have installed.
                let _ = hard_link_if_absent(&previous_path, &visible_path)?;
                return Ok(OutboxResult::Conflict { relative_path });
            }
        }
        match atomic_install_if_absent(&visible_path, &target_bytes) {
            Ok(true) => {}
            Ok(false) => return Ok(OutboxResult::Conflict { relative_path }),
            Err(error) => {
                if previous_path.exists() {
                    let _ = hard_link_if_absent(&previous_path, &visible_path);
                }
                return Err(error);
            }
        }
        if visible_hash_if_present(&visible_path)? != Some(target_blob_id) {
            return Ok(OutboxResult::Conflict { relative_path });
        }
        remove_if_present(&previous_path)?;
        self.complete_outbox(outbox_id)?;
        Ok(OutboxResult::Applied)
    }

    fn complete_outbox(&mut self, outbox_id: i64) -> Result<()> {
        let changed = self.connection.execute(
            "UPDATE visible_file_outbox SET state = 'completed', completed_at_ms = ?2 WHERE outbox_id = ?1 AND state = 'pending'",
            params![outbox_id, now_unix_ms()],
        )?;
        if changed > 1 {
            return Err(StoreError::CorruptDatabase(format!(
                "outbox update affected {changed} rows"
            )));
        }
        Ok(())
    }

    pub(crate) fn new_receipt(
        &self,
        command: CommandKind,
        started_at_ms: i64,
        source_revision_id: Option<RevisionId>,
        artifact_ids: &[ArtifactId],
        operation_ids: &[OperationId],
        revision_ids: &[RevisionId],
    ) -> CommandReceipt {
        CommandReceipt {
            command_id: CommandId::new(),
            command,
            project_id: self.manifest.project_id,
            project_schema_version: self.manifest.schema_version,
            source_revision_id,
            resulting_artifact_ids: artifact_ids.to_vec(),
            resulting_operation_ids: operation_ids.to_vec(),
            resulting_revision_ids: revision_ids.to_vec(),
            started_at_ms,
            completed_at_ms: now_unix_ms(),
        }
    }

    pub(crate) fn persist_receipt(&mut self, receipt: &CommandReceipt) -> Result<()> {
        persist_receipt_in(&self.connection, receipt)
    }

    fn table_count(&self, table: &str) -> Result<u64> {
        let sql = match table {
            "artifacts" => "SELECT COUNT(*) FROM artifacts",
            "blobs" => "SELECT COUNT(*) FROM blobs",
            "command_receipts" => "SELECT COUNT(*) FROM command_receipts",
            "operations" => "SELECT COUNT(*) FROM operations",
            "revisions" => "SELECT COUNT(*) FROM revisions",
            _ => {
                return Err(StoreError::CorruptDatabase(
                    "unsupported count table".into(),
                ));
            }
        };
        let count: i64 = self.connection.query_row(sql, [], |row| row.get(0))?;
        u64::try_from(count).map_err(|_| StoreError::CorruptDatabase("negative row count".into()))
    }
}

pub(crate) fn persist_receipt_in(connection: &Connection, receipt: &CommandReceipt) -> Result<()> {
    connection.execute(
        "INSERT INTO command_receipts(command_id, command_kind, receipt_json, completed_at_ms) VALUES (?1, ?2, ?3, ?4)",
        params![
            receipt.command_id.to_string(),
            receipt.command.as_str(),
            serde_json::to_string(receipt)?,
            receipt.completed_at_ms,
        ],
    )?;
    Ok(())
}

fn media_type(kind: DocumentKind) -> &'static str {
    match kind {
        DocumentKind::Hybrid | DocumentKind::Prose => "text/markdown; charset=utf-8",
        DocumentKind::Verse => "text/plain; charset=utf-8",
    }
}

pub(crate) fn visible_hash_if_present(path: &Path) -> Result<Option<BlobId>> {
    reject_symlink_target(path)?;
    if !path.exists() {
        return Ok(None);
    }
    read_bounded(path, MAX_DOCUMENT_BYTES).map(|bytes| Some(BlobId::digest(&bytes)))
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => sync_parent(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn validate_manifest(manifest: &ProjectManifest) -> Result<()> {
    if manifest.format != PROJECT_FORMAT {
        return Err(StoreError::UnsupportedFormat(manifest.format.clone()));
    }
    if manifest.schema_version > CURRENT_SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchema {
            found: manifest.schema_version,
            supported: CURRENT_SCHEMA_VERSION,
        });
    }
    if manifest.name.trim().is_empty() || manifest.name.len() > MAX_PROJECT_NAME_BYTES {
        return Err(StoreError::InvalidProjectName {
            max_bytes: MAX_PROJECT_NAME_BYTES,
        });
    }
    Ok(())
}

fn reject_root_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(StoreError::SymbolicLink(path.to_path_buf()))
        }
        Ok(metadata) if !metadata.is_dir() => Err(StoreError::NotDirectory(path.to_path_buf())),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
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
        .map_err(|error| StoreError::CorruptDatabase(format!("invalid blob_id `{value}`: {error}")))
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DocumentRecord {
    pub(crate) id: DocumentId,
    pub(crate) kind: DocumentKind,
}

#[derive(Clone, Copy, Debug)]
#[allow(clippy::struct_field_names)]
pub(crate) struct ActiveRevision {
    pub(crate) revision_id: RevisionId,
    pub(crate) artifact_id: ArtifactId,
    pub(crate) blob_id: BlobId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SaveOutcome {
    pub blob_id: BlobId,
    pub artifact_id: ArtifactId,
    pub operation_id: OperationId,
    pub revision_id: RevisionId,
    pub receipt: CommandReceipt,
}

/// The visible-file side of a semantically committed save.
///
/// Only `Applied` is a fully acknowledged save. Pending variants retain the
/// immutable revision and receipt while making it explicit that recovery or
/// author reconciliation is still required before the visible manuscript
/// reflects that revision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum VisibleProjectionState {
    Applied,
    PendingConflict {
        outbox_id: i64,
        relative_path: String,
    },
    PendingRetry {
        outbox_id: i64,
        relative_path: String,
        error: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DocumentSummary {
    pub document_id: DocumentId,
    pub relative_path: String,
    pub kind: DocumentKind,
    pub active_revision_id: Option<RevisionId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LoadedDocument {
    pub document_id: DocumentId,
    pub relative_path: String,
    pub kind: DocumentKind,
    pub revision_id: RevisionId,
    pub artifact_id: ArtifactId,
    pub blob_id: BlobId,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VisibleDocumentSnapshot {
    pub blob_id: BlobId,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DocumentReconciliationSnapshot {
    pub document_id: DocumentId,
    pub relative_path: String,
    pub kind: DocumentKind,
    pub active_revision_id: RevisionId,
    pub active_artifact_id: ArtifactId,
    pub active_blob_id: BlobId,
    pub base_text: String,
    pub visible: Option<VisibleDocumentSnapshot>,
    pub visible_matches_active: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RecoveryConflict {
    pub outbox_id: i64,
    pub relative_path: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RecoveryReport {
    pub applied: usize,
    pub already_applied: usize,
    pub conflicts: Vec<RecoveryConflict>,
    pub receipt: CommandReceipt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StoreCounts {
    pub blobs: u64,
    pub artifacts: u64,
    pub operations: u64,
    pub revisions: u64,
    pub receipts: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OutboxResult {
    Applied,
    AlreadyApplied,
    Conflict { relative_path: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn new_store() -> (tempfile::TempDir, ProjectStore) {
        let directory = tempdir().expect("temporary project root");
        let project = directory.path().join("Novel");
        let (store, _) = ProjectStore::initialize(&project, "Novel").expect("initialize project");
        (directory, store)
    }

    #[cfg(unix)]
    fn assert_private_tree(path: &Path) {
        use std::os::unix::fs::PermissionsExt as _;

        let metadata = fs::symlink_metadata(path).expect("sidecar metadata");
        assert!(!metadata.file_type().is_symlink());
        let mode = metadata.permissions().mode() & 0o777;
        if metadata.is_dir() {
            assert_eq!(mode, 0o700, "private directory: {}", path.display());
            for entry in fs::read_dir(path).expect("read private directory") {
                assert_private_tree(&entry.expect("private directory entry").path());
            }
        } else {
            assert!(metadata.is_file(), "sidecar entry: {}", path.display());
            assert_eq!(mode, 0o600, "private file: {}", path.display());
        }
    }

    #[cfg(unix)]
    #[test]
    fn newly_created_sidecar_state_is_private_without_chmodding_manuscript_directories() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempdir().expect("temporary project root");
        let project = directory.path().join("Novel");
        let manuscript = project.join("manuscript");
        fs::create_dir(&project).expect("project directory");
        fs::create_dir(&manuscript).expect("existing manuscript directory");
        fs::set_permissions(&manuscript, fs::Permissions::from_mode(0o750))
            .expect("set manuscript permissions");

        let (mut store, _) =
            ProjectStore::initialize(&project, "Novel").expect("initialize project");
        let saved = store
            .create_document_if_absent(
                "manuscript/001.md",
                DocumentContent::Prose("private beginning\n".into()),
                "initial",
            )
            .expect("create document");
        store
            .upsert_transient_draft(
                "manuscript/001.md",
                saved.revision_id,
                0,
                DocumentContent::Prose("private draft\n".into()),
            )
            .expect("write draft");

        assert_private_tree(&project.join(".loom"));
        assert_eq!(
            fs::metadata(&manuscript)
                .expect("manuscript metadata")
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
    }

    fn insert_pending_projection(
        store: &mut ProjectStore,
        saved: &SaveOutcome,
        target: &[u8],
    ) -> i64 {
        let pending_blob = store.put_blob(target).expect("store pending blob");
        let pending_artifact = ArtifactId::new();
        let pending_operation = OperationId::new();
        let pending_revision = RevisionId::new();
        let now = now_unix_ms();
        store
            .connection
            .execute(
                "INSERT OR IGNORE INTO blobs(blob_id, byte_len, media_type, created_at_ms) VALUES (?1, ?2, 'application/octet-stream', ?3)",
                params![
                    pending_blob.to_string(),
                    i64::try_from(target.len()).expect("target length"),
                    now
                ],
            )
            .expect("insert blob row");
        store
            .connection
            .execute(
                "INSERT INTO artifacts(artifact_id, blob_id, artifact_kind, media_type, metadata_json, created_at_ms) VALUES (?1, ?2, 'human_contribution', 'text/markdown', '{}', ?3)",
                params![pending_artifact.to_string(), pending_blob.to_string(), now],
            )
            .expect("insert artifact");
        store
            .connection
            .execute(
                "INSERT INTO operations(operation_id, operation_kind, metadata_json, created_at_ms) VALUES (?1, 'human_edit', '{}', ?2)",
                params![pending_operation.to_string(), now],
            )
            .expect("insert operation");
        store
            .connection
            .execute(
                "INSERT INTO operation_outputs(operation_id, position, artifact_id) VALUES (?1, 0, ?2)",
                params![pending_operation.to_string(), pending_artifact.to_string()],
            )
            .expect("insert output");
        let document = store
            .document_by_path("manuscript/001.md")
            .expect("query document")
            .expect("document");
        store
            .connection
            .execute(
                "INSERT INTO revisions(revision_id, document_id, parent_revision_id, artifact_id, reason, created_at_ms) VALUES (?1, ?2, ?3, ?4, 'pending', ?5)",
                params![
                    pending_revision.to_string(),
                    document.id.to_string(),
                    saved.revision_id.to_string(),
                    pending_artifact.to_string(),
                    now
                ],
            )
            .expect("insert revision");
        store
            .connection
            .execute(
                "INSERT INTO revision_segments(revision_id, position, artifact_id, start_byte, end_byte, contribution_kind) VALUES (?1, 0, ?2, 0, ?3, 'human')",
                params![
                    pending_revision.to_string(),
                    pending_artifact.to_string(),
                    i64::try_from(target.len()).expect("target length")
                ],
            )
            .expect("insert segment");
        store
            .connection
            .execute(
                "INSERT INTO visible_file_outbox(revision_id, relative_path, target_blob_id, expected_visible_blob_id, state, created_at_ms) VALUES (?1, 'manuscript/001.md', ?2, ?3, 'pending', ?4)",
                params![
                    pending_revision.to_string(),
                    pending_blob.to_string(),
                    saved.blob_id.to_string(),
                    now
                ],
            )
            .expect("insert outbox");
        store.connection.last_insert_rowid()
    }

    #[test]
    fn identical_content_shares_blob_but_not_occurrence() {
        let (_directory, mut store) = new_store();
        let first = store
            .save_document(
                "manuscript/001.md",
                DocumentContent::Prose("same".into()),
                "first checkpoint",
            )
            .expect("first save");
        let second = store
            .save_document(
                "manuscript/001.md",
                DocumentContent::Prose("same".into()),
                "second checkpoint",
            )
            .expect("second save");

        assert_eq!(first.blob_id, second.blob_id);
        assert_ne!(first.artifact_id, second.artifact_id);
        assert_ne!(first.operation_id, second.operation_id);
        assert_ne!(first.revision_id, second.revision_id);
        let counts = store.counts().expect("count store rows");
        assert_eq!(counts.blobs, 1);
        assert_eq!(counts.artifacts, 2);
        assert_eq!(counts.operations, 2);
        assert_eq!(counts.revisions, 2);
    }

    #[test]
    fn sidecar_removal_leaves_manuscript_readable() {
        let (directory, mut store) = new_store();
        let root = store.root().to_path_buf();
        store
            .save_document(
                "manuscript/poems/threshold.txt",
                DocumentContent::Verse("  light\n\nreturns  \n".into()),
                "save poem",
            )
            .expect("save poem");
        drop(store);

        fs::remove_dir_all(root.join(".loom")).expect("remove sidecar");
        let text = fs::read_to_string(root.join("manuscript/poems/threshold.txt"))
            .expect("read visible poem");
        assert_eq!(text, "  light\n\nreturns  \n");
        drop(directory);
    }

    #[test]
    fn database_uses_required_durability_pragmas() {
        let (_directory, store) = new_store();
        let foreign_keys: i64 = store
            .connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("foreign keys pragma");
        let journal_mode: String = store
            .connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("journal mode pragma");
        let synchronous: i64 = store
            .connection
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .expect("synchronous pragma");
        assert_eq!(foreign_keys, 1);
        assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
        assert_eq!(synchronous, 2);
    }

    #[test]
    fn core_history_tables_are_strict() {
        let (_directory, store) = new_store();
        for table in [
            "blobs",
            "artifacts",
            "operations",
            "revisions",
            "model_environments",
            "prompt_recipes",
            "prompt_recipe_inputs",
            "context_recipes",
            "context_recipe_sources",
            "authority_policies",
            "authority_policy_members",
            "branches",
            "generation_runs",
            "generation_events",
            "generation_candidates",
            "generation_terminals",
            "selection_events",
            "authorship_attestations",
            "command_requests",
            "transient_drafts",
        ] {
            let strict: i64 = store
                .connection
                .query_row(
                    "SELECT strict FROM pragma_table_list WHERE schema = 'main' AND name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .expect("strict table metadata");
            assert_eq!(strict, 1, "{table} must be STRICT");
        }
    }

    #[test]
    fn immutable_rows_reject_updates() {
        let (_directory, mut store) = new_store();
        let saved = store
            .save_document(
                "manuscript/001.md",
                DocumentContent::Prose("original".into()),
                "checkpoint",
            )
            .expect("save document");
        let result = store.connection.execute(
            "UPDATE artifacts SET artifact_kind = 'generated_span' WHERE artifact_id = ?1",
            [saved.artifact_id.to_string()],
        );
        assert!(result.is_err());
    }

    #[test]
    fn close_project_has_a_public_receipt() {
        let (_directory, mut store) = new_store();
        let receipt = store.record_close().expect("record close");
        assert_eq!(receipt.command, CommandKind::CloseProject);
        assert_eq!(
            store
                .load_receipt(receipt.command_id)
                .expect("load close receipt"),
            Some(receipt)
        );
    }

    #[test]
    fn recovery_refuses_to_overwrite_external_change() {
        let (_directory, mut store) = new_store();
        let saved = store
            .save_document(
                "manuscript/001.md",
                DocumentContent::Prose("first".into()),
                "checkpoint",
            )
            .expect("save document");
        let visible = store.root.join("manuscript/001.md");
        let pending_blob = store.put_blob(b"pending").expect("store pending blob");
        let pending_artifact = ArtifactId::new();
        let pending_operation = OperationId::new();
        let pending_revision = RevisionId::new();
        let now = now_unix_ms();
        store.connection.execute(
            "INSERT OR IGNORE INTO blobs(blob_id, byte_len, media_type, created_at_ms) VALUES (?1, 7, 'application/octet-stream', ?2)",
            params![pending_blob.to_string(), now],
        ).expect("insert blob row");
        store.connection.execute(
            "INSERT INTO artifacts(artifact_id, blob_id, artifact_kind, media_type, metadata_json, created_at_ms) VALUES (?1, ?2, 'human_contribution', 'text/markdown', '{}', ?3)",
            params![pending_artifact.to_string(), pending_blob.to_string(), now],
        ).expect("insert artifact");
        store.connection.execute(
            "INSERT INTO operations(operation_id, operation_kind, metadata_json, created_at_ms) VALUES (?1, 'human_edit', '{}', ?2)",
            params![pending_operation.to_string(), now],
        ).expect("insert operation");
        store.connection.execute(
            "INSERT INTO operation_outputs(operation_id, position, artifact_id) VALUES (?1, 0, ?2)",
            params![pending_operation.to_string(), pending_artifact.to_string()],
        ).expect("insert output");
        let document = store
            .document_by_path("manuscript/001.md")
            .expect("query document")
            .expect("document");
        store.connection.execute(
            "INSERT INTO revisions(revision_id, document_id, parent_revision_id, artifact_id, reason, created_at_ms) VALUES (?1, ?2, ?3, ?4, 'pending', ?5)",
            params![pending_revision.to_string(), document.id.to_string(), saved.revision_id.to_string(), pending_artifact.to_string(), now],
        ).expect("insert revision");
        store.connection.execute(
            "INSERT INTO revision_segments(revision_id, position, artifact_id, start_byte, end_byte, contribution_kind) VALUES (?1, 0, ?2, 0, 7, 'human')",
            params![pending_revision.to_string(), pending_artifact.to_string()],
        ).expect("insert segment");
        store.connection.execute(
            "INSERT INTO visible_file_outbox(revision_id, relative_path, target_blob_id, expected_visible_blob_id, state, created_at_ms) VALUES (?1, 'manuscript/001.md', ?2, ?3, 'pending', ?4)",
            params![pending_revision.to_string(), pending_blob.to_string(), saved.blob_id.to_string(), now],
        ).expect("insert outbox");

        fs::write(&visible, "external").expect("simulate external edit");
        let report = store.recover().expect("recovery attempt");
        assert_eq!(report.conflicts.len(), 1);
        assert_eq!(
            fs::read_to_string(visible).expect("read visible"),
            "external"
        );
        assert_eq!(store.pending_outbox_count().expect("pending count"), 1);
    }

    #[test]
    fn recovery_finishes_crash_after_visible_replace() {
        let (_directory, mut store) = new_store();
        let saved = store
            .save_document(
                "manuscript/001.md",
                DocumentContent::Prose("durable".into()),
                "checkpoint",
            )
            .expect("save document");
        store
            .connection
            .execute(
                "UPDATE visible_file_outbox SET state = 'pending', completed_at_ms = NULL WHERE revision_id = ?1",
                [saved.revision_id.to_string()],
            )
            .expect("simulate crash before outbox completion");

        let report = store.recover().expect("recover outbox");
        assert_eq!(report.already_applied, 1);
        assert_eq!(report.applied, 0);
        assert!(report.conflicts.is_empty());
        assert_eq!(store.pending_outbox_count().expect("pending count"), 0);
    }

    #[test]
    fn outbox_boundary_race_preserves_external_bytes() {
        let (_directory, mut store) = new_store();
        let saved = store
            .save_document(
                "manuscript/001.md",
                DocumentContent::Prose("first".into()),
                "checkpoint",
            )
            .expect("save document");
        let outbox_id = insert_pending_projection(&mut store, &saved, b"pending");
        let result = store
            .process_outbox_entry_with_boundary(outbox_id, |visible| {
                fs::write(visible, "external at boundary")?;
                Ok(())
            })
            .expect("process boundary race");

        assert_eq!(
            result,
            OutboxResult::Conflict {
                relative_path: "manuscript/001.md".into()
            }
        );
        assert_eq!(
            fs::read_to_string(store.root.join("manuscript/001.md"))
                .expect("read external visible bytes"),
            "external at boundary"
        );
        assert_eq!(
            fs::read_to_string(
                store
                    .root
                    .join(format!(".loom/backups/outbox/{outbox_id}.previous"))
            )
            .expect("read conflict backup"),
            "external at boundary"
        );
        assert_eq!(store.pending_outbox_count().expect("pending count"), 1);
        let recovery = store.recover().expect("conflict remains recoverable");
        assert_eq!(recovery.conflicts.len(), 1);
        assert_eq!(
            fs::read_to_string(store.root.join("manuscript/001.md"))
                .expect("external bytes survive recovery"),
            "external at boundary"
        );
    }

    #[test]
    fn create_if_absent_refuses_file_appearing_at_projection_boundary() {
        let (_directory, mut store) = new_store();
        let result = store.create_document_if_absent_with_boundary(
            "manuscript/new.md",
            DocumentContent::Prose(String::new()),
            "create empty document",
            |visible| {
                fs::write(visible, "appeared externally")?;
                Ok(())
            },
        );
        assert!(matches!(
            result,
            Err(StoreError::VisibleFileConflict { .. })
        ));
        assert_eq!(
            fs::read_to_string(store.root.join("manuscript/new.md"))
                .expect("external file survives"),
            "appeared externally"
        );
        assert_eq!(store.pending_outbox_count().expect("pending outbox"), 1);
    }

    #[test]
    fn create_if_absent_supports_an_empty_zero_segment_revision() {
        let (_directory, mut store) = new_store();
        let created = store
            .create_document_if_absent(
                "manuscript/new.md",
                DocumentContent::Prose(String::new()),
                "create empty document",
            )
            .expect("create absent document");
        assert!(
            store
                .revision_provenance(created.revision_id)
                .expect("empty provenance")
                .segments
                .is_empty()
        );
        assert_eq!(
            store
                .read_document("manuscript/new.md")
                .expect("read empty document")
                .text,
            ""
        );
        let second = store.create_document_if_absent(
            "manuscript/new.md",
            DocumentContent::Prose("overwrite".into()),
            "must not overwrite",
        );
        assert!(matches!(second, Err(StoreError::DocumentAlreadyExists(_))));
    }

    #[test]
    fn read_document_binds_visible_text_to_active_revision() {
        let (_directory, mut store) = new_store();
        let saved = store
            .save_document(
                "manuscript/001.md",
                DocumentContent::Prose("bound text".into()),
                "checkpoint",
            )
            .expect("save document");
        let loaded = store
            .read_document("manuscript/001.md")
            .expect("load document");
        assert_eq!(loaded.text, "bound text");
        assert_eq!(loaded.revision_id, saved.revision_id);
        assert_eq!(loaded.blob_id, saved.blob_id);

        fs::write(store.root.join("manuscript/001.md"), "external").expect("external edit");
        assert!(matches!(
            store.read_document("manuscript/001.md"),
            Err(StoreError::UncheckpointedVisibleChange(_))
        ));
    }

    #[test]
    fn reconciliation_snapshot_reads_base_and_external_text_without_writing() {
        let (_directory, mut store) = new_store();
        let saved = store
            .save_document(
                "manuscript/001.md",
                DocumentContent::Prose("base".into()),
                "checkpoint",
            )
            .expect("save base");
        let before = store.counts().expect("counts before reconciliation");
        fs::write(store.root.join("manuscript/001.md"), "external").expect("external edit");

        let snapshot = store
            .reconciliation_snapshot("manuscript/001.md")
            .expect("reconciliation snapshot");
        assert_eq!(snapshot.active_revision_id, saved.revision_id);
        assert_eq!(snapshot.active_blob_id, saved.blob_id);
        assert_eq!(snapshot.base_text, "base");
        assert_eq!(
            snapshot.visible.as_ref().expect("visible state").text,
            "external"
        );
        assert!(!snapshot.visible_matches_active);
        assert_eq!(store.counts().expect("counts after reconciliation"), before);
        assert_eq!(store.pending_outbox_count().expect("pending outbox"), 0);

        fs::remove_file(store.root.join("manuscript/001.md")).expect("external delete");
        let deleted = store
            .reconciliation_snapshot("manuscript/001.md")
            .expect("deleted-file snapshot");
        assert!(deleted.visible.is_none());
        assert_eq!(deleted.base_text, "base");
    }

    #[cfg(unix)]
    #[test]
    fn document_path_cannot_traverse_symlinked_directory() {
        use std::os::unix::fs::symlink;

        let (_directory, mut store) = new_store();
        let outside = tempdir().expect("outside directory");
        fs::remove_dir(store.root.join("manuscript")).expect("remove empty manuscript directory");
        symlink(outside.path(), store.root.join("manuscript")).expect("create symlink");
        let result = store.save_document(
            "manuscript/escape.md",
            DocumentContent::Prose("no escape".into()),
            "checkpoint",
        );
        assert!(matches!(result, Err(StoreError::SymbolicLink(_))));
        assert!(!outside.path().join("escape.md").exists());
    }
}
