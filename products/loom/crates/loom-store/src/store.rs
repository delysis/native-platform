use std::collections::BTreeSet;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Mutex, OnceLock};

use fs4::TryLockError;
use loom_document::DocumentContent;
use loom_types::{
    ArtifactId, BlobId, CommandId, CommandKind, CommandReceipt, DocumentId, DocumentKind,
    OperationId, OperationKind, ProjectId, ProjectManifest, RevisionId, now_unix_ms,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use unicode_normalization::UnicodeNormalization as _;

use crate::file_io::{
    BoundedNoFollowFile, atomic_install_if_absent, atomic_replace, atomic_replace_private,
    create_private_file_if_absent, ensure_document_lifecycle_supported, hard_link_if_absent,
    read_bounded, read_bounded_no_follow, rename_if_absent, sync_parent, sync_rename_parents,
};
use crate::paths::{
    ensure_document_parent, ensure_private_directory, inspect_document_path,
    normalize_document_path, reject_symlink_target,
};
use crate::schema::{CURRENT_SCHEMA_VERSION, initialize_schema};
use crate::{Result, StoreError};

const PROJECT_FORMAT: &str = "loom-project";
pub(crate) const DATABASE_FILE: &str = "loom.sqlite3";
const PROJECT_LEASE_FILE: &str = "session.lock";
const MANIFEST_FILE: &str = "project.json";
const DOCUMENT_RENAME_DIRECTORY: &str = ".loom/renames";
const DOCUMENT_DELETED_DIRECTORY: &str = ".loom/deleted";
const MAX_PROJECT_NAME_BYTES: usize = 512;
pub const MAX_DOCUMENT_TITLE_BYTES: usize = 256;
const MAX_DOCUMENT_FILE_NAME_BYTES: usize = 255;
const MAX_REASON_BYTES: usize = 4 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
pub const MAX_DOCUMENT_BYTES: u64 = 128 * 1024 * 1024;

pub struct ProjectStore {
    pub(crate) root: PathBuf,
    pub(crate) manifest: ProjectManifest,
    pub(crate) connection: Connection,
    pub(crate) folder_warnings: Vec<String>,
    _lease: ProjectLease,
}

pub(crate) struct ProjectLease {
    _file: File,
    root: PathBuf,
}

impl Drop for ProjectLease {
    fn drop(&mut self) {
        process_project_leases()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.root);
    }
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
        crate::paths::ensure_private_storage_supported()?;
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
            loom_dir.clone(),
            loom_dir.join("blobs"),
            loom_dir.join("blobs/sha256"),
            loom_dir.join("indexes"),
            loom_dir.join("backups"),
        ] {
            ensure_private_directory(&directory)?;
        }

        let lease = acquire_project_lease(&loom_dir, &root)?;
        // A concurrent initializer can create the manifest between the first
        // inspection and our lease acquisition. Never overwrite it.
        reject_symlink_target(&manifest_path)?;
        if manifest_path.exists() {
            return Err(StoreError::AlreadyInitialized(root));
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

        let mut store = Self::open_internal(root, manifest, Some(lease))?;
        let receipt =
            store.new_receipt(CommandKind::InitProject, started_at_ms, None, &[], &[], &[]);
        store.persist_receipt(&receipt)?;
        Ok((store, receipt))
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        crate::paths::ensure_private_storage_supported()?;
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
        Self::open_internal(root, manifest, None)
    }

    fn open_internal(
        root: PathBuf,
        manifest: ProjectManifest,
        lease: Option<ProjectLease>,
    ) -> Result<Self> {
        let initializing = lease.is_some();
        let loom_dir = root.join(".loom");
        let database_path = loom_dir.join(DATABASE_FILE);
        reject_symlink_target(&database_path)?;
        if !initializing && !database_path.is_file() {
            return Err(StoreError::CorruptDatabase(
                "project database is missing".into(),
            ));
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
        let lease = match lease {
            Some(lease) => lease,
            None => acquire_project_lease(&loom_dir, &root)?,
        };
        if initializing {
            create_private_file_if_absent(&database_path)?;
        }
        let mut connection = Connection::open_with_flags(
            &database_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        initialize_schema(&mut connection, initializing)?;
        let store = Self {
            root,
            manifest,
            connection,
            folder_warnings: Vec::new(),
            _lease: lease,
        };
        store.recover_document_rename_operations()?;
        store.recover_document_delete_operations()?;
        store.cleanup_terminal_rename_anchors();
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub const fn manifest(&self) -> &ProjectManifest {
        &self.manifest
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

    /// Reconciles durable prepared document-lifecycle intents before callers
    /// resolve a catalogue pathname. This is safe to call repeatedly and is
    /// deliberately available through a shared reference so snapshot/read
    /// admission cannot strand a same-session retry behind a missing old name.
    pub fn reconcile_document_lifecycle(&self) -> Result<()> {
        self.recover_document_rename_operations()?;
        self.recover_document_delete_operations()
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
        if self.document_path_is_reserved(&relative_path)? {
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
        if document_path_conflicts_in(&transaction, &relative_path, None)? {
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

    /// Registers an existing visible manuscript as a new human-authored document
    /// without normalizing or rewriting its bytes.
    ///
    /// The visible file is both the expected and target outbox state. An
    /// unchanged file therefore completes through the no-write fast path, while
    /// a concurrent edit remains visible and leaves a recoverable conflict.
    pub fn adopt_visible_document_if_absent(
        &mut self,
        relative_path: impl AsRef<Path>,
        kind: DocumentKind,
        reason: impl Into<String>,
    ) -> Result<SaveOutcome> {
        self.adopt_visible_document_if_absent_with_boundary(relative_path, kind, reason, |_| Ok(()))
    }

    fn adopt_visible_document_if_absent_with_boundary<F>(
        &mut self,
        relative_path: impl AsRef<Path>,
        kind: DocumentKind,
        reason: impl Into<String>,
        before_outbox_boundary: F,
    ) -> Result<SaveOutcome>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        self.adopt_visible_document_if_absent_with_boundaries(
            relative_path,
            kind,
            reason,
            |_| Ok(()),
            before_outbox_boundary,
        )
    }

    #[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
    fn adopt_visible_document_if_absent_with_boundaries<F, G>(
        &mut self,
        relative_path: impl AsRef<Path>,
        kind: DocumentKind,
        reason: impl Into<String>,
        before_read_boundary: F,
        before_outbox_boundary: G,
    ) -> Result<SaveOutcome>
    where
        F: FnOnce(&Path) -> Result<()>,
        G: FnOnce(&Path) -> Result<()>,
    {
        let started_at_ms = now_unix_ms();
        let relative_path = normalize_document_path(relative_path.as_ref())?;
        let reason = reason.into();
        if reason.len() > MAX_REASON_BYTES {
            return Err(StoreError::ReasonTooLong {
                max_bytes: MAX_REASON_BYTES,
            });
        }
        if self.document_path_is_reserved(&relative_path)? {
            return Err(StoreError::DocumentAlreadyExists(relative_path));
        }

        let visible_path = inspect_document_path(&self.root, &relative_path)?;
        before_read_boundary(&visible_path)?;
        let bytes = read_bounded_no_follow(&visible_path, MAX_DOCUMENT_BYTES)?;
        if std::str::from_utf8(&bytes).is_err() {
            return Err(StoreError::ExternalVisibleInvalidUtf8(relative_path));
        }
        let byte_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let byte_len_i64 = i64::try_from(byte_len).map_err(|_| StoreError::DocumentTooLarge {
            actual_bytes: byte_len,
            max_bytes: MAX_DOCUMENT_BYTES,
        })?;
        let blob_id = self.put_blob(&bytes)?;

        let document_id = DocumentId::new();
        let artifact_id = ArtifactId::new();
        let operation_id = OperationId::new();
        let revision_id = RevisionId::new();
        let command_id = CommandId::new();
        let created_at_ms = now_unix_ms();
        let receipt = CommandReceipt {
            command_id,
            command: CommandKind::Import,
            project_id: self.manifest.project_id,
            project_schema_version: self.manifest.schema_version,
            source_revision_id: None,
            resulting_artifact_ids: vec![artifact_id],
            resulting_operation_ids: vec![operation_id],
            resulting_revision_ids: vec![revision_id],
            started_at_ms,
            completed_at_ms: created_at_ms,
        };

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if document_path_conflicts_in(&transaction, &relative_path, None)? {
            return Err(StoreError::DocumentAlreadyExists(relative_path));
        }
        let current_visible_blob_id = visible_hash_if_present(&visible_path)?
            .ok_or_else(|| StoreError::ExternalVisibleFileDeleted(relative_path.clone()))?;
        if current_visible_blob_id != blob_id {
            return Err(StoreError::ExternalVisibleBlobMismatch {
                expected: blob_id,
                actual: current_visible_blob_id,
            });
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
                kind.as_str(),
                created_at_ms
            ],
        )?;
        let metadata = serde_json::to_string(&json!({
            "workflow": "adopt_visible_document",
            "source": "existing_visible_file",
            "relative_path": relative_path,
            "reason": reason,
            "source_blob_id": blob_id,
        }))?;
        transaction.execute(
            "INSERT INTO artifacts(artifact_id, blob_id, artifact_kind, media_type, metadata_json, created_at_ms)
             VALUES (?1, ?2, 'human_contribution', ?3, ?4, ?5)",
            params![
                artifact_id.to_string(),
                blob_id.to_string(),
                media_type(kind),
                metadata,
                created_at_ms,
            ],
        )?;
        transaction.execute(
            "INSERT INTO operations(operation_id, operation_kind, metadata_json, created_at_ms)
             VALUES (?1, 'import', ?2, ?3)",
            params![
                operation_id.to_string(),
                serde_json::to_string(&json!({
                    "workflow": "adopt_visible_document",
                    "source": "existing_visible_file",
                    "relative_path": relative_path,
                    "reason": reason,
                    "source_blob_id": blob_id,
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
             VALUES (?1, ?2, ?3, ?3, 'pending', ?4)",
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

        before_outbox_boundary(&visible_path)?;
        match self.process_outbox_entry(outbox_id)? {
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
        self.reconcile_document_lifecycle()?;
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
        self.reconcile_document_lifecycle()?;
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
        let owner = existing_document.as_ref().map(|document| document.id);
        if self.document_path_conflicts(relative_path, owner)? {
            return Err(StoreError::DocumentAlreadyExists(relative_path.to_owned()));
        }
        let document_id = owner.unwrap_or_else(DocumentId::new);
        let active = existing_document
            .as_ref()
            .map(|_| self.active_revision(document_id))
            .transpose()?
            .flatten();

        let visible_path = ensure_document_parent(&self.root, relative_path)?;
        let expected_visible_blob_id = visible_hash_if_present(&visible_path)?;
        let blob_id = self.put_blob(&projection.bytes)?;
        let media_type = media_type(content.kind());

        let artifact_id = ArtifactId::new();
        let operation_id = OperationId::new();
        let revision_id = RevisionId::new();
        let created_at_ms = now_unix_ms();
        let byte_len_i64 = i64::try_from(byte_len).map_err(|_| StoreError::DocumentTooLarge {
            actual_bytes: byte_len,
            max_bytes: MAX_DOCUMENT_BYTES,
        })?;

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if document_path_conflicts_in(&transaction, relative_path, owner)? {
            return Err(StoreError::DocumentAlreadyExists(relative_path.to_owned()));
        }
        if let Some(active) = active {
            crate::provenance::validate_active_in_transaction(&transaction, document_id, active)?;
        }
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
        let mut source = self.open_document_file(relative_path)?;
        self.export_document_file(&mut source, destination)
    }

    /// Exports the already-open, blob-verified descriptor authority without
    /// resolving or reopening its mutable visible path.
    pub fn export_document_file(
        &mut self,
        source: &mut DocumentFileAuthority,
        destination: impl AsRef<Path>,
    ) -> Result<CommandReceipt> {
        let started_at_ms = now_unix_ms();
        if source.project_id != self.manifest.project_id {
            return Err(StoreError::DocumentFileAuthorityMismatch);
        }
        let document = self
            .registered_document(source.document.document_id)?
            .ok_or(StoreError::DocumentFileAuthorityMismatch)?;
        if document.relative_path != source.document.relative_path
            || document.active_revision_id != Some(source.document.revision_id)
        {
            return Err(StoreError::DocumentFileAuthorityMismatch);
        }
        let active = self
            .active_revision(source.document.document_id)?
            .ok_or(StoreError::DocumentFileAuthorityMismatch)?;
        if active.revision_id != source.document.revision_id
            || active.artifact_id != source.document.artifact_id
            || active.blob_id != source.document.blob_id
        {
            return Err(StoreError::DocumentFileAuthorityMismatch);
        }
        source.revalidate()?;
        let bytes = source.document.text.as_bytes();

        if let Some(parent) = destination.as_ref().parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        atomic_replace(destination.as_ref(), bytes)?;

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
            "relative_path": &source.document.relative_path,
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

    /// Probes the primary-key document index without allocating or scanning
    /// the project's document catalogue. This is the bounded identity check
    /// for frequently refreshed, document-scoped projections.
    pub fn document_is_registered(&self, document_id: DocumentId) -> Result<bool> {
        Ok(self
            .connection
            .query_row(
                "SELECT 1 FROM documents WHERE document_id = ?1 LIMIT 1",
                [document_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some())
    }

    /// Reports whether initialization has ever registered a document. This is
    /// history-inclusive: a catalogue containing only tombstones is
    /// intentionally empty, not an uninitialized project to repopulate.
    pub fn has_registered_documents(&self) -> Result<bool> {
        Ok(self
            .connection
            .query_row("SELECT EXISTS(SELECT 1 FROM documents)", [], |row| {
                row.get::<_, bool>(0)
            })?)
    }

    /// Probes all current registrations, including tombstones, plus both
    /// endpoints of a live prepared rename. A committed rename frees its old
    /// pathname; stable IDs continue to disambiguate immutable history.
    pub fn document_path_is_reserved(&self, relative_path: impl AsRef<Path>) -> Result<bool> {
        self.document_path_conflicts(relative_path, None)
    }

    fn document_path_conflicts(
        &self,
        relative_path: impl AsRef<Path>,
        owner: Option<DocumentId>,
    ) -> Result<bool> {
        let relative_path = normalize_document_path(relative_path.as_ref())?;
        document_path_conflicts_in(&self.connection, &relative_path, owner)
    }

    /// Resolves one registered document through the primary-key index.
    ///
    /// This is the bounded authority lookup for commands that capture a
    /// document ID and must derive its current path and revision natively.
    pub fn registered_document(&self, document_id: DocumentId) -> Result<Option<DocumentSummary>> {
        let row: Option<(String, String, Option<String>)> = self
            .connection
            .query_row(
                "SELECT relative_path, document_kind, display_title
                 FROM documents WHERE document_id = ?1",
                [document_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((relative_path, kind, display_title)) = row else {
            return Ok(None);
        };
        let display_title = validate_stored_document_display_title(display_title)?;
        Ok(Some(DocumentSummary {
            document_id,
            relative_path,
            display_title,
            kind: DocumentKind::from_str(&kind)
                .map_err(|error| StoreError::CorruptDatabase(error.to_string()))?,
            active_revision_id: self
                .active_revision(document_id)?
                .map(|active| active.revision_id),
        }))
    }

    pub fn list_documents(&self) -> Result<Vec<DocumentSummary>> {
        let mut statement = self.connection.prepare(
            "SELECT d.document_id, d.relative_path, d.display_title, d.document_kind,
                    (SELECT r.revision_id FROM revisions r WHERE r.document_id = d.document_id ORDER BY r.created_at_ms DESC, r.revision_id DESC LIMIT 1)
             FROM documents d
             WHERE NOT EXISTS (
                 SELECT 1 FROM document_deletions deletion
                 WHERE deletion.document_id = d.document_id
             )
             ORDER BY d.relative_path",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?;
        let mut documents = Vec::new();
        for row in rows {
            let (document_id, relative_path, display_title, kind, revision_id) = row?;
            let display_title = validate_stored_document_display_title(display_title)?;
            documents.push(DocumentSummary {
                document_id: parse_id(&document_id, "document_id")?,
                relative_path,
                display_title,
                kind: DocumentKind::from_str(&kind)
                    .map_err(|error| StoreError::CorruptDatabase(error.to_string()))?,
                active_revision_id: revision_id
                    .map(|value| parse_id(&value, "revision_id"))
                    .transpose()?,
            });
        }
        Ok(documents)
    }

    /// Atomically captures the exact ordinary manuscript into private state,
    /// installs it at the new no-clobber path, and commits that path and title
    /// as the final fallible step. No visible pathname is ever unlinked.
    pub fn rename_document(
        &mut self,
        authority: &mut DocumentFileAuthority,
        requested_title: &str,
    ) -> Result<DocumentSummary> {
        self.recover_document_rename_operations()?;
        if authority.project_id != self.manifest.project_id {
            return Err(StoreError::DocumentFileAuthorityMismatch);
        }
        let display_title = normalize_document_display_title(requested_title)?;
        authority.revalidate()?;
        let source = authority.document().clone();
        let target_relative_path = document_path_for_title(&source.relative_path, &display_title)?;
        ensure_no_pending_document_outbox_in(&self.connection, source.document_id)?;
        self.rename_document_at_lifecycle_boundary(
            authority,
            source,
            display_title,
            target_relative_path,
            || Ok(()),
            || Ok(()),
            || Ok(()),
        )
    }

    // The injected crash boundaries keep every namespace/database transition
    // independently testable; the state machine remains linear on purpose.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn rename_document_at_lifecycle_boundary<F, G, H>(
        &mut self,
        authority: &mut DocumentFileAuthority,
        source: LoadedDocument,
        display_title: String,
        target_relative_path: String,
        before_private_capture: F,
        after_private_capture: G,
        before_catalog_commit: H,
    ) -> Result<DocumentSummary>
    where
        F: FnOnce() -> Result<()>,
        G: FnOnce() -> Result<()>,
        H: FnOnce() -> Result<()>,
    {
        if target_relative_path == source.relative_path {
            let changed = self.connection.execute(
                "UPDATE documents SET display_title = ?2
                 WHERE document_id = ?1
                   AND NOT EXISTS (
                       SELECT 1 FROM document_deletions deletion
                       WHERE deletion.document_id = documents.document_id
                   )",
                params![source.document_id.to_string(), &display_title],
            )?;
            if changed != 1 {
                return Err(StoreError::DocumentFileAuthorityMismatch);
            }
            return Ok(DocumentSummary {
                document_id: source.document_id,
                relative_path: source.relative_path,
                display_title: Some(display_title),
                kind: source.kind,
                active_revision_id: Some(source.revision_id),
            });
        }

        ensure_document_lifecycle_supported()?;

        if self.document_path_conflicts(&target_relative_path, Some(source.document_id))? {
            return Err(StoreError::DocumentAlreadyExists(target_relative_path));
        }

        let target = ensure_document_parent(&self.root, &target_relative_path)?;
        match fs::symlink_metadata(&target) {
            Ok(_) if authority.file.path_has_identity(&target)? => {
                // On a case-insensitive filesystem the new spelling may name
                // the held source until it is captured. Capturing first makes
                // the no-replace install safe and gives the filesystem a
                // chance to persist the requested case.
            }
            Ok(_) => return Err(StoreError::VisibleFileAlreadyExists(target_relative_path)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let rename_directory = self.root.join(DOCUMENT_RENAME_DIRECTORY);
        ensure_private_directory(&rename_directory)?;
        let operation_id = CommandId::new();
        let capture_path = rename_directory.join(format!("{operation_id}.capture"));
        let created_at_ms = now_unix_ms();

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_document_action_in(
            &transaction,
            source.document_id,
            &source.relative_path,
            source.revision_id,
            source.blob_id,
            source.kind,
        )?;
        ensure_no_pending_document_outbox_in(&transaction, source.document_id)?;
        transaction.execute(
            "INSERT INTO document_rename_operations(
                 operation_id, document_id, revision_id, blob_id,
                 source_relative_path, target_relative_path, target_display_title,
                 state, created_at_ms, captured_at_ms, committed_at_ms, finished_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'prepared', ?8, NULL, NULL, NULL)",
            params![
                operation_id.to_string(),
                source.document_id.to_string(),
                source.revision_id.to_string(),
                source.blob_id.to_string(),
                &source.relative_path,
                &target_relative_path,
                &display_title,
                created_at_ms,
            ],
        )?;
        transaction.commit()?;

        before_private_capture()?;
        let capture_move = rename_if_absent(authority.file.path(), &capture_path);
        if matches!(capture_move, Ok(false)) {
            return match self.abort_document_rename(operation_id) {
                Ok(()) => Err(StoreError::VisibleFileAlreadyExists(
                    capture_path.display().to_string(),
                )),
                Err(error) => Err(StoreError::DocumentLifecycleUncertain(format!(
                    "private capture slot was occupied and the prepared rename could not be retired: {error}"
                ))),
            };
        }
        let capture_is_exact = authority
            .file
            .path_has_identity(&capture_path)
            .unwrap_or(false);
        if !capture_is_exact {
            let source_is_exact = authority
                .file
                .path_has_identity(authority.file.path())
                .unwrap_or(false);
            let restored =
                restore_captured_path_no_clobber(&capture_path, authority.file.path())
                    .map_err(|error| StoreError::DocumentLifecycleUncertain(error.to_string()))?;
            if restored == CapturedPathDisposition::Restored
                || (restored == CapturedPathDisposition::Missing && source_is_exact)
            {
                self.abort_document_rename(operation_id).map_err(|error| {
                    StoreError::DocumentLifecycleUncertain(format!(
                        "capture identity changed and the prepared rename could not be retired: {error}"
                    ))
                })?;
                return match capture_move {
                    Ok(false) => unreachable!("handled capture collision above"),
                    Ok(true) if restored == CapturedPathDisposition::Restored => {
                        Err(StoreError::VisibleFileIdentityChanged(capture_path))
                    }
                    Ok(true) | Err(_) => Err(StoreError::DocumentLifecycleUncertain(
                        "capture namespace changed before identity verification".into(),
                    )),
                };
            }
            return Err(StoreError::DocumentLifecycleUncertain(
                "a raced source replacement remains preserved in the prepared private capture"
                    .into(),
            ));
        }
        if let Err(error) = authority
            .file
            .sync_all()
            .and_then(|()| sync_rename_parents(authority.file.path(), &capture_path))
        {
            return Err(self.rename_failure_after_capture(
                operation_id,
                &source.relative_path,
                &target_relative_path,
                source.blob_id,
                error,
            ));
        }
        if let Err(error) = after_private_capture() {
            return Err(self.rename_failure_after_capture(
                operation_id,
                &source.relative_path,
                &target_relative_path,
                source.blob_id,
                error,
            ));
        }
        let captured_at_ms = now_unix_ms();
        match self.connection.execute(
            "UPDATE document_rename_operations
             SET state = 'captured', captured_at_ms = ?2
             WHERE operation_id = ?1 AND state = 'prepared'",
            params![operation_id.to_string(), captured_at_ms],
        ) {
            Ok(1) => {}
            Ok(_) => {
                return Err(self.rename_failure_after_capture(
                    operation_id,
                    &source.relative_path,
                    &target_relative_path,
                    source.blob_id,
                    "prepared rename did not transition to captured",
                ));
            }
            Err(error) => {
                return Err(self.rename_failure_after_capture(
                    operation_id,
                    &source.relative_path,
                    &target_relative_path,
                    source.blob_id,
                    error,
                ));
            }
        }

        // Once `captured` is durable, the exact private capture itself proves
        // ownership. Link a durable inode anchor before the capture name can
        // move to the visible target; recovery can then distinguish our target
        // from an unrelated same-content replacement after restart.
        let anchor_path = rename_directory.join(format!("{operation_id}.anchor"));
        let anchor_link = hard_link_if_absent(&capture_path, &anchor_path);
        if matches!(anchor_link, Ok(false)) {
            return Err(self.rename_failure_after_capture(
                operation_id,
                &source.relative_path,
                &target_relative_path,
                source.blob_id,
                StoreError::VisibleFileAlreadyExists(anchor_path.display().to_string()),
            ));
        }
        let anchor_is_exact = authority
            .file
            .path_has_identity(&anchor_path)
            .unwrap_or(false);
        if !anchor_is_exact {
            return Err(self.rename_failure_after_capture(
                operation_id,
                &source.relative_path,
                &target_relative_path,
                source.blob_id,
                match anchor_link {
                    Ok(false) => unreachable!("handled anchor collision above"),
                    Ok(true) => StoreError::VisibleFileIdentityChanged(anchor_path),
                    Err(error) => error,
                },
            ));
        }
        if let Err(error) = authority
            .file
            .sync_all()
            .and_then(|()| sync_parent(&anchor_path))
        {
            return Err(self.rename_failure_after_capture(
                operation_id,
                &source.relative_path,
                &target_relative_path,
                source.blob_id,
                error,
            ));
        }

        let target_move = rename_if_absent(&capture_path, &target);
        if matches!(target_move, Ok(false)) {
            return match self.rollback_prepared_rename(
                operation_id,
                &source.relative_path,
                &target_relative_path,
                source.blob_id,
            ) {
                Ok(RenameRollbackDisposition::SourceRestored) => {
                    Err(StoreError::VisibleFileAlreadyExists(target_relative_path))
                }
                Ok(RenameRollbackDisposition::PreservedConflict) => {
                    Err(StoreError::DocumentLifecycleUncertain(
                        "rename target collision coincided with a recreated source; both objects were preserved"
                            .into(),
                    ))
                }
                Ok(RenameRollbackDisposition::SourceMissing) => {
                    Err(StoreError::DocumentLifecycleUncertain(
                        "rename target collision was preserved, but the expected source is externally missing"
                            .into(),
                    ))
                }
                Err(error) => Err(StoreError::DocumentLifecycleUncertain(format!(
                    "rename target was occupied and rollback remains pending: {error}"
                ))),
            };
        }
        let target_is_exact = authority.file.path_has_identity(&target).unwrap_or(false);
        if !target_is_exact {
            let cause = match target_move {
                Ok(false) => unreachable!("handled target collision above"),
                Ok(true) => StoreError::VisibleFileIdentityChanged(target),
                Err(error) => error,
            };
            return Err(self.rename_failure_after_capture(
                operation_id,
                &source.relative_path,
                &target_relative_path,
                source.blob_id,
                cause,
            ));
        }
        if let Err(error) = authority
            .file
            .sync_all()
            .and_then(|()| sync_rename_parents(&capture_path, &target))
        {
            return Err(self.rename_failure_after_capture(
                operation_id,
                &source.relative_path,
                &target_relative_path,
                source.blob_id,
                error,
            ));
        }
        let target_bytes = match read_bounded_no_follow(&target, MAX_DOCUMENT_BYTES) {
            Ok(bytes) => bytes,
            Err(error) => {
                return Err(self.rename_failure_after_capture(
                    operation_id,
                    &source.relative_path,
                    &target_relative_path,
                    source.blob_id,
                    error,
                ));
            }
        };
        if BlobId::digest(&target_bytes) != source.blob_id {
            let actual = BlobId::digest(&target_bytes);
            return Err(self.rename_failure_after_capture(
                operation_id,
                &source.relative_path,
                &target_relative_path,
                source.blob_id,
                StoreError::ExternalVisibleBlobMismatch {
                    expected: source.blob_id,
                    actual,
                },
            ));
        }
        if let Err(error) = before_catalog_commit() {
            return Err(self.rename_failure_after_capture(
                operation_id,
                &source.relative_path,
                &target_relative_path,
                source.blob_id,
                error,
            ));
        }

        let committed_at_ms = now_unix_ms();
        let rollback_root = self.root.clone();
        let transaction = match self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
        {
            Ok(transaction) => transaction,
            Err(error) => {
                let recovery = rollback_prepared_rename_namespace(
                    &rollback_root,
                    operation_id,
                    RenameOperationState::Captured,
                    &source.relative_path,
                    &target_relative_path,
                    source.blob_id,
                );
                return Err(StoreError::DocumentLifecycleUncertain(format!(
                    "{error}; namespace rollback after transaction acquisition failure: {recovery:?}; the prepared intent remains durable"
                )));
            }
        };
        if let Err(error) = validate_document_action_in(
            &transaction,
            source.document_id,
            &source.relative_path,
            source.revision_id,
            source.blob_id,
            source.kind,
        ) {
            drop(transaction);
            return Err(self.rename_failure_after_capture(
                operation_id,
                &source.relative_path,
                &target_relative_path,
                source.blob_id,
                error,
            ));
        }
        let changed = transaction.execute(
            "UPDATE documents
             SET relative_path = ?2, display_title = ?3
             WHERE document_id = ?1 AND relative_path = ?4",
            params![
                source.document_id.to_string(),
                &target_relative_path,
                &display_title,
                &source.relative_path,
            ],
        );
        let changed = match changed {
            Ok(changed) => changed,
            Err(error) => {
                drop(transaction);
                return Err(self.rename_failure_after_capture(
                    operation_id,
                    &source.relative_path,
                    &target_relative_path,
                    source.blob_id,
                    error,
                ));
            }
        };
        if changed != 1 {
            drop(transaction);
            return Err(self.rename_failure_after_capture(
                operation_id,
                &source.relative_path,
                &target_relative_path,
                source.blob_id,
                StoreError::DocumentFileAuthorityMismatch,
            ));
        }
        let operation_changed = transaction.execute(
            "UPDATE document_rename_operations
             SET state = 'committed', committed_at_ms = ?2, finished_at_ms = ?2
             WHERE operation_id = ?1 AND state = 'captured'",
            params![operation_id.to_string(), committed_at_ms],
        );
        match operation_changed {
            Ok(1) => {}
            Ok(_) => {
                drop(transaction);
                return Err(self.rename_failure_after_capture(
                    operation_id,
                    &source.relative_path,
                    &target_relative_path,
                    source.blob_id,
                    StoreError::DocumentFileAuthorityMismatch,
                ));
            }
            Err(error) => {
                drop(transaction);
                return Err(self.rename_failure_after_capture(
                    operation_id,
                    &source.relative_path,
                    &target_relative_path,
                    source.blob_id,
                    error,
                ));
            }
        }
        if let Err(error) = transaction.commit() {
            match self.document_rename_commit_state(
                operation_id,
                source.document_id,
                &target_relative_path,
            ) {
                Ok(true) => {}
                Ok(false) => {
                    return Err(self.rename_failure_after_capture(
                        operation_id,
                        &source.relative_path,
                        &target_relative_path,
                        source.blob_id,
                        error,
                    ));
                }
                Err(reconciliation_error) => {
                    return Err(StoreError::DocumentLifecycleUncertain(format!(
                        "{error}; commit reconciliation failed: {reconciliation_error}"
                    )));
                }
            }
        }

        // The namespace and SQLite authority are now committed. The ownership
        // anchor is no longer authoritative; cleanup is best-effort and can
        // never turn committed success into an error. Reopen retries cleanup.
        let _ = remove_if_present(&anchor_path);
        Ok(DocumentSummary {
            document_id: source.document_id,
            relative_path: target_relative_path,
            display_title: Some(display_title),
            kind: source.kind,
            active_revision_id: Some(source.revision_id),
        })
    }

    /// Captures one exact active manuscript directly into its deterministic
    /// private recovery slot, flushes it, then commits an immutable catalogue
    /// tombstone as the final fallible operation.
    pub fn delete_document_file_idempotent(
        &mut self,
        command_id: CommandId,
        document_id: DocumentId,
        expected_revision_id: RevisionId,
        expected_blob_id: BlobId,
    ) -> Result<()> {
        self.delete_document_file_at_lifecycle_boundary(
            command_id,
            document_id,
            expected_revision_id,
            expected_blob_id,
            || Ok(()),
            || Ok(()),
        )
    }

    // Keep the durable intent, capture, and tombstone transitions together so
    // every return path is audited against one linear state machine.
    #[allow(clippy::too_many_lines)]
    fn delete_document_file_at_lifecycle_boundary<F, G>(
        &mut self,
        command_id: CommandId,
        document_id: DocumentId,
        expected_revision_id: RevisionId,
        expected_blob_id: BlobId,
        before_private_capture: F,
        after_private_capture: G,
    ) -> Result<()>
    where
        F: FnOnce() -> Result<()>,
        G: FnOnce() -> Result<()>,
    {
        self.reconcile_document_lifecycle()?;
        if let Some(existing) = self.document_deletion_by_command(command_id)? {
            if existing.document_id == document_id
                && existing.revision_id == expected_revision_id
                && existing.blob_id == expected_blob_id
            {
                return Ok(());
            }
            return Err(StoreError::IdempotencyConflict { command_id });
        }
        let existing_operation = self.document_delete_operation_by_command(command_id)?;
        if self.document_deletion_command(document_id)?.is_some() {
            return Err(StoreError::IdempotencyConflict { command_id });
        }
        let registered = self
            .registered_document(document_id)?
            .ok_or(StoreError::DocumentFileAuthorityMismatch)?;
        if registered.active_revision_id != Some(expected_revision_id) {
            return Err(StoreError::SourceRevisionMismatch {
                expected: expected_revision_id,
                actual: registered.active_revision_id.ok_or_else(|| {
                    StoreError::NoActiveRevision(registered.relative_path.clone())
                })?,
            });
        }
        let active = self
            .active_revision(document_id)?
            .ok_or_else(|| StoreError::NoActiveRevision(registered.relative_path.clone()))?;
        if active.blob_id != expected_blob_id {
            return Err(StoreError::SourceBlobMismatch {
                expected: expected_blob_id,
                actual: active.blob_id,
            });
        }

        let reuse_aborted_operation = if let Some(operation) = existing_operation {
            if !operation.matches_at_path(
                document_id,
                expected_revision_id,
                expected_blob_id,
                &registered.relative_path,
            ) {
                return Err(StoreError::IdempotencyConflict { command_id });
            }
            match operation.state {
                DeleteOperationState::Aborted => true,
                DeleteOperationState::Committed => {
                    return Err(StoreError::CorruptDatabase(format!(
                        "committed delete operation {command_id} lacks its tombstone"
                    )));
                }
                DeleteOperationState::Prepared | DeleteOperationState::Captured => {
                    return Err(StoreError::DocumentLifecycleUncertain(format!(
                        "delete operation {command_id} remains live after reconciliation"
                    )));
                }
            }
        } else {
            false
        };

        ensure_no_transient_draft_in(&self.connection, document_id)?;
        ensure_no_pending_document_outbox_in(&self.connection, document_id)?;
        ensure_document_lifecycle_supported()?;

        let deleted_directory = self.root.join(DOCUMENT_DELETED_DIRECTORY);
        ensure_private_directory(&deleted_directory)?;
        let recovery_name = format!(
            "{command_id}.{document_id}.{expected_revision_id}.{expected_blob_id}.document"
        );
        let recovery_path = deleted_directory.join(&recovery_name);
        let visible_path = inspect_document_path(&self.root, &registered.relative_path)?;
        let mut visible = match BoundedNoFollowFile::open(&visible_path, MAX_DOCUMENT_BYTES) {
            Ok(visible) => visible,
            Err(StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::ExternalVisibleFileDeleted(
                    registered.relative_path,
                ));
            }
            Err(error) => return Err(error),
        };
        let bytes = visible.read()?;
        visible.ensure_path_binding()?;
        if BlobId::digest(&bytes) != expected_blob_id {
            return Err(StoreError::UncheckpointedVisibleChange(
                registered.relative_path,
            ));
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_document_action_in(
            &transaction,
            document_id,
            &registered.relative_path,
            expected_revision_id,
            expected_blob_id,
            registered.kind,
        )?;
        ensure_no_transient_draft_in(&transaction, document_id)?;
        ensure_no_pending_document_outbox_in(&transaction, document_id)?;
        if reuse_aborted_operation {
            let changed = transaction.execute(
                "UPDATE document_delete_operations
                 SET state = 'prepared', captured_at_ms = NULL,
                     committed_at_ms = NULL, finished_at_ms = NULL
                 WHERE command_id = ?1 AND state = 'aborted'",
                [command_id.to_string()],
            )?;
            if changed != 1 {
                return Err(StoreError::IdempotencyConflict { command_id });
            }
        } else {
            transaction.execute(
                "INSERT INTO document_delete_operations(
                     command_id, document_id, revision_id, blob_id, relative_path,
                     recovery_file_name, state, created_at_ms,
                     captured_at_ms, committed_at_ms, finished_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'prepared', ?7, NULL, NULL, NULL)",
                params![
                    command_id.to_string(),
                    document_id.to_string(),
                    expected_revision_id.to_string(),
                    expected_blob_id.to_string(),
                    &registered.relative_path,
                    &recovery_name,
                    now_unix_ms(),
                ],
            )?;
        }
        if let Err(error) = transaction.commit() {
            match self.document_delete_operation_by_command(command_id) {
                Ok(Some(operation))
                    if operation.matches(document_id, expected_revision_id, expected_blob_id)
                        && operation.state == DeleteOperationState::Prepared => {}
                Ok(None) => return Err(error.into()),
                Ok(Some(_)) => return Err(StoreError::IdempotencyConflict { command_id }),
                Err(reconciliation_error) => {
                    return Err(StoreError::DocumentLifecycleUncertain(format!(
                        "delete preparation commit failed: {error}; state reconciliation failed: {reconciliation_error}"
                    )));
                }
            }
        }

        if let Err(error) = before_private_capture() {
            return match self.abort_document_delete_operation(command_id) {
                Ok(()) => Err(error),
                Err(abort_error) => Err(StoreError::DocumentLifecycleUncertain(format!(
                    "{error}; pre-capture delete intent could not be retired: {abort_error}"
                ))),
            };
        }

        let capture = rename_if_absent(&visible_path, &recovery_path);
        if matches!(capture, Ok(false)) {
            return match self.abort_document_delete_operation(command_id) {
                Ok(()) => Err(StoreError::VisibleFileAlreadyExists(
                    recovery_path.display().to_string(),
                )),
                Err(error) => Err(StoreError::DocumentLifecycleUncertain(format!(
                    "private delete slot was occupied and its intent remains live: {error}"
                ))),
            };
        }
        if !visible.path_has_identity(&recovery_path).unwrap_or(false) {
            let source_is_exact = visible.path_has_identity(&visible_path).unwrap_or(false);
            let preservation = restore_captured_path_no_clobber(&recovery_path, &visible_path);
            let disposition = match preservation {
                Ok(disposition) => disposition,
                Err(error) => {
                    return Err(StoreError::DocumentLifecycleUncertain(format!(
                        "delete capture raced and replacement preservation is uncertain: {error}"
                    )));
                }
            };
            self.abort_document_delete_operation(command_id)
                .map_err(|error| {
                    StoreError::DocumentLifecycleUncertain(format!(
                        "delete capture raced and the durable intent could not be retired: {error}"
                    ))
                })?;
            return match disposition {
                CapturedPathDisposition::Restored => {
                    Err(StoreError::VisibleFileIdentityChanged(recovery_path))
                }
                CapturedPathDisposition::Missing if source_is_exact => {
                    Err(StoreError::DocumentLifecycleUncertain(
                        "delete capture failed before the held source moved".into(),
                    ))
                }
                CapturedPathDisposition::Missing
                | CapturedPathDisposition::PreservedPrivate => {
                    Err(StoreError::DocumentLifecycleUncertain(
                        "a raced delete replacement was preserved without consuming either namespace"
                            .into(),
                    ))
                }
            };
        }
        if let Err(error) = visible
            .sync_all()
            .and_then(|()| sync_rename_parents(&visible_path, &recovery_path))
        {
            return Err(StoreError::DocumentLifecycleUncertain(format!(
                "delete capture durability is uncertain: {error}"
            )));
        }
        let recovered = open_exact_private_blob_file(&recovery_path, expected_blob_id)
            .map_err(|error| StoreError::DocumentLifecycleUncertain(error.to_string()))?
            .ok_or_else(|| {
                StoreError::DocumentLifecycleUncertain(
                    "verified delete capture disappeared before state transition".into(),
                )
            })?;
        recovered.sync_all().map_err(|error| {
            StoreError::DocumentLifecycleUncertain(format!(
                "delete recovery file durability is uncertain: {error}"
            ))
        })?;

        after_private_capture()
            .map_err(|error| StoreError::DocumentLifecycleUncertain(error.to_string()))?;

        let captured_at_ms = now_unix_ms();
        let changed = self.connection.execute(
            "UPDATE document_delete_operations
             SET state = 'captured', captured_at_ms = ?2
             WHERE command_id = ?1 AND state = 'prepared'",
            params![command_id.to_string(), captured_at_ms],
        );
        match changed {
            Ok(1) => {}
            Ok(_) => {
                return Err(StoreError::DocumentLifecycleUncertain(
                    "prepared delete did not transition to captured".into(),
                ));
            }
            Err(error) => {
                return Err(StoreError::DocumentLifecycleUncertain(format!(
                    "delete capture is durable but its captured transition failed: {error}"
                )));
            }
        }

        self.commit_document_delete_operation(command_id)
    }

    fn document_deletion_by_command(
        &self,
        command_id: CommandId,
    ) -> Result<Option<DocumentDeletionRecord>> {
        document_deletion_by_command_in(&self.connection, command_id)
    }

    fn document_deletion_command(&self, document_id: DocumentId) -> Result<Option<CommandId>> {
        self.connection
            .query_row(
                "SELECT command_id FROM document_deletions WHERE document_id = ?1",
                [document_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|value| parse_id(&value, "document deletion command_id"))
            .transpose()
    }

    fn document_delete_operation_by_command(
        &self,
        command_id: CommandId,
    ) -> Result<Option<DocumentDeleteOperationRecord>> {
        document_delete_operation_by_command_in(&self.connection, command_id)
    }

    fn abort_document_delete_operation(&self, command_id: CommandId) -> Result<()> {
        let changed = self.connection.execute(
            "UPDATE document_delete_operations
             SET state = 'aborted', finished_at_ms = ?2
             WHERE command_id = ?1 AND state IN ('prepared', 'captured')",
            params![command_id.to_string(), now_unix_ms()],
        )?;
        if changed == 1 {
            return Ok(());
        }
        let state = self
            .connection
            .query_row(
                "SELECT state FROM document_delete_operations WHERE command_id = ?1",
                [command_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if state.as_deref() == Some("aborted") {
            Ok(())
        } else {
            Err(StoreError::DocumentLifecycleUncertain(format!(
                "delete operation {command_id} could not transition to aborted; observed state {state:?}"
            )))
        }
    }

    /// Commits the immutable deletion receipt and the durable operation state
    /// in one `SQLite` transaction. A failed commit is reconciled before return,
    /// so callers never receive an error after catalogue authority committed.
    fn commit_document_delete_operation(&self, command_id: CommandId) -> Result<()> {
        let operation = self
            .document_delete_operation_by_command(command_id)?
            .ok_or_else(|| {
                StoreError::DocumentLifecycleUncertain(format!(
                    "delete operation {command_id} is missing"
                ))
            })?;
        if operation.state == DeleteOperationState::Committed {
            let deletion = self.document_deletion_by_command(command_id)?;
            return if deletion.is_some_and(|deletion| operation.matches_record(deletion)) {
                Ok(())
            } else {
                Err(StoreError::CorruptDatabase(format!(
                    "committed delete operation {command_id} lacks its exact tombstone"
                )))
            };
        }
        if !matches!(
            operation.state,
            DeleteOperationState::Prepared | DeleteOperationState::Captured
        ) {
            return Err(StoreError::IdempotencyConflict { command_id });
        }

        let transaction = self.connection.unchecked_transaction().map_err(|error| {
            StoreError::DocumentLifecycleUncertain(format!(
                "delete catalogue transaction could not begin: {error}"
            ))
        })?;
        ensure_no_transient_draft_in(&transaction, operation.document_id).map_err(|error| {
            StoreError::DocumentLifecycleUncertain(format!(
                "delete capture remains private because a draft appeared before commit: {error}"
            ))
        })?;
        ensure_no_pending_document_outbox_in(&transaction, operation.document_id).map_err(
            |error| {
                StoreError::DocumentLifecycleUncertain(format!(
                    "delete capture remains private because an outbox entry appeared before commit: {error}"
                ))
            },
        )?;
        validate_document_action_in(
            &transaction,
            operation.document_id,
            &operation.relative_path,
            operation.revision_id,
            operation.blob_id,
            operation.kind_in(&transaction)?,
        )
        .map_err(|error| StoreError::DocumentLifecycleUncertain(error.to_string()))?;
        let committed_at_ms = now_unix_ms();
        transaction
            .execute(
                "INSERT INTO document_deletions(
                     command_id, document_id, revision_id, blob_id,
                     relative_path, recovery_file_name, deleted_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    command_id.to_string(),
                    operation.document_id.to_string(),
                    operation.revision_id.to_string(),
                    operation.blob_id.to_string(),
                    &operation.relative_path,
                    &operation.recovery_file_name,
                    committed_at_ms,
                ],
            )
            .map_err(|error| StoreError::DocumentLifecycleUncertain(error.to_string()))?;
        let changed = transaction
            .execute(
                "UPDATE document_delete_operations
                 SET state = 'committed',
                     captured_at_ms = COALESCE(captured_at_ms, ?2),
                     committed_at_ms = ?2,
                     finished_at_ms = ?2
                 WHERE command_id = ?1 AND state IN ('prepared', 'captured')",
                params![command_id.to_string(), committed_at_ms],
            )
            .map_err(|error| StoreError::DocumentLifecycleUncertain(error.to_string()))?;
        if changed != 1 {
            return Err(StoreError::DocumentLifecycleUncertain(
                "delete operation changed while committing its tombstone".into(),
            ));
        }
        if let Err(error) = transaction.commit() {
            return self.reconcile_document_delete_commit_error(command_id, &operation, &error);
        }
        Ok(())
    }

    fn reconcile_document_delete_commit_error(
        &self,
        command_id: CommandId,
        operation: &DocumentDeleteOperationRecord,
        commit_error: &dyn fmt::Display,
    ) -> Result<()> {
        self.reconcile_document_delete_commit_error_at_boundary(
            command_id,
            operation,
            commit_error,
            |store| store.document_delete_operation_by_command(command_id),
            |store| store.document_deletion_by_command(command_id),
        )
    }

    fn reconcile_document_delete_commit_error_at_boundary<F, G>(
        &self,
        _command_id: CommandId,
        operation: &DocumentDeleteOperationRecord,
        commit_error: &dyn fmt::Display,
        read_operation: F,
        read_deletion: G,
    ) -> Result<()>
    where
        F: FnOnce(&Self) -> Result<Option<DocumentDeleteOperationRecord>>,
        G: FnOnce(&Self) -> Result<Option<DocumentDeletionRecord>>,
    {
        let observed_operation = read_operation(self).map_err(|reconciliation_error| {
            StoreError::DocumentLifecycleUncertain(format!(
                "delete catalogue commit failed: {commit_error}; operation readback failed: {reconciliation_error}"
            ))
        })?;
        let observed_deletion = read_deletion(self).map_err(|reconciliation_error| {
            StoreError::DocumentLifecycleUncertain(format!(
                "delete catalogue commit failed: {commit_error}; tombstone readback failed: {reconciliation_error}"
            ))
        })?;
        let committed = observed_operation
            .is_some_and(|observed| observed.state == DeleteOperationState::Committed)
            && observed_deletion.is_some_and(|deletion| operation.matches_record(deletion));
        if committed {
            Ok(())
        } else {
            Err(StoreError::DocumentLifecycleUncertain(format!(
                "delete catalogue commit outcome is unresolved: {commit_error}"
            )))
        }
    }

    fn recover_document_delete_operations(&self) -> Result<()> {
        let mut statement = self.connection.prepare(
            "SELECT command_id, document_id, revision_id, blob_id, relative_path,
                    recovery_file_name, state
             FROM document_delete_operations
             WHERE state IN ('prepared', 'captured')
             ORDER BY created_at_ms, command_id",
        )?;
        let rows = statement.query_map([], document_delete_operation_from_row)?;
        let operations = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);

        let deleted_directory = self.root.join(DOCUMENT_DELETED_DIRECTORY);
        ensure_private_directory(&deleted_directory)?;
        for operation in operations {
            validate_private_lifecycle_file_name(&operation.recovery_file_name)?;
            let expected_recovery_name = format!(
                "{}.{}.{}.{}.document",
                operation.command_id,
                operation.document_id,
                operation.revision_id,
                operation.blob_id
            );
            if operation.recovery_file_name != expected_recovery_name {
                return Err(StoreError::CorruptDatabase(format!(
                    "delete operation {} has a noncanonical recovery name",
                    operation.command_id
                )));
            }
            let recovery_path = deleted_directory.join(&operation.recovery_file_name);
            match open_exact_private_blob_file(&recovery_path, operation.blob_id).map_err(
                |error| {
                    StoreError::DocumentLifecycleUncertain(format!(
                        "delete operation {} has unsafe private recovery evidence: {error}",
                        operation.command_id
                    ))
                },
            )? {
                Some(recovery) => {
                    recovery
                        .sync_all()
                        .and_then(|()| sync_parent(&recovery_path))
                        .map_err(|error| {
                            StoreError::DocumentLifecycleUncertain(format!(
                                "delete operation {} recovery durability is uncertain: {error}",
                                operation.command_id
                            ))
                        })?;
                    self.commit_document_delete_operation(operation.command_id)?;
                }
                None => self.abort_document_delete_operation(operation.command_id)?,
            }
        }
        Ok(())
    }

    fn abort_document_rename(&self, operation_id: CommandId) -> Result<()> {
        let changed = self.connection.execute(
            "UPDATE document_rename_operations
             SET state = 'aborted', finished_at_ms = ?2
             WHERE operation_id = ?1 AND state IN ('prepared', 'captured')",
            params![operation_id.to_string(), now_unix_ms()],
        )?;
        if changed == 1 {
            self.cleanup_rename_anchor(operation_id);
            return Ok(());
        }
        let state = self
            .connection
            .query_row(
                "SELECT state FROM document_rename_operations WHERE operation_id = ?1",
                [operation_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if state.as_deref() == Some("aborted") {
            self.cleanup_rename_anchor(operation_id);
            Ok(())
        } else {
            Err(StoreError::DocumentLifecycleUncertain(format!(
                "prepared rename {operation_id} could not transition to aborted; observed state {state:?}"
            )))
        }
    }

    fn document_rename_commit_state(
        &self,
        operation_id: CommandId,
        document_id: DocumentId,
        target_relative_path: &str,
    ) -> Result<bool> {
        Ok(self.connection.query_row(
            "SELECT EXISTS(
                 SELECT 1
                 FROM document_rename_operations operation
                 JOIN documents document ON document.document_id = operation.document_id
                 WHERE operation.operation_id = ?1
                   AND operation.state = 'committed'
                   AND document.document_id = ?2
                   AND document.relative_path = ?3
             )",
            params![
                operation_id.to_string(),
                document_id.to_string(),
                target_relative_path,
            ],
            |row| row.get::<_, bool>(0),
        )?)
    }

    fn rollback_prepared_rename(
        &self,
        operation_id: CommandId,
        source_relative_path: &str,
        target_relative_path: &str,
        expected_blob_id: BlobId,
    ) -> Result<RenameRollbackDisposition> {
        let state = self
            .connection
            .query_row(
                "SELECT state FROM document_rename_operations WHERE operation_id = ?1",
                [operation_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::DocumentLifecycleUncertain(format!(
                    "rename operation {operation_id} is missing"
                ))
            })?;
        let state = match state.as_str() {
            "prepared" => RenameOperationState::Prepared,
            "captured" => RenameOperationState::Captured,
            other => {
                return Err(StoreError::DocumentLifecycleUncertain(format!(
                    "rename operation {operation_id} is already {other}"
                )));
            }
        };
        let disposition = rollback_prepared_rename_namespace(
            &self.root,
            operation_id,
            state,
            source_relative_path,
            target_relative_path,
            expected_blob_id,
        )?;
        self.abort_document_rename(operation_id)?;
        Ok(disposition)
    }

    fn rename_failure_after_capture(
        &self,
        operation_id: CommandId,
        source_relative_path: &str,
        target_relative_path: &str,
        expected_blob_id: BlobId,
        cause: impl fmt::Display,
    ) -> StoreError {
        match self.rollback_prepared_rename(
            operation_id,
            source_relative_path,
            target_relative_path,
            expected_blob_id,
        ) {
            Ok(RenameRollbackDisposition::SourceRestored) => {
                StoreError::DocumentLifecycleUncertain(format!(
                    "{cause}; the prepared rename was rolled back to its source name"
                ))
            }
            Ok(RenameRollbackDisposition::PreservedConflict) => {
                StoreError::DocumentLifecycleUncertain(format!(
                    "{cause}; a recreated source name and the original captured object were both preserved"
                ))
            }
            Ok(RenameRollbackDisposition::SourceMissing) => {
                StoreError::DocumentLifecycleUncertain(format!(
                    "{cause}; lifecycle endpoints were preserved but the expected source is externally missing"
                ))
            }
            Err(recovery_error) => StoreError::DocumentLifecycleUncertain(format!(
                "{cause}; prepared rename recovery remains pending: {recovery_error}"
            )),
        }
    }

    fn recover_document_rename_operations(&self) -> Result<()> {
        let mut statement = self.connection.prepare(
            "SELECT operation_id, state, source_relative_path, target_relative_path, blob_id
             FROM document_rename_operations
             WHERE state IN ('prepared', 'captured')
             ORDER BY created_at_ms, operation_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let pending = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);
        for (operation_id, state, source_path, target_path, blob_id) in pending {
            let operation_id = parse_id(&operation_id, "document rename operation_id")?;
            let state = match state.as_str() {
                "prepared" => RenameOperationState::Prepared,
                "captured" => RenameOperationState::Captured,
                _ => unreachable!("query restricts live rename states"),
            };
            let disposition = rollback_prepared_rename_namespace(
                &self.root,
                operation_id,
                state,
                &source_path,
                &target_path,
                parse_blob_id(&blob_id)?,
            )
            .map_err(|error| {
                StoreError::DocumentLifecycleUncertain(format!(
                    "rename operation {operation_id} recovery remains fail-closed: {error}"
                ))
            })?;
            let _ = disposition;
            self.abort_document_rename(operation_id)?;
        }
        Ok(())
    }

    fn cleanup_rename_anchor(&self, operation_id: CommandId) {
        let rename_directory = self.root.join(DOCUMENT_RENAME_DIRECTORY);
        let _ = remove_if_present(&rename_directory.join(format!("{operation_id}.anchor")));
    }

    fn cleanup_terminal_rename_anchors(&self) {
        let operation_ids = (|| -> Result<Vec<String>> {
            let mut statement = self.connection.prepare(
                "SELECT operation_id FROM document_rename_operations
                 WHERE state IN ('committed', 'aborted') ORDER BY operation_id",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })();
        let Ok(operation_ids) = operation_ids else {
            return;
        };
        for operation_id in operation_ids {
            let Ok(operation_id) = parse_id::<CommandId>(&operation_id, "rename operation_id")
            else {
                continue;
            };
            self.cleanup_rename_anchor(operation_id);
        }
    }

    pub fn read_document(&self, relative_path: impl AsRef<Path>) -> Result<LoadedDocument> {
        self.open_document_file(relative_path)?.into_document()
    }

    /// Opens one no-follow descriptor and binds its exact UTF-8 bytes to the
    /// store's current document, revision, artifact, and blob identity.
    pub fn open_document_file(
        &self,
        relative_path: impl AsRef<Path>,
    ) -> Result<DocumentFileAuthority> {
        self.reconcile_document_lifecycle()?;
        let normalized = normalize_document_path(relative_path.as_ref())?;
        let document = self
            .document_by_path(&normalized)?
            .ok_or_else(|| StoreError::NoActiveRevision(normalized.clone()))?;
        let active = self
            .active_revision(document.id)?
            .ok_or_else(|| StoreError::NoActiveRevision(normalized.clone()))?;
        let visible_path = inspect_document_path(&self.root, &normalized)?;
        let mut file = match BoundedNoFollowFile::open(&visible_path, MAX_DOCUMENT_BYTES) {
            Ok(file) => file,
            Err(StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::ExternalVisibleFileDeleted(normalized));
            }
            Err(error) => return Err(error),
        };
        let bytes = file.read()?;
        file.ensure_path_binding()?;
        if BlobId::digest(&bytes) != active.blob_id {
            return Err(StoreError::UncheckpointedVisibleChange(normalized));
        }
        let text = String::from_utf8(bytes).map_err(loom_document::DocumentError::from)?;
        Ok(DocumentFileAuthority {
            project_id: self.manifest.project_id,
            document: LoadedDocument {
                document_id: document.id,
                relative_path: normalized,
                kind: document.kind,
                revision_id: active.revision_id,
                artifact_id: active.artifact_id,
                blob_id: active.blob_id,
                text,
            },
            file,
        })
    }

    /// Captures both sides of an external-edit reconciliation without changing
    /// the database, visible manuscript, outbox, or draft journal.
    pub fn reconciliation_snapshot(
        &self,
        relative_path: impl AsRef<Path>,
    ) -> Result<DocumentReconciliationSnapshot> {
        self.reconciliation_snapshot_at_boundary(relative_path, |_| Ok(()))
    }

    fn reconciliation_snapshot_at_boundary<F>(
        &self,
        relative_path: impl AsRef<Path>,
        after_visible_open: F,
    ) -> Result<DocumentReconciliationSnapshot>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
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
        let visible = match BoundedNoFollowFile::open(&visible_path, MAX_DOCUMENT_BYTES) {
            Ok(mut opened) => {
                after_visible_open(&visible_path)?;
                let bytes = opened.read()?;
                if opened.path_has_identity(&visible_path)? {
                    let blob_id = BlobId::digest(&bytes);
                    let text =
                        String::from_utf8(bytes).map_err(loom_document::DocumentError::from)?;
                    Some(VisibleDocumentSnapshot { blob_id, text })
                } else {
                    None
                }
            }
            Err(StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
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
        self.read_blob_bounded(blob_id, MAX_DOCUMENT_BYTES)
    }

    pub(crate) fn read_blob_bounded(&self, blob_id: BlobId, max_bytes: u64) -> Result<Vec<u8>> {
        let path = self.blob_path(blob_id);
        reject_symlink_target(&path)?;
        if !path.exists() {
            return Err(StoreError::MissingBlob { blob_id, path });
        }
        let bytes = read_bounded(&path, max_bytes)?;
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
            // `read_blob` is a document API and is intentionally capped at
            // 128 MiB. Research receipts have their own larger, checked
            // bounds, so idempotent content-addressed insertion must verify
            // against the exact caller-owned byte length instead of silently
            // inheriting the document limit.
            let exact_len = u64::try_from(bytes.len()).map_err(|_| {
                StoreError::CorruptDatabase(
                    "content blob length does not fit the store size domain".into(),
                )
            })?;
            let existing = self.read_blob_bounded(blob_id, exact_len)?;
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
                "SELECT document.document_id, document.document_kind
                 FROM documents document
                 WHERE document.relative_path = ?1
                   AND NOT EXISTS (
                       SELECT 1 FROM document_deletions deletion
                       WHERE deletion.document_id = document.document_id
                   )",
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
        let (revision_id, relative_path, target_blob_id, expected_visible_blob_id, state) = row;
        if state == "completed" {
            return Ok(OutboxResult::AlreadyApplied);
        }
        if state != "pending" {
            return Err(StoreError::CorruptDatabase(format!(
                "outbox {outbox_id} has invalid state `{state}`"
            )));
        }

        let explicitly_deleted = self.connection.query_row(
            "SELECT EXISTS(
                 SELECT 1
                 FROM revisions revision
                 JOIN document_deletions deletion
                   ON deletion.document_id = revision.document_id
                 WHERE revision.revision_id = ?1
             )",
            [&revision_id],
            |row| row.get::<_, bool>(0),
        )?;
        if explicitly_deleted {
            self.complete_outbox(outbox_id)?;
            return Ok(OutboxResult::AlreadyApplied);
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
    match read_bounded_no_follow(path, MAX_DOCUMENT_BYTES) {
        Ok(bytes) => Ok(Some(BlobId::digest(&bytes))),
        Err(StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CapturedPathDisposition {
    Restored,
    PreservedPrivate,
    Missing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExactMoveDisposition {
    Moved,
    DestinationOccupied,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RenameRollbackDisposition {
    SourceRestored,
    PreservedConflict,
    SourceMissing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RenameOperationState {
    Prepared,
    Captured,
}

/// Restores the exact object currently in a private capture slot without
/// replacing a concurrently recreated visible name. A retained descriptor
/// reconciles namespace state when the no-replace move reports an error after
/// the kernel may already have moved the name.
fn restore_captured_path_no_clobber(
    captured_path: &Path,
    visible_path: &Path,
) -> Result<CapturedPathDisposition> {
    let captured = match BoundedNoFollowFile::open(captured_path, MAX_DOCUMENT_BYTES) {
        Ok(captured) => captured,
        Err(StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CapturedPathDisposition::Missing);
        }
        Err(error) => return Err(error),
    };
    let moved = rename_if_absent(captured_path, visible_path);
    let at_visible = captured.path_has_identity(visible_path).unwrap_or(false);
    let at_capture = captured.path_has_identity(captured_path).unwrap_or(false);
    match moved {
        Ok(true) if at_visible && !at_capture => Ok(CapturedPathDisposition::Restored),
        Ok(false) | Err(_) if at_capture => Ok(CapturedPathDisposition::PreservedPrivate),
        Err(error) if at_visible && !at_capture => {
            captured
                .sync_all()
                .and_then(|()| sync_rename_parents(captured_path, visible_path))
                .map_err(|sync_error| {
                    StoreError::DocumentLifecycleUncertain(format!(
                        "{error}; captured path moved but durability reconciliation failed: {sync_error}"
                    ))
                })?;
            Ok(CapturedPathDisposition::Restored)
        }
        Ok(_) | Err(_) => Err(StoreError::DocumentLifecycleUncertain(
            "captured path namespace could not be reconciled".into(),
        )),
    }
}

fn open_exact_blob_file(
    path: &Path,
    expected_blob_id: BlobId,
) -> Result<Option<BoundedNoFollowFile>> {
    let mut opened = match BoundedNoFollowFile::open(path, MAX_DOCUMENT_BYTES) {
        Ok(opened) => opened,
        Err(StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let bytes = opened.read()?;
    opened.ensure_path_binding()?;
    if BlobId::digest(&bytes) == expected_blob_id {
        Ok(Some(opened))
    } else {
        Ok(None)
    }
}

/// Opens private lifecycle evidence without confusing corruption with
/// absence. A live operation may be retired only when its deterministic slot
/// is genuinely absent; wrong bytes, symlinks, and non-regular objects remain
/// fail-closed evidence for explicit recovery.
fn open_exact_private_blob_file(
    path: &Path,
    expected_blob_id: BlobId,
) -> Result<Option<BoundedNoFollowFile>> {
    let mut opened = match BoundedNoFollowFile::open(path, MAX_DOCUMENT_BYTES) {
        Ok(opened) => opened,
        Err(StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let bytes = opened.read()?;
    opened.ensure_path_binding()?;
    let actual = BlobId::digest(&bytes);
    if actual != expected_blob_id {
        return Err(StoreError::CorruptBlob {
            path: path.to_path_buf(),
            expected: expected_blob_id,
            actual,
        });
    }
    Ok(Some(opened))
}

/// Moves the path still bound to `held` with no replacement. The retained
/// descriptor distinguishes a completed move from an fsync-after-move error
/// and detects a source replacement captured at the syscall boundary.
fn move_held_path_if_absent(
    held: &BoundedNoFollowFile,
    source_path: &Path,
    destination_path: &Path,
) -> Result<ExactMoveDisposition> {
    let moved = rename_if_absent(source_path, destination_path);
    let at_destination = held.path_has_identity(destination_path).unwrap_or(false);
    let at_source = held.path_has_identity(source_path).unwrap_or(false);
    match moved {
        Ok(true) if at_destination && !at_source => Ok(ExactMoveDisposition::Moved),
        Ok(false) if at_source => Ok(ExactMoveDisposition::DestinationOccupied),
        Err(error) if at_destination && !at_source => {
            held.sync_all()
                .and_then(|()| sync_rename_parents(source_path, destination_path))
                .map_err(|sync_error| {
                    StoreError::DocumentLifecycleUncertain(format!(
                        "{error}; exact path moved but durability reconciliation failed: {sync_error}"
                    ))
                })?;
            Ok(ExactMoveDisposition::Moved)
        }
        Ok(_) | Err(_) => {
            let preservation = restore_captured_path_no_clobber(destination_path, source_path);
            Err(StoreError::DocumentLifecycleUncertain(format!(
                "exact path move could not be reconciled; raced object preservation: {preservation:?}"
            )))
        }
    }
}

fn rollback_prepared_rename_namespace(
    root: &Path,
    operation_id: CommandId,
    state: RenameOperationState,
    source_relative_path: &str,
    target_relative_path: &str,
    expected_blob_id: BlobId,
) -> Result<RenameRollbackDisposition> {
    let rename_directory = root.join(DOCUMENT_RENAME_DIRECTORY);
    ensure_private_directory(&rename_directory)?;
    let capture_path = rename_directory.join(format!("{operation_id}.capture"));
    let anchor_path = rename_directory.join(format!("{operation_id}.anchor"));
    let source_path = inspect_document_path(root, source_relative_path)?;
    let target_path = inspect_document_path(root, target_relative_path)?;

    let source = open_exact_blob_file(&source_path, expected_blob_id)?;
    let mut captured = open_exact_private_blob_file(&capture_path, expected_blob_id)?;
    let anchor = if state == RenameOperationState::Captured {
        open_exact_private_blob_file(&anchor_path, expected_blob_id)?
    } else {
        // A prepared operation never trusts a visible target, even if an
        // anchor was durably linked just before the state transition failed.
        // Its exact private capture is sufficient to roll back safely.
        return rollback_prepared_rename_without_anchor(
            source.as_ref(),
            captured,
            &capture_path,
            &source_path,
        );
    };
    if captured.as_ref().is_some_and(|capture| {
        anchor
            .as_ref()
            .is_some_and(|anchor| !capture.same_identity(anchor))
    }) {
        return Err(StoreError::DocumentLifecycleUncertain(format!(
            "captured rename {operation_id} private slot no longer matches its ownership anchor"
        )));
    }
    let target = match anchor.as_ref() {
        Some(anchor) => open_exact_blob_file(&target_path, expected_blob_id)?
            .filter(|target| target.same_identity(anchor)),
        None => None,
    };
    if source.is_some() {
        return if captured.is_some() || target.is_some() {
            Ok(RenameRollbackDisposition::PreservedConflict)
        } else {
            Ok(RenameRollbackDisposition::SourceRestored)
        };
    }

    if captured.is_none()
        && let Some(target) = target
    {
        match move_held_path_if_absent(&target, &target_path, &capture_path)? {
            ExactMoveDisposition::Moved => {
                captured = open_exact_private_blob_file(&capture_path, expected_blob_id)?;
            }
            ExactMoveDisposition::DestinationOccupied => {
                return Err(StoreError::DocumentLifecycleUncertain(
                    "private rename capture is occupied while the exact target remains visible"
                        .into(),
                ));
            }
        }
    }
    let captured = captured.or(anchor).ok_or_else(|| {
        StoreError::DocumentLifecycleUncertain(format!(
            "captured rename {operation_id} has neither exact private capture nor ownership anchor"
        ))
    })?;
    let captured_path = if captured.path_has_identity(&capture_path).unwrap_or(false) {
        capture_path
    } else {
        anchor_path
    };
    match move_held_path_if_absent(&captured, &captured_path, &source_path)? {
        ExactMoveDisposition::Moved => Ok(RenameRollbackDisposition::SourceRestored),
        ExactMoveDisposition::DestinationOccupied => {
            Ok(RenameRollbackDisposition::PreservedConflict)
        }
    }
}

fn rollback_prepared_rename_without_anchor(
    source: Option<&BoundedNoFollowFile>,
    captured: Option<BoundedNoFollowFile>,
    capture_path: &Path,
    source_path: &Path,
) -> Result<RenameRollbackDisposition> {
    if source.is_some() {
        return if captured.is_some() {
            Ok(RenameRollbackDisposition::PreservedConflict)
        } else {
            Ok(RenameRollbackDisposition::SourceRestored)
        };
    }
    let Some(captured) = captured else {
        return Ok(RenameRollbackDisposition::SourceMissing);
    };
    match move_held_path_if_absent(&captured, capture_path, source_path)? {
        ExactMoveDisposition::Moved => Ok(RenameRollbackDisposition::SourceRestored),
        ExactMoveDisposition::DestinationOccupied => {
            Ok(RenameRollbackDisposition::PreservedConflict)
        }
    }
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => sync_parent(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn acquire_project_lease(loom_dir: &Path, root: &Path) -> Result<ProjectLease> {
    let lease_path = loom_dir.join(PROJECT_LEASE_FILE);
    create_private_file_if_absent(&lease_path)?;
    reject_symlink_target(&lease_path)?;

    let lease = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lease_path)?;
    if !lease.metadata()?.is_file() {
        return Err(StoreError::NotRegularFile(lease_path));
    }
    match fs4::FileExt::try_lock(&lease) {
        Ok(()) => Ok(()),
        Err(TryLockError::WouldBlock) => Err(StoreError::ProjectAlreadyOpen(root.to_path_buf())),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }?;

    let mut process_leases = process_project_leases().lock().map_err(|_| {
        StoreError::Io(std::io::Error::other(
            "process-local project lease registry is poisoned",
        ))
    })?;
    if !process_leases.insert(root.to_path_buf()) {
        return Err(StoreError::ProjectAlreadyOpen(root.to_path_buf()));
    }
    drop(process_leases);
    Ok(ProjectLease {
        _file: lease,
        root: root.to_path_buf(),
    })
}

fn process_project_leases() -> &'static Mutex<BTreeSet<PathBuf>> {
    static LEASES: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();
    LEASES.get_or_init(|| Mutex::new(BTreeSet::new()))
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
    pub display_title: Option<String>,
    pub kind: DocumentKind,
    pub active_revision_id: Option<RevisionId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(clippy::struct_field_names)]
struct DocumentDeletionRecord {
    document_id: DocumentId,
    revision_id: RevisionId,
    blob_id: BlobId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeleteOperationState {
    Prepared,
    Captured,
    Committed,
    Aborted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DocumentDeleteOperationRecord {
    command_id: CommandId,
    document_id: DocumentId,
    revision_id: RevisionId,
    blob_id: BlobId,
    relative_path: String,
    recovery_file_name: String,
    state: DeleteOperationState,
}

impl DocumentDeleteOperationRecord {
    fn matches(&self, document_id: DocumentId, revision_id: RevisionId, blob_id: BlobId) -> bool {
        self.document_id == document_id
            && self.revision_id == revision_id
            && self.blob_id == blob_id
    }

    fn matches_at_path(
        &self,
        document_id: DocumentId,
        revision_id: RevisionId,
        blob_id: BlobId,
        relative_path: &str,
    ) -> bool {
        self.matches(document_id, revision_id, blob_id) && self.relative_path == relative_path
    }

    fn matches_record(&self, deletion: DocumentDeletionRecord) -> bool {
        self.matches(deletion.document_id, deletion.revision_id, deletion.blob_id)
    }

    fn kind_in(&self, connection: &Connection) -> Result<DocumentKind> {
        let kind = connection.query_row(
            "SELECT document_kind FROM documents WHERE document_id = ?1",
            [self.document_id.to_string()],
            |row| row.get::<_, String>(0),
        )?;
        DocumentKind::from_str(&kind)
            .map_err(|error| StoreError::CorruptDatabase(error.to_string()))
    }
}

fn document_delete_operation_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<DocumentDeleteOperationRecord> {
    let command_id = row.get::<_, String>(0)?;
    let document_id = row.get::<_, String>(1)?;
    let revision_id = row.get::<_, String>(2)?;
    let blob_id = row.get::<_, String>(3)?;
    let state = row.get::<_, String>(6)?;
    Ok(DocumentDeleteOperationRecord {
        command_id: CommandId::from_str(&command_id).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        document_id: DocumentId::from_str(&document_id).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        revision_id: RevisionId::from_str(&revision_id).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        blob_id: BlobId::from_str(&blob_id).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        relative_path: row.get(4)?,
        recovery_file_name: row.get(5)?,
        state: match state.as_str() {
            "prepared" => DeleteOperationState::Prepared,
            "captured" => DeleteOperationState::Captured,
            "committed" => DeleteOperationState::Committed,
            "aborted" => DeleteOperationState::Aborted,
            _ => {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    6,
                    rusqlite::types::Type::Text,
                    format!("invalid document delete operation state `{state}`").into(),
                ));
            }
        },
    })
}

fn document_delete_operation_by_command_in(
    connection: &Connection,
    command_id: CommandId,
) -> Result<Option<DocumentDeleteOperationRecord>> {
    connection
        .query_row(
            "SELECT command_id, document_id, revision_id, blob_id, relative_path,
                    recovery_file_name, state
             FROM document_delete_operations WHERE command_id = ?1",
            [command_id.to_string()],
            document_delete_operation_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn document_deletion_by_command_in(
    connection: &Connection,
    command_id: CommandId,
) -> Result<Option<DocumentDeletionRecord>> {
    let row: Option<(String, String, String)> = connection
        .query_row(
            "SELECT document_id, revision_id, blob_id
             FROM document_deletions WHERE command_id = ?1",
            [command_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    row.map(|(document_id, revision_id, blob_id)| {
        Ok(DocumentDeletionRecord {
            document_id: parse_id(&document_id, "document deletion document_id")?,
            revision_id: parse_id(&revision_id, "document deletion revision_id")?,
            blob_id: parse_blob_id(&blob_id)?,
        })
    })
    .transpose()
}

fn validate_document_action_in(
    connection: &Connection,
    document_id: DocumentId,
    relative_path: &str,
    expected_revision_id: RevisionId,
    expected_blob_id: BlobId,
    expected_kind: DocumentKind,
) -> Result<()> {
    let row: Option<(String, String, String)> = connection
        .query_row(
            "SELECT document.document_kind, revision.revision_id, artifact.blob_id
             FROM documents document
             JOIN revisions revision ON revision.document_id = document.document_id
             JOIN artifacts artifact ON artifact.artifact_id = revision.artifact_id
             WHERE document.document_id = ?1
               AND document.relative_path = ?2
               AND revision.revision_id = (
                   SELECT active.revision_id
                   FROM revisions active
                   WHERE active.document_id = document.document_id
                   ORDER BY active.created_at_ms DESC, active.revision_id DESC
                   LIMIT 1
               )
               AND NOT EXISTS (
                   SELECT 1 FROM document_deletions deletion
                   WHERE deletion.document_id = document.document_id
               )",
            params![document_id.to_string(), relative_path],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((kind, actual_revision_id, actual_blob_id)) = row else {
        return Err(StoreError::DocumentFileAuthorityMismatch);
    };
    let kind = DocumentKind::from_str(&kind)
        .map_err(|error| StoreError::CorruptDatabase(error.to_string()))?;
    if kind != expected_kind {
        return Err(StoreError::DocumentFileAuthorityMismatch);
    }
    let actual_revision_id = parse_id(&actual_revision_id, "revision_id")?;
    if actual_revision_id != expected_revision_id {
        return Err(StoreError::SourceRevisionMismatch {
            expected: expected_revision_id,
            actual: actual_revision_id,
        });
    }
    let actual_blob_id = parse_blob_id(&actual_blob_id)?;
    if actual_blob_id != expected_blob_id {
        return Err(StoreError::SourceBlobMismatch {
            expected: expected_blob_id,
            actual: actual_blob_id,
        });
    }
    Ok(())
}

fn ensure_no_transient_draft_in(connection: &Connection, document_id: DocumentId) -> Result<()> {
    let pending = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM transient_drafts WHERE document_id = ?1
         )",
        [document_id.to_string()],
        |row| row.get::<_, bool>(0),
    )?;
    if pending {
        Err(StoreError::DocumentHasTransientDraft(document_id))
    } else {
        Ok(())
    }
}

fn ensure_no_pending_document_outbox_in(
    connection: &Connection,
    document_id: DocumentId,
) -> Result<()> {
    let pending = connection.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM visible_file_outbox outbox
             JOIN revisions revision ON revision.revision_id = outbox.revision_id
             WHERE revision.document_id = ?1 AND outbox.state = 'pending'
         )",
        [document_id.to_string()],
        |row| row.get::<_, bool>(0),
    )?;
    if pending {
        Err(StoreError::DocumentHasPendingOutbox(document_id))
    } else {
        Ok(())
    }
}

fn validate_private_lifecycle_file_name(file_name: &str) -> Result<()> {
    if file_name.is_empty()
        || file_name.len() > MAX_DOCUMENT_FILE_NAME_BYTES
        || file_name == "."
        || file_name == ".."
        || file_name.contains(['/', '\\'])
        || file_name.chars().any(char::is_control)
    {
        return Err(StoreError::CorruptDatabase(
            "document lifecycle recovery name is not one bounded filename component".into(),
        ));
    }
    Ok(())
}

fn normalize_document_display_title(requested: &str) -> Result<String> {
    let title = requested.trim();
    if title.is_empty()
        || title.len() > MAX_DOCUMENT_TITLE_BYTES
        || title.chars().any(char::is_control)
    {
        return Err(StoreError::InvalidDocumentTitle {
            max_bytes: MAX_DOCUMENT_TITLE_BYTES,
        });
    }
    Ok(title.to_owned())
}

fn document_path_reservation_key(path: &str) -> String {
    let normalized = path.nfc().collect::<String>();
    unicase::UniCase::unicode(normalized)
        .to_folded_case()
        .nfc()
        .collect()
}

fn document_path_conflicts_in(
    connection: &Connection,
    relative_path: &str,
    owner: Option<DocumentId>,
) -> Result<bool> {
    let requested_key = document_path_reservation_key(relative_path);
    let mut statement = connection.prepare(
        "SELECT document_id, relative_path FROM documents
         UNION ALL
         SELECT document_id, source_relative_path
         FROM document_rename_operations
         WHERE state IN ('prepared', 'captured')
         UNION ALL
         SELECT document_id, target_relative_path
         FROM document_rename_operations
         WHERE state IN ('prepared', 'captured')",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (document_id, reserved_path) = row?;
        let document_id = parse_id(&document_id, "document path reservation document_id")?;
        if owner == Some(document_id) {
            continue;
        }
        if document_path_reservation_key(&reserved_path) == requested_key {
            return Ok(true);
        }
    }
    Ok(false)
}

fn document_path_for_title(relative_path: &str, title: &str) -> Result<String> {
    let relative_path = normalize_document_path(Path::new(relative_path))?;
    let (parent, source_file_name) = relative_path
        .rsplit_once('/')
        .unwrap_or(("", &relative_path));
    let extension = Path::new(source_file_name)
        .extension()
        .and_then(std::ffi::OsStr::to_str);
    let portable_forbidden = ['/', '\\', '<', '>', ':', '"', '|', '?', '*'];
    let reserved_stem = title
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let windows_reserved = matches!(reserved_stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || reserved_stem.strip_prefix("COM").is_some_and(|suffix| {
            matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        })
        || reserved_stem.strip_prefix("LPT").is_some_and(|suffix| {
            matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        });
    let file_name_bytes =
        title.len() + extension.map_or(0, |extension| extension.len().saturating_add(1));
    if title.starts_with('.')
        || title.ends_with('.')
        || title.ends_with(' ')
        || title
            .chars()
            .any(|character| portable_forbidden.contains(&character))
        || windows_reserved
        || file_name_bytes > MAX_DOCUMENT_FILE_NAME_BYTES
    {
        return Err(StoreError::InvalidDocumentFileName {
            max_bytes: MAX_DOCUMENT_FILE_NAME_BYTES,
        });
    }
    let mut file_name = title.to_owned();
    if let Some(extension) = extension {
        file_name.push('.');
        file_name.push_str(extension);
    }
    normalize_document_path(&Path::new(parent).join(file_name))
}

fn validate_stored_document_display_title(stored: Option<String>) -> Result<Option<String>> {
    let Some(stored) = stored else {
        return Ok(None);
    };
    match normalize_document_display_title(&stored) {
        Ok(canonical) if canonical == stored => Ok(Some(stored)),
        _ => Err(StoreError::CorruptDatabase(
            "document display_title is not canonical".into(),
        )),
    }
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

/// A store-derived visible document held through the exact descriptor whose
/// bytes were checked against the active blob. Paths remain native-only and
/// are never part of the renderer command contract.
#[derive(Debug)]
pub struct DocumentFileAuthority {
    project_id: ProjectId,
    document: LoadedDocument,
    file: BoundedNoFollowFile,
}

impl DocumentFileAuthority {
    pub const fn document(&self) -> &LoadedDocument {
        &self.document
    }

    fn revalidate(&mut self) -> Result<()> {
        let bytes = self.file.read()?;
        self.file.ensure_path_binding()?;
        if BlobId::digest(&bytes) != self.document.blob_id {
            return Err(StoreError::UncheckpointedVisibleChange(
                self.document.relative_path.clone(),
            ));
        }
        Ok(())
    }

    /// Returns the reveal target resolved from the retained descriptor. The
    /// stored path is used only as a binding expectation and is never reopened
    /// to manufacture the returned target.
    pub fn reveal_path(&mut self) -> Result<PathBuf> {
        self.revalidate()?;
        let descriptor_path = self.file.descriptor_path()?;
        if descriptor_path != self.file.path() {
            return Err(StoreError::VisibleFileIdentityChanged(
                self.file.path().to_path_buf(),
            ));
        }
        self.file.ensure_path_binding()?;
        Ok(descriptor_path)
    }

    pub fn into_document(self) -> Result<LoadedDocument> {
        self.file.ensure_path_binding()?;
        Ok(self.document)
    }
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

    #[cfg(not(unix))]
    #[test]
    fn unsupported_storage_rejects_initialize_and_open_without_mutation() {
        let directory = tempdir().expect("temporary root");
        let missing = directory.path().join("absent");
        assert!(matches!(
            ProjectStore::initialize(&missing, "Novel"),
            Err(StoreError::UnsupportedStoragePlatform)
        ));
        assert!(!missing.exists());
        let existing = directory.path().join("existing");
        fs::create_dir(&existing).expect("existing root");
        let manuscript = existing.join("manuscript.txt");
        fs::write(&manuscript, b"irreplaceable prose").expect("manuscript");
        assert!(matches!(
            ProjectStore::open(&existing),
            Err(StoreError::UnsupportedStoragePlatform)
        ));
        assert!(matches!(
            ProjectStore::initialize(&existing, "Novel"),
            Err(StoreError::UnsupportedStoragePlatform)
        ));
        assert_eq!(
            fs::read(&manuscript).expect("preserved manuscript"),
            b"irreplaceable prose"
        );
        assert_eq!(fs::read_dir(&existing).expect("unchanged root").count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn opening_a_damaged_project_never_recreates_its_database() {
        let (_directory, store) = new_store();
        let root = store.root().to_path_buf();
        drop(store);
        let database = root.join(".loom").join(DATABASE_FILE);
        fs::remove_file(&database).expect("remove fixture database");
        assert!(ProjectStore::open(&root).is_err());
        assert!(
            !database.exists(),
            "opening must not create a replacement database"
        );
        fs::write(&database, []).expect("empty database fixture");
        assert!(ProjectStore::open(&root).is_err());
        assert_eq!(
            fs::metadata(&database).expect("preserved empty file").len(),
            0
        );
    }

    fn new_store() -> (tempfile::TempDir, ProjectStore) {
        let directory = tempdir().expect("temporary project root");
        let project = directory.path().join("Novel");
        let (store, _) = ProjectStore::initialize(&project, "Novel").expect("initialize project");
        fs::create_dir(project.join("manuscript")).expect("fixture manuscript directory");
        (directory, store)
    }

    fn record_prepared_rename(
        store: &ProjectStore,
        loaded: &LoadedDocument,
        source_relative_path: &str,
        target_relative_path: &str,
    ) -> CommandId {
        let operation_id = CommandId::new();
        store
            .connection
            .execute(
                "INSERT INTO document_rename_operations(
                     operation_id, document_id, revision_id, blob_id,
                     source_relative_path, target_relative_path, target_display_title,
                     state, created_at_ms, captured_at_ms, committed_at_ms, finished_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'Target', 'prepared', ?7, NULL, NULL, NULL)",
                params![
                    operation_id.to_string(),
                    loaded.document_id.to_string(),
                    loaded.revision_id.to_string(),
                    loaded.blob_id.to_string(),
                    source_relative_path,
                    target_relative_path,
                    now_unix_ms(),
                ],
            )
            .expect("record prepared rename");
        operation_id
    }

    fn mark_rename_captured(store: &ProjectStore, operation_id: CommandId) {
        let rename_directory = store.root.join(DOCUMENT_RENAME_DIRECTORY);
        let capture_path = rename_directory.join(format!("{operation_id}.capture"));
        let anchor_path = rename_directory.join(format!("{operation_id}.anchor"));
        assert_eq!(
            store
                .connection
                .execute(
                    "UPDATE document_rename_operations
                     SET state = 'captured', captured_at_ms = ?2
                     WHERE operation_id = ?1 AND state = 'prepared'",
                    params![operation_id.to_string(), now_unix_ms()],
                )
                .expect("mark captured rename"),
            1
        );
        assert!(
            hard_link_if_absent(&capture_path, &anchor_path).expect("link rename ownership anchor")
        );
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

    #[test]
    #[cfg(unix)]
    fn registered_document_probe_is_exact_and_survives_reopen() {
        let (_directory, mut store) = new_store();
        let root = store.root().to_path_buf();
        store
            .create_document_if_absent(
                "manuscript/001.md",
                DocumentContent::Prose("exact indexed identity\n".into()),
                "register exact document",
            )
            .expect("create indexed document");
        let loaded = store
            .read_document("manuscript/001.md")
            .expect("read registered document");
        let registered = loaded.document_id;
        let foreign = DocumentId::new();

        assert!(
            store
                .document_is_registered(registered)
                .expect("probe registered document")
        );
        assert!(
            !store
                .document_is_registered(foreign)
                .expect("probe foreign document")
        );
        assert_eq!(
            store
                .registered_document(registered)
                .expect("resolve registered document")
                .expect("registered document"),
            DocumentSummary {
                document_id: registered,
                relative_path: "manuscript/001.md".to_owned(),
                display_title: None,
                kind: DocumentKind::Prose,
                active_revision_id: Some(loaded.revision_id),
            }
        );
        assert_eq!(
            store
                .registered_document(foreign)
                .expect("resolve foreign document"),
            None
        );

        drop(store);
        let reopened = ProjectStore::open(&root).expect("reopen project");
        assert!(
            reopened
                .document_is_registered(registered)
                .expect("probe registered document after reopen")
        );
        assert!(
            !reopened
                .document_is_registered(foreign)
                .expect("probe foreign document after reopen")
        );
        assert_eq!(
            reopened
                .registered_document(registered)
                .expect("resolve registered document after reopen")
                .expect("registered document after reopen")
                .active_revision_id,
            Some(loaded.revision_id)
        );
    }

    #[test]
    fn document_title_codec_is_portable_and_rejects_invalid_input() {
        let target =
            document_path_for_title("manuscript/chapters/Untitled.md", "A Portable Manuscript")
                .expect("construct target path");

        assert_eq!(target, "manuscript/chapters/A Portable Manuscript.md");
        assert!(!target.contains('\\'));
        for invalid in ["../escape", "CON", "trailing.", "bad:name"] {
            assert!(matches!(
                document_path_for_title("manuscript/Untitled.md", invalid),
                Err(StoreError::InvalidDocumentFileName { .. })
            ));
        }
        for invalid in ["   ".to_owned(), "line\nbreak".to_owned(), "é".repeat(129)] {
            assert!(matches!(
                normalize_document_display_title(&invalid),
                Err(StoreError::InvalidDocumentTitle {
                    max_bytes: MAX_DOCUMENT_TITLE_BYTES
                })
            ));
        }
        assert_eq!(
            document_path_reservation_key("manuscript/Café.md"),
            document_path_reservation_key("MANUSCRIPT/Cafe\u{301}.md")
        );
    }

    // These positive state-machine tests mirror `ensure_document_lifecycle_supported`;
    // unsupported targets retain the explicit fail-closed test below.
    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn document_rename_moves_the_visible_file_and_preserves_identity() {
        let (directory, mut store) = new_store();
        let root = store.root().to_path_buf();
        store
            .create_document_if_absent(
                "manuscript/Untitled.md",
                DocumentContent::Prose("ordinary manuscript\n".into()),
                "new document",
            )
            .expect("create document");
        let mut authority = store
            .open_document_file("manuscript/Untitled.md")
            .expect("capture exact source");

        let renamed = store
            .rename_document(&mut authority, "  Éowyn's Choice  ")
            .expect("rename document");

        assert_eq!(renamed.display_title.as_deref(), Some("Éowyn's Choice"));
        assert_eq!(renamed.relative_path, "manuscript/Éowyn's Choice.md");
        assert_eq!(
            fs::read_to_string(root.join("manuscript/Éowyn's Choice.md"))
                .expect("read renamed manuscript"),
            "ordinary manuscript\n"
        );
        assert!(!root.join("manuscript/Untitled.md").exists());
        assert!(
            fs::read_dir(root.join(DOCUMENT_RENAME_DIRECTORY))
                .expect("rename private directory")
                .all(|entry| !entry
                    .expect("rename private entry")
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".anchor"))
        );

        let document_id = renamed.document_id;
        drop(store);
        let reopened = ProjectStore::open(&root).expect("reopen titled project");
        assert_eq!(
            reopened
                .registered_document(document_id)
                .expect("read title after reopen")
                .expect("registered document")
                .display_title
                .as_deref(),
            Some("Éowyn's Choice")
        );
        drop(reopened);
        drop(directory);
    }

    #[cfg(not(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    )))]
    #[test]
    #[cfg(unix)]
    fn unsupported_lifecycle_platform_fails_before_durable_intent_or_capture() {
        let (_directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Source.md",
                DocumentContent::Prose("unchanged source\n".into()),
                "unsupported platform fixture",
            )
            .expect("create source");
        let loaded = store
            .read_document("manuscript/Source.md")
            .expect("read source");
        let mut authority = store
            .open_document_file("manuscript/Source.md")
            .expect("open source");

        assert!(matches!(
            store.rename_document(&mut authority, "Target"),
            Err(StoreError::UnsupportedDocumentLifecyclePlatform)
        ));
        assert!(matches!(
            store.delete_document_file_idempotent(
                CommandId::new(),
                loaded.document_id,
                loaded.revision_id,
                loaded.blob_id,
            ),
            Err(StoreError::UnsupportedDocumentLifecyclePlatform)
        ));
        for table in [
            "document_rename_operations",
            "document_delete_operations",
            "document_deletions",
        ] {
            let count: i64 = store
                .connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("lifecycle row count");
            assert_eq!(count, 0, "{table} must remain empty");
        }
        assert_eq!(
            fs::read_to_string(store.root.join("manuscript/Source.md"))
                .expect("source remains visible"),
            "unchanged source\n"
        );
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn committed_rename_releases_the_old_path_for_a_new_document() {
        let (_directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/A.md",
                DocumentContent::Prose("first identity\n".into()),
                "first document",
            )
            .expect("create A");
        let mut authority = store.open_document_file("manuscript/A.md").expect("open A");
        let first_id = authority.document().document_id;
        store
            .rename_document(&mut authority, "B")
            .expect("rename A to B");

        store
            .create_document_if_absent(
                "manuscript/A.md",
                DocumentContent::Prose("second identity\n".into()),
                "reuse released path",
            )
            .expect("create a new A");
        let second = store.read_document("manuscript/A.md").expect("read new A");

        assert_ne!(second.document_id, first_id);
        assert_eq!(second.text, "second identity\n");
        assert_eq!(
            store.read_document("manuscript/B.md").expect("read B").text,
            "first identity\n"
        );
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn document_display_title_rejects_blank_control_and_overlong_utf8_without_mutation() {
        let (_directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Untitled.md",
                DocumentContent::Prose("ordinary manuscript\n".into()),
                "new document",
            )
            .expect("create document");
        let document_id = store
            .read_document("manuscript/Untitled.md")
            .expect("read document")
            .document_id;

        for invalid in ["   ".to_owned(), "line\nbreak".to_owned(), "é".repeat(129)] {
            let mut authority = store
                .open_document_file("manuscript/Untitled.md")
                .expect("capture exact source");
            assert!(matches!(
                store.rename_document(&mut authority, &invalid),
                Err(StoreError::InvalidDocumentTitle {
                    max_bytes: MAX_DOCUMENT_TITLE_BYTES
                })
            ));
        }
        assert_eq!(
            store
                .registered_document(document_id)
                .expect("read unchanged title")
                .expect("registered document")
                .display_title,
            None
        );

        let exact_utf8_bound = "é".repeat(125);
        let mut authority = store
            .open_document_file("manuscript/Untitled.md")
            .expect("capture exact source at byte bound");
        let renamed = store
            .rename_document(&mut authority, &exact_utf8_bound)
            .expect("accept exact UTF-8 byte bound");
        assert_eq!(
            renamed.display_title.as_deref(),
            Some(exact_utf8_bound.as_str())
        );
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn document_rename_rejects_portable_name_violations_and_collisions_without_mutation() {
        let (_directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Source.md",
                DocumentContent::Prose("source\n".into()),
                "source",
            )
            .expect("create source");
        store
            .create_document_if_absent(
                "manuscript/Taken.md",
                DocumentContent::Prose("taken\n".into()),
                "taken",
            )
            .expect("create collision");
        let source = store
            .read_document("manuscript/Source.md")
            .expect("read source");

        for invalid in ["../escape", "CON", "trailing.", "bad:name"] {
            let mut authority = store
                .open_document_file("manuscript/Source.md")
                .expect("capture source");
            assert!(matches!(
                store.rename_document(&mut authority, invalid),
                Err(StoreError::InvalidDocumentFileName { .. })
            ));
        }
        let mut authority = store
            .open_document_file("manuscript/Source.md")
            .expect("capture source for collision");
        assert!(matches!(
            store.rename_document(&mut authority, "Taken"),
            Err(StoreError::DocumentAlreadyExists(path)) if path == "manuscript/Taken.md"
        ));
        let unchanged = store
            .read_document("manuscript/Source.md")
            .expect("source remains readable");
        assert_eq!(unchanged.document_id, source.document_id);
        assert_eq!(unchanged.revision_id, source.revision_id);
        assert_eq!(unchanged.blob_id, source.blob_id);
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn document_delete_moves_exact_file_to_command_recovery_and_replays() {
        let (_directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Delete Me.md",
                DocumentContent::Prose("recoverable manuscript\n".into()),
                "delete fixture",
            )
            .expect("create document");
        let loaded = store
            .read_document("manuscript/Delete Me.md")
            .expect("read document");
        let command_id = CommandId::new();

        store
            .delete_document_file_idempotent(
                command_id,
                loaded.document_id,
                loaded.revision_id,
                loaded.blob_id,
            )
            .expect("delete document");
        assert!(!store.root.join("manuscript/Delete Me.md").exists());
        assert!(
            store
                .registered_document(loaded.document_id)
                .expect("registered history")
                .is_some()
        );
        let deleted_entries = fs::read_dir(store.root.join(".loom/deleted"))
            .expect("deleted directory")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("deleted entries");
        assert_eq!(deleted_entries.len(), 1);
        assert_eq!(
            fs::read(deleted_entries[0].path()).expect("recovery bytes"),
            b"recoverable manuscript\n"
        );

        store
            .delete_document_file_idempotent(
                command_id,
                loaded.document_id,
                loaded.revision_id,
                loaded.blob_id,
            )
            .expect("replay exact delete");
    }

    #[test]
    #[cfg(unix)]
    fn opening_a_missing_registered_document_reports_external_deletion() {
        let (_directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Missing.md",
                DocumentContent::Prose("gone\n".into()),
                "missing fixture",
            )
            .expect("create document");
        fs::remove_file(store.root.join("manuscript/Missing.md")).expect("remove visible file");

        assert!(matches!(
            store.open_document_file("manuscript/Missing.md"),
            Err(StoreError::ExternalVisibleFileDeleted(path)) if path == "manuscript/Missing.md"
        ));
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn rename_capture_race_restores_the_replacement_without_unlinking_it() {
        let (_directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Source.md",
                DocumentContent::Prose("original\n".into()),
                "race fixture",
            )
            .expect("create source");
        let root = store.root.clone();
        let replacement = root.join("manuscript/replacement.tmp");
        fs::write(&replacement, "replacement\n").expect("stage replacement");
        let mut authority = store
            .open_document_file("manuscript/Source.md")
            .expect("capture source authority");
        let source = authority.document().clone();

        let failure = store
            .rename_document_at_lifecycle_boundary(
                &mut authority,
                source,
                "Renamed".to_owned(),
                "manuscript/Renamed.md".to_owned(),
                || {
                    fs::rename(&replacement, root.join("manuscript/Source.md"))?;
                    Ok(())
                },
                || Ok(()),
                || Ok(()),
            )
            .expect_err("raced replacement must fail closed");

        assert!(matches!(failure, StoreError::VisibleFileIdentityChanged(_)));
        assert_eq!(
            fs::read_to_string(root.join("manuscript/Source.md"))
                .expect("replacement remains reachable"),
            "replacement\n"
        );
        assert!(!root.join("manuscript/Renamed.md").exists());
        assert_eq!(
            store
                .registered_document(authority.document().document_id)
                .expect("read catalogue")
                .expect("registered source")
                .relative_path,
            "manuscript/Source.md"
        );
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn rename_collision_with_an_existing_same_inode_never_consumes_either_name() {
        let (_directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Source.md",
                DocumentContent::Prose("same inode\n".into()),
                "collision fixture",
            )
            .expect("create source");
        let source_path = store.root.join("manuscript/Source.md");
        let alias_path = store.root.join("manuscript/Alias.md");
        fs::hard_link(&source_path, &alias_path).expect("create same-inode collision");
        let mut authority = store
            .open_document_file("manuscript/Source.md")
            .expect("capture source");

        let failure = store
            .rename_document(&mut authority, "Alias")
            .expect_err("existing alias is still a collision");

        assert!(matches!(
            failure,
            StoreError::VisibleFileAlreadyExists(path) if path == "manuscript/Alias.md"
        ));
        assert_eq!(
            fs::read_to_string(source_path).expect("source"),
            "same inode\n"
        );
        assert_eq!(
            fs::read_to_string(alias_path).expect("alias"),
            "same inode\n"
        );
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn case_only_rename_uses_capture_before_no_clobber_install() {
        let (_directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Source.md",
                DocumentContent::Prose("case spelling\n".into()),
                "case fixture",
            )
            .expect("create source");
        let mut authority = store
            .open_document_file("manuscript/Source.md")
            .expect("capture source");

        let renamed = store
            .rename_document(&mut authority, "source")
            .expect("case-only rename");

        assert_eq!(renamed.relative_path, "manuscript/source.md");
        assert_eq!(
            fs::read_to_string(store.root.join("manuscript/source.md"))
                .expect("case-renamed source"),
            "case spelling\n"
        );
        #[cfg(target_vendor = "apple")]
        assert!(
            fs::read_dir(store.root.join("manuscript"))
                .expect("read manuscript directory")
                .any(|entry| entry.expect("directory entry").file_name() == "source.md")
        );
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn prepared_rename_capture_recovers_the_old_path_after_reopen() {
        let (directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Source.md",
                DocumentContent::Prose("recover rename\n".into()),
                "reopen fixture",
            )
            .expect("create source");
        let loaded = store
            .read_document("manuscript/Source.md")
            .expect("read source");
        let operation_id = record_prepared_rename(
            &store,
            &loaded,
            "manuscript/Source.md",
            "manuscript/Target.md",
        );
        let rename_directory = store.root.join(DOCUMENT_RENAME_DIRECTORY);
        ensure_private_directory(&rename_directory).expect("rename directory");
        assert!(
            rename_if_absent(
                &store.root.join("manuscript/Source.md"),
                &rename_directory.join(format!("{operation_id}.capture")),
            )
            .expect("capture source")
        );
        let root = store.root.clone();
        drop(store);

        let reopened = ProjectStore::open(&root).expect("recover prepared rename on reopen");

        assert_eq!(
            fs::read_to_string(root.join("manuscript/Source.md")).expect("restored source"),
            "recover rename\n"
        );
        assert!(!root.join("manuscript/Target.md").exists());
        let state: String = reopened
            .connection
            .query_row(
                "SELECT state FROM document_rename_operations WHERE operation_id = ?1",
                [operation_id.to_string()],
                |row| row.get(0),
            )
            .expect("recovered operation state");
        assert_eq!(state, "aborted");
        assert!(
            !root
                .join(DOCUMENT_RENAME_DIRECTORY)
                .join(format!("{operation_id}.anchor"))
                .exists(),
            "terminal aborted rename must not retain its ownership hard link"
        );
        drop(reopened);
        drop(directory);
    }

    #[test]
    #[cfg(unix)]
    fn prepared_before_capture_never_consumes_an_unrelated_same_blob_target() {
        let (directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Source.md",
                DocumentContent::Prose("same content\n".into()),
                "pre-capture fixture",
            )
            .expect("create source");
        let loaded = store
            .read_document("manuscript/Source.md")
            .expect("read source");
        let operation_id = record_prepared_rename(
            &store,
            &loaded,
            "manuscript/Source.md",
            "manuscript/Target.md",
        );
        fs::remove_file(store.root.join("manuscript/Source.md"))
            .expect("externally remove source before capture");
        fs::write(store.root.join("manuscript/Target.md"), "same content\n")
            .expect("create unrelated same-content target");
        let root = store.root.clone();
        drop(store);

        let reopened = ProjectStore::open(&root).expect("reconcile prepared intent");

        assert!(!root.join("manuscript/Source.md").exists());
        assert_eq!(
            fs::read_to_string(root.join("manuscript/Target.md")).expect("unrelated target"),
            "same content\n"
        );
        let state: String = reopened
            .connection
            .query_row(
                "SELECT state FROM document_rename_operations WHERE operation_id = ?1",
                [operation_id.to_string()],
                |row| row.get(0),
            )
            .expect("operation state");
        assert_eq!(state, "aborted");
        drop(reopened);
        drop(directory);
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn prepared_rename_with_corrupt_private_capture_stays_live() {
        let (_directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Source.md",
                DocumentContent::Prose("expected bytes\n".into()),
                "corrupt capture fixture",
            )
            .expect("create source");
        let loaded = store
            .read_document("manuscript/Source.md")
            .expect("read source");
        let operation_id = record_prepared_rename(
            &store,
            &loaded,
            "manuscript/Source.md",
            "manuscript/Target.md",
        );
        let capture_path = store
            .root
            .join(DOCUMENT_RENAME_DIRECTORY)
            .join(format!("{operation_id}.capture"));
        ensure_private_directory(capture_path.parent().expect("capture parent"))
            .expect("rename directory");
        assert!(
            rename_if_absent(&store.root.join("manuscript/Source.md"), &capture_path)
                .expect("capture source")
        );
        fs::write(&capture_path, "wrong private bytes\n").expect("corrupt private capture");

        let failure = store
            .recover_document_rename_operations()
            .expect_err("corrupt private capture fails closed");
        assert!(matches!(failure, StoreError::DocumentLifecycleUncertain(_)));
        let state: String = store
            .connection
            .query_row(
                "SELECT state FROM document_rename_operations WHERE operation_id = ?1",
                [operation_id.to_string()],
                |row| row.get(0),
            )
            .expect("operation state");
        assert_eq!(state, "prepared");
        assert!(!store.root.join("manuscript/Source.md").exists());
        assert_eq!(
            fs::read_to_string(capture_path).expect("corrupt evidence remains"),
            "wrong private bytes\n"
        );
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn prepared_rename_recovery_preserves_a_recreated_source_and_private_original() {
        let (directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Source.md",
                DocumentContent::Prose("original\n".into()),
                "recreated source fixture",
            )
            .expect("create source");
        let loaded = store
            .read_document("manuscript/Source.md")
            .expect("read source");
        let operation_id = record_prepared_rename(
            &store,
            &loaded,
            "manuscript/Source.md",
            "manuscript/Target.md",
        );
        let capture_path = store
            .root
            .join(DOCUMENT_RENAME_DIRECTORY)
            .join(format!("{operation_id}.capture"));
        ensure_private_directory(capture_path.parent().expect("capture parent"))
            .expect("rename directory");
        assert!(
            rename_if_absent(&store.root.join("manuscript/Source.md"), &capture_path)
                .expect("capture source")
        );
        fs::write(store.root.join("manuscript/Source.md"), "replacement\n")
            .expect("recreate source name");
        let root = store.root.clone();
        drop(store);

        let reopened = ProjectStore::open(&root).expect("recover prepared rename");

        assert_eq!(
            fs::read_to_string(root.join("manuscript/Source.md")).expect("recreated source"),
            "replacement\n"
        );
        assert_eq!(
            fs::read_to_string(&capture_path).expect("preserved original"),
            "original\n"
        );
        let state: String = reopened
            .connection
            .query_row(
                "SELECT state FROM document_rename_operations WHERE operation_id = ?1",
                [operation_id.to_string()],
                |row| row.get(0),
            )
            .expect("recovered operation state");
        assert_eq!(state, "aborted");
        drop(reopened);
        drop(directory);
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn prepared_rename_recovery_does_not_confuse_same_blob_recreation_with_inode_restore() {
        let (directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Source.md",
                DocumentContent::Prose("same bytes\n".into()),
                "same-blob recovery fixture",
            )
            .expect("create source");
        let loaded = store
            .read_document("manuscript/Source.md")
            .expect("read source");
        let operation_id = record_prepared_rename(
            &store,
            &loaded,
            "manuscript/Source.md",
            "manuscript/Target.md",
        );
        let capture_path = store
            .root
            .join(DOCUMENT_RENAME_DIRECTORY)
            .join(format!("{operation_id}.capture"));
        let source_path = store.root.join("manuscript/Source.md");
        let target_path = store.root.join("manuscript/Target.md");
        ensure_private_directory(capture_path.parent().expect("capture parent"))
            .expect("rename directory");
        assert!(rename_if_absent(&source_path, &capture_path).expect("capture original"));
        mark_rename_captured(&store, operation_id);
        assert!(rename_if_absent(&capture_path, &target_path).expect("install target"));
        fs::write(&source_path, "same bytes\n").expect("recreate same-blob source");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            assert_ne!(
                fs::metadata(&source_path).expect("source metadata").ino(),
                fs::metadata(&target_path).expect("target metadata").ino()
            );
        }
        let root = store.root.clone();
        drop(store);

        let reopened = ProjectStore::open(&root).expect("reconcile same-blob conflict");

        assert_eq!(
            fs::read_to_string(&source_path).expect("same-blob source remains"),
            "same bytes\n"
        );
        assert_eq!(
            fs::read_to_string(&target_path).expect("original target remains"),
            "same bytes\n"
        );
        let state: String = reopened
            .connection
            .query_row(
                "SELECT state FROM document_rename_operations WHERE operation_id = ?1",
                [operation_id.to_string()],
                |row| row.get(0),
            )
            .expect("recovered operation state");
        assert_eq!(state, "aborted");
        assert!(
            !root
                .join(DOCUMENT_RENAME_DIRECTORY)
                .join(format!("{operation_id}.anchor"))
                .exists(),
            "terminal conflict recovery must remove the redundant ownership hard link"
        );
        drop(reopened);
        drop(directory);
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn captured_rename_anchor_never_consumes_a_same_content_target_replacement() {
        let (directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Source.md",
                DocumentContent::Prose("anchored bytes\n".into()),
                "target replacement fixture",
            )
            .expect("create source");
        let loaded = store
            .read_document("manuscript/Source.md")
            .expect("read source");
        let operation_id = record_prepared_rename(
            &store,
            &loaded,
            "manuscript/Source.md",
            "manuscript/Target.md",
        );
        let capture_path = store
            .root
            .join(DOCUMENT_RENAME_DIRECTORY)
            .join(format!("{operation_id}.capture"));
        let target_path = store.root.join("manuscript/Target.md");
        let source_path = store.root.join("manuscript/Source.md");
        ensure_private_directory(capture_path.parent().expect("capture parent"))
            .expect("rename directory");
        assert!(rename_if_absent(&source_path, &capture_path).expect("capture source"));
        mark_rename_captured(&store, operation_id);
        assert!(rename_if_absent(&capture_path, &target_path).expect("install target"));
        fs::remove_file(&target_path).expect("externally remove operation target");
        fs::write(&target_path, "anchored bytes\n").expect("replace target with same bytes");
        let root = store.root.clone();
        drop(store);

        let reopened = ProjectStore::open(&root).expect("recover from ownership anchor");

        assert_eq!(
            fs::read_to_string(&source_path).expect("anchored original restored"),
            "anchored bytes\n"
        );
        assert_eq!(
            fs::read_to_string(&target_path).expect("replacement target preserved"),
            "anchored bytes\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            assert_ne!(
                fs::metadata(&source_path).expect("source metadata").ino(),
                fs::metadata(&target_path).expect("target metadata").ino()
            );
        }
        let state: String = reopened
            .connection
            .query_row(
                "SELECT state FROM document_rename_operations WHERE operation_id = ?1",
                [operation_id.to_string()],
                |row| row.get(0),
            )
            .expect("operation state");
        assert_eq!(state, "aborted");
        drop(reopened);
        drop(directory);
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn same_session_action_resolution_recovers_a_prepared_target_before_retry() {
        let (_directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Source.md",
                DocumentContent::Prose("retry rename\n".into()),
                "same-session fixture",
            )
            .expect("create source");
        let loaded = store
            .read_document("manuscript/Source.md")
            .expect("read source");
        let operation_id = record_prepared_rename(
            &store,
            &loaded,
            "manuscript/Source.md",
            "manuscript/Target.md",
        );
        let capture_path = store
            .root
            .join(DOCUMENT_RENAME_DIRECTORY)
            .join(format!("{operation_id}.capture"));
        let target_path = store.root.join("manuscript/Target.md");
        ensure_private_directory(capture_path.parent().expect("capture parent"))
            .expect("rename directory");
        assert!(
            rename_if_absent(&store.root.join("manuscript/Source.md"), &capture_path)
                .expect("capture source")
        );
        mark_rename_captured(&store, operation_id);
        assert!(rename_if_absent(&capture_path, &target_path).expect("install target"));

        let mut retry_authority = store
            .open_document_file("manuscript/Source.md")
            .expect("path resolution reconciles prepared rename");
        let renamed = store
            .rename_document(&mut retry_authority, "Target")
            .expect("same-session retry completes");

        assert_eq!(renamed.relative_path, "manuscript/Target.md");
        assert!(!store.root.join("manuscript/Source.md").exists());
        assert_eq!(
            fs::read_to_string(target_path).expect("renamed file"),
            "retry rename\n"
        );
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn post_target_precommit_failure_is_retryable_in_the_same_session() {
        let (_directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Source.md",
                DocumentContent::Prose("retry after failure\n".into()),
                "precommit failure fixture",
            )
            .expect("create source");
        let mut authority = store
            .open_document_file("manuscript/Source.md")
            .expect("open source");
        let source = authority.document().clone();

        let failure = store
            .rename_document_at_lifecycle_boundary(
                &mut authority,
                source,
                "Target".to_owned(),
                "manuscript/Target.md".to_owned(),
                || Ok(()),
                || Ok(()),
                || Err(std::io::Error::other("injected precommit failure").into()),
            )
            .expect_err("precommit failure remains uncertain");
        assert!(matches!(failure, StoreError::DocumentLifecycleUncertain(_)));
        assert!(
            fs::read_dir(store.root.join(DOCUMENT_RENAME_DIRECTORY))
                .expect("rename private directory")
                .all(|entry| !entry
                    .expect("rename private entry")
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".anchor")),
            "same-session terminal rollback must remove its redundant ownership hard link"
        );

        let mut retry_authority = store
            .open_document_file("manuscript/Source.md")
            .expect("source remains resolvable after uncertainty");
        let renamed = store
            .rename_document(&mut retry_authority, "Target")
            .expect("same-session retry completes");
        assert_eq!(renamed.relative_path, "manuscript/Target.md");
        assert_eq!(
            fs::read_to_string(store.root.join("manuscript/Target.md")).expect("renamed content"),
            "retry after failure\n"
        );
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn rename_to_a_tombstoned_historical_path_fails_before_capture() {
        let (_directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Archived.md",
                DocumentContent::Prose("archived\n".into()),
                "historical path fixture",
            )
            .expect("create archived document");
        let archived = store
            .read_document("manuscript/Archived.md")
            .expect("read archived document");
        store
            .delete_document_file_idempotent(
                CommandId::new(),
                archived.document_id,
                archived.revision_id,
                archived.blob_id,
            )
            .expect("delete archived document");
        store
            .create_document_if_absent(
                "manuscript/Source.md",
                DocumentContent::Prose("source\n".into()),
                "rename source fixture",
            )
            .expect("create source");
        let mut authority = store
            .open_document_file("manuscript/Source.md")
            .expect("open source");

        let failure = store
            .rename_document(&mut authority, "archived")
            .expect_err("case-folded historical path remains reserved");

        assert!(matches!(
            failure,
            StoreError::DocumentAlreadyExists(path) if path == "manuscript/archived.md"
        ));
        assert_eq!(
            fs::read_to_string(store.root.join("manuscript/Source.md")).expect("source intact"),
            "source\n"
        );
        let rename_count: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM document_rename_operations",
                [],
                |row| row.get(0),
            )
            .expect("rename operation count");
        assert_eq!(rename_count, 0);
    }

    #[test]
    #[cfg(unix)]
    fn active_and_unicode_normalized_path_aliases_are_reserved_portably() {
        let (_directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Foo.md",
                DocumentContent::Prose("case owner\n".into()),
                "case reservation fixture",
            )
            .expect("create case owner");
        fs::remove_file(store.root.join("manuscript/Foo.md"))
            .expect("externally remove visible case owner");
        assert!(matches!(
            store.create_document_if_absent(
                "manuscript/foo.md",
                DocumentContent::Prose("alias\n".into()),
                "case alias attempt",
            ),
            Err(StoreError::DocumentAlreadyExists(path)) if path == "manuscript/foo.md"
        ));

        store
            .create_document_if_absent(
                "manuscript/Café.md",
                DocumentContent::Prose("normalization owner\n".into()),
                "normalization fixture",
            )
            .expect("create NFC owner");
        fs::remove_file(store.root.join("manuscript/Café.md"))
            .expect("externally remove NFC owner");
        let nfd_path = "manuscript/Cafe\u{301}.md";
        assert!(matches!(
            store.create_document_if_absent(
                nfd_path,
                DocumentContent::Prose("normalization alias\n".into()),
                "normalization alias attempt",
            ),
            Err(StoreError::DocumentAlreadyExists(path)) if path == nfd_path
        ));
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn save_checkpoint_and_import_cannot_bypass_portable_path_reservations() {
        let (directory, mut store) = new_store();
        store
            .save_document(
                "manuscript/Source.md",
                DocumentContent::Prose("owner\n".into()),
                "create owner",
            )
            .expect("create active owner");
        let before_alias = store.counts().expect("counts before alias attempts");
        assert!(matches!(
            store.save_document(
                "manuscript/source.md",
                DocumentContent::Prose("alias\n".into()),
                "save alias attempt",
            ),
            Err(StoreError::DocumentAlreadyExists(path)) if path == "manuscript/source.md"
        ));

        let import_source = directory.path().join("import.md");
        fs::write(&import_source, "import alias\n").expect("write import source");
        assert!(matches!(
            store.import_file(
                &import_source,
                "manuscript/source.md",
                DocumentKind::Prose,
                "import alias attempt",
            ),
            Err(StoreError::DocumentAlreadyExists(path)) if path == "manuscript/source.md"
        ));

        #[cfg(not(target_vendor = "apple"))]
        {
            fs::write(
                store.root.join("manuscript/source.md"),
                "checkpoint alias\n",
            )
            .expect("create distinct case spelling for modeled case-insensitive collision");
            assert!(matches!(
                store.checkpoint_visible(
                    "manuscript/source.md",
                    DocumentKind::Prose,
                    "checkpoint alias attempt",
                ),
                Err(StoreError::DocumentAlreadyExists(path)) if path == "manuscript/source.md"
            ));
            fs::remove_file(store.root.join("manuscript/source.md"))
                .expect("remove modeled alias fixture");
        }
        assert_eq!(
            store.counts().expect("counts after alias attempts"),
            before_alias
        );

        store
            .create_document_if_absent(
                "manuscript/Archived.md",
                DocumentContent::Prose("archived\n".into()),
                "tombstone fixture",
            )
            .expect("create archived document");
        let archived = store
            .read_document("manuscript/Archived.md")
            .expect("read archived document");
        store
            .delete_document_file_idempotent(
                CommandId::new(),
                archived.document_id,
                archived.revision_id,
                archived.blob_id,
            )
            .expect("tombstone archived document");
        assert!(matches!(
            store.save_document(
                "manuscript/archived.md",
                DocumentContent::Prose("resurrection\n".into()),
                "tombstone alias attempt",
            ),
            Err(StoreError::DocumentAlreadyExists(path)) if path == "manuscript/archived.md"
        ));

        store
            .create_document_if_absent(
                "manuscript/Café.md",
                DocumentContent::Prose("normalized owner\n".into()),
                "normalization owner",
            )
            .expect("create NFC owner");
        let nfd_path = "manuscript/Cafe\u{301}.md";
        assert!(matches!(
            store.import_file(
                &import_source,
                nfd_path,
                DocumentKind::Prose,
                "normalization alias import",
            ),
            Err(StoreError::DocumentAlreadyExists(path)) if path == nfd_path
        ));
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn delete_capture_race_restores_replacement_and_does_not_tombstone() {
        let (_directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Delete.md",
                DocumentContent::Prose("original\n".into()),
                "delete race fixture",
            )
            .expect("create source");
        let loaded = store
            .read_document("manuscript/Delete.md")
            .expect("read source");
        let root = store.root.clone();
        let replacement = root.join("manuscript/replacement.tmp");
        fs::write(&replacement, "replacement\n").expect("stage replacement");

        let failure = store
            .delete_document_file_at_lifecycle_boundary(
                CommandId::new(),
                loaded.document_id,
                loaded.revision_id,
                loaded.blob_id,
                || {
                    fs::rename(&replacement, root.join("manuscript/Delete.md"))?;
                    Ok(())
                },
                || Ok(()),
            )
            .expect_err("raced replacement must fail closed");

        assert!(matches!(failure, StoreError::VisibleFileIdentityChanged(_)));
        assert_eq!(
            fs::read_to_string(root.join("manuscript/Delete.md"))
                .expect("replacement remains visible"),
            "replacement\n"
        );
        assert_eq!(store.list_documents().expect("active catalogue").len(), 1);
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn aborted_pre_capture_delete_reuses_the_exact_command_safely() {
        let (_directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Delete.md",
                DocumentContent::Prose("retry deletion\n".into()),
                "delete retry fixture",
            )
            .expect("create source");
        let loaded = store
            .read_document("manuscript/Delete.md")
            .expect("read source");
        let command_id = CommandId::new();

        store
            .delete_document_file_at_lifecycle_boundary(
                command_id,
                loaded.document_id,
                loaded.revision_id,
                loaded.blob_id,
                || Err(std::io::Error::other("stop before capture").into()),
                || Ok(()),
            )
            .expect_err("pre-capture interruption aborts the attempt");
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT state FROM document_delete_operations WHERE command_id = ?1",
                    [command_id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .expect("aborted state"),
            "aborted"
        );

        store
            .delete_document_file_idempotent(
                command_id,
                loaded.document_id,
                loaded.revision_id,
                loaded.blob_id,
            )
            .expect("same exact command rearms and commits");
        assert!(store.list_documents().expect("active documents").is_empty());
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn aborted_delete_command_does_not_follow_a_later_rename() {
        let (_directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/A.md",
                DocumentContent::Prose("stable identity\n".into()),
                "path fingerprint fixture",
            )
            .expect("create source");
        let loaded = store.read_document("manuscript/A.md").expect("read source");
        let command_id = CommandId::new();
        store
            .delete_document_file_at_lifecycle_boundary(
                command_id,
                loaded.document_id,
                loaded.revision_id,
                loaded.blob_id,
                || Err(std::io::Error::other("abort before capture").into()),
                || Ok(()),
            )
            .expect_err("abort delete attempt");
        let mut authority = store
            .open_document_file("manuscript/A.md")
            .expect("open source for rename");
        store
            .rename_document(&mut authority, "B")
            .expect("rename after aborted delete");

        let failure = store
            .delete_document_file_idempotent(
                command_id,
                loaded.document_id,
                loaded.revision_id,
                loaded.blob_id,
            )
            .expect_err("stale command path is part of its fingerprint");
        assert!(matches!(failure, StoreError::IdempotencyConflict { .. }));
        assert_eq!(
            fs::read_to_string(store.root.join("manuscript/B.md")).expect("renamed source"),
            "stable identity\n"
        );
    }

    #[test]
    #[cfg(unix)]
    fn delete_refuses_a_recoverable_transient_draft_before_intent() {
        let (_directory, mut store) = new_store();
        let saved = store
            .save_document(
                "manuscript/001.md",
                DocumentContent::Prose("checkpoint\n".into()),
                "draft deletion fixture",
            )
            .expect("create document");
        let document_id = store
            .read_document("manuscript/001.md")
            .expect("read document identity")
            .document_id;
        store
            .upsert_transient_draft(
                "manuscript/001.md",
                saved.revision_id,
                0,
                DocumentContent::Prose("newer recoverable draft\n".into()),
            )
            .expect("write draft");

        let failure = store
            .delete_document_file_idempotent(
                CommandId::new(),
                document_id,
                saved.revision_id,
                saved.blob_id,
            )
            .expect_err("draft blocks deletion");
        assert!(matches!(
            failure,
            StoreError::DocumentHasTransientDraft(id) if id == document_id
        ));
        let operations: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM document_delete_operations",
                [],
                |row| row.get(0),
            )
            .expect("operation count");
        assert_eq!(operations, 0);
        assert_eq!(
            fs::read_to_string(store.root.join("manuscript/001.md")).expect("visible source"),
            "checkpoint\n"
        );
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn pending_outbox_blocks_namespace_changes_until_recovery() {
        let (_directory, mut store) = new_store();
        let saved = store
            .save_document(
                "manuscript/001.md",
                DocumentContent::Prose("projected\n".into()),
                "pending lifecycle fixture",
            )
            .expect("create document");
        let document_id = store
            .read_document("manuscript/001.md")
            .expect("read document identity")
            .document_id;
        store
            .connection
            .execute(
                "UPDATE visible_file_outbox
                 SET state = 'pending', completed_at_ms = NULL
                 WHERE revision_id = ?1",
                [saved.revision_id.to_string()],
            )
            .expect("simulate pending acknowledgement");
        let mut authority = store
            .open_document_file("manuscript/001.md")
            .expect("open source");

        assert!(matches!(
            store.rename_document(&mut authority, "Renamed"),
            Err(StoreError::DocumentHasPendingOutbox(id)) if id == document_id
        ));
        assert!(matches!(
            store.delete_document_file_idempotent(
                CommandId::new(),
                document_id,
                saved.revision_id,
                saved.blob_id,
            ),
            Err(StoreError::DocumentHasPendingOutbox(id)) if id == document_id
        ));
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM document_rename_operations",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("rename operation count"),
            0
        );
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM document_delete_operations",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("delete operation count"),
            0
        );

        store.recover().expect("settle pending projection");
        let mut authority = store
            .open_document_file("manuscript/001.md")
            .expect("reopen source");
        store
            .rename_document(&mut authority, "Renamed")
            .expect("rename after recovery");
        store.recover().expect("later recovery is inert");
        assert!(!store.root.join("manuscript/001.md").exists());
        assert_eq!(
            fs::read_to_string(store.root.join("manuscript/Renamed.md"))
                .expect("renamed manuscript"),
            "projected\n"
        );
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn tombstoned_outbox_is_terminally_suppressed_without_visible_resurrection() {
        let (_directory, mut store) = new_store();
        let saved = store
            .save_document(
                "manuscript/001.md",
                DocumentContent::Prose("deleted projection\n".into()),
                "tombstone outbox fixture",
            )
            .expect("create document");
        let document_id = store
            .read_document("manuscript/001.md")
            .expect("read document identity")
            .document_id;
        store
            .delete_document_file_idempotent(
                CommandId::new(),
                document_id,
                saved.revision_id,
                saved.blob_id,
            )
            .expect("delete document");
        store
            .connection
            .execute(
                "UPDATE visible_file_outbox
                 SET state = 'pending', completed_at_ms = NULL
                 WHERE revision_id = ?1",
                [saved.revision_id.to_string()],
            )
            .expect("inject stale pre-delete pending projection");

        let report = store.recover().expect("suppress tombstoned outbox");
        assert_eq!(report.already_applied, 1);
        assert_eq!(store.pending_outbox_count().expect("pending count"), 0);
        assert!(!store.root.join("manuscript/001.md").exists());
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn corrupt_delete_recovery_evidence_stays_live() {
        let (_directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Delete.md",
                DocumentContent::Prose("expected delete bytes\n".into()),
                "corrupt delete recovery fixture",
            )
            .expect("create document");
        let loaded = store
            .read_document("manuscript/Delete.md")
            .expect("read document");
        let command_id = CommandId::new();
        store
            .delete_document_file_at_lifecycle_boundary(
                command_id,
                loaded.document_id,
                loaded.revision_id,
                loaded.blob_id,
                || Ok(()),
                || Err(std::io::Error::other("interrupt after capture").into()),
            )
            .expect_err("leave prepared exact recovery");
        let recovery_name: String = store
            .connection
            .query_row(
                "SELECT recovery_file_name FROM document_delete_operations WHERE command_id = ?1",
                [command_id.to_string()],
                |row| row.get(0),
            )
            .expect("recovery name");
        fs::write(
            store
                .root
                .join(DOCUMENT_DELETED_DIRECTORY)
                .join(recovery_name),
            "corrupt bytes\n",
        )
        .expect("corrupt recovery evidence");

        assert!(matches!(
            store.recover_document_delete_operations(),
            Err(StoreError::DocumentLifecycleUncertain(_))
        ));
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT state FROM document_delete_operations WHERE command_id = ?1",
                    [command_id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .expect("operation state"),
            "prepared"
        );
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn same_session_save_reconciles_a_captured_delete_without_resurrection() {
        let (_directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Delete.md",
                DocumentContent::Prose("delete before save\n".into()),
                "same-session save fixture",
            )
            .expect("create document");
        let loaded = store
            .read_document("manuscript/Delete.md")
            .expect("read document");
        store
            .delete_document_file_at_lifecycle_boundary(
                CommandId::new(),
                loaded.document_id,
                loaded.revision_id,
                loaded.blob_id,
                || Ok(()),
                || Err(std::io::Error::other("interrupt after capture").into()),
            )
            .expect_err("leave durable captured delete");
        let before_revisions = store.counts().expect("counts before stale save").revisions;

        let failure = store
            .save_document(
                "manuscript/Delete.md",
                DocumentContent::Prose("must not resurrect\n".into()),
                "stale save",
            )
            .expect_err("save reconciles deletion before path authority");
        assert!(matches!(failure, StoreError::DocumentAlreadyExists(_)));
        assert!(store.list_documents().expect("active documents").is_empty());
        assert_eq!(
            store.counts().expect("counts after stale save").revisions,
            before_revisions
        );
        assert!(!store.root.join("manuscript/Delete.md").exists());
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM visible_file_outbox WHERE state = 'pending'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("pending outbox count"),
            0
        );
    }

    #[test]
    #[cfg(unix)]
    fn delete_commit_error_with_failed_readback_remains_retryable_uncertainty() {
        let (_directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Delete.md",
                DocumentContent::Prose("commit uncertainty\n".into()),
                "commit readback fixture",
            )
            .expect("create document");
        let loaded = store
            .read_document("manuscript/Delete.md")
            .expect("read document");
        let command_id = CommandId::new();
        let operation = DocumentDeleteOperationRecord {
            command_id,
            document_id: loaded.document_id,
            revision_id: loaded.revision_id,
            blob_id: loaded.blob_id,
            relative_path: loaded.relative_path,
            recovery_file_name: format!(
                "{command_id}.{}.{}.{}.document",
                loaded.document_id, loaded.revision_id, loaded.blob_id
            ),
            state: DeleteOperationState::Captured,
        };
        let commit_error = std::io::Error::other("synthetic commit error");

        let failure = store
            .reconcile_document_delete_commit_error_at_boundary(
                command_id,
                &operation,
                &commit_error,
                |_| Err(StoreError::Sqlite(rusqlite::Error::InvalidQuery)),
                |_| Ok(None),
            )
            .expect_err("failed readback cannot downgrade commit uncertainty");
        assert!(matches!(
            failure,
            StoreError::DocumentLifecycleUncertain(message)
                if message.contains("operation readback failed")
        ));
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn draft_admission_reconciles_a_live_captured_delete_before_mutation() {
        let (_directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Delete.md",
                DocumentContent::Prose("active bytes\n".into()),
                "live delete mutation fixture",
            )
            .expect("create document");
        let loaded = store
            .read_document("manuscript/Delete.md")
            .expect("read document");
        let command_id = CommandId::new();
        let recovery_name = format!(
            "{command_id}.{}.{}.{}.document",
            loaded.document_id, loaded.revision_id, loaded.blob_id
        );
        let recovery_path = store
            .root
            .join(DOCUMENT_DELETED_DIRECTORY)
            .join(&recovery_name);
        ensure_private_directory(recovery_path.parent().expect("recovery parent"))
            .expect("deleted directory");
        store
            .connection
            .execute(
                "INSERT INTO document_delete_operations(
                     command_id, document_id, revision_id, blob_id, relative_path,
                     recovery_file_name, state, created_at_ms,
                     captured_at_ms, committed_at_ms, finished_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'prepared', ?7, NULL, NULL, NULL)",
                params![
                    command_id.to_string(),
                    loaded.document_id.to_string(),
                    loaded.revision_id.to_string(),
                    loaded.blob_id.to_string(),
                    &loaded.relative_path,
                    recovery_name,
                    now_unix_ms(),
                ],
            )
            .expect("prepare delete operation");
        assert!(
            rename_if_absent(&store.root.join("manuscript/Delete.md"), &recovery_path,)
                .expect("capture visible file")
        );
        store
            .connection
            .execute(
                "UPDATE document_delete_operations
                 SET state = 'captured', captured_at_ms = ?2
                 WHERE command_id = ?1",
                params![command_id.to_string(), now_unix_ms()],
            )
            .expect("mark captured delete");

        assert!(matches!(
            store.upsert_transient_draft(
                "manuscript/Delete.md",
                loaded.revision_id,
                0,
                DocumentContent::Prose("stale mutation\n".into()),
            ),
            Err(StoreError::NoActiveRevision(path)) if path == "manuscript/Delete.md"
        ));
        assert_eq!(
            store
                .connection
                .query_row("SELECT COUNT(*) FROM transient_drafts", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("draft count"),
            0
        );
        assert_eq!(
            fs::read_to_string(recovery_path).expect("captured bytes remain"),
            "active bytes\n"
        );
        assert!(store.list_documents().expect("active documents").is_empty());
        let root = store.root.clone();
        drop(store);
        let reopened = ProjectStore::open(&root).expect("reopen after native delete recovery");
        assert!(
            reopened
                .list_documents()
                .expect("reopened catalogue")
                .is_empty()
        );
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    #[cfg(unix)]
    fn delete_uncertainty_replays_after_reopen_and_tombstone_survives_recovery_loss() {
        let (directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Delete.md",
                DocumentContent::Prose("recoverable\n".into()),
                "delete uncertainty fixture",
            )
            .expect("create source");
        let loaded = store
            .read_document("manuscript/Delete.md")
            .expect("read source");
        let command_id = CommandId::new();

        let failure = store
            .delete_document_file_at_lifecycle_boundary(
                command_id,
                loaded.document_id,
                loaded.revision_id,
                loaded.blob_id,
                || Ok(()),
                || Err(std::io::Error::other("injected interruption").into()),
            )
            .expect_err("interrupted delete remains retryable");
        assert!(matches!(failure, StoreError::DocumentLifecycleUncertain(_)));
        let root = store.root.clone();
        fs::write(
            root.join("manuscript/Delete.md"),
            "recreated before reopen\n",
        )
        .expect("recreate old visible name before native recovery");
        drop(store);

        let reopened = ProjectStore::open(&root).expect("reopen project");
        // Reopen completes from durable native intent without a renderer
        // replay or retained volatile command identity.
        assert!(
            reopened
                .list_documents()
                .expect("active catalogue")
                .is_empty()
        );
        let operation_state: String = reopened
            .connection
            .query_row(
                "SELECT state FROM document_delete_operations WHERE command_id = ?1",
                [command_id.to_string()],
                |row| row.get(0),
            )
            .expect("delete operation state");
        assert_eq!(operation_state, "committed");
        assert_eq!(
            fs::read_to_string(root.join("manuscript/Delete.md"))
                .expect("recreated path remains unmanaged"),
            "recreated before reopen\n"
        );
        let recovery = fs::read_dir(root.join(DOCUMENT_DELETED_DIRECTORY))
            .expect("deleted directory")
            .next()
            .expect("recovery entry")
            .expect("recovery path")
            .path();
        fs::remove_file(&recovery).expect("simulate lost recovery copy");
        drop(reopened);

        let mut final_store = ProjectStore::open(&root).expect("reopen tombstoned project");
        final_store
            .delete_document_file_idempotent(
                command_id,
                loaded.document_id,
                loaded.revision_id,
                loaded.blob_id,
            )
            .expect("receipt-only exact replay");
        assert!(
            final_store
                .list_documents()
                .expect("tombstoned catalogue")
                .is_empty()
        );
        assert!(
            final_store
                .registered_document(loaded.document_id)
                .expect("historical catalogue lookup")
                .is_some()
        );
        assert!(final_store.read_document("manuscript/Delete.md").is_err());
        assert_eq!(
            fs::read_to_string(root.join("manuscript/Delete.md"))
                .expect("recreated path is never consumed"),
            "recreated before reopen\n"
        );
        drop(final_store);
        drop(directory);
    }

    #[test]
    #[cfg(unix)]
    fn document_summary_reads_reject_noncanonical_stored_titles() {
        let (_directory, mut store) = new_store();
        store
            .create_document_if_absent(
                "manuscript/Untitled.md",
                DocumentContent::Prose("ordinary manuscript\n".into()),
                "new document",
            )
            .expect("create document");
        let document_id = store
            .read_document("manuscript/Untitled.md")
            .expect("read document")
            .document_id;

        store
            .connection
            .pragma_update(None, "ignore_check_constraints", "ON")
            .expect("enable corruption fixture");
        store
            .connection
            .execute(
                "UPDATE documents SET display_title = 'corrupt' || char(10) || 'title'
                 WHERE document_id = ?1",
                [document_id.to_string()],
            )
            .expect("inject corrupt display title");
        store
            .connection
            .pragma_update(None, "ignore_check_constraints", "OFF")
            .expect("restore check constraints");

        assert!(matches!(
            store.registered_document(document_id),
            Err(StoreError::CorruptDatabase(message))
                if message == "document display_title is not canonical"
        ));
        assert!(matches!(
            store.list_documents(),
            Err(StoreError::CorruptDatabase(message))
                if message == "document display_title is not canonical"
        ));
    }

    #[test]
    #[cfg(unix)]
    fn project_lease_rejects_concurrent_open_and_releases_on_drop() {
        let directory = tempdir().expect("temporary project root");
        let project = directory.path().join("Novel");
        let (store, _) = ProjectStore::initialize(&project, "Novel").expect("initialize project");
        let canonical_project = project.canonicalize().expect("canonical project path");

        assert!(matches!(
            ProjectStore::open(&project),
            Err(StoreError::ProjectAlreadyOpen(path)) if path == canonical_project
        ));

        drop(store);
        let reopened = ProjectStore::open(&project).expect("lease released when store dropped");
        assert_eq!(reopened.root(), canonical_project);
    }

    #[cfg(unix)]
    #[test]
    fn project_lease_refuses_a_symbolic_link() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().expect("temporary project root");
        let project = directory.path().join("Novel");
        let (store, _) = ProjectStore::initialize(&project, "Novel").expect("initialize project");
        drop(store);

        let lease_path = project
            .canonicalize()
            .expect("canonical project path")
            .join(".loom")
            .join(PROJECT_LEASE_FILE);
        fs::remove_file(&lease_path).expect("remove lease file");
        let outside = directory.path().join("outside-lock");
        fs::write(&outside, b"").expect("create outside lock target");
        symlink(&outside, &lease_path).expect("replace lease with symlink");

        assert!(matches!(
            ProjectStore::open(&project),
            Err(StoreError::SymbolicLink(path)) if path == lease_path
        ));
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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
            "generation_terminal_evidence",
            "generation_command_events",
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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
    #[cfg(unix)]
    fn adopt_visible_document_preserves_exact_prose_bytes_and_human_import_provenance() {
        let (_directory, mut store) = new_store();
        let relative_path = "manuscript/Untitled.md";
        let visible_path = store.root.join(relative_path);
        let original = "  opening\r\n\r\ncafé\t \r\n".as_bytes();
        fs::write(&visible_path, original).expect("write existing visible manuscript");

        let adopted = store
            .adopt_visible_document_if_absent(
                relative_path,
                DocumentKind::Prose,
                "recover visible manuscript",
            )
            .expect("adopt exact visible manuscript");

        assert_eq!(
            fs::read(&visible_path).expect("read visible bytes"),
            original
        );
        assert_eq!(adopted.blob_id, BlobId::digest(original));
        assert_eq!(adopted.receipt.command, CommandKind::Import);
        assert_eq!(adopted.receipt.source_revision_id, None);
        assert_eq!(
            store
                .load_receipt(adopted.receipt.command_id)
                .expect("load import receipt"),
            Some(adopted.receipt.clone())
        );
        assert_eq!(
            store
                .read_document(relative_path)
                .expect("read adopted document")
                .text
                .as_bytes(),
            original
        );
        assert_eq!(
            store
                .reconstruct_revision(adopted.revision_id)
                .expect("reconstruct adopted revision"),
            original
        );

        let provenance = store
            .revision_provenance(adopted.revision_id)
            .expect("load import provenance");
        assert_eq!(provenance.segments.len(), 1);
        assert_eq!(provenance.segments[0].artifact_id, adopted.artifact_id);
        assert_eq!(provenance.segments[0].byte_range.start, 0);
        assert_eq!(
            provenance.segments[0].byte_range.end,
            u64::try_from(original.len()).expect("fixture length")
        );
        assert_eq!(
            provenance.segments[0].contribution,
            loom_types::ContributionKind::Human
        );

        let (artifact_kind, artifact_blob_id, metadata_json): (String, String, String) = store
            .connection
            .query_row(
                "SELECT artifact_kind, blob_id, metadata_json FROM artifacts WHERE artifact_id = ?1",
                [adopted.artifact_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("load adopted artifact");
        assert_eq!(artifact_kind, "human_contribution");
        assert_eq!(artifact_blob_id, adopted.blob_id.to_string());
        let metadata: serde_json::Value =
            serde_json::from_str(&metadata_json).expect("parse artifact metadata");
        assert_eq!(metadata["workflow"], "adopt_visible_document");
        assert_eq!(metadata["source"], "existing_visible_file");
        assert_eq!(metadata["source_blob_id"], adopted.blob_id.to_string());

        let operation_kind: String = store
            .connection
            .query_row(
                "SELECT operation_kind FROM operations WHERE operation_id = ?1",
                [adopted.operation_id.to_string()],
                |row| row.get(0),
            )
            .expect("load import operation");
        assert_eq!(operation_kind, "import");
        let (target, expected, state): (String, Option<String>, String) = store
            .connection
            .query_row(
                "SELECT target_blob_id, expected_visible_blob_id, state
                 FROM visible_file_outbox WHERE revision_id = ?1",
                [adopted.revision_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("load adoption outbox");
        assert_eq!(target, adopted.blob_id.to_string());
        assert_eq!(expected, Some(target));
        assert_eq!(state, "completed");
        assert_eq!(store.pending_outbox_count().expect("pending count"), 0);
    }

    #[test]
    #[cfg(unix)]
    fn adopt_visible_document_boundary_conflict_never_clobbers_newer_bytes() {
        let (_directory, mut store) = new_store();
        let relative_path = "manuscript/Untitled.md";
        let visible_path = store.root.join(relative_path);
        fs::write(&visible_path, b"original\r\n").expect("write original manuscript");

        let result = store.adopt_visible_document_if_absent_with_boundary(
            relative_path,
            DocumentKind::Prose,
            "recover visible manuscript",
            |visible| {
                fs::write(visible, b"newer external bytes\r\n")?;
                Ok(())
            },
        );

        assert!(matches!(
            result,
            Err(StoreError::VisibleFileConflict { ref path, .. }) if path == relative_path
        ));
        assert_eq!(
            fs::read(&visible_path).expect("read surviving visible bytes"),
            b"newer external bytes\r\n"
        );
        assert_eq!(store.pending_outbox_count().expect("pending count"), 1);
        let imported_receipts: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM command_receipts WHERE command_kind = 'import'",
                [],
                |row| row.get(0),
            )
            .expect("count atomic import receipt");
        assert_eq!(imported_receipts, 1);
        let document = store
            .list_documents()
            .expect("list imported document")
            .into_iter()
            .find(|document| document.relative_path == relative_path)
            .expect("registered imported document");
        assert_eq!(
            store
                .reconstruct_revision(document.active_revision_id.expect("active revision"))
                .expect("reconstruct imported bytes"),
            b"original\r\n"
        );

        let recovery = store.recover().expect("retry conflicted outbox");
        assert_eq!(recovery.conflicts.len(), 1);
        assert_eq!(store.pending_outbox_count().expect("pending count"), 1);
        assert_eq!(
            fs::read(&visible_path).expect("read visible after recovery"),
            b"newer external bytes\r\n"
        );
    }

    #[test]
    #[cfg(unix)]
    fn adopt_visible_document_interruption_recovers_committed_receipt_and_outbox() {
        let (directory, mut store) = new_store();
        let root = store.root.clone();
        let relative_path = "manuscript/Untitled.md";
        let original = b"durable\r\nbytes\r\n";
        fs::write(root.join(relative_path), original).expect("write visible manuscript");

        let result = store.adopt_visible_document_if_absent_with_boundary(
            relative_path,
            DocumentKind::Prose,
            "recover visible manuscript",
            |_| {
                Err(StoreError::Io(std::io::Error::other(
                    "simulated interruption before outbox settlement",
                )))
            },
        );
        assert!(matches!(result, Err(StoreError::Io(_))));
        assert_eq!(store.pending_outbox_count().expect("pending count"), 1);
        let imported_receipts: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM command_receipts WHERE command_kind = 'import'",
                [],
                |row| row.get(0),
            )
            .expect("count committed import receipt");
        assert_eq!(imported_receipts, 1);
        drop(store);

        let mut reopened = ProjectStore::open(&root).expect("reopen interrupted project");
        let recovery = reopened.recover().expect("settle interrupted adoption");
        assert_eq!(recovery.already_applied, 1);
        assert_eq!(recovery.applied, 0);
        assert!(recovery.conflicts.is_empty());
        assert_eq!(
            reopened
                .read_document(relative_path)
                .expect("read recovered document")
                .text
                .as_bytes(),
            original
        );
        assert_eq!(reopened.pending_outbox_count().expect("pending count"), 0);
        drop(directory);
    }

    #[test]
    #[cfg(unix)]
    fn adopt_visible_document_rejects_non_file_and_invalid_utf8_without_state() {
        let (_directory, mut store) = new_store();
        let counts_before = store.counts().expect("counts before invalid adoption");
        let directory_path = store.root.join("manuscript/not-a-file.md");
        fs::create_dir(&directory_path).expect("create directory at document path");
        let directory_error = store.adopt_visible_document_if_absent(
            "manuscript/not-a-file.md",
            DocumentKind::Prose,
            "must fail",
        );
        assert!(
            matches!(directory_error, Err(StoreError::NotRegularFile(path)) if path == directory_path)
        );

        let invalid_path = store.root.join("manuscript/invalid.md");
        let invalid = [0xff, 0xfe, b'x'];
        fs::write(&invalid_path, invalid).expect("write invalid UTF-8");
        let invalid_error = store.adopt_visible_document_if_absent(
            "manuscript/invalid.md",
            DocumentKind::Prose,
            "must fail",
        );
        assert!(matches!(
            invalid_error,
            Err(StoreError::ExternalVisibleInvalidUtf8(path)) if path == "manuscript/invalid.md"
        ));
        assert_eq!(
            fs::read(invalid_path).expect("invalid file survives"),
            invalid
        );
        assert_eq!(
            store.counts().expect("counts after invalid adoption"),
            counts_before
        );
        assert!(store.list_documents().expect("list documents").is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn adopt_visible_document_rejects_symlink_without_reading_or_mutating_target() {
        use std::os::unix::fs::symlink;

        let (directory, mut store) = new_store();
        let counts_before = store.counts().expect("counts before symlink adoption");
        let outside = directory.path().join("outside.md");
        let outside_bytes = b"outside secret\r\n";
        fs::write(&outside, outside_bytes).expect("write outside target");
        let visible_path = store.root.join("manuscript/Untitled.md");
        symlink(&outside, &visible_path).expect("create manuscript symlink");

        let result = store.adopt_visible_document_if_absent(
            "manuscript/Untitled.md",
            DocumentKind::Prose,
            "must refuse symlink",
        );

        assert!(matches!(result, Err(StoreError::SymbolicLink(path)) if path == visible_path));
        assert_eq!(
            fs::read(outside).expect("outside file survives"),
            outside_bytes
        );
        assert_eq!(
            store.counts().expect("counts after symlink adoption"),
            counts_before
        );
        assert!(store.list_documents().expect("list documents").is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn adopt_visible_document_rejects_symlink_swapped_after_inspection() {
        use std::os::unix::fs::symlink;

        let (directory, mut store) = new_store();
        let counts_before = store.counts().expect("counts before raced adoption");
        let relative_path = "manuscript/Untitled.md";
        let visible_path = store.root.join(relative_path);
        fs::write(&visible_path, b"safe manuscript\n").expect("write inspected manuscript");
        let outside = directory.path().join("outside-secret.md");
        let outside_bytes = b"bytes that must never enter Loom\n";
        fs::write(&outside, outside_bytes).expect("write outside target");

        let result = store.adopt_visible_document_if_absent_with_boundaries(
            relative_path,
            DocumentKind::Prose,
            "must refuse inspection race",
            |visible| {
                fs::remove_file(visible)?;
                symlink(&outside, visible)?;
                Ok(())
            },
            |_| Ok(()),
        );

        assert!(matches!(result, Err(StoreError::SymbolicLink(path)) if path == visible_path));
        assert_eq!(
            store.counts().expect("counts after raced adoption"),
            counts_before
        );
        assert!(store.list_documents().expect("list documents").is_empty());
        assert_eq!(store.pending_outbox_count().expect("pending outbox"), 0);
        assert_eq!(
            fs::read(outside).expect("outside target survives"),
            outside_bytes
        );
    }

    #[cfg(unix)]
    #[test]
    fn adoption_outbox_never_accepts_same_hash_symlink_after_commit() {
        use std::os::unix::fs::symlink;

        let (directory, mut store) = new_store();
        let relative_path = "manuscript/Untitled.md";
        let visible_path = store.root.join(relative_path);
        let bytes = b"identical bytes do not make a symlink safe\n";
        fs::write(&visible_path, bytes).expect("write visible manuscript");
        let outside = directory.path().join("same-bytes-outside.md");
        fs::write(&outside, bytes).expect("write same-hash outside target");

        let result = store.adopt_visible_document_if_absent_with_boundaries(
            relative_path,
            DocumentKind::Prose,
            "must refuse outbox race",
            |_| Ok(()),
            |visible| {
                fs::remove_file(visible)?;
                symlink(&outside, visible)?;
                Ok(())
            },
        );

        assert!(matches!(result, Err(StoreError::SymbolicLink(path)) if path == visible_path));
        assert_eq!(store.pending_outbox_count().expect("pending outbox"), 1);
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT state FROM visible_file_outbox WHERE relative_path = ?1",
                    [relative_path],
                    |row| row.get::<_, String>(0),
                )
                .expect("outbox state"),
            "pending"
        );
        assert_eq!(fs::read(outside).expect("outside target survives"), bytes);
    }

    #[test]
    #[cfg(unix)]
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
    #[cfg(unix)]
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

        fs::write(store.root.join("manuscript/001.md"), "transient")
            .expect("recreate visible file");
        let disappeared = store
            .reconciliation_snapshot_at_boundary("manuscript/001.md", |path| {
                fs::remove_file(path)?;
                Ok(())
            })
            .expect("mid-snapshot deletion is ordinary missing state");
        assert!(disappeared.visible.is_none());
        assert_eq!(disappeared.base_text, "base");
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
