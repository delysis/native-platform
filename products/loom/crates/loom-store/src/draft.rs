use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use loom_document::DocumentContent;
use loom_types::{BlobId, DocumentId, DocumentKind, RevisionId, now_unix_ms};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::file_io::{atomic_replace_private, read_bounded};
use crate::paths::{ensure_private_directory, normalize_document_path, reject_symlink_target};
use crate::provenance::validate_active_in_transaction;
use crate::store::ProjectStore;
use crate::{MAX_DOCUMENT_BYTES, Result, StoreError};

type LoadedDraftRecord = (String, String, String, i64, i64, i64, String, i64);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TransientDraft {
    pub document_id: DocumentId,
    pub source_revision_id: RevisionId,
    pub blob_id: BlobId,
    pub version: u64,
    pub kind: DocumentKind,
    pub text: String,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TransientDraftWriteOutcome {
    pub draft: TransientDraft,
    pub replayed: bool,
}

/// The exact transient state a semantic checkpoint is allowed to consume.
///
/// `version` is the last version observed by the caller. A checkpoint may also
/// consume its single exact successor when the successor records this version
/// as its base and has identical source and content identities. That narrowly
/// recovers a committed draft write whose reply was lost.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TransientDraftClaim {
    pub version: u64,
    pub source_revision_id: RevisionId,
    pub blob_id: BlobId,
}

impl ProjectStore {
    /// Durably replaces the one bounded transient draft for a document.
    ///
    /// Two alternating mutable slots bound storage to two full drafts per
    /// document, including every crash phase. `expected_version == 0` means
    /// the caller expects no draft. Retrying a committed write with the same
    /// source, expected version, and canonical bytes replays its result.
    #[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
    pub fn upsert_transient_draft(
        &mut self,
        relative_path: impl AsRef<Path>,
        source_revision_id: RevisionId,
        expected_version: u64,
        content: DocumentContent,
    ) -> Result<TransientDraftWriteOutcome> {
        let relative_path = normalize_document_path(relative_path.as_ref())?;
        let document = self
            .document_by_path(&relative_path)?
            .ok_or_else(|| StoreError::NoActiveRevision(relative_path.clone()))?;
        let content_kind = content.kind();
        if document.kind != content_kind {
            return Err(StoreError::DocumentKindMismatch {
                path: relative_path,
                stored: document.kind,
                requested: content_kind,
            });
        }
        let active = self
            .active_revision(document.id)?
            .ok_or_else(|| StoreError::NoActiveRevision(relative_path.clone()))?;
        if active.revision_id != source_revision_id {
            return Err(StoreError::SourceRevisionMismatch {
                expected: source_revision_id,
                actual: active.revision_id,
            });
        }
        let projection = content.project_visible()?;
        drop(content);
        ensure_draft_size(&projection.bytes)?;
        let blob_id = BlobId::digest(&projection.bytes);
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_active_in_transaction(&transaction, document.id, active)?;
        let current = draft_row(&transaction, document.id)?;
        if let Some(current) = &current
            && current.base_version == expected_version
            && current.source_revision_id == source_revision_id
            && current.blob_id == blob_id
        {
            let draft =
                load_draft_from_row(&self.root, document.kind, *current, &projection.bytes)?;
            transaction.rollback()?;
            return Ok(TransientDraftWriteOutcome {
                draft,
                replayed: true,
            });
        }
        let actual_version = current.as_ref().map(|draft| draft.version);
        if actual_version != version_option(expected_version) {
            return Err(StoreError::TransientDraftVersionConflict {
                expected: expected_version,
                actual: actual_version,
            });
        }
        let version = next_draft_version(&transaction, document.id, current.as_ref())?;
        let slot = storage_slot(version);
        let draft_path = draft_slot_path(&self.root, document.id, slot)?;
        if current.is_none() {
            remove_draft_slots(&self.root, document.id)?;
        }
        atomic_replace_private(&draft_path, &projection.bytes)?;
        let updated_at_ms = now_unix_ms();
        if current.is_none() {
            transaction.execute(
                "INSERT INTO transient_drafts(document_id, source_revision_id, draft_blob_id, storage_slot, draft_version, updated_at_ms, base_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    document.id.to_string(),
                    source_revision_id.to_string(),
                    blob_id.to_string(),
                    i64::from(slot),
                    sqlite_version(version)?,
                    updated_at_ms,
                    sqlite_version_allow_zero(expected_version)?,
                ],
            )?;
        } else {
            let changed = transaction.execute(
                "UPDATE transient_drafts
                 SET source_revision_id = ?2, draft_blob_id = ?3, storage_slot = ?4,
                     draft_version = ?5, updated_at_ms = ?6, base_version = ?7
                 WHERE document_id = ?1 AND draft_version = ?8",
                params![
                    document.id.to_string(),
                    source_revision_id.to_string(),
                    blob_id.to_string(),
                    i64::from(slot),
                    sqlite_version(version)?,
                    updated_at_ms,
                    sqlite_version_allow_zero(expected_version)?,
                    sqlite_version(expected_version)?,
                ],
            )?;
            if changed != 1 {
                return Err(StoreError::CorruptDatabase(format!(
                    "transient draft update affected {changed} rows"
                )));
            }
        }
        transaction.commit()?;
        let text =
            String::from_utf8(projection.bytes).map_err(loom_document::DocumentError::from)?;
        Ok(TransientDraftWriteOutcome {
            draft: TransientDraft {
                document_id: document.id,
                source_revision_id,
                blob_id,
                version,
                kind: document.kind,
                text,
                updated_at_ms,
            },
            replayed: false,
        })
    }

    pub fn load_transient_draft(
        &self,
        relative_path: impl AsRef<Path>,
    ) -> Result<Option<TransientDraft>> {
        let relative_path = normalize_document_path(relative_path.as_ref())?;
        let row: Option<LoadedDraftRecord> = self
            .connection
            .query_row(
                "SELECT td.document_id, td.source_revision_id, td.draft_blob_id,
                        td.storage_slot, td.draft_version, td.base_version,
                        d.document_kind, td.updated_at_ms
                 FROM transient_drafts td
                 JOIN documents d ON d.document_id = td.document_id
                 WHERE d.relative_path = ?1",
                [relative_path],
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
                    ))
                },
            )
            .optional()?;
        let Some((
            document_id,
            source_revision_id,
            blob_id,
            slot,
            version,
            base_version,
            kind,
            updated_at_ms,
        )) = row
        else {
            return Ok(None);
        };
        let row = DraftRow {
            document_id: parse_id(&document_id, "document_id")?,
            source_revision_id: parse_id(&source_revision_id, "source_revision_id")?,
            blob_id: parse_blob_id(&blob_id)?,
            slot: parse_slot(slot)?,
            version: parse_version(version)?,
            base_version: parse_version_allow_zero(base_version)?,
            updated_at_ms,
        };
        let kind = DocumentKind::from_str(&kind)
            .map_err(|error| StoreError::CorruptDatabase(error.to_string()))?;
        load_draft_from_row(&self.root, kind, row, &[]).map(Some)
    }

    pub fn clear_transient_draft(
        &mut self,
        relative_path: impl AsRef<Path>,
        expected_version: u64,
    ) -> Result<bool> {
        let relative_path = normalize_document_path(relative_path.as_ref())?;
        let document = self
            .document_by_path(&relative_path)?
            .ok_or_else(|| StoreError::NoActiveRevision(relative_path.clone()))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = draft_row(&transaction, document.id)?;
        let actual_version = current.as_ref().map(|draft| draft.version);
        if actual_version != version_option(expected_version) {
            return Err(StoreError::TransientDraftVersionConflict {
                expected: expected_version,
                actual: actual_version,
            });
        }
        let changed = if current.is_none() {
            0
        } else {
            transaction.execute(
                "DELETE FROM transient_drafts WHERE document_id = ?1 AND draft_version = ?2",
                params![document.id.to_string(), sqlite_version(expected_version)?],
            )?
        };
        transaction.commit()?;
        self.cleanup_draft_slots_if_unreferenced(document.id)?;
        Ok(changed == 1)
    }

    pub(crate) fn cleanup_draft_slots_if_unreferenced(
        &mut self,
        document_id: DocumentId,
    ) -> Result<()> {
        // This second immediate transaction closes the post-delete filesystem
        // race across multiple Loom processes. A concurrent writer either
        // commits first (and its slots are retained) or waits until cleanup
        // completes. A crash here is harmless because no row references a slot.
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if draft_row(&transaction, document_id)?.is_none() {
            remove_draft_slots(&self.root, document_id)?;
        }
        transaction.commit()?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
struct DraftRow {
    document_id: DocumentId,
    source_revision_id: RevisionId,
    blob_id: BlobId,
    slot: u8,
    version: u64,
    base_version: u64,
    updated_at_ms: i64,
}

fn draft_row(transaction: &Transaction<'_>, document_id: DocumentId) -> Result<Option<DraftRow>> {
    let row: Option<(String, String, i64, i64, i64, i64)> = transaction
        .query_row(
            "SELECT source_revision_id, draft_blob_id, storage_slot, draft_version, base_version, updated_at_ms
             FROM transient_drafts WHERE document_id = ?1",
            [document_id.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    row.map(
        |(source_revision_id, blob_id, slot, version, base_version, updated_at_ms)| {
            Ok(DraftRow {
                document_id,
                source_revision_id: parse_id(&source_revision_id, "source_revision_id")?,
                blob_id: parse_blob_id(&blob_id)?,
                slot: parse_slot(slot)?,
                version: parse_version(version)?,
                base_version: parse_version_allow_zero(base_version)?,
                updated_at_ms,
            })
        },
    )
    .transpose()
}

pub(crate) fn consume_transient_draft_in_transaction(
    transaction: &Transaction<'_>,
    document_id: DocumentId,
    claim: TransientDraftClaim,
) -> Result<u64> {
    let current =
        draft_row(transaction, document_id)?.ok_or(StoreError::TransientDraftVersionConflict {
            expected: claim.version,
            actual: None,
        })?;
    if current.version != claim.version && current.base_version != claim.version {
        return Err(StoreError::TransientDraftVersionConflict {
            expected: claim.version,
            actual: Some(current.version),
        });
    }
    if current.source_revision_id != claim.source_revision_id || current.blob_id != claim.blob_id {
        return Err(StoreError::TransientDraftIdentityMismatch {
            expected_source_revision_id: claim.source_revision_id,
            actual_source_revision_id: current.source_revision_id,
            expected_blob_id: claim.blob_id,
            actual_blob_id: current.blob_id,
        });
    }
    let changed = transaction.execute(
        "DELETE FROM transient_drafts
         WHERE document_id = ?1 AND draft_version = ?2
           AND source_revision_id = ?3 AND draft_blob_id = ?4",
        params![
            document_id.to_string(),
            sqlite_version(current.version)?,
            current.source_revision_id.to_string(),
            current.blob_id.to_string(),
        ],
    )?;
    if changed != 1 {
        return Err(StoreError::CorruptDatabase(format!(
            "exact transient draft consume affected {changed} rows"
        )));
    }
    Ok(current.version)
}

fn next_draft_version(
    transaction: &Transaction<'_>,
    document_id: DocumentId,
    current: Option<&DraftRow>,
) -> Result<u64> {
    let last_version = transaction
        .query_row(
            "SELECT last_version FROM transient_draft_sequences WHERE document_id = ?1",
            [document_id.to_string()],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .map(parse_version)
        .transpose()?
        .unwrap_or(0);
    if let Some(current) = current
        && last_version < current.version
    {
        return Err(StoreError::CorruptDatabase(
            "transient draft sequence trails the active draft".into(),
        ));
    }
    let version = last_version
        .checked_add(1)
        .ok_or_else(|| StoreError::CorruptDatabase("transient draft version overflow".into()))?;
    transaction.execute(
        "INSERT INTO transient_draft_sequences(document_id, last_version)
         VALUES (?1, ?2)
         ON CONFLICT(document_id) DO UPDATE SET last_version = excluded.last_version",
        params![document_id.to_string(), sqlite_version(version)?],
    )?;
    Ok(version)
}

fn load_draft_from_row(
    root: &Path,
    kind: DocumentKind,
    row: DraftRow,
    replay_bytes: &[u8],
) -> Result<TransientDraft> {
    let path = draft_slot_path(root, row.document_id, row.slot)?;
    let bytes = read_bounded(&path, MAX_DOCUMENT_BYTES)?;
    let actual = BlobId::digest(&bytes);
    if actual != row.blob_id {
        return Err(StoreError::CorruptBlob {
            path,
            expected: row.blob_id,
            actual,
        });
    }
    if !replay_bytes.is_empty() && bytes != replay_bytes {
        return Err(StoreError::CorruptDatabase(
            "idempotent draft replay bytes do not match the stored slot".into(),
        ));
    }
    let text = String::from_utf8(bytes).map_err(loom_document::DocumentError::from)?;
    Ok(TransientDraft {
        document_id: row.document_id,
        source_revision_id: row.source_revision_id,
        blob_id: row.blob_id,
        version: row.version,
        kind,
        text,
        updated_at_ms: row.updated_at_ms,
    })
}

fn draft_slot_path(root: &Path, document_id: DocumentId, slot: u8) -> Result<PathBuf> {
    let directory = root.join(".loom/drafts");
    ensure_private_directory(&directory)?;
    let path = directory.join(format!("{document_id}.{slot}.draft"));
    reject_symlink_target(&path)?;
    Ok(path)
}

fn remove_draft_slots(root: &Path, document_id: DocumentId) -> Result<()> {
    for slot in [0, 1] {
        let path = draft_slot_path(root, document_id, slot)?;
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn ensure_draft_size(bytes: &[u8]) -> Result<()> {
    let actual_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if actual_bytes > MAX_DOCUMENT_BYTES {
        return Err(StoreError::DocumentTooLarge {
            actual_bytes,
            max_bytes: MAX_DOCUMENT_BYTES,
        });
    }
    Ok(())
}

const fn storage_slot(version: u64) -> u8 {
    (version & 1) as u8
}

const fn version_option(version: u64) -> Option<u64> {
    if version == 0 { None } else { Some(version) }
}

fn sqlite_version(version: u64) -> Result<i64> {
    i64::try_from(version)
        .map_err(|_| StoreError::CorruptDatabase("transient draft version exceeds SQLite".into()))
}

fn sqlite_version_allow_zero(version: u64) -> Result<i64> {
    i64::try_from(version)
        .map_err(|_| StoreError::CorruptDatabase("transient draft version exceeds SQLite".into()))
}

fn parse_version(version: i64) -> Result<u64> {
    let version = parse_version_allow_zero(version)?;
    if version == 0 {
        return Err(StoreError::CorruptDatabase(
            "transient draft version must be positive".into(),
        ));
    }
    Ok(version)
}

fn parse_version_allow_zero(version: i64) -> Result<u64> {
    u64::try_from(version)
        .map_err(|_| StoreError::CorruptDatabase("negative transient draft version".into()))
}

fn parse_slot(slot: i64) -> Result<u8> {
    match slot {
        0 => Ok(0),
        1 => Ok(1),
        _ => Err(StoreError::CorruptDatabase(format!(
            "invalid transient draft storage slot {slot}"
        ))),
    }
}

fn parse_id<T>(value: &str, column: &str) -> Result<T>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    value.parse().map_err(|error| {
        StoreError::CorruptDatabase(format!("invalid {column} `{value}`: {error}"))
    })
}

fn parse_blob_id(value: &str) -> Result<BlobId> {
    value
        .parse()
        .map_err(|error| StoreError::CorruptDatabase(format!("invalid draft blob id: {error}")))
}

#[cfg(test)]
mod tests {
    use loom_document::DocumentContent;
    use tempfile::tempdir;

    use super::*;

    fn new_store() -> (tempfile::TempDir, ProjectStore, crate::LoadedDocument) {
        let directory = tempdir().expect("temporary project");
        let root = directory.path().join("Novel");
        let (mut store, _) = ProjectStore::initialize(&root, "Novel").expect("initialize");
        store
            .save_document(
                "manuscript/001.md",
                DocumentContent::Prose("source".into()),
                "initial",
            )
            .expect("initial save");
        let source = store
            .read_document("manuscript/001.md")
            .expect("load source");
        (directory, store, source)
    }

    #[test]
    fn thousands_of_drafts_use_two_slots_without_semantic_history() {
        let (_directory, mut store, source) = new_store();
        let counts = store.counts().expect("semantic counts");
        let mut version = 0;
        for index in 0..1_000 {
            let outcome = store
                .upsert_transient_draft(
                    "manuscript/001.md",
                    source.revision_id,
                    version,
                    DocumentContent::Prose(format!("source {index}")),
                )
                .expect("write draft");
            assert!(!outcome.replayed);
            version = outcome.draft.version;
        }
        let after_drafts = store.counts().expect("semantic counts");
        assert_eq!(after_drafts, counts);
        let slot_count = fs::read_dir(store.root.join(".loom/drafts"))
            .expect("draft directory")
            .count();
        assert_eq!(slot_count, 2);
        assert_eq!(
            store
                .read_document("manuscript/001.md")
                .expect("visible source")
                .text,
            "source"
        );
    }

    #[test]
    fn draft_retry_is_idempotent_and_stale_different_bytes_fail() {
        let (_directory, mut store, source) = new_store();
        let request = DocumentContent::Prose("newer".into());
        let first = store
            .upsert_transient_draft("manuscript/001.md", source.revision_id, 0, request.clone())
            .expect("new draft");
        let replay = store
            .upsert_transient_draft("manuscript/001.md", source.revision_id, 0, request)
            .expect("replay after lost acknowledgement");
        assert!(replay.replayed);
        assert_eq!(replay.draft.version, first.draft.version);
        let result = store.upsert_transient_draft(
            "manuscript/001.md",
            source.revision_id,
            0,
            DocumentContent::Prose("stale different bytes".into()),
        );
        assert!(matches!(
            result,
            Err(StoreError::TransientDraftVersionConflict {
                expected: 0,
                actual: Some(1)
            })
        ));
    }

    #[test]
    fn crash_before_database_commit_cannot_replace_active_draft() {
        let (_directory, mut store, source) = new_store();
        let first = store
            .upsert_transient_draft(
                "manuscript/001.md",
                source.revision_id,
                0,
                DocumentContent::Prose("committed".into()),
            )
            .expect("committed draft");
        let uncommitted_slot = storage_slot(first.draft.version + 1);
        let uncommitted_path =
            draft_slot_path(&store.root, source.document_id, uncommitted_slot).expect("slot path");
        atomic_replace_private(&uncommitted_path, b"uncommitted")
            .expect("simulate slot write before commit");

        assert_eq!(
            store
                .load_transient_draft("manuscript/001.md")
                .expect("load draft")
                .expect("draft")
                .text,
            "committed"
        );
        let second = store
            .upsert_transient_draft(
                "manuscript/001.md",
                source.revision_id,
                first.draft.version,
                DocumentContent::Prose("next committed".into()),
            )
            .expect("overwrite inactive crash slot");
        assert_eq!(second.draft.version, 2);
    }

    #[test]
    fn clear_removes_both_bounded_slots() {
        let (_directory, mut store, source) = new_store();
        let first = store
            .upsert_transient_draft(
                "manuscript/001.md",
                source.revision_id,
                0,
                DocumentContent::Prose("first".into()),
            )
            .expect("first draft");
        let second = store
            .upsert_transient_draft(
                "manuscript/001.md",
                source.revision_id,
                first.draft.version,
                DocumentContent::Prose("second".into()),
            )
            .expect("second draft");
        assert!(
            store
                .clear_transient_draft("manuscript/001.md", second.draft.version)
                .expect("clear draft")
        );
        assert_eq!(
            fs::read_dir(store.root.join(".loom/drafts"))
                .expect("draft directory")
                .count(),
            0
        );
    }

    #[test]
    fn cleared_versions_are_never_reused_or_vulnerable_to_aba_clear() {
        let (_directory, mut store, source) = new_store();
        let first = store
            .upsert_transient_draft(
                "manuscript/001.md",
                source.revision_id,
                0,
                DocumentContent::Prose("first".into()),
            )
            .expect("first draft");
        store
            .clear_transient_draft("manuscript/001.md", first.draft.version)
            .expect("clear first draft");
        let second = store
            .upsert_transient_draft(
                "manuscript/001.md",
                source.revision_id,
                0,
                DocumentContent::Prose("second".into()),
            )
            .expect("second draft");

        assert!(second.draft.version > first.draft.version);
        assert!(matches!(
            store.clear_transient_draft("manuscript/001.md", first.draft.version),
            Err(StoreError::TransientDraftVersionConflict {
                expected,
                actual: Some(actual),
            }) if expected == first.draft.version && actual == second.draft.version
        ));
        assert_eq!(
            store
                .load_transient_draft("manuscript/001.md")
                .expect("load second draft")
                .expect("second draft remains")
                .text,
            "second"
        );
    }

    #[test]
    fn first_write_after_clear_replays_exactly_after_lost_acknowledgement() {
        let (_directory, mut store, source) = new_store();
        let first = store
            .upsert_transient_draft(
                "manuscript/001.md",
                source.revision_id,
                0,
                DocumentContent::Prose("before clear".into()),
            )
            .expect("first draft");
        store
            .clear_transient_draft("manuscript/001.md", first.draft.version)
            .expect("clear first draft");
        let request = DocumentContent::Prose("after clear".into());
        let committed = store
            .upsert_transient_draft("manuscript/001.md", source.revision_id, 0, request.clone())
            .expect("commit after clear");
        let replay = store
            .upsert_transient_draft("manuscript/001.md", source.revision_id, 0, request)
            .expect("replay lost reply");

        assert!(replay.replayed);
        assert_eq!(replay.draft.version, committed.draft.version);
        assert!(committed.draft.version > first.draft.version);
    }

    #[test]
    fn checkpoint_atomically_consumes_exact_lost_ack_draft_and_replays() {
        let (_directory, mut store, source) = new_store();
        let text = "semantic checkpoint";
        let committed = store
            .upsert_transient_draft(
                "manuscript/001.md",
                source.revision_id,
                0,
                DocumentContent::Prose(text.into()),
            )
            .expect("draft committed despite imagined lost reply");
        let command_id = loom_types::CommandId::new();
        // The caller still observes zero because the draft reply was lost.
        let observed_draft_version = 0;
        let saved = store
            .save_document_if_source_idempotent_consuming_draft(
                command_id,
                "manuscript/001.md",
                DocumentContent::Prose(text.into()),
                "idle save",
                source.revision_id,
                source.blob_id,
                observed_draft_version,
            )
            .expect("consume exact successor draft");
        assert!(!saved.replayed);
        assert!(committed.draft.version > observed_draft_version);
        assert!(
            store
                .load_transient_draft("manuscript/001.md")
                .expect("load consumed draft")
                .is_none()
        );

        let replay = store
            .save_document_if_source_idempotent_consuming_draft(
                command_id,
                "manuscript/001.md",
                DocumentContent::Prose(text.into()),
                "idle save",
                source.revision_id,
                source.blob_id,
                observed_draft_version,
            )
            .expect("replay consumed checkpoint");
        assert!(replay.replayed);
        assert_eq!(replay.save.revision_id, saved.save.revision_id);
    }

    #[test]
    fn checkpoint_refuses_mismatched_draft_without_consuming_or_saving() {
        let (_directory, mut store, source) = new_store();
        let current = store
            .upsert_transient_draft(
                "manuscript/001.md",
                source.revision_id,
                0,
                DocumentContent::Prose("newer draft".into()),
            )
            .expect("current draft");
        let attempted = "different checkpoint";
        let result = store.save_document_if_source_idempotent_consuming_draft(
            loom_types::CommandId::new(),
            "manuscript/001.md",
            DocumentContent::Prose(attempted.into()),
            "idle save",
            source.revision_id,
            source.blob_id,
            0,
        );

        assert!(matches!(
            result,
            Err(StoreError::TransientDraftIdentityMismatch { .. })
        ));
        assert_eq!(
            store
                .load_transient_draft("manuscript/001.md")
                .expect("load current draft")
                .expect("draft retained"),
            current.draft
        );
        assert_eq!(
            store
                .read_document("manuscript/001.md")
                .expect("active manuscript unchanged")
                .text,
            "source"
        );
    }
}
