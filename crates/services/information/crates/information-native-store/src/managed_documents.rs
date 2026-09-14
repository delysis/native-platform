#![forbid(unsafe_code)]

use super::{
    ManagedStore, StoreError, create_private_dir, create_private_dir_all, directory_bytes,
    enforce_private_directory, enforce_private_file, hash_file, path_exists, read_json,
    reject_symlink, sync_directory, write_new_json,
};
use chrono::{DateTime, Utc};
use information_native_types::{
    EvidenceLocator, MANAGED_DOCUMENTS_RECEIPT_SCHEMA, MANAGED_DOCUMENTS_REMOVAL_SCHEMA,
    MANAGED_DOCUMENTS_SEARCH_SCHEMA, ManagedDocumentId, ManagedDocumentLineage,
    ManagedDocumentsReceipt, ManagedDocumentsRemovalPlan, ManagedDocumentsRemovalReceipt,
    ManagedDocumentsRemovalRequest, ManagedDocumentsSearchHit, ManagedDocumentsSearchRequest,
    ManagedDocumentsSearchResult, ManagedDocumentsV1, ManagedMaterializationId, ManagedSegmentId,
    ManagedSourceArtifact, Provenance, ReleaseId, RepresentationId, ResourceId, RightsStatement,
    UsePolicy,
};
use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

const MANAGED_DOCUMENTS_DIRECTORY: &str = "managed-documents-v1";
const ACTIVE_DIRECTORY: &str = "active";
const STAGING_DIRECTORY: &str = "staging";
const DATABASE_FILE: &str = "documents.sqlite3";
const MANIFEST_FILE: &str = "manifest.json";
const RECEIPT_FILE: &str = "receipt.json";
const EXPECTED_ACTIVE_FILES: [&str; 3] = [DATABASE_FILE, MANIFEST_FILE, RECEIPT_FILE];
const MAX_ACTIVE_PROJECTION_ENTRIES: usize = 256;
const MAX_RECEIPT_PROJECTION_BYTES: u64 = 64 * 1024;
const MAX_MANIFEST_PROJECTION_BYTES: u64 = 2 * 1024 * 1024;

/// Path-free projection for one active immutable managed representation.
/// Canonical text remains inside the managed database. The producing method's
/// contract determines whether this is a bounded manifest projection or a fully
/// database-validated exact representation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ActiveManagedMaterialization {
    pub materialization_id: ManagedMaterializationId,
    pub resource_id: ResourceId,
    pub release_id: ReleaseId,
    pub representation_id: RepresentationId,
    pub content_sha256: String,
    pub database_sha256: String,
    pub document_count: u64,
    pub segment_count: u64,
    pub text_bytes: u64,
    pub activated_at: DateTime<Utc>,
    pub provenance: Provenance,
    pub source_artifacts: Vec<ManagedSourceArtifact>,
    pub documents: Vec<ActiveManagedDocument>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ActiveManagedDocument {
    pub document_id: ManagedDocumentId,
    pub title: String,
    pub creator: Option<String>,
    pub locator: EvidenceLocator,
    pub lineage: Vec<ManagedDocumentLineage>,
    pub rights: Vec<RightsStatement>,
    pub use_policy: UsePolicy,
    pub segments: Vec<ActiveManagedSegment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ActiveManagedSegment {
    pub segment_id: ManagedSegmentId,
    pub ordinal: u32,
    pub text_sha256: String,
    pub locator: EvidenceLocator,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ActiveManagedDocumentsProjection {
    pub complete: bool,
    pub entries: Vec<ActiveManagedReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ActiveManagedReceipt {
    pub materialization_id: ManagedMaterializationId,
    pub resource_id: ResourceId,
    pub release_id: ReleaseId,
    pub representation_id: RepresentationId,
    pub content_sha256: String,
    pub database_sha256: String,
    pub document_count: u64,
    pub segment_count: u64,
    pub text_bytes: u64,
    pub activated_at: DateTime<Utc>,
}

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
    /// Enumerate a caller-bounded discoverability projection of active receipt
    /// identities without exposing managed or source filesystem paths. This
    /// validates directory shape plus a byte-capped receipt and path binding;
    /// it intentionally does not deserialize manifests or hash databases.
    /// Exact actions must call [`Self::active_managed_document`].
    pub fn list_active_managed_documents(
        &self,
        max_entries: usize,
    ) -> Result<ActiveManagedDocumentsProjection, StoreError> {
        if max_entries == 0 || max_entries > MAX_ACTIVE_PROJECTION_ENTRIES {
            return Err(StoreError::RegistryCorrupt(format!(
                "managed documents projection cap must be between 1 and {MAX_ACTIVE_PROJECTION_ENTRIES}"
            )));
        }
        let _lock = self.try_lock()?;
        ensure_layout(self)?;
        let root = active_root(self);
        let directory = fs::read_dir(&root).map_err(|source| StoreError::Io {
            operation: "list active managed documents",
            path: root.clone(),
            source,
        })?;
        let mut paths = Vec::with_capacity(max_entries.saturating_add(1));
        for entry in directory.take(max_entries.saturating_add(1)) {
            let entry = entry.map_err(|source| StoreError::Io {
                operation: "read active managed documents entry",
                path: root.clone(),
                source,
            })?;
            paths.push(entry.path());
        }
        paths.sort();
        if paths.len() > max_entries {
            return Err(StoreError::RegistryCorrupt(format!(
                "active managed documents count {} exceeds product projection cap {max_entries}",
                paths.len()
            )));
        }
        let entries = paths
            .iter()
            .map(|path| active_receipt_projection(self, path))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ActiveManagedDocumentsProjection {
            complete: true,
            entries,
        })
    }

    /// Resolve one exact active representation through full receipt, manifest,
    /// and database validation. Bounded enumeration intentionally does less and
    /// cannot authorize an exact action.
    pub fn active_managed_document(
        &self,
        materialization_id: &ManagedMaterializationId,
    ) -> Result<ActiveManagedMaterialization, StoreError> {
        let _lock = self.try_lock()?;
        ensure_layout(self)?;
        let active = active_path(self, materialization_id);
        if !path_exists(&active)? {
            return Err(StoreError::ManagedDocumentsNotFound(
                materialization_id.clone(),
            ));
        }
        active_projection(self, &active)
    }

    /// Read one exact active receipt and a caller-selected, byte-bounded
    /// manifest projection without hashing the managed database. This is for
    /// discoverability only. Search, citation, and removal must use
    /// [`Self::active_managed_document`] first.
    pub fn project_active_managed_document(
        &self,
        materialization_id: &ManagedMaterializationId,
        max_manifest_bytes: u64,
    ) -> Result<ActiveManagedMaterialization, StoreError> {
        if max_manifest_bytes == 0 || max_manifest_bytes > MAX_MANIFEST_PROJECTION_BYTES {
            return Err(StoreError::RegistryCorrupt(format!(
                "managed documents manifest projection cap must be between 1 and {MAX_MANIFEST_PROJECTION_BYTES} bytes"
            )));
        }
        let _lock = self.try_lock()?;
        ensure_layout(self)?;
        let active = active_path(self, materialization_id);
        if !path_exists(&active)? {
            return Err(StoreError::ManagedDocumentsNotFound(
                materialization_id.clone(),
            ));
        }
        active_projection_without_database(self, &active, max_manifest_bytes)
    }

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
            if receipt_matches_materialization(&receipt, materialization) {
                make_materialization_directory_immutable(&active)?;
                sync_publication_roots(self)
                    .map_err(|source| committed_documents_error(&receipt, true, source))?;
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
            removing
        } else if path_exists(&removing)? {
            removing
        } else {
            return Err(StoreError::ManagedDocumentsNotFound(
                request.materialization_id.clone(),
            ));
        };

        let finish = || {
            sync_publication_roots(self)?;
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
        };
        let removal = finish().map_err(|source| StoreError::ManagedDocumentsCommitted {
            materialization_id: request.materialization_id.clone(),
            content_sha256: request.content_sha256.clone(),
            database_sha256: request.database_sha256.clone(),
            visible: false,
            source: Box::new(source),
        })?;

        Ok(removal)
    }
}

fn active_projection(
    store: &ManagedStore,
    active: &Path,
) -> Result<ActiveManagedMaterialization, StoreError> {
    let receipt = validate_active_materialization(store, active)?;
    let manifest: ManagedDocumentsV1 =
        read_json(&active.join(MANIFEST_FILE), "managed documents manifest")?;
    projection_from_parts(receipt_projection_from_receipt(receipt), manifest)
}

fn projection_from_parts(
    receipt: ActiveManagedReceipt,
    manifest: ManagedDocumentsV1,
) -> Result<ActiveManagedMaterialization, StoreError> {
    let (document_count, segment_count, text_bytes) = materialization_counts(&manifest)?;
    if document_count != receipt.document_count
        || segment_count != receipt.segment_count
        || text_bytes != receipt.text_bytes
    {
        return Err(StoreError::RegistryCorrupt(
            "managed documents receipt accounting disagrees with its manifest".to_string(),
        ));
    }
    let documents = manifest
        .documents
        .iter()
        .map(|document| ActiveManagedDocument {
            document_id: document.document_id.clone(),
            title: document.title.clone(),
            creator: document.creator.clone(),
            locator: document.locator.clone(),
            lineage: document.lineage.clone(),
            rights: document.rights.clone(),
            use_policy: document.use_policy,
            segments: document
                .segments
                .iter()
                .map(|segment| ActiveManagedSegment {
                    segment_id: segment.segment_id.clone(),
                    ordinal: segment.ordinal,
                    text_sha256: segment.text_sha256.clone(),
                    locator: segment.locator.clone(),
                })
                .collect(),
        })
        .collect();
    Ok(ActiveManagedMaterialization {
        materialization_id: receipt.materialization_id,
        resource_id: receipt.resource_id,
        release_id: receipt.release_id,
        representation_id: receipt.representation_id,
        content_sha256: receipt.content_sha256,
        database_sha256: receipt.database_sha256,
        document_count: receipt.document_count,
        segment_count: receipt.segment_count,
        text_bytes: receipt.text_bytes,
        activated_at: receipt.activated_at,
        provenance: manifest.provenance,
        source_artifacts: manifest.source_artifacts,
        documents,
    })
}

fn active_receipt_projection(
    store: &ManagedStore,
    active: &Path,
) -> Result<ActiveManagedReceipt, StoreError> {
    validate_materialization_directory(active, true)?;
    let receipt: ManagedDocumentsReceipt = read_json_bounded(
        &active.join(RECEIPT_FILE),
        "managed documents receipt projection",
        MAX_RECEIPT_PROJECTION_BYTES,
    )?;
    receipt.validate()?;
    let expected = store.root.join(&receipt.managed_relative_path);
    if expected != active {
        return Err(StoreError::RegistryCorrupt(
            "managed documents receipt path disagrees with its active directory".to_string(),
        ));
    }
    Ok(receipt_projection_from_receipt(receipt))
}

fn receipt_projection_from_receipt(receipt: ManagedDocumentsReceipt) -> ActiveManagedReceipt {
    ActiveManagedReceipt {
        materialization_id: receipt.materialization_id,
        resource_id: receipt.resource_id,
        release_id: receipt.release_id,
        representation_id: receipt.representation_id,
        content_sha256: receipt.content_sha256,
        database_sha256: receipt.database_sha256,
        document_count: receipt.document_count,
        segment_count: receipt.segment_count,
        text_bytes: receipt.text_bytes,
        activated_at: receipt.activated_at,
    }
}

fn active_projection_without_database(
    store: &ManagedStore,
    active: &Path,
    max_manifest_bytes: u64,
) -> Result<ActiveManagedMaterialization, StoreError> {
    let receipt = active_receipt_projection(store, active)?;
    let manifest: ManagedDocumentsV1 = read_json_bounded(
        &active.join(MANIFEST_FILE),
        "managed documents manifest projection",
        max_manifest_bytes,
    )?;
    manifest.validate()?;
    let synthetic_receipt = ManagedDocumentsReceipt {
        schema: MANAGED_DOCUMENTS_RECEIPT_SCHEMA.to_string(),
        materialization_id: receipt.materialization_id.clone(),
        resource_id: receipt.resource_id.clone(),
        release_id: receipt.release_id.clone(),
        representation_id: receipt.representation_id.clone(),
        content_sha256: receipt.content_sha256.clone(),
        database_sha256: receipt.database_sha256.clone(),
        document_count: receipt.document_count,
        segment_count: receipt.segment_count,
        text_bytes: receipt.text_bytes,
        managed_relative_path: String::new(),
        activated_at: receipt.activated_at,
    };
    if !receipt_matches_materialization(&synthetic_receipt, &manifest) {
        return Err(StoreError::RegistryCorrupt(
            "managed documents receipt disagrees with its immutable manifest".to_string(),
        ));
    }
    projection_from_parts(receipt, manifest)
}

fn read_json_bounded<T: for<'de> Deserialize<'de>>(
    path: &Path,
    context: &'static str,
    max_bytes: u64,
) -> Result<T, StoreError> {
    reject_symlink(path)?;
    let file = OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|source| StoreError::Io {
            operation: "open bounded managed projection file",
            path: path.to_path_buf(),
            source,
        })?;
    let metadata = file.metadata().map_err(|source| StoreError::Io {
        operation: "inspect bounded managed projection file",
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        return Err(StoreError::UnsafePath {
            path: path.to_path_buf(),
            reason: "managed projection file is not regular or exceeds its byte cap",
        });
    }
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| StoreError::Io {
            operation: "read bounded managed projection file",
            path: path.to_path_buf(),
            source,
        })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(StoreError::UnsafePath {
            path: path.to_path_buf(),
            reason: "managed projection file grew beyond its byte cap while being read",
        });
    }
    serde_json::from_slice(&bytes).map_err(|source| StoreError::Json { context, source })
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
    // macOS requires write permission on the directory being moved across
    // parents. Hardening therefore happens after rename and must retain the
    // committed identity if chmod fails.
    make_materialization_directory_immutable(active)
        .map_err(|source| committed_documents_error(&receipt, true, source))?;
    observe_publish_boundary(PublishBoundary::ActivationHardened);
    sync_publication_roots(store)
        .map_err(|source| committed_documents_error(&receipt, true, source))?;
    observe_publish_boundary(PublishBoundary::ActivationSynced);
    Ok(receipt)
}

fn committed_documents_error(
    receipt: &ManagedDocumentsReceipt,
    visible: bool,
    source: StoreError,
) -> StoreError {
    StoreError::ManagedDocumentsCommitted {
        materialization_id: receipt.materialization_id.clone(),
        content_sha256: receipt.content_sha256.clone(),
        database_sha256: receipt.database_sha256.clone(),
        visible,
        source: Box::new(source),
    }
}

#[cfg(test)]
thread_local! {
    static MANAGED_SYNC_FAULT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

fn sync_managed_directory(path: &Path) -> Result<(), StoreError> {
    #[cfg(test)]
    MANAGED_SYNC_FAULT.with(|fault| {
        if fault.borrow().as_deref() == Some(path) {
            fault.replace(None);
            return Err(StoreError::Io {
                operation: "sync managed directory",
                path: path.to_path_buf(),
                source: std::io::Error::other("injected sync failure"),
            });
        }
        Ok(())
    })?;
    sync_directory(path)
}

fn sync_publication_roots(store: &ManagedStore) -> Result<(), StoreError> {
    sync_managed_directory(&active_root(store))?;
    sync_managed_directory(&staging_root(store))
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
    let mut options = OpenOptions::new();
    // Windows FlushFileBuffers requires a write-capable handle. Unix fsync
    // accepts the read-only handle this staging-file boundary historically used.
    #[cfg(windows)]
    options.write(true);
    #[cfg(not(windows))]
    options.read(true);
    let file = options.open(path).map_err(|error| StoreError::Io {
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
    Err(StoreError::UnsupportedPlatform)
}

#[cfg(not(unix))]
fn make_materialization_directory_immutable(_path: &Path) -> Result<(), StoreError> {
    Err(StoreError::UnsupportedPlatform)
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
    Err(StoreError::UnsupportedPlatform)
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
    Err(StoreError::UnsupportedPlatform)
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

    #[cfg(unix)]
    #[test]
    fn post_publication_sync_failure_preserves_visible_identity_and_retry()
    -> Result<(), Box<dyn Error>> {
        for destination in [true, false] {
            let temp = tempdir()?;
            let store = ManagedStore::open(temp.path().join("managed"))?;
            let materialization = fixture_materialization()?;
            MANAGED_SYNC_FAULT.with(|fault| {
                fault.replace(Some(if destination {
                    active_root(&store)
                } else {
                    staging_root(&store)
                }))
            });
            let result = store.materialize_documents(&materialization);
            let Err(StoreError::ManagedDocumentsCommitted {
                materialization_id,
                content_sha256,
                visible,
                ..
            }) = result
            else {
                panic!("expected committed uncertainty: {result:?}");
            };
            assert!(visible);
            assert_eq!(materialization_id, materialization.materialization_id);
            assert_eq!(content_sha256, materialization.content_sha256);
            assert!(
                active_path(&store, &materialization_id)
                    .join(DATABASE_FILE)
                    .is_file()
            );
            let retry = store.materialize_documents(&materialization)?;
            assert_eq!(retry.materialization_id, materialization_id);
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn removal_sync_failure_reports_hidden_identity_and_finishes_on_retry()
    -> Result<(), Box<dyn Error>> {
        let temp = tempdir()?;
        let store = ManagedStore::open(temp.path().join("managed"))?;
        let materialization = fixture_materialization()?;
        let receipt = store.materialize_documents(&materialization)?;
        let request = ManagedDocumentsRemovalRequest {
            schema: MANAGED_DOCUMENTS_REMOVAL_SCHEMA.to_string(),
            materialization_id: receipt.materialization_id.clone(),
            content_sha256: receipt.content_sha256,
            database_sha256: receipt.database_sha256,
        };
        MANAGED_SYNC_FAULT.with(|fault| fault.replace(Some(active_root(&store))));
        let result = store.remove_managed_documents(&request);
        assert!(
            matches!(
                result,
                Err(StoreError::ManagedDocumentsCommitted { visible: false, .. })
            ),
            "{result:?}"
        );
        assert!(!active_path(&store, &request.materialization_id).exists());
        store.remove_managed_documents(&request)?;
        Ok(())
    }

    #[cfg(unix)]
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

    #[cfg(unix)]
    #[test]
    fn active_receipt_enumeration_rejects_overflow_before_opening_entries()
    -> Result<(), Box<dyn Error>> {
        let temporary = tempdir()?;
        let store = ManagedStore::open(temporary.path().join("managed"))?;
        assert!(store.list_active_managed_documents(1)?.entries.is_empty());
        let root = active_root(&store);
        fs::create_dir(root.join("first"))?;
        fs::create_dir(root.join("second"))?;
        assert!(matches!(
            store.list_active_managed_documents(1),
            Err(StoreError::RegistryCorrupt(message)) if message.contains("projection cap")
        ));
        assert!(
            store
                .list_active_managed_documents(MAX_ACTIVE_PROJECTION_ENTRIES + 1)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn bounded_json_reader_accepts_exact_limit_and_rejects_one_byte_over()
    -> Result<(), Box<dyn Error>> {
        let temporary = tempdir()?;
        let path = temporary.path().join("projection.json");
        let mut exact = vec![b' '; 62];
        exact.extend_from_slice(b"{}");
        assert_eq!(exact.len(), 64);
        fs::write(&path, exact)?;
        let value: serde_json::Value = read_json_bounded(&path, "projection fixture", 64)?;
        assert_eq!(value, serde_json::json!({}));

        let mut oversized = vec![b' '; 63];
        oversized.extend_from_slice(b"{}");
        assert_eq!(oversized.len(), 65);
        fs::write(&path, oversized)?;
        assert!(matches!(
            read_json_bounded::<serde_json::Value>(&path, "projection fixture", 64),
            Err(StoreError::UnsafePath { .. })
        ));
        Ok(())
    }

    #[test]
    fn managed_file_sync_uses_a_flush_capable_handle_without_changing_bytes()
    -> Result<(), Box<dyn Error>> {
        let temporary = tempdir()?;
        let path = temporary.path().join("managed-file.bin");
        let expected = b"durable managed bytes";
        fs::write(&path, expected)?;

        sync_regular_file(&path, "sync managed file fixture")?;

        assert_eq!(fs::read(path)?, expected);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn lightweight_receipt_projection_never_confers_database_authority()
    -> Result<(), Box<dyn Error>> {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempdir()?;
        let store = ManagedStore::open(temporary.path().join("managed"))?;
        let materialization = fixture_materialization()?;
        store.materialize_documents(&materialization)?;
        let database = active_path(&store, &materialization.materialization_id).join(DATABASE_FILE);
        fs::set_permissions(&database, fs::Permissions::from_mode(0o600))?;
        fs::write(&database, b"corrupted after activation")?;

        let listed = store.list_active_managed_documents(8)?;
        assert_eq!(listed.entries.len(), 1);
        assert!(
            store
                .active_managed_document(&materialization.materialization_id)
                .is_err(),
            "an exact action must reject the database that lightweight discovery did not hash"
        );
        Ok(())
    }

    #[cfg(unix)]
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

    #[cfg(unix)]
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

            let immediately_visible = store.list_active_managed_documents(8)?;
            assert!(immediately_visible.complete);
            let expected_visible = usize::from(matches!(
                boundary,
                PublishBoundary::ActivationRenamed
                    | PublishBoundary::ActivationHardened
                    | PublishBoundary::ActivationSynced
            ));
            assert_eq!(immediately_visible.entries.len(), expected_visible);

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
            let reopened = ManagedStore::open(temporary.path().join("managed"))?;
            let listed = reopened.list_active_managed_documents(8)?;
            assert_eq!(listed.entries.len(), 1);
            assert_eq!(
                listed.entries[0].materialization_id,
                materialization.materialization_id
            );
            let projected = reopened.project_active_managed_document(
                &materialization.materialization_id,
                MAX_MANIFEST_PROJECTION_BYTES,
            )?;
            assert_eq!(projected.content_sha256, materialization.content_sha256);
            assert_eq!(projected.documents[0].title, "Fixture Document");
        }
        Ok(())
    }
}
