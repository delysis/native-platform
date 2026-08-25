#![forbid(unsafe_code)]

use super::{
    ManagedStore, StoreError, create_private_dir, create_private_dir_all, directory_bytes,
    enforce_private_directory, enforce_private_file, hash_file, path_exists, read_json,
    reject_symlink, sync_directory, write_new_json,
};
use chrono::Utc;
use information_native_types::{
    MANAGED_DOCUMENTS_RECEIPT_SCHEMA, MANAGED_DOCUMENTS_REMOVAL_SCHEMA,
    MANAGED_DOCUMENTS_SEARCH_SCHEMA, ManagedDocumentId, ManagedDocumentLineage,
    ManagedDocumentsReceipt, ManagedDocumentsRemovalPlan, ManagedDocumentsRemovalReceipt,
    ManagedDocumentsRemovalRequest, ManagedDocumentsSearchHit, ManagedDocumentsSearchRequest,
    ManagedDocumentsSearchResult, ManagedDocumentsV1, ManagedMaterializationId, ManagedSegmentId,
    ManagedSourceArtifact,
};
use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

const MANAGED_DOCUMENTS_DIRECTORY: &str = "managed-documents-v1";
const ACTIVE_DIRECTORY: &str = "active";
const STAGING_DIRECTORY: &str = "staging";
const DATABASE_FILE: &str = "documents.sqlite3";
const MANIFEST_FILE: &str = "manifest.json";
const RECEIPT_FILE: &str = "receipt.json";
const EXPECTED_ACTIVE_FILES: [&str; 3] = [DATABASE_FILE, MANIFEST_FILE, RECEIPT_FILE];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublishBoundary {
    DatabaseSynced,
    ManifestPublished,
    ReceiptPublished,
    StageSynced,
    ActivationRenamed,
    ActivationHardened,
    ActivationSynced,
}

#[cfg(test)]
thread_local! {
    static PUBLISH_FAULT: std::cell::Cell<Option<PublishBoundary>> = const {
        std::cell::Cell::new(None)
    };
}

#[cfg(test)]
fn observe_publish_boundary(boundary: PublishBoundary) {
    PUBLISH_FAULT.with(|fault| {
        if fault.get() == Some(boundary) {
            fault.set(None);
            panic!("managed documents publish fault at {boundary:?}");
        }
    });
}

#[cfg(not(test))]
fn observe_publish_boundary(_boundary: PublishBoundary) {}

impl ManagedStore {
    /// Materialize one complete `managed.documents.v1` value into a private
    /// SQLite/FTS5 database and make it visible with one same-filesystem rename.
    /// Source paths are recorded as evidence only and are never opened here.
    pub fn materialize_documents(
        &self,
        materialization: &ManagedDocumentsV1,
    ) -> Result<ManagedDocumentsReceipt, StoreError> {
        materialization.validate()?;
        let _lock = self.try_lock()?;
        ensure_layout(self)?;

        let key = materialization_key(&materialization.materialization_id);
        let active = active_root(self).join(&key);
        if path_exists(&active)? {
            let receipt = validate_active_materialization(self, &active)?;
            make_materialization_directory_immutable(&active)?;
            if receipt_matches_materialization(&receipt, materialization) {
                return Ok(receipt);
            }
            return Err(StoreError::ManagedDocumentsConflict(
                materialization.materialization_id.clone(),
            ));
        }

        let stage = staging_root(self).join(format!("building-{key}"));
        if path_exists(&stage)? {
            remove_build_stage(&stage)?;
        }
        create_private_dir(&stage)?;

        let result = build_and_activate(self, materialization, &stage, &active);
        if result.is_err() && path_exists(&stage).is_ok_and(|exists| exists) {
            let _ = remove_build_stage(&stage);
        }
        result
    }

    /// Run a bounded literal-term FTS5 query against one exact immutable
    /// materialization. This endpoint is local-search-only and does not grant
    /// model-context or export permission.
    pub fn search_managed_documents(
        &self,
        request: &ManagedDocumentsSearchRequest,
    ) -> Result<ManagedDocumentsSearchResult, StoreError> {
        request.validate()?;
        let _lock = self.try_lock()?;
        ensure_layout(self)?;
        let active = active_path(self, &request.materialization_id);
        if !path_exists(&active)? {
            return Err(StoreError::ManagedDocumentsNotFound(
                request.materialization_id.clone(),
            ));
        }
        let receipt = validate_active_materialization(self, &active)?;
        if !digest_equal(&receipt.content_sha256, &request.content_sha256) {
            return Err(StoreError::ManagedDocumentsIdentityMismatch);
        }
        let manifest: ManagedDocumentsV1 =
            read_json(&active.join(MANIFEST_FILE), "managed documents manifest")?;
        manifest.validate()?;
        if manifest.materialization_id != request.materialization_id
            || !digest_equal(&manifest.content_sha256, &request.content_sha256)
        {
            return Err(StoreError::ManagedDocumentsIdentityMismatch);
        }
        let result = search_database(&active.join(DATABASE_FILE), &manifest, request)?;
        result.validate(request)?;
        Ok(result)
    }

    /// Describe the exact Information-owned bytes affected by removal. No
    /// source archive, attachment, or caller-provided path is part of the plan.
    pub fn plan_managed_documents_removal(
        &self,
        materialization_id: &ManagedMaterializationId,
    ) -> Result<ManagedDocumentsRemovalPlan, StoreError> {
        let _lock = self.try_lock()?;
        ensure_layout(self)?;
        let active = active_path(self, materialization_id);
        if !path_exists(&active)? {
            return Err(StoreError::ManagedDocumentsNotFound(
                materialization_id.clone(),
            ));
        }
        let receipt = validate_active_materialization(self, &active)?;
        let plan = ManagedDocumentsRemovalPlan {
            schema: MANAGED_DOCUMENTS_REMOVAL_SCHEMA.to_string(),
            materialization_id: receipt.materialization_id,
            content_sha256: receipt.content_sha256,
            database_sha256: receipt.database_sha256,
            managed_relative_path: receipt.managed_relative_path,
            observed_managed_bytes: directory_bytes(&active)?,
            external_source_bytes_removed: false,
            requires_exact_confirmation: true,
        };
        plan.validate()?;
        Ok(plan)
    }

    /// Remove exactly one confirmed managed representation. A deterministic
    /// removal-stage name makes the operation retryable after an interrupted
    /// delete without accepting a renderer-supplied path.
    pub fn remove_managed_documents(
        &self,
        request: &ManagedDocumentsRemovalRequest,
    ) -> Result<ManagedDocumentsRemovalReceipt, StoreError> {
        request.validate()?;
        let _lock = self.try_lock()?;
        ensure_layout(self)?;
        let key = materialization_key(&request.materialization_id);
        let active = active_root(self).join(&key);
        let removing = staging_root(self).join(format!("removing-{key}"));

        let target = if path_exists(&active)? {
            if path_exists(&removing)? {
                return Err(StoreError::ManagedDocumentsConflict(
                    request.materialization_id.clone(),
                ));
            }
            let receipt = validate_active_materialization(self, &active)?;
            require_exact_receipt(&receipt, request)?;
            make_materialization_directory_writable(&active)?;
            if let Err(error) = fs::rename(&active, &removing) {
                let _ = make_materialization_directory_immutable(&active);
                return Err(StoreError::Io {
                    operation: "hide managed documents before removal",
                    path: removing.clone(),
                    source: error,
                });
            }
            sync_directory(&active_root(self))?;
            sync_directory(&staging_root(self))?;
            removing
        } else if path_exists(&removing)? {
            removing
        } else {
            return Err(StoreError::ManagedDocumentsNotFound(
                request.materialization_id.clone(),
            ));
        };

        let receipt: ManagedDocumentsReceipt =
            read_json(&target.join(RECEIPT_FILE), "managed documents receipt")?;
        receipt.validate()?;
        require_exact_receipt(&receipt, request)?;
        let removed_managed_bytes = directory_bytes(&target)?;
        finish_removal(&target, &receipt)?;
        sync_directory(&staging_root(self))?;

        let removal = ManagedDocumentsRemovalReceipt {
            schema: MANAGED_DOCUMENTS_REMOVAL_SCHEMA.to_string(),
            materialization_id: request.materialization_id.clone(),
            content_sha256: request.content_sha256.clone(),
            database_sha256: request.database_sha256.clone(),
            removed_managed_bytes,
            external_source_bytes_removed: false,
            removed_at: Utc::now(),
        };
        removal.validate()?;
        Ok(removal)
    }
}

fn build_and_activate(
    store: &ManagedStore,
    materialization: &ManagedDocumentsV1,
    stage: &Path,
    active: &Path,
) -> Result<ManagedDocumentsReceipt, StoreError> {
    let database = stage.join(DATABASE_FILE);
    build_database(&database, materialization)?;
    sync_regular_file(&database, "sync managed documents database")?;
    observe_publish_boundary(PublishBoundary::DatabaseSynced);
    let (_, database_sha256, _) = hash_file(&database)?;

    write_new_json(
        &stage.join(MANIFEST_FILE),
        materialization,
        "managed documents manifest",
    )?;
    observe_publish_boundary(PublishBoundary::ManifestPublished);
    let (document_count, segment_count, text_bytes) = materialization_counts(materialization)?;
    let key = materialization_key(&materialization.materialization_id);
    let receipt = ManagedDocumentsReceipt {
        schema: MANAGED_DOCUMENTS_RECEIPT_SCHEMA.to_string(),
        materialization_id: materialization.materialization_id.clone(),
        resource_id: materialization.resource_id.clone(),
        release_id: materialization.release_id.clone(),
        representation_id: materialization.representation_id.clone(),
        content_sha256: materialization.content_sha256.clone(),
        database_sha256,
        document_count,
        segment_count,
        text_bytes,
        managed_relative_path: format!("{MANAGED_DOCUMENTS_DIRECTORY}/{ACTIVE_DIRECTORY}/{key}"),
        activated_at: Utc::now(),
    };
    receipt.validate()?;
    write_new_json(
        &stage.join(RECEIPT_FILE),
        &receipt,
        "managed documents receipt",
    )?;
    observe_publish_boundary(PublishBoundary::ReceiptPublished);
    make_materialization_files_immutable(stage)?;
    sync_directory(stage)?;
    observe_publish_boundary(PublishBoundary::StageSynced);

    fs::rename(stage, active).map_err(|error| StoreError::Io {
        operation: "activate managed documents",
        path: active.to_path_buf(),
        source: error,
    })?;
    observe_publish_boundary(PublishBoundary::ActivationRenamed);
    make_materialization_directory_immutable(active)?;
    observe_publish_boundary(PublishBoundary::ActivationHardened);
    sync_directory(&active_root(store))?;
    sync_directory(&staging_root(store))?;
    observe_publish_boundary(PublishBoundary::ActivationSynced);
    Ok(receipt)
}

fn build_database(path: &Path, materialization: &ManagedDocumentsV1) -> Result<(), StoreError> {
    if path_exists(path)? {
        return Err(StoreError::ManagedDocumentsConflict(
            materialization.materialization_id.clone(),
        ));
    }
    let mut connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|source| sqlite_error("create database", source))?;
    enforce_private_file(path)?;
    connection
        .execute_batch(
            "PRAGMA page_size = 4096;
             PRAGMA journal_mode = DELETE;
             PRAGMA synchronous = FULL;
             PRAGMA foreign_keys = ON;
             PRAGMA trusted_schema = OFF;
             PRAGMA temp_store = MEMORY;
             CREATE TABLE metadata (
                 key TEXT PRIMARY KEY NOT NULL,
                 value TEXT NOT NULL
             ) STRICT;
             CREATE TABLE documents (
                 document_id TEXT PRIMARY KEY NOT NULL,
                 title TEXT NOT NULL,
                 creator TEXT,
                 source_uri TEXT,
                 locator_json TEXT NOT NULL,
                 lineage_json TEXT NOT NULL,
                 rights_json TEXT NOT NULL,
                 use_policy_json TEXT NOT NULL,
                 visibility TEXT NOT NULL CHECK (visibility = 'private'),
                 immutable INTEGER NOT NULL CHECK (immutable = 1)
             ) STRICT;
             CREATE TABLE segments (
                 rowid INTEGER PRIMARY KEY NOT NULL,
                 document_id TEXT NOT NULL REFERENCES documents(document_id),
                 segment_id TEXT NOT NULL UNIQUE,
                 ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
                 text TEXT NOT NULL,
                 text_sha256 TEXT NOT NULL,
                 locator_json TEXT NOT NULL,
                 UNIQUE(document_id, ordinal)
             ) STRICT;
             CREATE VIRTUAL TABLE segments_fts USING fts5(
                 document_id UNINDEXED,
                 segment_id UNINDEXED,
                 title,
                 text,
                 tokenize = 'unicode61 remove_diacritics 2'
             );",
        )
        .map_err(|source| sqlite_error("create strict schema", source))?;

    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|source| sqlite_error("begin materialization", source))?;
    transaction
        .execute(
            "INSERT INTO metadata(key, value) VALUES
             ('schema', ?1), ('content_sha256', ?2), ('materialization_id', ?3)",
            params![
                information_native_types::MANAGED_DOCUMENTS_SCHEMA,
                materialization.content_sha256,
                materialization.materialization_id.as_str(),
            ],
        )
        .map_err(|source| sqlite_error("write materialization metadata", source))?;

    let mut rowid = 0_i64;
    for document in &materialization.documents {
        let locator_json = encode_json(&document.locator, "managed document locator")?;
        let lineage_json = encode_json(&document.lineage, "managed document lineage")?;
        let rights_json = encode_json(&document.rights, "managed document rights")?;
        let use_policy_json = encode_json(&document.use_policy, "managed document use policy")?;
        transaction
            .execute(
                "INSERT INTO documents(
                     document_id, title, creator, source_uri, locator_json,
                     lineage_json, rights_json, use_policy_json, visibility, immutable
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'private', 1)",
                params![
                    document.document_id.as_str(),
                    document.title,
                    document.creator,
                    document.source_uri,
                    locator_json,
                    lineage_json,
                    rights_json,
                    use_policy_json,
                ],
            )
            .map_err(|source| sqlite_error("insert managed document", source))?;

        for segment in &document.segments {
            rowid = rowid.checked_add(1).ok_or(StoreError::IntegerOverflow)?;
            let segment_locator = encode_json(&segment.locator, "managed segment locator")?;
            transaction
                .execute(
                    "INSERT INTO segments(
                         rowid, document_id, segment_id, ordinal, text, text_sha256, locator_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        rowid,
                        document.document_id.as_str(),
                        segment.segment_id.as_str(),
                        i64::from(segment.ordinal),
                        segment.text,
                        segment.text_sha256,
                        segment_locator,
                    ],
                )
                .map_err(|source| sqlite_error("insert managed segment", source))?;
            transaction
                .execute(
                    "INSERT INTO segments_fts(rowid, document_id, segment_id, title, text)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        rowid,
                        document.document_id.as_str(),
                        segment.segment_id.as_str(),
                        document.title,
                        segment.text,
                    ],
                )
                .map_err(|source| sqlite_error("index managed segment", source))?;
        }
    }
    transaction
        .commit()
        .map_err(|source| sqlite_error("commit materialization", source))?;
    connection
        .execute_batch("PRAGMA optimize;")
        .map_err(|source| sqlite_error("optimize managed document index", source))?;
    drop(connection);
    Ok(())
}

fn search_database(
    database: &Path,
    manifest: &ManagedDocumentsV1,
    request: &ManagedDocumentsSearchRequest,
) -> Result<ManagedDocumentsSearchResult, StoreError> {
    let connection = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|source| sqlite_error("open managed documents read-only", source))?;
    connection
        .execute_batch(
            "PRAGMA query_only = ON;
             PRAGMA trusted_schema = OFF;
             PRAGMA foreign_keys = ON;
             PRAGMA temp_store = MEMORY;",
        )
        .map_err(|source| sqlite_error("configure managed documents read-only", source))?;
    let query = literal_fts_query(&request.query);
    let mut statement = connection
        .prepare(
            "SELECT
                 s.document_id,
                 s.segment_id,
                 s.ordinal,
                 d.title,
                 d.creator,
                 snippet(segments_fts, 3, '', '', ' … ', 32),
                 s.text_sha256,
                 d.locator_json,
                 s.locator_json,
                 d.lineage_json,
                 d.rights_json,
                 d.use_policy_json
             FROM segments_fts
             JOIN segments s ON s.rowid = segments_fts.rowid
             JOIN documents d ON d.document_id = s.document_id
             WHERE segments_fts MATCH ?1
             ORDER BY bm25(segments_fts), s.document_id, s.ordinal
             LIMIT ?2",
        )
        .map_err(|source| sqlite_error("prepare managed documents search", source))?;
    let rows = statement
        .query_map(params![query, i64::from(request.max_hits)], |row| {
            Ok(SearchRow {
                document_id: row.get(0)?,
                segment_id: row.get(1)?,
                ordinal: row.get(2)?,
                title: row.get(3)?,
                creator: row.get(4)?,
                snippet: row.get(5)?,
                segment_text_sha256: row.get(6)?,
                document_locator_json: row.get(7)?,
                segment_locator_json: row.get(8)?,
                lineage_json: row.get(9)?,
                rights_json: row.get(10)?,
                use_policy_json: row.get(11)?,
            })
        })
        .map_err(|source| sqlite_error("execute managed documents search", source))?;

    let mut hits = Vec::new();
    for row in rows {
        let row =
            row.map_err(|source| sqlite_error("read managed documents search row", source))?;
        let lineage: Vec<ManagedDocumentLineage> =
            decode_json(&row.lineage_json, "managed document lineage")?;
        let source_artifacts = source_artifacts_for_lineage(manifest, &lineage)?;
        let rank = u32::try_from(hits.len() + 1).map_err(|_| StoreError::IntegerOverflow)?;
        hits.push(ManagedDocumentsSearchHit {
            rank,
            document_id: ManagedDocumentId::parse(row.document_id)?,
            segment_id: ManagedSegmentId::parse(row.segment_id)?,
            ordinal: u32::try_from(row.ordinal).map_err(|_| {
                StoreError::RegistryCorrupt("managed segment ordinal is negative".to_string())
            })?,
            title: row.title,
            creator: row.creator,
            snippet: truncate_chars(&row.snippet, request.max_snippet_chars)?,
            segment_text_sha256: row.segment_text_sha256,
            document_locator: decode_json(&row.document_locator_json, "managed document locator")?,
            segment_locator: decode_json(&row.segment_locator_json, "managed segment locator")?,
            provenance: manifest.provenance.clone(),
            source_artifacts,
            lineage,
            rights: decode_json(&row.rights_json, "managed document rights")?,
            use_policy: decode_json(&row.use_policy_json, "managed document use policy")?,
        });
    }
    Ok(ManagedDocumentsSearchResult {
        schema: MANAGED_DOCUMENTS_SEARCH_SCHEMA.to_string(),
        materialization_id: request.materialization_id.clone(),
        content_sha256: request.content_sha256.clone(),
        complete: true,
        hits,
    })
}

#[derive(Debug)]
struct SearchRow {
    document_id: String,
    segment_id: String,
    ordinal: i64,
    title: String,
    creator: Option<String>,
    snippet: String,
    segment_text_sha256: String,
    document_locator_json: String,
    segment_locator_json: String,
    lineage_json: String,
    rights_json: String,
    use_policy_json: String,
}

fn source_artifacts_for_lineage(
    manifest: &ManagedDocumentsV1,
    lineage: &[ManagedDocumentLineage],
) -> Result<Vec<ManagedSourceArtifact>, StoreError> {
    let ids = lineage
        .iter()
        .map(|entry| &entry.source_artifact_id)
        .collect::<BTreeSet<_>>();
    let artifacts = manifest
        .source_artifacts
        .iter()
        .filter(|artifact| ids.contains(&artifact.artifact_id))
        .cloned()
        .collect::<Vec<_>>();
    if artifacts.len() != ids.len() {
        return Err(StoreError::RegistryCorrupt(
            "managed document lineage references a missing source artifact".to_string(),
        ));
    }
    Ok(artifacts)
}

fn validate_active_materialization(
    store: &ManagedStore,
    active: &Path,
) -> Result<ManagedDocumentsReceipt, StoreError> {
    validate_materialization_directory(active, true)?;
    let receipt: ManagedDocumentsReceipt =
        read_json(&active.join(RECEIPT_FILE), "managed documents receipt")?;
    receipt.validate()?;
    let expected = store.root.join(&receipt.managed_relative_path);
    if expected != active {
        return Err(StoreError::RegistryCorrupt(
            "managed documents receipt path disagrees with its active directory".to_string(),
        ));
    }
    let manifest: ManagedDocumentsV1 =
        read_json(&active.join(MANIFEST_FILE), "managed documents manifest")?;
    manifest.validate()?;
    if !receipt_matches_materialization(&receipt, &manifest) {
        return Err(StoreError::RegistryCorrupt(
            "managed documents receipt disagrees with its immutable manifest".to_string(),
        ));
    }
    let (_, observed_database_sha256, _) = hash_file(&active.join(DATABASE_FILE))?;
    if !digest_equal(&receipt.database_sha256, &observed_database_sha256) {
        return Err(StoreError::RegistryCorrupt(
            "managed documents database digest disagrees with its receipt".to_string(),
        ));
    }
    Ok(receipt)
}

fn validate_materialization_directory(path: &Path, require_all: bool) -> Result<(), StoreError> {
    reject_symlink(path)?;
    let metadata = fs::metadata(path).map_err(|error| StoreError::Io {
        operation: "inspect managed documents directory",
        path: path.to_path_buf(),
        source: error,
    })?;
    if !metadata.is_dir() {
        return Err(StoreError::UnsafePath {
            path: path.to_path_buf(),
            reason: "managed documents representation is not a directory",
        });
    }
    let mut observed = BTreeSet::new();
    for entry in fs::read_dir(path)
        .map_err(|error| StoreError::Io {
            operation: "read managed documents directory",
            path: path.to_path_buf(),
            source: error,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| StoreError::Io {
            operation: "read managed documents directory entry",
            path: path.to_path_buf(),
            source: error,
        })?
    {
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| StoreError::UnsafePath {
            path: entry.path(),
            reason: "managed documents entry name is not UTF-8",
        })?;
        if !EXPECTED_ACTIVE_FILES.contains(&name) {
            return Err(StoreError::UnsafePath {
                path: entry.path(),
                reason: "unexpected file in managed documents representation",
            });
        }
        let file_type = entry.file_type().map_err(|error| StoreError::Io {
            operation: "inspect managed documents entry",
            path: entry.path(),
            source: error,
        })?;
        if file_type.is_symlink() || !file_type.is_file() {
            return Err(StoreError::UnsafePath {
                path: entry.path(),
                reason: "managed documents entry is not a regular file",
            });
        }
        observed.insert(name.to_string());
    }
    if require_all
        && EXPECTED_ACTIVE_FILES
            .iter()
            .any(|expected| !observed.contains(*expected))
    {
        return Err(StoreError::RegistryCorrupt(
            "managed documents representation is incomplete".to_string(),
        ));
    }
    Ok(())
}

fn finish_removal(path: &Path, receipt: &ManagedDocumentsReceipt) -> Result<(), StoreError> {
    validate_materialization_directory(path, false)?;
    make_materialization_writable(path)?;
    let database = path.join(DATABASE_FILE);
    if path_exists(&database)? {
        let (_, observed_sha256, _) = hash_file(&database)?;
        if !digest_equal(&receipt.database_sha256, &observed_sha256) {
            return Err(StoreError::ManagedDocumentsIdentityMismatch);
        }
        fs::remove_file(&database).map_err(|error| StoreError::Io {
            operation: "remove managed documents database",
            path: database,
            source: error,
        })?;
        sync_directory(path)?;
    }
    for (name, operation) in [
        (MANIFEST_FILE, "remove managed documents manifest"),
        (RECEIPT_FILE, "remove managed documents receipt"),
    ] {
        let target = path.join(name);
        if path_exists(&target)? {
            fs::remove_file(&target).map_err(|error| StoreError::Io {
                operation,
                path: target,
                source: error,
            })?;
        }
    }
    fs::remove_dir(path).map_err(|error| StoreError::Io {
        operation: "remove empty managed documents representation",
        path: path.to_path_buf(),
        source: error,
    })
}

fn remove_build_stage(path: &Path) -> Result<(), StoreError> {
    validate_materialization_directory(path, false)?;
    make_materialization_writable(path)?;
    for name in EXPECTED_ACTIVE_FILES {
        let target = path.join(name);
        if path_exists(&target)? {
            fs::remove_file(&target).map_err(|error| StoreError::Io {
                operation: "remove interrupted managed documents stage",
                path: target,
                source: error,
            })?;
        }
    }
    fs::remove_dir(path).map_err(|error| StoreError::Io {
        operation: "remove empty managed documents stage",
        path: path.to_path_buf(),
        source: error,
    })
}

fn ensure_layout(store: &ManagedStore) -> Result<(), StoreError> {
    let root = documents_root(store);
    create_private_dir_all(&root)?;
    create_private_dir_all(&active_root(store))?;
    create_private_dir_all(&staging_root(store))?;
    for directory in [root, active_root(store), staging_root(store)] {
        reject_symlink(&directory)?;
        enforce_private_directory(&directory)?;
    }
    Ok(())
}

fn documents_root(store: &ManagedStore) -> PathBuf {
    store.root.join(MANAGED_DOCUMENTS_DIRECTORY)
}

fn active_root(store: &ManagedStore) -> PathBuf {
    documents_root(store).join(ACTIVE_DIRECTORY)
}

fn staging_root(store: &ManagedStore) -> PathBuf {
    documents_root(store).join(STAGING_DIRECTORY)
}

fn active_path(store: &ManagedStore, materialization_id: &ManagedMaterializationId) -> PathBuf {
    active_root(store).join(materialization_key(materialization_id))
}

fn materialization_key(materialization_id: &ManagedMaterializationId) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"managed.documents.v1.materialization-key\0");
    hasher.update(materialization_id.as_str().as_bytes());
    hex::encode(hasher.finalize())
}

fn receipt_matches_materialization(
    receipt: &ManagedDocumentsReceipt,
    materialization: &ManagedDocumentsV1,
) -> bool {
    receipt.materialization_id == materialization.materialization_id
        && receipt.resource_id == materialization.resource_id
        && receipt.release_id == materialization.release_id
        && receipt.representation_id == materialization.representation_id
        && digest_equal(&receipt.content_sha256, &materialization.content_sha256)
}

fn require_exact_receipt(
    receipt: &ManagedDocumentsReceipt,
    request: &ManagedDocumentsRemovalRequest,
) -> Result<(), StoreError> {
    if receipt.materialization_id != request.materialization_id
        || !digest_equal(&receipt.content_sha256, &request.content_sha256)
        || !digest_equal(&receipt.database_sha256, &request.database_sha256)
    {
        return Err(StoreError::ManagedDocumentsIdentityMismatch);
    }
    Ok(())
}

fn materialization_counts(
    materialization: &ManagedDocumentsV1,
) -> Result<(u64, u64, u64), StoreError> {
    let documents =
        u64::try_from(materialization.documents.len()).map_err(|_| StoreError::IntegerOverflow)?;
    let mut segments = 0_u64;
    let mut text_bytes = 0_u64;
    for document in &materialization.documents {
        segments = segments
            .checked_add(
                u64::try_from(document.segments.len()).map_err(|_| StoreError::IntegerOverflow)?,
            )
            .ok_or(StoreError::IntegerOverflow)?;
        for segment in &document.segments {
            text_bytes = text_bytes
                .checked_add(
                    u64::try_from(segment.text.len()).map_err(|_| StoreError::IntegerOverflow)?,
                )
                .ok_or(StoreError::IntegerOverflow)?;
        }
    }
    Ok((documents, segments, text_bytes))
}

fn literal_fts_query(input: &str) -> String {
    input
        .split_whitespace()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn truncate_chars(value: &str, max_chars: u32) -> Result<String, StoreError> {
    let max_chars = usize::try_from(max_chars).map_err(|_| StoreError::IntegerOverflow)?;
    if value.chars().count() <= max_chars {
        return Ok(value.to_string());
    }
    Ok(value.chars().take(max_chars).collect())
}

fn encode_json<T: Serialize>(value: &T, context: &'static str) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|source| StoreError::Json { context, source })
}

fn decode_json<T: DeserializeOwned>(value: &str, context: &'static str) -> Result<T, StoreError> {
    serde_json::from_str(value).map_err(|source| StoreError::Json { context, source })
}

fn sqlite_error(operation: &'static str, source: rusqlite::Error) -> StoreError {
    StoreError::ManagedDocumentsSqlite { operation, source }
}

fn digest_equal(left: &str, right: &str) -> bool {
    left.strip_prefix("sha256:")
        .unwrap_or(left)
        .eq_ignore_ascii_case(right.strip_prefix("sha256:").unwrap_or(right))
}

fn sync_regular_file(path: &Path, operation: &'static str) -> Result<(), StoreError> {
    let file = OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|error| StoreError::Io {
            operation,
            path: path.to_path_buf(),
            source: error,
        })?;
    file.sync_all().map_err(|error| StoreError::Io {
        operation,
        path: path.to_path_buf(),
        source: error,
    })
}

#[cfg(unix)]
fn make_materialization_files_immutable(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;

    for name in EXPECTED_ACTIVE_FILES {
        let file = path.join(name);
        fs::set_permissions(&file, fs::Permissions::from_mode(0o400)).map_err(|error| {
            StoreError::Io {
                operation: "make managed documents file immutable",
                path: file,
                source: error,
            }
        })?;
    }
    Ok(())
}

#[cfg(unix)]
fn make_materialization_directory_immutable(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o500)).map_err(|error| StoreError::Io {
        operation: "make managed documents directory immutable",
        path: path.to_path_buf(),
        source: error,
    })
}

#[cfg(not(unix))]
fn make_materialization_files_immutable(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}

#[cfg(not(unix))]
fn make_materialization_directory_immutable(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}

#[cfg(unix)]
fn make_materialization_writable(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;

    make_materialization_directory_writable(path)?;
    for name in EXPECTED_ACTIVE_FILES {
        let file = path.join(name);
        if path_exists(&file)? {
            fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).map_err(|error| {
                StoreError::Io {
                    operation: "make managed documents removal file writable",
                    path: file,
                    source: error,
                }
            })?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn make_materialization_writable(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}

#[cfg(unix)]
fn make_materialization_directory_writable(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| StoreError::Io {
        operation: "make managed documents removal directory writable",
        path: path.to_path_buf(),
        source: error,
    })
}

#[cfg(not(unix))]
fn make_materialization_directory_writable(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use information_native_types::{
        ArtifactId, EvidenceLocator, MANAGED_DOCUMENTS_SCHEMA, ManagedDocumentLineage,
        ManagedTextSegment, Provenance, ReleaseId, RepresentationId, ResourceId,
        default_managed_document_rights, default_managed_document_use_policy,
    };
    use std::collections::BTreeMap;
    use std::error::Error;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use tempfile::tempdir;

    fn fixture_materialization() -> Result<ManagedDocumentsV1, Box<dyn Error>> {
        let artifact_id = ArtifactId::parse("source-archive")?;
        let locator = EvidenceLocator::Record {
            collection: Some("fixture".to_string()),
            key: "record-1".to_string(),
        };
        let mut segment = ManagedTextSegment {
            segment_id: ManagedSegmentId::parse("segment-1")?,
            ordinal: 0,
            text: "Alpha contemplative text and beta evidence.".to_string(),
            text_sha256: String::new(),
            locator: locator.clone(),
        };
        segment.refresh_text_sha256();
        let document = information_native_types::ManagedDocument {
            document_id: ManagedDocumentId::parse("document-1")?,
            title: "Fixture Document".to_string(),
            creator: Some("Fixture Custodian".to_string()),
            source_uri: Some("file:///immutable/fixture.txt".to_string()),
            locator: locator.clone(),
            immutable: true,
            visibility: Default::default(),
            lineage: vec![ManagedDocumentLineage {
                source_artifact_id: artifact_id.clone(),
                source_record_id: "record-1".to_string(),
                source_record_sha256: "1".repeat(64),
                source_locator: locator,
                transformation: "bounded inert text extraction".to_string(),
            }],
            rights: default_managed_document_rights(),
            use_policy: default_managed_document_use_policy(),
            segments: vec![segment],
        };
        let mut materialization = ManagedDocumentsV1 {
            schema: MANAGED_DOCUMENTS_SCHEMA.to_string(),
            materialization_id: ManagedMaterializationId::parse("fixture-materialization")?,
            resource_id: ResourceId::parse("fixture-resource")?,
            release_id: ReleaseId::parse("fixture-release")?,
            representation_id: RepresentationId::parse("fixture-representation")?,
            created_at: Utc::now(),
            provenance: Provenance {
                publisher: "Fixture Publisher".to_string(),
                source_uri: "file:///immutable/fixture.txt".to_string(),
                upstream_record_id: Some("fixture".to_string()),
                source_inputs: vec!["source-archive".to_string()],
                transformation: Some("managed.documents.v1 fixture".to_string()),
                metadata: BTreeMap::new(),
            },
            source_artifacts: vec![ManagedSourceArtifact {
                artifact_id,
                source_uri: "file:///immutable/fixture.txt".to_string(),
                bytes: 42,
                sha256: "2".repeat(64),
                immutable: true,
            }],
            documents: vec![document],
            content_sha256: String::new(),
        };
        materialization.refresh_content_sha256()?;
        Ok(materialization)
    }

    #[test]
    fn materialization_is_atomic_searchable_idempotent_and_exactly_removable()
    -> Result<(), Box<dyn Error>> {
        let temporary = tempdir()?;
        let store = ManagedStore::open(temporary.path().join("managed"))?;
        let materialization = fixture_materialization()?;

        let receipt = store.materialize_documents(&materialization)?;
        assert_eq!(receipt.document_count, 1);
        assert_eq!(receipt.segment_count, 1);
        assert_eq!(
            store.materialize_documents(&materialization)?,
            receipt,
            "an exact retry must be idempotent"
        );

        let request = ManagedDocumentsSearchRequest {
            schema: MANAGED_DOCUMENTS_SEARCH_SCHEMA.to_string(),
            materialization_id: materialization.materialization_id.clone(),
            content_sha256: materialization.content_sha256.clone(),
            query: "contemplative evidence".to_string(),
            max_hits: 10,
            max_snippet_chars: 256,
        };
        let search = store.search_managed_documents(&request)?;
        assert_eq!(search.hits.len(), 1);
        assert_eq!(search.hits[0].document_id.as_str(), "document-1");
        assert_eq!(search.hits[0].source_artifacts.len(), 1);

        let plan = store.plan_managed_documents_removal(&materialization.materialization_id)?;
        assert!(!plan.external_source_bytes_removed);
        assert!(plan.requires_exact_confirmation);
        let removal = store.remove_managed_documents(&ManagedDocumentsRemovalRequest {
            schema: MANAGED_DOCUMENTS_REMOVAL_SCHEMA.to_string(),
            materialization_id: materialization.materialization_id.clone(),
            content_sha256: plan.content_sha256,
            database_sha256: plan.database_sha256,
        })?;
        assert!(!removal.external_source_bytes_removed);
        assert!(removal.removed_managed_bytes > 0);
        assert!(matches!(
            store.search_managed_documents(&request),
            Err(StoreError::ManagedDocumentsNotFound(_))
        ));
        Ok(())
    }

    #[test]
    fn conflicting_content_and_stale_removal_confirmation_fail_closed() -> Result<(), Box<dyn Error>>
    {
        let temporary = tempdir()?;
        let store = ManagedStore::open(temporary.path().join("managed"))?;
        let materialization = fixture_materialization()?;
        let receipt = store.materialize_documents(&materialization)?;

        let mut conflict = materialization.clone();
        conflict.documents[0].segments[0].text.push_str(" changed");
        conflict.documents[0].segments[0].refresh_text_sha256();
        conflict.refresh_content_sha256()?;
        assert!(matches!(
            store.materialize_documents(&conflict),
            Err(StoreError::ManagedDocumentsConflict(_))
        ));

        let stale = ManagedDocumentsRemovalRequest {
            schema: MANAGED_DOCUMENTS_REMOVAL_SCHEMA.to_string(),
            materialization_id: materialization.materialization_id,
            content_sha256: receipt.content_sha256,
            database_sha256: "f".repeat(64),
        };
        assert!(matches!(
            store.remove_managed_documents(&stale),
            Err(StoreError::ManagedDocumentsIdentityMismatch)
        ));
        Ok(())
    }

    #[test]
    fn every_publish_boundary_recovers_to_one_exact_searchable_release()
    -> Result<(), Box<dyn Error>> {
        let boundaries = [
            PublishBoundary::DatabaseSynced,
            PublishBoundary::ManifestPublished,
            PublishBoundary::ReceiptPublished,
            PublishBoundary::StageSynced,
            PublishBoundary::ActivationRenamed,
            PublishBoundary::ActivationHardened,
            PublishBoundary::ActivationSynced,
        ];

        for boundary in boundaries {
            let temporary = tempdir()?;
            let store = ManagedStore::open(temporary.path().join("managed"))?;
            let materialization = fixture_materialization()?;
            PUBLISH_FAULT.with(|fault| fault.set(Some(boundary)));

            let crashed = catch_unwind(AssertUnwindSafe(|| {
                let _ = store.materialize_documents(&materialization);
            }));
            assert!(
                crashed.is_err(),
                "fault at {boundary:?} must interrupt publish"
            );

            let key = materialization_key(&materialization.materialization_id);
            let active = active_root(&store).join(&key);
            let stage = staging_root(&store).join(format!("building-{key}"));
            if matches!(
                boundary,
                PublishBoundary::ActivationRenamed
                    | PublishBoundary::ActivationHardened
                    | PublishBoundary::ActivationSynced
            ) {
                assert!(path_exists(&active)?);
                assert!(!path_exists(&stage)?);
            } else {
                assert!(!path_exists(&active)?);
                assert!(path_exists(&stage)?);
            }

            let receipt = store.materialize_documents(&materialization)?;
            assert_eq!(receipt.content_sha256, materialization.content_sha256);
            assert!(!path_exists(&stage)?);
            let request = ManagedDocumentsSearchRequest {
                schema: MANAGED_DOCUMENTS_SEARCH_SCHEMA.to_string(),
                materialization_id: materialization.materialization_id.clone(),
                content_sha256: materialization.content_sha256.clone(),
                query: "contemplative evidence".to_string(),
                max_hits: 10,
                max_snippet_chars: 256,
            };
            assert_eq!(store.search_managed_documents(&request)?.hits.len(), 1);
        }
        Ok(())
    }
}
